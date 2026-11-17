//! Standalone aviation-AM demodulator contract.
//!
//! This module owns the complete AM-only stateful chain:
//!
//! ```text
//! AmChannelizer
//! → channel-power SquelchState
//! → complex magnitude (AM envelope)
//! → stateful DC blocker
//! → selected voice band-pass
//! → SpeechAgc (frozen while squelched)
//! → SoftLimiter
//! ```
//!
//! It is wired into `audio::pump_audio` for `DemodMode::AM` only; legacy AM/FM/NFM routing stays
//! untouched, and a stream that mixes AM with another mode falls back to the legacy demodulator.
//! All frequencies and rates are hertz, powers and gains are decibels, and
//! [`AmDemodulator::latency_samples`] is expressed at the output rate.
//!
//! # Streaming contract
//!
//! One instance owns every stage's history. Ordinary input partitions, empty calls, and retunes do
//! not reset the resampler, NCO, filters, squelch, or AGC. Empty channelizer output produces empty
//! audio and does not advance squelch time. Profile changes are transactional: every replacement
//! filter is designed before any live state is changed. Invalid input, tuning, or profile changes
//! return `Err` without poisoning the prior usable state.

// Frozen reviewer tests intentionally use assertion wording and one long end-to-end timing fixture
// that pedantic Clippy would otherwise ask to rewrite.
#![cfg_attr(test, allow(clippy::assert_is_empty, clippy::too_many_lines))]

use anyhow::{bail, Context, Result};

use crate::{
    audio::{
        agc::{
            SpeechAgc, DEFAULT_ATTACK_MS, DEFAULT_MAX_GAIN_DB, DEFAULT_MIN_GAIN_DB,
            DEFAULT_RELEASE_MS, DEFAULT_TARGET_RMS,
        },
        channelizer::AmChannelizer,
        limiter::SoftLimiter,
        profile::AmAudioProfile,
        squelch::{ChannelPowerSample, SquelchState, MAX_BLOCK_FRAMES},
    },
    types::IqSample,
};

/// One block of demodulated AM audio and its post-block diagnostics.
#[derive(Debug, Clone)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "diagnostic fields are reported to callers that do not yet log them"
    )
)]
pub(crate) struct AmAudioBlock {
    /// Mono audio at the configured output rate, in linear full scale and bounded to `[-1, 1]`.
    pub samples: Vec<f32>,
    /// Mean pre-demodulation channel power for this channelized block, in dBFS.
    pub channel_power_dbfs: f32,
    /// Squelch noise-floor estimate after this block, in dBFS.
    pub noise_floor_dbfs: f32,
    /// Effective squelch opening threshold after this block, in dBFS.
    pub squelch_threshold_dbfs: f32,
    /// Whether the squelch is open at the end of this block.
    pub squelch_open: bool,
    /// Smoothed speech-AGC gain after this block, in dB relative to unity.
    pub agc_gain_db: f32,
    /// Peak limiter reduction accumulated by this demodulator, in non-negative dB.
    pub limiter_reduction_db: f32,
    /// Effective upper voice cutoff after RF-profile clamping, in hertz.
    pub effective_voice_high_hz: f32,
}

/// Linear-phase Kaiser-windowed-sinc voice band-pass.
///
/// The 200 Hz lower transition is the tightest profile constraint and determines the tap count.
/// Sixty decibels of design attenuation leaves margin over the direct 30 dB stop-band contract.
/// The odd symmetric impulse response has deterministic delay `(taps_len - 1) / 2`, emits exactly
/// one output for every input, and retains its circular history across arbitrary partitions.
#[derive(Debug)]
struct VoiceBandpass {
    taps: Vec<f32>,
    history: Vec<f32>,
    cursor: usize,
}

impl VoiceBandpass {
    const STOPBAND_DB: f64 = 60.0;
    const LOWER_TRANSITION_HZ: f64 = 200.0;

    fn design(low_hz: f64, high_hz: f64, sample_rate_hz: f64) -> Result<Self> {
        let nyquist_hz = sample_rate_hz / 2.0;
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            bail!("voice-filter sample rate must be finite and positive");
        }
        if !low_hz.is_finite()
            || !high_hz.is_finite()
            || low_hz <= 0.0
            || high_hz <= low_hz
            || high_hz >= nyquist_hz
        {
            bail!("invalid voice band {low_hz}..{high_hz} Hz at {sample_rate_hz} Hz sample rate");
        }

        let transition_normalized = Self::LOWER_TRANSITION_HZ / sample_rate_hz;
        let estimate = (Self::STOPBAND_DB - 8.0)
            / (2.285 * std::f64::consts::TAU * transition_normalized)
            + 1.0;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let estimated_taps = estimate.ceil() as usize;
        let taps_len = estimated_taps | 1;
        let centre = (taps_len - 1) / 2;
        let beta = 0.1102 * (Self::STOPBAND_DB - 8.7);
        let window_denominator = Self::bessel_i0(beta);
        #[allow(clippy::cast_precision_loss)]
        let centre_f64 = centre as f64;
        let mut taps_f64 = Vec::with_capacity(taps_len);
        for index in 0..taps_len {
            #[allow(clippy::cast_precision_loss)]
            let offset = index as f64 - centre_f64;
            let ideal = if offset == 0.0 {
                2.0 * (high_hz - low_hz) / sample_rate_hz
            } else {
                ((std::f64::consts::TAU * high_hz * offset / sample_rate_hz).sin()
                    - (std::f64::consts::TAU * low_hz * offset / sample_rate_hz).sin())
                    / (std::f64::consts::PI * offset)
            };
            let ratio = offset / centre_f64;
            let window =
                Self::bessel_i0(beta * (1.0 - ratio * ratio).max(0.0).sqrt()) / window_denominator;
            taps_f64.push(ideal * window);
        }

        // Normalize at 1 kHz, which lies inside every supported aviation voice profile. This avoids
        // the zero-DC sum of a band-pass and gives profile comparisons a stable unity reference.
        let reference_hz = 1_000.0_f64.clamp(low_hz, high_hz);
        let omega = std::f64::consts::TAU * reference_hz / sample_rate_hz;
        let gain: f64 = taps_f64
            .iter()
            .enumerate()
            .map(|(index, tap)| {
                #[allow(clippy::cast_precision_loss)]
                let offset = index as f64 - centre_f64;
                tap * (omega * offset).cos()
            })
            .sum();
        if !gain.is_finite() || gain.abs() <= f64::MIN_POSITIVE {
            bail!("voice-filter normalization failed for {low_hz}..{high_hz} Hz");
        }
        let taps: Vec<f32> = taps_f64
            .into_iter()
            .map(|tap| {
                #[allow(clippy::cast_possible_truncation)]
                let tap = (tap / gain) as f32;
                tap
            })
            .collect();
        if taps.iter().any(|tap| !tap.is_finite()) {
            bail!("voice-filter design produced a non-finite coefficient");
        }
        Ok(Self {
            history: vec![0.0; taps.len()],
            taps,
            cursor: 0,
        })
    }

    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(input.len());
        for &sample in input {
            self.history[self.cursor] = sample;
            let mut accumulator = 0.0_f64;
            let mut history_index = self.cursor;
            for &tap in &self.taps {
                accumulator += f64::from(tap) * f64::from(self.history[history_index]);
                history_index = if history_index == 0 {
                    self.taps.len() - 1
                } else {
                    history_index - 1
                };
            }
            #[allow(clippy::cast_possible_truncation)]
            output.push(accumulator as f32);
            self.cursor = (self.cursor + 1) % self.taps.len();
        }
        output
    }

    #[must_use]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "stage delay only feeds the end-to-end latency report"
        )
    )]
    fn latency_samples(&self) -> usize {
        (self.taps.len() - 1) / 2
    }

    fn bessel_i0(value: f64) -> f64 {
        let mut sum = 1.0;
        let mut term = 1.0;
        let half = value / 2.0;
        for index in 1..=64_u32 {
            let factor = half / f64::from(index);
            term *= factor * factor;
            sum += term;
            if term < sum * 1.0e-17 {
                break;
            }
        }
        sum
    }
}

