use anyhow::Result;
use clap::Parser;

use crate::types::AppConfig;

/// TermSDR — Terminal-based SDR application.
#[derive(Parser, Debug)]
#[command(name = "termsdr", version, about)]
pub struct CliArgs {
    /// Center frequency (e.g., "100M", "118.1M", "433920000")
    #[arg(short = 'f', long, default_value = "100M")]
    pub frequency: String,

    /// Sample rate (e.g., "2.048M", "1024000")
    #[arg(short = 's', long, default_value = "2.048M")]
    pub sample_rate: String,

    /// RF gain in dB (omit for AGC)
    #[arg(short, long)]
    pub gain: Option<f32>,

    /// FFT size
    #[arg(long, default_value = "2048")]
    pub fft_size: usize,

    /// PPM correction
    #[arg(long, default_value = "0")]
    pub ppm: i32,

    /// Use test signal source instead of hardware
    #[arg(long)]
    pub test: bool,

    /// IQ file path for file replay mode
    #[arg(long)]
    pub file: Option<String>,

    /// JSON config file path
    #[arg(short, long)]
    pub config: Option<String>,

    /// Import bookmarks from CubicSDR XML file
    #[arg(long)]
    pub import_bookmarks: Option<String>,
}

/// Parse a frequency string like "100M", "118.1M", "433920000" into Hz.
pub fn parse_frequency(s: &str) -> Result<f64> {
    let s = s.trim();
    if let Some(prefix) = s.strip_suffix('M') {
        Ok(prefix.parse::<f64>()? * 1_000_000.0)
    } else if let Some(prefix) = s.strip_suffix('k') {
        Ok(prefix.parse::<f64>()? * 1_000.0)
    } else if let Some(prefix) = s.strip_suffix('G') {
        Ok(prefix.parse::<f64>()? * 1_000_000_000.0)
    } else {
        Ok(s.parse::<f64>()?)
    }
}

impl CliArgs {
    /// Convert CLI args into an AppConfig.
    pub fn to_config(&self) -> Result<AppConfig> {
        let center_freq = parse_frequency(&self.frequency)?;
        let sample_rate = parse_frequency(&self.sample_rate)?;

        let source = if self.test {
            crate::types::SourceType::Test {
                signals: vec![
                    (100_000.0, 0.8),   // Strong signal at +100kHz
                    (-200_000.0, 0.4),  // Medium signal at -200kHz
                    (350_000.0, 0.2),   // Weak signal at +350kHz
                    (-500_000.0, 0.15), // Very weak signal at -500kHz
                ],
            }
        } else if let Some(path) = &self.file {
            crate::types::SourceType::File { path: path.clone() }
        } else {
            crate::types::SourceType::RtlSdr { device_index: 0 }
        };

        Ok(AppConfig {
            center_freq,
            sample_rate,
            fft_size: self.fft_size,
            gain: self.gain,
            ppm: self.ppm,
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frequency_mhz() {
        assert!((parse_frequency("100M").unwrap() - 100_000_000.0).abs() < 1.0);
        assert!((parse_frequency("118.1M").unwrap() - 118_100_000.0).abs() < 1.0);
    }

    #[test]
    fn test_parse_frequency_khz() {
        assert!((parse_frequency("433920k").unwrap() - 433_920_000.0).abs() < 1.0);
    }

    #[test]
    fn test_parse_frequency_ghz() {
        assert!((parse_frequency("1.42G").unwrap() - 1_420_000_000.0).abs() < 1.0);
    }

    #[test]
    fn test_parse_frequency_raw() {
        assert!((parse_frequency("433920000").unwrap() - 433_920_000.0).abs() < 1.0);
    }

    // === Edge-case tests ===

    #[test]
    fn test_parse_frequency_empty_string() {
        assert!(parse_frequency("").is_err());
    }

    #[test]
    fn test_parse_frequency_suffix_only() {
        assert!(parse_frequency("M").is_err());
        assert!(parse_frequency("k").is_err());
        assert!(parse_frequency("G").is_err());
    }

    #[test]
    fn test_parse_frequency_negative() {
        assert!((parse_frequency("-100M").unwrap() - (-100_000_000.0)).abs() < 1.0);
    }

    #[test]
    fn test_parse_frequency_zero() {
        assert!((parse_frequency("0").unwrap()).abs() < f64::EPSILON);
        assert!((parse_frequency("0M").unwrap()).abs() < f64::EPSILON);
        assert!((parse_frequency("0k").unwrap()).abs() < f64::EPSILON);
        assert!((parse_frequency("0G").unwrap()).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_frequency_very_large() {
        assert!((parse_frequency("999G").unwrap() - 999_000_000_000.0).abs() < 1.0);
    }

    #[test]
    fn test_parse_frequency_very_small() {
        assert!((parse_frequency("0.001k").unwrap() - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_frequency_invalid_strings() {
        assert!(parse_frequency("abc").is_err());
        assert!(parse_frequency("12.3.4M").is_err());
        assert!(parse_frequency("M100").is_err());
    }

    #[test]
    fn test_parse_frequency_whitespace() {
        assert!((parse_frequency(" 100M ").unwrap() - 100_000_000.0).abs() < 1.0);
        assert!(parse_frequency("100 M").is_err());
    }

    #[test]
    fn test_parse_frequency_case_sensitivity() {
        assert!(parse_frequency("100m").is_err());
        assert!(parse_frequency("100K").is_err());
    }

    #[test]
    fn test_to_config_default() {
        let args = CliArgs::parse_from(["termsdr", "--test"]);
        let config = args.to_config().unwrap();
        assert!((config.center_freq - 100_000_000.0).abs() < 1.0);
        assert!((config.sample_rate - 2_048_000.0).abs() < 1.0);
        assert_eq!(config.fft_size, 2048);
        assert!(config.gain.is_none());
        assert_eq!(config.ppm, 0);
    }

    #[test]
    fn test_to_config_test_source_signals() {
        let args = CliArgs::parse_from(["termsdr", "--test"]);
        let config = args.to_config().unwrap();
        match &config.source {
            crate::types::SourceType::Test { signals } => {
                assert_eq!(signals.len(), 4);
                assert!((signals[0].0 - 100_000.0).abs() < 1.0);
                assert!((signals[0].1 - 0.8).abs() < 0.01);
            }
            _ => panic!("Expected Test source"),
        }
    }

    #[test]
    fn test_to_config_file_source() {
        let args = CliArgs::parse_from(["termsdr", "--file", "/tmp/test.iq"]);
        let config = args.to_config().unwrap();
        match &config.source {
            crate::types::SourceType::File { path } => {
                assert_eq!(path, "/tmp/test.iq");
            }
            _ => panic!("Expected File source"),
        }
    }
}
