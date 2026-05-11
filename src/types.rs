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
    pub fn bin_frequency(&self, bin: usize) -> f64 {
        let n = self.bins.len() as f64;
        let bin_width = self.sample_rate / n;
        self.center_freq - (self.sample_rate / 2.0) + (bin as f64 * bin_width)
    }

    /// Returns the bin index closest to a given frequency.
    pub fn frequency_to_bin(&self, freq: f64) -> usize {
        let n = self.bins.len() as f64;
        let bin_width = self.sample_rate / n;
        let offset = freq - (self.center_freq - self.sample_rate / 2.0);
        let bin = (offset / bin_width).round() as usize;
        bin.min(self.bins.len().saturating_sub(1))
    }

    /// Returns the peak bin index and its dB value.
    pub fn peak(&self) -> (usize, f32) {
        self.bins
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
            .map(|(i, &v)| (i, v))
            .unwrap_or((0, f32::NEG_INFINITY))
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
    RtlSdr,
    /// IQ file replay.
    File { path: String },
    /// Synthetic test signal.
    Test {
        /// Signal peaks as (frequency_offset_hz, amplitude) pairs.
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
}
