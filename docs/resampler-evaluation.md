# Resampler dependency evaluation

## Baseline

Evaluation host and toolchain:

- CPU: AMD Ryzen 7 9700X 8-Core Processor (16 hardware threads)
- `rustc 1.99.1 (58cf8f9c3 2026-10-13)`
- `cargo 1.99.1 (5f94df478 2026-08-27)`
- Baseline `cargo test --no-default-features --lib`: 576 passed
- Baseline integration suite: 4 passed
- Baseline `cargo clippy --no-default-features --all-targets -- -D warnings`: failed with 206 pre-existing errors
- Baseline `cargo fmt -- --check`: failed on pre-existing formatting differences
- Web checks could not run because `npm` was absent from the host

The exact initial `git status --short` lines were:

```text
?? .hermes/
?? atlas-termsdr.html
?? atlas-termsdr.html.report.json
?? atlas-termsdr.png
?? atlas-termsdr.png.report.json
```

Those pre-existing planning and atlas files were preserved. The evaluation test was then created by the Test Writer and frozen before each production implementation pass; implementation agents did not modify its assertions.

## Candidates

### rubato 5.1.0 — selected

- Crate: <https://crates.io/crates/rubato/5.1.0>
- Documentation: <https://docs.rs/rubato/5.1.0>
- Repository: <https://github.com/HEnquist/rubato>
- Release: 5.1.0, 2026-11-12
- License: MIT OR Apache-2.0
- MSRV: Rust 1.87, as declared by the crate metadata
- Implementation: pure Rust; no FFI or system package
- API fit: stateful chunk processing, preallocated input/output adapters, two synchronized channels, an explicit output-delay query, and final partial-block processing
- Direct feature selection: default FFT resampler (`fft_resampler`)
- Relevant resolved transitive crates: `audioadapter 5.0.0`, `audioadapter-buffers 5.2.0`, `audioadapter-sample 5.2.0`, `audio-codec-algorithms 0.8.1`, `realfft 3.5.1`, `windowfunctions 0.1.1`, and `visibility 0.1.1`; existing numeric dependencies are shared with the project where possible

The adapter uses `rubato::Fft<f32>` at fixed input and output rates. Arbitrary caller partitions are staged until the backend's fixed input block is available. The requested staging target is 4,096 input frames, but Rubato's fixed-both mode rounds that target up to a multiple of the reduced input-rate component; it is not unconditionally 4,096. TermSDR now computes the reduced ratio and resulting input/output blocks before constructing Rubato. Either block is limited to 65,536 frames (16 times the 4,096-frame staging target), preventing pathological allocation while preserving the required receiver rates and conventional conversions such as 44.1 kHz to 48 kHz. The required 2.048 MHz to 48 kHz ratio reduces to 128:3 and produces 4,096:96-frame blocks; 1.024 MHz to 48 kHz reduces to 64:3 and produces 4,096:192-frame blocks. The nearly-coprime 65,537 Hz to 48 kHz pair would require a 65,537-frame input block and is rejected with a structured error before backend construction.

I and Q are supplied as two channels to one backend instance. Rubato's private backend delay is 48 output samples for the evaluated 2.048 MHz to 48 kHz configuration. The adapter trims that startup padding, emits an exact-duration compensated stream, and therefore reports **zero residual output latency**. Zero-padding is used only to drain the final partial block and filter tail at EOF; output is capped to the exact rational duration.

TermSDR now declares `rust-version = "1.87"`, matching Rubato 5.1.0's crate metadata, and the README states the same requirement. This host has Rust 1.99.1 but no `rustup`, so an actual Rust 1.87 build could not be run here; the MSRV gate is based on the dependency metadata plus the successful current-host build, not a claimed 1.87 execution.

### resampler 0.5.2 — not selected

