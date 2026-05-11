// TODO: RTL-SDR hardware source implementation.
// Requires librtlsdr-dev to be installed.
// Will implement the Source trait for real hardware access.

use anyhow::{Result, bail};

use crate::types::IqSample;
use super::Source;

/// RTL-SDR hardware source.
pub struct RtlSdrSource {
    _center_freq: f64,
    _sample_rate: f64,
}

impl RtlSdrSource {
    /// Create a new RTL-SDR source.
    pub fn new(_center_freq: f64, _sample_rate: f64, _gain: Option<f32>, _ppm: i32) -> Result<Self> {
        bail!("RTL-SDR support not yet implemented. Install librtlsdr-dev and enable the rtlsdr feature.")
    }
}

impl Source for RtlSdrSource {
    fn read_samples(&mut self, _buffer: &mut [IqSample]) -> Result<usize> {
        bail!("RTL-SDR not implemented")
    }

    fn sample_rate(&self) -> f64 {
        self._sample_rate
    }

    fn center_freq(&self) -> f64 {
        self._center_freq
    }
}
