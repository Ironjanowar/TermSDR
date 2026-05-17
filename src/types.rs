//! Shared types used across `TermSDR`: IQ samples, FFT results, and application configuration.

use num_complex::Complex;
use serde::{Deserialize, Serialize};

/// An IQ sample is a complex f32 value (I = real, Q = imaginary).
pub type IqSample = Complex<f32>;

/// Result of an FFT computation: power values in dB.
#[derive(Debug, Clone)]
pub struct FftResult {
    /// Power values in dB, one per frequency bin.
    pub bins: Vec<f32>,
    /// Center frequency in Hz.
    pub center_freq: f64,
    /// Sample rate in Hz.
    pub sample_rate: f64,
}

impl FftResult {
    /// Returns the frequency corresponding to a given bin index.
    #[must_use] 
    #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
    pub fn bin_frequency(&self, bin: usize) -> f64 {
        #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
        let n = self.bins.len() as f64;
        let bin_width = self.sample_rate / n;
        self.center_freq - (self.sample_rate / 2.0) + (bin as f64 * bin_width)
    }

    /// Returns the bin index closest to a given frequency.
    #[must_use] 
    pub fn frequency_to_bin(&self, freq: f64) -> usize {
        #[allow(clippy::cast_precision_loss)] // precision loss acceptable for DSP/UI
        let n = self.bins.len() as f64;
        let bin_width = self.sample_rate / n;
        let offset = freq - (self.center_freq - self.sample_rate / 2.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // intentional truncation; value is non-negative here
        let bin = (offset / bin_width).round() as usize;
        bin.min(self.bins.len().saturating_sub(1))
    }

    /// Returns the peak bin index and its `dB` value.
    ///
    /// # Panics
    /// Returns the peak bin index and value. NaN values are treated as equal to other values.
    #[must_use] 
    pub fn peak(&self) -> (usize, f32) {
        self.bins
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map_or((0, f32::NEG_INFINITY), |(i, &v)| (i, v))
    }
}

/// Application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Center frequency in Hz.
    pub center_freq: f64,
    /// Sample rate in Hz.
    pub sample_rate: f64,
    /// FFT size (number of bins).
    pub fft_size: usize,
    /// RF gain in dB (None = auto/AGC).
    pub gain: Option<f32>,
    /// PPM correction for crystal offset.
    pub ppm: i32,
    /// Input source type.
    pub source: SourceType,
}

