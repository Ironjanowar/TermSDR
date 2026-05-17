# TermSDR — Domain Context

TermSDR is a terminal-based Software-Defined Radio application. This glossary defines the SDR and DSP terms as used in this project to prevent confusion between contributors (human or AI).

## Language

### Signal & Sampling

**IQ Sample**:
A complex-valued sample (I = in-phase = real, Q = quadrature = imaginary) representing a radio signal at baseband. Type: `Complex<f32>`.
_Avoid_: audio sample, raw sample

**Sample Rate**:
The rate at which IQ samples are captured from the SDR hardware, in Hz (e.g., 2.048 MHz). Distinct from **Audio Rate**.
_Avoid_: bandwidth (related but different)

**Audio Rate**:
The playback sample rate of demodulated audio sent to the audio sink, in Hz (e.g., 48000 Hz). Always lower than **Sample Rate** after decimation.
_Avoid_: sample rate (ambiguous — always qualify as "audio rate" or "SDR sample rate")

**Center Frequency**:
The frequency the SDR hardware is tuned to, in Hz. The middle of the captured spectrum. IQ samples are relative to this frequency.
_Avoid_: channel frequency, tuned frequency

### DSP Pipeline

**FFT Bin**:
One element of the FFT output array, representing power at a specific frequency. Index 0 = center frequency minus half the sample rate.
_Avoid_: channel, bucket, slot

**FftResult**:
The output of an FFT computation: a vector of power values in dB, plus metadata (center frequency, sample rate).

**Windowing**:
A function applied to IQ samples before FFT to reduce spectral leakage (e.g., Hann, Blackman-Harris).

**Decimation**:
Reducing the sample rate by keeping every Nth sample (after low-pass filtering). Used to bring the SDR sample rate down to the **Audio Rate**.
_Avoid_: downsampling (acceptable synonym but we prefer "decimation")

### Demodulation

**Demodulation**:
Extracting the audio signal from modulated IQ data. TermSDR supports AM, FM, and NFM modes.
_Avoid_: decoding (decoding implies digital protocols, not analog audio extraction)

**DemodMode**:
The enum representing the current demodulation type (AM, FM, NFM). Each mode has a default channel bandwidth.

**Channel Bandwidth**:
The width of the frequency band around the center that is demodulated, in Hz. Depends on the **DemodMode** (e.g., NFM = 12.5 kHz, FM = 150 kHz).
_Avoid_: filter width, passband (acceptable in DSP context but we use "channel bandwidth" consistently)

**AGC (Automatic Gain Control)**:
Normalizes audio amplitude dynamically. Has attack (fast gain reduction) and decay (slow gain increase) parameters.

### Display

**Spectrum**:
The frequency-domain power display — a line chart of dB vs frequency, updated in real time.
_Avoid_: FFT display, frequency plot

**Waterfall**:
A scrolling time-frequency display where color represents power. New FFT results appear at the top and scroll downward.
_Avoid_: spectrogram (acceptable synonym but we use "waterfall" in code and UI)

### Sources

**Source**:
A trait representing anything that produces IQ samples: hardware SDR, file replay, or synthetic test signal.

**TestSource**:
A synthetic signal generator implementing **Source**. The foundation of TDD — generates IQ data with known properties (noise floor, signal peaks at controlled offsets).
_Avoid_: mock source, fake source (it's not a mock — it produces real, mathematically valid IQ data)

### Server

**Headless Mode**:
Running without the TUI, only the WebSocket server. Activated with `--headless`.

**Audio Sink**:
The destination for demodulated audio: either the system sound device (`CpalSink`) or a WebSocket stream (`WsSink`).

## Relationships

- A **Source** produces **IQ Samples** at a given **Sample Rate** and **Center Frequency**
- **IQ Samples** are windowed and transformed via FFT into **FFT Bins** (packaged as **FftResult**)
- **FftResult** feeds the **Spectrum** and **Waterfall** displays
- **IQ Samples** are also passed through **Demodulation** (filtered by **Channel Bandwidth**, then demodulated according to **DemodMode**)
- Demodulated audio is **Decimated** from **Sample Rate** to **Audio Rate** and sent to an **Audio Sink**
- **AGC** is applied after demodulation, before the **Audio Sink**

## Example dialogue

> **Dev:** "When the user changes the **Center Frequency**, do we reset the **Waterfall**?"
> **Domain expert:** "Yes — the old waterfall data is at different frequencies, so it's meaningless after retuning."

> **Dev:** "Is **Decimation** part of **Demodulation**?"
> **Domain expert:** "It's in the same pipeline but conceptually separate. Demodulation extracts audio from IQ; decimation adjusts the rate afterward. They happen sequentially in the audio thread."

> **Dev:** "Can I call the **TestSource** a mock?"
> **Domain expert:** "No — mocks fake behavior. TestSource generates mathematically valid IQ data with known properties. It's a real source, just synthetic."

## Flagged ambiguities

- **"sample rate"** was ambiguous — resolved: always qualify as **Sample Rate** (SDR capture rate) or **Audio Rate** (playback rate). Never use "sample rate" alone.
- **"channel"** was overloaded — resolved: use **FFT Bin** for frequency bins, **Channel Bandwidth** for demod filter width, and avoid "channel" standalone.
