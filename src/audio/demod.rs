//! Demodulation pipeline: frequency shift → LPF → decimate → demod (AM/FM/NFM) → DC removal.

use serde::{Serialize, Deserialize};

use crate::types::IqSample;
use std::f64::consts::PI;

// ============================================================================
// Demodulation Mode
// ============================================================================

/// Demodulation mode for a VFO or cursor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
    pub fn default_channel_bw(&self) -> f64 {
        match self {
            DemodMode::AM => 25_000.0,
            DemodMode::FM => 200_000.0,
            DemodMode::NFM => 25_000.0,
        }
    }

    /// Short label for display.
    pub fn label(&self) -> &'static str {
        match self {
            DemodMode::AM => "AM",
            DemodMode::FM => "FM",
            DemodMode::NFM => "NFM",
        }
    }

    /// Cycle to the next mode: AM → FM → NFM → AM.
    pub fn next(&self) -> DemodMode {
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
        let cos_val = phase.cos() as f32;
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
    pub fn low_pass(cutoff_hz: f64, sample_rate: f64, num_taps: usize) -> Self {
        // Ensure odd number of taps
        let num_taps = if num_taps % 2 == 0 { num_taps + 1 } else { num_taps };
        let m = (num_taps - 1) as f64;
        let fc = cutoff_hz / sample_rate; // Normalized cutoff frequency

        let mut taps = Vec::with_capacity(num_taps);

        for i in 0..num_taps {
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
    pub fn new(sample_rate: f64, tau: f64) -> Self {
        let alpha = (-1.0 / (sample_rate * tau)).exp() as f32;
        Self {
            alpha,
            prev_output: 0.0,
        }
    }

    /// Get the alpha coefficient (for testing).
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
    pub fn new(alpha: f64) -> Self {
        Self {
            alpha,
            prev_input: 0.0,
            prev_output: 0.0,
        }
    }

    /// Process audio samples, removing the DC offset.
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(samples.len());

        for &sample in samples {
            let x = sample as f64;
            let y = x - self.prev_input + self.alpha * self.prev_output;
            self.prev_input = x;
            self.prev_output = y;
            output.push(y as f32);
        }

        output
    }
}

// ============================================================================
// 1.6 Full Demodulation Pipeline
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
    prev_sample: IqSample,
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
    pub fn new(
        center_freq: f64,
        cursor_freq: f64,
        channel_bw: f64,
        input_sample_rate: f64,
        output_sample_rate: f64,
        mode: DemodMode,
    ) -> Self {
        let decimation_factor = (input_sample_rate / output_sample_rate).round() as usize;
        let decimation_factor = decimation_factor.max(1);

        // FIR filter: cutoff at half the channel bandwidth
        // Use 101 taps — good tradeoff between rejection and latency
        let num_taps = 101;
        let fir_filter = FirFilter::low_pass(channel_bw / 2.0, input_sample_rate, num_taps);

        let de_emphasis = match mode {
            DemodMode::FM => {
                let output_sr = input_sample_rate / decimation_factor as f64;
                Some(DeEmphasis::new(output_sr, 50e-6))
            }
            _ => None,
        };

        Self {
            fir_filter,
            dc_removal: DcRemoval::new(0.995),
            shift_phase: 0.0,
            decimation_factor,
            cursor_freq,
            center_freq,
            input_sample_rate,
            mode,
            de_emphasis,
            prev_sample: IqSample::new(0.0, 0.0),
        }
    }

    /// Create a new AM demodulator (convenience constructor for backward compatibility).
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

        // 6. DC removal
        self.dc_removal.process(&audio)
    }

    /// Update the cursor (tuning) frequency. Recalculates the shift.
    pub fn set_cursor_freq(&mut self, freq: f64) {
        self.cursor_freq = freq;
        // Phase continues naturally — no need to reset
    }

    /// Update the channel bandwidth. Recalculates the FIR filter.
    pub fn set_channel_bw(&mut self, bw: f64) {
        self.fir_filter = FirFilter::low_pass(bw / 2.0, self.input_sample_rate, 101);
    }

    /// Set the demodulation mode. Reconfigures de-emphasis and resets FM state.
    pub fn set_mode(&mut self, mode: DemodMode) {
        self.mode = mode;
        self.de_emphasis = match mode {
            DemodMode::FM => {
                let output_sr = self.input_sample_rate / self.decimation_factor as f64;
                Some(DeEmphasis::new(output_sr, 50e-6))
            }
            _ => None,
        };
        self.prev_sample = IqSample::new(0.0, 0.0);
    }

    /// Get the current demodulation mode.
    pub fn mode(&self) -> DemodMode {
        self.mode
    }

    /// Get the current decimation factor.
    pub fn decimation_factor(&self) -> usize {
        self.decimation_factor
    }

    /// Get the actual output sample rate after decimation.
    pub fn actual_output_sample_rate(&self) -> f64 {
        self.input_sample_rate / self.decimation_factor as f64
    }
}

/// Type alias for backward compatibility.
pub type AmDemodulator = Demodulator;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::test_signal::{generate_am_signal, generate_fm_signal, generate_tone};

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
        let power: f64 = audio.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>()
            / audio.len() as f64;

        assert!(
            power < 0.01,
            "Audio should be near-silent when signal is outside channel, power = {:.6}",
            power
        );
    }

    // ========================================================================
    // 8. DemodMode tests
    // ========================================================================

    #[test]
    fn test_demod_mode_default_channel_bw() {
        assert!((DemodMode::AM.default_channel_bw() - 25_000.0).abs() < 0.1);
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
}
