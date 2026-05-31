# MeowWave Flash

A tiny, focused Rust desktop app for flashing TI classic xWR / IWR / AWR mmWave
merged `metaImage.bin` files over the UART ROM bootloader.

This project intentionally avoids the previous all-in-one downloader shape. It
does one thing: make classic mmWave metaImage flashing pleasant, visible, and
scriptable.

## Scope

Supported target family:

- TI classic xWR / IWR / AWR mmWave UART ROM bootloader
- Tested protocol shape for `IWR6843AOP`, `IWR1843`, `IWR1642`, and `AWR1843`
- Serial UART at `115200`
- Merged `metaImage.bin` input
- Meta image slots `1` through `4`
- Optional serial flash erase before write
- Bootloader ACK/status verification

Not in scope:

- MSS/DSS split-image workflows
- Auto-detecting TI mmWave boards
- A large multi-vendor plugin shell

## App

```bash
cargo run
```

Pick the UART port, select a merged `metaImage.bin`, choose a slot, then click
`Flash metaImage`.

## CLI

List serial ports:

```bash
cargo run -- ports
cargo run -- ports --json
```

AI dry-run plan:

```bash
cargo run -- plan \
  --port /dev/cu.usbserial-xxxx \
  --file path/to/metaImage.bin \
  --meta-slot 1 \
  --json
```

Flash:

```bash
cargo run -- flash \
  --port /dev/cu.usbserial-xxxx \
  --file path/to/metaImage.bin \
  --meta-slot 1 \
  --erase true \
  --verify true
```

`download` is an alias for `flash`:

```bash
cargo run -- download --port /dev/cu.usbserial-xxxx --file path/to/metaImage.bin --json
```

JSON output:

```bash
cargo run -- flash \
  --port /dev/cu.usbserial-xxxx \
  --file path/to/metaImage.bin \
  --meta-slot 1 \
  --json
```

NDJSON progress events for AI agents:

```bash
cargo run -- flash \
  --port /dev/cu.usbserial-xxxx \
  --file path/to/metaImage.bin \
  --ndjson
```

## Hardware Notes

Put the board into the classic xWR UART ROM boot mode before flashing. The app
sends a UART break, pings the ROM bootloader, optionally erases serial flash,
opens the requested metaImage slot, writes 240-byte chunks, closes the image,
and checks bootloader status.

## Development

```bash
cargo fmt --all --check
cargo test
cargo check
```

## License

MIT.
