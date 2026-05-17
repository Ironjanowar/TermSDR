//! Demodulation pipeline: frequency shift → LPF → decimate → demod (AM/FM/NFM) → DC removal.

use serde::{Serialize, Deserialize};

use crate::types::IqSample;
use std::f64::consts::PI;

/// Default channel bandwidth for AM mode in Hz.
pub const AM_DEFAULT_CHANNEL_BW: f64 = 8_000.0;

/// Default channel bandwidth for wideband FM mode in Hz.
pub const FM_DEFAULT_CHANNEL_BW: f64 = 200_000.0;

/// Default channel bandwidth for narrowband FM mode in Hz.
pub const NFM_DEFAULT_CHANNEL_BW: f64 = 25_000.0;

/// Default DC removal filter coefficient (IIR high-pass alpha).
pub const DC_REMOVAL_ALPHA: f64 = 0.995;

/// Number of FIR filter taps for channel filtering.
pub const FIR_NUM_TAPS: usize = 101;

/// Default AGC attack time in milliseconds.
pub const DEFAULT_AGC_ATTACK_MS: f64 = 10.0;

/// Default AGC decay time in milliseconds.
pub const DEFAULT_AGC_DECAY_MS: f64 = 300.0;

/// Default AGC target output level (0.0–1.0).
pub const DEFAULT_AGC_TARGET_LEVEL: f32 = 0.3;

/// Default AGC maximum gain in dB.
pub const DEFAULT_AGC_MAX_GAIN_DB: f32 = 50.0;

/// Default bandpass filter low cutoff in Hz (voice low end).
pub const DEFAULT_BANDPASS_LOW_HZ: f64 = 300.0;

/// Default bandpass filter high cutoff in Hz (voice high end).
pub const DEFAULT_BANDPASS_HIGH_HZ: f64 = 3400.0;

/// FM de-emphasis time constant in seconds (50 µs, European standard).
pub const FM_DEEMPHASIS_TAU: f64 = 50e-6;

// ============================================================================
// Demodulation Mode
// ============================================================================

/// Demodulation mode for a VFO or cursor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[allow(clippy::upper_case_acronyms)]
pub enum DemodMode {
    /// Amplitude Modulation (aviation 118-137 MHz, HF)
    AM,
    /// Wideband FM (commercial radio 87-108 MHz, BW: 200 kHz)
    FM,
    /// Narrowband FM (walkie-talkies, PMR446, BW: 25 kHz)
    NFM,
}

impl DemodMode {
    /// Default channel bandwidth for this demodulation mode.
    #[must_use] 
    pub fn default_channel_bw(self) -> f64 {
        match self {
            DemodMode::AM => AM_DEFAULT_CHANNEL_BW,
            DemodMode::FM => FM_DEFAULT_CHANNEL_BW,
            DemodMode::NFM => NFM_DEFAULT_CHANNEL_BW,
        }
    }

    /// Short label for display.
    #[must_use] 
    pub fn label(self) -> &'static str {
        match self {
            DemodMode::AM => "AM",
            DemodMode::FM => "FM",
            DemodMode::NFM => "NFM",
        }
    }

    /// Cycle to the next mode: AM → FM → NFM → AM.
    #[must_use] 
    pub fn next(self) -> DemodMode {
        match self {
            DemodMode::AM => DemodMode::FM,
            DemodMode::FM => DemodMode::NFM,
            DemodMode::NFM => DemodMode::AM,
        }
    }
}

// ============================================================================
// 1.1 Frequency Shift (Mix to Baseband)
// ============================================================================

/// Shift IQ samples in frequency by multiplying with a local oscillator.
///
/// Multiplies each sample by `e^(-j * 2π * shift_freq * t)` to move
/// the signal at `shift_freq` down to baseband (0 Hz).
///
/// `phase` is tracked across calls to avoid discontinuities.
pub fn frequency_shift(
    samples: &[IqSample],
    shift_freq: f64,
    sample_rate: f64,
    phase: &mut f64,
) -> Vec<IqSample> {
    let phase_step = -2.0 * PI * shift_freq / sample_rate;
    let mut output = Vec::with_capacity(samples.len());

    for &sample in samples {
        #[allow(clippy::cast_possible_truncation)] // intentional truncation
        let cos_val = phase.cos() as f32;
        #[allow(clippy::cast_possible_truncation)] // intentional truncation
        let sin_val = phase.sin() as f32;
        let lo = IqSample::new(cos_val, sin_val);
        output.push(sample * lo);

        *phase += phase_step;
        // Keep phase in [-π, π] to avoid precision loss over time
        if *phase > PI {
            *phase -= 2.0 * PI;
        } else if *phase < -PI {
            *phase += 2.0 * PI;
        }
    }

    output
}

// ============================================================================
// 1.2 Low-Pass FIR Filter
// ============================================================================

/// A FIR (Finite Impulse Response) low-pass filter.
pub struct FirFilter {
    taps: Vec<f32>,
    delay_line: Vec<IqSample>,
    pos: usize,
}

impl FirFilter {
    /// Design a low-pass FIR filter using the windowed sinc method with Blackman window.
    ///
    /// # Arguments
    /// * `cutoff_hz` - Cutoff frequency in Hz
    /// * `sample_rate` - Sample rate in Hz
    /// * `num_taps` - Number of filter taps (should be odd)
    #[must_use] 
    #[allow(clippy::cast_possible_truncation)] // intentional truncation
    pub fn low_pass(cutoff_hz: f64, sample_rate: f64, num_taps: usize) -> Self {
        // Ensure odd number of taps
        let num_taps = if num_taps.is_multiple_of(2) { num_taps + 1 } else { num_taps };
        #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
        let m = (num_taps - 1) as f64;
        let fc = cutoff_hz / sample_rate; // Normalized cutoff frequency

        let mut taps = Vec::with_capacity(num_taps);

        for i in 0..num_taps {
            #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
            let n = i as f64;
            // Sinc function
            let sinc = if (n - m / 2.0).abs() < 1e-10 {
                2.0 * fc
            } else {
                let x = 2.0 * PI * fc * (n - m / 2.0);
                x.sin() / (PI * (n - m / 2.0))
            };

            // Blackman window
            let window = 0.42 - 0.5 * (2.0 * PI * n / m).cos()
                + 0.08 * (4.0 * PI * n / m).cos();

            taps.push((sinc * window) as f32);
        }

        // Normalize taps so they sum to 1.0 (unity gain at DC)
        let sum: f32 = taps.iter().sum();
        if sum.abs() > 1e-10 {
            for tap in &mut taps {
                *tap /= sum;
            }
        }

        let delay_line = vec![IqSample::new(0.0, 0.0); num_taps];

        Self {
            taps,
            delay_line,
            pos: 0,
        }
    }

    /// Process IQ samples through the filter.
    pub fn process(&mut self, samples: &[IqSample]) -> Vec<IqSample> {
        let n = self.taps.len();
        let mut output = Vec::with_capacity(samples.len());

        for &sample in samples {
            // Insert new sample into circular delay line
            self.delay_line[self.pos] = sample;

            // Compute convolution
            let mut acc = IqSample::new(0.0, 0.0);
            for j in 0..n {
                let idx = (self.pos + n - j) % n;
                let tap = self.taps[j];
                acc.re += self.delay_line[idx].re * tap;
                acc.im += self.delay_line[idx].im * tap;
            }

            output.push(acc);

            self.pos = (self.pos + 1) % n;
        }

        output
    }

    /// Get the number of taps (filter length).
    #[must_use]
    #[allow(dead_code)]
    pub fn num_taps(&self) -> usize {
        self.taps.len()
    }
}

// ============================================================================
// 1.3 Decimation
// ============================================================================

/// Decimate (downsample) IQ samples by taking every `factor`-th sample.
///
/// IMPORTANT: Low-pass filter should be applied BEFORE decimation to prevent aliasing.
#[must_use] 
pub fn decimate(samples: &[IqSample], factor: usize) -> Vec<IqSample> {
    if factor == 0 {
        return Vec::new();
    }
    samples.iter().step_by(factor).copied().collect()
}

// ============================================================================
// 1.4 AM Envelope Detection
// ============================================================================

/// AM demodulation via envelope detection: extract the magnitude of each IQ sample.
///
/// `magnitude = sqrt(I² + Q²)`
#[must_use] 
pub fn am_demod(samples: &[IqSample]) -> Vec<f32> {
    samples.iter().map(|s| s.norm()).collect()
}

// ============================================================================
// 1.4b FM Discriminator
// ============================================================================

