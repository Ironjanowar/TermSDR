# AGENTS.md — TermSDR Development Guide

## Project Overview

**TermSDR** is a terminal-based Software-Defined Radio application written in Rust.
It renders spectrum and waterfall displays via `ratatui`, processes IQ data through an FFT pipeline,
demodulates AM/FM/NFM audio, and exposes a WebSocket server mode for remote control.

**Repo:** <https://github.com/Ironjanowar/TermSDR>

## Tech Stack & Key Dependencies

| Crate | Purpose | Version |
|---|---|---|
| `ratatui` + `crossterm` | TUI rendering & terminal events | 0.29 / 0.28 |
| `rustfft` + `num-complex` | FFT computation, IQ sample type | 6 / 0.4 |
| `cpal` | Audio output (system audio sink) | 0.15 |
| `axum` (+ `tokio`) | WebSocket server mode | 0.8 / 1 |
| `crossbeam-channel` | Lock-free inter-thread messaging | 0.5 |
| `clap` (derive) | CLI argument parsing | 4 |
| `serde` + `serde_json` | Config serialization, WS protocol | 1 |
| `anyhow` | Application-level error handling | 1 |
| `opus` (optional) | Opus audio encoding for WS | 0.3 |
| `rtlsdr` (optional) | RTL-SDR hardware support | 0.1 |
| `approx` (dev) | Floating-point assertions in tests | 0.5 |

## Project Structure

```
src/
├── main.rs              # CLI args, startup, thread orchestration
├── lib.rs               # Public module declarations
├── types.rs             # Shared types: IqSample, FftResult, AppConfig
├── config.rs            # CLI parsing (clap) + JSON config loading
├── bookmarks.rs         # Frequency bookmarks persistence
├── capture/
│   ├── mod.rs           # Source trait + dispatch
│   ├── rtlsdr.rs        # RTL-SDR hardware (feature-gated)
│   ├── file.rs          # IQ file replay
│   └── test_signal.rs   # Synthetic signal generator (TDD workhorse)
├── dsp/
│   ├── mod.rs           # DSP pipeline orchestration
│   └── fft.rs           # FFT, windowing, magnitude → dB
├── audio/
│   ├── mod.rs           # Audio pipeline: demod → sink routing
│   ├── demod.rs         # AM/FM/NFM demodulation, FIR, AGC, DC removal
│   ├── cpal_sink.rs     # System audio output via cpal
│   ├── ws_sink.rs       # WebSocket audio streaming sink
│   └── test_signal.rs   # Audio test signal generators
├── server/
│   ├── mod.rs           # Server bootstrap + tokio runtime
│   ├── ws.rs            # WebSocket connection handler + state
│   ├── commands.rs      # WS command parsing + dispatch
│   └── protocol.rs      # Binary/JSON protocol definitions
└── ui/
    ├── mod.rs           # App state + event loop
    ├── spectrum.rs      # Spectrum display widget
    ├── waterfall.rs     # Waterfall display widget
    ├── colors.rs        # Color palette definitions
    ├── status_bar.rs    # Status bar widget
    ├── command_palette.rs # Command palette overlay
    └── settings_modal.rs # Settings modal overlay
tests/
    └── integration.rs   # Cross-module integration tests
```

## Philosophy: Test-Driven Development (TDD)

**Tests first. Always.**

1. Write a failing test that describes the desired behavior
2. Write the minimum code to make it pass
3. Refactor with confidence — tests catch regressions

### Why TDD Works Here

We have a superpower: **synthetic test signals**. `TestSource` and `audio::test_signal` generate IQ/audio data with known properties (noise floor, signal peaks, modulation parameters). This means we can test the entire DSP pipeline — from signal generation through FFT to dB output — without hardware.

Use this. Every new feature should have a test that exercises it with synthetic data before touching real SDR hardware.

## Commands

```bash
# Essentials
cargo build                    # Debug build
cargo test                     # Run all tests (~555 tests)
cargo test -- --nocapture      # Tests with stdout visible
cargo test <module>::tests     # Run module-specific tests (e.g., audio::demod::tests)
cargo clippy -- -D warnings    # Lint — treat all warnings as errors
cargo fmt -- --check           # Format check (CI mode)
cargo fmt                      # Auto-format

# Running
cargo run                                  # Defaults (test signal)
cargo run -- --test                        # Explicit test signal mode
cargo run -- -f 118M -s 2.048M             # Custom frequency
cargo run -- --headless --server --port 8080  # WebSocket server mode

# Feature-gated builds
cargo build --features rtlsdr              # With RTL-SDR support
cargo build --features opus                # With Opus encoding
cargo build --no-default-features          # Without RTL-SDR
```

