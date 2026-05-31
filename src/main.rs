mod mmwave;

use anyhow::{Context, Result};
use clap::{ArgAction, Parser, Subcommand};
use serde::Serialize;
use slint::{ComponentHandle, Model};

slint::include_modules!();

#[derive(Debug, Parser)]
#[command(
    name = "mmwave-flasher",
    version,
    about = "Tiny Rust UI and CLI for TI classic xWR mmWave metaImage flashing"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List serial ports.
    Ports {
        #[arg(long)]
        json: bool,
    },
    /// Produce a machine-readable dry-run plan.
    Plan(PlanArgs),
    /// Flash a merged mmWave metaImage over UART ROM bootloader.
    #[command(alias = "download")]
    Flash(FlashArgs),
}

#[derive(Debug, Parser)]
struct PlanArgs {
    #[arg(long)]
    port: Option<String>,
    #[arg(long)]
    file: String,
    #[arg(long, default_value_t = 1)]
    meta_slot: u8,
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    erase: bool,
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    verify: bool,
    #[arg(long, default_value_t = mmwave::DEFAULT_BAUDRATE)]
    baudrate: u32,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
struct FlashArgs {
    #[arg(long)]
    port: String,
    #[arg(long)]
    file: String,
    #[arg(long, default_value_t = 1)]
    meta_slot: u8,
    #[arg(long, default_value_t = mmwave::DEFAULT_BAUDRATE)]
    baudrate: u32,
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    erase: bool,
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    verify: bool,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    ndjson: bool,
}

#[derive(Debug, Serialize)]
struct PlanOutput {
    ok: bool,
    operation: String,
    port: Option<String>,
    file: String,
    meta_slot: u8,
    baudrate: u32,
    erase: bool,
    verify: bool,
    blockers: Vec<String>,
    steps: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct PortRow {
    name: String,
    kind: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Ports { json }) => list_ports(json),
        Some(Command::Plan(args)) => plan_cli(args),
        Some(Command::Flash(args)) => flash_cli(args),
        None => run_ui(),
    }
}

fn list_ports(json: bool) -> Result<()> {
    let rows = serialport::available_ports()
        .context("failed to list serial ports")?
        .into_iter()
        .map(|port| PortRow {
            name: port.port_name,
            kind: format!("{:?}", port.port_type),
        })
        .collect::<Vec<_>>();

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if rows.is_empty() {
        println!("No serial ports found.");
    } else {
        for row in rows {
            println!("{}  {}", row.name, row.kind);
        }
    }
    Ok(())
}

fn flash_cli(args: FlashArgs) -> Result<()> {
    let config = mmwave::FlashConfig {
        port: args.port.clone(),
        file: args.file.clone(),
        slot: args.meta_slot,
        erase: args.erase,
        verify_status: args.verify,
        baudrate: args.baudrate,
    };

    let summary = mmwave::flash_file(&config, |event, message, progress| {
        if args.ndjson {
            println!(
                "{}",
                serde_json::json!({
                    "event": event,
                    "message": message,
                    "progress": progress
                })
            );
        } else if !args.json {
            eprintln!("[{progress:>3}%] {event}: {message}");
        }
        Ok(())
    })?;

    if args.json || args.ndjson {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!(
            "Flashed {} bytes in {} chunk(s) to slot {}.",
            summary.bytes_written, summary.chunks_written, summary.slot
        );
    }
    Ok(())
}

fn plan_cli(args: PlanArgs) -> Result<()> {
    let output = build_plan_output(&args);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Operation: {}", output.operation);
        println!("File: {}", output.file);
        println!("Port: {}", output.port.as_deref().unwrap_or("<required>"));
        println!("Slot: {}", output.meta_slot);
        if output.blockers.is_empty() {
            println!("Ready to download.");
        } else {
            println!("Blocked:");
            for blocker in output.blockers {
                println!("- {blocker}");
            }
        }
    }
    Ok(())
}

fn build_plan_output(args: &PlanArgs) -> PlanOutput {
    let mut blockers = Vec::new();
    if args.port.as_deref().unwrap_or_default().trim().is_empty() {
        blockers.push("serial port is required".into());
    }
    if args.file.trim().is_empty() {
        blockers.push("metaImage file is required".into());
    } else if !std::path::Path::new(&args.file).is_file() {
        blockers.push(format!("metaImage file does not exist: {}", args.file));
    }
    if !(1..=4).contains(&args.meta_slot) {
        blockers.push("meta slot must be 1..4".into());
    }

    PlanOutput {
        ok: blockers.is_empty(),
        operation: "download".into(),
        port: args.port.clone(),
        file: args.file.clone(),
        meta_slot: args.meta_slot,
        baudrate: args.baudrate,
        erase: args.erase,
        verify: args.verify,
        blockers,
        steps: vec![
            "open serial port",
            "send UART break",
            "ping ROM bootloader",
            "optional erase serial flash",
            "open metaImage slot",
            "write 240-byte chunks",
            "close image",
            "check ACK/status",
        ],
    }
}

