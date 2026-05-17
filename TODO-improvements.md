# TODO — TermSDR Code Improvements

Generated 2026-05-17 from AGENTS.md audit. **473 clippy pedantic errors** total.

---

## Dependencies to Add

- [ ] Add `[lints.clippy]` section to `Cargo.toml`: `pedantic = { level = "warn", priority = -1 }`
- [ ] Add `[lints.rust]` section to `Cargo.toml`: `unused = "warn"`
- [ ] Add `thiserror` to `[dependencies]` — needed for typed server errors (`server/error.rs`)
- [ ] Add `tracing` to `[dependencies]` — replace all `println!`/`eprintln!` in prod code
- [ ] Add `tracing-subscriber` to `[dependencies]` — with file appender (TUI owns stdout)
- [ ] Add `proptest` to `[dev-dependencies]` — property-based tests for DSP functions
- [ ] Add `tokio-tungstenite` to `[dev-dependencies]` — WS integration tests

---

## Clippy Pedantic Fixes (473 errors)

### `#[must_use]` Missing (68 method + 22 function + 20 return-self = 110 total)

Every `pub fn` that returns a value without side effects needs `#[must_use]`. Key files:

- `src/audio/demod.rs`: `default_channel_bw` (L25), `label` (L34), `next` (L43), `low_pass` (L107), `num_taps` (L176), `decimate` (L188), `am_demod` (L202), `fm_demod` (L215), `new` on `DcRemover` (L247), `alpha` (L256), `new` on `DcBlocker` (L291), `new` on `Agc` (L344), `enabled` (L417), `current_gain_db` (L422), `new` on `AudioPipeline` (L460)
- `src/ui/mod.rs`: `display_source` (L896), `cursor_pos` (L905), `cursor_frequency` (L911), `cursor_db` (L916)
- `src/ui/spectrum.rs`, `src/ui/waterfall.rs`, `src/ui/status_bar.rs`, `src/ui/command_palette.rs`, `src/ui/settings_modal.rs`, `src/ui/colors.rs`: all `new`/accessor methods
- `src/server/protocol.rs`: `parse_demod_mode` (L265), `color_to_string` (L275), `encode_spectrum_frame` (L15), `decode_spectrum_frame` (L38)
- `src/server/commands.rs`: `dispatch` (L111)
- `src/dsp/fft.rs`: `hanning_window` (L113), `power_to_db` (L123)
- `src/capture/test_signal.rs`, `src/capture/file.rs`: constructors
- `src/types.rs`, `src/config.rs`, `src/bookmarks.rs`: various accessors

### Cast Warnings (~130 total)

**`cast_precision_loss`** (28 `usize→f64` + 12 `usize→f32`):
- `src/audio/demod.rs`: L110, L116
- `src/dsp/fft.rs`: multiple FFT bin indexing lines
- `src/ui/mod.rs`: L922, L1747
- `src/ui/spectrum.rs`: multiple bin resampling lines
- `src/ui/waterfall.rs`: rendering calculations
- `src/ui/status_bar.rs`: frequency formatting
- `src/ui/settings_modal.rs`: slider calculations

**`cast_possible_truncation`** (28 `f64→f32` + 10 `f64→usize` + 25 `usize→u16` + 3 `f64→u32` + 3 `f32→u8` + 1 `f64→i64`):
- `src/audio/demod.rs`: L72, L73, L129, L248, L308, L378, L468
- `src/ui/mod.rs`: L663, L667, L807, L907, L922, L1204, L1282, L1337, L1362, L1666, L1747, L1822, L1852
- `src/ui/spectrum.rs`: multiple rendering calculations
- `src/ui/waterfall.rs`: row/column calculations
- `src/ui/colors.rs`: color index
- `src/ui/settings_modal.rs`: L118 (`250000.0` → `250_000.0`)
- `src/server/commands.rs`: various
- `src/dsp/fft.rs`: FFT size casts

**`cast_sign_loss`** (10 `f64→usize` + 9 `f32→u16` + 3 `f32→u8` + 3 `f64→u32`):
- `src/ui/mod.rs`: L663, L667, L807, L922, L1596, L1747
- `src/ui/spectrum.rs`, `src/ui/waterfall.rs`, `src/ui/colors.rs`
- `src/audio/demod.rs`: L468

**`cast_lossless`** (12 `f32→f64` + 10 `u16→f32` + 3 `u8→f32`):
- `src/audio/demod.rs`: L304, L369
- `src/ui/spectrum.rs`, `src/ui/waterfall.rs`, `src/ui/settings_modal.rs`

