# TermSDR — Audio Quality Improvement Design Document

**Date:** 2026-05-12
**Status:** Approved (Grill Session)
**Goal:** Fix robotic-sounding AM aviation voice audio

---

## Problem Statement

When listening to aviation AM transmissions (118–137 MHz), the voice sounds robotic and hard to understand. After analyzing the current DSP pipeline and researching how professional SDR software (SDR++, GQRX, RTLSDR-Airband) handles AM audio, we identified four root causes.

### Root Causes

1. **No AGC (Automatic Gain Control)** — Aviation signals vary hugely in amplitude (near vs. far aircraft). Without AGC, weak signals are inaudible and strong ones can distort.

2. **No audio bandpass filter** — After AM demod, the audio contains DC to ~12.5 kHz. Aviation voice only occupies 300 Hz – 3.4 kHz. Everything else is noise that makes it sound metallic/robotic.

3. **Channel bandwidth too wide** — The default 25 kHz is the RF channel spacing, but the actual AM audio bandwidth for aviation is ~6–8 kHz. A wider filter lets in more noise.

4. **No audio normalization** — The raw magnitude output from `am_demod` isn't normalized, so levels vary wildly.

---

## Decision 1: AGC Placement

### Where in the pipeline should AGC go?

| Option | Description | Pros | Cons |
|--------|-------------|------|------|
| **A. AGC on RF (pre-demod)** | Normalize IQ amplitude before envelope detection | Better for SSB; what GQRX does | More complex; affects spectrum/waterfall display |
| **B. AGC on audio (post-demod)** ✅ | Normalize audio output after AM envelope detection | Simple; doesn't affect visualization; sufficient for AM aviation; what RTLSDR-Airband does | Less useful for future SSB mode |

### Decision: **Option B — AGC post-demod**

**Rationale:** Simpler implementation, doesn't interfere with spectrum/waterfall display, and perfectly adequate for AM aviation. If we add SSB later, we can revisit RF AGC.

---

## Decision 2: AGC Algorithm

### What type of AGC?

| Option | Description | Pros | Cons |
|--------|-------------|------|------|
| **A. Peak AGC** | Track signal peaks, normalize to target | Fast response | Amplifies noise between words/transmissions |
| **B. RMS AGC with dual time constants** ✅ | Track RMS power level with separate attack/decay | Smooth for voice; industry standard | Slightly more complex |

### Decision: **Option B — RMS AGC with dual time constants**

**Parameters:**

| Parameter | Default | Range | Description |
|-----------|---------|-------|-------------|
| Enabled | ON | ON/OFF | Toggle AGC |
| Attack | 10 ms | 1–100 ms | How fast gain drops when loud signal arrives (prevents clipping) |
| Decay | 300 ms | 50–2000 ms | How fast gain rises when signal gets weak (slow = avoids amplifying noise between words) |
| Target Level | 0.3 | 0.05–1.0 | Desired RMS output level |
| Max Gain | 50 dB | 1–100 dB | Maximum amplification (prevents infinite gain on silence/noise) |

**How it works:**

1. Compute RMS power over a short window of audio samples
2. Compare to target level
3. If current level > target → decrease gain with **attack** time constant (fast)
4. If current level < target → increase gain with **decay** time constant (slow)
5. Clamp gain to [0 dB, max_gain dB]
6. Multiply audio samples by computed gain

The slow decay is critical: if gain rises too quickly during pauses between words, it amplifies background noise and creates a "pumping" effect.

---

## Decision 3: Audio Bandpass Filter

### How to implement the voice bandpass filter?

| Option | Description | Pros | Cons |
|--------|-------------|------|------|
| **A. Two filters in series** | Existing LPF + new HPF | Reuses code | Two convolution passes; less efficient |
| **B. Single FIR bandpass** ✅ | Design one FIR bandpass kernel (highsinc − lowsinc) | Single convolution pass; 2x faster; cleaner | New function, but trivial |

### Decision: **Option B — Single FIR bandpass**

**Implementation:** `FirFilter::band_pass(low_hz, high_hz, sample_rate, num_taps)`

The bandpass kernel is computed as: `bandpass = lowpass(high_cutoff) − lowpass(low_cutoff)`, both windowed with Blackman.

**Parameters:**

| Parameter | Default | Range | Description |
|-----------|---------|-------|-------------|
| Enabled | ON | ON/OFF | Toggle bandpass filter |
| Low Cut | 300 Hz | 50–1000 Hz | High-pass cutoff (removes rumble, hum) |
| High Cut | 3400 Hz | 1000–8000 Hz | Low-pass cutoff (removes hiss, noise) |

**Defaults are ITU-T standard telephone bandwidth** (300–3400 Hz), which matches aviation AM voice channels exactly.

---

## Decision 4: Channel Bandwidth Default

The current default channel bandwidth for AM is 25 kHz (full RF channel spacing).

**New default for AM:** 8 kHz — this is what SDR++ recommends for AM aviation. It reduces noise without cutting voice content.

FM and NFM defaults remain unchanged (200 kHz and 25 kHz respectively).