fn run_ui() -> Result<()> {
    let app = AppWindow::new().context("failed to create UI")?;
    app.set_progress(0);
    app.set_status_text("Ready".into());

    {
        let weak = app.as_weak();
        app.on_browse_file(move || {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("TI mmWave metaImage", &["bin"])
                .pick_file()
            {
                if let Some(app) = weak.upgrade() {
                    app.set_file_text(path.display().to_string().into());
                }
            }
        });
    }

    {
        let weak = app.as_weak();
        app.on_scan_ports(move || match serialport::available_ports() {
            Ok(ports) if ports.is_empty() => {
                if let Some(app) = weak.upgrade() {
                    app.set_status_text("No ports".into());
                    app.set_log_text("No serial ports were found.".into());
                }
            }
            Ok(ports) => {
                let first = ports[0].port_name.clone();
                let options = ports
                    .iter()
                    .map(|port| slint::SharedString::from(port.port_name.as_str()))
                    .collect::<Vec<_>>();
                let text = ports
                    .iter()
                    .map(|port| format!("{}  {:?}", port.port_name, port.port_type))
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Some(app) = weak.upgrade() {
                    app.set_port_text(first.into());
                    app.set_port_options(std::rc::Rc::new(slint::VecModel::from(options)).into());
                    app.set_status_text("Ports found".into());
                    app.set_log_text(text.into());
                }
            }
            Err(err) => {
                if let Some(app) = weak.upgrade() {
                    app.set_status_text("Scan failed".into());
                    app.set_log_text(err.to_string().into());
                }
            }
        });
    }

    {
        let weak = app.as_weak();
        app.on_select_port(move |index| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if let Some(port) = app.get_port_options().row_data(index as usize) {
                app.set_port_text(port);
            }
        });
    }

    {
        let weak = app.as_weak();
        app.on_start_flash(move || {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if app.get_busy() {
                return;
            }

            let config = mmwave::FlashConfig {
                port: app.get_port_text().to_string(),
                file: app.get_file_text().to_string(),
                slot: app.get_meta_slot() as u8,
                erase: app.get_erase_first(),
                verify_status: app.get_verify_status(),
                baudrate: mmwave::DEFAULT_BAUDRATE,
            };
            if config.port.trim().is_empty() || config.file.trim().is_empty() {
                app.set_status_text("Need input".into());
                app.set_log_text("Pick a serial port and a merged metaImage .bin first.".into());
                return;
            }

            app.set_busy(true);
            app.set_progress(0);
            app.set_status_text("Flashing".into());
            app.set_log_text("Starting mmWave UART ROM flash...".into());

            let worker_weak = weak.clone();
            std::thread::spawn(move || {
                let result = mmwave::flash_file(&config, |event, message, progress| {
                    post_ui(
                        &worker_weak,
                        "Flashing",
                        &format!("{event}: {message}"),
                        progress,
                    );
                    Ok(())
                });

                match result {
                    Ok(summary) => post_ui_done(
                        &worker_weak,
                        "Done",
                        &format!(
                            "Flash complete.\nBytes: {}\nChunks: {}\nSlot: {}\nROM: {}",
                            summary.bytes_written,
                            summary.chunks_written,
                            summary.slot,
                            summary.rom_version.unwrap_or_else(|| "unknown".into())
                        ),
                        100,
                        false,
                    ),
                    Err(err) => post_ui_done(&worker_weak, "Failed", &format!("{err:#}"), 0, false),
                }
            });
        });
    }

    app.run().context("UI loop failed")
}

fn post_ui(weak: &slint::Weak<AppWindow>, status: &str, log: &str, progress: i32) {
    let weak = weak.clone();
    let status = status.to_string();
    let log = log.to_string();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = weak.upgrade() {
            app.set_status_text(status.into());
            app.set_log_text(log.into());
            app.set_progress(progress.clamp(0, 100));
        }
    });
}

fn post_ui_done(weak: &slint::Weak<AppWindow>, status: &str, log: &str, progress: i32, busy: bool) {
    let weak = weak.clone();
    let status = status.to_string();
    let log = log.to_string();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = weak.upgrade() {
            app.set_status_text(status.into());
            app.set_log_text(log.into());
            app.set_progress(progress.clamp(0, 100));
            app.set_busy(busy);
        }
    });
}
