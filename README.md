# 📡 TermSDR

> **CubicSDR-like experience, but in the terminal.**

[![Rust](https://img.shields.io/badge/Rust-1.70%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-199%20passing-brightgreen)](#testing)

A terminal-based Software Defined Radio application built with Rust and [ratatui](https://github.com/ratatui/ratatui). Real-time spectrum analysis and waterfall display right in your terminal.

<!-- TODO: Add screenshot/GIF -->

---

## ✨ Features

- **Real-time spectrum + waterfall display** — dual-pane visualization with auto-scaling dB range
- **Command palette** (`/`) — VSCode-style command palette with autocompletion, parameterized commands, and error handling
- **Cursor system** — frequency cursor with active/inactive states, dB readout at cursor position
- **Channel bandwidth visualization** — dashed lines showing channel boundaries around cursor
- **Synthetic test signals** — develop and test without hardware using built-in signal generator
- **3-thread pipeline architecture** — Capture → DSP/FFT → TUI for smooth, non-blocking operation
- **Source abstraction** — TestSource, File replay, and RTL-SDR (hardware support in progress)
- **Frequency suffixes** — use `100M`, `118.1M`, `433.92k`, `1.42G` anywhere frequencies are expected

## 📦 Installation

### From Source

Requires [Rust toolchain](https://rustup.rs/) (1.70+):

```bash
git clone https://github.com/Ironjanowar/TermSDR.git
cd TermSDR
cargo build --release
```

The binary will be at `target/release/termsdr`.

### Pre-built Binaries

Check [GitHub Releases](https://github.com/Ironjanowar/TermSDR/releases) for pre-built binaries for Linux (x86_64, aarch64).

## 🚀 Usage

```bash
# Run with synthetic test signals (no hardware needed)
cargo run -- --test

# Tune to a specific frequency
cargo run -- --test -f 118.1M

# Set sample rate / bandwidth
cargo run -- --test -s 2.048M

# Set RF gain (dB)
cargo run -- --test --gain 30

# Replay from IQ file
cargo run -- --file recording.iq -f 100M

# Full example
cargo run -- --test -f 433.92M -s 1M --gain 20 --fft-size 4096
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
| `--file` | IQ file path for replay mode | — |
| `-c`, `--config` | JSON config file path | — |

## ⌨️ Keyboard Shortcuts

### Navigation

| Key | Action |
|-----|--------|
| `←` / `→` | Move cursor (cursor active) / Fine tune ±25 kHz (cursor inactive) |
| `PgUp` / `PgDn` | Coarse tune ±1 MHz |
| `f` | Enter frequency input mode |
| `+` / `-` | Zoom in / out (adjust bandwidth) |

### Control

| Key | Action |
|-----|--------|
| `↑` / `↓` | Increase / decrease RF gain |
| `b` | Enter bandwidth input mode |
| `w` / `W` | Widen / narrow channel bandwidth (×2 / ÷2) |
| `c` | Center cursor on screen |
| `Space` | Toggle cursor active / inactive |
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

### Commands

| Command | Description | Parameter | Hotkey |
|---------|-------------|-----------|--------|
| `bandwidth` | Set sample rate/bandwidth | `<value>` (required, e.g. `2M`, `500k`) | `b` |
| `center` | Center cursor on screen | — | `c` |
| `channelbw` | Set channel bandwidth | `<value>` (optional, e.g. `25k`, `50k`) | `w`/`W` |
| `frequency` | Jump to frequency | `<value>` (required, e.g. `118.1M`) | `f` |
| `gain+` | Increase gain by 1 dB | — | `↑` |
| `gain-` | Decrease gain by 1 dB | — | `↓` |
| `help` | Show keyboard shortcuts | — | `?` |
| `quit` | Quit application | — | `q` |
| `toggle-cursor` | Toggle cursor on/off | — | `Space` |
| `tune+` | Tune up 25 kHz | — | `←` |
| `tune-` | Tune down 25 kHz | — | `→` |
| `tune-coarse+` | Tune up 1 MHz | — | `PgUp` |
| `tune-coarse-` | Tune down 1 MHz | — | `PgDn` |
| `zoom-in` | Zoom in (decrease bandwidth) | — | `+` |
| `zoom-out` | Zoom out (increase bandwidth) | — | `-` |

### Parameterized Commands

Commands that accept parameters let you set exact values:

```
/frequency 118.1M     → Jump to 118.1 MHz
/channelbw 50k        → Set channel bandwidth to 50 kHz
/bandwidth 500k       → Set sample rate to 500 kHz
```

Frequency suffixes supported: `M` (MHz), `k` (kHz), `G` (GHz), or raw Hz.

### Error Handling

- Commands that **require** a parameter (`frequency`, `bandwidth`) will show an error if invoked without one
- Invalid parameter values (e.g. `frequency abc`) show a descriptive error message
- Errors are displayed in the palette — the palette stays open so you can correct and retry
- Errors clear automatically when you type or delete

## 🏗️ Architecture

TermSDR uses a **3-thread pipeline** for smooth real-time operation:

```
┌─────────────┐      ┌─────────────┐      ┌─────────────┐
│   Capture   │ IQ → │   DSP/FFT   │ dB → │     TUI     │
│  (Thread 1) │      │  (Thread 2) │      │  (Thread 3) │
└─────────────┘      └─────────────┘      └─────────────┘
```

- **Thread 1 — Capture**: Reads IQ samples from the configured source (TestSource, File, or RTL-SDR) at the specified sample rate
- **Thread 2 — DSP**: Applies Hann windowing and computes FFT, converts to dB power spectrum
- **Thread 3 — TUI**: Renders spectrum + waterfall display, handles keyboard input at ~30 FPS

Threads communicate via bounded `crossbeam-channel` queues with non-blocking sends to prevent backpressure.

### Source Abstraction

All signal sources implement the `Source` trait:

| Source | Status | Description |
|--------|--------|-------------|
| `TestSource` | ✅ Ready | Synthetic signals with configurable peaks + noise floor |
| `FileSource` | 🔲 Stub | Replay from IQ recordings |
| `RtlSdrSource` | 🔲 Stub | Real hardware via librtlsdr |

### Project Structure

```
src/
├── main.rs                 # CLI, startup, thread orchestration
├── types.rs                # IqSample, FftResult, AppConfig, SourceType
├── config.rs               # CLI parsing (clap) + frequency parser
├── lib.rs                  # Module declarations
├── capture/
│   ├── mod.rs              # Source trait + dispatch
│   ├── test_signal.rs      # Synthetic signal generator
│   ├── file.rs             # IQ file replay (stub)
│   └── rtlsdr.rs           # RTL-SDR hardware (stub)
├── dsp/
│   ├── mod.rs              # DSP pipeline
│   └── fft.rs              # FFT + Hann window + dB conversion
└── ui/
    ├── mod.rs              # App state, event loop, key handling
    ├── command_palette.rs  # Command palette: registry, state, execution
    ├── spectrum.rs         # Spectrum display widget
    ├── waterfall.rs        # Waterfall display widget
    ├── colors.rs           # Color utilities
    └── status_bar.rs       # Status bar widget
```

## 🔧 Building

```bash
# Debug build
cargo build

# Release build (optimized)
cargo build --release

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

Currently: **199 tests** (195 unit + 4 integration) covering types, FFT, capture, UI state, command palette, and end-to-end pipeline validation.

## 🗺️ Roadmap

### v0.2 (Next)
- 🔊 Audio demodulation (AM for aviation, FM for broadcast)
- 🔌 RTL-SDR hardware integration (requires `librtlsdr-dev`)
- 📼 IQ file replay source

### Future
- 🌐 WebSocket server mode (headless backend + web frontend)
- 📍 Multiple VFOs
- 💾 Recording to IQ files
- 📡 CubicSDR session import

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
