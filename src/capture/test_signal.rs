use std::f32::consts::PI;

use anyhow::Result;

use crate::types::IqSample;
use super::Source;

/// Synthetic test signal generator.
///
/// Generates IQ data with a configurable noise floor and signal peaks
/// at specific frequency offsets from center. Useful for testing the
/// entire DSP pipeline without hardware.
pub struct TestSource {
    sample_rate: f64,
    center_freq: f64,
    /// Signal definitions: (frequency_offset_hz, amplitude).
    signals: Vec<(f64, f32)>,
    /// Noise amplitude (RMS).
    noise_amplitude: f32,
    /// Current phase accumulator for each signal (in radians).
    phases: Vec<f64>,
    /// Simple PRNG state for noise generation.
    rng_state: u64,
}

impl TestSource {
    /// Create a new test signal source.
    ///
    /// # Arguments
    /// * `sample_rate` - Sample rate in Hz
    /// * `center_freq` - Center frequency in Hz
    /// * `signals` - List of (frequency_offset_hz, amplitude) pairs
    /// * `noise_amplitude` - RMS amplitude of white noise floor
    pub fn new(
        sample_rate: f64,
        center_freq: f64,
        signals: Vec<(f64, f32)>,
        noise_amplitude: f32,
    ) -> Self {
        let phases = vec![0.0; signals.len()];
        Self {
            sample_rate,
            center_freq,
            signals,
            noise_amplitude,
            phases,
            rng_state: 12345,
        }
    }

    /// Simple xorshift64 PRNG for deterministic noise.
    fn next_random(&mut self) -> f32 {
        self.rng_state ^= self.rng_state << 13;
        self.rng_state ^= self.rng_state >> 7;
        self.rng_state ^= self.rng_state << 17;
        // Map to [-1.0, 1.0]
        (self.rng_state as i64 as f64 / i64::MAX as f64) as f32
    }
}

impl Source for TestSource {
    fn read_samples(&mut self, buffer: &mut [IqSample]) -> Result<usize> {
        let dt = 1.0 / self.sample_rate;

        for sample in buffer.iter_mut() {
            // Start with noise
            let noise_i = self.next_random() * self.noise_amplitude;
            let noise_q = self.next_random() * self.noise_amplitude;
            let mut i = noise_i;
            let mut q = noise_q;

            // Add each signal tone
            for (sig_idx, (freq_offset, amplitude)) in self.signals.iter().enumerate() {
                let phase = self.phases[sig_idx];
                i += amplitude * (phase as f32).cos();
                q += amplitude * (phase as f32).sin();

                // Advance phase
                let phase_increment = 2.0 * PI as f64 * freq_offset * dt;
                self.phases[sig_idx] = (phase + phase_increment) % (2.0 * PI as f64);
            }

            *sample = IqSample::new(i, q);
        }

        Ok(buffer.len())
    }

    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn center_freq(&self) -> f64 {
        self.center_freq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_produces_correct_number_of_samples() {
        let mut source = TestSource::new(2_048_000.0, 100e6, vec![(100_000.0, 0.8)], 0.01);
        let mut buffer = vec![IqSample::new(0.0, 0.0); 2048];
        let n = source.read_samples(&mut buffer).unwrap();
        assert_eq!(n, 2048);
    }

    #[test]
    fn test_noise_only_has_low_amplitude() {
        let mut source = TestSource::new(2_048_000.0, 100e6, vec![], 0.01);
        let mut buffer = vec![IqSample::new(0.0, 0.0); 4096];
        source.read_samples(&mut buffer).unwrap();

        // All samples should be near zero with noise_amplitude = 0.01
        for sample in &buffer {
            assert!(sample.norm() < 0.1, "Noise-only sample too large: {}", sample.norm());
        }
    }

    #[test]
    fn test_signal_has_expected_amplitude() {
        let amplitude = 0.8_f32;
        let mut source = TestSource::new(2_048_000.0, 100e6, vec![(100_000.0, amplitude)], 0.0);
        let mut buffer = vec![IqSample::new(0.0, 0.0); 4096];
        source.read_samples(&mut buffer).unwrap();

        // With no noise, each sample's magnitude should be close to the signal amplitude
        let avg_magnitude: f32 = buffer.iter().map(|s| s.norm()).sum::<f32>() / buffer.len() as f32;
        assert!(
            (avg_magnitude - amplitude).abs() < 0.05,
            "Expected avg magnitude ~{}, got {}",
            amplitude,
            avg_magnitude
        );
    }

    #[test]
    fn test_multiple_signals() {
        let mut source = TestSource::new(
            2_048_000.0,
            100e6,
            vec![(100_000.0, 0.5), (-200_000.0, 0.3)],
            0.0,
        );
        let mut buffer = vec![IqSample::new(0.0, 0.0); 2048];
        let n = source.read_samples(&mut buffer).unwrap();
        assert_eq!(n, 2048);

        // Combined signal magnitude should be related to both amplitudes
        // Max possible magnitude is sum of amplitudes = 0.8
        let max_mag = buffer.iter().map(|s| s.norm()).fold(0.0_f32, f32::max);
        assert!(max_mag <= 0.85, "Max magnitude too large: {}", max_mag);
        assert!(max_mag > 0.1, "Max magnitude too small: {}", max_mag);
    }

    #[test]
    fn test_source_trait_properties() {
        let source = TestSource::new(2_048_000.0, 100e6, vec![], 0.01);
        assert_eq!(source.sample_rate(), 2_048_000.0);
        assert_eq!(source.center_freq(), 100e6);
    }

    #[test]
    fn test_deterministic_output() {
        // Two sources with same params should produce identical output
        let mut source1 = TestSource::new(2_048_000.0, 100e6, vec![(50_000.0, 0.5)], 0.01);
        let mut source2 = TestSource::new(2_048_000.0, 100e6, vec![(50_000.0, 0.5)], 0.01);

        let mut buf1 = vec![IqSample::new(0.0, 0.0); 1024];
        let mut buf2 = vec![IqSample::new(0.0, 0.0); 1024];

        source1.read_samples(&mut buf1).unwrap();
        source2.read_samples(&mut buf2).unwrap();

        for (a, b) in buf1.iter().zip(buf2.iter()) {
            assert_eq!(a.re, b.re);
            assert_eq!(a.im, b.im);
        }
    }
}