/// FM demodulation via frequency discriminator.
///
/// `audio[n] = arg(sample[n] * conj(sample[n-1]))`
///
/// The output is the instantaneous frequency deviation (in radians per sample).
#[must_use] 
pub fn fm_demod(samples: &[IqSample]) -> Vec<f32> {
    if samples.len() < 2 {
        return vec![0.0; samples.len()];
    }
    let mut output = Vec::with_capacity(samples.len());
    output.push(0.0); // first sample has no previous
    for i in 1..samples.len() {
        let product = samples[i] * samples[i - 1].conj();
        output.push(product.arg());
    }
    output
}

// ============================================================================
// 1.4c De-emphasis Filter
// ============================================================================

/// IIR de-emphasis filter for FM audio.
///
/// Applies a single-pole low-pass to counteract the FM pre-emphasis used in
/// broadcast FM transmission.
pub struct DeEmphasis {
    alpha: f32,
    prev_output: f32,
}

impl DeEmphasis {
    /// Create a de-emphasis filter.
    ///
    /// # Arguments
    /// * `sample_rate` - Audio sample rate in Hz
    /// * `tau` - Time constant in seconds (50e-6 for Europe, 75e-6 for US)
    #[must_use] 
    pub fn new(sample_rate: f64, tau: f64) -> Self {
        #[allow(clippy::cast_possible_truncation)] // intentional truncation
        let alpha = (-1.0 / (sample_rate * tau)).exp() as f32;
        Self {
            alpha,
            prev_output: 0.0,
        }
    }

    /// Get the alpha coefficient (for testing).
    #[must_use]
    #[allow(dead_code)]
    pub fn alpha(&self) -> f32 {
        self.alpha
    }

    /// Process audio samples through the de-emphasis filter.
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(samples.len());
        for &s in samples {
            let out = s * (1.0 - self.alpha) + self.prev_output * self.alpha;
            self.prev_output = out;
            output.push(out);
        }
        output
    }
}

// ============================================================================
// 1.5 DC Removal
// ============================================================================

/// DC removal filter using a simple IIR high-pass.
///
/// Transfer function: `y[n] = x[n] - x[n-1] + α * y[n-1]`
/// where α ≈ 0.995 (closer to 1.0 = lower cutoff frequency).
pub struct DcRemoval {
    alpha: f64,
    prev_input: f64,
    prev_output: f64,
}

impl DcRemoval {
    /// Create a new DC removal filter.
    ///
    /// `alpha` controls the cutoff frequency. Typical value: 0.995.
    /// Higher alpha = lower cutoff = removes only DC. Lower alpha = removes more low freq.
    #[must_use] 
    pub fn new(alpha: f64) -> Self {
        Self {
            alpha,
            prev_input: 0.0,
            prev_output: 0.0,
        }
    }

    /// Process audio samples, removing the DC offset.
    #[allow(clippy::cast_possible_truncation)] // intentional truncation
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(samples.len());

        for &sample in samples {
            let x = f64::from(sample);
            let y = x - self.prev_input + self.alpha * self.prev_output;
            self.prev_input = x;
            self.prev_output = y;
            output.push(y as f32);
        }

        output
    }
}

// ============================================================================
// 1.6 AGC (Automatic Gain Control)
// ============================================================================

/// RMS-based Automatic Gain Control with dual time constants.
///
/// Tracks the RMS level of audio using an exponential moving average and
/// adjusts gain to maintain a target output level. Uses separate attack
/// (gain decrease) and decay (gain increase) time constants.
pub struct Agc {
    enabled: bool,
    attack_coeff: f64,
    decay_coeff: f64,
    target_level: f32,
    max_gain_linear: f32,
    current_gain: f32,
    rms_estimate: f64,
    sample_rate: f64,
}

impl Agc {
    /// Create a new AGC processor.
    ///
    /// # Arguments
    /// * `sample_rate` - Audio sample rate in Hz
    /// * `attack_ms` - Attack time in ms (how fast gain drops on loud signal)
    /// * `decay_ms` - Decay time in ms (how fast gain rises on weak signal)
    /// * `target_level` - Desired RMS output level (e.g., 0.3)
    /// * `max_gain_db` - Maximum amplification in dB (e.g., 50.0)
    #[must_use] 
    pub fn new(sample_rate: f64, attack_ms: f64, decay_ms: f64, target_level: f32, max_gain_db: f32) -> Self {
        let attack_coeff = (-1.0 / (sample_rate * attack_ms / 1000.0)).exp();
        let decay_coeff = (-1.0 / (sample_rate * decay_ms / 1000.0)).exp();
        let max_gain_linear = 10.0_f32.powf(max_gain_db / 20.0);
        Self {
            enabled: true,
            attack_coeff,
            decay_coeff,
            target_level,
            max_gain_linear,
            current_gain: 1.0,
            rms_estimate: 0.0,
            sample_rate,
        }
    }

    /// Process audio samples through AGC.
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        if !self.enabled {
            return samples.to_vec();
        }

        let mut output = Vec::with_capacity(samples.len());
        for &s in samples {
            // Update RMS estimate with exponential moving average
            let sq = f64::from(s) * f64::from(s);
            // Use attack when signal is getting louder, decay when getting quieter
            let coeff = if sq > self.rms_estimate {
                self.attack_coeff
            } else {
                self.decay_coeff
            };
            self.rms_estimate = coeff * self.rms_estimate + (1.0 - coeff) * sq;

            #[allow(clippy::cast_possible_truncation)] // intentional truncation
            let rms = (self.rms_estimate.max(1e-20)).sqrt() as f32;

            // Compute desired gain
            let desired_gain = self.target_level / rms;

            // Clamp gain — allow attenuation down to -20dB (0.1) for loud signals
            self.current_gain = desired_gain.clamp(0.1, self.max_gain_linear);

            output.push(s * self.current_gain);
        }
        output
    }

    /// Set attack time in milliseconds.
    pub fn set_attack_ms(&mut self, ms: f64) {
        self.attack_coeff = (-1.0 / (self.sample_rate * ms / 1000.0)).exp();
    }

    /// Set decay time in milliseconds.
    pub fn set_decay_ms(&mut self, ms: f64) {
        self.decay_coeff = (-1.0 / (self.sample_rate * ms / 1000.0)).exp();
    }

    /// Set target output level.
    pub fn set_target_level(&mut self, level: f32) {
        self.target_level = level;
    }

    /// Set maximum gain in dB.
    pub fn set_max_gain_db(&mut self, db: f32) {
        self.max_gain_linear = 10.0_f32.powf(db / 20.0);
    }

    /// Enable or disable AGC.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Check if AGC is enabled.
    #[must_use]
    #[allow(dead_code)]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Get current gain in dB.
    #[must_use]
    #[allow(dead_code)]
    pub fn current_gain_db(&self) -> f32 {
        20.0 * self.current_gain.log10()
    }
}

// ============================================================================
// 1.7 Full Demodulation Pipeline
// ============================================================================

/// Complete demodulator that chains all DSP stages.
/// Supports AM, FM (wideband), and NFM (narrowband) modes.
pub struct Demodulator {
    fir_filter: FirFilter,
    dc_removal: DcRemoval,
    shift_phase: f64,
    decimation_factor: usize,
    cursor_freq: f64,
    center_freq: f64,
    input_sample_rate: f64,
    mode: DemodMode,
    de_emphasis: Option<DeEmphasis>,
    agc: Agc,
    audio_bandpass: AudioBandpass,
    prev_sample: IqSample,
    /// Pre-AGC RMS level of the most recent block (used for power-weighted mixing)
    pre_agc_rms: f32,
}

