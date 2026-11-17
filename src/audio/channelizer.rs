//! Stateful AM channelizer: tuned I/Q to baseband, exact resampling, channel filtering.
//!
//! This is the first AM-specific stage of the receive chain described in
//! `docs/audio-quality-design.md`. A block of RF-rate I/Q goes through:
//!
//! ```text
//! continuous NCO at the input rate
//! → exact stateful complex resampler to the sink rate
//! → complex AM channel FIR selected by the RF profile
//! → pre-demodulation channel power meter
//! ```
//!
//! Resampling happens **before** the narrow channel filter on purpose: the resampler's anti-alias
//! response only has to reject content above the sink Nyquist frequency, so a 3–6 kHz complex
//! channel filter at the sink rate can reach 60 dB of rejection with a practical tap count. The
//! tuned channel must still lie fully inside the resampler passband, which
//! [`AmChannelizer::new`] enforces by rejecting an output rate above the input rate.
//!
//! # Units and conventions
//!
//! * Every frequency is hertz, as `f64`.
//! * Every latency, buffer length, and count is samples, as `usize`.
//! * `power_dbfs` is `10 * log10(mean(norm_sqr))` of the channelized I/Q, floored at −200 dBFS.
//!   A unit-amplitude complex tone therefore reads 0 dBFS.
//! * Output samples are produced at exactly [`AmChannelizer::output_rate_hz`]; the stream length
//!   follows the exact rational RF-to-sink clock, not a rounded integer decimation factor.
//! * Filter and resampler delay are reported together in output samples by
//!   [`AmChannelizer::latency_samples`].
//!
//! # Why the emitted sample count is exact
//!
//! [`ComplexResampler`] removes its fixed-block start-up delay by *skipping* the first
//! `fft_size_out / 2` output frames of the stream instead of interpolating them back. Those frames
//! describe input that has not arrived yet, so dropping them is correct in time, but it also means
//! the stream is short by that constant number of output samples forever. No amount of streaming
//! repays it: ten seconds of 2.048 MS/s input would lose 48 of its 480,000 sink samples at the
//! tail, which is exactly what the duration contract forbids.
//!
//! [`prime_by_skipped_delay`] repays it at construction time, honestly and without padding:
//!
//! 1. Silent input frames are fed one at a time until the resampler first emits output. The
//!    backend stages whole chunks and emits nothing before its first complete one, so this is
//!    exactly one backend input chunk: 4,096 frames for 2.048 MS/s → 48 kHz, 4,116 frames for
//!    44.1 kHz → 48 kHz.
//! 2. Everything it emitted while those silent frames were staged is discarded. Silent input can
//!    only produce silence, so no observable sample is lost, and one input chunk always maps to
//!    exactly one backend output block.
//!
//! Afterwards, for `C` = one backend input chunk in frames and `D` = one backend output block in
//! samples, `N` input frames emit
//!
//! ```text
//! (floor(N / C) + 1) * D - D = floor(N / C) * D
//! ```
//!
//! samples, because the priming prefix cancels exactly: `C * output_rate / input_rate` is an
//! integer `D`, so the discarded count `D - skipped_delay` plus the skipped `skipped_delay` is one
//! whole block. For `N` an integer number of backend chunks — every real capture block is, and ten
//! seconds of 2.048 MS/s input is 5,000 of them — that is also exactly
//! `N * output_rate / input_rate`, i.e. 480,000 samples, never 479,952, and it is a function of the
//! input duration alone: not of the caller's partitioning, and not of a final flush, which this
//! type never performs.
//!
//! Between chunk boundaries the resampler holds the unfinished chunk, so the instantaneous count
//! trails the ideal clock by at most one output block (96 samples at 48 kHz, about 2 ms). That lag
//! is bounded and never accumulates across a stream — it is repaid at the next chunk boundary — and
//! the type never pads, duplicates, or synthesises a sample to hide it. A caller that needs an
//! exact count at an arbitrary cut point feeds whole backend chunks, or accepts the sub-block
//! granularity that the fixed-rate backend fundamentally has.
//!
//! # What delay and transient remain
//!
//! Priming consumes silent input, so no observable content is lost — but the discarded prefix does
//! shift the origin of the delivered stream later. The stream is late by exactly one backend output
//! block minus the delay the adapter had already skipped, in output samples: 48 for
//! 2.048 MS/s → 48 kHz, 102 for 960 kS/s → 48 kHz. [`AmChannelizer::latency_samples`] includes that
//! shift, which is why it exceeds the filter and adapter delay alone. An unprimed stream would be
//! earlier by that same amount but short by it at the tail, which is precisely the duration error
//! the priming exists to remove.
//!
//! On top of that shift sits the ordinary start-up transient of any linear-phase cascade: the
//! resampler's overlap-add window and the channel FIR both fill with real input over the first
//! `latency_samples()` output samples. Callers measuring gain, phase, or alignment must discard at
//! least that prefix; later samples are unaffected.

// The AM pipeline starts consuming this module in Plan Task 10; until that wiring lands only the
// inline tests construct it, so a non-test build legitimately sees every item as unused.
#![cfg_attr(not(test), allow(dead_code))]

use anyhow::{anyhow, bail, Context, Result};

use crate::audio::profile::{AmRfProfile, EffectiveEdgesHz, VoiceProfile};
use crate::dsp::fir::{ComplexFir, LowPassSpec};
use crate::dsp::resampler::ComplexResampler;
use crate::types::IqSample;

/// Channel-filter stopband attenuation requested at design time, in decibels.
///
/// The plan's hard contract is 60 dB of achieved rejection at the profile stop edge, but the Kaiser
/// order estimate in [`crate::dsp::fir`] can land roughly a decibel short of what it was asked for.
/// Requesting 80 dB keeps the achieved stopband around −80 dB, far inside the contract instead of
/// straddling it.
const CHANNEL_STOPBAND_DB: f64 = 80.0;

/// Power reported for an empty or all-zero channelized block, in dBFS.
const POWER_FLOOR_DBFS: f64 = -200.0;

/// Upper bound on the silent input frames primed into the resampler at construction, in frames.
///
/// A backend that never emits output for a million silent input frames is broken, not slow; the
/// bound turns that into an error instead of an unbounded allocation.
const PRIMING_FRAME_LIMIT: usize = 1 << 20;

/// One channelized block: baseband I/Q at the sink rate plus the channel power it carries.
#[derive(Debug, Clone)]
pub(crate) struct ChannelizedBlock {
    /// Channelized complex baseband I/Q, in samples at [`AmChannelizer::output_rate_hz`].
    pub iq: Vec<IqSample>,
    /// Pre-demodulation channel power of this block, in dBFS (`10 * log10(mean(norm_sqr))`),
    /// floored at −200 dBFS for an all-zero block.
    pub power_dbfs: f32,
}

/// Stateful AM channelizer for one tuned channel.
///
/// All state — NCO phase, resampler history, channel-filter history — survives across
/// [`process`](Self::process) calls, so any input partition produces the same output stream.
/// Retuning changes the NCO frequency without discarding that state: oscillator phase stays
/// continuous, so a cursor change does not inject a click.
///
/// The AM pipeline consumes this in Plan Task 10; until that wiring lands, the inline tests in this
/// module are the only callers.
pub(crate) struct AmChannelizer {
    /// RF center frequency of the input stream, in hertz. Input sample 0 sits at baseband 0 Hz.
    center_hz: f64,
    /// Absolute tuned channel frequency, in hertz. The NCO mixes `channel_hz - center_hz` to 0 Hz.
    channel_hz: f64,
    /// Sample rate of the incoming I/Q, in hertz.
    input_rate_hz: f64,
    /// Sample rate of the produced channelized I/Q, in hertz.
    output_rate_hz: f64,
    /// Selected RF bandwidth profile.
    rf: AmRfProfile,
    /// Selected voice profile.
    voice: VoiceProfile,
    /// RF/voice edges derived from `rf` and `voice`, in hertz.
    edges: EffectiveEdgesHz,
    /// NCO phase accumulator in radians, wrapped to `[0, TAU)`. Never reset by a retune.
    nco_phase_rad: f64,
    /// Exact stateful complex resampler from `input_rate_hz` to `output_rate_hz`.
    resampler: Box<dyn ComplexResampler>,
    /// RF frames accepted so far, used to derive the exact delivered duration at end of stream.
    frames_in: u64,
    /// Channelized frames emitted so far, in output samples.
    frames_out: u64,
    /// Complex channel filter, designed at `output_rate_hz`.
    channel_fir: ComplexFir,
    /// Total delay of the delivered stream, in output samples: resampler residual buffering, the
    /// channel filter's group delay, and the silent prefix consumed while priming.
    latency_output_samples: usize,
    /// Silent output prefix discarded while priming, in output samples.
    priming_shift_samples: usize,
    /// Scratch buffer for the mixed-down block before resampling, at `input_rate_hz`.
    mixed: Vec<IqSample>,
    /// Scratch buffer for the resampled block before channel filtering, at `output_rate_hz`.
    resampled: Vec<IqSample>,
}

