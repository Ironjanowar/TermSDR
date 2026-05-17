//! RTL-SDR hardware source implementation.
//!
//! When compiled with the `rtlsdr` feature, provides full hardware access.
//! Without the feature, provides stubs that return appropriate errors.

use crate::types::IqSample;
use super::Source;

/// Convert raw RTL-SDR u8 IQ data to Complex<f32> samples.
///
/// RTL-SDR outputs unsigned 8-bit IQ pairs. This converts them to the
/// normalized [-1.0, 1.0] range expected by the DSP pipeline.
///
/// Each pair of bytes represents one IQ sample: [I0, Q0, I1, Q1, ...].
/// The u8 value 127.5 maps to 0.0 (DC center).
pub(crate) fn convert_u8_to_iq(raw: &[u8], output: &mut [IqSample]) -> usize {
    let samples = (raw.len() / 2).min(output.len());
    for i in 0..samples {
        let i_val = (f32::from(raw[i * 2]) - 127.5) / 127.5;
        let q_val = (f32::from(raw[i * 2 + 1]) - 127.5) / 127.5;
        output[i] = IqSample::new(i_val, q_val);
    }
    samples
}

// ─── Feature-gated device enumeration ───────────────────────────────────────

/// Get the number of detected RTL-SDR devices.
#[cfg(feature = "rtlsdr")]
#[must_use] 
pub(crate) fn get_device_count() -> i32 {
    rtlsdr_rs::get_device_count()
}

/// Get the name for a specific RTL-SDR device index.
#[cfg(feature = "rtlsdr")]
#[must_use] 
pub(crate) fn get_device_name(index: i32) -> String {
    rtlsdr_rs::get_device_name(index)
}

/// Get the number of detected RTL-SDR devices (stub: always 0).
#[cfg(not(feature = "rtlsdr"))]
pub(crate) fn get_device_count() -> i32 {
    0
}

/// Get the name for a specific RTL-SDR device index (stub: always empty).
#[cfg(not(feature = "rtlsdr"))]
pub(crate) fn get_device_name(_index: i32) -> String {
    String::new()
}

// ─── Feature-enabled: full implementation ───────────────────────────────────

#[cfg(feature = "rtlsdr")]
mod hw {
    use anyhow::{Result, anyhow};
    use crate::types::IqSample;
    use super::{Source, convert_u8_to_iq};

    /// RTL-SDR hardware source.
    pub struct RtlSdrSource {
        device: rtlsdr_rs::RTLSDRDevice,
        sample_rate: f64,
        center_freq: f64,
    }

    // RTLSDRDevice is Send+Sync per the crate
    unsafe impl Send for RtlSdrSource {}

    impl RtlSdrSource {
        /// Create a new RTL-SDR source.
        ///
        /// # Arguments
        /// * `device_index` - Zero-based device index
        /// * `center_freq` - Center frequency in Hz
        /// * `sample_rate` - Sample rate in Hz
        /// * `gain` - Tuner gain in `dB`, or None for AGC
        /// * `ppm` - Frequency correction in PPM
        ///
        /// # Errors
        /// Returns an error if the RTL-SDR device cannot be opened or configured.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // intentional truncation; value is non-negative here
        pub fn new(
            device_index: usize,
            center_freq: f64,
            sample_rate: f64,
            gain: Option<f32>,
            ppm: i32,
        ) -> Result<Self> {
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)] // intentional truncation; value fits in target type
            let mut dev = rtlsdr_rs::open(device_index as i32)
                .map_err(|e| anyhow!("Failed to open RTL-SDR device {device_index}: {e}"))?;

            dev.set_center_freq(center_freq as u32)
                .map_err(|e| anyhow!("Failed to set center freq: {e}"))?;

            dev.set_sample_rate(sample_rate as u32)
                .map_err(|e| anyhow!("Failed to set sample rate: {e}"))?;

            if let Some(g) = gain {
                dev.set_tuner_gain_mode(true)
                    .map_err(|e| anyhow!("Failed to set manual gain mode: {e}"))?;
                // rtlsdr crate uses tenths of dB
                dev.set_tuner_gain((g * 10.0) as i32)
                    .map_err(|e| anyhow!("Failed to set tuner gain: {e}"))?;
            } else {
                dev.set_tuner_gain_mode(false)
                    .map_err(|e| anyhow!("Failed to set AGC mode: {e}"))?;
            }

            if ppm != 0 {
                dev.set_freq_correction(ppm)
                    .map_err(|e| anyhow!("Failed to set freq correction: {e}"))?;
            }

            dev.reset_buffer()
                .map_err(|e| anyhow!("Failed to reset buffer: {e}"))?;

            Ok(Self {
                device: dev,
                sample_rate,
                center_freq,
            })
        }
    }

    impl Source for RtlSdrSource {
        fn read_samples(&mut self, buffer: &mut [IqSample]) -> Result<usize> {
            let bytes_needed = buffer.len() * 2;

            let raw_data = self.device
                .read_sync(bytes_needed)
                .map_err(|e| anyhow::anyhow!("RTL-SDR read error: {e}"))?;

            let samples = convert_u8_to_iq(&raw_data, buffer);
            Ok(samples)
        }

        fn sample_rate(&self) -> f64 {
            self.sample_rate
        }

        fn center_freq(&self) -> f64 {
            self.center_freq
        }

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // intentional truncation; value is non-negative here
        fn set_center_freq(&mut self, freq: f64) -> Result<()> {
            self.device
                .set_center_freq(freq as u32)
                .map_err(|e| anyhow::anyhow!("Failed to retune RTL-SDR: {e}"))?;
            self.center_freq = freq;
            Ok(())
        }
    }
}