/// Stateful first-order DC blocker for the AM envelope.
#[derive(Debug)]
struct DcBlocker {
    alpha: f32,
    previous_input: f32,
    previous_output: f32,
}

impl DcBlocker {
    fn new(sample_rate_hz: f64) -> Result<Self> {
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            bail!("DC-blocker sample rate must be finite and positive");
        }
        // A 30 Hz corner removes carrier DC while remaining far below every voice profile.
        #[allow(clippy::cast_possible_truncation)]
        let alpha = (-std::f64::consts::TAU * 30.0 / sample_rate_hz).exp() as f32;
        Ok(Self {
            alpha,
            previous_input: 0.0,
            previous_output: 0.0,
        })
    }

    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(input.len());
        for &sample in input {
            let filtered = sample - self.previous_input + self.alpha * self.previous_output;
            self.previous_input = sample;
            self.previous_output = filtered;
            output.push(filtered);
        }
        output
    }
}

/// Stateful standalone AM demodulator for one tuned channel.
#[derive(Debug)]
pub(crate) struct AmDemodulator {
    channelizer: AmChannelizer,
    squelch: SquelchState,
    dc_blocker: DcBlocker,
    voice_filter: VoiceBandpass,
    agc: SpeechAgc,
    limiter: SoftLimiter,
    profile: AmAudioProfile,
    output_rate_hz: f64,
}

impl AmDemodulator {
    /// Build one AM chain tuned from `center_hz` to `channel_hz`.
    ///
    /// `input_rate_hz` is the RF I/Q clock and `output_rate_hz` is both the channelizer and audio
    /// clock. `profile` selects RF filtering, voice filtering, and squelch behavior.
    ///
    /// # Errors
    ///
    /// Returns an error for non-finite frequencies, a non-finite frequency difference, invalid
    /// rates/profile values, or any filter/resampler design that cannot satisfy the selected
    /// profile at the requested output rate.
    pub fn new(
        center_hz: f64,
        channel_hz: f64,
        input_rate_hz: f64,
        output_rate_hz: f64,
        profile: AmAudioProfile,
    ) -> Result<Self> {
        let offset_hz = channel_hz - center_hz;
        if !center_hz.is_finite() || !channel_hz.is_finite() || !offset_hz.is_finite() {
            bail!("AM tuning frequencies and their difference must be finite");
        }
        profile.validate().context("invalid AM audio profile")?;
        let channelizer = AmChannelizer::new(
            center_hz,
            channel_hz,
            input_rate_hz,
            output_rate_hz,
            profile.rf,
            profile.voice,
        )?;
        let effective_high_hz = channelizer.effective_voice_high_hz();
        let voice_filter = VoiceBandpass::design(
            f64::from(profile.voice.low_cutoff_hz()),
            effective_high_hz,
            output_rate_hz,
        )?;
        let squelch = SquelchState::new(profile.squelch, output_rate_hz)?;
        let dc_blocker = DcBlocker::new(output_rate_hz)?;
        let agc = SpeechAgc::new(
            output_rate_hz,
            DEFAULT_TARGET_RMS,
            DEFAULT_ATTACK_MS,
            DEFAULT_RELEASE_MS,
            DEFAULT_MIN_GAIN_DB,
            DEFAULT_MAX_GAIN_DB,
        )?;
        Ok(Self {
            channelizer,
            squelch,
            dc_blocker,
            voice_filter,
            agc,
            limiter: SoftLimiter::new(),
            profile,
            output_rate_hz,
        })
    }

    /// Process an arbitrary RF-rate I/Q partition through the complete chain.
    ///
    /// The returned sample count follows the channelizer's exact rational clock. An empty
    /// channelized block returns empty audio and leaves squelch timing unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error before changing state when any input component is non-finite or its squared
    /// complex magnitude exceeds `f32::MAX`. That bound admits ordinary finite SDR amplitudes while
    /// ensuring the downstream `f32` power and envelope paths cannot overflow.
    pub fn process(&mut self, iq: &[IqSample]) -> Result<AmAudioBlock> {
        for sample in iq {
            let real = f64::from(sample.re);
            let imaginary = f64::from(sample.im);
            let norm_squared = real * real + imaginary * imaginary;
            if !norm_squared.is_finite() || norm_squared > f64::from(f32::MAX) {
                bail!(
                    "AM input must have finite I/Q and squared magnitude no greater than f32::MAX"
                );
            }
        }

        let channelized = self.channelizer.process(iq)?;
        if channelized.iq.is_empty() {
            return Ok(self.block(Vec::new(), channelized.power_dbfs));
        }

        let envelope: Vec<f32> = channelized.iq.iter().map(|sample| sample.norm()).collect();
        let dc_removed = self.dc_blocker.process(&envelope);
        let filtered = self.voice_filter.process(&dc_removed);

        // One channelizer output has one block-mean power measurement. Advance exact frame timing
        // with that constant value, retaining the convention that the frame completing an interval
        // is still governed by the old state and the following frame sees the transition. Very large
        // caller partitions are split only to respect the squelch state's bounded work-per-call;
        // every chunk retains the same channelizer block-mean measurement.
        let mut gained = Vec::with_capacity(filtered.len());
        let mut filtered_offset = 0;
        while filtered_offset < filtered.len() {
            let chunk_len = (filtered.len() - filtered_offset).min(MAX_BLOCK_FRAMES);
            let open_before = self.squelch.is_open();
            let update = self.squelch.process(ChannelPowerSample {
                power_dbfs: channelized.power_dbfs,
                frames: chunk_len,
            })?;
            let transition_index = update.transition.map_or(chunk_len, |transition| {
                let chunk_start = update.frames_elapsed
                    - u64::try_from(chunk_len).expect("bounded chunk length fits u64");
                usize::try_from(transition.at_frame - chunk_start)
                    .expect("transition lies inside the current chunk")
            });
            let chunk = &filtered[filtered_offset..filtered_offset + chunk_len];
            self.agc.set_squelch_open(open_before);
            gained.extend(self.agc.process(&chunk[..transition_index])?);
            if transition_index < chunk.len() {
                self.agc.set_squelch_open(update.open);
                gained.extend(self.agc.process(&chunk[transition_index..])?);
            }
            filtered_offset += chunk_len;
        }
        let samples = self.limiter.process(&gained);
        Ok(self.block(samples, channelized.power_dbfs))
    }

    fn block(&self, samples: Vec<f32>, channel_power_dbfs: f32) -> AmAudioBlock {
        #[allow(clippy::cast_possible_truncation)]
        let effective_voice_high_hz = self.channelizer.effective_voice_high_hz() as f32;
        AmAudioBlock {
            samples,
            channel_power_dbfs,
            noise_floor_dbfs: self.squelch.noise_floor_dbfs(),
            squelch_threshold_dbfs: self.squelch.threshold_dbfs(),
            squelch_open: self.squelch.is_open(),
            agc_gain_db: self.agc.current_gain_db(),
            limiter_reduction_db: self.limiter.reduction_db(),
            effective_voice_high_hz,
        }
    }

    /// Retune without resetting oscillator phase, resampling/filter history, squelch, or AGC.
    ///
    /// # Errors
    ///
    /// Returns an error and preserves the previous tuning when either frequency or their difference
    /// is non-finite.
    pub fn retune(&mut self, center_hz: f64, channel_hz: f64) -> Result<()> {
        let offset_hz = channel_hz - center_hz;
        if !center_hz.is_finite() || !channel_hz.is_finite() || !offset_hz.is_finite() {
            bail!("AM tuning frequencies and their difference must be finite");
        }
        self.channelizer.retune(center_hz, channel_hz);
        Ok(())
    }

