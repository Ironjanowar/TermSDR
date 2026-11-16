//! CLI argument parsing and configuration loading.

use anyhow::Result;
use clap::Parser;

use crate::capture::file::IqFileFormat;
use crate::types::AppConfig;

/// `TermSDR` — Terminal-based SDR application.
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

    /// Sample format of a raw IQ file (`SigMF` datasets provide it themselves)
    #[arg(long, value_enum)]
    pub iq_format: Option<IqFileFormat>,

    /// Sample rate of a raw IQ file in Hz (`SigMF` datasets provide it themselves)
    #[arg(long)]
    pub file_sample_rate: Option<f64>,

    /// Center frequency of a raw IQ file in Hz (`SigMF` datasets provide it themselves)
    #[arg(long)]
    pub file_center_frequency: Option<f64>,

    /// JSON config file path
    #[arg(short, long)]
    pub config: Option<String>,

    /// Import bookmarks from `CubicSDR` XML file
    #[arg(long)]
    pub import_bookmarks: Option<String>,

    /// Run in headless mode (WebSocket server, no TUI)
    #[arg(long)]
    pub headless: bool,

    /// WebSocket server port (headless mode)
    #[arg(long, default_value = "8080")]
    pub port: u16,

    /// WebSocket server bind address (headless mode)
    #[arg(long, default_value = "0.0.0.0")]
    pub bind: String,

    /// Don't serve web frontend (WebSocket only)
    #[arg(long)]
    pub no_web: bool,

    /// Audio format for WebSocket streaming
    #[arg(long, default_value = "opus")]
    pub audio_format: String,
}

/// Parse a frequency string like "100M", "118.1M", "433920000" into Hz.
///
/// # Errors
/// Returns an error if the string cannot be parsed as a valid frequency.
pub(crate) fn parse_frequency(s: &str) -> Result<f64> {
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
    /// Convert CLI args into an `AppConfig`.
    ///
    /// # Errors
    /// Returns an error if frequency or sample rate values are invalid.
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

    // === IQ file replay CLI metadata (plan Task 3) ===
    //
    // Frozen contract for the raw-file flags:
    //
    // ```text
    // --iq-format <cu8|cf32le>          -> CliArgs::iq_format: Option<IqFileFormat>
    // --file-sample-rate <Hz>           -> CliArgs::file_sample_rate: Option<f64>
    // --file-center-frequency <Hz>      -> CliArgs::file_center_frequency: Option<f64>
    // ```
    //
    // Decisions frozen here where the plan left a choice:
    //
    // * `--iq-format` takes the `IqFileFormat` `ValueEnum` directly, so clap rejects
    //   unknown formats instead of the capture layer failing later. The accepted
    //   spellings are `cu8` and `cf32le` — the variant must carry
    //   `#[value(name = "cf32le")]`, because a bare derive would only accept
    //   `cf32-le` and the plan's documented spelling would stop working.
    // * The two frequency flags parse plain `f64` Hz, exactly as the plan writes
    //   `<Hz>`. They deliberately do not accept the `118.1M` shorthand that
    //   `--sample-rate`/`--frequency` accept: file metadata is a machine-facing
    //   value and must not be re-scaled implicitly.
    // * All three default to `None` so that a `.sigmf-meta` dataset can supply them
    //   and so existing invocations keep their behavior.
    // * These fields reference `crate::capture::file::IqFileFormat`, which requires
    //   `pub mod file;` in `src/capture/mod.rs` (a `pub` field cannot expose a
    //   `pub(crate)` type).

    use crate::capture::file::IqFileFormat;

    #[test]
    fn cli_iq_format_flag_parses_cu8() {
        let args = CliArgs::parse_from(["termsdr", "--file", "capture.iq", "--iq-format", "cu8"]);
        assert_eq!(args.iq_format, Some(IqFileFormat::Cu8));
    }

    #[test]
    fn cli_iq_format_flag_parses_cf32le() {
        let args =
            CliArgs::parse_from(["termsdr", "--file", "capture.iq", "--iq-format", "cf32le"]);
        assert_eq!(args.iq_format, Some(IqFileFormat::Cf32Le));
    }

    #[test]
    fn cli_iq_format_flag_defaults_to_none() {
        let args = CliArgs::parse_from(["termsdr", "--file", "capture.iq"]);
        assert!(args.iq_format.is_none());
    }

    #[test]
    fn cli_iq_format_flag_rejects_unknown_format() {
        let result =
            CliArgs::try_parse_from(["termsdr", "--file", "capture.iq", "--iq-format", "s16"]);
        assert!(
            result.is_err(),
            "unknown --iq-format must be rejected by clap, not accepted"
        );
    }

    #[test]
    fn cli_file_sample_rate_parses_hz() {
        let args = CliArgs::parse_from([
            "termsdr",
            "--file",
            "capture.iq",
            "--file-sample-rate",
            "2048000",
        ]);
        assert_eq!(args.file_sample_rate, Some(2_048_000.0));
    }

    #[test]
    fn cli_file_center_frequency_parses_hz() {
        let args = CliArgs::parse_from([
            "termsdr",
            "--file",
            "capture.iq",
            "--file-center-frequency",
            "118100000",
        ]);
        assert_eq!(args.file_center_frequency, Some(118_100_000.0));
    }

    #[test]
    fn cli_file_metadata_flags_default_to_none() {
        let args = CliArgs::parse_from(["termsdr", "--file", "capture.sigmf-meta"]);
        assert!(args.file_sample_rate.is_none());
        assert!(args.file_center_frequency.is_none());
    }

    #[test]
    fn cli_file_metadata_flags_reject_non_numeric_hz() {
        let rate = CliArgs::try_parse_from([
            "termsdr",
            "--file",
            "capture.iq",
            "--file-sample-rate",
            "2.048M",
        ]);
        assert!(
            rate.is_err(),
            "--file-sample-rate must accept plain Hz only, got accepted shorthand"
        );

        let freq = CliArgs::try_parse_from([
            "termsdr",
            "--file",
            "capture.iq",
            "--file-center-frequency",
            "118.1M",
        ]);
        assert!(
            freq.is_err(),
            "--file-center-frequency must accept plain Hz only, got accepted shorthand"
        );
    }
}
