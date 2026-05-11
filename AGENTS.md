# AGENTS.md — TermSDR Development Guide

## Philosophy: Test-Driven Development (TDD)

**Tests first. Always.**

1. **Write a failing test** that describes what you want
2. **Write the minimum code** to make it pass
3. **Refactor** with confidence — the tests catch regressions

### Why TDD for TermSDR?

We have a superpower: **synthetic test signals**. The `TestSource` generates IQ data with known properties (noise floor, signal peaks at specific offsets). This means we can test the entire DSP pipeline — from signal generation through FFT to dB output — without any hardware.

Use this. Every new feature should have a test that exercises it with synthetic data before touching real SDR hardware.

## Project Structure

```
src/
├── main.rs              # CLI args, startup, thread orchestration
├── types.rs             # Shared types: IqSample, FftResult, AppConfig
├── config.rs            # CLI parsing + JSON config loading
├── capture/
│   ├── mod.rs           # Source trait + dispatch
│   ├── rtlsdr.rs        # RTL-SDR hardware (stub until librtlsdr available)
│   ├── file.rs          # IQ file replay
│   └── test_signal.rs   # Synthetic signal generator (the TDD workhorse)
├── dsp/
│   ├── mod.rs           # DSP pipeline orchestration
│   └── fft.rs           # FFT, windowing, magnitude → dB
└── ui/
    ├── mod.rs           # App state + event loop
    ├── spectrum.rs      # Spectrum display widget
    ├── waterfall.rs     # Waterfall display widget
    └── status_bar.rs    # Status bar widget
```

## Conventions

- **Idiomatic Rust**: Use `Result<T>` with `anyhow`, derive traits where sensible, prefer iterators over manual loops.
- **No unwrap in production code**: Use `?` or proper error handling. `unwrap()` is fine in tests.
- **Stubs use TODO comments**: Unimplemented modules should compile and have `// TODO:` markers.
- **Tests live next to code**: Use `#[cfg(test)] mod tests { ... }` in each module.
- **Integration tests**: `tests/` directory for cross-module tests (e.g., TestSource → FFT pipeline).

## Testing Approach

| Layer | Test Strategy |
|-------|--------------|
| Types | Construction, conversion, default values |
| Capture (TestSource) | Known signal properties, correct sample count, trait compliance |
| DSP/FFT | Pure tone → correct bin, flat noise floor, dB accuracy, window shape |
| Integration | TestSource → FFT → verify peak at expected frequency |
| UI | Manual testing (TUI is hard to unit test) |

## Running

```bash
cargo test          # Run all tests
cargo test -- --nocapture  # See println! output in tests
cargo build         # Build debug
cargo run           # Run with defaults
cargo run -- -f 118M -s 2.048M  # Custom frequency
```