## Rust Coding Conventions

### Naming
- `snake_case` for functions, variables, modules
- `PascalCase` for types, traits, enums
- `SCREAMING_SNAKE_CASE` for constants
- Descriptive names: `calculate_fft_magnitude()` not `calc()`

### Error Handling
- Use `Result<T>` with `anyhow` for application-level errors (main, config, capture, dsp, audio)
- Use `thiserror` with typed error enums in `server/` (distinguish protocol vs connection vs invalid command errors for proper WS responses)
- Propagate errors with `?` — never swallow them silently
- **No `.unwrap()` in production code** — use `?`, `.context()`, or `.expect("invariant: ...")`
- `.unwrap()` and `.expect()` are fine in tests
- Add context with `anyhow::Context`: `.context("failed to open IQ file")?`

#### Server Error Pattern

```rust
// src/server/error.rs
#[derive(thiserror::Error, Debug)]
pub enum ServerError {
    #[error("invalid command: {0}")]
    InvalidCommand(String),
    #[error("connection error: {0}")]
    Connection(#[from] axum::Error),
    #[error("encoding error: {0}")]
    Encoding(String),
}
```

This allows WS handlers to match on error variants and send appropriate error responses to clients.

### Ownership & Borrowing
- Prefer borrowing (`&T`, `&mut T`) over cloning
- Use `&str` over `String` in function parameters when ownership isn't needed
- Prefer iterators + combinators over manual loops when they're clearer
- Return early to reduce nesting

### Type System
- Use newtypes to distinguish semantically different values of the same type
- Derive `Debug` on all types; derive `Clone`, `PartialEq` where sensible
- Prefer `Option<T>` over sentinel values
- Use the type system to enforce invariants at compile time

### Code Organization
- Keep functions focused on a single responsibility
- Separate business/DSP logic from I/O (rendering, network, file access)
- **Minimum visibility principle:** use `pub(crate)` by default; only use `pub` for items that genuinely need to be accessible outside the crate. Refactor existing `pub` to `pub(crate)` gradually.
- Tests live next to code: `#[cfg(test)] mod tests { ... }` in each module

### Documentation
- **Doc comments (`///`) are mandatory on all public items** — functions, structs, enums, traits, methods
- Include `# Examples` in doc comments for non-trivial APIs
- Module-level docs (`//!`) at the top of each module file
- Keep comments up-to-date; delete stale comments rather than leaving them wrong
- DSP functions must document units (Hz, dB, samples) and expected value ranges

### Logging
- Use `tracing` (`tracing::info!`, `tracing::debug!`, `tracing::warn!`, `tracing::error!`) — never `println!`/`eprintln!` in production code
- `tracing-subscriber` with file appender for log output (no stdout — TUI owns the terminal)
- Filter with `RUST_LOG=termsdr=debug` environment variable
- `println!` is acceptable only in tests (with `--nocapture`)

### Performance Awareness
- Profile before optimizing — `cargo bench` with `criterion`
- Be mindful of allocations in hot paths (FFT loop, audio pipeline)
- Use `crossbeam-channel` for inter-thread communication (already in place)
- Prefer stack allocation for fixed-size DSP buffers where practical

## Testing Approach

| Layer | Strategy |
|-------|----------|
| Types | Construction, conversion, default values |
| Capture (TestSource) | Known signal properties, sample count, trait compliance |
| DSP/FFT | Pure tone → correct bin, flat noise floor, dB accuracy, windowing |
| Audio/Demod | AM/FM demod round-trip, FIR filter shapes, AGC behavior, DC removal |
| Server/Protocol | **Unit tests**: command parsing (`commands.rs`), protocol encode/decode (`protocol.rs`). **Integration tests**: real WS client with `tokio-tungstenite` in `tests/integration.rs` (connect, send command, verify response) |
| Integration | TestSource → FFT → verify peak at expected frequency |
| UI | **Smoke tests** with `ratatui::backend::TestBackend` (widget renders without panic, layout correctness). **Visual validation** through manual testing. |

### Testing Principles

