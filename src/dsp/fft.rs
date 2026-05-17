//! FFT processing, windowing functions, and power-to-dB conversion.

use std::f32::consts::PI;

use rustfft::{FftPlanner, num_complex::Complex};

use crate::types::{FftResult, IqSample};

/// FFT processor with windowing and dB conversion.
pub struct FftProcessor {
    fft_size: usize,
    window: Vec<f32>,
    planner: FftPlanner<f32>,
    /// Optional averaging: number of frames to average.
    avg_count: usize,
    /// Accumulator for averaging.
    avg_accum: Vec<f64>,
    /// Number of frames accumulated so far.
    frames_accumulated: usize,
}

impl FftProcessor {
    /// Create a new FFT processor.
    ///
    /// # Arguments
    /// * `fft_size` - Number of FFT bins (must be power of 2 for efficiency)
    /// * `avg_count` - Number of frames to average (1 = no averaging)
    #[must_use] 
    pub fn new(fft_size: usize, avg_count: usize) -> Self {
        let window = hanning_window(fft_size);
        let avg_count = avg_count.max(1);
        Self {
            fft_size,
            window,
            planner: FftPlanner::new(),
            avg_count,
            avg_accum: vec![0.0; fft_size],
            frames_accumulated: 0,
        }
    }

    /// Update the averaging count at runtime.
    pub fn set_avg_count(&mut self, count: usize) {
        let count = count.max(1);
        if count != self.avg_count {
            self.avg_count = count;
            self.frames_accumulated = 0;
            self.avg_accum = vec![0.0; self.fft_size];
        }
    }

    /// Process a buffer of IQ samples through windowing + FFT + magnitude.
    ///
    /// Returns `Some(FftResult)` when enough frames have been accumulated
    /// for averaging, or `None` if still accumulating.
    ///
    /// # Panics
    /// Panics if the FFT planner returns an invalid plan.
    #[allow(clippy::cast_possible_truncation)] // intentional truncation
    pub fn process(
        &mut self,
        samples: &[IqSample],
        center_freq: f64,
        sample_rate: f64,
    ) -> Option<FftResult> {
        assert!(
            samples.len() >= self.fft_size,
            "Need at least {} samples, got {}",
            self.fft_size,
            samples.len()
        );

        // Apply window function
        let mut windowed: Vec<Complex<f32>> = samples[..self.fft_size]
            .iter()
            .zip(self.window.iter())
            .map(|(s, w)| Complex::new(s.re * w, s.im * w))
            .collect();

        // Compute FFT
        let fft = self.planner.plan_fft_forward(self.fft_size);
        fft.process(&mut windowed);

        // Calculate magnitude squared (power) and accumulate
        for (i, bin) in windowed.iter().enumerate() {
            let power = f64::from(bin.re * bin.re + bin.im * bin.im);
            self.avg_accum[i] += power;
        }
        self.frames_accumulated += 1;

        if self.frames_accumulated >= self.avg_count {
            // Compute averaged dB values with FFT shift (DC in center)
            let n = self.fft_size;
            #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
            let avg_factor = self.frames_accumulated as f64;
            let mut bins = vec![0.0_f32; n];

            for (i, bin) in bins.iter_mut().enumerate().take(n) {
                // FFT shift: swap first half and second half so DC is centered
                let src = (i + n / 2) % n;
                let avg_power = self.avg_accum[src] / avg_factor;
                *bin = power_to_db(avg_power as f32, n);
            }

            // Reset accumulator
            self.avg_accum.iter_mut().for_each(|v| *v = 0.0);
            self.frames_accumulated = 0;

            Some(FftResult {
                bins,
                center_freq,
                sample_rate,
            })
        } else {
            None
        }
    }
}

/// Generate a Hanning window of the given size.
#[must_use] 
#[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
pub(crate) fn hanning_window(size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / size as f32).cos()))
        .collect()
}

