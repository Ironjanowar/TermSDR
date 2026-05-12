//! Synthetic AM signal generator for testing the demodulation pipeline.

use crate::types::IqSample;
use std::f64::consts::PI;

/// Generate a synthetic AM-modulated signal for testing.
///
/// Produces: `A * (1 + m * sin(2π * f_audio * t)) * e^(j * 2π * f_carrier * t)`
///
/// # Arguments
/// * `carrier_freq` - Carrier frequency in Hz, relative to center_freq (offset)
/// * `audio_freq` - Modulating audio tone frequency in Hz
/// * `modulation_index` - AM modulation depth, 0.0 to 1.0
/// * `sample_rate` - Sample rate in Hz
/// * `center_freq` - Center frequency of the "receiver" in Hz (used to compute carrier offset)
/// * `num_samples` - Number of IQ samples to generate
pub fn generate_am_signal(
    carrier_freq: f64,
    audio_freq: f64,
    modulation_index: f64,
    sample_rate: f64,
    center_freq: f64,
    num_samples: usize,
) -> Vec<IqSample> {
    let offset = carrier_freq - center_freq;
    let mut samples = Vec::with_capacity(num_samples);

    for i in 0..num_samples {
        let t = i as f64 / sample_rate;

        // AM envelope: 1 + m * sin(2π * f_audio * t)
        let envelope = 1.0 + modulation_index * (2.0 * PI * audio_freq * t).sin();

        // Carrier at offset frequency: e^(j * 2π * offset * t)
        let phase = 2.0 * PI * offset * t;
        let carrier_i = phase.cos();
        let carrier_q = phase.sin();

        // Modulated signal
        let i_val = (envelope * carrier_i) as f32;
        let q_val = (envelope * carrier_q) as f32;

        samples.push(IqSample::new(i_val, q_val));
    }

    samples
}

/// Generate a pure tone (unmodulated carrier) for testing frequency shift.
pub fn generate_tone(
    freq_offset: f64,
    amplitude: f64,
    sample_rate: f64,
    num_samples: usize,
) -> Vec<IqSample> {
    let mut samples = Vec::with_capacity(num_samples);
    for i in 0..num_samples {
        let t = i as f64 / sample_rate;
        let phase = 2.0 * std::f64::consts::PI * freq_offset * t;
        let i_val = (amplitude * phase.cos()) as f32;
        let q_val = (amplitude * phase.sin()) as f32;
        samples.push(IqSample::new(i_val, q_val));
    }
    samples
}

