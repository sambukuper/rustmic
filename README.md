# rustmic

WO Mic client rewritten in Rust. Talks to the WO Mic app on an Android phone
and plays its microphone stream on the computer, without the original
Windows client.

Install (Windows, PowerShell):

```powershell
irm https://raw.githubusercontent.com/sambukuper/rustmic/main/scripts/install.ps1 | iex
```

Install (Linux):

```sh
curl -fsSL https://raw.githubusercontent.com/sambukuper/rustmic/main/scripts/install.sh | sh
```

Both commands fetch the latest release, unpack the client into a user
directory and add it to the PATH. The Linux client can also expose the
phone's microphone as a system-wide virtual microphone:

```sh
rustmic virtual-mic --transport wifi --host 192.168.1.178
```

The protocol was recovered from the shipped client binary; every constant
and message layout is documented with the disassembly evidence in
[docs/ANALYSIS.md](docs/ANALYSIS.md).

## Status

Works, tested against a mock server that speaks the recovered protocol end
to end (TCP control handshake, UDP media, Opus decode). Nobody has run it
against a real phone yet; what the analysis did not verify is listed in
[docs/ANALYSIS.md](docs/ANALYSIS.md#not-verified-against-a-live-server).

## Build

Rust 1.74 or newer. Windows: MSVC toolchain. Linux: ALSA headers for audio
output (`libasound2-dev` on Debian/Ubuntu).

```sh
cargo build --release
```

The build compiles a static libopus through CMake, so CMake and a C
toolchain must be available. `.cargo/config.toml` already sets
`CMAKE_POLICY_VERSION_MINIMUM` for CMake 4.x.

Both front ends run on Windows and Linux; on Linux the GUI works under
Wayland and X11 (audio goes through ALSA, which PipeWire provides through
its ALSA interface). Pushing a `v*` tag builds release bundles for both
platforms through the `release` workflow.

## Usage

Two front ends, same pipeline:

* `rustmic-gui` opens a window in the spirit of the original client: a
  Connection menu (Connect / Disconnect / Exit), connection fields, an About
  dialog and a status bar that shows the state on the left and the transport
  on the right. It reads and writes the same `settings.dat`, and its
  `Connect when the client starts` option matches the original
  `autoConnect` flag.
* `rustmic` is the command line client described below.

Start the WO Mic app on the phone, pick a transport and connect:

```sh
# Wi-Fi (phone and PC in the same network)
rustmic --transport wifi --host 192.168.1.178

# USB (adb in PATH, or point --adb at it)
rustmic --transport usb

# Bluetooth (phone must be paired; rustmic lists paired devices)
rustmic --list-bt-devices
rustmic --transport bluetooth --bt-address AA:BB:CC:DD:EE:FF
```

Values not given on the command line come from a `settings.dat` file next to
the executable or in the current directory. The format is the same as the
original client's, so an existing configuration works as is.

More options:

* `--stdout` writes the decoded PCM (16 bit little endian mono 48 kHz) to
  stdout instead of playing it, for piping into ffmpeg, a virtual audio
  cable or anything else.
* `--device substring` selects an output device; `--list-devices` lists
  them.
* `--buffer N` sets the jitter buffer depth (default 3 packets, the
  original `bufferSize`).
* `--reconnect 0` disables the automatic reconnect after a broken stream.
* `--stats N` prints packet and latency statistics every N seconds.

Exit codes match the original client's error codes: 6 = connection refused,
7 = version mismatch, 8 = stream could not start, 21 = no audio data.

## Transports

| transport | control channel | media channel | notes |
|-----------|-----------------|---------------|-------|
| `wifi` | TCP 1488 | UDP 49152 | same LAN |
| `wifi-direct` | TCP | UDP | phone as hotspot, `softApAddr` |
| `usb` | TCP via `adb forward` | same connection | one forwarded port |
| `bluetooth` | RFCOMM | same connection | channels 1..=30 scanned, `--bt-channel` to dial directly |

Bluetooth and USB carry control and media over a single connection, exactly
like the original client; only Wi-Fi uses a second channel.

## Repository layout

```text
crates/rustmic-core        protocol, framing, settings, jitter buffer, stats
crates/rustmic-transport   TCP, UDP, adb and Bluetooth (RFCOMM) transports
crates/rustmic-app         the womic binary: CLI, Opus decoder, audio output
crates/rustmic-gui         the rustmic-gui binary: desktop window (egui)
docs/ANALYSIS.md         disassembly evidence for every protocol detail
```

## License

GPL-3.0. The protocol knowledge comes from analyzing the shipped Windows
client for interoperability; no original code was copied.