impl std::fmt::Debug for AmChannelizer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AmChannelizer")
            .field("center_hz", &self.center_hz)
            .field("channel_hz", &self.channel_hz)
            .field("input_rate_hz", &self.input_rate_hz)
            .field("output_rate_hz", &self.output_rate_hz)
            .field("rf", &self.rf)
            .field("voice", &self.voice)
            .field("latency_output_samples", &self.latency_output_samples)
            .finish_non_exhaustive()
    }
}

/// Rejects a frequency that is NaN or infinite, in hertz.
///
/// `label` names the offending argument in the error message.
fn require_finite_frequency(label: &str, value_hz: f64) -> Result<()> {
    if !value_hz.is_finite() {
        bail!("{label} must be finite, got {value_hz} Hz");
    }
    Ok(())
}

/// Validates a sample rate as a finite, positive, integral clock in hertz and returns it as `u32`.
///
/// `label` names the offending argument in the error message.
fn require_integral_rate(label: &str, rate_hz: f64) -> Result<u32> {
    if !rate_hz.is_finite() || rate_hz <= 0.0 {
        bail!("{label} must be finite and positive, got {rate_hz} Hz");
    }
    let rounded = rate_hz.round();
    if (rounded - rate_hz).abs() > f64::EPSILON {
        bail!("{label} must be an integral clock, got {rate_hz} Hz");
    }
    if rounded > f64::from(u32::MAX) {
        bail!("{label} does not fit an unsigned 32-bit hertz clock, got {rate_hz} Hz");
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    // range and sign checked above
    let rate = rounded as u32;
    if rate == 0 {
        bail!("{label} must be non-zero, got {rate_hz} Hz");
    }
    Ok(rate)
}

/// Designs the channel filter for `rf`/`voice` at `output_rate_hz`, in hertz.
///
/// Returns the filter together with the profile edges it was designed from, so the caller can store
/// both without recomputing them. The band edges come from
/// [`AmRfProfile::effective_edges_hz`]; nothing here hardcodes a profile frequency.
fn design_channel_filter(
    rf: AmRfProfile,
    voice: VoiceProfile,
    output_rate_hz: f64,
) -> Result<(ComplexFir, EffectiveEdgesHz)> {
    let edges = rf.effective_edges_hz(voice);
    let spec = LowPassSpec {
        sample_rate_hz: output_rate_hz,
        pass_edge_hz: f64::from(edges.pass_edge_hz),
        stop_edge_hz: f64::from(edges.stop_edge_hz),
        stopband_db: CHANNEL_STOPBAND_DB,
    };
    let filter = ComplexFir::design(spec).with_context(|| {
        format!(
            "failed to design the {rf:?}/{voice:?} channel filter at {output_rate_hz} Hz with \
             spec {spec:?}"
        )
    })?;
    Ok((filter, edges))
}

/// Combined group delay of `resampler` and `channel_fir`, in output samples.
///
/// Two parts add up:
///
/// * `resampler.latency_output_samples()` — the adapter's own residual buffering, in output
///   samples. The backend's fixed-block start-up delay is removed internally, so this is normally
///   zero; whatever the adapter does report still delays the stream by that many output samples.
/// * `channel_fir.group_delay_samples()` — half the FIR's impulse-response length, in output
///   samples, because the design is symmetric and therefore exactly linear phase.
/// * `priming_shift_samples` — the silent output prefix discarded by
///   [`prime_by_skipped_delay`], in output samples. It carries no content, but it moves the origin
///   of the delivered stream that far later, so a caller aligning output to input must skip it.
fn combined_latency_output_samples(
    resampler: &dyn ComplexResampler,
    channel_fir: &ComplexFir,
    priming_shift_samples: usize,
) -> usize {
    resampler
        .latency_output_samples()
        .saturating_add(channel_fir.group_delay_samples())
        .saturating_add(priming_shift_samples)
}

/// Repays the resampler's skipped start-up delay by priming it with silent input.
///
/// Feeds single silent input frames until the backend first emits output — exactly one backend
/// input chunk — and discards everything it emitted. Because silent input can only produce silence,
/// no observable sample is lost; the constant that had been missing from every block of the stream
/// is exchanged for a constant prefix that carries no signal. The module documentation derives why
/// the emitted count is then exactly `floor(N * output_rate / input_rate)`.
///
/// Returns the number of discarded output samples, in samples. That value is the stream's added
/// start-up shift and must be included in [`AmChannelizer::latency_samples`]: the discarded prefix
/// carries no content, but it still moves the origin of the delivered stream that many samples
/// later. It equals one backend output block minus the start-up delay the adapter already dropped.
///
/// # Errors
///
/// Returns an error when the resampler rejects the silent frames or when it produces no output
/// within [`PRIMING_FRAME_LIMIT`] frames, which means the backend is misconfigured rather than
/// merely slow.
fn prime_by_skipped_delay(resampler: &mut dyn ComplexResampler) -> Result<usize> {
    let silent_frame = [IqSample::new(0.0, 0.0)];
    let mut discarded: Vec<IqSample> = Vec::new();
    for _ in 0..PRIMING_FRAME_LIMIT {
        discarded.clear();
        resampler
            .process(&silent_frame, &mut discarded)
            .context("failed to prime the channel resampler with silence")?;
        if !discarded.is_empty() {
            // The discarded count is exactly one backend output block minus the start-up delay
            // the adapter had already swallowed. Throwing those samples away is what repays the
            // missing count, and it is also what delays the delivered stream by that many output
            // samples: the content itself is silence, but the stream origin moves later.
            return Ok(discarded.len());
        }
    }
    Err(anyhow!(
        "channel resampler emitted no output for {PRIMING_FRAME_LIMIT} silent input frames; its \
         backend staging is misconfigured"
    ))
}

/// Pre-demodulation channel power of `samples` in dBFS, floored at [`POWER_FLOOR_DBFS`].
///
/// `10 * log10(mean(norm_sqr))`, so a unit-amplitude complex tone reads 0 dBFS. An empty or
/// all-zero block has no finite logarithm and reports the floor instead.
fn channel_power_dbfs(samples: &[IqSample]) -> f32 {
    if samples.is_empty() {
        #[allow(clippy::cast_possible_truncation)] // the floor is a small exact constant
        return POWER_FLOOR_DBFS as f32;
    }
    let energy: f64 = samples
        .iter()
        .map(|sample| f64::from(sample.norm_sqr()))
        .sum();
    #[allow(clippy::cast_precision_loss)] // block lengths stay far below 2^53
    let mean_power = energy / samples.len() as f64;
    if !mean_power.is_finite() || mean_power <= 0.0 {
        #[allow(clippy::cast_possible_truncation)]
        return POWER_FLOOR_DBFS as f32;
    }
    let power_dbfs = 10.0 * mean_power.log10();
    #[allow(clippy::cast_possible_truncation)] // deliberate f64 accumulator -> f32 sink
    let clamped = power_dbfs.max(POWER_FLOOR_DBFS) as f32;
    clamped
}

impl AmChannelizer {
    /// Builds a channelizer for one tuned channel.
    ///
    /// `center_hz` is the RF center of the incoming stream and `channel_hz` the absolute frequency
    /// to tune; both are hertz. `input_rate_hz` and `output_rate_hz` are hertz and must be finite,
    /// positive, integral clocks with `input_rate_hz > output_rate_hz`. The channel filter is
    /// designed from `rf` and `voice` with the edges reported by
    /// [`AmRfProfile::effective_edges_hz`], at `output_rate_hz`.
    ///
    /// # Errors
    ///
    /// Returns an error when any frequency is not finite, either sample rate is zero, negative, or
    /// not an integral `u32` clock, `input_rate_hz <= output_rate_hz` (the narrow channel filter
    /// only has to reject above the sink Nyquist, which needs downsampling), or the derived filter
    /// specification is invalid.
    ///
    /// # Duration contract
    ///
    /// Construction absorbs the complex resampler's fixed-block start-up delay by priming it with
    /// one chunk of silent input and discarding what that produces (see the module documentation),
    /// so [`process`](Self::process) emits one output sample per `output_rate_hz / input_rate_hz`
    /// input samples from the very first block. Without that, ten seconds of input loses the
    /// skipped delay at the tail and misses the exact sink-rate count by 48 samples.
    pub fn new(
        center_hz: f64,
        channel_hz: f64,
        input_rate_hz: f64,
        output_rate_hz: f64,
        rf: AmRfProfile,
        voice: VoiceProfile,
    ) -> Result<Self> {
        require_finite_frequency("center_hz", center_hz)?;
        require_finite_frequency("channel_hz", channel_hz)?;
        let input_clock_hz = require_integral_rate("input_rate_hz", input_rate_hz)?;
        let output_clock_hz = require_integral_rate("output_rate_hz", output_rate_hz)?;
        if input_clock_hz <= output_clock_hz {
            bail!(
                "channelizer needs an input rate above the output rate so the channel filter only \
                 rejects above the sink Nyquist, got input_rate_hz={input_rate_hz} Hz and \
                 output_rate_hz={output_rate_hz} Hz"
            );
        }

        let (channel_fir, edges) = design_channel_filter(rf, voice, output_rate_hz)?;
        let mut resampler = Box::new(
            crate::dsp::resampler::create_complex_resampler(input_clock_hz, output_clock_hz)
                .with_context(|| {
                    format!(
                        "failed to create the {input_rate_hz} Hz -> {output_rate_hz} Hz complex \
                         resampler"
                    )
                })?,
        );
        let priming_shift_samples = prime_by_skipped_delay(&mut *resampler)?;
        let latency_output_samples =
            combined_latency_output_samples(&*resampler, &channel_fir, priming_shift_samples);

        Ok(Self {
            center_hz,
            channel_hz,
            input_rate_hz,
            output_rate_hz,
            rf,
            voice,
            edges,
            nco_phase_rad: 0.0,
            resampler,
            frames_in: 0,
            frames_out: 0,
            channel_fir,
            latency_output_samples,
            priming_shift_samples,
            mixed: Vec::new(),
            resampled: Vec::new(),
        })
    }

    /// Mixed, resampled, and channel-filtered a block of RF-rate I/Q.
    ///
    /// Accepts any block length, including an empty slice, and preserves all filter, resampler,
    /// and NCO state across calls. Returns channelized I/Q at [`output_rate_hz`](Self::output_rate_hz)
    /// and the channel power of that same block in dBFS.
    ///
    /// # Errors
    ///
    /// Returns an error when the block contains a non-finite I/Q sample, when the resampler or
    /// channel filter rejects the block, or when an internal length computation overflows. A
    /// non-finite sample is rejected here because the backend would otherwise panic instead of
    /// returning an error.
    pub fn process(&mut self, input: &[IqSample]) -> Result<ChannelizedBlock> {
        if input
            .iter()
            .any(|sample| !sample.re.is_finite() || !sample.im.is_finite())
        {
            bail!("channelizer input must contain only finite I/Q samples");
        }

        let offset_hz = self.channel_hz - self.center_hz;
        let phase_step_rad = (std::f64::consts::TAU * offset_hz / self.input_rate_hz)
            .rem_euclid(std::f64::consts::TAU);

        self.mixed.clear();
        self.mixed.reserve(input.len());
        let mut phase_rad = self.nco_phase_rad;
        for &sample in input {
            #[allow(clippy::cast_possible_truncation)]
            // rotation is applied at f32 sample precision
            let rotation = IqSample::new((-phase_rad).cos() as f32, (-phase_rad).sin() as f32);
            self.mixed.push(sample * rotation);
            phase_rad += phase_step_rad;
            if phase_rad >= std::f64::consts::TAU {
                phase_rad -= std::f64::consts::TAU;
            }
        }
        // The accumulator carries over: a retune changes the step, never the phase.
        self.nco_phase_rad = phase_rad;

        self.resampled.clear();
        self.resampler
            .process(&self.mixed, &mut self.resampled)
            .context("channel resampler failed")?;

        let iq = self.channel_fir.process(&self.resampled);
        #[allow(clippy::cast_possible_truncation)] // frame counters stay far below 2^53
        let delivered = iq.len() as u64;
        self.frames_in += input.len() as u64;
        self.frames_out += delivered;
        let power_dbfs = channel_power_dbfs(&iq);
        Ok(ChannelizedBlock { iq, power_dbfs })
    }

    /// Flushes the resampler tail so the stream's last frames are not lost at end of input.
    ///
    /// The rational resampler buffers up to one output interval of input. Without this call the
    /// final partial interval never reaches the channel filter, so a stream whose RF-to-sink ratio
    /// leaves a remainder ends short of its exact duration. Call it once, after the last input
    /// block, and discard the chain afterwards.
    ///
    /// The tail is trimmed to the exact rational duration: flushing can deliver a frame or two more
    /// than `ceil(frames_in * output_rate / input_rate) - frames_out`, and a stream must not exceed
    /// its own duration just because its filter history was longer than the remainder.
    ///
    /// # Errors
    ///
    /// Returns an error when the resampler rejects the flush.
    pub fn finish(&mut self) -> Result<ChannelizedBlock> {
        self.resampled.clear();
        self.resampler
            .flush(&mut self.resampled)
            .context("channel resampler flush failed")?;
        let mut tail = self.channel_fir.process(&self.resampled);

        let expected_total =
            Self::exact_output_frames(self.frames_in, self.input_rate_hz, self.output_rate_hz);
        let remaining = expected_total.saturating_sub(self.frames_out);
        #[allow(clippy::cast_possible_truncation)] // remaining is bounded by the flush length
        let keep = remaining.min(tail.len() as u64) as usize;
        tail.truncate(keep);
        self.frames_out += keep as u64;

        let power_dbfs = channel_power_dbfs(&tail);
        Ok(ChannelizedBlock {
            iq: tail,
            power_dbfs,
        })
    }

    /// Retunes the channel without resetting any DSP state.
    ///
    /// Both arguments are hertz: `center_hz` is the RF center of the incoming stream and
    /// `channel_hz` the absolute frequency to tune. The NCO phase accumulator must carry over, so
    /// the channelized stream stays continuous across the change; resampler and channel-filter
    /// history, profiles, and reported latency are unaffected.
    ///
    /// A pair is ignored unless both frequencies and their difference are finite: a finite pair
    /// whose difference overflows would turn the mixing step into `NaN` and poison every later
    /// sample, so the previous tuning stays in effect. Validate with [`new`](Self::new) instead.
    pub fn retune(&mut self, center_hz: f64, channel_hz: f64) {
        // Each value may be finite while their difference overflows, and `TAU * inf / rate` would
        // poison the accumulator with NaN forever. Reject the pair, not just the fields.
        let offset_hz = channel_hz - center_hz;
        if !center_hz.is_finite() || !channel_hz.is_finite() || !offset_hz.is_finite() {
            return;
        }
        self.center_hz = center_hz;
        self.channel_hz = channel_hz;
    }

    /// Replaces the RF and voice profiles, redesigning the channel filter only when its response
    /// changes.
    ///
    /// Several profile pairs derive the same pass and stop edges (for example every `Khz6` voice
    /// selection). In that case the live filter, its history, and the reported latency are kept,
    /// and only the profile metadata and effective voice cutoff are refreshed.
    ///
    /// The NCO phase, the resampler, and the reported output rate are untouched. The effective
    /// voice high cutoff follows [`AmRfProfile::effective_edges_hz`] and is reported by
    /// [`effective_voice_high_hz`](Self::effective_voice_high_hz).
    ///
    /// # Errors
    ///
    /// Returns an error when the newly derived filter specification is invalid. The previous
    /// filter then stays in use.
    pub fn set_profiles(&mut self, rf: AmRfProfile, voice: VoiceProfile) -> Result<()> {
        let edges = rf.effective_edges_hz(voice);
        let response_changed = self.edges.pass_edge_hz.to_bits() != edges.pass_edge_hz.to_bits()
            || self.edges.stop_edge_hz.to_bits() != edges.stop_edge_hz.to_bits();

        if response_changed {
            let (channel_fir, designed_edges) =
                design_channel_filter(rf, voice, self.output_rate_hz)?;
            let latency_output_samples = combined_latency_output_samples(
                &*self.resampler,
                &channel_fir,
                self.priming_shift_samples,
            );
            self.channel_fir = channel_fir;
            self.edges = designed_edges;
            self.latency_output_samples = latency_output_samples;
        } else {
            // Profile names are observable metadata even when they collapse to the same RF response.
            // Keep the live FIR history and update the equivalent effective edges in place.
            self.edges = edges;
        }
        self.rf = rf;
        self.voice = voice;
        Ok(())
    }

    /// Sample rate of the produced channelized I/Q, in hertz.
    ///
    /// Equal to the `output_rate_hz` passed to [`new`](Self::new); unchanged by retuning.
    #[must_use]
    pub fn output_rate_hz(&self) -> f64 {
        self.output_rate_hz
    }

    /// Group delay of the channel filter alone, in output samples.
    ///
    /// Exposed for tests that must distinguish the filter's linear-phase delay from the additional
    /// priming shift included in [`latency_samples`](Self::latency_samples).
    #[cfg(test)]
    #[must_use]
    pub fn channel_fir_group_delay_for_test(&self) -> usize {
        self.channel_fir.group_delay_samples()
    }

    /// Exact number of output frames `frames_in` RF frames represent at these clocks.
    ///
    /// Rounded up, so a stream never reports less than the duration it accepted, and computed in
    /// `u128` so no intermediate product overflows for any receiver rate the CLI accepts.
    fn exact_output_frames(frames_in: u64, input_rate_hz: f64, output_rate_hz: f64) -> u64 {
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let numerator = u128::from(frames_in) * (output_rate_hz.round() as u128);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let denominator = input_rate_hz.round() as u128;
        if denominator == 0 {
            return 0;
        }
        #[allow(clippy::cast_possible_truncation)] // the count fits the stream length
        let frames = numerator.div_ceil(denominator) as u64;
        frames
    }

    /// Total delay of the delivered stream, in **output** samples.
    ///
    /// It sums three contributions: the adapter's residual buffering, the channel filter's group
    /// delay, and the silent prefix consumed while priming the resampler. Output sample `n`
    /// therefore describes the channel content of input sample
    /// `(n - latency) * input_rate / output_rate`, so callers measuring gain, phase, or alignment
    /// must discard at least this many leading output samples. The value is a property of the
    /// design and does not depend on block partitioning; [`set_profiles`](Self::set_profiles)
    /// updates it when a new filter changes the group delay.
    #[must_use]
    pub fn latency_samples(&self) -> usize {
        self.latency_output_samples
    }

    /// Highest usable voice frequency after RF clamping, in hertz.
    ///
    /// This is `min(voice.high_cutoff_hz(), pass_edge_hz - 100)`, so narrow RF profiles report a
    /// value below the requested voice cutoff instead of silently ignoring the clamp. Units are
    /// hertz.
    #[must_use]
    pub fn effective_voice_high_hz(&self) -> f64 {
        f64::from(self.edges.effective_voice_high_hz)
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::unwrap_used
)]
mod tests {
    use std::f64::consts::TAU;

    use super::*;

    /// RF sample rate used by every test, in hertz (the receiver's default capture rate).
    const INPUT_RATE_HZ: f64 = 2_048_000.0;
    /// Sink sample rate used by every test, in hertz.
    const OUTPUT_RATE_HZ: f64 = 48_000.0;
    /// RF center frequency of the synthetic captures, in hertz.
    const CENTER_FREQ_HZ: f64 = 118_000_000.0;
    /// Nominal capture block size, in samples.
    const BLOCK_SAMPLES: usize = 2_048;
    /// Length of the spectral measurement fixture, in seconds.
    const FIXTURE_SECONDS: f64 = 0.2;
    /// Length of the retune fixture, in seconds.
    ///
    /// Twice the spectral fixture, so both halves keep a long settled window on either side of a
    /// mid-fixture retune even when the design's group delay is a few tens of milliseconds.
    const RETUNE_FIXTURE_SECONDS: f64 = 0.4;
    /// Floor applied to the settle prefix, in seconds.
    ///
    /// The measured prefix is derived from [`AmChannelizer::latency_samples`], but a report of zero
    /// would otherwise measure the resampler's own start-up transient, so a 20 ms floor is kept.
    const MIN_SETTLE_SECONDS: f64 = 0.02;
    /// Multiple of the reported latency discarded before measuring.
    ///
    /// Three times the group delay fills the delay line well past its first tap and keeps the
    /// resampler's own window overlap out of the measurement.
    const SETTLE_LATENCY_FACTOR: usize = 3;
    /// Channel spacing of the aviation band's 8.33 kHz raster, in hertz.
    const AVIATION_RASTER_HZ: f64 = 8_330.0;

    /// Converts a duration in seconds to a sample count at `rate_hz`.
    fn seconds_to_samples(seconds: f64, rate_hz: f64) -> usize {
        (seconds * rate_hz).round() as usize
    }

    /// Sum of complex tones evaluated on the absolute sample grid of the capture.
    ///
    /// A captured I/Q stream is already at baseband, so 0 Hz is the RF center: `tones` holds
    /// `(offset_hz, amplitude)` pairs where `offset_hz` is the carrier's offset from
    /// [`CENTER_FREQ_HZ`], not an absolute RF frequency. Mapping the absolute frequency instead
    /// would alias a 118 MHz carrier inside a 2.048 MS/s stream.
    ///
    /// `start_index` is the absolute sample position, so blocks generated separately join without a
    /// phase seam.
    fn tone_block(
        sample_rate_hz: f64,
        start_index: usize,
        len: usize,
        tones: &[(f64, f64)],
    ) -> Vec<IqSample> {
        (0..len)
            .map(|offset| {
                let time = (start_index + offset) as f64 / sample_rate_hz;
                let mut sample = IqSample::new(0.0, 0.0);
                for &(offset_hz, amplitude) in tones {
                    let phase = TAU * offset_hz * time;
                    sample += IqSample::new(
                        (amplitude * phase.cos()) as f32,
                        (amplitude * phase.sin()) as f32,
                    );
                }
                sample
            })
            .collect()
    }

    /// Root-mean-square amplitude of a complex block, in the units of the samples.
    fn amplitude(samples: &[IqSample]) -> f64 {
        assert!(
            !samples.is_empty(),
            "an RMS amplitude needs at least one sample"
        );
        let energy: f64 = samples
            .iter()
            .map(|sample| f64::from(sample.norm_sqr()))
            .sum();
        (energy / samples.len() as f64).sqrt()
    }

    /// Converts an amplitude or power ratio to decibels.
    fn to_db(ratio: f64) -> f64 {
        20.0 * ratio.log10()
    }

    /// Leading output samples to discard before measuring `channelizer`, in samples.
    ///
    /// Derived from the design's own reported latency rather than a fixed constant: a design that
    /// needs more settling than this must report that latency, and an implausible report then fails
    /// loudly in the window assertions instead of silently measuring a transient.
    fn settle_samples(channelizer: &AmChannelizer) -> usize {
        let floor = seconds_to_samples(MIN_SETTLE_SECONDS, OUTPUT_RATE_HZ);
        (channelizer.latency_samples() * SETTLE_LATENCY_FACTOR).max(floor)
    }

    /// Builds a channelizer with the rates and tuning every test shares.
    fn channelizer(rf: AmRfProfile, voice: VoiceProfile) -> AmChannelizer {
        AmChannelizer::new(
            CENTER_FREQ_HZ,
            CENTER_FREQ_HZ,
            INPUT_RATE_HZ,
            OUTPUT_RATE_HZ,
            rf,
            voice,
        )
        .expect("valid rates and profiles must construct a channelizer")
    }

    /// Builds the default-profile channelizer (8 kHz RF, Standard voice).
    fn default_channelizer() -> AmChannelizer {
        channelizer(AmRfProfile::Khz8, VoiceProfile::Standard)
    }

    /// Feeds `input` in fixed `block`-sized partitions and concatenates the channelized output.
    fn run(channelizer: &mut AmChannelizer, input: &[IqSample], block: usize) -> Vec<IqSample> {
        let mut output = Vec::new();
        for chunk in input.chunks(block) {
            let block = channelizer
                .process(chunk)
                .expect("channelizer must process synthetic IQ");
            output.extend_from_slice(&block.iq);
        }
        output
    }

    /// Feeds `input` in explicit partitions and concatenates the channelized output.
    fn run_partitioned(
        channelizer: &mut AmChannelizer,
        input: &[IqSample],
        sizes: &[usize],
    ) -> Vec<IqSample> {
        let mut output = Vec::new();
        let mut offset = 0;
        for &size in sizes {
            let end = offset + size;
            let block = channelizer
                .process(&input[offset..end])
                .expect("channelizer must process synthetic IQ");
            output.extend_from_slice(&block.iq);
            offset = end;
        }
        assert_eq!(
            offset,
            input.len(),
            "partitions must cover the input exactly"
        );
        output
    }

    /// Deterministic irregular partition sizes covering `total` samples.
    ///
    /// A local LCG keeps the partitions reproducible without adding a dependency.
    fn irregular_partitions(total: usize) -> Vec<usize> {
        let mut state = 0x6d2b_79f5_u32;
        let mut remaining = total;
        let mut sizes = Vec::new();
        while remaining > 0 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let requested = 1 + usize::try_from(state % 4_096).unwrap();
            let size = requested.min(remaining);
            sizes.push(size);
            remaining -= size;
        }
        sizes
    }

    /// Amplitude of the settled channel output for a single carrier at `offset_hz` from the tuned
    /// channel, in the units of the input amplitude.
    fn settled_amplitude(rf: AmRfProfile, voice: VoiceProfile, offset_hz: f64) -> f64 {
        let mut channelizer = channelizer(rf, voice);
        let input = tone_block(
            INPUT_RATE_HZ,
            0,
            seconds_to_samples(FIXTURE_SECONDS, INPUT_RATE_HZ),
            &[(offset_hz, 1.0)],
        );
        let settle = settle_samples(&channelizer);
        let output = run(&mut channelizer, &input, BLOCK_SAMPLES);
        assert!(
            output.len() > settle * 2,
            "a {} sample output leaves no steady-state window after a {settle} sample settle",
            output.len()
        );
        amplitude(&output[settle..])
    }

    #[test]
    fn am_carrier_at_channel_center_passes_with_unity_gain() {
        let mut channelizer = default_channelizer();
        let input = tone_block(
            INPUT_RATE_HZ,
            0,
            seconds_to_samples(FIXTURE_SECONDS, INPUT_RATE_HZ),
            &[(0.0, 1.0)],
        );

        let mut output = Vec::new();
        let mut last_power_dbfs = None;
        for chunk in input.chunks(BLOCK_SAMPLES) {
            let block = channelizer
                .process(chunk)
                .expect("channelizer must process a synthetic carrier");
            output.extend_from_slice(&block.iq);
            last_power_dbfs = Some(block.power_dbfs);
        }

        assert_eq!(
            channelizer.output_rate_hz(),
            OUTPUT_RATE_HZ,
            "channelized output must be produced at the requested sink rate"
        );

        let settle = settle_samples(&channelizer);
        assert!(
            output.len() > settle * 2,
            "a {} sample output leaves no steady-state window after a {settle} sample settle",
            output.len()
        );
        let gain_db = to_db(amplitude(&output[settle..]));
        assert!(
            gain_db.abs() <= 1.0,
            "an on-channel carrier measured {gain_db:.3} dB of gain, outside the +/-1 dB passband contract"
        );

        let power_dbfs = last_power_dbfs.expect("processing at least one block must report power");
        assert!(
            power_dbfs.is_finite(),
            "channel power must stay finite, got {power_dbfs} dBFS"
        );
        // A unit-amplitude complex tone is 0 dBFS, so a unity-gain channel reports the input level.
        assert!(
            (f64::from(power_dbfs) - gain_db).abs() <= 1.0,
            "reported channel power {power_dbfs} dBFS must describe the settled on-channel carrier, \
             which measured {gain_db:.3} dB relative to a 0 dBFS input tone"
        );
    }

    #[test]
    fn off_channel_carrier_at_8330_hz_is_rejected_by_sixty_db() {
        // The 8 kHz profile stops at 4.4 kHz, so the adjacent aviation channel on the 8.33 kHz
        // raster must be rejected by at least 60 dB once the filters have settled.
        let reference = settled_amplitude(AmRfProfile::Khz8, VoiceProfile::Standard, 0.0);
        assert!(
            reference > 0.5,
            "the on-channel reference carrier must pass to compare rejection against, measured {reference}"
        );

        for offset_hz in [AVIATION_RASTER_HZ, -AVIATION_RASTER_HZ] {
            let measured = settled_amplitude(AmRfProfile::Khz8, VoiceProfile::Standard, offset_hz);
            let rejection_db = to_db(measured / reference);
            assert!(
                rejection_db <= -60.0,
                "a carrier {offset_hz} Hz from the tuned channel appeared at {rejection_db:.2} dB \
                 relative to the on-channel reference, which is less than the 60 dB the 8 kHz \
                 profile promises"
            );
        }
    }

    #[test]
    fn every_rf_profile_meets_its_pass_and_stop_contract() {
        let voice = VoiceProfile::Standard;
        for rf in [
            AmRfProfile::Khz6,
            AmRfProfile::Khz8,
            AmRfProfile::Khz10,
            AmRfProfile::Khz12,
        ] {
            let bandwidth_hz = f64::from(rf.bandwidth_hz());
            let voice_high_hz = f64::from(voice.high_cutoff_hz());
            // Derived from the profile API rather than hardcoded, so a profile change cannot leave
            // this test asserting the old numbers.
            let pass_edge_hz = (voice_high_hz + 200.0).min(0.45 * bandwidth_hz);
            let stop_edge_hz = 0.55 * bandwidth_hz;
            let reported = rf.effective_edges_hz(voice);
            assert!(
                (f64::from(reported.pass_edge_hz) - pass_edge_hz).abs() <= 0.01,
                "{rf:?}: derived pass edge {pass_edge_hz} Hz disagrees with the profile API's {} Hz",
                reported.pass_edge_hz
            );
            assert!(
                (f64::from(reported.stop_edge_hz) - stop_edge_hz).abs() <= 0.01,
                "{rf:?}: derived stop edge {stop_edge_hz} Hz disagrees with the profile API's {} Hz",
                reported.stop_edge_hz
            );

            let channelizer = channelizer(rf, voice);
            assert!(
                (channelizer.effective_voice_high_hz()
                    - f64::from(reported.effective_voice_high_hz))
                .abs()
                    <= 0.5,
                "{rf:?}: channelizer must report the profile API's effective voice cutoff of {} Hz",
                reported.effective_voice_high_hz
            );

            let reference = settled_amplitude(rf, voice, 0.0);
            let pass = settled_amplitude(rf, voice, pass_edge_hz);
            let pass_gain_db = to_db(pass / reference);
            assert!(
                pass_gain_db.abs() <= 1.0,
                "{rf:?}: a carrier at the {pass_edge_hz} Hz pass edge measured {pass_gain_db:.3} dB, \
                 outside the +/-1 dB passband contract"
            );

            let stop = settled_amplitude(rf, voice, stop_edge_hz);
            let stop_gain_db = to_db(stop / reference);
            assert!(
                stop_gain_db <= -60.0,
                "{rf:?}: a carrier at the {stop_edge_hz} Hz stop edge measured {stop_gain_db:.2} dB, \
                 less than the 60 dB the profile promises"
            );
        }
    }

    #[test]
    fn retuning_preserves_oscillator_continuity() {
        // Two carriers are present for the whole fixture, each offset from a different channel, so
        // the tuned channel is identifiable from the output content alone:
        //
        // * carrier A at channel A + 1,000 Hz,
        // * carrier B at channel B + 2,400 Hz, with channel B 20 kHz above channel A.
        //
        // Both channels sit *off* the stream center (never at 0 Hz offset), so the NCO is genuinely
        // running and carries a non-zero phase at the retune instant. A channel at 0 Hz offset would
        // leave the oscillator still, making a phase reset indistinguishable from correct behavior.
        // The retune instant is also offset by one block so the accumulated phase is not an exact
        // multiple of one cycle for the same reason.
        //
        // 20 kHz puts carrier B at 22.4 kHz in the 48 kHz channel domain: below Nyquist, so it
        // aliases nowhere near the voice band, yet far outside the 8 kHz profile's 4.4 kHz stop
        // edge, so it is filtered out of a channel-A output and kept by a channel-B output. A
        // retune that did nothing would keep emitting channel A's 1 kHz tone and be caught by the
        // reference comparison below, not just by the step measurement.
        //
        // Each entry is `(offset of the channel from the stream center, carrier offset above that
        // channel)`, in hertz. Offsets are relative to CENTER_FREQ_HZ because a capture is already
        // at baseband. The 50 Hz steps off the round 10 kHz / 30 kHz raster are what keep the
        // retune instant away from a whole number of oscillator cycles.
        let first = (10_050.0, 1_000.0);
        let second = (30_050.0, 2_400.0);
        let total_input = seconds_to_samples(RETUNE_FIXTURE_SECONDS, INPUT_RATE_HZ);
        let retune_at = total_input / 2 + BLOCK_SAMPLES;
        assert_eq!(
            retune_at % BLOCK_SAMPLES,
            0,
            "the retune must land on a block boundary so both halves share the reference partition"
        );
        assert!(
            retune_at < total_input,
            "the retune must land inside the fixture"
        );
        let input = tone_block(
            INPUT_RATE_HZ,
            0,
            total_input,
            &[(first.0 + first.1, 0.5), (second.0 + second.1, 0.5)],
        );

        let mut retuned = AmChannelizer::new(
            CENTER_FREQ_HZ,
            CENTER_FREQ_HZ + first.0,
            INPUT_RATE_HZ,
            OUTPUT_RATE_HZ,
            AmRfProfile::Khz8,
            VoiceProfile::Standard,
        )
        .expect("valid rates and profiles must construct a channelizer");
        let output_rate_before = retuned.output_rate_hz();
        let voice_high_before = retuned.effective_voice_high_hz();
        let latency = retuned.latency_samples();

        let mut output = Vec::new();
        for chunk in input[..retune_at].chunks(BLOCK_SAMPLES) {
            output.extend_from_slice(&retuned.process(chunk).expect("first half must process").iq);
        }
        let boundary = output.len();
        retuned.retune(CENTER_FREQ_HZ, CENTER_FREQ_HZ + second.0);
        for chunk in input[retune_at..].chunks(BLOCK_SAMPLES) {
            output.extend_from_slice(&retuned.process(chunk).expect("second half must process").iq);
        }

        assert_eq!(
            retuned.output_rate_hz(),
            output_rate_before,
            "retuning must not change the output sample rate"
        );
        assert_eq!(
            retuned.effective_voice_high_hz(),
            voice_high_before,
            "retuning must not change the effective voice cutoff"
        );

        let settle = settle_samples(&retuned);
        assert!(
            boundary > settle * 2,
            "the retune boundary at {boundary} leaves no settled pre-retune window of {settle} \
             samples; latency_samples() reports {latency}"
        );
        assert!(
            boundary + settle < output.len(),
            "the retune boundary at {boundary} leaves no settled post-retune window of {settle} \
             samples inside {} emitted samples",
            output.len()
        );

        // References: the same input through instances that were never retuned, one per channel.
        let reference = |channel_hz: f64| {
            let mut instance = AmChannelizer::new(
                CENTER_FREQ_HZ,
                channel_hz,
                INPUT_RATE_HZ,
                OUTPUT_RATE_HZ,
                AmRfProfile::Khz8,
                VoiceProfile::Standard,
            )
            .expect("valid rates and profiles must construct a channelizer");
            run(&mut instance, &input, BLOCK_SAMPLES)
        };
        let reference_a = reference(CENTER_FREQ_HZ + first.0);
        let reference_b = reference(CENTER_FREQ_HZ + second.0);
        assert_eq!(
            reference_a.len().abs_diff(output.len()),
            0,
            "retuning changed the output length: {} samples against {}",
            output.len(),
            reference_a.len()
        );
        assert_eq!(reference_b.len(), reference_a.len());

        // Root-mean-square distance from a never-retuned reference over `[from, until)`, reported
        // with that reference's own level so the gate is a relative error, not an absolute
        // amplitude. The range matters: each half must be compared only against the reference for
        // the channel that is tuned during that half.
        let relative_difference =
            |candidate: &[IqSample], expected: &[IqSample], from: usize, until: usize| {
                let end = until.min(candidate.len()).min(expected.len());
                assert!(
                    from < end,
                    "the measurement window [{from}, {end}) is empty"
                );
                let energy: f64 = candidate[from..end]
                    .iter()
                    .zip(&expected[from..end])
                    .map(|(actual, wanted)| f64::from((*actual - *wanted).norm_sqr()))
                    .sum();
                let rms = (energy / (end - from) as f64).sqrt();
                (rms, amplitude(&expected[from..end]))
            };

        // The pre-retune half must look like channel A. This proves the fixture really separates
        // the two carriers, so the references below are meaningful measurements.
        let (pre_rms, pre_level) = relative_difference(&output, &reference_a, settle, boundary);
        assert!(
            pre_rms <= 0.1 * pre_level,
            "before retuning the stream differed from a channel-A reference by {pre_rms:.6} RMS \
             against a {pre_level:.6} level; the fixture must isolate the two carriers"
        );

        // Continuity: the output lags the input by `latency_samples`, so a retune taking effect at
        // input time `retune_at` shows up anywhere in `[boundary, boundary + latency]`. Scan that
        // window and compare its worst single-sample step against the settled post-retune median.
        //
        // Rationale for the 10x tolerance: a settled 2.4 kHz complex exponential at amplitude 0.5
        // advances |delta| = 2*A*sin(pi*f/fs) ~ 0.157 per sample, so 10x is ~1.57. That admits any
        // legitimate sub-sample realignment of the resampler output grid while staying far below
        // the ~0.5 jump a reset phase accumulator would inject.
        let window_start = boundary.saturating_sub(1);
        let window_end = (boundary + latency + 1).min(output.len() - 1);
        let worst_transition_step = output[window_start..=window_end]
            .windows(2)
            .map(|pair| f64::from((pair[1] - pair[0]).norm()))
            .fold(0.0_f64, f64::max);

        let mut settled_steps: Vec<f64> = output[boundary + settle..]
            .windows(2)
            .map(|pair| f64::from((pair[1] - pair[0]).norm()))
            .collect();
        settled_steps.sort_by(|left, right| {
            left.partial_cmp(right)
                .expect("synthetic tone steps are finite and ordered")
        });
        let median_step = settled_steps[settled_steps.len() / 2];
        assert!(
            median_step > 0.0,
            "a settled 2.4 kHz baseband tone must advance between samples"
        );
        assert!(
            worst_transition_step <= 10.0 * median_step,
            "across the retune the worst sample-to-sample step was {worst_transition_step:.6} \
             against a {median_step:.6} settled median step, which is a discontinuity rather than a \
             continuous oscillator"
        );

        // The post-retune half must look like channel B. Ignoring the retune would leave channel A's
        // 1 kHz tone, whose distance from this reference is on the order of the level itself.
        let (post_rms, post_level) =
            relative_difference(&output, &reference_b, boundary + settle, output.len());
        assert!(
            post_rms <= 0.1 * post_level,
            "after retuning the stream differed from a channel-B reference by {post_rms:.6} RMS \
             against a {post_level:.6} level, so the tuned channel did not follow the retune"
        );
    }

    #[test]
    fn ten_seconds_produces_exact_sink_rate_sample_count() {
        // The slowest test in this module: 20.48 million complex input samples. It is the duration
        // contract, so it stays at ten seconds rather than a fraction.
        let mut channelizer = default_channelizer();
        let total_input = seconds_to_samples(10.0, INPUT_RATE_HZ);
        assert_eq!(
            total_input % BLOCK_SAMPLES,
            0,
            "the fixture must be an exact number of capture blocks"
        );

        let mut produced = 0_usize;
        let mut index = 0_usize;
        while index < total_input {
            let block = tone_block(INPUT_RATE_HZ, index, BLOCK_SAMPLES, &[(0.0, 1.0)]);
            produced += channelizer
                .process(&block)
                .expect("channelizer must process a synthetic carrier")
                .iq
                .len();
            index += BLOCK_SAMPLES;
        }

        assert_eq!(
            produced, 480_000,
            "ten seconds of {INPUT_RATE_HZ} Hz input must produce exactly ten seconds at \
             {OUTPUT_RATE_HZ} Hz, not a count rounded through an integer decimation factor"
        );
    }

    #[test]
    fn output_is_invariant_to_arbitrary_block_partitions() {
        let input = tone_block(
            INPUT_RATE_HZ,
            0,
            seconds_to_samples(FIXTURE_SECONDS, INPUT_RATE_HZ),
            &[(700.0, 0.5), (2_100.0, 0.2)],
        );

        let mut reference_channelizer = default_channelizer();
        let reference = run(&mut reference_channelizer, &input, BLOCK_SAMPLES);
        // Reported latency is a property of the design, not of the partitioning, so both streams
        // carry the same delay; skipping it from both keeps the comparison honest if the design
        // changes and documents which prefix is transient. `settle_samples` already covers three
        // times the reported latency, so it subsumes the delay itself.
        let skip = settle_samples(&reference_channelizer);
        assert!(
            skip * 2 < reference.len(),
            "a {skip} sample skip leaves no window inside {} samples",
            reference.len()
        );

        let mut cases: Vec<(String, Vec<IqSample>)> = Vec::new();
        for block in [257_usize, 1_024, 2_048, 4_096] {
            let mut instance = default_channelizer();
            cases.push((
                format!("{block}-sample blocks"),
                run(&mut instance, &input, block),
            ));
        }
        let mut instance = default_channelizer();
        cases.push((
            "random partitions".to_string(),
            run_partitioned(&mut instance, &input, &irregular_partitions(input.len())),
        ));

        for (label, candidate) in cases {
            assert!(
                candidate.len().abs_diff(reference.len()) <= 1,
                "{label}: output length {} differs from the reference length {}",
                candidate.len(),
                reference.len()
            );
            let aligned = candidate.len().min(reference.len());
            let energy: f64 = reference[skip..aligned]
                .iter()
                .zip(&candidate[skip..aligned])
                .map(|(expected, actual)| f64::from((*expected - *actual).norm_sqr()))
                .sum();
            // 1e-4 rather than the resampler-only 1e-5 gate: this stream is the cascade of a
            // resampler and a channel filter, so rounding from both stages accumulates.
            let rms = (energy / (aligned - skip) as f64).sqrt();
            assert!(
                rms <= 1.0e-4,
                "{label}: RMS difference {rms:.3e} from the reference exceeds the 1e-4 gate"
            );
        }
    }

    #[test]
    fn equivalent_edge_profile_change_updates_metadata_without_rebuilding_fir() {
        let initial_rf = AmRfProfile::Khz6;
        let initial_voice = VoiceProfile::Narrow;
        let updated_rf = AmRfProfile::Khz6;
        let updated_voice = VoiceProfile::Standard;
        let initial_edges = initial_rf.effective_edges_hz(initial_voice);
        let updated_edges = updated_rf.effective_edges_hz(updated_voice);
        assert_eq!(initial_edges.pass_edge_hz, updated_edges.pass_edge_hz);
        assert_eq!(initial_edges.stop_edge_hz, updated_edges.stop_edge_hz);
        assert_eq!(updated_edges.pass_edge_hz, 2_700.0);
        assert_eq!(updated_edges.stop_edge_hz, 3_300.0);

        let mut changed = channelizer(initial_rf, initial_voice);
        let mut reference = channelizer(initial_rf, initial_voice);
        let warm_len = 8 * 4_096;
        let warm = tone_block(
            INPUT_RATE_HZ,
            0,
            warm_len,
            &[(350.0, 0.55), (1_900.0, 0.25), (5_100.0, 0.15)],
        );
        let changed_warm = changed
            .process(&warm)
            .expect("changed channelizer must accept the warm-up stream");
        let reference_warm = reference
            .process(&warm)
            .expect("reference channelizer must accept the warm-up stream");
        assert_eq!(
            changed_warm.iq, reference_warm.iq,
            "identical channelizers must have identical FIR history before the profile update"
        );
        assert_eq!(changed_warm.power_dbfs, reference_warm.power_dbfs);

        let latency_before = changed.latency_samples();
        changed
            .set_profiles(updated_rf, updated_voice)
            .expect("an equivalent-edge profile update must succeed");

        assert_eq!(changed.rf, AmRfProfile::Khz6);
        assert_eq!(changed.voice, VoiceProfile::Standard);
        assert_eq!(changed.edges, updated_edges);
        assert_eq!(changed.effective_voice_high_hz(), 2_600.0);
        assert_eq!(
            changed.latency_samples(),
            latency_before,
            "equivalent RF pass/stop edges must keep the design latency unchanged"
        );

        let continuation = tone_block(
            INPUT_RATE_HZ,
            warm_len,
            2 * 4_096,
            &[(350.0, 0.55), (1_900.0, 0.25), (5_100.0, 0.15)],
        );
        let changed_output = changed
            .process(&continuation)
            .expect("updated channelizer must process the continuation");
        let reference_output = reference
            .process(&continuation)
            .expect("reference channelizer must process the continuation");
        assert!(
            !changed_output.iq.is_empty(),
            "the continuation must emit IQ"
        );
        assert_eq!(changed_output.iq.len(), reference_output.iq.len());
        for (index, (actual, expected)) in changed_output
            .iq
            .iter()
            .zip(&reference_output.iq)
            .enumerate()
        {
            assert_eq!(
                actual, expected,
                "equivalent RF edges must preserve FIR history; output differs at sample {index}"
            );
        }
        assert_eq!(
            changed_output.power_dbfs, reference_output.power_dbfs,
            "bit-identical channelized IQ must report bit-identical power"
        );
    }

    #[test]
    fn finishing_a_chain_delivers_the_exact_rational_duration() {
        // 960 kHz and 1.2 MHz are receiver rates the CLI accepts, and both leave a remainder when
        // divided into 48 kHz. The resampler holds that last partial interval until it is flushed.
        for input_rate_hz in [960_000.0_f64, 1_024_000.0, 1_200_000.0, 2_048_000.0] {
            let seconds = 2.0_f64;
            let total = (input_rate_hz * seconds) as usize;
            let expected = ((total as f64) * OUTPUT_RATE_HZ / input_rate_hz).round() as usize;
            let mut channelizer = AmChannelizer::new(
                CENTER_FREQ_HZ,
                CENTER_FREQ_HZ,
                input_rate_hz,
                OUTPUT_RATE_HZ,
                AmRfProfile::Khz8,
                VoiceProfile::Standard,
            )
            .expect("receiver rate must construct a channelizer");
            let mut produced = 0_usize;
            let mut index = 0_usize;
            while index < total {
                let count = (total - index).min(BLOCK_SAMPLES);
                let block = tone_block(input_rate_hz, index, count, &[(1_500.0, 0.5)]);
                produced += channelizer
                    .process(&block)
                    .expect("finite input must process")
                    .iq
                    .len();
                index += count;
            }
            let tail = channelizer
                .finish()
                .expect("flushing the chain must succeed");
            produced += tail.iq.len();
            assert_eq!(
                produced, expected,
                "{input_rate_hz} Hz for {seconds} s must deliver exactly {expected} frames at \
                 {OUTPUT_RATE_HZ} Hz, but produced {produced} without the resampler tail"
            );
        }
    }

    #[test]
    fn effective_voice_high_is_clamped_for_narrow_rf_profiles() {
        let rf = AmRfProfile::Khz6;
        let voice = VoiceProfile::Wide;
        let bandwidth_hz = f64::from(rf.bandwidth_hz());
        let requested_hz = f64::from(voice.high_cutoff_hz());
        let pass_edge_hz = (requested_hz + 200.0).min(0.45 * bandwidth_hz);
        let expected_hz = requested_hz.min(pass_edge_hz - 100.0);

        let channelizer = channelizer(rf, voice);
        let reported_hz = channelizer.effective_voice_high_hz();
        assert!(
            (reported_hz - expected_hz).abs() <= 0.5,
            "{rf:?}/{voice:?}: reported effective voice cutoff {reported_hz} Hz, expected the \
             documented clamp {expected_hz} Hz from a pass edge of {pass_edge_hz} Hz"
        );
        assert!(
            reported_hz < requested_hz,
            "{rf:?}/{voice:?}: a 6 kHz RF profile must report the clamp below the requested \
             {requested_hz} Hz instead of ignoring it"
        );
        let api_hz = f64::from(rf.effective_edges_hz(voice).effective_voice_high_hz);
        assert!(
            (reported_hz - api_hz).abs() <= 0.5,
            "{rf:?}/{voice:?}: reported {reported_hz} Hz must match the profile API's {api_hz} Hz"
        );
    }

    #[test]
    fn invalid_rates_and_profiles_are_errors_not_panics() {
        let cases: [(&str, f64, f64, f64, f64); 11] = [
            (
                "zero input rate",
                CENTER_FREQ_HZ,
                CENTER_FREQ_HZ,
                0.0,
                OUTPUT_RATE_HZ,
            ),
            (
                "negative input rate",
                CENTER_FREQ_HZ,
                CENTER_FREQ_HZ,
                -INPUT_RATE_HZ,
                OUTPUT_RATE_HZ,
            ),
            (
                "NaN input rate",
                CENTER_FREQ_HZ,
                CENTER_FREQ_HZ,
                f64::NAN,
                OUTPUT_RATE_HZ,
            ),
            (
                "infinite input rate",
                CENTER_FREQ_HZ,
                CENTER_FREQ_HZ,
                f64::INFINITY,
                OUTPUT_RATE_HZ,
            ),
            (
                "zero output rate",
                CENTER_FREQ_HZ,
                CENTER_FREQ_HZ,
                INPUT_RATE_HZ,
                0.0,
            ),
            (
                "NaN output rate",
                CENTER_FREQ_HZ,
                CENTER_FREQ_HZ,
                INPUT_RATE_HZ,
                f64::NAN,
            ),
            (
                "infinite output rate",
                CENTER_FREQ_HZ,
                CENTER_FREQ_HZ,
                INPUT_RATE_HZ,
                f64::INFINITY,
            ),
            (
                "input rate below output rate",
                CENTER_FREQ_HZ,
                CENTER_FREQ_HZ,
                24_000.0,
                OUTPUT_RATE_HZ,
            ),
            (
                "NaN center frequency",
                f64::NAN,
                CENTER_FREQ_HZ,
                INPUT_RATE_HZ,
                OUTPUT_RATE_HZ,
            ),
            (
                "infinite center frequency",
                f64::INFINITY,
                CENTER_FREQ_HZ,
                INPUT_RATE_HZ,
                OUTPUT_RATE_HZ,
            ),
            (
                "NaN channel frequency",
                CENTER_FREQ_HZ,
                f64::NAN,
                INPUT_RATE_HZ,
                OUTPUT_RATE_HZ,
            ),
        ];

        for (label, center_hz, channel_hz, input_rate_hz, output_rate_hz) in cases {
            let result = AmChannelizer::new(
                center_hz,
                channel_hz,
                input_rate_hz,
                output_rate_hz,
                AmRfProfile::Khz8,
                VoiceProfile::Standard,
            );
            assert!(
                result.is_err(),
                "{label}: invalid configuration must return Err rather than panic or construct a \
                 channelizer"
            );
        }
    }

    #[test]
    fn reported_latency_matches_measured_impulse_delay() {
        // Priming shifts the delivered stream later by the count it discards, so `latency_samples`
        // must include that shift. A single impulse at a known input frame is the unambiguous
        // measurement: its output peak lands at the ideal output time plus the true delay.
        let mut channelizer = default_channelizer();
        let latency = channelizer.latency_samples();
        let impulse_frame = 3 * BLOCK_SAMPLES;
        let total_input = 8 * BLOCK_SAMPLES;
        let mut input = vec![IqSample::new(0.0, 0.0); total_input];
        input[impulse_frame] = IqSample::new(1.0, 0.0);

        let output = run(&mut channelizer, &input, BLOCK_SAMPLES);
        let ideal_output = impulse_frame * OUTPUT_RATE_HZ as usize / INPUT_RATE_HZ as usize;
        let peak = output
            .iter()
            .enumerate()
            .map(|(index, sample)| (index, f64::from(sample.norm())))
            .fold((0_usize, 0.0_f64), |best, candidate| {
                if candidate.1 > best.1 {
                    candidate
                } else {
                    best
                }
            })
            .0;

        let measured = f64::from(u32_from_usize(peak - ideal_output));
        let reported = f64::from(u32_from_usize(latency));
        assert!(
            (measured - reported).abs() <= 2.0,
            "impulse peak at output {peak} (delay {measured}) must match the reported latency \
             {reported}, within the resampler's sub-sample reconstruction"
        );
        assert!(
            latency > channelizer.channel_fir_group_delay_for_test(),
            "reported latency {latency} must exceed the filter group delay alone; the priming \
             shift is part of the true delay and omitting it under-reports by that amount"
        );
    }

    #[test]
    fn overflowing_retune_is_ignored_instead_of_poisoning_the_nco() {
        let mut channelizer = default_channelizer();
        let before = channelizer.latency_samples();
        // Both values are finite, but their difference overflows; accepting it would make the
        // mixing step NaN forever and make the backend panic on the next block.
        channelizer.retune(1e308, -1e308);
        let block = tone_block(INPUT_RATE_HZ, 0, BLOCK_SAMPLES, &[(0.0, 0.5)]);
        let output = channelizer
            .process(&block)
            .expect("an ignored retune must leave the channelizer usable");
        assert!(
            output
                .iq
                .iter()
                .all(|sample| sample.re.is_finite() && sample.im.is_finite()),
            "output after an overflowing retune must stay finite"
        );
        assert_eq!(
            channelizer.latency_samples(),
            before,
            "an ignored retune must not change the design latency"
        );
    }

    #[test]
    fn non_finite_input_samples_are_an_error_not_a_panic() {
        let mut channelizer = default_channelizer();
        for sample in [
            IqSample::new(f32::NAN, 0.0),
            IqSample::new(0.0, f32::INFINITY),
        ] {
            let block = [sample];
            assert!(
                channelizer.process(&block).is_err(),
                "a non-finite sample {sample:?} must be rejected with Err, never reach the backend"
            );
        }
    }

    /// `usize` to `u32` for error messages where the value is a small sample count.
    fn u32_from_usize(value: usize) -> u32 {
        u32::try_from(value).expect("sample counts in this test fit u32")
    }
}
