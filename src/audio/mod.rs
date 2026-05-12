pub mod demod;
pub mod test_signal;
pub mod cpal_sink;

use crate::types::IqSample;
use demod::AmDemodulator;

// ============================================================================
// Audio Sink Abstraction
// ============================================================================

/// Trait for audio output destinations.
pub trait AudioSink: Send {
    fn write(&mut self, samples: &[f32]) -> anyhow::Result<()>;
    fn sample_rate(&self) -> u32;
}

/// A sink that records all samples for testing.
pub struct NullSink {
    pub samples: Vec<f32>,
    sample_rate: u32,
}

impl NullSink {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            samples: Vec::new(),
            sample_rate,
        }
    }
}

impl AudioSink for NullSink {
    fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
        self.samples.extend_from_slice(samples);
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

// ============================================================================
// Control Messages
// ============================================================================

/// Messages sent to the audio thread to change parameters at runtime.
pub enum AudioControl {
    SetCursorFreq(f64),
    SetChannelBw(f64),
    SetVolume(u8),            // 0-100
    SetSquelch(Option<f32>),  // None = disabled, Some(dB) = threshold
    Mute(bool),
    Shutdown,
}

// ============================================================================
// Audio Thread
// ============================================================================

/// Run the audio processing loop.
///
/// Receives IQ blocks, demodulates AM, applies volume/squelch/mute, and writes to sink.
/// Exits on `Shutdown` or channel disconnect.
pub fn audio_thread(
    iq_rx: crossbeam_channel::Receiver<Vec<IqSample>>,
    control_rx: crossbeam_channel::Receiver<AudioControl>,
    mut sink: Box<dyn AudioSink>,
    center_freq: f64,
    cursor_freq: f64,
    channel_bw: f64,
    input_sample_rate: f64,
) {
    let output_sample_rate = sink.sample_rate() as f64;
    let mut demod = AmDemodulator::new(
        center_freq,
        cursor_freq,
        channel_bw,
        input_sample_rate,
        output_sample_rate,
    );

    let mut volume: f32 = 1.0; // 100%
    let mut muted = false;
    let mut squelch: Option<f32> = None; // dB threshold

    loop {
        // Process all pending control messages (non-blocking)
        loop {
            match control_rx.try_recv() {
                Ok(AudioControl::SetCursorFreq(f)) => demod.set_cursor_freq(f),
                Ok(AudioControl::SetChannelBw(bw)) => demod.set_channel_bw(bw),
                Ok(AudioControl::SetVolume(v)) => volume = v.min(100) as f32 / 100.0,
                Ok(AudioControl::SetSquelch(s)) => squelch = s,
                Ok(AudioControl::Mute(m)) => muted = m,
                Ok(AudioControl::Shutdown) => return,
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => return,
            }
        }

        // Wait for either IQ data or a control message (e.g. Shutdown)
        let iq_block;
        crossbeam_channel::select! {
            recv(iq_rx) -> msg => {
                match msg {
                    Ok(block) => iq_block = block,
                    Err(_) => return,
                }
            }
            recv(control_rx) -> msg => {
                match msg {
                    Ok(AudioControl::Shutdown) => return,
                    Ok(ctrl) => {
                        // Apply the control and loop back
                        match ctrl {
                            AudioControl::SetCursorFreq(f) => demod.set_cursor_freq(f),
                            AudioControl::SetChannelBw(bw) => demod.set_channel_bw(bw),
                            AudioControl::SetVolume(v) => volume = v.min(100) as f32 / 100.0,
                            AudioControl::SetSquelch(s) => squelch = s,
                            AudioControl::Mute(m) => muted = m,
                            AudioControl::Shutdown => unreachable!(),
                        }
                        continue;
                    }
                    Err(_) => return,
                }
            }
        }

        // Demodulate
        let mut audio = demod.process(&iq_block);

        // Apply volume
        for s in &mut audio {
            *s *= volume;
        }

        // Squelch: if enabled, check RMS power
        let silenced = if let Some(threshold_db) = squelch {
            let rms = if audio.is_empty() {
                0.0
            } else {
                let sum_sq: f64 = audio.iter().map(|&s| (s as f64) * (s as f64)).sum();
                (sum_sq / audio.len() as f64).sqrt()
            };
            let power_db = if rms > 1e-20 {
                20.0 * (rms as f32).log10()
            } else {
                -200.0
            };
            power_db < threshold_db
        } else {
            false
        };

        if muted || silenced {
            let silence = vec![0.0f32; audio.len()];
            let _ = sink.write(&silence);
        } else {
            let _ = sink.write(&audio);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::test_signal::generate_am_signal;
    use crossbeam_channel::{bounded, unbounded};
    use std::time::Duration;

    fn make_test_iq(cursor_freq: f64, center_freq: f64, sample_rate: f64, num: usize) -> Vec<IqSample> {
        generate_am_signal(cursor_freq, 1000.0, 0.8, sample_rate, center_freq, num)
    }

    #[test]
    fn test_null_sink_records_samples() {
        let mut sink = NullSink::new(48000);
        sink.write(&[1.0, 2.0, 3.0]).unwrap();
        sink.write(&[4.0, 5.0]).unwrap();
        assert_eq!(sink.samples, vec![1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(sink.sample_rate(), 48000);
    }

    #[test]
    fn test_audio_thread_produces_audio() {
        let sample_rate = 2.048e6;
        let center = 100.0e6;
        let cursor = 100.1e6;
        let bw = 25_000.0;

        let (iq_tx, iq_rx) = bounded(4);
        let (ctrl_tx, ctrl_rx) = unbounded();

        let sink = NullSink::new(48000);
        // We need shared access to read samples after thread finishes.
        use std::sync::{Arc, Mutex};
        let recorded = Arc::new(Mutex::new(Vec::<f32>::new()));
        let recorded2 = recorded.clone();

        struct SharedSink {
            recorded: Arc<Mutex<Vec<f32>>>,
            sr: u32,
        }
        impl AudioSink for SharedSink {
            fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
                self.recorded.lock().unwrap().extend_from_slice(samples);
                Ok(())
            }
            fn sample_rate(&self) -> u32 { self.sr }
        }

        let sink = Box::new(SharedSink { recorded: recorded2, sr: 48000 });

        let handle = std::thread::spawn(move || {
            audio_thread(iq_rx, ctrl_rx, sink, center, cursor, bw, sample_rate);
        });

        // Send IQ data
        let iq = make_test_iq(cursor, center, sample_rate, 8192);
        iq_tx.send(iq).unwrap();

        // Give it time to process, then shutdown
        std::thread::sleep(Duration::from_millis(50));
        ctrl_tx.send(AudioControl::Shutdown).unwrap();
        handle.join().unwrap();

        let samples = recorded.lock().unwrap();
        assert!(!samples.is_empty(), "Audio thread should produce output");
    }

    #[test]
    fn test_audio_thread_volume_scaling() {
        use std::sync::{Arc, Mutex};

        let sample_rate = 2.048e6;
        let center = 100.0e6;
        let cursor = 100.1e6;
        let bw = 25_000.0;

        // Run twice: once at volume 100, once at volume 50
        let mut results = Vec::new();

        for vol in [100u8, 50u8] {
            let (iq_tx, iq_rx) = bounded(4);
            let (ctrl_tx, ctrl_rx) = unbounded();
            let recorded = Arc::new(Mutex::new(Vec::<f32>::new()));
            let recorded2 = recorded.clone();

            struct SharedSink { recorded: Arc<Mutex<Vec<f32>>>, sr: u32 }
            impl AudioSink for SharedSink {
                fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
                    self.recorded.lock().unwrap().extend_from_slice(samples);
                    Ok(())
                }
                fn sample_rate(&self) -> u32 { self.sr }
            }

            // Set volume before sending data
            ctrl_tx.send(AudioControl::SetVolume(vol)).unwrap();

            let sink = Box::new(SharedSink { recorded: recorded2, sr: 48000 });
            let handle = std::thread::spawn(move || {
                audio_thread(iq_rx, ctrl_rx, sink, center, cursor, bw, sample_rate);
            });

            let iq = make_test_iq(cursor, center, sample_rate, 8192);
            iq_tx.send(iq).unwrap();
            std::thread::sleep(Duration::from_millis(50));
            ctrl_tx.send(AudioControl::Shutdown).unwrap();
            handle.join().unwrap();

            let samples = recorded.lock().unwrap();
            let rms: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>()
                / samples.len().max(1) as f64;
            results.push(rms.sqrt());
        }

        // Volume 50 should produce roughly half the amplitude of volume 100
        let ratio = results[1] / results[0];
        assert!(
            ratio > 0.35 && ratio < 0.65,
            "Volume 50 should be ~half of volume 100, ratio = {:.3}",
            ratio
        );
    }

    #[test]
    fn test_audio_thread_mute() {
        use std::sync::{Arc, Mutex};

        let sample_rate = 2.048e6;
        let center = 100.0e6;
        let cursor = 100.1e6;

        let (iq_tx, iq_rx) = bounded(4);
        let (ctrl_tx, ctrl_rx) = unbounded();
        let recorded = Arc::new(Mutex::new(Vec::<f32>::new()));
        let recorded2 = recorded.clone();

        struct SharedSink { recorded: Arc<Mutex<Vec<f32>>>, sr: u32 }
        impl AudioSink for SharedSink {
            fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
                self.recorded.lock().unwrap().extend_from_slice(samples);
                Ok(())
            }
            fn sample_rate(&self) -> u32 { self.sr }
        }

        ctrl_tx.send(AudioControl::Mute(true)).unwrap();

        let sink = Box::new(SharedSink { recorded: recorded2, sr: 48000 });
        let handle = std::thread::spawn(move || {
            audio_thread(iq_rx, ctrl_rx, sink, center, cursor, 25_000.0, sample_rate);
        });

        let iq = make_test_iq(cursor, center, sample_rate, 8192);
        iq_tx.send(iq).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        ctrl_tx.send(AudioControl::Shutdown).unwrap();
        handle.join().unwrap();

        let samples = recorded.lock().unwrap();
        assert!(!samples.is_empty(), "Should still write silence when muted");
        assert!(samples.iter().all(|&s| s == 0.0), "All samples should be zero when muted");
    }

    #[test]
    fn test_audio_thread_squelch_silences_weak_signal() {
        use std::sync::{Arc, Mutex};

        let sample_rate = 2.048e6;
        let center = 100.0e6;
        let cursor = 100.5e6; // Signal at 100.1, cursor at 100.5 → very weak

        let (iq_tx, iq_rx) = bounded(4);
        let (ctrl_tx, ctrl_rx) = unbounded();
        let recorded = Arc::new(Mutex::new(Vec::<f32>::new()));
        let recorded2 = recorded.clone();

        struct SharedSink { recorded: Arc<Mutex<Vec<f32>>>, sr: u32 }
        impl AudioSink for SharedSink {
            fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
                self.recorded.lock().unwrap().extend_from_slice(samples);
                Ok(())
            }
            fn sample_rate(&self) -> u32 { self.sr }
        }

        // Set squelch high enough to silence the weak signal
        ctrl_tx.send(AudioControl::SetSquelch(Some(0.0))).unwrap();

        let sink = Box::new(SharedSink { recorded: recorded2, sr: 48000 });
        let handle = std::thread::spawn(move || {
            audio_thread(iq_rx, ctrl_rx, sink, center, cursor, 25_000.0, sample_rate);
        });

        // Signal at 100.1MHz but cursor at 100.5MHz → out of band, very weak
        let iq = generate_am_signal(100.1e6, 1000.0, 0.8, sample_rate, center, 8192);
        iq_tx.send(iq).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        ctrl_tx.send(AudioControl::Shutdown).unwrap();
        handle.join().unwrap();

        let samples = recorded.lock().unwrap();
        assert!(!samples.is_empty());
        assert!(samples.iter().all(|&s| s == 0.0), "Squelch should silence weak signal");
    }

    #[test]
    fn test_audio_thread_frequency_change_mid_stream() {
        use std::sync::{Arc, Mutex};

        let sample_rate = 2.048e6;
        let center = 100.0e6;

        let (iq_tx, iq_rx) = bounded(4);
        let (ctrl_tx, ctrl_rx) = unbounded();
        let recorded = Arc::new(Mutex::new(Vec::<f32>::new()));
        let recorded2 = recorded.clone();

        struct SharedSink { recorded: Arc<Mutex<Vec<f32>>>, sr: u32 }
        impl AudioSink for SharedSink {
            fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
                self.recorded.lock().unwrap().extend_from_slice(samples);
                Ok(())
            }
            fn sample_rate(&self) -> u32 { self.sr }
        }

        let sink = Box::new(SharedSink { recorded: recorded2, sr: 48000 });
        let handle = std::thread::spawn(move || {
            audio_thread(iq_rx, ctrl_rx, sink, center, 100.1e6, 25_000.0, sample_rate);
        });

        // Send first block
        let iq1 = make_test_iq(100.1e6, center, sample_rate, 8192);
        iq_tx.send(iq1).unwrap();
        std::thread::sleep(Duration::from_millis(20));

        // Change frequency
        ctrl_tx.send(AudioControl::SetCursorFreq(100.2e6)).unwrap();

        // Send second block at new frequency
        let iq2 = make_test_iq(100.2e6, center, sample_rate, 8192);
        iq_tx.send(iq2).unwrap();
        std::thread::sleep(Duration::from_millis(50));

        ctrl_tx.send(AudioControl::Shutdown).unwrap();
        handle.join().unwrap();

        let samples = recorded.lock().unwrap();
        assert!(samples.len() > 100, "Should have audio from both blocks");
    }

    #[test]
    fn test_audio_thread_shutdown() {
        let (iq_tx, iq_rx) = bounded(4);
        let (ctrl_tx, ctrl_rx) = unbounded();

        let sink = Box::new(NullSink::new(48000));
        let handle = std::thread::spawn(move || {
            audio_thread(iq_rx, ctrl_rx, sink, 100.0e6, 100.1e6, 25_000.0, 2.048e6);
        });

        ctrl_tx.send(AudioControl::Shutdown).unwrap();
        // Should exit quickly
        handle.join().unwrap();
    }

    #[test]
    fn test_audio_thread_disconnected_iq_channel() {
        let (iq_tx, iq_rx) = bounded::<Vec<IqSample>>(4);
        let (_ctrl_tx, ctrl_rx) = unbounded();

        let sink = Box::new(NullSink::new(48000));
        let handle = std::thread::spawn(move || {
            audio_thread(iq_rx, ctrl_rx, sink, 100.0e6, 100.1e6, 25_000.0, 2.048e6);
        });

        drop(iq_tx); // Disconnect
        handle.join().unwrap();
    }
}
