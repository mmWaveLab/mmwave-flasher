use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, StopBits};

pub const DEFAULT_BAUDRATE: u32 = 115_200;
pub const MAX_CHUNK_SIZE: usize = 240;

const SYNC: u8 = 0xaa;
const ACK: u16 = 0x00cc;
const STORAGE_SFLASH: u32 = 2;

const CMD_PING: u8 = 0x20;
const CMD_OPEN_FILE: u8 = 0x21;
const CMD_CLOSE_FILE: u8 = 0x22;
const CMD_GET_STATUS: u8 = 0x23;
const CMD_WRITE_FILE_SFLASH: u8 = 0x24;
const CMD_ERASE_DEVICE: u8 = 0x28;
const CMD_GET_VERSION: u8 = 0x2f;

const STATUS_INITIAL: u8 = 0x00;
const STATUS_SUCCESS: u8 = 0x40;
const STATUS_ACCESS_IN_PROGRESS: u8 = 0x4b;

#[derive(Debug, Clone)]
pub struct FlashConfig {
    pub port: String,
    pub file: String,
    pub slot: u8,
    pub erase: bool,
    pub verify_status: bool,
    pub baudrate: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlashSummary {
    pub bytes_written: usize,
    pub chunks_written: usize,
    pub slot: u8,
    pub rom_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaImageSlot {
    Image1,
    Image2,
    Image3,
    Image4,
}

impl MetaImageSlot {
    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Image1),
            2 => Ok(Self::Image2),
            3 => Ok(Self::Image3),
            4 => Ok(Self::Image4),
            _ => bail!("meta slot must be 1, 2, 3, or 4"),
        }
    }

    fn number(self) -> u8 {
        match self {
            Self::Image1 => 1,
            Self::Image2 => 2,
            Self::Image3 => 3,
            Self::Image4 => 4,
        }
    }

    fn file_type(self) -> u32 {
        match self {
            Self::Image1 => 4,
            Self::Image2 => 5,
            Self::Image3 => 6,
            Self::Image4 => 7,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FlashOptions {
    pub erase: bool,
    pub verify_status: bool,
    pub slot: MetaImageSlot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Response {
    payload: Vec<u8>,
}

pub fn flash_file<F>(config: &FlashConfig, mut emit_progress: F) -> Result<FlashSummary>
where
    F: FnMut(&str, &str, i32) -> Result<()>,
{
    let path = Path::new(&config.file);
    let image = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    ensure!(!config.port.trim().is_empty(), "serial port is required");

    emit_progress("read", "Reading merged metaImage", 4)?;
    emit_progress("connect", "Opening UART bootloader port", 8)?;
    let mut port = serialport::new(&config.port, config.baudrate)
        .data_bits(DataBits::Eight)
        .flow_control(FlowControl::None)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .timeout(Duration::from_secs(3))
        .open()
        .with_context(|| format!("failed to open serial port {}", config.port))?;
    let _ = port.clear(ClearBuffer::All);

    emit_progress("break", "Sending UART break", 12)?;
    port.set_break().context("failed to assert UART break")?;
    std::thread::sleep(Duration::from_millis(100));
    port.clear_break().context("failed to clear UART break")?;
    std::thread::sleep(Duration::from_millis(100));

    flash_image(
        &mut port,
        &image,
        FlashOptions {
            erase: config.erase,
            verify_status: config.verify_status,
            slot: MetaImageSlot::from_u8(config.slot)?,
        },
        emit_progress,
    )
}

pub fn flash_image<T, F>(
    transport: &mut T,
    image: &[u8],
    options: FlashOptions,
    mut emit_progress: F,
) -> Result<FlashSummary>
where
    T: Read + Write,
    F: FnMut(&str, &str, i32) -> Result<()>,
{
    ensure!(!image.is_empty(), "metaImage is empty");
    ensure!(
        image.len() <= u32::MAX as usize,
        "metaImage is too large for the ROM protocol"
    );

    emit_progress("ping", "Pinging mmWave ROM bootloader", 14)?;
    ping(transport).context("ROM bootloader did not ACK ping")?;

    let rom_version = match get_version(transport) {
        Ok(version) => {
            emit_progress("version", &format!("ROM version {version}"), 16)?;
            Some(version)
        }
        Err(_) => {
            emit_progress("version", "ROM version query skipped", 16)?;
            None
        }
    };

    if options.erase {
        emit_progress("erase", "Erasing serial flash", 20)?;
        send_and_expect_ack(transport, &erase_command()).context("erase command failed")?;
        wait_for_success(transport, Duration::from_secs(120), "erase")
            .context("erase did not complete")?;
    }

    emit_progress("open", "Opening serial flash image slot", 28)?;
    send_and_expect_ack(
        transport,
        &open_file_command(image.len() as u32, options.slot),
    )
    .context("open file command failed")?;

    let chunks_total = image.len().div_ceil(MAX_CHUNK_SIZE);
    for (index, chunk) in image.chunks(MAX_CHUNK_SIZE).enumerate() {
        send_and_expect_ack(transport, &write_flash_command(chunk))
            .with_context(|| format!("write chunk {}/{} failed", index + 1, chunks_total))?;
        emit_progress(
            "program",
            &format!("Programming chunk {}/{}", index + 1, chunks_total),
            progress(index + 1, chunks_total, 30, 88),
        )?;
    }

    emit_progress("close", "Closing serial flash image", 92)?;
    send_and_expect_ack(transport, &close_file_command()).context("close file command failed")?;

    if options.verify_status {
        emit_progress("verify", "Checking bootloader status", 96)?;
        wait_for_success(transport, Duration::from_secs(30), "program")
            .context("program status check failed")?;
    }

    emit_progress("done", "Flash complete", 100)?;
    Ok(FlashSummary {
        bytes_written: image.len(),
        chunks_written: chunks_total,
        slot: options.slot.number(),
        rom_version,
    })
}

pub fn ping<T: Read + Write>(transport: &mut T) -> Result<()> {
    send_and_expect_ack(transport, &simple_command(CMD_PING))
}

fn get_version<T: Read + Write>(transport: &mut T) -> Result<String> {
    transport.write_all(&simple_command(CMD_GET_VERSION))?;
    transport.flush()?;
    let response = read_response(transport)?;
    ensure!(
        response.payload.len() >= 4,
        "version response payload is too short"
    );
    Ok(format!(
        "{:02x}.{:02x}.{:02x}.{:02x}",
        response.payload[0], response.payload[1], response.payload[2], response.payload[3]
    ))
}

fn send_and_expect_ack<T: Read + Write>(transport: &mut T, command: &[u8]) -> Result<()> {
    transport.write_all(command)?;
    transport.flush()?;
    let response = read_response(transport)?;
    ensure!(
        is_ack(&response),
        "expected ACK response, got {:02x?}",
        response.payload
    );
    Ok(())
}

fn wait_for_success<T: Read + Write>(
    transport: &mut T,
    timeout: Duration,
    label: &str,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = get_status(transport)?;
        match status {
            STATUS_SUCCESS => return Ok(()),
            STATUS_INITIAL | STATUS_ACCESS_IN_PROGRESS if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(250));
            }
            STATUS_INITIAL | STATUS_ACCESS_IN_PROGRESS => {
                bail!("{label} timed out with status 0x{status:02x}");
            }
            other => bail!("{label} failed with bootloader status 0x{other:02x}"),
        }
    }
}

