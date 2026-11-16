//! Audio pipeline: IQ demodulation, mixing, and output routing.

pub(crate) mod channelizer;
pub(crate) mod demod;
pub mod profile;
#[allow(dead_code)]
pub(crate) mod test_signal;
pub mod cpal_sink;
pub mod wav_sink;
pub mod ws_sink;

use crate::types::IqSample;
use crate::dsp::resampler::{create_decimated_real_resampler, RealResampler};
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

    /// Sample rate, in Hz, that this sink consumes.
    fn sample_rate(&self) -> u32;

    /// Finalize the destination after the last [`AudioSink::write`].
    ///
    /// The audio thread calls this once on its shutdown path, so a sink whose
    /// output needs a closing record — [`wav_sink::WavSink`] patching its chunk
    /// lengths, for instance — can report a failure to the pipeline instead of
    /// only logging it from `Drop`. Sinks with nothing to close keep the default
    /// no-op implementation.
    ///
    /// # Errors
    /// Returns an error if the destination cannot be finalized.
    fn finish(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
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

/// Audio sink that fans every write out to two destinations.
///
/// Used to record with [`wav_sink::WavSink`] while the same samples still reach
/// playback. Both arms are written on every call, even when one of them fails:
/// a dead arm must not blind the other, and the failure is still reported
/// afterwards. When both arms fail, the primary's error is the one returned,
/// because that is the sink the caller was already observing.
pub struct TeeSink {
    /// Sink written first; also the sink whose sample rate is reported.
    primary: Box<dyn AudioSink>,
    /// Sink written second, with identical samples.
    secondary: Box<dyn AudioSink>,
}

impl TeeSink {
    /// Build a fan-out sink from a `primary` and a `secondary` destination.
    ///
    /// Both arms must already agree on the sample rate: the demodulator is built
    /// from the single rate reported by [`AudioSink::sample_rate`].
    #[must_use]
    pub fn new(primary: Box<dyn AudioSink>, secondary: Box<dyn AudioSink>) -> Self {
        Self { primary, secondary }
    }
}

impl AudioSink for TeeSink {
    /// Write `samples` to both arms, then report the first failure.
    ///
    /// # Errors
    /// Returns the primary arm's error when it fails, otherwise the secondary
    /// arm's error. Neither failure stops the other arm from being written.
    fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
        let primary_result = self.primary.write(samples);
        let secondary_result = self.secondary.write(samples);
        primary_result.and(secondary_result)
    }

    fn sample_rate(&self) -> u32 {
        self.primary.sample_rate()
    }

    /// Finalize both arms, then report the first failure.
    ///
    /// # Errors
    /// Returns the primary arm's error when it fails, otherwise the secondary
    /// arm's error. Neither failure stops the other arm from being finalized.
    fn finish(&mut self) -> anyhow::Result<()> {
        let primary_result = self.primary.finish();
        let secondary_result = self.secondary.finish();
        primary_result.and(secondary_result)
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
            let mut demod = Demodulator::new(
                *center_freq,
                freq,
                channel_bw,
                input_sample_rate,
                output_sample_rate,
                mode,
            );
            demod.align_decimation_phase(cursor_demod);
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

/// Demodulate IQ samples, mix outputs, and apply volume and squelch.
fn demod_and_mix(
    iq_block: &[IqSample],
    cursor_demod: &mut Demodulator,
    vfo_demods: &mut HashMap<usize, Demodulator>,
    muted: bool,
    volume: f32,
    squelch: Option<f32>,
) -> Vec<f32> {
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
        audio.fill(0.0);
    }
    audio
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

/// Run the audio processing loop until `Shutdown` or channel disconnect.
///
/// Receives IQ blocks, demodulates AM, applies volume/squelch/mute, and writes to
/// `sink`. Returns only after [`AudioSink::finish`] has been called on the sink,
/// so a recording destination is finalized on the explicit shutdown path rather
/// than left to `Drop`. Processing and finalization failures panic the thread so
/// its join handle reports failure to the pipeline caller.
///
/// # Panics
/// Panics if audio processing or sink finalization fails, or if an internal
/// invariant is violated.
pub fn audio_thread(
    iq_rx: &crossbeam_channel::Receiver<Vec<IqSample>>,
    control_rx: &crossbeam_channel::Receiver<AudioControl>,
    mut sink: Box<dyn AudioSink>,
    center_freq: f64,
    cursor_freq: f64,
    channel_bw: f64,
    input_sample_rate: f64,
) {
    use anyhow::Context as _;

    let pump_result = pump_audio(
        iq_rx,
        control_rx,
        &mut *sink,
        center_freq,
        cursor_freq,
        channel_bw,
        input_sample_rate,
    );
    let finish_result = sink.finish().context("failed to finalize audio sink");
    if let Err(error) = pump_result.and(finish_result) {
        panic!("audio thread failed: {error:#}");
    }
    tracing::debug!("Audio: sink finalized");
}

/// Demodulate and mix IQ blocks into `sink` until shutdown or disconnect.
///
/// Split out of [`audio_thread`] so the caller can finalize the sink exactly once
/// after every exit path of this loop.
#[allow(clippy::too_many_arguments)] // audio thread state is tightly coupled
fn pump_audio(
    iq_rx: &crossbeam_channel::Receiver<Vec<IqSample>>,
    control_rx: &crossbeam_channel::Receiver<AudioControl>,
    sink: &mut dyn AudioSink,
    mut center_freq: f64,
    cursor_freq: f64,
    channel_bw: f64,
    input_sample_rate: f64,
) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let sink_sample_rate = sink.sample_rate();
    if !input_sample_rate.is_finite() || input_sample_rate <= 0.0 || sink_sample_rate == 0 {
        anyhow::bail!(
            "invalid audio sample rates (input={input_sample_rate}, sink={sink_sample_rate})"
        );
    }

    let output_sample_rate = f64::from(sink_sample_rate);
    let mut cursor_demod = Demodulator::new_am(
        center_freq,
        cursor_freq,
        channel_bw,
        input_sample_rate,
        output_sample_rate,
    );

    let rounded_input_rate = input_sample_rate.round();
    if (rounded_input_rate - input_sample_rate).abs() > f64::EPSILON
        || rounded_input_rate > f64::from(u32::MAX)
    {
        anyhow::bail!("RF sample rate {input_sample_rate} Hz is not an exact u32 clock");
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rf_sample_rate = rounded_input_rate as u32;
    let decimation_factor = u32::try_from(cursor_demod.decimation_factor())
        .context("audio decimation factor does not fit u32")?;
    let backend_output_rate = sink_sample_rate
        .checked_mul(decimation_factor)
        .context("audio resampler rate overflow")?;

    let mut resampler = if rf_sample_rate == backend_output_rate {
        None
    } else {
        Some(
            create_decimated_real_resampler(
                rf_sample_rate,
                sink_sample_rate,
                decimation_factor,
            )
            .context("failed to create exact RF-clock audio resampler")?,
        )
    };
    let mut resampled = Vec::new();
    let mut vfo_demods: HashMap<usize, Demodulator> = HashMap::new();
    let mut volume: f32 = 1.0;
    let mut muted = false;
    let mut squelch: Option<f32> = None;
    let mut shutdown_requested = false;

    'audio: loop {
        // Process all pending control messages (non-blocking). Shutdown stops
        // accepting future work but drains IQ blocks already queued.
        if !shutdown_requested {
            loop {
                match control_rx.try_recv() {
                    Ok(ctrl) => {
                        if apply_control(
                            &ctrl,
                            &mut cursor_demod,
                            &mut vfo_demods,
                            &mut volume,
                            &mut squelch,
                            &mut muted,
                            &mut center_freq,
                            input_sample_rate,
                            output_sample_rate,
                        ) {
                            shutdown_requested = true;
                            break;
                        }
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => break,
                }
            }
        }

        let iq_block = if shutdown_requested {
            match iq_rx.try_recv() {
                Ok(block) => block,
                Err(crossbeam_channel::TryRecvError::Empty | crossbeam_channel::TryRecvError::Disconnected) => {
                    break 'audio;
                }
            }
        } else {
            // Wait for either IQ data or a control message (e.g. Shutdown).
            crossbeam_channel::select! {
                recv(iq_rx) -> msg => {
                    match msg {
                        Ok(block) => block,
                        Err(_) => break 'audio,
                    }
                }
                recv(control_rx) -> msg => {
                    match msg {
                        Ok(ctrl) => {
                            if apply_control(&ctrl, &mut cursor_demod, &mut vfo_demods, &mut volume, &mut squelch, &mut muted, &mut center_freq, input_sample_rate, output_sample_rate) {
                                shutdown_requested = true;
                            }
                            continue 'audio;
                        }
                        Err(_) => break 'audio,
                    }
                }
            }
        };

        let mixed = demod_and_mix(
            &iq_block,
            &mut cursor_demod,
            &mut vfo_demods,
            muted,
            volume,
            squelch,
        );
        let samples = if let Some(shared_resampler) = resampler.as_mut() {
            resampled.clear();
            shared_resampler
                .process_with_source_frames(&mixed, iq_block.len(), &mut resampled)
                .context("shared audio resampler processing failed")?;
            &resampled
        } else {
            &mixed
        };
        if !samples.is_empty() {
            sink.write(samples).context("audio sink write failed")?;
        }
    }

    if let Some(shared_resampler) = resampler.as_mut() {
        resampled.clear();
        shared_resampler
            .flush(&mut resampled)
            .context("failed to flush shared audio resampler")?;
        if !resampled.is_empty() {
            sink.write(&resampled).context("final audio sink write failed")?;
        }
    }
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::test_signal::generate_am_signal;
    use crate::audio::wav_sink::WavSink;
    use crossbeam_channel::{bounded, unbounded};
    use std::f64::consts::PI;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    fn make_test_iq(cursor_freq: f64, center_freq: f64, sample_rate: f64, num: usize) -> Vec<IqSample> {
        generate_am_signal(cursor_freq, 1000.0, 0.8, sample_rate, center_freq, num)
    }

    const RATE_CONTRACT_SINK_HZ: u32 = 48_000;

    fn deterministic_am_range(
        sample_rate_hz: u32,
        start_index: usize,
        sample_count: usize,
    ) -> Vec<IqSample> {
        let modulation_hz = 1_000.0;
        (start_index..start_index + sample_count)
            .map(|index| {
                #[allow(clippy::cast_precision_loss)]
                let time = index as f64 / f64::from(sample_rate_hz);
                let envelope = 1.0 + 0.8 * (2.0 * PI * modulation_hz * time).sin();
                // Carrier is at the configured center frequency, so its complex
                // phase is zero and only the deterministic AM envelope varies.
                IqSample::new(envelope as f32, 0.0)
            })
            .collect()
    }

    fn deterministic_am(sample_rate_hz: u32, sample_count: usize) -> Vec<IqSample> {
        deterministic_am_range(sample_rate_hz, 0, sample_count)
    }

    fn irregular_partitions(total: usize) -> Vec<usize> {
        let mut state = 0x6d2b_79f5_u32;
        let mut remaining = total;
        let mut result = Vec::new();
        while remaining > 0 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let requested = 1 + usize::try_from(state % 4096).unwrap();
            let size = requested.min(remaining);
            result.push(size);
            remaining -= size;
        }
        result
    }

    fn run_real_audio_path(
        input_rate_hz: u32,
        iq: &[IqSample],
        partitions: &[usize],
        sink: &mut dyn AudioSink,
    ) {
        let (iq_tx, iq_rx) = unbounded();
        let (_control_tx, control_rx) = unbounded();
        let mut offset = 0;
        for &size in partitions {
            let end = (offset + size).min(iq.len());
            iq_tx.send(iq[offset..end].to_vec()).unwrap();
            offset = end;
            if offset == iq.len() {
                break;
            }
        }
        assert_eq!(offset, iq.len(), "partitions must cover the source exactly");
        drop(iq_tx);
        pump_audio(
            &iq_rx,
            &control_rx,
            sink,
            100_000_000.0,
            100_000_000.0,
            25_000.0,
            f64::from(input_rate_hz),
        )
        .unwrap();
    }

    fn fixed_partitions(total: usize, size: usize) -> Vec<usize> {
        std::iter::repeat(size).take(total.div_ceil(size)).collect()
    }

    fn estimate_tone_hz(samples: &[f32], sample_rate_hz: u32) -> f64 {
        let settled = &samples[samples.len() / 4..];
        let mean = settled.iter().map(|&sample| f64::from(sample)).sum::<f64>()
            / settled.len() as f64;
        let mut best = (0.0, f64::NEG_INFINITY);
        for frequency_hz in 950..=1050 {
            let angular_step = 2.0 * PI * f64::from(frequency_hz) / f64::from(sample_rate_hz);
            let (real, imaginary) = settled.iter().enumerate().fold(
                (0.0, 0.0),
                |(real, imaginary), (index, &sample)| {
                    #[allow(clippy::cast_precision_loss)]
                    let phase = angular_step * index as f64;
                    let centered = f64::from(sample) - mean;
                    (
                        real + centered * phase.cos(),
                        imaginary - centered * phase.sin(),
                    )
                },
            );
            let power = real * real + imaginary * imaginary;
            if power > best.1 {
                best = (f64::from(frequency_hz), power);
            }
        }
        best.0
    }

    struct TempRateWav {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempRateWav {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "termsdr-rate-contract-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join("audio.wav");
            Self { dir, path }
        }
    }

    impl Drop for TempRateWav {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn assert_ten_second_rate_contract(input_rate_hz: u32) {
        let input_count = input_rate_hz as usize * 10;
        let (iq_tx, iq_rx) = bounded(4);
        let (_control_tx, control_rx) = unbounded();
        let mut sink = NullSink::new(RATE_CONTRACT_SINK_HZ);

        std::thread::scope(|scope| {
            scope.spawn(move || {
                let mut offset = 0;
                while offset < input_count {
                    let count = (input_count - offset).min(2048);
                    iq_tx
                        .send(deterministic_am_range(input_rate_hz, offset, count))
                        .unwrap();
                    offset += count;
                }
            });
            pump_audio(
                &iq_rx,
                &control_rx,
                &mut sink,
                100_000_000.0,
                100_000_000.0,
                25_000.0,
                f64::from(input_rate_hz),
            )
            .unwrap();
        });

        assert_eq!(
            sink.samples.len(),
            RATE_CONTRACT_SINK_HZ as usize * 10,
            "{input_rate_hz} Hz input must produce exactly ten seconds at the sink's 48 kHz, independent of 2048-sample capture blocks"
        );
    }

    #[test]
    fn ten_seconds_at_1024000_hz_produces_exact_sink_rate_sample_count() {
        assert_ten_second_rate_contract(1_024_000);
    }

    #[test]
    fn ten_seconds_at_960000_hz_produces_exact_sink_rate_sample_count() {
        assert_ten_second_rate_contract(960_000);
    }

    #[test]
    fn audio_entering_sink_is_partition_invariant() {
        let input_rate_hz = 1_024_000_u32;
        let sample_count = input_rate_hz as usize / 4;
        let iq = deterministic_am(input_rate_hz, sample_count);
        let mut outputs = Vec::new();
        for partitions in [
            fixed_partitions(sample_count, 2048),
            fixed_partitions(sample_count, 4096),
            fixed_partitions(sample_count, 257),
            irregular_partitions(sample_count),
        ] {
            let mut sink = NullSink::new(RATE_CONTRACT_SINK_HZ);
            run_real_audio_path(input_rate_hz, &iq, &partitions, &mut sink);
            outputs.push(sink.samples);
        }

        let reference = &outputs[0];
        for (index, candidate) in outputs[1..].iter().enumerate() {
            assert!(
                reference.len().abs_diff(candidate.len()) <= 1,
                "partition case {} changed sink length: {} versus {}",
                index + 1,
                candidate.len(),
                reference.len()
            );
            let aligned = reference.len().min(candidate.len());
            let rms = reference[..aligned]
                .iter()
                .zip(&candidate[..aligned])
                .map(|(&left, &right)| {
                    let difference = f64::from(left) - f64::from(right);
                    difference * difference
                })
                .sum::<f64>()
                / aligned as f64;
            let rms = rms
                .sqrt();
            assert!(rms <= 1.0e-5, "partition case {} RMS {rms:.3e} exceeds 1e-5", index + 1);
        }
    }

    #[test]
    fn one_kilohertz_am_tone_reaches_48khz_sink_within_one_hertz() {
        let input_rate_hz = 1_024_000_u32;
        let sample_count = input_rate_hz as usize / 2;
        let iq = deterministic_am(input_rate_hz, sample_count);
        let mut sink = NullSink::new(RATE_CONTRACT_SINK_HZ);
        run_real_audio_path(
            input_rate_hz,
            &iq,
            &fixed_partitions(sample_count, 2048),
            &mut sink,
        );

        let measured_hz = estimate_tone_hz(&sink.samples, RATE_CONTRACT_SINK_HZ);
        assert!(
            (measured_hz - 1_000.0).abs() <= 1.0,
            "1 kHz source measured {measured_hz:.1} Hz when interpreted at the sink's declared 48 kHz"
        );
    }

    #[test]
    fn wav_header_duration_matches_rf_source_duration_for_multiple_partitions() {
        let input_rate_hz = 1_024_000_u32;
        let source_duration_seconds = 0.25_f64;
        let rf_sample_count = input_rate_hz as usize / 4;
        let iq = deterministic_am(input_rate_hz, rf_sample_count);

        let ideal_output_numerator = u64::try_from(rf_sample_count)
            .unwrap()
            .checked_mul(u64::from(RATE_CONTRACT_SINK_HZ))
            .unwrap();
        assert_eq!(
            ideal_output_numerator % u64::from(input_rate_hz),
            0,
            "this fixture's exact RF-to-sink duration must be an integer sample count"
        );
        let ideal_output_samples = ideal_output_numerator
            .checked_div(u64::from(input_rate_hz))
            .unwrap();

        for partitions in [
            fixed_partitions(rf_sample_count, 2048),
            fixed_partitions(rf_sample_count, 4096),
            irregular_partitions(rf_sample_count),
        ] {
            let temp = TempRateWav::new();
            let mut sink = WavSink::new(&temp.path, RATE_CONTRACT_SINK_HZ).unwrap();
            run_real_audio_path(input_rate_hz, &iq, &partitions, &mut sink);
            sink.finish().unwrap();

            let bytes = fs::read(&temp.path).unwrap();
            let declared_rate = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
            let data_bytes = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
            let actual_output_samples =
                u64::from(data_bytes) / u64::try_from(std::mem::size_of::<f32>()).unwrap();
            let declared_duration = actual_output_samples as f64 / f64::from(declared_rate);
            assert_eq!(declared_rate, RATE_CONTRACT_SINK_HZ);
            assert!(
                actual_output_samples.abs_diff(ideal_output_samples) <= 1,
                "WAV duration {declared_duration:.9}s ({actual_output_samples} samples) must match RF duration {source_duration_seconds:.9}s ({ideal_output_samples} ideal samples) within one output sample"
            );
        }
    }

    #[test]
    fn one_rf_block_uses_the_exact_rf_clock_at_eof() {
        let input_rate_hz = 1_024_000_u32;
        let rf_sample_count = 2_048_usize;
        let iq = deterministic_am(input_rate_hz, rf_sample_count);
        let mut sink = NullSink::new(RATE_CONTRACT_SINK_HZ);

        run_real_audio_path(input_rate_hz, &iq, &[rf_sample_count], &mut sink);

        let expected = rf_sample_count
            .checked_mul(RATE_CONTRACT_SINK_HZ as usize)
            .unwrap()
            .div_ceil(input_rate_hz as usize);
        assert_eq!(expected, 96, "fixture must expose the fractional RF clock");
        assert_eq!(
            sink.samples.len(),
            expected,
            "EOF length must be ceil(total RF samples * sink rate / exact RF rate), not a conversion through a rounded post-decimation rate"
        );
    }

    #[test]
    fn exact_rf_clock_length_is_invariant_to_arbitrary_partitions() {
        let input_rate_hz = 1_024_000_u32;
        let rf_sample_count = 204_800_usize;
        let iq = deterministic_am(input_rate_hz, rf_sample_count);
        let expected = rf_sample_count
            .checked_mul(RATE_CONTRACT_SINK_HZ as usize)
            .unwrap()
            .div_ceil(input_rate_hz as usize);
        assert_eq!(expected, 9_600, "fixture must render exactly 200 ms");

        for (name, partitions) in [
            ("single", vec![rf_sample_count]),
            ("capture-sized", fixed_partitions(rf_sample_count, 2_048)),
            ("irregular", irregular_partitions(rf_sample_count)),
        ] {
            let mut sink = NullSink::new(RATE_CONTRACT_SINK_HZ);
            run_real_audio_path(input_rate_hz, &iq, &partitions, &mut sink);
            assert_eq!(
                sink.samples.len(),
                expected,
                "{name} partitions changed exact RF-clock duration"
            );
        }
    }

    #[test]
    fn shutdown_drains_iq_blocks_already_accepted_by_audio() {
        use std::sync::{Arc, Mutex};

        struct SharedSink {
            samples: Arc<Mutex<Vec<f32>>>,
        }
        impl AudioSink for SharedSink {
            fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
                self.samples.lock().unwrap().extend_from_slice(samples);
                Ok(())
            }
            fn sample_rate(&self) -> u32 {
                RATE_CONTRACT_SINK_HZ
            }
        }

        let blocks = [17_usize, 31, 5];
        let expected = blocks.iter().sum::<usize>();
        let (iq_tx, iq_rx) = unbounded();
        let (control_tx, control_rx) = unbounded();
        for &len in &blocks {
            iq_tx
                .send(deterministic_am(RATE_CONTRACT_SINK_HZ, len))
                .unwrap();
        }
        control_tx.send(AudioControl::Shutdown).unwrap();
        drop(iq_tx);
        drop(control_tx);

        let samples = Arc::new(Mutex::new(Vec::new()));
        audio_thread(
            &iq_rx,
            &control_rx,
            Box::new(SharedSink {
                samples: samples.clone(),
            }),
            100_000_000.0,
            100_000_000.0,
            25_000.0,
            f64::from(RATE_CONTRACT_SINK_HZ),
        );

        assert_eq!(
            samples.lock().unwrap().len(),
            expected,
            "Shutdown must drain all IQ blocks accepted before the command"
        );
    }

    #[test]
    fn vfo_added_mid_decimation_cycle_stays_on_the_cursor_phase() {
        let input_rate_hz = 1_024_000.0;
        let output_rate_hz = 48_000.0;
        let center_freq = 100_000_000.0;
        let mut cursor = Demodulator::new_am(
            center_freq,
            center_freq,
            25_000.0,
            input_rate_hz,
            output_rate_hz,
        );
        let mut vfos = HashMap::new();
        let mut volume = 1.0;
        let mut squelch = None;
        let mut muted = false;
        let mut center = center_freq;

        // 1,024,000 / 48,000 rounds to a decimation factor of 21. Seven RF
        // samples therefore leave the cursor seven samples into that cycle.
        let prefix = deterministic_am_range(1_024_000, 0, 7);
        assert_eq!(cursor.process(&prefix).len(), 1);
        apply_control(
            &AudioControl::AddVfo {
                id: 1,
                freq: center_freq,
                channel_bw: 25_000.0,
                mode: DemodMode::AM,
            },
            &mut cursor,
            &mut vfos,
            &mut volume,
            &mut squelch,
            &mut muted,
            &mut center,
            input_rate_hz,
            output_rate_hz,
        );

        // Fourteen samples finish the cursor's current cycle but do not contain
        // another selected sample. A newly added VFO must share that phase; if it
        // restarts at phase zero it incorrectly emits one sample here.
        let next = deterministic_am_range(1_024_000, prefix.len(), 14);
        let cursor_audio = cursor.process(&next);
        let vfo_audio = vfos.get_mut(&1).unwrap().process(&next);
        assert_eq!(
            vfo_audio.len(),
            cursor_audio.len(),
            "new VFO restarted decimation at the block boundary instead of inheriting the cursor phase"
        );
        assert_eq!(
            vfo_audio, cursor_audio,
            "same-frequency VFO must select the same globally aligned RF samples as the cursor"
        );
    }

    struct PipelineFailingSink {
        fail_write: bool,
        fail_finish: bool,
        sample_rate: u32,
    }

    impl AudioSink for PipelineFailingSink {
        fn write(&mut self, _samples: &[f32]) -> anyhow::Result<()> {
            if self.fail_write {
                anyhow::bail!("injected sink write failure");
            }
            Ok(())
        }

        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        fn finish(&mut self) -> anyhow::Result<()> {
            if self.fail_finish {
                anyhow::bail!("injected sink finish failure");
            }
            Ok(())
        }
    }

    fn spawn_failing_audio_sink(fail_write: bool, fail_finish: bool) -> std::thread::Result<()> {
        let (iq_tx, iq_rx) = unbounded();
        let (_control_tx, control_rx) = unbounded();
        iq_tx
            .send(deterministic_am(RATE_CONTRACT_SINK_HZ, 64))
            .unwrap();
        drop(iq_tx);
        std::thread::spawn(move || {
            audio_thread(
                &iq_rx,
                &control_rx,
                Box::new(PipelineFailingSink {
                    fail_write,
                    fail_finish,
                    sample_rate: RATE_CONTRACT_SINK_HZ,
                }),
                100_000_000.0,
                100_000_000.0,
                25_000.0,
                f64::from(RATE_CONTRACT_SINK_HZ),
            );
        })
        .join()
    }

    #[test]
    fn sink_write_error_makes_the_audio_thread_unsuccessful() {
        assert!(
            spawn_failing_audio_sink(true, false).is_err(),
            "audio_thread returned success after AudioSink::write failed; the failure must propagate to the pipeline"
        );
    }

    #[test]
    fn sink_finish_error_makes_the_audio_thread_unsuccessful() {
        assert!(
            spawn_failing_audio_sink(false, true).is_err(),
            "audio_thread returned success after AudioSink::finish failed; the failure must propagate to the pipeline"
        );
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
        let (_iq_tx, iq_rx) = bounded(4);
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
        let (_iq_tx, iq_rx) = bounded(4);
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

    // ========================================================================
    // TeeSink fan-out (plan Task 4)
    // ========================================================================
    //
    // Frozen contract:
    //
    // ```text
    // pub struct TeeSink { .. }
    //
    // impl TeeSink {
    //     pub fn new(primary: Box<dyn AudioSink>, secondary: Box<dyn AudioSink>) -> Self;
    // }
    //
    // impl AudioSink for TeeSink {
    //     fn write(&mut self, samples: &[f32]) -> anyhow::Result<()>;
    //     fn sample_rate(&self) -> u32;
    // }
    // ```
    //
    // Decisions frozen here where the plan left a choice:
    //
    // * Both sinks receive every write, in call order, with identical sample
    //   values and no truncation to the shorter sink.
    // * Every sink error propagates. A failure from *either* arm is returned to
    //   the caller instead of being swallowed. The audio thread currently ignores
    //   sink errors, but a `TeeSink` that hid a broken WAV export would produce a
    //   silently truncated recording — exactly what `--wav-output` exists to rule
    //   out. `TeeSink` reports the error and lets the caller decide; when both
    //   arms fail, the primary's error is the one returned, because that is the
    //   sink the caller was already observing before the fan-out existed.
    // * A failing arm does not stop the other arm from being written. The fan-out
    //   is best-effort per write so that one dead sink cannot blind the other, and
    //   the error is still reported afterwards.
    // * `sample_rate` reports the primary sink's rate. Both arms must already
    //   agree, because the demodulator is built from a single rate; taking the
    //   primary's value is a choice of which arm to believe, not a conversion.

    /// Sink that appends every write to a shared buffer, for fan-out assertions.
    struct RecordingSink {
        recorded: std::sync::Arc<std::sync::Mutex<Vec<f32>>>,
        sample_rate: u32,
    }

    impl RecordingSink {
        fn new(sample_rate: u32) -> (Self, std::sync::Arc<std::sync::Mutex<Vec<f32>>>) {
            let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            (
                Self {
                    recorded: recorded.clone(),
                    sample_rate,
                },
                recorded,
            )
        }
    }

    impl AudioSink for RecordingSink {
        fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
            self.recorded.lock().unwrap().extend_from_slice(samples);
            Ok(())
        }

        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
    }

    /// Sink that records its writes and then fails, to prove errors propagate.
    struct FailingSink {
        recorded: std::sync::Arc<std::sync::Mutex<Vec<f32>>>,
        sample_rate: u32,
    }

    impl FailingSink {
        fn new(sample_rate: u32) -> (Self, std::sync::Arc<std::sync::Mutex<Vec<f32>>>) {
            let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            (
                Self {
                    recorded: recorded.clone(),
                    sample_rate,
                },
                recorded,
            )
        }
    }

    impl AudioSink for FailingSink {
        fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
            self.recorded.lock().unwrap().extend_from_slice(samples);
            anyhow::bail!("sink is closed")
        }

        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
    }

    #[test]
    fn tee_sink_writes_same_samples_to_both_sinks_in_order() {
        let (primary, primary_buf) = RecordingSink::new(48_000);
        let (secondary, secondary_buf) = RecordingSink::new(48_000);
        let mut tee = TeeSink::new(Box::new(primary), Box::new(secondary));

        tee.write(&[0.1, -0.2, 0.3]).unwrap();
        tee.write(&[0.4]).unwrap();
        tee.write(&[-0.5, 0.6]).unwrap();

        let expected = [0.1f32, -0.2, 0.3, 0.4, -0.5, 0.6];
        for (name, buf) in [("primary", &primary_buf), ("secondary", &secondary_buf)] {
            let recorded = buf.lock().unwrap();
            assert_eq!(
                recorded.len(),
                expected.len(),
                "{name} sink must receive every sample"
            );
            for (index, (written, wanted)) in recorded.iter().zip(expected.iter()).enumerate() {
                assert_eq!(
                    written.to_bits(),
                    wanted.to_bits(),
                    "{name} sink sample {index} must match what was written"
                );
            }
        }
    }

    #[test]
    fn tee_sink_propagates_secondary_error() {
        let (primary, primary_buf) = RecordingSink::new(48_000);
        let (secondary, secondary_buf) = FailingSink::new(48_000);
        let mut tee = TeeSink::new(Box::new(primary), Box::new(secondary));

        let result = tee.write(&[0.1, 0.2]);
        assert!(
            result.is_err(),
            "a secondary sink failure must reach the caller, not be swallowed"
        );

        assert_eq!(
            primary_buf.lock().unwrap().len(),
            2,
            "the healthy arm must still have been written"
        );
        assert_eq!(
            secondary_buf.lock().unwrap().len(),
            2,
            "the arm that failed must have been offered the samples"
        );
    }

    #[test]
    fn tee_sink_propagates_primary_error() {
        let (primary, primary_buf) = FailingSink::new(48_000);
        let (secondary, secondary_buf) = RecordingSink::new(48_000);
        let mut tee = TeeSink::new(Box::new(primary), Box::new(secondary));

        let result = tee.write(&[0.1, 0.2]);
        assert!(
            result.is_err(),
            "a primary sink failure must reach the caller, not be swallowed"
        );

        assert_eq!(primary_buf.lock().unwrap().len(), 2);
        assert_eq!(
            secondary_buf.lock().unwrap().len(),
            2,
            "a failing primary must not stop the secondary from being written"
        );
    }

    #[test]
    fn tee_sink_reports_primary_sample_rate() {
        let (primary, _primary_buf) = RecordingSink::new(22_050);
        let (secondary, _secondary_buf) = RecordingSink::new(22_050);
        let tee = TeeSink::new(Box::new(primary), Box::new(secondary));
        assert_eq!(
            tee.sample_rate(),
            22_050,
            "TeeSink must report the rate the demodulator is built from"
        );
    }

    #[test]
    fn tee_sink_accepts_empty_write_without_error() {
        let (primary, primary_buf) = RecordingSink::new(48_000);
        let (secondary, secondary_buf) = RecordingSink::new(48_000);
        let mut tee = TeeSink::new(Box::new(primary), Box::new(secondary));

        tee.write(&[]).unwrap();
        assert!(primary_buf.lock().unwrap().is_empty());
        assert!(secondary_buf.lock().unwrap().is_empty());
    }

    #[test]
    fn tee_sink_is_send_and_usable_as_boxed_audio_sink() {
        fn assert_send<T: Send>() {}
        assert_send::<TeeSink>();

        let (primary, primary_buf) = RecordingSink::new(48_000);
        let (secondary, secondary_buf) = RecordingSink::new(48_000);
        let mut boxed: Box<dyn AudioSink> =
            Box::new(TeeSink::new(Box::new(primary), Box::new(secondary)));

        boxed.write(&[0.25, -0.75]).unwrap();
        assert_eq!(boxed.sample_rate(), 48_000);
        assert_eq!(primary_buf.lock().unwrap().len(), 2);
        assert_eq!(secondary_buf.lock().unwrap().len(), 2);
    }
}
