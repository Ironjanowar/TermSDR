//! cpal-based audio output sink for real-time playback.

use super::AudioSink;
use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Wrapper to allow `cpal::Stream` to be sent across threads.
/// Safety: `CpalSink` is only ever used from a single thread (the audio thread).
struct SendStream(cpal::Stream);
unsafe impl Send for SendStream {}

/// Audio sink that plays audio through a system audio device using cpal.
pub struct CpalSink {
    buffer: Arc<Mutex<VecDeque<f32>>>,
    _stream: SendStream,
    sample_rate: u32,
}

impl CpalSink {
    /// Create a new CpalSink.
    ///
    /// `device_name` — if `None`, uses the default output device.
    pub fn new(device_name: Option<&str>) -> Result<Self> {
        let host = cpal::default_host();

        let device = if let Some(name) = device_name {
            host.output_devices()
                .context("Failed to enumerate output devices")?
                .find(|d| d.name().map(|n| n == name).unwrap_or(false))
                .context(format!("Audio device '{}' not found", name))?
        } else {
            host.default_output_device()
                .context("No default audio output device available")?
        };

        let supported = device.default_output_config()
            .context("Failed to get default output config")?;

        let sample_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;

        let buffer: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::with_capacity(sample_rate as usize)));
        let buffer_clone = buffer.clone();

        let config = cpal::StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut buf = buffer_clone.lock().unwrap();
                for frame in data.chunks_mut(channels) {
                    let sample = buf.pop_front().unwrap_or(0.0);
                    // Write mono to all channels
                    for s in frame.iter_mut() {
                        *s = sample;
                    }
                }
            },
            |err| {
                eprintln!("cpal stream error: {}", err);
            },
            None,
        ).context("Failed to build output stream")?;

        stream.play().context("Failed to start audio stream")?;

        Ok(Self {
            buffer,
            _stream: SendStream(stream),
            sample_rate,
        })
    }
}

impl AudioSink for CpalSink {
    fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
        let mut buf = self.buffer.lock().unwrap();
        buf.extend(samples);
        // Cap buffer to ~1 second to prevent unbounded growth
        let max = self.sample_rate as usize;
        while buf.len() > max {
            buf.pop_front();
        }
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

/// List all available audio output devices.
pub fn list_output_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let devices: Vec<String> = host
        .output_devices()
        .context("Failed to enumerate output devices")?
        .filter_map(|d| d.name().ok())
        .collect();
    Ok(devices)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_output_devices_does_not_panic() {
        // May return empty list in CI/headless environments
        let result = list_output_devices();
        assert!(result.is_ok(), "list_output_devices should not error: {:?}", result.err());
    }

    #[test]
    #[ignore] // Requires audio hardware
    fn test_cpal_sink_creation() {
        let sink = CpalSink::new(None);
        assert!(sink.is_ok(), "CpalSink creation failed: {:?}", sink.err());
    }

    #[test]
    #[ignore] // Requires audio hardware
    fn test_cpal_sink_write() {
        let mut sink = CpalSink::new(None).unwrap();
        let samples = vec![0.0f32; 4800];
        assert!(sink.write(&samples).is_ok());
    }
}