fn get_status<T: Read + Write>(transport: &mut T) -> Result<u8> {
    transport.write_all(&simple_command(CMD_GET_STATUS))?;
    transport.flush()?;
    let response = read_response(transport)?;
    ensure!(
        response.payload.len() == 1,
        "status response payload has {} byte(s)",
        response.payload.len()
    );
    Ok(response.payload[0])
}

fn read_response<T: Read>(transport: &mut T) -> Result<Response> {
    let mut header = [0u8; 3];
    transport.read_exact(&mut header)?;
    let length = u16::from_be_bytes([header[0], header[1]]);
    ensure!(length >= 2, "invalid response length {length}");
    let payload_len = (length - 2) as usize;
    let mut payload = vec![0u8; payload_len];
    transport.read_exact(&mut payload)?;
    ensure!(
        checksum(&payload) == header[2],
        "response checksum mismatch: expected 0x{:02x}, got 0x{:02x}",
        checksum(&payload),
        header[2]
    );
    Ok(Response { payload })
}

fn is_ack(response: &Response) -> bool {
    match response.payload.as_slice() {
        [0xcc] => true,
        [hi, lo] => u16::from_be_bytes([*hi, *lo]) == ACK,
        _ => false,
    }
}

pub fn simple_command(opcode: u8) -> Vec<u8> {
    frame(&[opcode])
}

pub fn open_file_command(file_size: u32, slot: MetaImageSlot) -> Vec<u8> {
    let mut payload = Vec::with_capacity(17);
    payload.push(CMD_OPEN_FILE);
    payload.extend_from_slice(&file_size.to_be_bytes());
    payload.extend_from_slice(&STORAGE_SFLASH.to_be_bytes());
    payload.extend_from_slice(&slot.file_type().to_be_bytes());
    payload.extend_from_slice(&0u32.to_be_bytes());
    frame(&payload)
}