impl Demodulator {
    /// Create a new demodulator.
    ///
    /// # Arguments
    /// * `center_freq` - Center frequency of the SDR in Hz
    /// * `cursor_freq` - Frequency to demodulate (cursor position) in Hz
    /// * `channel_bw` - Channel bandwidth in Hz
    /// * `input_sample_rate` - Input sample rate from SDR in Hz
    /// * `output_sample_rate` - Desired audio output sample rate in Hz (e.g., 48000)
    /// * `mode` - Demodulation mode (AM, FM, NFM)
    #[must_use] 
    pub fn new(
        center_freq: f64,
        cursor_freq: f64,
        channel_bw: f64,
        input_sample_rate: f64,
        output_sample_rate: f64,
        mode: DemodMode,
    ) -> Self {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // intentional truncation; value is non-negative here
        let decimation_factor = (input_sample_rate / output_sample_rate).round() as usize;
        let decimation_factor = decimation_factor.max(1);

        // FIR filter: cutoff at half the channel bandwidth
        // Use 101 taps — good tradeoff between rejection and latency
        let fir_filter = FirFilter::low_pass(channel_bw / 2.0, input_sample_rate, FIR_NUM_TAPS);

        let de_emphasis = match mode {
            DemodMode::FM => {
                #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
                let output_sr = input_sample_rate / decimation_factor as f64;
                Some(DeEmphasis::new(output_sr, FM_DEEMPHASIS_TAU))
            }
            _ => None,
        };

        #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
        let actual_output_sr = input_sample_rate / decimation_factor as f64;
        Self {
            fir_filter,
            dc_removal: DcRemoval::new(DC_REMOVAL_ALPHA),
            shift_phase: 0.0,
            decimation_factor,
            cursor_freq,
            center_freq,
            input_sample_rate,
            mode,
            de_emphasis,
            agc: Agc::new(actual_output_sr, DEFAULT_AGC_ATTACK_MS, DEFAULT_AGC_DECAY_MS, DEFAULT_AGC_TARGET_LEVEL, DEFAULT_AGC_MAX_GAIN_DB),
            audio_bandpass: AudioBandpass::new(DEFAULT_BANDPASS_LOW_HZ, DEFAULT_BANDPASS_HIGH_HZ, actual_output_sr, FIR_NUM_TAPS),
            prev_sample: IqSample::new(0.0, 0.0),
            pre_agc_rms: 0.0,
        }
    }

    /// Create a new AM demodulator (convenience constructor for backward compatibility).
    #[must_use] 
    pub fn new_am(
        center_freq: f64,
        cursor_freq: f64,
        channel_bw: f64,
        input_sample_rate: f64,
        output_sample_rate: f64,
    ) -> Self {
        Self::new(center_freq, cursor_freq, channel_bw, input_sample_rate, output_sample_rate, DemodMode::AM)
    }

    /// Process a block of IQ samples through the full demodulation pipeline.
    ///
    /// Returns audio samples at the output sample rate.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI; intentional truncation
    pub fn process(&mut self, samples: &[IqSample]) -> Vec<f32> {
        // 1. Frequency shift: move cursor frequency to baseband
        let shift_freq = self.cursor_freq - self.center_freq;
        let shifted = frequency_shift(samples, shift_freq, self.input_sample_rate, &mut self.shift_phase);

        // 2. Low-pass filter: keep only the channel
        let filtered = self.fir_filter.process(&shifted);

        // 3. Decimate: reduce sample rate
        let decimated = decimate(&filtered, self.decimation_factor);

        // 4. Demodulate based on mode
        let audio = match self.mode {
            DemodMode::AM => am_demod(&decimated),
            DemodMode::FM | DemodMode::NFM => {
                // Prepend prev_sample for continuity across blocks
                let mut with_prev = Vec::with_capacity(decimated.len() + 1);
                with_prev.push(self.prev_sample);
                with_prev.extend_from_slice(&decimated);
                if let Some(last) = decimated.last() {
                    self.prev_sample = *last;
                }
                let raw = fm_demod(&with_prev);
                // Skip first element (from prepended prev_sample)
                raw[1..].to_vec()
            }
        };

        // 5. Apply de-emphasis for FM mode only
        let audio = if let Some(ref mut de_emph) = self.de_emphasis {
            de_emph.process(&audio)
        } else {
            audio
        };

        // 6. Audio bandpass filter (voice frequencies)
        let audio = self.audio_bandpass.process(&audio);

        // 6.5 Measure pre-AGC RMS for power-weighted mixing
        if audio.is_empty() {
            self.pre_agc_rms = 0.0;
        } else {
            let sum_sq: f64 = audio.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
            self.pre_agc_rms = (sum_sq / audio.len() as f64).sqrt() as f32;
        }

        // 7. AGC
        let audio = self.agc.process(&audio);

        // 8. DC removal
        self.dc_removal.process(&audio)
    }

    /// Update the cursor (tuning) frequency. Recalculates the shift.
    pub fn set_cursor_freq(&mut self, freq: f64) {
        self.cursor_freq = freq;
        // Phase continues naturally — no need to reset
    }

    /// Update the center frequency (when SDR retunes).
    pub fn set_center_freq(&mut self, freq: f64) {
        self.center_freq = freq;
    }

    /// Update the channel bandwidth. Recalculates the FIR filter.
    pub fn set_channel_bw(&mut self, bw: f64) {
        self.fir_filter = FirFilter::low_pass(bw / 2.0, self.input_sample_rate, FIR_NUM_TAPS);
    }

    /// Set the demodulation mode. Reconfigures de-emphasis and resets FM state.
    pub fn set_mode(&mut self, mode: DemodMode) {
        self.mode = mode;
        self.de_emphasis = match mode {
            DemodMode::FM => {
                #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
                let output_sr = self.input_sample_rate / self.decimation_factor as f64;
                Some(DeEmphasis::new(output_sr, FM_DEEMPHASIS_TAU))
            }
            _ => None,
        };
        self.prev_sample = IqSample::new(0.0, 0.0);
    }

    /// Get the current demodulation mode.
    #[must_use]
    #[allow(dead_code)]
    pub fn mode(&self) -> DemodMode {
        self.mode
    }

    /// Get the current decimation factor.
    #[must_use]
    #[allow(dead_code)]
    pub fn decimation_factor(&self) -> usize {
        self.decimation_factor
    }

    /// Get the actual output sample rate after decimation.
    #[must_use]
    #[allow(dead_code)]
    #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
    pub fn actual_output_sample_rate(&self) -> f64 {
        self.input_sample_rate / self.decimation_factor as f64
    }

    /// Enable or disable AGC.
    pub fn set_agc_enabled(&mut self, enabled: bool) {
        self.agc.set_enabled(enabled);
    }

    /// Set AGC attack time in milliseconds.
    pub fn set_agc_attack(&mut self, ms: f64) {
        self.agc.set_attack_ms(ms);
    }

    /// Set AGC decay time in milliseconds.
    pub fn set_agc_decay(&mut self, ms: f64) {
        self.agc.set_decay_ms(ms);
    }

    /// Set AGC target output level.
    pub fn set_agc_target(&mut self, level: f32) {
        self.agc.set_target_level(level);
    }

    /// Set AGC maximum gain in dB.
    pub fn set_agc_max_gain(&mut self, db: f32) {
        self.agc.set_max_gain_db(db);
    }

    pub fn set_bandpass_enabled(&mut self, enabled: bool) {
        self.audio_bandpass.set_enabled(enabled);
    }

    pub fn set_bandpass_low_cut(&mut self, hz: f64) {
        self.audio_bandpass.set_low_cut(hz);
    }

    pub fn set_bandpass_high_cut(&mut self, hz: f64) {
        self.audio_bandpass.set_high_cut(hz);
    }

    /// Get the pre-AGC RMS level from the most recent `process()` call.
    /// Used for power-weighted mixing of multiple VFOs.
    #[must_use] 
    pub fn pre_agc_rms(&self) -> f32 {
        self.pre_agc_rms
    }
}

/// Type alias for backward compatibility.
#[allow(dead_code)]
pub type AmDemodulator = Demodulator;

// ============================================================================
// 1.8 Audio Bandpass Filter
// ============================================================================

/// FIR bandpass filter for f32 audio samples.
///
/// Applied after demodulation to filter voice frequencies (default 300-3400 Hz,
/// matching ITU-T G.711 telephone bandwidth for aviation AM voice).
pub struct AudioBandpass {
    taps: Vec<f32>,
    delay_line: Vec<f32>,
    pos: usize,
    low_cut_hz: f64,
    high_cut_hz: f64,
    sample_rate: f64,
    num_taps: usize,
    enabled: bool,
}

impl AudioBandpass {
    /// Create a new audio bandpass filter.
    ///
    /// # Arguments
    /// * `low_hz` - Lower cutoff frequency in Hz (high-pass, removes rumble/hum)
    /// * `high_hz` - Upper cutoff frequency in Hz (low-pass, removes hiss/noise)
    /// * `sample_rate` - Audio sample rate in Hz
    /// * `num_taps` - Number of filter taps (should be odd)
    #[must_use] 
    pub fn new(low_hz: f64, high_hz: f64, sample_rate: f64, num_taps: usize) -> Self {
        let num_taps = if num_taps.is_multiple_of(2) { num_taps + 1 } else { num_taps };
        let taps = Self::design_bandpass(low_hz, high_hz, sample_rate, num_taps);
        let delay_line = vec![0.0f32; num_taps];
        Self {
            taps,
            delay_line,
            pos: 0,
            low_cut_hz: low_hz,
            high_cut_hz: high_hz,
            sample_rate,
            num_taps,
            enabled: true,
        }
    }