#[cfg(feature = "rtlsdr")]
pub use hw::RtlSdrSource;

// ─── Feature-disabled: stub implementation ──────────────────────────────────

#[cfg(not(feature = "rtlsdr"))]
mod stub {
    use anyhow::{Result, bail};
    use crate::types::IqSample;
    use super::Source;

    /// RTL-SDR hardware source (stub — compiled without `rtlsdr` feature).
    #[derive(Debug)]
    pub struct RtlSdrSource {
        sample_rate: f64,
        center_freq: f64,
    }

    impl RtlSdrSource {
        /// Create a new RTL-SDR source.
        ///
        /// Always returns an error when compiled without the `rtlsdr` feature.
        pub fn new(
            _device_index: usize,
            _center_freq: f64,
            _sample_rate: f64,
            _gain: Option<f32>,
            _ppm: i32,
        ) -> Result<Self> {
            bail!(
                "RTL-SDR support not compiled. Rebuild with: cargo build --features rtlsdr"
            )
        }

        /// Create a stub instance for testing (not publicly useful).
        #[cfg(test)]
        fn new_unchecked(center_freq: f64, sample_rate: f64) -> Self {
            Self { sample_rate, center_freq }
        }
    }

    impl Source for RtlSdrSource {
        fn read_samples(&mut self, _buffer: &mut [IqSample]) -> Result<usize> {
            bail!("RTL-SDR support not compiled")
        }

        fn sample_rate(&self) -> f64 {
            self.sample_rate
        }

        fn center_freq(&self) -> f64 {
            self.center_freq
        }
    }
}

#[cfg(not(feature = "rtlsdr"))]
pub use stub::RtlSdrSource;

// ─── Tests (always compiled) ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // === convert_u8_to_iq tests ===

    #[test]
    fn test_convert_center_values() {
        // 127 and 128 should map close to 0.0
        let raw = [127u8, 128u8];
        let mut output = [IqSample::new(0.0, 0.0)];
        let n = convert_u8_to_iq(&raw, &mut output);
        assert_eq!(n, 1);
        // 127 → (127 - 127.5) / 127.5 ≈ -0.00392
        assert!((output[0].re - (-0.003_921_569)).abs() < 1e-5);
        // 128 → (128 - 127.5) / 127.5 ≈ 0.00392
        assert!((output[0].im - 0.003_921_569).abs() < 1e-5);
    }

    #[test]
    fn test_convert_min_values() {
        // 0 → (0 - 127.5) / 127.5 = -1.0
        let raw = [0u8, 0u8];
        let mut output = [IqSample::new(0.0, 0.0)];
        convert_u8_to_iq(&raw, &mut output);
        assert!((output[0].re - (-1.0)).abs() < 1e-5);
        assert!((output[0].im - (-1.0)).abs() < 1e-5);
    }

    #[test]
    fn test_convert_max_values() {
        // 255 → (255 - 127.5) / 127.5 = 1.0
        let raw = [255u8, 255u8];
        let mut output = [IqSample::new(0.0, 0.0)];
        convert_u8_to_iq(&raw, &mut output);
        assert!((output[0].re - 1.0).abs() < 1e-5);
        assert!((output[0].im - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_convert_multiple_samples() {
        let raw = [0u8, 255, 127, 128, 255, 0];
        let mut output = [IqSample::new(0.0, 0.0); 3];
        let n = convert_u8_to_iq(&raw, &mut output);
        assert_eq!(n, 3);
        assert!((output[0].re - (-1.0)).abs() < 1e-5);
        assert!((output[0].im - 1.0).abs() < 1e-5);
        assert!((output[2].re - 1.0).abs() < 1e-5);
        assert!((output[2].im - (-1.0)).abs() < 1e-5);
    }

    #[test]
    fn test_convert_odd_length_input() {
        // Odd number of bytes — last byte should be ignored
        let raw = [0u8, 255, 127];
        let mut output = [IqSample::new(0.0, 0.0); 3];
        let n = convert_u8_to_iq(&raw, &mut output);
        assert_eq!(n, 1); // Only 1 complete pair
    }

    #[test]
    fn test_convert_empty_input() {
        let raw: [u8; 0] = [];
        let mut output = [IqSample::new(0.0, 0.0); 1];
        let n = convert_u8_to_iq(&raw, &mut output);
        assert_eq!(n, 0);
    }

    #[test]
    fn test_convert_output_smaller_than_input() {
        let raw = [0u8, 0, 255, 255, 127, 128];
        let mut output = [IqSample::new(0.0, 0.0); 2]; // Only room for 2
        let n = convert_u8_to_iq(&raw, &mut output);
        assert_eq!(n, 2);
    }

    // === Stub behavior tests (only when feature disabled) ===

    #[cfg(not(feature = "rtlsdr"))]
    mod stub_tests {
        use super::*;

        #[test]
        fn test_device_count_returns_zero() {
            assert_eq!(get_device_count(), 0);
        }

        #[test]
        fn test_device_name_returns_empty() {
            assert_eq!(get_device_name(0), "");
            assert_eq!(get_device_name(42), "");
        }

        #[test]
        fn test_source_creation_fails() {
            let result = RtlSdrSource::new(0, 100e6, 2_048_000.0, None, 0);
            assert!(result.is_err());
            let err_msg = result.unwrap_err().to_string();
            assert!(err_msg.contains("not compiled"), "Error: {}", err_msg);
        }
    }

    // === SourceSelection variant exists ===

    #[test]
    fn test_source_selection_rtlsdr_variant() {
        // Verify the RtlSdr variant can be constructed
        use crate::ui::SourceSelection;
        let sel = SourceSelection::RtlSdr(0);
        assert_eq!(sel, SourceSelection::RtlSdr(0));
    }
}