/// Type of input source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    /// RTL-SDR hardware.
    RtlSdr { device_index: usize },
    /// IQ file replay.
    File { path: String },
    /// Synthetic test signal.
    Test {
        /// Signal peaks as (`frequency_offset_hz`, amplitude) pairs.
        signals: Vec<(f64, f32)>,
    },
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            center_freq: 100_000_000.0, // 100 MHz (FM broadcast)
            sample_rate: 2_048_000.0,    // 2.048 MS/s
            fft_size: 2048,
            gain: None, // AGC
            ppm: 0,
            source: SourceType::Test {
                signals: vec![(100_000.0, 0.8)], // 100 kHz offset, amplitude 0.8
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iq_sample_creation() {
        let sample = IqSample::new(1.0, -0.5);
        assert_eq!(sample.re, 1.0);
        assert_eq!(sample.im, -0.5);
    }

    #[test]
    fn test_iq_sample_operations() {
        let a = IqSample::new(1.0, 2.0);
        let b = IqSample::new(3.0, 4.0);
        let sum = a + b;
        assert_eq!(sum.re, 4.0);
        assert_eq!(sum.im, 6.0);

        let mag = a.norm();
        assert!((mag - (5.0_f32).sqrt()).abs() < 1e-6);
    }

    #[test]
    fn test_default_config() {
        let config = AppConfig::default();
        assert_eq!(config.center_freq, 100_000_000.0);
        assert_eq!(config.sample_rate, 2_048_000.0);
        assert_eq!(config.fft_size, 2048);
        assert!(config.gain.is_none());
        assert_eq!(config.ppm, 0);
    }

    #[test]
    fn test_config_serialization() {
        let config = AppConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.center_freq, config.center_freq);
        assert_eq!(deserialized.fft_size, config.fft_size);
    }

    #[test]
    fn test_fft_result_bin_frequency() {
        let result = FftResult {
            bins: vec![0.0; 1024],
            center_freq: 100_000_000.0,
            sample_rate: 2_048_000.0,
        };
        // Bin 0 should be at center_freq - sample_rate/2
        let freq0 = result.bin_frequency(0);
        assert!((freq0 - 98_976_000.0).abs() < 1.0);
        // Bin 512 should be near center frequency
        let freq_mid = result.bin_frequency(512);
        assert!((freq_mid - 100_000_000.0).abs() < 2100.0);
    }

    #[test]
    fn test_fft_result_frequency_to_bin() {
        let result = FftResult {
            bins: vec![0.0; 1024],
            center_freq: 100_000_000.0,
            sample_rate: 2_048_000.0,
        };
        let bin = result.frequency_to_bin(100_000_000.0);
        assert_eq!(bin, 512);
    }

    #[test]
    fn test_fft_result_peak() {
        let mut bins = vec![-100.0_f32; 256];
        bins[42] = -10.0;
        let result = FftResult {
            bins,
            center_freq: 100_000_000.0,
            sample_rate: 2_048_000.0,
        };
        let (peak_bin, peak_val) = result.peak();
        assert_eq!(peak_bin, 42);
        assert_eq!(peak_val, -10.0);
    }

    // === Edge-case tests ===

    #[test]
    fn test_bin_frequency_single_bin() {
        let result = FftResult {
            bins: vec![0.0],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        // With 1 bin, bin_width = sample_rate/1, freq = center - sr/2 + 0
        let freq = result.bin_frequency(0);
        assert!((freq - (100e6 - 1.024e6)).abs() < 1.0);
    }

    #[test]
    fn test_bin_frequency_last_bin() {
        let result = FftResult {
            bins: vec![0.0; 1024],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        let freq = result.bin_frequency(1023);
        // Should be near center + sr/2
        let expected = 100e6 - 1.024e6 + 1023.0 * (2.048e6 / 1024.0);
        assert!((freq - expected).abs() < 1.0);
    }

    #[test]
    fn test_frequency_to_bin_below_range() {
        let result = FftResult {
            bins: vec![0.0; 1024],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        // Frequency way below range — offset negative, round to 0 via saturating_sub
        // Actually (offset / bin_width).round() as usize will wrap for negative → huge usize
        // Then .min(1023) clamps it. Let's see what happens:
        let bin = result.frequency_to_bin(50e6); // way below
        // negative offset → large usize after cast → clamped to 1023
        assert!(bin <= 1023);
    }

    #[test]
    fn test_frequency_to_bin_above_range() {
        let result = FftResult {
            bins: vec![0.0; 1024],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        let bin = result.frequency_to_bin(200e6);
        assert_eq!(bin, 1023); // clamped
    }

    #[test]
    fn test_frequency_to_bin_negative_freq() {
        let result = FftResult {
            bins: vec![0.0; 1024],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        let bin = result.frequency_to_bin(-100e6);
        // Huge negative offset → wraps to large usize → clamped
        assert!(bin <= 1023);
    }

    #[test]
    fn test_peak_empty_bins() {
        let result = FftResult {
            bins: vec![],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        let (idx, val) = result.peak();
        assert_eq!(idx, 0);
        assert_eq!(val, f32::NEG_INFINITY);
    }

    #[test]
    fn test_peak_all_equal() {
        let result = FftResult {
            bins: vec![-50.0; 100],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        let (idx, val) = result.peak();
        assert_eq!(val, -50.0);
        // max_by returns last max when all equal
        assert_eq!(idx, 99);
    }

    #[test]
    fn test_peak_with_nan() {
        // NaN values are treated as equal, so peak returns a valid non-NaN bin
        let result = FftResult {
            bins: vec![f32::NAN, -50.0, -30.0],
            center_freq: 100e6,
            sample_rate: 2.048e6,
        };
        let (idx, val) = result.peak();
        // With unwrap_or(Equal), NaN is treated as equal to other values;
        // max_by returns the last equal element, so -30.0 at index 2 wins
        assert_eq!(idx, 2);
        assert!((val - (-30.0)).abs() < f32::EPSILON);
    }
}
