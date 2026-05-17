//! Signal capture sources: RTL-SDR hardware, IQ file replay, and synthetic test signals.

pub(crate) mod file;
pub mod rtlsdr;
pub mod test_signal;

use anyhow::Result;

use crate::types::IqSample;

/// A source of IQ samples.
pub trait Source: Send {
    /// Read IQ samples into the provided buffer.
    /// Returns the number of samples actually read.
    ///
    /// # Errors
    /// Returns an error if samples cannot be read from the source.
    fn read_samples(&mut self, buffer: &mut [IqSample]) -> Result<usize>;

    /// Get the sample rate in Hz.
    fn sample_rate(&self) -> f64;

    /// Get the center frequency in Hz.
    fn center_freq(&self) -> f64;

    /// Retune the source to a new center frequency (if supported by hardware).
    /// Default implementation does nothing.
    ///
    /// # Errors
    /// Returns an error if the hardware rejects the requested frequency.
    fn set_center_freq(&mut self, _freq: f64) -> Result<()> {
        Ok(())
    }
}
