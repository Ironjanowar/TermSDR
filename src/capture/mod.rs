pub mod file;
pub mod rtlsdr;
pub mod test_signal;

use anyhow::Result;

use crate::types::IqSample;

/// A source of IQ samples.
pub trait Source: Send {
    /// Read IQ samples into the provided buffer.
    /// Returns the number of samples actually read.
    fn read_samples(&mut self, buffer: &mut [IqSample]) -> Result<usize>;

    /// Get the sample rate in Hz.
    fn sample_rate(&self) -> f64;

    /// Get the center frequency in Hz.
    fn center_freq(&self) -> f64;
}