/// Convert power (magnitude squared) to dB, normalized by FFT size.
///
/// Formula: 10 * log10(power / N^2), where N is FFT size.
/// The N^2 normalization accounts for FFT scaling.
#[must_use] 
pub(crate) fn power_to_db(power: f32, fft_size: usize) -> f32 {
    #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
    let normalized = power / (fft_size as f32 * fft_size as f32);
    if normalized > 1e-20 {
        10.0 * normalized.log10()
    } else {
        -200.0 // Floor for very small values
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::IqSample;

    #[test]
    fn test_hanning_window_shape() {
        let win = hanning_window(256);
        assert_eq!(win.len(), 256);

        // Hanning window starts and ends near zero
        assert!(win[0].abs() < 1e-6, "Window start should be ~0, got {}", win[0]);

        // Middle should be close to 1.0
        let mid = win[128];
        assert!(
            (mid - 1.0).abs() < 0.01,
            "Window middle should be ~1.0, got {}",
            mid
        );

        // Periodic Hanning window: w[i] = w[N-i] for i in 1..N
        for i in 1..128 {
            assert!(
                (win[i] - win[256 - i]).abs() < 1e-6,
                "Window not symmetric at index {}: {} vs {}",
                i,
                win[i],
                win[256 - i]
            );
        }
    }

    #[test]
    fn test_hanning_window_values_positive() {
        let win = hanning_window(1024);
        for (i, &val) in win.iter().enumerate() {
            assert!(val >= 0.0, "Negative window value at index {}: {}", i, val);
            assert!(val <= 1.0, "Window value > 1 at index {}: {}", i, val);
        }
    }

    #[test]
    fn test_power_to_db_known_values() {
        // Full-scale signal: power = N^2, should give 0 dB
        let db = power_to_db(1024.0 * 1024.0, 1024);
        assert!(
            db.abs() < 0.01,
            "Full-scale should be ~0 dB, got {}",
            db
        );

        // Half power should be -3 dB relative
        let db_half = power_to_db(1024.0 * 1024.0 / 2.0, 1024);
        assert!(
            (db_half - (-3.01)).abs() < 0.1,
            "Half power should be ~-3 dB, got {}",
            db_half
        );
    }

    #[test]
    fn test_power_to_db_floor() {
        let db = power_to_db(0.0, 1024);
        assert_eq!(db, -200.0);
    }

    #[test]
    fn test_pure_tone_peak_at_correct_bin() {
        // Generate a pure tone at exactly bin_width * 100 Hz offset
        let fft_size = 2048;
        let sample_rate = 2_048_000.0;
        let bin_width = sample_rate / fft_size as f64;
        let target_bin_offset = 100; // 100 bins above DC
        let freq_offset = bin_width * target_bin_offset as f64;

        // Generate IQ samples for this tone
        let mut samples = vec![IqSample::new(0.0, 0.0); fft_size];
        let dt = 1.0 / sample_rate;
        for i in 0..fft_size {
            let t = i as f64 * dt;
            let phase = 2.0 * std::f64::consts::PI * freq_offset * t;
            samples[i] = IqSample::new(phase.cos() as f32, phase.sin() as f32);
        }

        let mut processor = FftProcessor::new(fft_size, 1);
        let result = processor
            .process(&samples, 100e6, sample_rate)
            .expect("Should produce result with avg_count=1");

        // After FFT shift, DC is at bin fft_size/2.
        // A positive frequency offset should appear at bin fft_size/2 + target_bin_offset.
        let expected_bin = fft_size / 2 + target_bin_offset;
        let (peak_bin, _peak_db) = result.peak();

        assert!(
            (peak_bin as i32 - expected_bin as i32).unsigned_abs() <= 2,
            "Peak at bin {} but expected near bin {} (freq_offset={}Hz, bin_width={}Hz)",
            peak_bin,
            expected_bin,
            freq_offset,
            bin_width
        );
    }

    #[test]
    fn test_negative_frequency_offset_peak() {
        let fft_size = 2048;
        let sample_rate = 2_048_000.0;
        let bin_width = sample_rate / fft_size as f64;
        let target_bin_offset: i32 = -50;
        let freq_offset = bin_width * target_bin_offset as f64;

        let mut samples = vec![IqSample::new(0.0, 0.0); fft_size];
        let dt = 1.0 / sample_rate;
        for i in 0..fft_size {
            let t = i as f64 * dt;
            let phase = 2.0 * std::f64::consts::PI * freq_offset * t;
            samples[i] = IqSample::new(phase.cos() as f32, phase.sin() as f32);
        }

        let mut processor = FftProcessor::new(fft_size, 1);
        let result = processor
            .process(&samples, 100e6, sample_rate)
            .expect("Should produce result");

        let expected_bin = (fft_size as i32 / 2 + target_bin_offset) as usize;
        let (peak_bin, _) = result.peak();

        assert!(
            (peak_bin as i32 - expected_bin as i32).unsigned_abs() <= 2,
            "Negative offset: peak at bin {} but expected near bin {}",
            peak_bin,
            expected_bin
        );
    }

    #[test]
    fn test_noise_floor_relatively_flat() {
        // Generate white noise and check that FFT output is roughly flat
        let fft_size = 2048;
        let sample_rate = 2_048_000.0;

        // Use a simple PRNG for noise
        let mut rng_state: u64 = 42;
        let mut samples = vec![IqSample::new(0.0, 0.0); fft_size];
        for sample in samples.iter_mut() {
            rng_state ^= rng_state << 13;
            rng_state ^= rng_state >> 7;
            rng_state ^= rng_state << 17;
            let i = (rng_state as i64 as f64 / i64::MAX as f64) as f32 * 0.1;
            rng_state ^= rng_state << 13;
            rng_state ^= rng_state >> 7;
            rng_state ^= rng_state << 17;
            let q = (rng_state as i64 as f64 / i64::MAX as f64) as f32 * 0.1;
            *sample = IqSample::new(i, q);
        }

        let mut processor = FftProcessor::new(fft_size, 1);
        let result = processor
            .process(&samples, 100e6, sample_rate)
            .expect("Should produce result");

        // Noise floor: check that the standard deviation of bin values is reasonable
        let mean: f32 = result.bins.iter().sum::<f32>() / result.bins.len() as f32;
        let variance: f32 = result.bins.iter().map(|b| (b - mean).powi(2)).sum::<f32>()
            / result.bins.len() as f32;
        let std_dev = variance.sqrt();

        // For white noise, bins should be within ~15 dB std dev of mean
        assert!(
            std_dev < 15.0,
            "Noise floor not flat enough: std_dev = {} dB (mean = {} dB)",
            std_dev,
            mean
        );
    }

    #[test]
    fn test_fft_size_matches_output() {
        let fft_size = 1024;
        let samples = vec![IqSample::new(0.1, 0.1); fft_size];
        let mut processor = FftProcessor::new(fft_size, 1);
        let result = processor
            .process(&samples, 100e6, 2_048_000.0)
            .expect("Should produce result");
        assert_eq!(result.bins.len(), fft_size);
    }

    #[test]
    fn test_averaging() {
        let fft_size = 512;
        let samples = vec![IqSample::new(0.5, 0.0); fft_size];
        let mut processor = FftProcessor::new(fft_size, 3);

        // First two calls should return None (still accumulating)
        assert!(processor.process(&samples, 100e6, 2_048_000.0).is_none());
        assert!(processor.process(&samples, 100e6, 2_048_000.0).is_none());

        // Third call should return the averaged result
        let result = processor.process(&samples, 100e6, 2_048_000.0);
        assert!(result.is_some(), "Third frame should produce averaged result");
    }

    #[test]
    fn test_set_avg_count() {
        let fft_size = 512;
        let samples = vec![IqSample::new(0.5, 0.0); fft_size];
        let mut processor = FftProcessor::new(fft_size, 1);

        // With avg_count=1, every call produces a result
        assert!(processor.process(&samples, 100e6, 2_048_000.0).is_some());

        // Change to avg_count=3
        processor.set_avg_count(3);
        assert!(processor.process(&samples, 100e6, 2_048_000.0).is_none());
        assert!(processor.process(&samples, 100e6, 2_048_000.0).is_none());
        assert!(processor.process(&samples, 100e6, 2_048_000.0).is_some());
    }

    #[test]
    fn test_set_avg_count_zero_clamps_to_one() {
        let fft_size = 512;
        let samples = vec![IqSample::new(0.5, 0.0); fft_size];
        let mut processor = FftProcessor::new(fft_size, 1);

        processor.set_avg_count(0);
        // Should behave as avg_count=1
        assert!(processor.process(&samples, 100e6, 2_048_000.0).is_some());
    }

    #[test]
    fn test_set_avg_count_same_value_no_reset() {
        let fft_size = 512;
        let samples = vec![IqSample::new(0.5, 0.0); fft_size];
        let mut processor = FftProcessor::new(fft_size, 3);

        // Accumulate one frame
        assert!(processor.process(&samples, 100e6, 2_048_000.0).is_none());

        // Set same value — should NOT reset accumulator
        processor.set_avg_count(3);

        // Two more frames should complete the average
        assert!(processor.process(&samples, 100e6, 2_048_000.0).is_none());
        assert!(processor.process(&samples, 100e6, 2_048_000.0).is_some());
    }
}