    /// Design a bandpass FIR kernel using windowed sinc method.
    ///
    /// Bandpass = `lowpass(high_cutoff)` - `lowpass(low_cutoff)`, both windowed with Blackman.
    #[allow(clippy::cast_possible_truncation)] // intentional truncation
    fn design_bandpass(low_hz: f64, high_hz: f64, sample_rate: f64, num_taps: usize) -> Vec<f32> {
        #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
        let m = (num_taps - 1) as f64;
        let fc_low = low_hz / sample_rate;
        let fc_high = high_hz / sample_rate;

        let mut taps = Vec::with_capacity(num_taps);

        for i in 0..num_taps {
            #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
            let n = i as f64;
            let center = m / 2.0;

            // High cutoff lowpass sinc
            let sinc_high = if (n - center).abs() < 1e-10 {
                2.0 * fc_high
            } else {
                let x = 2.0 * PI * fc_high * (n - center);
                x.sin() / (PI * (n - center))
            };

            // Low cutoff lowpass sinc
            let sinc_low = if (n - center).abs() < 1e-10 {
                2.0 * fc_low
            } else {
                let x = 2.0 * PI * fc_low * (n - center);
                x.sin() / (PI * (n - center))
            };

            // Bandpass = highpass_lp - lowpass_lp
            let bp = sinc_high - sinc_low;

            // Blackman window
            let window = 0.42 - 0.5 * (2.0 * PI * n / m).cos()
                + 0.08 * (4.0 * PI * n / m).cos();

            taps.push((bp * window) as f32);
        }

        taps
    }

    /// Process f32 audio samples through the bandpass filter.
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        if !self.enabled {
            return samples.to_vec();
        }

        let n = self.taps.len();
        let mut output = Vec::with_capacity(samples.len());

        for &sample in samples {
            self.delay_line[self.pos] = sample;

            let mut acc = 0.0f32;
            for j in 0..n {
                let idx = (self.pos + n - j) % n;
                acc += self.delay_line[idx] * self.taps[j];
            }

            output.push(acc);
            self.pos = (self.pos + 1) % n;
        }