---

## Updated DSP Pipeline

### Before (current)

```
Freq Shift → LPF → Decimate → AM Envelope → DC Removal → Volume → Output
```

### After (new)

```
Freq Shift → LPF → Decimate → AM Envelope → Audio Bandpass → AGC → DC Removal → Volume → Output
```

**New stages in bold context:**
1. **Audio Bandpass** (300–3400 Hz) — Removes out-of-band noise after demodulation
2. **AGC** (RMS, attack=10ms, decay=300ms) — Normalizes volume level

---

## Decision 5: Settings Modal UI

### Why a modal?

With volume, squelch, channel BW, demod mode, AGC (5 params), and bandpass (3 params), there are too many audio settings for individual hotkeys. We need a unified settings panel.

### Options considered

| Option | Description | Pros | Cons |
|--------|-------------|------|------|
| **A. Single "Audio Settings" modal** ✅ | All audio params in one scrollable modal | Everything in one place; one hotkey to remember | Longer list |
| **B. Multiple separate modals** | One modal per category (AGC, Filter, etc.) | Shorter modals | More hotkeys; harder to discover |

### Decision: **Option A — Single Audio Settings modal**

**Hotkey:** `s` (for "settings")

**Existing hotkeys** (`v`/`V` for volume, `w`/`W` for channel BW) continue working as quick shortcuts outside the modal.

### Modal Layout

```
┌─ Audio Settings ──────────────────────────┐
│                                           │
│  ► Volume:         ████████░░  80%        │
│    Squelch:        -60 dB  [ON]           │
│    Channel BW:     8.0 kHz                │
│    Demod Mode:     AM                     │
│                                           │
│  ── AGC ────────────────────────────────  │
│    AGC Enabled:    [ON]                   │
│    Attack:         10 ms                  │
│    Decay:          300 ms                 │
│    Target Level:   0.3                    │
│    Max Gain:       50 dB                  │
│                                           │
│  ── Audio Filter ───────────────────────  │
│    Bandpass:       [ON]                   │
│    Low Cut:        300 Hz                 │
│    High Cut:       3400 Hz                │
│                                           │
│  [←] -1 dB  [→] +1 dB  [⇧←] -10 dB  [⇧→] +10 dB  │
│  [Enter] Edit value  [Esc] Close          │
└───────────────────────────────────────────┘
```

### Interaction Model

| Key | Action |
|-----|--------|
| `↑` / `↓` | Navigate between parameters |
| `←` / `→` | Fine adjustment (±1 minimum unit) |
| `Shift+←` / `Shift+→` | Coarse adjustment (±10x or large step) |
| `Enter` | Direct value input (type exact number) |
| `Space` or `Enter` on toggles | Toggle ON/OFF |
| `Esc` | Close modal |

### Per-Parameter Increments

| Parameter | Range | Fine (←→) | Coarse (⇧←⇧→) | Unit |
|-----------|-------|-----------|----------------|------|
| Volume | 0–100 | ±1 | ±10 | % |
| Squelch | −120 – 0 | ±1 | ±10 | dB |
| Channel BW | 1k–25k | ±500 | ±5000 | Hz |
| Demod Mode | AM/FM/NFM | cycle | cycle | — |
| AGC Enabled | ON/OFF | toggle | toggle | — |
| AGC Attack | 1–100 | ±1 | ±10 | ms |
| AGC Decay | 50–2000 | ±10 | ±100 | ms |
| AGC Target | 0.05–1.0 | ±0.05 | ±0.1 | — |
| AGC Max Gain | 1–100 | ±1 | ±10 | dB |
| Bandpass Enabled | ON/OFF | toggle | toggle | — |
| Bandpass Low | 50–1000 | ±50 | ±100 | Hz |
| Bandpass High | 1000–8000 | ±100 | ±500 | Hz |

### Dynamic Legend

The bottom bar of the modal updates based on the selected parameter, always showing exactly what ← → ⇧← ⇧→ will do:

- On "Volume": `[←] -1%  [→] +1%  [⇧←] -10%  [⇧→] +10%`
- On "Squelch": `[←] -1 dB  [→] +1 dB  [⇧←] -10 dB  [⇧→] +10 dB`
- On "AGC Enabled": `[←→] Toggle  [Enter] Toggle`

---

## Implementation Order

1. **AGC** — Highest audio quality impact
2. **Audio Bandpass Filter** — Second highest impact
3. **Settings Modal UI** — Unified control for all new + existing params
4. **Channel BW default adjustment** — Minor tweak, big noise reduction

---

## References

- **SDR++** — Recommends 6–8 kHz bandwidth, AM mode, carrier AGC for aviation
- **GQRX** — GNU Radio-based, uses RF AGC + audio filters
- **RTLSDR-Airband** — Dedicated aviation AM demodulator, uses post-demod AGC
- **ATCO2 Project** — Professional ATC recording setup guide (gain tuning, FFT size)
- **ITU-T G.711** — Telephone voice bandwidth standard (300–3400 Hz)