    /// Atomically apply a new AM profile.
    ///
    /// Required replacement filters are designed before live stages change. A successful change
    /// preserves AGC and squelch history; only filter stages that require redesign are rebuilt.
    ///
    /// # Errors
    ///
    /// Returns an error with the old profile and filters still usable when profile validation or a
    /// rate-derived filter design fails.
    pub fn set_profile(&mut self, profile: AmAudioProfile) -> Result<()> {
        profile.validate().context("invalid AM audio profile")?;
        if profile == self.profile {
            return Ok(());
        }

        let old_edges = self.profile.rf.effective_edges_hz(self.profile.voice);
        let new_edges = profile.rf.effective_edges_hz(profile.voice);
        let channel_profile_changed =
            profile.rf != self.profile.rf || profile.voice != self.profile.voice;
        let voice_response_changed = profile.voice != self.profile.voice
            || new_edges.effective_voice_high_hz.to_bits()
                != old_edges.effective_voice_high_hz.to_bits();
        let replacement_voice = if voice_response_changed {
            Some(VoiceBandpass::design(
                f64::from(profile.voice.low_cutoff_hz()),
                f64::from(new_edges.effective_voice_high_hz),
                self.output_rate_hz,
            )?)
        } else {
            None
        };

        if channel_profile_changed {
            self.channelizer.set_profiles(profile.rf, profile.voice)?;
        }
        if profile.squelch != self.profile.squelch {
            self.squelch.set_mode(profile.squelch)?;
        }
        if let Some(voice_filter) = replacement_voice {
            self.voice_filter = voice_filter;
        }
        self.profile = profile;
        Ok(())
    }

    /// Flushes the chain's resampler tail after the last input block.
    ///
    /// A stream must end on its exact RF-clock duration. The channelizer's resampler buffers a
    /// partial interval, so without this call a rate whose ratio leaves a remainder ends short.
    /// The returned block is the final audio to write; call this once and then discard the chain.
    ///
    /// # Errors
    ///
    /// Returns an error when the channelizer cannot flush.
    pub fn finish(&mut self) -> Result<AmAudioBlock> {
        let channelized = self.channelizer.finish()?;
        if channelized.iq.is_empty() {
            return Ok(self.block(Vec::new(), channelized.power_dbfs));
        }
        let envelope: Vec<f32> = channelized.iq.iter().map(|sample| sample.norm()).collect();
        let dc_removed = self.dc_blocker.process(&envelope);
        let filtered = self.voice_filter.process(&dc_removed);
        let open = self.squelch.is_open();
        self.agc.set_squelch_open(open);
        let gained = self.agc.process(&filtered)?;
        let samples = self.limiter.process(&gained);
        Ok(self.block(samples, channelized.power_dbfs))
    }

    /// The profile currently in effect.
    ///
    /// Callers build a replacement profile from this one so a single control changes only its own
    /// field, leaving the RF bandwidth, voice shape, squelch, and prebuffer otherwise intact.
    #[must_use]
    pub fn profile(&self) -> &AmAudioProfile {
        &self.profile
    }

