//! IQ file replay source for offline analysis.

// TODO: IQ file replay source.
// Will support raw IQ files (interleaved f32 or u8) for offline analysis.

use anyhow::{Result, bail};

use crate::types::IqSample;
use super::Source;

/// IQ file replay source.
#[allow(dead_code)] // not yet implemented
pub struct FileSource {
    path: String,
    sample_rate: f64,
    center_freq: f64,
}

impl FileSource {
    /// Create a new file source from an IQ recording.
    ///
    /// # Errors
    /// Currently always returns an error (not yet implemented).
    #[allow(dead_code)]
    pub fn new(_path: String, _sample_rate: f64, _center_freq: f64) -> Result<Self> {
        bail!("File replay source not yet implemented")
    }
}

impl Source for FileSource {
    fn read_samples(&mut self, _buffer: &mut [IqSample]) -> Result<usize> {
        bail!("File replay not implemented")
    }

    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn center_freq(&self) -> f64 {
        self.center_freq
    }
}
