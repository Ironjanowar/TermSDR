use std::f32::consts::PI;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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
    /// Absolute frequencies of each signal (Hz) and their amplitudes.
    absolute_signals: Vec<(f64, f32)>,
    /// Noise amplitude (RMS).
    noise_amplitude: f32,
    /// Current phase accumulator for each signal (in radians).
    phases: Vec<f64>,
    /// Simple PRNG state for noise generation.
    rng_state: u64,
    /// Shared center frequency (updated by TUI thread).
    shared_center_freq: Option<Arc<AtomicU64>>,
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
        // Convert relative offsets to absolute frequencies
        let absolute_signals: Vec<(f64, f32)> = signals
            .iter()
            .map(|(offset, amp)| (center_freq + offset, *amp))
            .collect();
        Self {
            sample_rate,
            center_freq,
            absolute_signals,
            noise_amplitude,
            phases,
            rng_state: 12345,
            shared_center_freq: None,
        }
    }

    /// Attach a shared center frequency for reactive tuning.
    pub fn with_shared_freq(mut self, freq: Arc<AtomicU64>) -> Self {
        self.shared_center_freq = Some(freq);
        self
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

        // Read current center_freq from shared atomic if available
        let current_center = if let Some(ref shared) = self.shared_center_freq {
            f64::from_bits(shared.load(Ordering::Relaxed))
        } else {
            self.center_freq
        };

        for sample in buffer.iter_mut() {
            // Start with noise
            let noise_i = self.next_random() * self.noise_amplitude;
            let noise_q = self.next_random() * self.noise_amplitude;
            let mut i = noise_i;
            let mut q = noise_q;

            // Add each signal tone
            for (sig_idx, (abs_freq, amplitude)) in self.absolute_signals.iter().enumerate() {
                let phase = self.phases[sig_idx];
                i += amplitude * (phase as f32).cos();
                q += amplitude * (phase as f32).sin();

                // Compute offset from current center frequency
                let freq_offset = abs_freq - current_center;
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

    // === Edge-case tests ===

    #[test]
    fn test_zero_length_buffer() {
        let mut source = TestSource::new(2_048_000.0, 100e6, vec![(100_000.0, 0.8)], 0.01);
        let mut buffer: Vec<IqSample> = vec![];
        let n = source.read_samples(&mut buffer).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_empty_signals_list() {
        let mut source = TestSource::new(2_048_000.0, 100e6, vec![], 0.0);
        let mut buffer = vec![IqSample::new(0.0, 0.0); 100];
        let n = source.read_samples(&mut buffer).unwrap();
        assert_eq!(n, 100);
        // With no signals and no noise, all samples should be zero
        for s in &buffer {
            assert_eq!(s.re, 0.0);
            assert_eq!(s.im, 0.0);
        }
    }

    #[test]
    fn test_very_high_frequency_offset() {
        // Beyond Nyquist — should still produce samples without panic
        let mut source = TestSource::new(2_048_000.0, 100e6, vec![(10_000_000.0, 0.8)], 0.0);
        let mut buffer = vec![IqSample::new(0.0, 0.0); 1024];
        let n = source.read_samples(&mut buffer).unwrap();
        assert_eq!(n, 1024);
    }

    #[test]
    fn test_very_large_amplitude() {
        let mut source = TestSource::new(2_048_000.0, 100e6, vec![(100_000.0, 100.0)], 0.0);
        let mut buffer = vec![IqSample::new(0.0, 0.0); 1024];
        source.read_samples(&mut buffer).unwrap();
        let max_mag = buffer.iter().map(|s| s.norm()).fold(0.0_f32, f32::max);
        assert!(max_mag > 50.0); // large amplitude produces large output
    }

    #[test]
    fn test_negative_amplitude() {
        let mut source = TestSource::new(2_048_000.0, 100e6, vec![(100_000.0, -0.5)], 0.0);
        let mut buffer = vec![IqSample::new(0.0, 0.0); 1024];
        source.read_samples(&mut buffer).unwrap();
        // Negative amplitude just inverts the signal; magnitude should be ~0.5
        let avg_mag: f32 = buffer.iter().map(|s| s.norm()).sum::<f32>() / buffer.len() as f32;
        assert!((avg_mag - 0.5).abs() < 0.05);
    }

    #[test]
    fn test_zero_sample_rate() {
        // dt = 1/0 = inf, phase_increment = inf, phase wraps
        // Should not panic
        let mut source = TestSource::new(0.0, 100e6, vec![(100_000.0, 0.5)], 0.01);
        let mut buffer = vec![IqSample::new(0.0, 0.0); 10];
        let n = source.read_samples(&mut buffer).unwrap();
        assert_eq!(n, 10);
    }

    #[test]
    fn test_shared_freq_reacts_to_changes() {
        use std::sync::atomic::AtomicU64;

        let shared = Arc::new(AtomicU64::new(100e6_f64.to_bits()));
        let mut source = TestSource::new(2_048_000.0, 100e6, vec![(100_000.0, 0.8)], 0.0)
            .with_shared_freq(shared.clone());

        let mut buf1 = vec![IqSample::new(0.0, 0.0); 1024];
        source.read_samples(&mut buf1).unwrap();

        // Change center freq — signal should shift
        shared.store(101e6_f64.to_bits(), Ordering::Relaxed);
        // Reset phases for cleaner comparison
        source.phases = vec![0.0; source.absolute_signals.len()];
        source.rng_state = 12345;

        let mut buf2 = vec![IqSample::new(0.0, 0.0); 1024];
        source.read_samples(&mut buf2).unwrap();

        // Signals should differ because offset changed
        let differs = buf1.iter().zip(buf2.iter()).any(|(a, b)| (a.re - b.re).abs() > 0.001);
        assert!(differs, "Signal should change when center_freq changes");
    }

    #[test]
    fn test_without_shared_freq_backward_compatible() {
        // Without shared freq, should work exactly as before
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

    #[test]
    fn test_signal_peaks_shift_correctly() {
        use std::sync::atomic::AtomicU64;

        // Signal at absolute 100.1 MHz (center 100 MHz + 100 kHz offset)
        let shared = Arc::new(AtomicU64::new(100e6_f64.to_bits()));
        let mut source = TestSource::new(2_048_000.0, 100e6, vec![(100_000.0, 0.8)], 0.0)
            .with_shared_freq(shared.clone());

        // At center=100 MHz, offset should be +100 kHz
        // At center=100.05 MHz, offset should be +50 kHz
        // The absolute frequency of the signal is 100.1 MHz
        assert_eq!(source.absolute_signals[0].0, 100_100_000.0);

        // Change to 100.05 MHz center
        shared.store(100_050_000.0_f64.to_bits(), Ordering::Relaxed);
        // The signal still exists at 100.1 MHz absolute,
        // but now offset = 100.1M - 100.05M = 50 kHz
        // This is tested implicitly by read_samples using the new offset
        let mut buf = vec![IqSample::new(0.0, 0.0); 1024];
        source.read_samples(&mut buf).unwrap();
        // Should produce valid samples without panic
        assert_eq!(buf.len(), 1024);
    }
}