- Crate/release metadata: <https://crates.io/crates/resampler/0.5.2> — released 2026-10-10
- Documentation: <https://docs.rs/resampler/0.5.2>
- Repository: <https://github.com/hasenbanck/resampler/tree/v0.5.2>
- Changelog: <https://github.com/hasenbanck/resampler/blob/v0.5.2/CHANGELOG.md> — maintained through the 0.5.2 release and records its FIR correctness fixes
- Manifest: <https://github.com/hasenbanck/resampler/blob/v0.5.2/Cargo.toml> — Rust 1.89 MSRV and Apache-2.0 OR MIT license
- Dependencies/features: the default feature set has no normal dependency; the sole normal dependency is optional `libm`, enabled by the `no_std` feature
- Implementation: pure Rust; no FFI or required system package

This candidate was rejected before integration benchmarking. Inspection of the exported 0.5.2 API and implementations found processing and reset methods but no explicit EOF flush operation ([crate exports](https://github.com/hasenbanck/resampler/blob/v0.5.2/src/lib.rs), [FIR implementation](https://github.com/hasenbanck/resampler/blob/v0.5.2/src/resampler_fir.rs), [FFT implementation](https://github.com/hasenbanck/resampler/blob/v0.5.2/src/resampler_fft.rs)). That makes delayed-tail and exact-duration EOF handling less direct for the required project-owned streaming contract. Its downsampling FIR also scales taps by the rate ratio and then caps them at `MAX_TAPS = INPUT_CAPACITY / 2`, i.e. 2,048 taps ([source](https://github.com/hasenbanck/resampler/blob/v0.5.2/src/resampler_fir.rs#L17-L24), [cap application](https://github.com/hasenbanck/resampler/blob/v0.5.2/src/resampler_fir.rs#L325-L340)). For the large 2.048 MHz to 48 kHz downsampling ratio, that cap warrants a dedicated quality evaluation; Rubato already passed the frozen alias-rejection gate, so a second production dependency and additional integration were not justified. No claim is made that the cap necessarily fails the gate.

## Measured gate results

All signal measurements used the frozen `tests/resampler_evaluation.rs` contract and release performance used its ignored release-only test.

| Gate | Requirement | rubato 5.1.0 result |
|---|---:|---:|
| Ten-second count, 2.048 MHz to 48 kHz | 480,000 ±1 | 480,000 (error 0) |
| Ten-second count, 1.024 MHz to 48 kHz | 480,000 ±1 | 480,000 (error 0) |
| Residual latency after startup compensation | 0 output samples | 0 |
| Nearly-coprime 65,537 Hz to 48 kHz | reject before backend allocation | rejected |
| Partition RMS, blocks 257/1024/2048/4096 | ≤1e-5 | I = 0.000000000e0, Q = 0.000000000e0 for every fixed block |
| Random partition invariance | ≤1e-5 | passed frozen test |
| I/Q gain mismatch | ≤1e-5 | passed frozen test |
| I/Q phase error | ≤1e-5 rad | passed frozen test |
| 483 kHz alias at 3 kHz | ≤-60 dB | -147.838 dB |
| One second of 2.048 Msps complex input | <0.5 CPU-core seconds | 7.799 ms, 0.007799 core fraction (0.780%) |
| One-second output count | 48,000 | 48,000 |

The focused debug evaluation completed with 12 passed and 1 ignored in 6.54 s. The ignored release performance gate completed with 1 passed (12 filtered out) and printed the timing above. The final full `cargo test --no-default-features --tests` run was green: 576 library tests passed (2 ignored), 4 integration tests passed, and 12 resampler evaluation tests passed (1 ignored), for **592 passed and 3 ignored overall**; the resampler portion completed in 6.54 s.

A scoped `cargo clippy --no-default-features --test resampler_evaluation -- -D warnings` attempt was blocked by 11 existing errors outside the new resampler file (including unused RTL-SDR stub items, `drain_collect`, missing error docs, long functions, and semicolon style). Clippy reported no error in `src/dsp/resampler.rs`; this does not erase the repository's pre-existing lint debt.

## Decision

rubato 5.1.0 passes the dependency gate and is pinned exactly in `Cargo.toml`. It is license-compatible, pure Rust, stateful, channel-synchronous, partition-invariant, duration-correct, comfortably above the alias-rejection threshold, and well below the CPU budget. Pathological reduced ratios are rejected before Rubato allocation, compensated output reports zero residual latency, and the project MSRV now matches the selected crate. No FFI or alternative resampler dependency was added.
