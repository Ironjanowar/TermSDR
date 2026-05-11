// TODO: CLI argument parsing and JSON config loading.
// Will use clap derive for CLI args and merge with JSON config file.
// CLI args override JSON config values.

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

    /// JSON config file path
    #[arg(short, long)]
    pub config: Option<String>,
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
                signals: vec![(100_000.0, 0.8)],
            }
        } else {
            crate::types::SourceType::RtlSdr
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
}