**`cast_possible_wrap`** (1):
- `src/ui/mod.rs`: L1594 (`u32→i32`)

### Format String Inlining (49 errors — `uninlined_format_args`)

All files using `format!("{}", var)` instead of `format!("{var}")`:
- `src/ui/mod.rs`: L1257, L1483, L1702, L1704, L1759, L1854
- `src/ui/spectrum.rs`: multiple label formatting lines
- `src/ui/status_bar.rs`: frequency/label formatting
- `src/ui/settings_modal.rs`: value display formatting
- `src/ui/command_palette.rs`: command display
- `src/server/protocol.rs`: error messages
- `src/server/ws.rs`: logging
- `src/server/commands.rs`: response formatting
- `src/bookmarks.rs`: import formatting
- `src/main.rs`: startup messages

### Documentation Missing (26 `item_in_docs_missing_backticks` + 16 `missing_errors_doc` + 8 `missing_panics_doc`)

**`missing_errors_doc`** — functions returning `Result` without `# Errors`:
- `src/ui/mod.rs`: `init_terminal` (L929), `restore_terminal` (L939), `show_source_selection` (L1511)
- `src/config.rs`: config loading functions
- `src/capture/file.rs`: file source constructor
- `src/server/mod.rs`: `run_server` (L28)
- `src/bookmarks.rs`: `import_cubicsdr` (L105)

**`missing_panics_doc`** — functions with `.unwrap()` missing `# Panics`:
- `src/ui/mod.rs`: `show_source_selection` (L1511)
- `src/ui/command_palette.rs`: `execute` method
- `src/types.rs`: `peak_frequency` method

**`item_in_docs_missing_backticks`** — 26 instances across all documented items (types, functions, parameters mentioned without backticks)

### Pattern & Style Fixes

**`unnested_or_patterns`** (8):
- `src/bookmarks.rs`: L127
- `src/ui/mod.rs`: L358, L363, L428, L621, L1584, L1587, L1599

**`collapsible_match`** (5):
- `src/ui/mod.rs`: L682, L704, L1645

**`redundant_field_names`** (1):
- `src/ui/mod.rs`: L1832

**`bool_to_int_with_if`** (1):
- `src/ui/mod.rs`: L1203

**`explicit_iter_loop`** (1):
- `src/ui/mod.rs`: L1543

**`manual_is_multiple_of`** (1):
- `src/audio/demod.rs`: L109

**`used_cloned_where_copied`** (3):
- Various files using `.cloned()` on `Copy` types

**`these_match_arms_have_identical_bodies`** (3):
- Duplicate match arm bodies that should be combined

**`manual_range_contains`** (8):
- Manual `x >= lo && x <= hi` instead of `(lo..=hi).contains(&x)`

**`used_underscore_prefixed_binding`** (2):
- Variables prefixed with `_` but actually used

**`pass_by_value`** (4):
- Small `Copy` types passed by reference instead of value

**`empty_line_after_doc_comments`** (1):
- `src/ui/colors.rs`: L3

**`unreadable_literal`** (1):
- `src/ui/settings_modal.rs`: L118 (`250000.0` → `250_000.0`)

### Functions Too Long (5 — `too_many_lines` >100)

- `src/ui/mod.rs`: `apply_command_action` (L745, 146 lines)
- `src/ui/mod.rs`: `draw_ui` (L947, 115 lines)
- `src/ui/mod.rs`: `draw_help_modal` (L1088, 102 lines)
- `src/ui/mod.rs`: `draw_command_palette` (L1196, 114 lines)
- `src/ui/mod.rs`: `draw_settings_modal` (L1660, 164 lines)

---

## Visibility Fixes (`pub` → `pub(crate)`)

Most items are `pub` but only used within the crate. Convert to `pub(crate)`:

### `src/lib.rs` — Module declarations
- [ ] L1-8: All `pub mod` → `pub(crate) mod` (unless the crate is meant to be a library — likely not)

### `src/types.rs`
- [ ] L5: `pub type IqSample` — keep `pub` (used everywhere)
- [ ] L9: `pub struct FftResult` — likely `pub(crate)`
- [ ] L48: `pub struct AppConfig` — likely `pub(crate)`
- [ ] L66: `pub enum SourceType` — likely `pub(crate)`

