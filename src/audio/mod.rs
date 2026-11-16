//! Audio pipeline: IQ demodulation, mixing, and output routing.

pub(crate) mod demod;
pub mod profile;
#[allow(dead_code)]
pub(crate) mod test_signal;
pub mod cpal_sink;
pub mod ws_sink;

use crate::types::IqSample;
use demod::{Demodulator, DemodMode};

/// dB floor value for silence/zero-power audio.
const AUDIO_DB_FLOOR: f32 = -200.0;

// ============================================================================
// Audio Sink Abstraction
// ============================================================================

/// Trait for audio output destinations.
pub trait AudioSink: Send {
    /// Write audio samples to the output.
    ///
    /// # Errors
    /// Returns an error if the samples cannot be written to the audio output.
    fn write(&mut self, samples: &[f32]) -> anyhow::Result<()>;
    fn sample_rate(&self) -> u32;
}

/// A sink that records all samples for testing.
pub struct NullSink {
    pub samples: Vec<f32>,
    sample_rate: u32,
}

impl NullSink {
    #[must_use] 
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
#[derive(Debug, Clone)]
pub enum AudioControl {
    SetCursorFreq(f64),
    SetCenterFreq(f64),
    SetChannelBw(f64),
    SetVolume(u8),            // 0-100
    SetSquelch(Option<f32>),  // None = disabled, Some(dB) = threshold
    Mute(bool),
    AddVfo { id: usize, freq: f64, channel_bw: f64, mode: DemodMode },
    RemoveVfo(usize),
    SetDemodMode(DemodMode),               // for cursor (id=0)
    SetVfoDemodMode { id: usize, mode: DemodMode },  // for specific VFO
    SetAgcEnabled(bool),
    SetAgcAttack(f64),   // ms
    SetAgcDecay(f64),    // ms
    SetAgcTarget(f32),
    SetAgcMaxGain(f32),  // dB
    SetBandpassEnabled(bool),
    SetBandpassLowCut(f64),   // Hz
    SetBandpassHighCut(f64),  // Hz
    Shutdown,
}

use std::collections::HashMap;

// ============================================================================
// Audio Thread
// ============================================================================

/// Apply a control message to the audio thread state.
/// Returns `true` if the thread should shut down.
#[allow(clippy::too_many_arguments)] // audio thread state is tightly coupled
fn apply_control(
    ctrl: &AudioControl,
    cursor_demod: &mut Demodulator,
    vfo_demods: &mut HashMap<usize, Demodulator>,
    volume: &mut f32,
    squelch: &mut Option<f32>,
    muted: &mut bool,
    center_freq: &mut f64,
    input_sample_rate: f64,
    output_sample_rate: f64,
) -> bool {
    match *ctrl {
        AudioControl::SetCursorFreq(f) => cursor_demod.set_cursor_freq(f),
        AudioControl::SetCenterFreq(f) => {
            *center_freq = f;
            cursor_demod.set_center_freq(f);
            for demod in vfo_demods.values_mut() {
                demod.set_center_freq(f);
            }
        }
        AudioControl::SetChannelBw(bw) => cursor_demod.set_channel_bw(bw),
        AudioControl::SetVolume(v) => *volume = f32::from(v.min(100)) / 100.0,
        AudioControl::SetSquelch(ref s) => *squelch = *s,
        AudioControl::Mute(m) => *muted = m,
        AudioControl::AddVfo { id, freq, channel_bw, mode } => {
            let demod = Demodulator::new(
                *center_freq,
                freq,
                channel_bw,
                input_sample_rate,
                output_sample_rate,
                mode,
            );
            vfo_demods.insert(id, demod);
        }
        AudioControl::RemoveVfo(id) => {
            vfo_demods.remove(&id);
        }
        AudioControl::SetDemodMode(mode) => {
            cursor_demod.set_mode(mode);
        }
        AudioControl::SetVfoDemodMode { id, mode } => {
            if let Some(demod) = vfo_demods.get_mut(&id) {
                demod.set_mode(mode);
            }
        }
        AudioControl::SetAgcEnabled(enabled) => {
            cursor_demod.set_agc_enabled(enabled);
            for demod in vfo_demods.values_mut() {
                demod.set_agc_enabled(enabled);
            }
        }
        AudioControl::SetAgcAttack(ms) => {
            cursor_demod.set_agc_attack(ms);
            for demod in vfo_demods.values_mut() {
                demod.set_agc_attack(ms);
            }
        }
        AudioControl::SetAgcDecay(ms) => {
            cursor_demod.set_agc_decay(ms);
            for demod in vfo_demods.values_mut() {
                demod.set_agc_decay(ms);
            }
        }
        AudioControl::SetAgcTarget(level) => {
            cursor_demod.set_agc_target(level);
            for demod in vfo_demods.values_mut() {
                demod.set_agc_target(level);
            }
        }
        AudioControl::SetAgcMaxGain(db) => {
            cursor_demod.set_agc_max_gain(db);
            for demod in vfo_demods.values_mut() {
                demod.set_agc_max_gain(db);
            }
        }
        AudioControl::SetBandpassEnabled(enabled) => {
            cursor_demod.set_bandpass_enabled(enabled);
            for demod in vfo_demods.values_mut() {
                demod.set_bandpass_enabled(enabled);
            }
        }
        AudioControl::SetBandpassLowCut(hz) => {
            cursor_demod.set_bandpass_low_cut(hz);
            for demod in vfo_demods.values_mut() {
                demod.set_bandpass_low_cut(hz);
            }
        }
        AudioControl::SetBandpassHighCut(hz) => {
            cursor_demod.set_bandpass_high_cut(hz);
            for demod in vfo_demods.values_mut() {
                demod.set_bandpass_high_cut(hz);
            }
        }
        AudioControl::Shutdown => return true,
    }
    false
}

/// Demodulate IQ samples, mix outputs, apply volume and squelch, write to sink.
fn demod_mix_and_output(
    iq_block: &[IqSample],
    cursor_demod: &mut Demodulator,
    vfo_demods: &mut HashMap<usize, Demodulator>,
    muted: bool,
    volume: f32,
    squelch: Option<f32>,
    sink: &mut dyn AudioSink,
) {
    // Collect outputs from all active demodulators with their pre-AGC levels
    let mut outputs: Vec<(Vec<f32>, f32)> = Vec::new();

    if !muted {
        let audio = cursor_demod.process(iq_block);
        let rms = cursor_demod.pre_agc_rms();
        outputs.push((audio, rms));
    }

    for demod in vfo_demods.values_mut() {
        let audio = demod.process(iq_block);
        let rms = demod.pre_agc_rms();
        outputs.push((audio, rms));
    }

    let mut audio = mix_outputs(outputs, cursor_demod, iq_block);

    // Apply volume
    for s in &mut audio {
        *s *= volume;
    }

    // Squelch
    if check_squelch(&audio, squelch) {
        let silence = vec![0.0f32; audio.len()];
        let _ = sink.write(&silence);
    } else {
        let _ = sink.write(&audio);
    }
}

/// Mix demodulator outputs using power-weighted mixing.
fn mix_outputs(
    outputs: Vec<(Vec<f32>, f32)>,
    cursor_demod: &mut Demodulator,
    iq_block: &[IqSample],
) -> Vec<f32> {
    let num_active = outputs.len();
    if num_active == 0 {
        let silent = cursor_demod.process(iq_block);
        vec![0.0f32; silent.len()]
    } else if num_active == 1 {
        outputs.into_iter().next().expect("invariant: num_active == 1").0
    } else {
        let max_len = outputs.iter().map(|o| o.0.len()).max().unwrap_or(0);
        let mut mixed = vec![0.0f32; max_len];
        let total_rms: f32 = outputs.iter().map(|o| o.1).sum();

        if total_rms > 1e-10 {
            for (output, rms) in &outputs {
                let weight = rms / total_rms;
                for (i, &s) in output.iter().enumerate() {
                    mixed[i] += s * weight;
                }
            }
        } else {
            for (output, _) in &outputs {
                for (i, &s) in output.iter().enumerate() {
                    mixed[i] += s;
                }
            }
            #[allow(clippy::cast_precision_loss)]
            let divisor = num_active as f32;
            for s in &mut mixed {
                *s /= divisor;
            }
        }
        mixed
    }
}

/// Check if audio should be silenced by squelch.
fn check_squelch(audio: &[f32], squelch: Option<f32>) -> bool {
    if let Some(threshold_db) = squelch {
        #[allow(clippy::cast_precision_loss)]
        let rms = if audio.is_empty() {
            0.0
        } else {
            let sum_sq: f64 = audio.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
            (sum_sq / audio.len() as f64).sqrt()
        };
        #[allow(clippy::cast_possible_truncation)]
        let power_db = if rms > 1e-20 {
            20.0 * (rms as f32).log10()
        } else {
            AUDIO_DB_FLOOR
        };
        power_db < threshold_db
    } else {
        false
    }
}

/// Run the audio processing loop.
///
/// Receives IQ blocks, demodulates AM, applies volume/squelch/mute, and writes to sink.
/// Exits on `Shutdown` or channel disconnect.
///
/// # Panics
/// Panics if internal invariants are violated (e.g., empty demodulator output).
pub fn audio_thread(
    iq_rx: &crossbeam_channel::Receiver<Vec<IqSample>>,
    control_rx: &crossbeam_channel::Receiver<AudioControl>,
    mut sink: Box<dyn AudioSink>,
    mut center_freq: f64,
    cursor_freq: f64,
    channel_bw: f64,
    input_sample_rate: f64,
) {
    let output_sample_rate = f64::from(sink.sample_rate());
    let mut cursor_demod = Demodulator::new_am(
        center_freq,
        cursor_freq,
        channel_bw,
        input_sample_rate,
        output_sample_rate,
    );

    let mut vfo_demods: HashMap<usize, Demodulator> = HashMap::new();
    let mut volume: f32 = 1.0;
    let mut muted = false;
    let mut squelch: Option<f32> = None;

    loop {
        // Process all pending control messages (non-blocking)
        loop {
            match control_rx.try_recv() {
                Ok(ctrl) => {
                    if apply_control(&ctrl, &mut cursor_demod, &mut vfo_demods, &mut volume, &mut squelch, &mut muted, &mut center_freq, input_sample_rate, output_sample_rate) {
                        return;
                    }
                }
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
                    Ok(ctrl) => {
                        if apply_control(&ctrl, &mut cursor_demod, &mut vfo_demods, &mut volume, &mut squelch, &mut muted, &mut center_freq, input_sample_rate, output_sample_rate) {
                            return;
                        }
                        continue;
                    }
                    Err(_) => return,
                }
            }
        }

        demod_mix_and_output(&iq_block, &mut cursor_demod, &mut vfo_demods, muted, volume, squelch, &mut *sink);
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
            audio_thread(&iq_rx, &ctrl_rx, sink, center, cursor, bw, sample_rate);
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
                audio_thread(&iq_rx, &ctrl_rx, sink, center, cursor, bw, sample_rate);
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
            audio_thread(&iq_rx, &ctrl_rx, sink, center, cursor, 25_000.0, sample_rate);
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
            audio_thread(&iq_rx, &ctrl_rx, sink, center, cursor, 25_000.0, sample_rate);
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
            audio_thread(&iq_rx, &ctrl_rx, sink, center, 100.1e6, 25_000.0, sample_rate);
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
            audio_thread(&iq_rx, &ctrl_rx, sink, 100.0e6, 100.1e6, 25_000.0, 2.048e6);
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
            audio_thread(&iq_rx, &ctrl_rx, sink, 100.0e6, 100.1e6, 25_000.0, 2.048e6);
        });

        drop(iq_tx); // Disconnect
        handle.join().unwrap();
    }

    #[test]
    fn test_audio_thread_two_vfos_mix() {
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

        // Add a VFO
        ctrl_tx.send(AudioControl::AddVfo { id: 1, freq: 100.2e6, channel_bw: 25_000.0, mode: DemodMode::AM }).unwrap();

        let sink = Box::new(SharedSink { recorded: recorded2, sr: 48000 });
        let handle = std::thread::spawn(move || {
            audio_thread(&iq_rx, &ctrl_rx, sink, center, cursor, 25_000.0, sample_rate);
        });

        let iq = make_test_iq(cursor, center, sample_rate, 8192);
        iq_tx.send(iq).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        ctrl_tx.send(AudioControl::Shutdown).unwrap();
        handle.join().unwrap();

        let samples = recorded.lock().unwrap();
        assert!(!samples.is_empty(), "Should produce mixed audio from cursor + VFO");
    }

    #[test]
    fn test_audio_thread_muted_cursor_vfo_plays() {
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

        // Mute cursor, add VFO at signal frequency
        ctrl_tx.send(AudioControl::Mute(true)).unwrap();
        ctrl_tx.send(AudioControl::AddVfo { id: 1, freq: cursor, channel_bw: 25_000.0, mode: DemodMode::AM }).unwrap();

        let sink = Box::new(SharedSink { recorded: recorded2, sr: 48000 });
        let handle = std::thread::spawn(move || {
            audio_thread(&iq_rx, &ctrl_rx, sink, center, cursor, 25_000.0, sample_rate);
        });

        let iq = make_test_iq(cursor, center, sample_rate, 8192);
        iq_tx.send(iq).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        ctrl_tx.send(AudioControl::Shutdown).unwrap();
        handle.join().unwrap();

        let samples = recorded.lock().unwrap();
        assert!(!samples.is_empty());
        // VFO should produce non-silent audio even though cursor is muted
        let has_audio = samples.iter().any(|&s| s.abs() > 0.001);
        assert!(has_audio, "VFO should produce audio when cursor is muted");
    }

    #[test]
    fn test_audio_thread_remove_vfo() {
        let (iq_tx, iq_rx) = bounded(4);
        let (ctrl_tx, ctrl_rx) = unbounded();

        ctrl_tx.send(AudioControl::AddVfo { id: 1, freq: 100.1e6, channel_bw: 25_000.0, mode: DemodMode::AM }).unwrap();
        ctrl_tx.send(AudioControl::RemoveVfo(1)).unwrap();

        let sink = Box::new(NullSink::new(48000));
        let handle = std::thread::spawn(move || {
            audio_thread(&iq_rx, &ctrl_rx, sink, 100.0e6, 100.1e6, 25_000.0, 2.048e6);
        });

        ctrl_tx.send(AudioControl::Shutdown).unwrap();
        handle.join().unwrap();
    }
}