/// Generate a synthetic FM-modulated signal for testing.
///
/// Produces: `e^(j * phase(t))` where
/// `phase(t) = 2π * offset * t + (deviation/audio_freq) * sin(2π * audio_freq * t)`
///
/// # Arguments
/// * `carrier_freq` - Carrier frequency in Hz (absolute, offset from center_freq)
/// * `audio_freq` - Modulating audio tone frequency in Hz
/// * `deviation` - Frequency deviation in Hz
/// * `sample_rate` - Sample rate in Hz
/// * `center_freq` - Center frequency of the "receiver" in Hz
/// * `num_samples` - Number of IQ samples to generate
pub fn generate_fm_signal(
    carrier_freq: f64,
    audio_freq: f64,
    deviation: f64,
    sample_rate: f64,
    center_freq: f64,
    num_samples: usize,
) -> Vec<IqSample> {
    let offset = carrier_freq - center_freq;
    let mut samples = Vec::with_capacity(num_samples);
    let modulation_index = deviation / audio_freq;

    for i in 0..num_samples {
        let t = i as f64 / sample_rate;
        let phase = 2.0 * PI * offset * t
            + modulation_index * (2.0 * PI * audio_freq * t).sin();
        let i_val = phase.cos() as f32;
        let q_val = phase.sin() as f32;
        samples.push(IqSample::new(i_val, q_val));
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_am_signal_length() {
        let samples = generate_am_signal(100.1e6, 1000.0, 0.8, 2.048e6, 100.0e6, 4096);
        assert_eq!(samples.len(), 4096);
    }

    #[test]
    fn test_am_signal_amplitude_range() {
        // With m=0.8, envelope varies from 0.2 to 1.8
        // Magnitude of each sample should be within that range
        let samples = generate_am_signal(100.1e6, 1000.0, 0.8, 2.048e6, 100.0e6, 4096);
        for s in &samples {
            let mag = s.norm();
            assert!(mag >= 0.0, "Negative magnitude impossible");
            assert!(mag <= 2.0, "Magnitude {} too high for m=0.8", mag);
        }
    }

    #[test]
    fn test_am_signal_unmodulated_constant_magnitude() {
        // With m=0, envelope is constant 1.0, so magnitude should be ~1.0 everywhere
        let samples = generate_am_signal(100.1e6, 1000.0, 0.0, 2.048e6, 100.0e6, 4096);
        for s in &samples {
            let mag = s.norm();
            assert!((mag - 1.0).abs() < 0.01, "Unmodulated signal magnitude should be ~1.0, got {}", mag);
        }
    }

    #[test]
    fn test_am_signal_at_center_freq_is_dc() {
        // Carrier at center_freq → offset = 0 → all samples have phase 0 → I ≈ envelope, Q ≈ 0
        let samples = generate_am_signal(100.0e6, 1000.0, 0.5, 2.048e6, 100.0e6, 4096);
        for s in &samples {
            assert!(s.im.abs() < 0.01, "Q should be ~0 when carrier is at center, got {}", s.im);
            assert!(s.re > 0.0, "I should be positive (envelope)");
        }
    }

    #[test]
    fn test_tone_length() {
        let samples = generate_tone(100_000.0, 1.0, 2.048e6, 2048);
        assert_eq!(samples.len(), 2048);
    }

    #[test]
    fn test_tone_constant_magnitude() {
        let samples = generate_tone(100_000.0, 0.7, 2.048e6, 2048);
        for s in &samples {
            let mag = s.norm();
            assert!((mag - 0.7).abs() < 0.01, "Tone magnitude should be ~0.7, got {}", mag);
        }
    }

    #[test]
    fn test_tone_dc_is_constant() {
        // A tone at 0 Hz offset should be a constant: I = amplitude, Q = 0
        let samples = generate_tone(0.0, 1.0, 2.048e6, 1024);
        for s in &samples {
            assert!((s.re - 1.0).abs() < 0.001);
            assert!(s.im.abs() < 0.001);
        }
    }

    // FM signal tests

    #[test]
    fn test_fm_signal_length() {
        let samples = generate_fm_signal(100.1e6, 1000.0, 75_000.0, 2.048e6, 100.0e6, 4096);
        assert_eq!(samples.len(), 4096);
    }

    #[test]
    fn test_fm_signal_constant_magnitude() {
        // FM signal should have constant magnitude (~1.0)
        let samples = generate_fm_signal(100.1e6, 1000.0, 75_000.0, 2.048e6, 100.0e6, 4096);
        for s in &samples {
            let mag = s.norm();
            assert!(
                (mag - 1.0).abs() < 0.01,
                "FM signal magnitude should be ~1.0, got {}",
                mag
            );
        }
    }

    #[test]
    fn test_fm_signal_unmodulated_is_carrier() {
        // With deviation=0, should be same as a pure tone
        let fm = generate_fm_signal(100.1e6, 1000.0, 0.0, 2.048e6, 100.0e6, 1024);
        let tone = generate_tone(100_000.0, 1.0, 2.048e6, 1024);
        for (f, t) in fm.iter().zip(tone.iter()) {
            assert!((f.re - t.re).abs() < 0.01, "I mismatch: {} vs {}", f.re, t.re);
            assert!((f.im - t.im).abs() < 0.01, "Q mismatch: {} vs {}", f.im, t.im);
        }
    }
}