### `src/server/protocol.rs`
- [ ] L7: `pub const TAG_SPECTRUM` → `pub(crate)`
- [ ] L10: `pub const TAG_AUDIO` → `pub(crate)`
- [ ] L15: `pub fn encode_spectrum_frame` → `pub(crate)`
- [ ] L38: `pub fn decode_spectrum_frame` → `pub(crate)`
- [ ] L69: `pub struct AgcState` → `pub(crate)`
- [ ] L94: `pub struct BandpassState` → `pub(crate)`
- [ ] L115: `pub struct VfoState` → `pub(crate)`
- [ ] L129: `pub struct StateMessage` → `pub(crate)`
- [ ] L184: `pub enum Command` → `pub(crate)`
- [ ] L244: `pub struct ErrorMessage` → `pub(crate)`
- [ ] L265: `pub fn parse_demod_mode` → `pub(crate)`
- [ ] L275: `pub fn color_to_string` → `pub(crate)`

### `src/server/ws.rs`
- [ ] L16-26: All type aliases and structs → `pub(crate)`
- [ ] L72, L81, L90: broadcast/handler functions → `pub(crate)`

### `src/server/commands.rs`
- [ ] L18: `pub struct SharedState` → `pub(crate)`
- [ ] L38: `pub struct VfoEntry` → `pub(crate)`
- [ ] L101: `pub enum DispatchResult` → `pub(crate)`
- [ ] L111: `pub fn dispatch` → `pub(crate)`

### `src/server/mod.rs`
- [ ] L1-3: `pub mod` → `pub(crate) mod`
- [ ] L18: `pub struct ServerConfig` → `pub(crate)`
- [ ] L28: `pub fn run_server` → `pub(crate)`

### `src/capture/` — all submodule declarations and types
- [ ] `mod.rs` L1-3: `pub mod` → `pub(crate) mod`
- [ ] `mod.rs` L10: `pub trait Source` → `pub(crate) trait Source`
- [ ] `file.rs` L10: `pub struct FileSource` → `pub(crate)`
- [ ] `test_signal.rs` L15: `pub struct TestSource` → `pub(crate)`
- [ ] `rtlsdr.rs`: L16, L30, L36, L42, L48, L153, L209 — all `pub` → `pub(crate)`

### `src/dsp/`
- [ ] `mod.rs` L1: `pub mod fft` → `pub(crate) mod fft`
- [ ] `fft.rs` L8: `pub struct FftProcessor` → `pub(crate)`
- [ ] `fft.rs` L113: `pub fn hanning_window` → `pub(crate)`
- [ ] `fft.rs` L123: `pub fn power_to_db` → `pub(crate)`

### `src/ui/` — all widgets, functions, types
- [ ] All `pub struct` widget types → `pub(crate)`
- [ ] `mod.rs`: `pub fn init_terminal`, `pub fn restore_terminal`, `pub fn draw_ui`, `pub fn show_source_selection` → `pub(crate)`
- [ ] `colors.rs`: L81, L94, L101 → `pub(crate)`
- [ ] `spectrum.rs`: L360 `resample_bins`, L387 `generate_freq_labels` → `pub(crate)`
- [ ] `command_palette.rs`: all `pub` types and structs → `pub(crate)`
- [ ] `settings_modal.rs`: all `pub` enums, structs, functions → `pub(crate)`
- [ ] `status_bar.rs`: L12 `DisplaySource`, L29 `StatusBarWidget` → `pub(crate)`

### `src/bookmarks.rs`
- [ ] L11: `pub struct Bookmark` → `pub(crate)`
- [ ] L21: `pub struct BookmarkStore` → `pub(crate)`
- [ ] L26: `pub fn default_catalog` → `pub(crate)`
- [ ] L105: `pub fn import_cubicsdr` → `pub(crate)`

### `src/audio/`
- [ ] `mod.rs`: submodule declarations → `pub(crate) mod`
- [ ] `demod.rs`: all `pub fn` and `pub struct` → `pub(crate)` (demod functions, FIR, AGC, etc.)

---

## Documentation

### Missing Module-Level Docs (`//!`)

All of these files need `//!` module documentation at the top:

- [ ] `src/main.rs`
- [ ] `src/lib.rs`
- [ ] `src/types.rs`
- [ ] `src/config.rs`
- [ ] `src/audio/mod.rs`
- [ ] `src/capture/mod.rs`
- [ ] `src/capture/file.rs`
- [ ] `src/capture/test_signal.rs`
- [ ] `src/dsp/mod.rs`
- [ ] `src/dsp/fft.rs`
- [ ] `src/server/mod.rs`
- [ ] `src/server/protocol.rs`
- [ ] `src/server/ws.rs`
- [ ] `src/ui/mod.rs`
- [ ] `src/ui/spectrum.rs`
- [ ] `src/ui/waterfall.rs`
- [ ] `src/ui/colors.rs`
- [ ] `src/ui/status_bar.rs`
- [ ] `src/ui/settings_modal.rs`

Only files with existing `//!` docs:
- `src/bookmarks.rs`
- `src/audio/demod.rs`
- `src/audio/test_signal.rs`
- `src/audio/cpal_sink.rs`
- `src/audio/ws_sink.rs`
- `src/capture/rtlsdr.rs`
- `src/server/commands.rs`
- `src/ui/command_palette.rs`

### Missing Doc Comments (`///`) on Public Items

Every `pub` (or `pub(crate)`) function, struct, enum, trait, and method needs `///` doc comments. This affects virtually every file. Key gaps:
- All items in `src/types.rs`
- All items in `src/server/protocol.rs`
- All items in `src/server/ws.rs`
- All items in `src/ui/` widgets
- DSP functions need unit documentation (Hz, dB, samples)

### Missing `# Errors` Sections

16 functions returning `Result` need `# Errors` doc sections (see clippy list above).

### Missing `# Panics` Sections

8 functions with potential panics need `# Panics` doc sections.

---

## Error Handling

### `.unwrap()` in Production Code

- [ ] `src/types.rs:40` — `.partial_cmp(b).unwrap()` in `peak_frequency` → use `.unwrap_or(Ordering::Equal)` or handle `NaN`
- [ ] `src/server/mod.rs:112` — `TcpListener::bind(addr).await.unwrap()` → propagate with `?`
- [ ] `src/server/mod.rs:120` — `.unwrap()` on server serve → propagate with `?`
- [ ] `src/ui/command_palette.rs:206,215,221,225,255,267,290,299,308,314` — 10× `param.unwrap()` → use `?` or `if let`
- [ ] `src/ui/mod.rs:394` — `c.to_digit(10).unwrap()` → use `if let` or `?`
- [ ] `src/ui/mod.rs:1594` — `c.to_digit(10).unwrap()` → use `if let` or `?`
- [ ] `src/audio/cpal_sink.rs:57,84` — `Mutex::lock().unwrap()` → use `.expect("invariant: mutex not poisoned")`
- [ ] `src/audio/mod.rs:269` — `.next().unwrap()` → use `.expect("invariant: ...")`

### `thiserror` Migration for Server Module

- [ ] Create `src/server/error.rs` with typed `ServerError` enum (per AGENTS.md pattern)
- [ ] Variants: `InvalidCommand`, `Connection`, `Encoding`
- [ ] Update `server/commands.rs` and `server/ws.rs` to use `ServerError` instead of `anyhow`

---

## Logging Migration (`println!`/`eprintln!` → `tracing`)

All 17 occurrences in production code:

- [ ] `src/main.rs:38` — `println!("Imported {} bookmarks...")` → `tracing::info!`
- [ ] `src/main.rs:40` — `eprintln!("Import failed: {}")` → `tracing::error!`
- [ ] `src/main.rs:167` — `eprintln!("Audio: Opus not compiled in...")` → `tracing::warn!`
- [ ] `src/main.rs:189` — `eprintln!("Audio: no output device...")` → `tracing::warn!`
- [ ] `src/main.rs:253` — `eprintln!("File replay not yet implemented")` → `tracing::warn!`
- [ ] `src/main.rs:260` — `eprintln!("RTL-SDR error: {}")` → `tracing::error!`
- [ ] `src/main.rs:281` — `eprintln!("Retune error: {}")` → `tracing::error!`
- [ ] `src/main.rs:295` — `eprintln!("Capture error: {}")` → `tracing::error!`
- [ ] `src/server/ws.rs:112` — `eprintln!("WS: client connected...")` → `tracing::info!`
- [ ] `src/server/ws.rs:159` — `eprintln!("WS: client sent command...")` → `tracing::debug!`
- [ ] `src/server/ws.rs:238` — `eprintln!("WS: invalid command...")` → `tracing::warn!`
- [ ] `src/server/ws.rs:263` — `eprintln!("WS: client disconnected...")` → `tracing::info!`
- [ ] `src/server/mod.rs:72` — `eprintln!("TermSDR WS server listening...")` → `tracing::info!`
- [ ] `src/audio/ws_sink.rs:74` — `eprintln!("WS Audio: Failed to create Opus...")` → `tracing::warn!`
- [ ] `src/audio/ws_sink.rs:127` — `eprintln!("WS Audio: Opus encode error...")` → `tracing::error!`
- [ ] `src/audio/cpal_sink.rs:67` — `eprintln!("cpal stream error...")` → `tracing::error!`
- [ ] Set up `tracing-subscriber` with file appender in `main.rs` (TUI owns the terminal)