pub fn write_flash_command(chunk: &[u8]) -> Vec<u8> {
    assert!(chunk.len() <= MAX_CHUNK_SIZE);
    let mut payload = Vec::with_capacity(chunk.len() + 1);
    payload.push(CMD_WRITE_FILE_SFLASH);
    payload.extend_from_slice(chunk);
    frame(&payload)
}

pub fn close_file_command() -> Vec<u8> {
    let mut payload = Vec::with_capacity(5);
    payload.push(CMD_CLOSE_FILE);
    payload.extend_from_slice(&STORAGE_SFLASH.to_be_bytes());
    frame(&payload)
}

pub fn erase_command() -> Vec<u8> {
    simple_command(CMD_ERASE_DEVICE)
}

pub fn frame(payload: &[u8]) -> Vec<u8> {
    let length = (payload.len() + 2) as u16;
    let mut out = Vec::with_capacity(payload.len() + 4);
    out.push(SYNC);
    out.extend_from_slice(&length.to_be_bytes());
    out.push(checksum(payload));
    out.extend_from_slice(payload);
    out
}

fn checksum(payload: &[u8]) -> u8 {
    payload
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte))
}

fn progress(done: usize, total: usize, start: i32, end: i32) -> i32 {
    if total == 0 {
        return end;
    }
    let ratio = done as f64 / total as f64;
    (start as f64 + (end - start) as f64 * ratio).round() as i32
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{Read, Result as IoResult, Write};

    use super::*;

    #[test]
    fn builds_ping_with_big_endian_length_and_checksum() {
        assert_eq!(simple_command(CMD_PING), vec![0xaa, 0x00, 0x03, 0x20, 0x20]);
    }

    #[test]
    fn builds_open_file_for_meta_image_slot_1() {
        let command = open_file_command(0x1234_5678, MetaImageSlot::Image1);
        assert_eq!(&command[..4], &[0xaa, 0x00, 0x13, 0x3b]);
        assert_eq!(command[4], CMD_OPEN_FILE);
        assert_eq!(&command[5..9], &0x1234_5678u32.to_be_bytes());
        assert_eq!(&command[9..13], &STORAGE_SFLASH.to_be_bytes());
        assert_eq!(&command[13..17], &4u32.to_be_bytes());
        assert_eq!(&command[17..21], &0u32.to_be_bytes());
    }

    #[test]
    fn flash_image_splits_into_240_byte_chunks() {
        let image: Vec<u8> = (0..241).map(|value| value as u8).collect();
        let mut transport = FakeTransport::new(vec![
            ack_response(),
            version_response(),
            ack_response(),
            ack_response(),
            ack_response(),
            ack_response(),
            status_response(STATUS_SUCCESS),
        ]);

        let summary = flash_image(
            &mut transport,
            &image,
            FlashOptions {
                erase: false,
                verify_status: true,
                slot: MetaImageSlot::Image1,
            },
            |_, _, _| Ok(()),
        )
        .expect("flash");

        assert_eq!(summary.bytes_written, 241);
        assert_eq!(summary.chunks_written, 2);
        assert_eq!(
            transport
                .writes
                .iter()
                .filter(|write| write.get(4) == Some(&CMD_WRITE_FILE_SFLASH))
                .count(),
            2
        );
    }

    fn ack_response() -> Vec<u8> {
        response(&ACK.to_be_bytes())
    }

    fn version_response() -> Vec<u8> {
        let mut payload = vec![1, 2, 3, 4];
        payload.extend_from_slice(&[0; 8]);
        response(&payload)
    }

    fn status_response(status: u8) -> Vec<u8> {
        response(&[status])
    }

    fn response(payload: &[u8]) -> Vec<u8> {
        let length = (payload.len() + 2) as u16;
        let mut out = Vec::new();
        out.extend_from_slice(&length.to_be_bytes());
        out.push(checksum(payload));
        out.extend_from_slice(payload);
        out
    }

    struct FakeTransport {
        reads: VecDeque<u8>,
        writes: Vec<Vec<u8>>,
    }

    impl FakeTransport {
        fn new(responses: Vec<Vec<u8>>) -> Self {
            Self {
                reads: responses.into_iter().flatten().collect(),
                writes: Vec::new(),
            }
        }
    }

    impl Read for FakeTransport {
        fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
            let mut read = 0;
            for slot in buf.iter_mut() {
                let Some(byte) = self.reads.pop_front() else {
                    break;
                };
                *slot = byte;
                read += 1;
            }
            Ok(read)
        }
    }

    impl Write for FakeTransport {
        fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
            self.writes.push(buf.to_vec());
            Ok(buf.len())
        }

        fn flush(&mut self) -> IoResult<()> {
            Ok(())
        }
    }
}
