# TermSDR — Initial Plan

> CubicSDR-like experience, but in the terminal.

## Project Overview

A terminal-based SDR (Software Defined Radio) application that renders real-time spectrum and waterfall displays using a TUI interface. Think CubicSDR but for the command line.

## Decisions Log

### Hardware Target
- **Decision:** RTL-SDR (USB dongle)
- **Rationale:** Most common and affordable SDR hardware. Will abstract the hardware layer for future device support.

### Language & Framework
- **Decision:** Rust + `ratatui` (TUI framework)
- **Rationale:**
  - Native performance critical for real-time FFT processing and rendering
  - `ratatui` is the leading Rust TUI framework (successor to `tui-rs`)
  - Strong ecosystem: `rustfft` for FFT, `rtlsdr` crate for hardware access
  - Memory safety without GC pauses

### v0.1 Scope
- **Decision:** Spectrum display + Waterfall + Frequency selection + Bandwidth configuration
- **Features:**
  - [x] FFT spectrum graph (power vs frequency)
  - [x] Waterfall display (time-scrolling color heatmap)
  - [x] Frequency tuning (select center frequency)
  - [x] Bandwidth configuration (sample rate / visible bandwidth)
- **Not in v0.1:**
  - Audio demodulation (FM/AM) → v0.2
  - Recording → future
  - Multiple VFOs → future
  - WebSocket server mode (headless backend + remote web frontend) → future

## Architecture (Proposed)

```
┌─────────────────────────────────────┐
│           TUI (ratatui)             │
│  ┌──────────┐  ┌──────────────────┐ │
│  │ Spectrum  │  │    Waterfall     │ │
│  │  (FFT)   │  │  (color heatmap) │ │
│  └──────────┘  └──────────────────┘ │
│  ┌──────────────────────────────────┤
│  │ Status bar: freq, BW, gain, etc │
│  └──────────────────────────────────┤
└──────────┬──────────────────────────┘
           │
┌──────────▼──────────────┐
│    DSP Pipeline          │
│  IQ samples → FFT →     │
│  magnitude → dB scale    │
└──────────┬──────────────┘
           │
┌──────────▼──────────────┐
│   RTL-SDR Driver         │
│  (librtlsdr / rtlsdr-rs) │
└─────────────────────────┘
```

## Key Dependencies (Rust Crates)

| Crate | Purpose |
|-------|---------|
| `ratatui` | TUI framework |
| `crossterm` | Terminal backend |
| `rustfft` | FFT computation |
| `rtlsdr` | RTL-SDR hardware access |
| `num-complex` | Complex number types for IQ |
| `tokio` / `crossbeam` | Async / threading |

## Controls (Proposed)

| Key | Action |
|-----|--------|
| `←` / `→` | Tune frequency (fine step, e.g. 25 kHz) |
| `PgUp` / `PgDn` | Tune frequency (coarse step, e.g. 1 MHz) |
| `↑` / `↓` | Adjust gain |
| `+` / `-` | Zoom bandwidth |
| `f` | Jump to frequency (modal input) |
| `b` | Enter bandwidth directly |
| `Space` | Toggle cursor active/inactive |
| `m` | Place marker at cursor position (for future multi-listen) |
| `q` | Quit |

## Defaults & CLI

```bash
# Basic usage with defaults
termsdr

# Custom frequency and sample rate
termsdr -f 118M -s 2.048M

# Aviation band
termsdr -f 118.1M -s 2.048M --gain 40
```

### Default Values
| Parameter | Default | Notes |
|-----------|---------|-------|
| Center frequency | 100.0 MHz | FM broadcast band |
| Sample rate | 2.048 MS/s | Max stable for RTL-SDR |
| Gain | Auto (AGC) | Can override with `--gain` |
| FFT size | 2048 | Good resolution/speed balance |
| PPM correction | 0 | `--ppm` flag for crystal offset |

### Aviation Frequencies (Madrid/Barajas reference)
| Frequency | Service |
|-----------|---------|
| 118.15 MHz | Madrid Barajas Tower |
| 119.55 MHz | Madrid Approach |
| 121.50 MHz | Emergency/distress (international) |
| 123.95 MHz | Madrid Ground |
| 127.60 MHz | Madrid Approach (alt) |
| 134.55 MHz | Tower (common EU) |

Aviation uses **AM modulation** (not FM), so demod in v0.2 must support AM.

### CubicSDR Session Files (Future Import)

CubicSDR uses `.cubicsession` XML files with a custom `DataNode` tree:
- Device config: id, name, ppm, offset, sample_rate, agc_mode, gains
- Bookmarks: frequency, bandwidth, modulation type, label
- Session state: center freq, zoom, active demodulators

We'll parse these in a future version to import bookmarks and device settings.

## Open Questions

11. ~~Threading~~ → 3-thread pipeline: Capture | DSP/FFT | TUI. Designed for easy v0.2 extension (fan-out demod threads from capture channel) ✅
12. ~~Input handling~~ → Standard ratatui event loop with crossterm poll (~16ms timeout, ~60fps) ✅
13. ~~Project structure~~ → B: Modular single crate (capture/, dsp/, ui/, config, types) ✅

1. ~~Hardware~~ → RTL-SDR ✅
2. ~~Language~~ → Rust ✅
3. ~~v0.1 scope~~ → Spectrum + Waterfall + freq/BW controls ✅
4. ~~Layout~~ → Stacked vertical (spectrum top, waterfall bottom) ✅
5. ~~Color scheme~~ → Viridis (purple → blue → green → yellow). Configurable in future ✅
6. ~~Frequency step~~ → Multiple steps (arrows = fine, PgUp/PgDn = coarse) + direct frequency input (`f` key) ✅
7. ~~Cursor / markers~~ → Cursor system for marking frequencies. Space to toggle cursor active/inactive. Marked cursors persist (for future audio monitoring of multiple freqs). May defer marker persistence to v0.2 ✅
7. ~~Sample rates & config~~ → CLI args with sensible defaults. See defaults below ✅
8. ~~CubicSDR compat~~ → Future: import `.cubicsession` XML files (bookmarks, device config) ✅
9. ~~Test mode~~ → D: Synthetic test signal + IQ file playback for dev/offline use ✅
10. ~~Config format~~ → JSON for native config files. CubicSDR XML import as separate compat feature ✅