---

## Magic Numbers

Hardcoded values that should be named constants or config fields:

- [ ] `src/ui/settings_modal.rs:118` — `8000.0`, `1000.0`, `250000.0` (channel BW defaults)
- [ ] `src/ui/mod.rs` — `30.0` waterfall refresh rate (L663, L667, L807), `8` max VFOs (L682)
- [ ] `src/audio/demod.rs` — AGC attack/decay defaults, FIR tap counts, DC removal time constants
- [ ] `src/dsp/fft.rs` — FFT default sizes, window parameters
- [ ] `src/server/ws.rs` — any buffer sizes, timeouts
- [ ] `src/capture/test_signal.rs` — signal generation parameters
- [ ] All frequency values (118 MHz, etc.) used as defaults → named constants

---

## Missing Tests

### Modules Without `#[cfg(test)] mod tests`

- [ ] `src/capture/file.rs` — needs tests for IQ file reading, error handling
- [ ] `src/capture/mod.rs` — needs tests for source dispatch logic
- [ ] `src/dsp/mod.rs` — needs tests for DSP pipeline orchestration
- [ ] `src/lib.rs` — no tests needed (just module declarations)
- [ ] `src/main.rs` — no unit tests needed (integration tests cover this)

### Server Module — Expand Test Coverage

- [ ] `src/server/commands.rs` — has tests, but verify coverage of all command variants
- [ ] `src/server/protocol.rs` — has tests, verify encode/decode roundtrip for all message types
- [ ] `tests/integration.rs` — add real WS client tests with `tokio-tungstenite` (connect → send command → verify response)

### UI Module — Add Smoke Tests

- [ ] `src/ui/mod.rs` — add `TestBackend` smoke tests: `draw_ui` renders without panic
- [ ] `src/ui/spectrum.rs` — has tests, verify widget rendering with `TestBackend`
- [ ] `src/ui/waterfall.rs` — has tests, verify widget rendering with `TestBackend`
- [ ] `src/ui/settings_modal.rs` — has tests, add rendering smoke test
- [ ] `src/ui/command_palette.rs` — has tests, add rendering smoke test

---

## Property-Based Tests (`proptest`)

DSP functions with clear mathematical properties:

- [ ] `src/dsp/fft.rs` — FFT of pure tone always peaks at correct bin (∀ frequency, sample rate)
- [ ] `src/dsp/fft.rs` — `power_to_db` is monotonically increasing
- [ ] `src/dsp/fft.rs` — `hanning_window` is symmetric and values ∈ [0, 1]
- [ ] `src/audio/demod.rs` — AM demod roundtrip preserves envelope shape
- [ ] `src/audio/demod.rs` — FM demod of constant frequency → constant output
- [ ] `src/audio/demod.rs` — FIR filter: output length = input length (for `apply`)
- [ ] `src/audio/demod.rs` — `decimate` output length = `ceil(input.len() / factor)`
- [ ] `src/audio/demod.rs` — AGC output amplitude converges to target level
- [ ] `src/audio/demod.rs` — DC removal: output mean ≈ 0 for any input
- [ ] `src/server/protocol.rs` — encode/decode roundtrip: `decode(encode(x)) == x` ∀ valid `FftResult`

---

## Summary

| Category | Count |
|---|---|
| Dependencies to add | 7 |
| Clippy pedantic errors | 473 |
| `pub` → `pub(crate)` conversions | ~80+ items |
| Missing module docs (`//!`) | 19 files |
| Missing doc comments (`///`) | ~80+ public items |
| `.unwrap()` in prod code | 18 call sites |
| `println!`/`eprintln!` → `tracing` | 17 call sites |
| Magic numbers to extract | ~15+ values |
| Modules without tests | 3 files |
| Property-based tests to add | 10 test cases |
| Functions too long (>100 lines) | 5 functions |
