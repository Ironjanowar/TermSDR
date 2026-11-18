# 📡 TermSDR

> **CubicSDR-like experience, but in the terminal.**

[![Rust](https://img.shields.io/badge/Rust-1.87%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-557%20passing-brightgreen)](#testing)

A terminal-based Software Defined Radio application built with Rust and [ratatui](https://github.com/ratatui/ratatui). Real-time spectrum analysis, waterfall display, and AM/FM/NFM audio demodulation — in your terminal or in the browser via the built-in web frontend.

<!-- TODO: Add screenshot/GIF -->

---

## ✨ Features

- **Real-time spectrum + waterfall display** — dual-pane visualization with auto-scaling dB range
- **AM audio demodulation** — tune the cursor to a signal and hear it live; the cursor acts as your tuner
- **RTL-SDR hardware support** — use a real RTL-SDR dongle (compile with `--features rtlsdr`)
- **Multiple VFOs** — pin up to 8 independent Variable Frequency Oscillators, each with its own frequency, channel bandwidth, and color; all VFOs are mixed to mono output simultaneously
- **Command palette** (`/`) — VSCode-style command palette with autocompletion, parameterized commands, and error handling
- **Cursor system** — frequency cursor with active/inactive states, dB readout at cursor position
- **Channel bandwidth visualization** — dashed lines showing channel boundaries around cursor
- **Volume & squelch controls** — adjust volume with `v`/`V`, set squelch threshold via command palette
- **Configurable waterfall time span** — adjust how many seconds of history the waterfall shows (`t`/`T`, 5–120 s)
- **Synthetic test signals** — develop and test without hardware using built-in signal generator
- **🌐 WebSocket server mode** — run headless with `--headless` and control from any browser via the built-in Svelte web frontend
- **4-thread pipeline architecture** — Capture → DSP/FFT → Audio demod → TUI/WS for smooth, non-blocking operation
- **Source abstraction** — TestSource, File replay, and RTL-SDR hardware
- **Source selection screen** — interactive startup screen to pick your signal source (auto-detects RTL-SDR devices)
- **Frequency suffixes** — use `100M`, `118.1M`, `433.92k`, `1.42G` anywhere frequencies are expected

## 📦 Installation

### Prerequisites

- [Rust toolchain](https://rustup.rs/) (1.87+; required by Rubato 5.1)
- ALSA development libraries (Linux): `libasound2-dev` or equivalent
- **For RTL-SDR hardware support:** `librtlsdr-dev` (e.g. `sudo apt install librtlsdr-dev`)

### From Source

```bash
git clone https://github.com/Ironjanowar/TermSDR.git
cd TermSDR

# Without RTL-SDR hardware (test signals & file replay only)
cargo build --release

# With RTL-SDR hardware support
cargo build --release --features rtlsdr
```

The binary will be at `target/release/termsdr`.

### Pre-built Binaries

Check [GitHub Releases](https://github.com/Ironjanowar/TermSDR/releases) for pre-built binaries for Linux (x86_64, aarch64).

## 🚀 Usage

```bash
# Run with synthetic test signals (no hardware needed)
cargo run -- --test

# Headless WebSocket server mode (no TUI, access via browser)
cargo run -- --headless --test
cargo run -- --headless --test --port 9090
cargo run -- --headless --test --bind 127.0.0.1

# Tune to a specific frequency
cargo run -- --test -f 118.1M

# Set sample rate / bandwidth
cargo run -- --test -s 2.048M

# Set RF gain (dB)
cargo run -- --test --gain 30

# Replay a raw IQ file (format + rate + center must be given)
cargo run -- --file recording.cu8 --iq-format cu8 \
    --file-sample-rate 2048000 --file-center-frequency 118100000

# Replay a SigMF dataset pair (metadata comes from the .sigmf-meta sidecar)
cargo run -- --file capture.sigmf-meta

# With RTL-SDR hardware (requires --features rtlsdr)
cargo run --features rtlsdr

# Full example
cargo run -- --test -f 433.92M -s 1M --gain 20 --fft-size 4096

# Load the source-independent Barajas airband session
cargo run -- --test --config examples/barajas-session-v1.json

# Explicit receiver flags override only those fields from the session
cargo run -- --test --config examples/barajas-session-v1.json --frequency 119M
```

### CLI Options

| Flag | Description | Default |
|------|-------------|---------|
| `-f`, `--frequency` | Center frequency (e.g. `100M`, `118.1M`) | `100M` |
| `-s`, `--sample-rate` | Sample rate (e.g. `2.048M`, `1024000`) | `2.048M` |
| `-g`, `--gain` | RF gain in dB (omit for AGC) | AGC |
| `--fft-size` | FFT size (number of bins) | `2048` |
| `--ppm` | PPM crystal correction | `0` |
| `--test` | Use synthetic test signal source | — |
| `--headless` | Run as WebSocket server (no TUI) | — |
| `--port` | WebSocket server port | `8080` |
| `--bind` | WebSocket server bind address | `0.0.0.0` |
| `--no-web` | Don't serve web frontend (WS only) | — |
| `--audio-format` | Audio format: `opus` or `pcm` | `opus` |
| `--file` | IQ file path for replay mode (raw file, `.sigmf-meta`, or `.sigmf-data`) | — |
| `--iq-format` | Raw IQ sample format: `cu8` or `cf32le` | — |
| `--file-sample-rate` | Raw IQ sample rate in Hz | — |
| `--file-center-frequency` | Raw IQ center frequency in Hz | — |
| `-c`, `--config`, `--config-file` | Native JSON session path | — |

### Native sessions (JSON v1)

Native sessions are source-independent snapshots: choose the source with `--test`,
`--file`, or the startup source picker, then load the same session on top of it.
The file stores receiver tuning, cursor and pinned VFOs, volume/squelch, AGC,
bandpass, display, waterfall, and scan state. It intentionally does not store an
RTL-SDR device index, IQ file path, FFT frames, waterfall history, open dialogs,
or other transient runtime state.

At startup, `--config <path>` and `--config-file <path>` are equivalent. Values
from the session form the baseline; only receiver flags explicitly present on the
command line override it. The public
[`examples/barajas-session-v1.json`](examples/barajas-session-v1.json) profile is
a complete v1 example.

The top-level v1 schema is:

```text
version: 1
receiver: center_frequency_hz, sample_rate_hz, fft_size, gain_db, ppm
cursor: name, frequency_hz, mode, channel_bandwidth_hz, muted
vfos[]: id, name, frequency_hz, mode, channel_bandwidth_hz
audio: volume, global_squelch_dbfs, agc, bandpass
display: min_db, max_db, averaging, waterfall_span_seconds
scan: active, interval_ms
```

In the TUI, `/config-save <path>` atomically writes to the exact path (without
adding an extension), creating parent directories as needed. `/config-load
<path>` replaces the durable runtime state and updates the audio pipeline. Sample
rate, FFT size, RF gain, and PPM are hardware-bound and are reported as requiring
a restart when a session is loaded at runtime.

## ⌨️ Keyboard Shortcuts

### Navigation

| Key | Action |
|-----|--------|
| `←` / `→` | Move cursor ±1 kHz, whether audio is muted or unmuted; the viewport auto-pans at an edge |
| `Shift+←` / `Shift+→` | Move cursor ±100 kHz, whether audio is muted or unmuted; the viewport auto-pans at an edge |
| `PgUp` / `PgDn` | Move cursor ±1 MHz, whether audio is muted or unmuted; the viewport auto-pans at an edge |
| `f` | Enter frequency input mode |
| `+` / `-` | Zoom the display in / out around the cursor; never retunes the receiver |
| `c` | Center view on cursor without moving the cursor |

### Zooming the display

`+` / `-` and `b` change **what is drawn**, not what is captured. The receiver keeps
its sample rate, so audio never drops and the whole band is still there when you
zoom back out. The view is anchored on the cursor, and the axis stops shrinking at
four FFT bins — below that you would be magnifying interpolation, not signal. To see
a narrower span with more resolution, capture at a lower `-s` or raise `--fft-size`.

### Control

| Key | Action |
|-----|--------|
| `↑` / `↓` | Increase / decrease RF gain |
| `b` | Set the displayed bandwidth exactly (e.g. `20k`); never retunes the receiver |
| `w` / `W` | Widen / narrow channel bandwidth (×2 / ÷2) |
| `Space` | Mute/unmute cursor audio only |

### Audio & VFOs

| Key | Action |
|-----|--------|
| `v` / `V` | Volume up / down (+5 / -5, range 0–100) |
| `m` | Pin a VFO at current cursor position (max 8) |
| `x` | Open VFO delete modal (↑↓ to select, Enter to remove) |

### Waterfall

| Key | Action |
|-----|--------|
| `t` / `T` | Increase / decrease waterfall time span by 5 s (range 5–120 s) |

### General

| Key | Action |
|-----|--------|
| `/` | **Open command palette** |
| `?` | Show help overlay |
| `q` / `Esc` | Quit |
| `Ctrl+C` | Force quit |

## 🎯 Command Palette

Press `/` to open the command palette — a powerful way to execute any action with autocompletion and parameterized input.

### How it works

1. Press `/` to open the palette
2. Start typing to filter commands (case-insensitive, substring match)
3. Use `↑`/`↓` to navigate the filtered list
4. Press `Tab` to autocomplete the selected command
5. Press `Enter` to execute
6. Press `Esc` to close

### Commands (22 total)

| Command | Description | Parameter | Hotkey |
|---------|-------------|-----------|--------|
| `bandwidth` | Set sample rate/bandwidth | `<value>` (required, e.g. `2M`, `500k`) | `b` |
| `center` | Center view on cursor | — | `c` |
| `channelbw` | Set channel bandwidth | `<value>` (optional, e.g. `25k`, `50k`) | `w`/`W` |
| `config-load` | Load a native session | `<path>` (required) | — |
| `config-save` | Atomically save a native session | `<path>` (required) | — |
| `frequency` | Jump to frequency | `<value>` (required, e.g. `118.1M`) | `f` |
| `gain+` | Increase gain by 1 dB | — | `↑` |
| `gain-` | Decrease gain by 1 dB | — | `↓` |
| `help` | Show keyboard shortcuts | — | `?` |
| `output` | Select audio output device | — | — |
| `quit` | Quit application | — | `q` |
| `squelch` | Set squelch threshold (dB) | `<dB>` (optional; omit to disable) | — |
| `toggle-cursor` | Mute/unmute cursor audio | — | `Space` |
| `tune+` | Move cursor up 1 kHz | — | `→` |
| `tune-` | Move cursor down 1 kHz | — | `←` |
| `tune-coarse+` | Move cursor up 100 kHz | — | `Shift+→` |
| `tune-coarse-` | Move cursor down 100 kHz | — | `Shift+←` |
| `vfo-add` | Add VFO at frequency | `<freq>` (required, e.g. `118.1M`) | `m` |
| `vfo-clear` | Remove all VFOs | — | — |
| `vfo-remove` | Remove VFO by ID | `<id>` (required) | `x` |
| `volume` | Set volume (0–100) | `<value>` (required) | `v`/`V` |
| `waterfall` | Set waterfall time span | `<seconds>` (required, 5–120) | `t`/`T` |
| `zoom-in` | Zoom the display in around the cursor | — | `+` |
| `zoom-out` | Zoom the display out around the cursor | — | `-` |

### Parameterized Commands

Commands that accept parameters let you set exact values:

```
/frequency 118.1M     → Jump to 118.1 MHz
/channelbw 50k        → Set channel bandwidth to 50 kHz
/bandwidth 500k       → Set sample rate to 500 kHz
/volume 80            → Set volume to 80%
/squelch -60          → Set squelch threshold to -60 dB
/squelch              → Disable squelch
/waterfall 60         → Set waterfall to show 60 seconds of history
/vfo-add 118.1M       → Pin a VFO at 118.1 MHz
/vfo-remove 3         → Remove VFO #3
/vfo-clear            → Remove all VFOs
/config-load profiles/airband.json → Replace runtime state from a session
/config-save profiles/airband.json → Save runtime state to the exact path
```

Frequency suffixes supported: `M` (MHz), `k` (kHz), `G` (GHz), or raw Hz.

### Error Handling

- Commands that **require** a parameter (`frequency`, `bandwidth`, `volume`, etc.) will show an error if invoked without one
- Invalid parameter values (e.g. `frequency abc`) show a descriptive error message
- Errors are displayed in the palette — the palette stays open so you can correct and retry
- Errors clear automatically when you type or delete

## 📡 Multiple VFOs

TermSDR supports up to **8 simultaneous VFOs** (Variable Frequency Oscillators), letting you monitor multiple frequencies at once within your visible bandwidth.

### Workflow

1. **Tune** the cursor to a frequency of interest
2. Press `m` to **pin a VFO** at the cursor position — it captures the current frequency and channel bandwidth
3. Each VFO gets a unique **color** (Red, Green, Blue, Magenta, Cyan, etc.) and **ID number** (1–8)
4. VFOs appear as colored vertical lines with bandwidth markers on both the **spectrum** and **waterfall** displays
5. All pinned VFOs are **demodulated simultaneously** and mixed to mono audio output
6. Press `x` to open the **delete modal**, move with `↑`/`↓` and press `Enter` to remove the highlighted pin

### VFO Management

- **Pin:** `m` (at cursor position) or `/vfo-add 118.1M` (at specific frequency)
- **Delete one:** `x` → press number, or `/vfo-remove 3`
- **Delete all:** `/vfo-clear`
- **Max 8** VFOs at a time; color wraps if you somehow exceed the palette

## 🏗️ Architecture

TermSDR uses a **4-thread pipeline** for smooth real-time operation:

**TUI Mode** (default):
```
┌─────────────┐      ┌─────────────┐      ┌─────────────┐      ┌─────────────┐
│   Capture   │ IQ → │   DSP/FFT   │ dB → │    Audio    │      │     TUI     │
│  (Thread 1) │      │  (Thread 2) │      │  (Thread 3) │      │  (Thread 4) │
└─────────────┘      └─────────────┘      └─────────────┘      └─────────────┘
```

**Headless Mode** (`--headless`):
```
┌─────────────┐      ┌─────────────┐      ┌─────────────┐      ┌─────────────┐
│   Capture   │ IQ → │   DSP/FFT   │ dB → │    Audio    │      │  WS Server  │
│  (Thread 1) │      │  (Thread 2) │      │  (Thread 3) │      │  (Thread 4) │
└─────────────┘      └─────────────┘      └─────────────┘      └─────────────┘
                                           Opus/PCM →            ↕ WebSocket
                                           WebSocketSink         ↕ HTTP (Svelte)
```

- **Thread 1 — Capture**: Reads IQ samples from the configured source (TestSource, File, or RTL-SDR) at the specified sample rate
- **Thread 2 — DSP**: Applies Hann windowing and computes FFT, converts to dB power spectrum
- **Thread 3 — Audio**: AM demodulation, per-VFO channel extraction, mono mix, volume/squelch, output via cpal (TUI) or WebSocketSink (headless)
- **Thread 4 — TUI/WS**: Renders spectrum + waterfall display (TUI mode), or runs axum WebSocket server serving the Svelte frontend and streaming data to connected clients (headless mode)

Threads communicate via bounded `crossbeam-channel` queues with non-blocking sends to prevent backpressure.

### Source Abstraction

All signal sources implement the `Source` trait:

| Source | Status | Description |
|--------|--------|-------------|
| `TestSource` | ✅ Ready | Synthetic signals with configurable peaks + noise floor |
| `FileSource` | ✅ Ready | Deterministic replay of raw IQ or SigMF datasets |
| `RtlSdrSource` | ✅ Ready | Real hardware via librtlsdr (requires `--features rtlsdr`) |

### Project Structure

```
src/
├── main.rs                 # CLI, startup, thread orchestration
├── types.rs                # IqSample, FftResult, AppConfig, SourceType
├── config.rs               # CLI parsing (clap) + frequency parser
├── lib.rs                  # Module declarations
├── audio/
│   ├── mod.rs              # Audio control, VFO management, pipeline
│   ├── demod.rs            # AM/FM/NFM demodulation
│   ├── cpal_sink.rs        # cpal audio output (TUI mode)
│   ├── ws_sink.rs          # WebSocket audio output (headless mode)
│   └── test_signal.rs      # Audio test utilities
├── capture/
│   ├── mod.rs              # Source trait + dispatch
│   ├── test_signal.rs      # Synthetic signal generator
│   ├── file.rs             # IQ file replay
│   └── rtlsdr.rs           # RTL-SDR hardware
├── dsp/
│   ├── mod.rs              # DSP pipeline
│   └── fft.rs              # FFT + Hann window + dB conversion
├── server/
│   ├── mod.rs              # axum server, spectrum/audio broadcast
│   ├── ws.rs               # WebSocket handler, client registry
│   ├── protocol.rs         # Binary frame encoding, JSON message types
│   └── commands.rs         # Command dispatcher, shared state
└── ui/
    ├── mod.rs              # App state, event loop, key handling, VFO management
    ├── command_palette.rs  # Command palette: registry, state, execution (22 commands)
    ├── spectrum.rs         # Spectrum display widget
    ├── waterfall.rs        # Waterfall display widget
    ├── colors.rs           # Color utilities
    └── status_bar.rs       # Status bar widget
```

## 🌐 WebSocket Server Mode (Headless)

TermSDR can run as a **headless WebSocket server**, streaming spectrum and audio data to a web browser. No terminal needed — control everything from the built-in Svelte frontend.

### Quick Start

```bash
# 1. Build the backend
cargo build --release

# 2. Build the web frontend
cd web
npm install
npm run build
cd ..

# 3. Run headless with test signal
./target/release/termsdr --headless --test

# 4. Open browser at http://localhost:8080
```

### Headless CLI Options

| Flag | Description | Default |
|------|-------------|----------|
| `--headless` | Run as WebSocket server (no TUI) | — |
| `--port` | Server port | `8080` |
| `--bind` | Bind address | `0.0.0.0` |
| `--no-web` | Don’t serve frontend (WS endpoint only) | — |
| `--audio-format` | Audio encoding: `opus` or `pcm` | `opus` |

### Examples

```bash
# Test signal, default port
termsdr --headless --test

# Aviation band with RTL-SDR, custom port
termsdr --headless --features rtlsdr -f 118.1M --port 9090

# Local-only access (not exposed to network)
termsdr --headless --test --bind 127.0.0.1

# WS endpoint only (bring your own frontend)
termsdr --headless --test --no-web

# PCM raw audio (lower latency on LAN, higher bandwidth)
termsdr --headless --test --audio-format pcm
```

### What the Web Frontend Provides

- 📊 **Real-time spectrum graph** — Canvas 2D, click to tune, scroll to zoom
- 🌊 **Waterfall heatmap** — Viridis palette, drag to pan, click to tune
- 🔊 **Live audio playback** — Web Audio API, PCM streaming (Opus planned)
- 🏛️ **Full controls** — frequency input (with M/k/G suffixes), gain, volume, demod mode
- ⚙️ **Settings modal** — AGC, bandpass filter, channel BW, squelch — every field has both slider and text input for exact values
- 📍 **VFO management** — add/remove VFOs from the browser
- 🔄 **Auto-reconnect** — exponential backoff, automatic state refresh
- 👥 **Multi-client** — multiple browsers can connect simultaneously with shared control

### Protocol

The WebSocket protocol is fully documented in [`PROTOCOL.md`](PROTOCOL.md). Third-party clients (React, Flutter, native apps) can connect to the same WS endpoint — the Svelte frontend is just the reference implementation.

---

## 🛠️ Development Setup

### Backend (Rust)

```bash
# Build
cargo build

# Build with RTL-SDR support
cargo build --features rtlsdr

# Build with Opus audio encoding
cargo build --features opus

# Run tests
cargo test

# Lint
cargo clippy

# Format
cargo fmt
```

**System dependencies:**
- `libasound2-dev` (Debian/Ubuntu) or `alsa-lib` (Arch) — required for audio
- `librtlsdr-dev` — only for `--features rtlsdr`
- `libopus-dev` — only for `--features opus`

### Frontend (Svelte + TypeScript)

```bash
cd web

# Install dependencies
npm install

# Development server (hot reload)
npm run dev

# Production build (output: web/dist/)
npm run build

# Preview production build
npm run preview
```

The production build outputs to `web/dist/` which the Rust backend serves automatically at `http://host:port/` in headless mode.

**Frontend tech stack:**
- [Svelte 5](https://svelte.dev/) with `$state` / `$derived` runes
- [TypeScript](https://www.typescriptlang.org/) (strict mode)
- [Vite 6](https://vitejs.dev/) (build tool)
- Canvas 2D for spectrum/waterfall rendering
- Web Audio API for audio playback
- No external runtime dependencies

### Frontend Project Structure

```
web/
├── src/
│   ├── main.ts                 # Entry point
│   ├── App.svelte              # Main layout + wiring
│   ├── lib/
│   │   ├── protocol.ts         # WS protocol types + binary frame parser
│   │   ├── ws.svelte.ts        # WebSocket client + auto-reconnect
│   │   ├── state.svelte.ts     # Reactive global state (synced with server)
│   │   └── audio.ts            # Web Audio API player + ring buffer
│   └── components/
│       ├── Spectrum.svelte     # Real-time spectrum graph (Canvas 2D)
│       ├── Waterfall.svelte    # Scrolling waterfall heatmap (Canvas 2D)
│       ├── Header.svelte       # Top bar (freq, mode, connection status)
│       ├── Controls.svelte     # Bottom controls bar
│       ├── FreqInput.svelte    # Frequency input with suffix parsing
│       ├── SettingsModal.svelte # Full settings panel (⚙️)
│       ├── SettingsField.svelte # Reusable slider + text input field
│       ├── Toggle.svelte       # Reusable toggle switch
│       └── AudioControl.svelte # Audio status + mute toggle
├── index.html
├── package.json
├── vite.config.ts
├── tsconfig.json
└── svelte.config.js
```

---

## 📡 Building & Running (All Modes)

### TUI Mode (default — terminal interface)

```bash
cargo build --release
./target/release/termsdr --test              # Test signal
./target/release/termsdr --features rtlsdr   # Real hardware
```

### Headless + Web Mode

```bash
# Build everything
cargo build --release
cd web && npm install && npm run build && cd ..

# Run
./target/release/termsdr --headless --test
# Open http://localhost:8080 in your browser
```

### Headless for Raspberry Pi / Remote Server

```bash
# On the Pi/server:
termsdr --headless --features rtlsdr -f 118.1M

# On your laptop/phone:
# Open http://<pi-ip>:8080 in the browser
```

---

## 🔧 Building

```bash
# Debug build
cargo build

# Release build (optimized)
cargo build --release

# With RTL-SDR support
cargo build --release --features rtlsdr

# Run directly
cargo run -- --test
```

## 🧪 Testing

TermSDR follows **Test-Driven Development (TDD)**. The synthetic `TestSource` generates IQ data with known properties, enabling full pipeline testing without hardware.

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run specific test module
cargo test command_palette
```

Currently: **557 tests** (553 unit + 4 integration) covering types, FFT, capture, UI state, command palette, audio demodulation (AM/FM/NFM), AGC, bandpass filter, VFOs, bookmarks, WebSocket server, protocol encoding/decoding, and end-to-end pipeline validation.

## 🗺️ Roadmap

### Done ✅
- 📊 Real-time spectrum + waterfall display
- 🎯 Command palette with 22 commands
- 🔊 AM audio demodulation (cursor = tuner)
- 📻 FM & NFM demodulation (broadcast radio + walkie-talkies/PMR446)
- 🔌 RTL-SDR hardware integration
- 📍 Multiple VFOs (up to 8, simultaneous demod)
- 🔈 Volume & squelch controls
- 🎛️ Audio settings modal (AGC, bandpass filter, per-parameter controls)
- 📡 Auto-scan peak hunting mode
- 📚 Bookmarks — frequency catalog + personal bookmarks + CubicSDR import
- ⏱️ Configurable waterfall time span (5–120 s)
- 📼 IQ file replay — raw `cu8`/`cf32le` and SigMF datasets
- 🌐 WebSocket server mode — headless backend + Svelte web frontend ([protocol docs](PROTOCOL.md))

### Future
- 📻 SSB demodulation (ham radio, maritime)
- 💾 Recording to IQ files
- 📡 Full CubicSDR session import (device config, zoom, active demodulators)
- 🎨 Configurable waterfall color palettes
- 💾 VFO/bookmark persistence across sessions

## 📄 License

[MIT](LICENSE)

## 🤝 Contributing

Contributions welcome! This project follows TDD — please include tests with any new features.

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/my-feature`)
3. Write tests first, then implement
4. Run `cargo test` and `cargo clippy` to verify
5. Submit a pull request

### Code Style

- `cargo fmt` for formatting
- `cargo clippy` for lints
- Tests live next to code (`#[cfg(test)] mod tests`)
- Use `Result<T>` with `anyhow` — no `unwrap()` in production code