        output
    }

    /// Set the lower cutoff frequency and rebuild taps.
    pub fn set_low_cut(&mut self, hz: f64) {
        self.low_cut_hz = hz;
        self.taps = Self::design_bandpass(self.low_cut_hz, self.high_cut_hz, self.sample_rate, self.num_taps);
        self.delay_line = vec![0.0f32; self.num_taps];
        self.pos = 0;
    }

    /// Set the upper cutoff frequency and rebuild taps.
    pub fn set_high_cut(&mut self, hz: f64) {
        self.high_cut_hz = hz;
        self.taps = Self::design_bandpass(self.low_cut_hz, self.high_cut_hz, self.sample_rate, self.num_taps);
        self.delay_line = vec![0.0f32; self.num_taps];
        self.pos = 0;
    }

    /// Enable or disable the filter. When disabled, `process()` is a passthrough.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Returns whether the filter is enabled.
    #[must_use]
    #[allow(dead_code)]
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::test_signal::{generate_am_signal, generate_fm_signal, generate_tone};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn decimate_output_length(input_len in 1usize..=1000, factor in 1usize..=16) {
            let input: Vec<IqSample> = vec![IqSample::new(0.0, 0.0); input_len];
            let output = decimate(&input, factor);
            let expected = (input_len + factor - 1) / factor;
            prop_assert_eq!(output.len(), expected);
        }

        #[test]
        fn dc_removal_output_mean_near_zero(len in 1000usize..=5000, dc_offset in -10.0f32..10.0) {
            let input: Vec<f32> = vec![dc_offset; len];
            let mut remover = DcRemoval::new(DC_REMOVAL_ALPHA);
            let output = remover.process(&input);
            // After settling, last samples should be near zero
            let tail = &output[output.len() - 100..];
            let mean: f32 = tail.iter().sum::<f32>() / tail.len() as f32;
            prop_assert!(mean.abs() < 0.5, "DC removal mean {} too high for offset {}", mean, dc_offset);
        }

        #[test]
        fn fm_demod_constant_frequency_near_zero(len in 10usize..=500) {
            // Constant frequency (no modulation) should produce near-zero output
            let carrier: Vec<IqSample> = (0..len).map(|_| IqSample::new(1.0, 0.0)).collect();
            let output = fm_demod(&carrier);
            for &s in &output[1..] {
                prop_assert!(s.abs() < 0.01, "FM demod of constant carrier should be near zero, got {}", s);
            }
        }
    }

    // ========================================================================
    // 1. Frequency shift tests
    // ========================================================================

    #[test]
    fn test_frequency_shift_tone_to_dc() {
        // Generate a tone at +100kHz, then shift by +100kHz → should land at DC
        let sample_rate = 2.048e6;
        let freq = 100_000.0;
        let samples = generate_tone(freq, 1.0, sample_rate, 4096);

        let mut phase = 0.0;
        let shifted = frequency_shift(&samples, freq, sample_rate, &mut phase);

        // After shifting, signal should be at DC: I ≈ constant, Q ≈ 0
        // Skip first few samples for transients
        for s in &shifted[100..] {
            assert!(
                s.im.abs() < 0.05,
                "Q should be ~0 after shifting tone to DC, got {}",
                s.im
            );
            assert!(
                (s.re - 1.0).abs() < 0.05,
                "I should be ~1.0 after shifting tone to DC, got {}",
                s.re
            );
        }
    }

    #[test]
    fn test_frequency_shift_preserves_magnitude() {
        let sample_rate = 2.048e6;
        let samples = generate_tone(200_000.0, 0.75, sample_rate, 2048);

        let mut phase = 0.0;
        let shifted = frequency_shift(&samples, 50_000.0, sample_rate, &mut phase);

        // Magnitude should be preserved
        for (orig, shifted) in samples.iter().zip(shifted.iter()) {
            let orig_mag = orig.norm();
            let shifted_mag = shifted.norm();
            assert!(
                (orig_mag - shifted_mag).abs() < 0.01,
                "Magnitude not preserved: {} vs {}",
                orig_mag,
                shifted_mag
            );
        }
    }

    #[test]
    fn test_frequency_shift_phase_continuity() {
        // Process in two blocks, verify phase continuity
        let sample_rate = 2.048e6;
        let freq = 100_000.0;
        let all_samples = generate_tone(freq, 1.0, sample_rate, 4096);

        let mut phase = 0.0;
        let shifted_all = frequency_shift(&all_samples, freq, sample_rate, &mut phase);

        // Reset and process in two halves
        let mut phase2 = 0.0;
        let shifted_first = frequency_shift(&all_samples[..2048], freq, sample_rate, &mut phase2);
        let shifted_second = frequency_shift(&all_samples[2048..], freq, sample_rate, &mut phase2);

        // Results should match
        for i in 0..2048 {
            assert!(
                (shifted_all[i].re - shifted_first[i].re).abs() < 0.001,
                "First half mismatch at {}",
                i
            );
        }
        for i in 0..2048 {
            assert!(
                (shifted_all[2048 + i].re - shifted_second[i].re).abs() < 0.001,
                "Second half mismatch at {}",
                i
            );
        }
    }

    #[test]
    fn test_frequency_shift_zero_shift() {
        let sample_rate = 2.048e6;
        let samples = generate_tone(100_000.0, 1.0, sample_rate, 1024);

        let mut phase = 0.0;
        let shifted = frequency_shift(&samples, 0.0, sample_rate, &mut phase);

        // Zero shift should leave samples unchanged
        for (orig, shifted) in samples.iter().zip(shifted.iter()) {
            assert!((orig.re - shifted.re).abs() < 0.001);
            assert!((orig.im - shifted.im).abs() < 0.001);
        }
    }

    // ========================================================================
    // 2. FIR filter tests
    // ========================================================================

    #[test]
    fn test_fir_dc_passthrough() {
        // DC signal (all real = 1.0) should pass through a low-pass filter unchanged
        let mut filter = FirFilter::low_pass(100_000.0, 2.048e6, 101);
        let dc_signal: Vec<IqSample> = vec![IqSample::new(1.0, 0.0); 500];
        let output = filter.process(&dc_signal);

        // After filter settles (skip transient = num_taps), output should be ~1.0
        for s in &output[110..] {
            assert!(
                (s.re - 1.0).abs() < 0.01,
                "DC should pass through LPF, got {}",
                s.re
            );
            assert!(
                s.im.abs() < 0.01,
                "Imaginary part should be ~0, got {}",
                s.im
            );
        }
    }

    #[test]
    fn test_fir_attenuates_high_frequency() {
        // A tone well above cutoff should be attenuated significantly
        let sample_rate = 2.048e6;
        let cutoff = 25_000.0;
        let mut filter = FirFilter::low_pass(cutoff, sample_rate, 101);

        // Generate tone at 3x cutoff (75 kHz) — should be heavily attenuated
        let high_freq_tone = generate_tone(cutoff * 3.0, 1.0, sample_rate, 1000);
        let output = filter.process(&high_freq_tone);

        // Measure power after filter settles
        let power_out: f64 = output[200..]
            .iter()
            .map(|s| (s.re * s.re + s.im * s.im) as f64)
            .sum::<f64>()
            / (output.len() - 200) as f64;

        let power_in: f64 = high_freq_tone[200..]
            .iter()
            .map(|s| (s.re * s.re + s.im * s.im) as f64)
            .sum::<f64>()
            / (high_freq_tone.len() - 200) as f64;

        let attenuation_db = 10.0 * (power_out / power_in).log10();

        assert!(
            attenuation_db < -40.0,
            "Expected >40dB attenuation at 3x cutoff, got {:.1} dB",
            attenuation_db
        );
    }

    #[test]
    fn test_fir_passes_in_band() {
        // A tone at half the cutoff should pass with minimal attenuation
        let sample_rate = 2.048e6;
        let cutoff = 100_000.0;
        let mut filter = FirFilter::low_pass(cutoff, sample_rate, 101);

        let in_band_tone = generate_tone(cutoff * 0.5, 1.0, sample_rate, 1000);
        let output = filter.process(&in_band_tone);

        // Measure power after settling
        let power_out: f64 = output[200..]
            .iter()
            .map(|s| (s.re * s.re + s.im * s.im) as f64)
            .sum::<f64>()
            / (output.len() - 200) as f64;

        let power_in: f64 = in_band_tone[200..]
            .iter()
            .map(|s| (s.re * s.re + s.im * s.im) as f64)
            .sum::<f64>()
            / (in_band_tone.len() - 200) as f64;

        let attenuation_db = 10.0 * (power_out / power_in).log10();

        assert!(
            attenuation_db > -3.0,
            "In-band signal should pass with <3dB loss, got {:.1} dB",
            attenuation_db
        );
    }

    #[test]
    fn test_fir_num_taps_odd() {
        // Even input should be made odd
        let filter = FirFilter::low_pass(100_000.0, 2.048e6, 100);
        assert_eq!(filter.num_taps() % 2, 1);
    }

    #[test]
    fn test_fir_output_length_equals_input() {
        let mut filter = FirFilter::low_pass(100_000.0, 2.048e6, 51);
        let input = vec![IqSample::new(1.0, 0.0); 256];
        let output = filter.process(&input);
        assert_eq!(output.len(), input.len());
    }

    // ========================================================================
    // 3. Decimation tests
    // ========================================================================

    #[test]
    fn test_decimate_output_length() {
        let samples: Vec<IqSample> = (0..100)
            .map(|i| IqSample::new(i as f32, 0.0))
            .collect();

        let result = decimate(&samples, 4);
        assert_eq!(result.len(), 25);
    }

    #[test]
    fn test_decimate_correct_samples() {
        let samples: Vec<IqSample> = (0..12)
            .map(|i| IqSample::new(i as f32, 0.0))
            .collect();

        let result = decimate(&samples, 3);
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].re, 0.0);
        assert_eq!(result[1].re, 3.0);
        assert_eq!(result[2].re, 6.0);
        assert_eq!(result[3].re, 9.0);
    }

    #[test]
    fn test_decimate_by_one_identity() {
        let samples: Vec<IqSample> = (0..10)
            .map(|i| IqSample::new(i as f32, i as f32 * 0.5))
            .collect();

        let result = decimate(&samples, 1);
        assert_eq!(result.len(), samples.len());
        for (a, b) in samples.iter().zip(result.iter()) {
            assert_eq!(a.re, b.re);
            assert_eq!(a.im, b.im);
        }
    }

    #[test]
    fn test_decimate_empty() {
        let samples: Vec<IqSample> = vec![];
        let result = decimate(&samples, 4);
        assert!(result.is_empty());
    }

    #[test]
    fn test_decimate_factor_zero() {
        let samples = vec![IqSample::new(1.0, 0.0); 10];
        let result = decimate(&samples, 0);
        assert!(result.is_empty());
    }

    // ========================================================================
    // 4. AM demod tests
    // ========================================================================

    #[test]
    fn test_am_demod_known_magnitude() {
        let samples = vec![
            IqSample::new(3.0, 4.0),  // magnitude = 5.0
            IqSample::new(1.0, 0.0),  // magnitude = 1.0
            IqSample::new(0.0, 0.0),  // magnitude = 0.0
            IqSample::new(0.6, 0.8),  // magnitude = 1.0
        ];
        let audio = am_demod(&samples);
        assert!((audio[0] - 5.0).abs() < 0.001);
        assert!((audio[1] - 1.0).abs() < 0.001);
        assert!((audio[2] - 0.0).abs() < 0.001);
        assert!((audio[3] - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_am_demod_zero_signal() {
        let samples = vec![IqSample::new(0.0, 0.0); 10];
        let audio = am_demod(&samples);
        for s in &audio {
            assert_eq!(*s, 0.0);
        }
    }

    #[test]
    fn test_am_demod_output_length() {
        let samples = vec![IqSample::new(1.0, 1.0); 42];
        let audio = am_demod(&samples);
        assert_eq!(audio.len(), 42);
    }

    // ========================================================================
    // 5. DC removal tests
    // ========================================================================

    #[test]
    fn test_dc_removal_removes_dc() {
        let mut dc_removal = DcRemoval::new(0.995);

        // Constant DC signal
        let dc_signal: Vec<f32> = vec![1.0; 5000];
        let output = dc_removal.process(&dc_signal);

        // After settling, output should converge to ~0
        let last_100_mean: f32 = output[4900..].iter().sum::<f32>() / 100.0;
        assert!(
            last_100_mean.abs() < 0.05,
            "DC should be removed, mean of last 100 samples = {}",
            last_100_mean
        );
    }

    #[test]
    fn test_dc_removal_preserves_ac() {
        let mut dc_removal = DcRemoval::new(0.995);
        let sample_rate = 48_000.0;
        let freq = 1000.0; // 1 kHz tone

        // Generate 1kHz sine wave with DC offset
        let signal: Vec<f32> = (0..48000)
            .map(|i| {
                let t = i as f64 / sample_rate;
                (5.0 + 0.5 * (2.0 * PI * freq * t).sin()) as f32 // DC=5, AC amplitude=0.5
            })
            .collect();

        let output = dc_removal.process(&signal);

        // After settling, measure AC power in the last portion
        let steady_start = 24000; // second half
        let ac_power: f64 = output[steady_start..]
            .iter()
            .map(|&s| (s as f64) * (s as f64))
            .sum::<f64>()
            / (output.len() - steady_start) as f64;

        // AC power should be roughly 0.5² / 2 = 0.125 (RMS of sine)
        let ac_rms = ac_power.sqrt();
        assert!(
            ac_rms > 0.2,
            "AC signal should be preserved, RMS = {:.4}",
            ac_rms
        );
    }

    #[test]
    fn test_dc_removal_zero_input() {
        let mut dc_removal = DcRemoval::new(0.995);
        let signal: Vec<f32> = vec![0.0; 100];
        let output = dc_removal.process(&signal);
        for s in &output {
            assert_eq!(*s, 0.0);
        }
    }

    // ========================================================================
    // 6. Synthetic AM signal tests
    // ========================================================================
    // (These are in test_signal.rs but we add cross-module tests here too)

    #[test]
    fn test_generated_am_signal_has_correct_offset() {
        // Generate AM signal at carrier=100.1MHz, center=100MHz → offset=100kHz
        let sample_rate = 2.048e6;
        let carrier = 100.1e6;
        let center = 100.0e6;
        let samples = generate_am_signal(carrier, 1000.0, 0.5, sample_rate, center, 4096);

        // Verify the signal is NOT at DC (has oscillation)
        let has_variation = samples.windows(2).any(|w| (w[1].re - w[0].re).abs() > 0.001);
        assert!(has_variation, "Signal at 100kHz offset should oscillate");
    }

    // ========================================================================
    // 7. End-to-end pipeline tests
    // ========================================================================

    #[test]
    fn test_am_demod_pipeline_produces_output() {
        let sample_rate = 2.048e6;
        let center_freq = 100.0e6;
        let cursor_freq = 100.1e6; // +100kHz offset
        let channel_bw = 25_000.0;
        let audio_rate = 48_000.0;

        let mut demod = Demodulator::new_am(
            center_freq,
            cursor_freq,
            channel_bw,
            sample_rate,
            audio_rate,
        );

        // Generate AM signal at cursor frequency
        let samples = generate_am_signal(cursor_freq, 1000.0, 0.8, sample_rate, center_freq, 8192);
        let audio = demod.process(&samples);

        assert!(!audio.is_empty(), "Pipeline should produce audio output");
    }

    #[test]
    fn test_am_demod_pipeline_output_sample_rate() {
        let sample_rate = 2.048e6;
        let audio_rate = 48_000.0;

        let demod = Demodulator::new_am(100.0e6, 100.1e6, 25_000.0, sample_rate, audio_rate);

        // Decimation factor should be ~42
        let factor = demod.decimation_factor();
        assert!(
            factor >= 40 && factor <= 44,
            "Decimation factor should be ~42, got {}",
            factor
        );

        // Actual output rate
        let actual = demod.actual_output_sample_rate();
        assert!(
            (actual - audio_rate).abs() < 3000.0,
            "Output sample rate should be ~48kHz, got {:.0}",
            actual
        );
    }

    #[test]
    fn test_am_demod_pipeline_output_length() {
        let sample_rate = 2.048e6;
        let mut demod = Demodulator::new_am(100.0e6, 100.1e6, 25_000.0, sample_rate, 48_000.0);
        let factor = demod.decimation_factor();

        let input_len = 4096;
        let samples = generate_am_signal(100.1e6, 1000.0, 0.5, sample_rate, 100.0e6, input_len);
        let audio = demod.process(&samples);

        // Output length may differ by ±1 due to step_by rounding
        let expected = input_len / factor;
        assert!(
            (audio.len() as i64 - expected as i64).abs() <= 1,
            "Output length {} should be ~{}",
            audio.len(),
            expected
        );
    }

    #[test]
    fn test_am_demod_pipeline_detects_1khz_tone() {
        // Generate AM signal: carrier at +100kHz, modulated with 1kHz audio, m=0.8
        let sample_rate = 2.048e6;
        let center_freq = 100.0e6;
        let cursor_freq = 100.1e6;
        let channel_bw = 25_000.0;
        let audio_rate = 48_000.0;

        let mut demod = Demodulator::new_am(
            center_freq,
            cursor_freq,
            channel_bw,
            sample_rate,
            audio_rate,
        );

        // Generate enough samples for multiple audio cycles
        // At 48kHz output, we need at least 48 samples per 1kHz cycle
        // Generate 100ms of data = 204800 input samples → ~4876 audio samples
        let num_samples = (sample_rate * 0.1) as usize;
        let samples = generate_am_signal(
            cursor_freq, 1000.0, 0.8, sample_rate, center_freq, num_samples,
        );

        let audio = demod.process(&samples);

        // Verify we have enough audio samples
        assert!(
            audio.len() > 200,
            "Need enough audio samples for analysis, got {}",
            audio.len()
        );

        // Skip initial transient (first 50% due to filter settling)
        let steady_audio = &audio[audio.len() / 2..];

        // Verify audio has AC content (not silence or pure DC)
        let mean: f64 = steady_audio.iter().map(|&s| s as f64).sum::<f64>()
            / steady_audio.len() as f64;
        let variance: f64 = steady_audio
            .iter()
            .map(|&s| {
                let d = s as f64 - mean;
                d * d
            })
            .sum::<f64>()
            / steady_audio.len() as f64;

        assert!(
            variance > 1e-6,
            "Audio should have AC content (variance = {:.2e})",
            variance
        );

        // Zero-crossing analysis to verify ~1kHz tone
        let actual_audio_rate = demod.actual_output_sample_rate();
        let mut zero_crossings = 0;
        for i in 1..steady_audio.len() {
            let prev = steady_audio[i - 1] - mean as f32;
            let curr = steady_audio[i] - mean as f32;
            if (prev < 0.0 && curr >= 0.0) || (prev >= 0.0 && curr < 0.0) {
                zero_crossings += 1;
            }
        }

        // For a 1kHz sine at the actual output sample rate:
        // Expected zero crossings = 2 * freq * duration
        let duration = steady_audio.len() as f64 / actual_audio_rate;
        let expected_crossings = 2.0 * 1000.0 * duration;

        let ratio = zero_crossings as f64 / expected_crossings;
        assert!(
            ratio > 0.5 && ratio < 2.0,
            "Zero crossings suggest wrong frequency: {} crossings, expected ~{:.0} (ratio={:.2})",
            zero_crossings,
            expected_crossings,
            ratio
        );
    }

    #[test]
    fn test_am_demod_retune() {
        let sample_rate = 2.048e6;
        let center_freq = 100.0e6;

        let mut demod = Demodulator::new_am(
            center_freq,
            100.1e6,
            25_000.0,
            sample_rate,
            48_000.0,
        );

        // Retune to a different frequency
        demod.set_cursor_freq(100.2e6);

        // Should still produce output
        let samples = generate_am_signal(100.2e6, 1000.0, 0.8, sample_rate, center_freq, 4096);
        let audio = demod.process(&samples);
        assert!(!audio.is_empty());
    }

    #[test]
    fn test_am_demod_change_channel_bw() {
        let sample_rate = 2.048e6;
        let center_freq = 100.0e6;

        let mut demod = Demodulator::new_am(
            center_freq,
            100.1e6,
            25_000.0,
            sample_rate,
            48_000.0,
        );

        // Change channel bandwidth
        demod.set_channel_bw(50_000.0);

        // Should still produce output
        let samples = generate_am_signal(100.1e6, 1000.0, 0.8, sample_rate, center_freq, 4096);
        let audio = demod.process(&samples);
        assert!(!audio.is_empty());
    }

    #[test]
    fn test_am_demod_no_signal_produces_near_silence() {
        let sample_rate = 2.048e6;
        let center_freq = 100.0e6;
        let cursor_freq = 100.5e6; // cursor at +500kHz

        let mut demod = Demodulator::new_am(
            center_freq,
            cursor_freq,
            25_000.0,
            sample_rate,
            48_000.0,
        );

        // Signal is at +100kHz, cursor is at +500kHz → nothing in channel
        let samples = generate_am_signal(100.1e6, 1000.0, 0.8, sample_rate, center_freq, 8192);
        let audio = demod.process(&samples);

        // Audio should be very quiet (signal is outside the channel)
        // Note: AGC will amplify noise to some degree, so threshold is relaxed
        let power: f64 = audio.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>()
            / audio.len() as f64;

        assert!(
            power < 0.5,
            "Audio should be near-silent when signal is outside channel, power = {:.6}",
            power
        );
    }

    // ========================================================================
    // 8. DemodMode tests
    // ========================================================================

    #[test]
    fn test_demod_mode_default_channel_bw() {
        assert!((DemodMode::AM.default_channel_bw() - 8_000.0).abs() < 0.1);
        assert!((DemodMode::FM.default_channel_bw() - 200_000.0).abs() < 0.1);
        assert!((DemodMode::NFM.default_channel_bw() - 25_000.0).abs() < 0.1);
    }

    #[test]
    fn test_demod_mode_labels() {
        assert_eq!(DemodMode::AM.label(), "AM");
        assert_eq!(DemodMode::FM.label(), "FM");
        assert_eq!(DemodMode::NFM.label(), "NFM");
    }

    #[test]
    fn test_demod_mode_cycle() {
        assert_eq!(DemodMode::AM.next(), DemodMode::FM);
        assert_eq!(DemodMode::FM.next(), DemodMode::NFM);
        assert_eq!(DemodMode::NFM.next(), DemodMode::AM);
    }

    // ========================================================================
    // 9. FM discriminator tests
    // ========================================================================

    #[test]
    fn test_fm_demod_pure_carrier_near_zero() {
        // Pure carrier (no modulation) at DC → output near 0
        let samples: Vec<IqSample> = vec![IqSample::new(1.0, 0.0); 100];
        let output = fm_demod(&samples);
        assert_eq!(output.len(), 100);
        for &s in &output[1..] {
            assert!(s.abs() < 0.001, "Pure carrier should produce near-zero FM output, got {}", s);
        }
    }

    #[test]
    fn test_fm_demod_single_sample() {
        let samples = vec![IqSample::new(1.0, 0.0)];
        let output = fm_demod(&samples);
        assert_eq!(output, vec![0.0]);
    }

    #[test]
    fn test_fm_demod_empty() {
        let samples: Vec<IqSample> = vec![];
        let output = fm_demod(&samples);
        assert!(output.is_empty());
    }

    #[test]
    fn test_fm_demod_rotating_carrier() {
        // A tone at a known frequency should produce a constant discriminator output
        let sample_rate = 48_000.0;
        let freq = 1000.0; // 1 kHz
        let samples = generate_tone(freq, 1.0, sample_rate, 1000);
        let output = fm_demod(&samples);
        // Expected phase increment per sample: 2π * freq / sample_rate
        let expected = 2.0 * std::f64::consts::PI * freq / sample_rate;
        for &s in &output[1..] {
            assert!(
                (s - expected as f32).abs() < 0.01,
                "Expected ~{:.4}, got {:.4}",
                expected,
                s
            );
        }
    }

    // ========================================================================
    // 10. De-emphasis tests
    // ========================================================================

    #[test]
    fn test_de_emphasis_dc_passthrough() {
        // DC should pass through (attenuated by the filter gain at DC)
        let mut de_emph = DeEmphasis::new(48_000.0, 50e-6);
        let dc: Vec<f32> = vec![1.0; 10000];
        let output = de_emph.process(&dc);
        // After settling, output should converge to 1.0
        let last = output.last().unwrap();
        assert!(
            (last - 1.0).abs() < 0.01,
            "DC should pass through de-emphasis, got {}",
            last
        );
    }

    #[test]
    fn test_de_emphasis_attenuates_high_freq() {
        let sample_rate = 48_000.0;
        let mut de_emph = DeEmphasis::new(sample_rate, 50e-6);
        // Generate high-frequency signal (10 kHz)
        let signal: Vec<f32> = (0..10000)
            .map(|i| (2.0 * std::f64::consts::PI * 10_000.0 * i as f64 / sample_rate).sin() as f32)
            .collect();
        let output = de_emph.process(&signal);
        // Measure output power in steady state
        let input_power: f64 = signal[5000..].iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / 5000.0;
        let output_power: f64 = output[5000..].iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / 5000.0;
        let attenuation_db = 10.0 * (output_power / input_power).log10();
        assert!(
            attenuation_db < -5.0,
            "High frequency should be attenuated, got {:.1} dB",
            attenuation_db
        );
    }

    #[test]
    fn test_de_emphasis_coefficients() {
        // alpha = exp(-1 / (sample_rate * tau))
        let sr = 48_000.0;
        let tau = 50e-6;
        let expected_alpha = (-1.0_f64 / (sr * tau)).exp() as f32;
        let de_emph = DeEmphasis::new(sr, tau);
        assert!(
            (de_emph.alpha() - expected_alpha).abs() < 1e-6,
            "Alpha mismatch: expected {}, got {}",
            expected_alpha,
            de_emph.alpha()
        );
    }

    // ========================================================================
    // 11. FM pipeline tests
    // ========================================================================

    #[test]
    fn test_fm_demod_pipeline_produces_output() {
        let sample_rate = 2.048e6;
        let center_freq = 100.0e6;
        let cursor_freq = 100.1e6;
        let channel_bw = 200_000.0;
        let audio_rate = 48_000.0;

        let mut demod = Demodulator::new(
            center_freq,
            cursor_freq,
            channel_bw,
            sample_rate,
            audio_rate,
            DemodMode::FM,
        );

        let samples = generate_fm_signal(
            cursor_freq, 1000.0, 75_000.0, sample_rate, center_freq, 8192,
        );
        let audio = demod.process(&samples);
        assert!(!audio.is_empty(), "FM pipeline should produce audio output");
    }

    #[test]
    fn test_nfm_demod_pipeline_produces_output() {
        let sample_rate = 2.048e6;
        let center_freq = 100.0e6;
        let cursor_freq = 100.1e6;
        let channel_bw = 25_000.0;
        let audio_rate = 48_000.0;

        let mut demod = Demodulator::new(
            center_freq,
            cursor_freq,
            channel_bw,
            sample_rate,
            audio_rate,
            DemodMode::NFM,
        );

        let samples = generate_fm_signal(
            cursor_freq, 1000.0, 5_000.0, sample_rate, center_freq, 8192,
        );
        let audio = demod.process(&samples);
        assert!(!audio.is_empty(), "NFM pipeline should produce audio output");
    }

    #[test]
    fn test_mode_switch_am_to_fm() {
        let sample_rate = 2.048e6;
        let center_freq = 100.0e6;
        let cursor_freq = 100.1e6;
        let audio_rate = 48_000.0;

        let mut demod = Demodulator::new(
            center_freq, cursor_freq, 25_000.0, sample_rate, audio_rate, DemodMode::AM,
        );
        assert_eq!(demod.mode(), DemodMode::AM);

        // Process AM
        let am_samples = generate_am_signal(cursor_freq, 1000.0, 0.8, sample_rate, center_freq, 4096);
        let am_audio = demod.process(&am_samples);
        assert!(!am_audio.is_empty());

        // Switch to FM
        demod.set_mode(DemodMode::FM);
        assert_eq!(demod.mode(), DemodMode::FM);
        demod.set_channel_bw(200_000.0);

        let fm_samples = generate_fm_signal(cursor_freq, 1000.0, 75_000.0, sample_rate, center_freq, 4096);
        let fm_audio = demod.process(&fm_samples);
        assert!(!fm_audio.is_empty());
    }

    #[test]
    fn test_fm_demod_detects_tone() {
        // Generate FM signal with 1kHz tone, demodulate, verify tone is present
        let sample_rate = 2.048e6;
        let center_freq = 100.0e6;
        let cursor_freq = 100.1e6;
        let channel_bw = 25_000.0;
        let audio_rate = 48_000.0;

        let mut demod = Demodulator::new(
            center_freq, cursor_freq, channel_bw, sample_rate, audio_rate, DemodMode::NFM,
        );

        // Generate 100ms of data with low deviation (NFM-like)
        let num_samples = (sample_rate * 0.1) as usize;
        let samples = generate_fm_signal(
            cursor_freq, 1000.0, 5_000.0, sample_rate, center_freq, num_samples,
        );
        let audio = demod.process(&samples);

        assert!(audio.len() > 200);

        // Skip transient
        let steady_audio = &audio[audio.len() / 2..];

        // Verify AC content exists
        let mean: f64 = steady_audio.iter().map(|&s| s as f64).sum::<f64>() / steady_audio.len() as f64;
        let variance: f64 = steady_audio.iter().map(|&s| { let d = s as f64 - mean; d * d }).sum::<f64>() / steady_audio.len() as f64;
        assert!(variance > 1e-6, "FM demod audio should have AC content (variance = {:.2e})", variance);

        // Zero-crossing analysis
        let actual_audio_rate = demod.actual_output_sample_rate();
        let mut zero_crossings = 0;
        for i in 1..steady_audio.len() {
            let prev = steady_audio[i - 1] - mean as f32;
            let curr = steady_audio[i] - mean as f32;
            if (prev < 0.0 && curr >= 0.0) || (prev >= 0.0 && curr < 0.0) {
                zero_crossings += 1;
            }
        }
        let duration = steady_audio.len() as f64 / actual_audio_rate;
        let expected_crossings = 2.0 * 1000.0 * duration;
        let ratio = zero_crossings as f64 / expected_crossings;
        assert!(
            ratio > 0.5 && ratio < 2.0,
            "FM zero crossings suggest wrong freq: {} crossings, expected ~{:.0} (ratio={:.2})",
            zero_crossings, expected_crossings, ratio
        );
    }

    // ========================================================================
    // AGC Tests
    // ========================================================================

    #[test]
    fn agc_normalizes_constant_amplitude() {
        let sample_rate = 48000.0;
        // Use fast decay so AGC converges within 1 second
        let mut agc = super::Agc::new(sample_rate, 10.0, 100.0, 0.3, 50.0);

        // Process 2 seconds to let AGC fully converge
        let input: Vec<f32> = (0..96000)
            .map(|i| 0.1 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sample_rate as f32).sin())
            .collect();

        let output = agc.process(&input);

        let tail = &output[output.len()-1000..];
        let rms: f32 = (tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt();
        assert!(
            (rms - 0.3).abs() < 0.08,
            "AGC output RMS should be near target 0.3, got {rms}"
        );
    }

    #[test]
    fn agc_limits_gain_to_max() {
        let sample_rate = 48000.0;
        let mut agc = super::Agc::new(sample_rate, 10.0, 300.0, 0.3, 20.0);

        let input: Vec<f32> = (0..48000)
            .map(|i| 0.001 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sample_rate as f32).sin())
            .collect();

        let output = agc.process(&input);

        let max_out = output.iter().map(|s| s.abs()).fold(0.0_f32, f32::max);
        assert!(
            max_out <= 0.001 * 10.0 + 0.001,
            "AGC should limit gain to max_gain_db, max output was {max_out}"
        );
    }

    #[test]
    fn agc_attack_reduces_gain_quickly() {
        let sample_rate = 48000.0;
        let mut agc = super::Agc::new(sample_rate, 5.0, 300.0, 0.3, 50.0);

        let quiet: Vec<f32> = (0..24000)
            .map(|i| 0.05 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sample_rate as f32).sin())
            .collect();
        agc.process(&quiet);
        let gain_before = agc.current_gain_db();

        let loud: Vec<f32> = (0..4800)
            .map(|i| 0.8 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sample_rate as f32).sin())
            .collect();
        agc.process(&loud);
        let gain_after = agc.current_gain_db();

        assert!(
            gain_after < gain_before,
            "AGC gain should decrease on loud signal: before={gain_before} dB, after={gain_after} dB"
        );
    }

    #[test]
    fn agc_decay_increases_gain_slowly() {
        let sample_rate = 48000.0;
        let mut agc = super::Agc::new(sample_rate, 10.0, 300.0, 0.3, 50.0);

        let loud: Vec<f32> = (0..48000)
            .map(|i| 0.8 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sample_rate as f32).sin())
            .collect();
        agc.process(&loud);
        let gain_after_loud = agc.current_gain_db();

        let quiet: Vec<f32> = (0..48000)
            .map(|i| 0.05 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sample_rate as f32).sin())
            .collect();
        agc.process(&quiet);
        let gain_after_quiet = agc.current_gain_db();

        assert!(
            gain_after_quiet > gain_after_loud,
            "AGC gain should increase on quiet signal: after_loud={gain_after_loud} dB, after_quiet={gain_after_quiet} dB"
        );
    }

    #[test]
    fn agc_disabled_passes_through() {
        let mut agc = super::Agc::new(48000.0, 10.0, 300.0, 0.3, 50.0);
        agc.set_enabled(false);
        assert!(!agc.enabled());

        let input: Vec<f32> = vec![0.1, 0.2, 0.3, -0.1, -0.2];
        let output = agc.process(&input);
        assert_eq!(input, output);
    }

    #[test]
    fn agc_does_not_distort_sine() {
        let sample_rate = 48000.0;
        let freq = 1000.0;
        let mut agc = super::Agc::new(sample_rate, 10.0, 300.0, 0.3, 50.0);

        let input: Vec<f32> = (0..48000)
            .map(|i| 0.1 * (2.0 * std::f32::consts::PI * freq as f32 * i as f32 / sample_rate as f32).sin())
            .collect();

        let output = agc.process(&input);

        let tail = &output[24000..];
        let mut crossings = 0;
        for i in 1..tail.len() {
            if (tail[i] >= 0.0) != (tail[i-1] >= 0.0) {
                crossings += 1;
            }
        }
        let expected = (2.0 * freq * 0.5) as usize;
        let ratio = crossings as f64 / expected as f64;
        assert!(
            ratio > 0.95 && ratio < 1.05,
            "AGC should preserve zero crossings: got {crossings}, expected ~{expected}"
        );
    }

    // ========================================================================
    // AudioBandpass tests
    // ========================================================================

    /// Generate a sine wave at the given frequency.
    fn generate_sine(freq: f64, sample_rate: f64, num_samples: usize, amplitude: f32) -> Vec<f32> {
        (0..num_samples)
            .map(|i| amplitude * (2.0 * PI * freq * i as f64 / sample_rate).sin() as f32)
            .collect()
    }

    /// Measure RMS of a signal.
    fn rms(signal: &[f32]) -> f32 {
        let sum_sq: f32 = signal.iter().map(|s| s * s).sum();
        (sum_sq / signal.len() as f32).sqrt()
    }

    #[test]
    fn bandpass_passes_1khz_tone() {
        let sr = 48000.0;
        let mut bp = AudioBandpass::new(300.0, 3400.0, sr, 101);
        let input = generate_sine(1000.0, sr, 4800, 1.0);
        let output = bp.process(&input);
        let steady = &output[200..];
        let input_rms = rms(&input[200..]);
        let output_rms = rms(steady);
        let ratio = output_rms / input_rms;
        assert!(
            ratio > 0.7,
            "1kHz tone should pass through bandpass mostly unchanged, got ratio {ratio}"
        );
    }

    #[test]
    fn bandpass_attenuates_100hz_tone() {
        let sr = 48000.0;
        let mut bp = AudioBandpass::new(300.0, 3400.0, sr, 101);
        let input = generate_sine(100.0, sr, 4800, 1.0);
        let output = bp.process(&input);
        let steady = &output[200..];
        let input_rms = rms(&input[200..]);
        let output_rms = rms(steady);
        let ratio = output_rms / input_rms;
        assert!(
            ratio < 0.6,
            "100Hz tone should be attenuated by bandpass, got ratio {ratio}"
        );
    }

    #[test]
    fn bandpass_attenuates_5khz_tone() {
        let sr = 48000.0;
        let mut bp = AudioBandpass::new(300.0, 3400.0, sr, 101);
        let input = generate_sine(5000.0, sr, 4800, 1.0);
        let output = bp.process(&input);
        let steady = &output[200..];
        let input_rms = rms(&input[200..]);
        let output_rms = rms(steady);
        let ratio = output_rms / input_rms;
        assert!(
            ratio < 0.6,
            "5kHz tone should be heavily attenuated by bandpass, got ratio {ratio}"
        );
    }

    #[test]
    fn bandpass_disabled_is_passthrough() {
        let sr = 48000.0;
        let mut bp = AudioBandpass::new(300.0, 3400.0, sr, 101);
        bp.set_enabled(false);
        let input = generate_sine(100.0, sr, 480, 1.0);
        let output = bp.process(&input);
        assert_eq!(input.len(), output.len());
        for (a, b) in input.iter().zip(output.iter()) {
            assert!((a - b).abs() < 1e-10, "Disabled bandpass should be exact passthrough");
        }
    }

    #[test]
    fn bandpass_enabled_toggle() {
        let sr = 48000.0;
        let mut bp = AudioBandpass::new(300.0, 3400.0, sr, 101);
        assert!(bp.enabled());
        bp.set_enabled(false);
        assert!(!bp.enabled());
        bp.set_enabled(true);
        assert!(bp.enabled());
    }

    #[test]
    fn bandpass_set_low_cut_rebuilds_taps() {
        let sr = 48000.0;
        let mut bp = AudioBandpass::new(300.0, 3400.0, sr, 101);
        let input = generate_sine(200.0, sr, 4800, 1.0);
        let output1 = bp.process(&input);
        let rms1 = rms(&output1[200..]);

        bp.set_low_cut(100.0);
        let output2 = bp.process(&input);
        let rms2 = rms(&output2[200..]);

        assert!(
            rms2 > rms1 * 1.5,
            "Lowering low_cut should let 200Hz pass better: rms1={rms1}, rms2={rms2}"
        );
    }

    #[test]
    fn bandpass_set_high_cut_rebuilds_taps() {
        let sr = 48000.0;
        let mut bp = AudioBandpass::new(300.0, 3400.0, sr, 101);
        let input = generate_sine(4000.0, sr, 4800, 1.0);
        let output1 = bp.process(&input);
        let rms1 = rms(&output1[200..]);

        bp.set_high_cut(5000.0);
        let output2 = bp.process(&input);
        let rms2 = rms(&output2[200..]);

        assert!(
            rms2 > rms1 * 1.5,
            "Raising high_cut should let 4kHz pass better: rms1={rms1}, rms2={rms2}"
        );
    }

    #[test]
    fn bandpass_kernel_shape() {
        let sr = 48000.0;
        let mut bp = AudioBandpass::new(300.0, 3400.0, sr, 101);

        let test_freqs = [
            (50.0, false),
            (100.0, false),
            (1000.0, true),
            (2000.0, true),
            (3000.0, true),
            (5000.0, false),
            (8000.0, false),
        ];

        for (freq, should_pass) in test_freqs {
            let input = generate_sine(freq, sr, 4800, 1.0);
            let output = bp.process(&input);
            bp = AudioBandpass::new(300.0, 3400.0, sr, 101);
            let ratio = rms(&output[200..]) / rms(&input[200..]);
            if should_pass {
                assert!(
                    ratio > 0.5,
                    "Freq {freq}Hz should be in passband, got ratio {ratio}"
                );
            } else {
                assert!(
                    ratio < 0.6,
                    "Freq {freq}Hz should be in stopband, got ratio {ratio}"
                );
            }
        }
    }
}