- **FIRST**: Fast, Independent, Repeatable, Self-validating, Timely
- **One assertion concept per test** — test names describe the behavior being verified
- **Use `approx` for floating-point comparisons** — never `assert_eq!` on `f32`/`f64`
- **Test edge cases explicitly** — empty buffers, zero-length signals, single samples
- **Parametrized tests**: Use `#[test]` with helper functions or consider `rstest` for table-driven tests if patterns emerge
- **Property-based testing with `proptest`**: Mandatory where mathematical properties are clear and a model can be defined (DSP invariants, protocol encode/decode roundtrip, filter frequency responses). Not required for UI or config tests where properties aren't well-defined.
- **No test interdependence** — each test sets up its own state

### Test Naming Convention

```rust
#[test]
fn fir_attenuates_high_frequency() { ... }     // behavior-driven
#[test]
fn agc_normalizes_constant_amplitude() { ... } // describes what's verified
```

## Linting & Formatting

### Clippy

Run `cargo clippy -- -D warnings` before every commit. Recommended lint configuration for `Cargo.toml` or `clippy.toml`:

```toml
# Cargo.toml [lints.clippy] section
[lints.clippy]
pedantic = { level = "warn", priority = -1 }
# No allows — fix every warning. The compiler is our strictest reviewer.
```

**Cast lints (`cast_possible_truncation`, `cast_sign_loss`, `cast_precision_loss`) stay enabled.** Fix them with explicit conversions:
- Use `f64::from(x)` instead of `x as f64` when lossless
- Use `x.try_into().expect("invariant: ...")` or `i32::try_from(x)?` for fallible casts
- Use `#[allow(clippy::cast_precision_loss)]` on individual lines only when the loss is analyzed and acceptable (e.g., `usize` → `f64` for FFT bin indexing where the value range is bounded)

The goal: make the compiler protest as much as possible so we fix the code, not silence the warnings.

### Rustfmt

Use default `rustfmt` settings. If we add a `rustfmt.toml`, keep it minimal:

```toml
edition = "2021"
max_width = 100
```

### Compiler Warnings

```toml
# Cargo.toml
[lints.rust]
unused = "warn"
```

Fix all warnings. Prefix intentionally unused variables with `_`.

## Git Workflow

- **Commit messages**: imperative mood, concise (`Add FM demod bandpass filter`, not `Added stuff`)
- **One logical change per commit** — don't mix refactoring with feature work
- **Run `cargo test` and `cargo clippy` before committing**
- **Don't commit broken tests** — if a test is WIP, mark it `#[ignore]`
- **Feature branches**: `feature/websocket-auth`, `fix/fft-window-symmetry`

## Architecture Patterns

### Thread Model

TermSDR uses OS threads with `crossbeam-channel` for communication:
- **Capture thread**: reads IQ samples from source → sends to DSP
- **DSP thread**: FFT processing → sends results to UI/server
- **Audio thread**: demodulation pipeline → audio sink
- **UI/Server thread**: rendering (TUI) or WebSocket handling (headless)

### Adding a New Signal Source

1. Implement the `Source` trait in `src/capture/`
2. Add variant to `Source` dispatch in `capture/mod.rs`
3. Write tests using known signal properties
4. Add CLI flag in `config.rs`

### Adding a New Demodulation Mode

1. Add mode variant to `DemodMode` enum in `audio/demod.rs`
2. Implement the demod function
3. Write tests with synthetic modulated signals
4. Wire into the audio pipeline
5. Add UI keybinding in `ui/mod.rs`

### WebSocket Protocol

The binary/JSON hybrid protocol is documented in `PROTOCOL.md` (source of truth). Do not duplicate protocol details here.

## Boundaries

### ✅ Always
- Write tests for new functionality before implementing
- Run `cargo test` and `cargo clippy` before pushing
- Use `?` for error propagation
- Document public APIs
- Use synthetic signals for testable DSP code

### ⚠️ Ask First
- **Adding new crate dependencies** — evaluate: is it maintained? how many transitive deps? could we implement it in <50 lines ourselves? No crates that duplicate existing functionality. Updating versions or enabling features on existing crates is fine without asking.
- Changing the thread model or channel topology
- Modifying the WebSocket protocol (breaking change for clients)
- Changing public API signatures

### 🚫 Never
- `.unwrap()` in non-test code
- Hardcode frequencies, sample rates, or buffer sizes as magic numbers — extract to named `const` values in the relevant module, or make them fields of a config struct when they should be tunable (e.g., AGC attack/decay rates)
- Commit code with compiler warnings
- Skip tests for "trivial" changes
- Use `unsafe` — zero tolerance, no exceptions. The project has no need for it (FFI is handled by crate wrappers, perf-critical FFT is inside `rustfft`). If a future need arises, discuss first and create an ADR