    /// Total deterministic pipeline latency, in output-rate samples.
    ///
    /// Callers discard at least this many leading samples before spectral or timing measurements.
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "latency is consumed by callers that discard lead-in samples"
        )
    )]
    pub fn latency_samples(&self) -> usize {
        self.channelizer
            .latency_samples()
            .saturating_add(self.voice_filter.latency_samples())
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use proptest::prelude::*;

    use super::*;
    use crate::audio::profile::{AmRfProfile, SquelchMode, VoiceProfile};

    const CENTER_HZ: f64 = 125_000_000.0;
    const RF_RATE_HZ: f64 = 2_048_000.0;
    const CHEAP_RF_RATE_HZ: f64 = 192_000.0;
    const AUDIO_RATE_HZ: f64 = 48_000.0;
    const TONE_HZ: f64 = 1_000.0;
    const RF_BLOCK: usize = 4_096;

    fn profile(rf: AmRfProfile, voice: VoiceProfile, squelch: SquelchMode) -> AmAudioProfile {
        AmAudioProfile {
            rf,
            voice,
            squelch,
            prebuffer_ms: 100,
        }
    }

    fn open_standard() -> AmAudioProfile {
        profile(AmRfProfile::Khz8, VoiceProfile::Standard, SquelchMode::Open)
    }

    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    fn am_sample(
        index: usize,
        input_rate_hz: f64,
        carrier_offset_hz: f64,
        modulation_hz: f64,
        carrier_amplitude: f64,
        modulation_depth: f64,
    ) -> IqSample {
        let time = index as f64 / input_rate_hz;
        let envelope =
            carrier_amplitude * (1.0 + modulation_depth * (TAU * modulation_hz * time).sin());
        let phase = TAU * carrier_offset_hz * time;
        IqSample::new(
            (envelope * phase.cos()) as f32,
            (envelope * phase.sin()) as f32,
        )
    }

    fn process_generated<F>(
        demodulator: &mut AmDemodulator,
        input_frames: usize,
        block_size: usize,
        mut sample: F,
    ) -> Result<Vec<f32>>
    where
        F: FnMut(usize) -> IqSample,
    {
        let mut output = Vec::new();
        let mut offset = 0_usize;
        while offset < input_frames {
            let count = block_size.min(input_frames - offset);
            let input: Vec<IqSample> = (offset..offset + count).map(&mut sample).collect();
            output.extend(demodulator.process(&input)?.samples);
            offset += count;
        }
        Ok(output)
    }

    fn assert_finite_bounded(samples: &[f32]) {
        assert!(
            samples.iter().all(|sample| sample.is_finite()),
            "audio must contain no NaN or infinity"
        );
        assert!(
            samples.iter().all(|sample| sample.abs() <= 1.0),
            "the limiter must bound every sample to full scale"
        );
    }

    fn rms(samples: &[f32]) -> f64 {
        assert!(
            !samples.is_empty(),
            "RMS needs a non-empty measurement window"
        );
        let sum: f64 = samples
            .iter()
            .map(|sample| f64::from(*sample) * f64::from(*sample))
            .sum();
        #[allow(clippy::cast_precision_loss)]
        let count = samples.len() as f64;
        (sum / count).sqrt()
    }

    fn assert_audio_blocks_bit_identical(
        actual: &AmAudioBlock,
        expected: &AmAudioBlock,
        context: &str,
    ) {
        assert_eq!(
            actual.samples.len(),
            expected.samples.len(),
            "{context}: sample count"
        );
        for (index, (actual, expected)) in actual.samples.iter().zip(&expected.samples).enumerate()
        {
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "{context}: audio sample {index} differs"
            );
        }
        assert_eq!(
            actual.channel_power_dbfs.to_bits(),
            expected.channel_power_dbfs.to_bits(),
            "{context}: channel power"
        );
        assert_eq!(
            actual.noise_floor_dbfs.to_bits(),
            expected.noise_floor_dbfs.to_bits(),
            "{context}: noise floor"
        );
        assert_eq!(
            actual.squelch_threshold_dbfs.to_bits(),
            expected.squelch_threshold_dbfs.to_bits(),
            "{context}: squelch threshold"
        );
        assert_eq!(
            actual.squelch_open, expected.squelch_open,
            "{context}: gate state"
        );
        assert_eq!(
            actual.agc_gain_db.to_bits(),
            expected.agc_gain_db.to_bits(),
            "{context}: AGC gain"
        );
        assert_eq!(
            actual.limiter_reduction_db.to_bits(),
            expected.limiter_reduction_db.to_bits(),
            "{context}: limiter reduction"
        );
        assert_eq!(
            actual.effective_voice_high_hz.to_bits(),
            expected.effective_voice_high_hz.to_bits(),
            "{context}: effective voice cutoff"
        );
    }

    fn generated_block(start: usize, count: usize, carrier_offset_hz: f64) -> Vec<IqSample> {
        (start..start + count)
            .map(|index| am_sample(index, RF_RATE_HZ, carrier_offset_hz, TONE_HZ, 0.25, 0.85))
            .collect()
    }

    fn settled_voice_amplitude(low_hz: f64, high_hz: f64, tone_hz: f64) -> f64 {
        let mut filter = VoiceBandpass::design(low_hz, high_hz, AUDIO_RATE_HZ)
            .expect("literal voice band must design");
        let input: Vec<f32> = (0..48_000_usize)
            .map(|index| {
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                let sample = (TAU * tone_hz * index as f64 / AUDIO_RATE_HZ).sin() as f32;
                sample
            })
            .collect();
        let output = filter.process(&input);
        assert_eq!(
            output.len(),
            input.len(),
            "voice filter must preserve the clock"
        );
        assert!(output.iter().all(|sample| sample.is_finite()));
        dft_amplitude(&output[24_000..], tone_hz, AUDIO_RATE_HZ)
    }

    fn dft_amplitude(samples: &[f32], frequency_hz: f64, rate_hz: f64) -> f64 {
        let mut real = 0.0_f64;
        let mut imaginary = 0.0_f64;
        for (index, sample) in samples.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let phase = TAU * frequency_hz * index as f64 / rate_hz;
            real += f64::from(*sample) * phase.cos();
            imaginary -= f64::from(*sample) * phase.sin();
        }
        #[allow(clippy::cast_precision_loss)]
        let scale = 2.0 / samples.len() as f64;
        scale * real.hypot(imaginary)
    }

    fn estimate_frequency_from_positive_crossings(samples: &[f32], rate_hz: f64) -> f64 {
        let mut crossings = Vec::new();
        for (index, pair) in samples.windows(2).enumerate() {
            if pair[0] < 0.0 && pair[1] >= 0.0 {
                let left = f64::from(pair[0]);
                let right = f64::from(pair[1]);
                let fraction = -left / (right - left);
                #[allow(clippy::cast_precision_loss)]
                crossings.push(index as f64 + fraction);
            }
        }
        assert!(
            crossings.len() > 20,
            "fixture must contain many positive crossings"
        );
        let periods = crossings.len() - 1;
        let elapsed_samples = crossings[crossings.len() - 1] - crossings[0];
        #[allow(clippy::cast_precision_loss)]
        let periods = periods as f64;
        periods * rate_hz / elapsed_samples
    }

    fn zero_crossings(samples: &[f32]) -> usize {
        samples
            .windows(2)
            .filter(|pair| (pair[0] >= 0.0) != (pair[1] >= 0.0))
            .count()
    }

    fn deterministic_noise(index: usize, amplitude: f32) -> IqSample {
        let mut value = u64::try_from(index).expect("test index fits u64") ^ 0x9e37_79b9_7f4a_7c15;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        let first = value.wrapping_mul(0x2545_f491_4f6c_dd1d);
        let second = first.rotate_left(29).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let to_bipolar = |bits: u64| {
            let unit = (bits >> 40) as u32;
            #[allow(clippy::cast_precision_loss)]
            let normalized = unit as f32 / 16_777_215.0;
            amplitude * (2.0 * normalized - 1.0)
        };
        IqSample::new(to_bipolar(first), to_bipolar(second))
    }

    #[test]
    fn synthetic_am_recovers_one_khz_with_less_than_one_hz_error() {
        let mut demodulator = AmDemodulator::new(
            CENTER_HZ,
            CENTER_HZ,
            RF_RATE_HZ,
            AUDIO_RATE_HZ,
            open_standard(),
        )
        .expect("literal rates and profile must construct");
        let latency = demodulator.latency_samples();
        let input_frames = 2 * 2_048_000;
        let output = process_generated(&mut demodulator, input_frames, RF_BLOCK, |index| {
            am_sample(index, RF_RATE_HZ, 0.0, 1_000.0, 0.35, 0.70)
        })
        .expect("finite synthetic AM must process");

        assert_eq!(
            output.len(),
            96_000,
            "two seconds must yield literal 48 kHz audio"
        );
        assert_finite_bounded(&output);
        let settled_start = latency + 24_000;
        let measured_hz =
            estimate_frequency_from_positive_crossings(&output[settled_start..], 48_000.0);
        assert!(
            (measured_hz - 1_000.0).abs() < 1.0,
            "recovered tone was {measured_hz:.4} Hz, expected literal 1000 Hz"
        );
    }

    #[test]
    fn adjacent_am_at_8330_hz_is_rejected_by_at_least_forty_db() {
        let mut demodulator = AmDemodulator::new(
            CENTER_HZ,
            CENTER_HZ,
            RF_RATE_HZ,
            AUDIO_RATE_HZ,
            open_standard(),
        )
        .expect("literal rates and profile must construct");
        let latency = demodulator.latency_samples();
        let output = process_generated(&mut demodulator, 3 * 2_048_000, RF_BLOCK, |index| {
            let desired = am_sample(index, RF_RATE_HZ, 0.0, 1_000.0, 0.20, 0.65);
            let adjacent = am_sample(index, RF_RATE_HZ, 8_330.0, 2_000.0, 0.70, 0.85);
            desired + adjacent
        })
        .expect("two-carrier fixture must process");

        let settled = &output[latency + 48_000..];
        let desired = dft_amplitude(settled, 1_000.0, 48_000.0);
        let leaked_adjacent_modulation = dft_amplitude(settled, 2_000.0, 48_000.0);
        assert!(
            desired > 1e-4,
            "desired 1 kHz coefficient must be measurable"
        );
        let rejection_db = 20.0 * (desired / leaked_adjacent_modulation.max(1e-12)).log10();
        assert!(
            rejection_db >= 40.0,
            "desired/adjacent-modulation ratio was {rejection_db:.2} dB, required >= 40 dB"
        );
    }

    #[test]
    fn closed_squelch_outputs_exact_zero_and_freezes_agc() {
        let mut demodulator = AmDemodulator::new(
            CENTER_HZ,
            CENTER_HZ,
            RF_RATE_HZ,
            AUDIO_RATE_HZ,
            open_standard(),
        )
        .expect("literal rates and profile must construct");
        process_generated(&mut demodulator, 3 * 2_048_000, RF_BLOCK, |index| {
            am_sample(index, RF_RATE_HZ, 0.0, 1_000.0, 0.015, 0.75)
        })
        .expect("warmup must process");

        demodulator
            .set_profile(profile(
                AmRfProfile::Khz8,
                VoiceProfile::Standard,
                SquelchMode::Manual {
                    threshold_dbfs: -30.0,
                },
            ))
            .expect("finite manual threshold must apply");
        process_generated(&mut demodulator, 614_400, RF_BLOCK, |index| {
            deterministic_noise(index, 1e-5)
        })
        .expect("close-hang lead-in must process");
        let closed = demodulator
            .process(&[])
            .expect("empty diagnostic read must be accepted");
        assert!(
            !closed.squelch_open,
            "noise below -30 dBFS must close the gate"
        );
        let frozen_gain_bits = closed.agc_gain_db.to_bits();
        assert!(
            closed.agc_gain_db.abs() > 0.25,
            "warmup must establish non-default gain"
        );

        let muted = process_generated(&mut demodulator, 2 * 2_048_000, RF_BLOCK, |index| {
            deterministic_noise(index + 614_400, 1e-5)
        })
        .expect("two seconds of finite noise must process");
        assert_eq!(
            muted.len(),
            96_000,
            "two seconds must still retain the output clock"
        );
        assert!(
            muted
                .iter()
                .all(|sample| sample.to_bits() == 0.0_f32.to_bits()),
            "closed squelch must emit exact positive-zero bits"
        );
        let after = demodulator
            .process(&[])
            .expect("diagnostics must remain readable");
        assert!(!after.squelch_open);
        assert_eq!(
            after.agc_gain_db.to_bits(),
            frozen_gain_bits,
            "AGC gain must be bit-identical for at least two closed seconds"
        );
    }

    #[test]
    fn forty_db_rf_amplitude_steps_remain_finite_bounded_and_intelligible() {
        let mut demodulator = AmDemodulator::new(
            CENTER_HZ,
            CENTER_HZ,
            RF_RATE_HZ,
            AUDIO_RATE_HZ,
            open_standard(),
        )
        .expect("literal rates and profile must construct");
        let segment_rf_frames = 3 * 2_048_000;
        let output =
            process_generated(&mut demodulator, 2 * segment_rf_frames, RF_BLOCK, |index| {
                let amplitude = if index < segment_rf_frames {
                    0.005
                } else {
                    0.5
                };
                am_sample(index, RF_RATE_HZ, 0.0, 1_000.0, amplitude, 0.75)
            })
            .expect("40 dB amplitude-step fixture must process");

        assert_finite_bounded(&output);
        let latency = demodulator.latency_samples();
        let quiet = &output[latency + 48_000..144_000];
        let loud = &output[144_000 + 48_000..];
        for (label, window) in [("quiet", quiet), ("loud", loud)] {
            let measured = estimate_frequency_from_positive_crossings(window, 48_000.0);
            assert!(
                (measured - 1_000.0).abs() < 2.0,
                "{label} segment was {measured} Hz"
            );
            let expected_crossings = 2 * window.len() / 48;
            assert!(
                zero_crossings(window).abs_diff(expected_crossings) <= 4,
                "{label} segment must retain intelligible 1 kHz zero crossings"
            );
        }
    }

    #[test]
    fn voice_profiles_match_literal_frequency_edges() {
        const CASES: [(VoiceProfile, f64, f64); 3] = [
            (VoiceProfile::Narrow, 300.0, 2_800.0),
            (VoiceProfile::Standard, 250.0, 3_400.0),
            (VoiceProfile::Wide, 150.0, 4_000.0),
        ];

        for (voice, literal_low_hz, literal_high_hz) in CASES {
            let selected = profile(AmRfProfile::Khz12, voice, SquelchMode::Open);
            let mut diagnostic_demodulator = AmDemodulator::new(
                CENTER_HZ,
                CENTER_HZ,
                CHEAP_RF_RATE_HZ,
                AUDIO_RATE_HZ,
                selected.clone(),
            )
            .expect("12 kHz RF profile is valid at 48 kHz output");
            let diagnostics = diagnostic_demodulator
                .process(&[])
                .expect("empty block is valid");
            assert!(
                (f64::from(diagnostics.effective_voice_high_hz) - literal_high_hz).abs() < 0.01,
                "{voice:?} must report literal high edge {literal_high_hz} Hz"
            );

            let measure = |modulation_hz: f64| {
                let mut demodulator = AmDemodulator::new(
                    CENTER_HZ,
                    CENTER_HZ,
                    CHEAP_RF_RATE_HZ,
                    AUDIO_RATE_HZ,
                    selected.clone(),
                )
                .expect("profile fixture must construct");
                let latency = demodulator.latency_samples();
                let output = process_generated(&mut demodulator, 192_000, 1_536, |index| {
                    am_sample(index, CHEAP_RF_RATE_HZ, 0.0, modulation_hz, 0.3, 0.8)
                })
                .expect("finite sweep tone must process");
                dft_amplitude(&output[latency + 24_000..], modulation_hz, 48_000.0)
            };

            let reference = measure(1_000.0);
            let inside_low = measure(literal_low_hz + 150.0);
            let inside_high = measure(literal_high_hz - 400.0);
            let below = measure((literal_low_hz - 125.0).max(25.0));
            let above = measure(literal_high_hz + 600.0);
            for (label, amplitude) in [("low interior", inside_low), ("high interior", inside_high)]
            {
                let error_db = 20.0 * (amplitude / reference).log10();
                assert!(
                    error_db.abs() <= 1.0,
                    "{voice:?} {label} ripple was {error_db:.2} dB"
                );
            }
            let below_db = 20.0 * (below / reference).log10();
            let above_db = 20.0 * (above / reference).log10();
            assert!(
                below_db <= -6.0,
                "{voice:?} below {literal_low_hz} Hz was {below_db:.2} dB"
            );
            assert!(
                above_db <= -12.0,
                "{voice:?} above {literal_high_hz} Hz was {above_db:.2} dB"
            );
        }

        let mut clamped = AmDemodulator::new(
            CENTER_HZ,
            CENTER_HZ,
            CHEAP_RF_RATE_HZ,
            AUDIO_RATE_HZ,
            profile(AmRfProfile::Khz6, VoiceProfile::Wide, SquelchMode::Open),
        )
        .expect("6 kHz RF/Wide is valid at 48 kHz");
        let diagnostics = clamped.process(&[]).expect("empty block is valid");
        assert_eq!(
            diagnostics.effective_voice_high_hz.to_bits(),
            2_600.0_f32.to_bits(),
            "Wide's literal 4000 Hz edge must be RF-clamped to literal 2600 Hz under 6 kHz RF"
        );
    }

    #[test]
    fn silence_full_scale_empty_blocks_and_retune_never_emit_non_finite_values() {
        let mut demodulator = AmDemodulator::new(
            CENTER_HZ,
            CENTER_HZ,
            RF_RATE_HZ,
            AUDIO_RATE_HZ,
            open_standard(),
        )
        .expect("literal rates and profile must construct");

        let empty = demodulator.process(&[]).expect("empty input is valid");
        assert!(empty.samples.is_empty());
        let silence = demodulator
            .process(&vec![IqSample::new(0.0, 0.0); RF_BLOCK])
            .expect("silence is valid");
        assert_finite_bounded(&silence.samples);
        let full_scale = demodulator
            .process(&vec![IqSample::new(1.0, 1.0); RF_BLOCK])
            .expect("finite full-scale IQ is valid");
        assert_finite_bounded(&full_scale.samples);

        demodulator
            .retune(CENTER_HZ, CENTER_HZ + 25_000.0)
            .expect("finite valid retune must succeed");
        let after_retune = demodulator
            .process(&vec![IqSample::new(0.5, -0.5); RF_BLOCK])
            .expect("valid retune must remain usable");
        assert_finite_bounded(&after_retune.samples);

        for bad in [
            IqSample::new(f32::NAN, 0.0),
            IqSample::new(0.0, f32::INFINITY),
        ] {
            assert!(
                demodulator.process(&[bad]).is_err(),
                "non-finite IQ contract is Err"
            );
        }
        for (center_hz, channel_hz) in [
            (f64::NAN, CENTER_HZ),
            (CENTER_HZ, f64::INFINITY),
            (-f64::MAX, f64::MAX),
        ] {
            assert!(
                demodulator.retune(center_hz, channel_hz).is_err(),
                "non-finite value or overflowing difference must be rejected"
            );
        }
        let recovered = demodulator
            .process(&vec![IqSample::new(0.25, 0.0); RF_BLOCK])
            .expect("rejected input/retunes must leave prior tuning usable");
        assert_finite_bounded(&recovered.samples);
    }

    #[test]
    fn random_block_partitioning_produces_equivalent_audio() {
        let input_frames = 1_024_000_usize;
        let input: Vec<IqSample> = (0..input_frames)
            .map(|index| am_sample(index, RF_RATE_HZ, 17_000.0, 1_000.0, 0.25, 0.7))
            .collect();
        let mut random_sizes = Vec::new();
        let mut state = 0x1234_5678_u32;
        for _ in 0..2_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            random_sizes.push(1 + usize::try_from(state % 4_096).expect("u32 fits usize"));
        }
        let plans: [(&str, Vec<usize>); 5] = [
            ("257", vec![257]),
            ("1024", vec![1_024]),
            ("2048", vec![2_048]),
            ("4096", vec![4_096]),
            ("random", random_sizes),
        ];
        let mut reference: Option<Vec<f32>> = None;

        for (label, sizes) in plans {
            let mut demodulator = AmDemodulator::new(
                CENTER_HZ,
                CENTER_HZ + 17_000.0,
                RF_RATE_HZ,
                AUDIO_RATE_HZ,
                open_standard(),
            )
            .expect("partition fixture must construct");
            let latency = demodulator.latency_samples();
            let mut output = Vec::new();
            let mut offset = 0_usize;
            let mut block = 0_usize;
            while offset < input.len() {
                if block.is_multiple_of(3) {
                    let empty = demodulator
                        .process(&[])
                        .expect("interspersed empty input is valid");
                    assert!(empty.samples.is_empty(), "empty input must emit no audio");
                }
                let count = sizes[block % sizes.len()].min(input.len() - offset);
                output.extend(
                    demodulator
                        .process(&input[offset..offset + count])
                        .expect("finite partition must process")
                        .samples,
                );
                offset += count;
                block += 1;
            }
            assert_eq!(
                output.len(),
                24_000,
                "{label}: 0.5 s must yield exactly 24,000 samples"
            );
            match &reference {
                None => reference = Some(output),
                Some(expected) => {
                    let difference: Vec<f32> = expected[latency..]
                        .iter()
                        .zip(&output[latency..])
                        .map(|(left, right)| left - right)
                        .collect();
                    assert!(
                        rms(&difference) <= 1e-4,
                        "{label}: aligned partition RMS difference exceeded 1e-4"
                    );
                }
            }
        }
    }

    #[test]
    fn retune_preserves_agc_squelch_and_oscillator_state() {
        let mut demodulator = AmDemodulator::new(
            CENTER_HZ,
            CENTER_HZ + 11_000.0,
            RF_RATE_HZ,
            AUDIO_RATE_HZ,
            open_standard(),
        )
        .expect("literal rates and profile must construct");
        let before_audio = process_generated(&mut demodulator, 3 * 2_048_000, RF_BLOCK, |index| {
            am_sample(index, RF_RATE_HZ, 11_000.0, 1_000.0, 0.02, 0.75)
        })
        .expect("warmup must process");
        let before = demodulator.process(&[]).expect("diagnostic read must work");
        assert!(
            before.agc_gain_db.abs() > 0.25,
            "warmup must establish non-default gain"
        );

        demodulator
            .retune(CENTER_HZ, CENTER_HZ - 19_000.0)
            .expect("finite retune must succeed");
        let after_audio = process_generated(&mut demodulator, 2 * 2_048_000, RF_BLOCK, |index| {
            am_sample(
                index + 3 * 2_048_000,
                RF_RATE_HZ,
                -19_000.0,
                1_000.0,
                0.02,
                0.75,
            )
        })
        .expect("retuned carrier must process");
        let after = demodulator.process(&[]).expect("diagnostic read must work");

        assert_eq!(
            before.squelch_open, after.squelch_open,
            "retune must preserve gate state"
        );
        assert_eq!(
            before.noise_floor_dbfs.to_bits(),
            after.noise_floor_dbfs.to_bits(),
            "Open-mode noise floor must not reset on retune"
        );
        assert!(
            after.agc_gain_db.abs() > 0.25,
            "retune must not reset AGC gain to unity"
        );
        assert_finite_bounded(&after_audio);
        let boundary = before_audio.last().copied().unwrap_or(0.0);
        let first = after_audio.first().copied().unwrap_or(boundary);
        assert!(
            (first - boundary).abs() < 1.5,
            "continuous NCO retune must not create a click-scale jump"
        );
    }

    #[test]
    fn profile_change_is_atomic_and_updates_effective_cutoff() {
        let output_rate_hz = 10_000.0;
        let initial = profile(AmRfProfile::Khz6, VoiceProfile::Narrow, SquelchMode::Open);
        let mut demodulator = AmDemodulator::new(
            CENTER_HZ,
            CENTER_HZ,
            CHEAP_RF_RATE_HZ,
            output_rate_hz,
            initial.clone(),
        )
        .expect("6 kHz RF/Narrow is valid at a 10 kHz output rate");
        process_generated(&mut demodulator, 384_000, 1_536, |index| {
            am_sample(index, CHEAP_RF_RATE_HZ, 0.0, 1_000.0, 0.02, 0.75)
        })
        .expect("warmup must process");
        let before = demodulator
            .process(&[])
            .expect("diagnostics must be available");
        assert_eq!(
            before.effective_voice_high_hz.to_bits(),
            2_600.0_f32.to_bits()
        );
        assert!(before.agc_gain_db.abs() > 0.25);

        let invalid = profile(AmRfProfile::Khz12, VoiceProfile::Wide, SquelchMode::Open);
        assert!(
            demodulator.set_profile(invalid).is_err(),
            "12 kHz RF stop edge exceeds the 5 kHz Nyquist limit"
        );
        let after_error = demodulator
            .process(&[])
            .expect("old profile must remain usable after rejected change");
        assert_eq!(
            after_error.effective_voice_high_hz.to_bits(),
            before.effective_voice_high_hz.to_bits()
        );
        assert_eq!(
            after_error.agc_gain_db.to_bits(),
            before.agc_gain_db.to_bits()
        );
        assert_eq!(
            after_error.noise_floor_dbfs.to_bits(),
            before.noise_floor_dbfs.to_bits()
        );

        let valid = profile(AmRfProfile::Khz8, VoiceProfile::Standard, SquelchMode::Open);
        demodulator
            .set_profile(valid)
            .expect("8 kHz/Standard is valid below 5 kHz Nyquist");
        let after_valid = demodulator
            .process(&[])
            .expect("updated diagnostics must be available");
        assert_eq!(
            after_valid.effective_voice_high_hz.to_bits(),
            3_400.0_f32.to_bits(),
            "valid profile change must expose literal Standard high cutoff"
        );
        assert_eq!(
            after_valid.agc_gain_db.to_bits(),
            before.agc_gain_db.to_bits()
        );
        assert_eq!(
            after_valid.noise_floor_dbfs.to_bits(),
            before.noise_floor_dbfs.to_bits()
        );
    }

    #[test]
    fn block_mean_squelch_opens_after_exactly_480_eligible_frames() {
        const THRESHOLD_DBFS: f32 = -18.0;
        let selected = profile(
            AmRfProfile::Khz8,
            VoiceProfile::Standard,
            SquelchMode::Manual {
                threshold_dbfs: THRESHOLD_DBFS,
            },
        );
        let mut demodulator =
            AmDemodulator::new(CENTER_HZ, CENTER_HZ, RF_RATE_HZ, AUDIO_RATE_HZ, selected)
                .expect("literal rates and profile must construct");
        let mut meter = AmChannelizer::new(
            CENTER_HZ,
            CENTER_HZ,
            RF_RATE_HZ,
            AUDIO_RATE_HZ,
            AmRfProfile::Khz8,
            VoiceProfile::Standard,
        )
        .expect("independent channel-power meter must construct");
        demodulator.squelch = SquelchState::new(
            SquelchMode::Manual {
                threshold_dbfs: 1.0,
            },
            AUDIO_RATE_HZ,
        )
        .expect("warmup gate must construct closed");

        let mut input_index = 0_usize;
        for _ in 0..24 {
            let input: Vec<IqSample> = (input_index..input_index + RF_BLOCK)
                .map(|index| am_sample(index, RF_RATE_HZ, 0.0, TONE_HZ, 0.25, 0.0))
                .collect();
            demodulator.process(&input).expect("warmup must process");
            meter.process(&input).expect("meter warmup must process");
            input_index += RF_BLOCK;
        }
        for _ in 0..8 {
            let input: Vec<IqSample> = (input_index..input_index + RF_BLOCK)
                .map(|index| am_sample(index, RF_RATE_HZ, 0.0, TONE_HZ, 0.25, 0.99))
                .collect();
            let warm = demodulator
                .process(&input)
                .expect("modulation preroll must process");
            assert!(
                !warm.squelch_open,
                "the +1 dBFS warmup gate must stay closed"
            );
            meter.process(&input).expect("meter preroll must process");
            input_index += RF_BLOCK;
        }
        demodulator.squelch = SquelchState::new(
            SquelchMode::Manual {
                threshold_dbfs: THRESHOLD_DBFS,
            },
            AUDIO_RATE_HZ,
        )
        .expect("literal manual squelch must construct");
        let below_threshold_startup = demodulator
            .squelch
            .process(ChannelPowerSample {
                power_dbfs: THRESHOLD_DBFS - 20.0,
                frames: 96,
            })
            .expect("finite startup power must process");
        assert!(
            !below_threshold_startup.open,
            "96 startup frames below threshold must not count toward confirmation"
        );

        let mut emitted = Vec::new();
        for block_index in 0..6_usize {
            let input: Vec<IqSample> = (input_index..input_index + RF_BLOCK)
                .map(|index| am_sample(index, RF_RATE_HZ, 0.0, TONE_HZ, 0.25, 0.99))
                .collect();
            let measured = meter
                .process(&input)
                .expect("independent meter must process");
            assert_eq!(
                measured.iq.len(),
                96,
                "fixture pins one 96-frame power block"
            );
            let mean_power: f64 = measured
                .iq
                .iter()
                .map(|sample| f64::from(sample.norm_sqr()))
                .sum::<f64>()
                / 96.0;
            let independently_measured_dbfs = 10.0 * mean_power.log10();
            let minimum_dbfs = measured
                .iq
                .iter()
                .map(|sample| {
                    let power = f64::from(sample.norm_sqr());
                    if power > 0.0 {
                        10.0 * power.log10()
                    } else {
                        -200.0
                    }
                })
                .fold(f64::INFINITY, f64::min);
            assert!(
                independently_measured_dbfs >= f64::from(THRESHOLD_DBFS),
                "block {block_index} mean {independently_measured_dbfs:.2} dBFS must be eligible"
            );
            assert!(
                minimum_dbfs < f64::from(THRESHOLD_DBFS),
                "block {block_index} needs instantaneous minima below threshold, got {minimum_dbfs:.2} dBFS"
            );

            emitted.push(
                demodulator
                    .process(&input)
                    .expect("deeply modulated finite AM must process"),
            );
            input_index += RF_BLOCK;
        }

        for (index, block) in emitted[..5].iter().enumerate() {
            assert!(
                block.samples.iter().all(|sample| sample.to_bits() == 0),
                "eligible block {index} must remain closed through frame 479"
            );
        }
        assert!(
            emitted[4].squelch_open,
            "the transition completes after exactly five 96-frame blocks (480 frames)"
        );
        assert!(
            emitted[5]
                .samples
                .iter()
                .any(|sample| sample.to_bits() != 0),
            "the state reached at frame 480 must govern the next 96-frame block"
        );
    }

    fn assert_profile_noop_preserves_state(change_prebuffer: bool) {
        let selected = profile(
            AmRfProfile::Khz8,
            VoiceProfile::Standard,
            SquelchMode::Manual {
                threshold_dbfs: -30.0,
            },
        );
        let make = || {
            AmDemodulator::new(
                CENTER_HZ,
                CENTER_HZ,
                RF_RATE_HZ,
                AUDIO_RATE_HZ,
                selected.clone(),
            )
            .expect("literal profile must construct")
        };
        let mut reference = make();
        let mut changed = make();
        let mut input_index = 0_usize;
        for _ in 0..16 {
            let input = generated_block(input_index, RF_BLOCK, 0.0);
            reference.process(&input).expect("reference warmup");
            changed.process(&input).expect("candidate warmup");
            input_index += RF_BLOCK;
        }
        for demodulator in [&mut reference, &mut changed] {
            demodulator.squelch = SquelchState::new(selected.squelch, AUDIO_RATE_HZ)
                .expect("manual squelch reset must construct");
        }
        for _ in 0..4 {
            let input = generated_block(input_index, RF_BLOCK, 0.0);
            reference.process(&input).expect("reference confirmation");
            changed.process(&input).expect("candidate confirmation");
            input_index += RF_BLOCK;
        }
        assert!(
            !reference.squelch.is_open(),
            "384 eligible frames remain closed"
        );
        assert!(
            !changed.squelch.is_open(),
            "fixtures must be identical before update"
        );

        let mut replacement = selected.clone();
        if change_prebuffer {
            replacement.prebuffer_ms = 125;
        }
        changed
            .set_profile(replacement)
            .expect("no-DSP profile update must be valid");

        for continuation in 0..2_usize {
            let input = generated_block(input_index, RF_BLOCK, 0.0);
            let expected = reference.process(&input).expect("reference continuation");
            let actual = changed.process(&input).expect("candidate continuation");
            if continuation == 0 {
                assert!(expected.squelch_open, "384 + 96 eligible frames must open");
                assert!(
                    actual.squelch_open,
                    "a no-DSP profile update must preserve 384 confirmed frames"
                );
            }
            assert_audio_blocks_bit_identical(&actual, &expected, "profile no-op continuation");
            input_index += RF_BLOCK;
        }
    }

    #[test]
    fn prebuffer_only_profile_change_preserves_all_dsp_state() {
        assert_profile_noop_preserves_state(true);
    }

    #[test]
    fn output_clock_is_exact_and_empty_calls_advance_no_state() {
        let mut demodulator = AmDemodulator::new(
            CENTER_HZ,
            CENTER_HZ,
            RF_RATE_HZ,
            AUDIO_RATE_HZ,
            open_standard(),
        )
        .expect("literal clock fixture must construct");
        let before = demodulator.process(&[]).expect("empty input is valid");
        let after = demodulator
            .process(&[])
            .expect("repeated empty input is valid");
        assert_audio_blocks_bit_identical(&after, &before, "empty input state");

        let mut total_output = 0_usize;
        for block_index in 0..5_000_usize {
            let input = generated_block(block_index * RF_BLOCK, RF_BLOCK, 0.0);
            total_output += demodulator
                .process(&input)
                .expect("ten-second clock fixture must process")
                .samples
                .len();
        }
        assert_eq!(
            total_output, 480_000,
            "ten seconds at 2.048 MHz must emit exactly 480,000 frames at 48 kHz"
        );
    }

    #[test]
    fn identical_profile_is_a_bit_exact_noop() {
        assert_profile_noop_preserves_state(false);
    }

    #[test]
    fn rejected_extreme_finite_input_is_fully_transactional() {
        let make = || {
            AmDemodulator::new(
                CENTER_HZ,
                CENTER_HZ,
                RF_RATE_HZ,
                AUDIO_RATE_HZ,
                open_standard(),
            )
            .expect("literal profile must construct")
        };
        let mut reference = make();
        let mut rejected = make();
        let mut input_index = 0_usize;
        for _ in 0..12 {
            let input = generated_block(input_index, RF_BLOCK, 0.0);
            reference.process(&input).expect("reference warmup");
            rejected.process(&input).expect("candidate warmup");
            input_index += RF_BLOCK;
        }

        let extreme = vec![IqSample::new(1.0e30, 1.0e30); RF_BLOCK];
        assert!(
            rejected.process(&extreme).is_err(),
            "finite input whose downstream arithmetic overflows must return Err"
        );

        for continuation in 0..3_usize {
            let input = generated_block(input_index, RF_BLOCK, 0.0);
            let expected = reference.process(&input).expect("reference continuation");
            let actual = rejected.process(&input);
            assert!(
                actual.is_ok(),
                "rejected finite input must not poison continuation {continuation}: {actual:?}"
            );
            let actual = actual.expect("the assertion above establishes successful recovery");
            assert_audio_blocks_bit_identical(
                &actual,
                &expected,
                &format!("post-rejection continuation {continuation}"),
            );
            input_index += RF_BLOCK;
        }
    }

    #[test]
    fn voice_filter_response_matches_literal_profiles_without_agc() {
        const CASES: [(VoiceProfile, f64, f64, [f64; 2]); 3] = [
            (VoiceProfile::Narrow, 300.0, 2_800.0, [600.0, 2_200.0]),
            (VoiceProfile::Standard, 250.0, 3_400.0, [500.0, 2_800.0]),
            (VoiceProfile::Wide, 150.0, 4_000.0, [350.0, 3_400.0]),
        ];
        for (voice, low_hz, high_hz, passband_hz) in CASES {
            let reference = settled_voice_amplitude(low_hz, high_hz, 1_000.0);
            assert!(reference > 0.1, "{voice:?}: 1 kHz reference must pass");
            for frequency_hz in passband_hz {
                let amplitude = settled_voice_amplitude(low_hz, high_hz, frequency_hz);
                let ripple_db = 20.0 * (amplitude / reference).log10();
                assert!(
                    ripple_db.abs() <= 1.0,
                    "{voice:?}: literal passband tone {frequency_hz} Hz has {ripple_db:.2} dB ripple"
                );
            }
            for frequency_hz in [50.0, 6_000.0] {
                let amplitude = settled_voice_amplitude(low_hz, high_hz, frequency_hz);
                let rejection_db = 20.0 * (amplitude / reference).log10();
                assert!(
                    rejection_db <= -30.0,
                    "{voice:?}: literal stopband tone {frequency_hz} Hz is only {rejection_db:.2} dB down"
                );
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(8))]

        #[test]
        fn arbitrary_partitions_and_empty_calls_match_fixed_blocks(
            partitions in prop::collection::vec(0_usize..=2_048, 1..32)
        ) {
            const INPUT_FRAMES: usize = 32_768;
            let input: Vec<IqSample> = (0..INPUT_FRAMES)
                .map(|index| am_sample(index, RF_RATE_HZ, 13_750.0, TONE_HZ, 0.25, 0.8))
                .collect();
            let make = || AmDemodulator::new(
                CENTER_HZ,
                CENTER_HZ + 13_750.0,
                RF_RATE_HZ,
                AUDIO_RATE_HZ,
                open_standard(),
            ).expect("literal property fixture must construct");
            let mut fixed = make();
            let mut candidate = make();
            let mut expected = Vec::new();
            for chunk in input.chunks(RF_BLOCK) {
                expected.extend(fixed.process(chunk).expect("fixed reference must process").samples);
            }

            let mut actual = Vec::new();
            let empty = candidate
                .process(&[])
                .expect("every property case includes a zero-sized call");
            prop_assert!(empty.samples.is_empty());
            let mut offset = 0_usize;
            for requested in partitions {
                let count = requested.min(INPUT_FRAMES - offset);
                actual.extend(
                    candidate
                        .process(&input[offset..offset + count])
                        .expect("arbitrary valid partition must process")
                        .samples,
                );
                offset += count;
                if offset == INPUT_FRAMES {
                    break;
                }
            }
            actual.extend(
                candidate
                    .process(&input[offset..])
                    .expect("final remainder must process")
                    .samples,
            );
            prop_assert_eq!(actual.len(), expected.len());
            let difference: Vec<f32> = actual
                .iter()
                .zip(&expected)
                .map(|(actual, expected)| actual - expected)
                .collect();
            prop_assert!(rms(&difference) <= 1.0e-4);
        }
    }

    #[test]
    fn same_offset_retune_is_bit_exact_and_preserves_streaming_state() {
        // AM magnitude cannot observe a global NCO phase by itself. Comparing a same-offset retune
        // with an untouched streaming reference is stronger: any NCO phase or filter-history reset
        // creates a transient against the live complex-filter history and fails bit equality.
        const OFFSET_HZ: f64 = 17_350.0;
        let make = || {
            AmDemodulator::new(
                CENTER_HZ,
                CENTER_HZ + OFFSET_HZ,
                RF_RATE_HZ,
                AUDIO_RATE_HZ,
                open_standard(),
            )
            .expect("literal retune fixture must construct")
        };
        let mut reference = make();
        let mut retuned = make();
        let mut input_index = 0_usize;
        for _ in 0..16 {
            let input = generated_block(input_index, RF_BLOCK, OFFSET_HZ);
            reference.process(&input).expect("reference warmup");
            retuned.process(&input).expect("retuned warmup");
            input_index += RF_BLOCK;
        }
        retuned
            .retune(CENTER_HZ + 5_000.0, CENTER_HZ + 5_000.0 + OFFSET_HZ)
            .expect("finite same-offset retune must succeed");
        for continuation in 0..4_usize {
            let input = generated_block(input_index, RF_BLOCK, OFFSET_HZ);
            let expected = reference.process(&input).expect("reference continuation");
            let actual = retuned.process(&input).expect("retuned continuation");
            assert_audio_blocks_bit_identical(
                &actual,
                &expected,
                &format!("same-offset retune continuation {continuation}"),
            );
            input_index += RF_BLOCK;
        }
    }

    #[test]
    fn equivalent_khz6_voice_alias_preserves_channel_filter_history() {
        // Every 6 kHz voice alias derives the same literal RF FIR edges (2700/3300 Hz). Align the
        // legitimate voice-filter replacement independently in the reference, then compare the
        // channelizers directly so an enum-only RF FIR rebuild cannot hide behind that reset.
        let initial = profile(
            AmRfProfile::Khz6,
            VoiceProfile::Narrow,
            SquelchMode::Manual {
                threshold_dbfs: -80.0,
            },
        );
        let replacement = profile(
            AmRfProfile::Khz6,
            VoiceProfile::Standard,
            SquelchMode::Manual {
                threshold_dbfs: -80.0,
            },
        );
        let make = || {
            AmDemodulator::new(
                CENTER_HZ,
                CENTER_HZ,
                RF_RATE_HZ,
                AUDIO_RATE_HZ,
                initial.clone(),
            )
            .expect("literal 6 kHz alias fixture must construct")
        };
        let mut reference = make();
        let mut changed = make();
        let mut input_index = 0_usize;
        for _ in 0..16 {
            let input = generated_block(input_index, RF_BLOCK, 0.0);
            reference.process(&input).expect("reference warmup");
            changed.process(&input).expect("candidate warmup");
            input_index += RF_BLOCK;
        }
        assert!(
            reference.squelch.is_open(),
            "manual warmup must open reference"
        );
        assert!(
            changed.squelch.is_open(),
            "manual warmup must open candidate"
        );

        reference.voice_filter = VoiceBandpass::design(250.0, 2_600.0, 48_000.0)
            .expect("literal clamped Standard voice filter must design");
        reference.profile = replacement.clone();
        changed
            .set_profile(replacement)
            .expect("equivalent 6 kHz alias transition must apply");
        assert_eq!(
            reference.voice_filter.taps.len(),
            changed.voice_filter.taps.len()
        );
        for (index, (expected, actual)) in reference
            .voice_filter
            .taps
            .iter()
            .zip(&changed.voice_filter.taps)
            .enumerate()
        {
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "aligned Standard voice-filter tap {index}"
            );
        }

        let continuation = generated_block(input_index, RF_BLOCK, 0.0);
        let expected = reference
            .channelizer
            .process(&continuation)
            .expect("reference channelizer continuation");
        let actual = changed
            .channelizer
            .process(&continuation)
            .expect("updated channelizer continuation");
        assert_eq!(
            actual.iq.len(),
            expected.iq.len(),
            "channelized sample count"
        );
        for (index, (actual, expected)) in actual.iq.iter().zip(&expected.iq).enumerate() {
            assert_eq!(
                (actual.re.to_bits(), actual.im.to_bits()),
                (expected.re.to_bits(), expected.im.to_bits()),
                "equivalent RF edges must preserve channel FIR history at sample {index}"
            );
        }
        assert_eq!(
            actual.power_dbfs.to_bits(),
            expected.power_dbfs.to_bits(),
            "equivalent RF edges must preserve bit-identical channel power"
        );
    }

    #[test]
    fn voice_filter_reports_bounded_deterministic_latency() {
        for (low_hz, high_hz) in [(300.0, 2_800.0), (250.0, 3_400.0), (150.0, 4_000.0)] {
            let mut filter = VoiceBandpass::design(low_hz, high_hz, 48_000.0)
                .expect("literal voice profile must design");
            let reported_delay = filter.latency_samples();
            // These literals freeze the accepted 80 dB/transition design against tap-count or
            // latency mutations, including Standard's effective default 3400 Hz high edge.
            assert_eq!(
                filter.taps.len(),
                871,
                "every literal 48 kHz voice profile must retain exactly 871 taps"
            );
            assert_eq!(
                reported_delay, 435,
                "every literal 48 kHz voice profile must retain 435 samples of group delay"
            );
            let mut impulse = vec![0.0_f32; 4_096];
            impulse[0] = 1.0;
            let output = filter.process(&impulse);
            assert_eq!(output.len(), impulse.len());
            assert!(output.iter().all(|sample| sample.is_finite()));
            let peak = output
                .iter()
                .enumerate()
                .max_by(|(_, left), (_, right)| {
                    left.abs()
                        .partial_cmp(&right.abs())
                        .expect("finite impulse response is ordered")
                })
                .map(|(index, _)| index)
                .expect("impulse response is non-empty");
            assert_eq!(
                peak, reported_delay,
                "impulse peak must equal reported delay"
            );
        }

        let demodulator = AmDemodulator::new(
            CENTER_HZ,
            CENTER_HZ,
            RF_RATE_HZ,
            AUDIO_RATE_HZ,
            open_standard(),
        )
        .expect("literal demodulator must construct");
        assert_eq!(
            demodulator.channelizer.latency_samples(),
            199,
            "default channelizer latency must retain the literal 199-sample contract"
        );
        assert_eq!(
            demodulator.latency_samples(),
            634,
            "default AM latency must remain 199 channelizer + 435 voice samples"
        );
        #[allow(clippy::cast_precision_loss)]
        let total_latency_ms = demodulator.latency_samples() as f64 * 1_000.0 / 48_000.0;
        assert!(
            (total_latency_ms - 13.208_333_333_333_334).abs() <= 1.0e-12,
            "634/48000 seconds must remain 13.208333333333334 ms, got {total_latency_ms}"
        );
    }
}
