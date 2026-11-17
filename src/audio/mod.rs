//! Audio pipeline: IQ demodulation, mixing, and output routing.

pub(crate) mod agc;
pub(crate) mod am;
pub(crate) mod channelizer;
pub mod cpal_sink;
pub(crate) mod demod;
pub(crate) mod limiter;
pub mod profile;
pub(crate) mod squelch;
#[allow(dead_code)]
pub(crate) mod test_signal;
pub mod wav_sink;
pub mod ws_sink;

use crate::dsp::resampler::{create_decimated_real_resampler, RealResampler};
use crate::types::IqSample;
use am::AmDemodulator;
use demod::{DemodMode, Demodulator};
use profile::{AmAudioProfile, AmRfProfile, SquelchMode, VoiceProfile, DEFAULT_PREBUFFER_MS};

/// dB floor value for silence/zero-power audio.
const AUDIO_DB_FLOOR: f32 = -200.0;

/// Lowest channel bandwidth, in hertz, that maps onto an AM RF profile.
const AM_RF_MIN_BW_HZ: f64 = 6_000.0;
/// Highest channel bandwidth, in hertz, that maps onto an AM RF profile.
const AM_RF_MAX_BW_HZ: f64 = 12_000.0;

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
    SetVolume(u8),           // 0-100
    SetSquelch(Option<f32>), // None = disabled, Some(dB) = threshold
    Mute(bool),
    AddVfo {
        id: usize,
        freq: f64,
        channel_bw: f64,
        mode: DemodMode,
    },
    RemoveVfo(usize),
    SetDemodMode(DemodMode), // for cursor (id=0)
    SetVfoDemodMode {
        id: usize,
        mode: DemodMode,
    }, // for specific VFO
    SetAgcEnabled(bool),
    SetAgcAttack(f64), // ms
    SetAgcDecay(f64),  // ms
    SetAgcTarget(f32),
    SetAgcMaxGain(f32), // dB
    SetBandpassEnabled(bool),
    SetBandpassLowCut(f64),  // Hz
    SetBandpassHighCut(f64), // Hz
    Shutdown,
}

use std::borrow::Cow;
use std::collections::HashMap;

// ============================================================================
// Audio Chains
// ============================================================================

/// Clocks shared by every chain of one audio stream, in hertz.
#[derive(Debug, Clone, Copy)]
struct ChainContext {
    /// RF clock of the incoming I/Q stream.
    input_sample_rate: f64,
    /// Sink clock every chain must ultimately produce.
    output_sample_rate: f64,
}

/// Per-channel request retained so one chain can be rebuilt when the stream's path changes.
#[derive(Debug, Clone, Copy)]
struct ChainSpec {
    /// RF center of the incoming stream, in hertz.
    center_freq: f64,
    /// Absolute frequency this chain receives, in hertz.
    cursor_freq: f64,
    /// Requested channel bandwidth, in hertz.
    channel_bw: f64,
    /// Requested demodulation mode. A cursor that has never received a mode control stays on
    /// [`DemodMode::AM`], the legacy constructor's default.
    mode: DemodMode,
    /// Whether the AM chain was explicitly requested, as opposed to the legacy AM default.
    ///
    /// Only an explicit request selects the aviation AM chain: a stream that received no mode
    /// control keeps the legacy demodulator it has always used.
    am_requested: bool,
}

impl ChainSpec {
    /// Builds the request for one channel at `mode`.
    fn new(center_freq: f64, cursor_freq: f64, channel_bw: f64, mode: DemodMode) -> Self {
        Self {
            center_freq,
            cursor_freq,
            channel_bw,
            mode,
            am_requested: mode == DemodMode::AM,
        }
    }

    /// Builds the request for a stream that has not chosen a mode yet.
    ///
    /// This is the legacy AM default the pump has always started from; it keeps the legacy
    /// demodulator until a mode control arrives.
    fn legacy_default(center_freq: f64, cursor_freq: f64, channel_bw: f64) -> Self {
        Self {
            center_freq,
            cursor_freq,
            channel_bw,
            mode: DemodMode::AM,
            am_requested: false,
        }
    }
}

/// One demodulation chain of the audio pipeline.
///
/// A [`AudioChain::Legacy`] chain emits audio at its decimated RF rate, which the shared resampler
/// converts to the sink rate exactly as it always has. An [`AudioChain::Am`] chain owns its own
/// channelizer and already emits the sink rate, so an all-AM stream bypasses that resampler.
///
/// The legacy variant is stored inline because the frozen pipeline contract names it as
/// `Legacy(Demodulator)`. There is one chain per live channel, so the variant size difference costs
/// a few hundred bytes per VFO rather than per sample.
#[expect(
    clippy::large_enum_variant,
    reason = "the pipeline contract pins Legacy(Demodulator); one instance exists per channel"
)]
pub(crate) enum AudioChain {
    /// Legacy demodulator serving AM envelope, FM, and NFM.
    Legacy(Demodulator),
    /// Aviation AM chain: channelizer, channel-power squelch, voice filter, AGC, limiter.
    Am(Box<AmDemodulator>),
}

impl AudioChain {
    /// Builds the chain `spec` asks for.
    ///
    /// An explicitly requested [`DemodMode::AM`] selects the aviation AM chain and everything else
    /// the legacy [`Demodulator`] exactly as before. An AM chain that cannot be designed for these
    /// clocks falls back to the legacy path with a warning, because a rejected profile must not
    /// stop a stream the legacy path still accepts.
    fn build(context: &ChainContext, spec: &ChainSpec, squelch: SquelchMode) -> Self {
        if spec.am_requested && spec.mode == DemodMode::AM {
            if let Some(chain) = Self::design_am(context, spec, squelch) {
                return chain;
            }
        }
        Self::legacy(context, spec)
    }

    /// Builds the legacy demodulator for `spec`, bypassing any AM request.
    ///
    /// The pump's mixed-path fallback needs this: every chain of a mixed stream must run the
    /// legacy demodulator even though some of them asked for AM.
    fn legacy(context: &ChainContext, spec: &ChainSpec) -> Self {
        // The pump's cursor chain has always been built with `new_am` for AM, so the legacy AM
        // case keeps that exact constructor.
        if spec.mode == DemodMode::AM {
            return Self::Legacy(Demodulator::new_am(
                spec.center_freq,
                spec.cursor_freq,
                spec.channel_bw,
                context.input_sample_rate,
                context.output_sample_rate,
            ));
        }
        Self::Legacy(Demodulator::new(
            spec.center_freq,
            spec.cursor_freq,
            spec.channel_bw,
            context.input_sample_rate,
            context.output_sample_rate,
            spec.mode,
        ))
    }

    /// Designs the aviation AM chain, or reports why it cannot serve this stream.
    fn design_am(context: &ChainContext, spec: &ChainSpec, squelch: SquelchMode) -> Option<Self> {
        let profile = AmAudioProfile {
            rf: am_rf_profile_for_channel_bw(spec.channel_bw),
            voice: VoiceProfile::default(),
            squelch,
            prebuffer_ms: DEFAULT_PREBUFFER_MS,
        };
        match AmDemodulator::new(
            spec.center_freq,
            spec.cursor_freq,
            context.input_sample_rate,
            context.output_sample_rate,
            profile,
        ) {
            Ok(chain) => Some(Self::Am(Box::new(chain))),
            Err(error) => {
                tracing::warn!(
                    %error,
                    channel_bw = spec.channel_bw,
                    "AM chain rejected this stream; using the legacy demodulator instead"
                );
                None
            }
        }
    }

    /// Processes one RF-rate block into audio plus the weight it contributes to a mix.
    ///
    /// The weight is the same pre-AGC notion the legacy path reports: the block's pre-AGC RMS for
    /// a legacy chain, and the block's pre-demodulation channel power for an AM chain. Weights only
    /// matter when more than one chain is live.
    ///
    /// # Errors
    /// Returns an error when an AM chain rejects its input.
    fn process(&mut self, iq_block: &[IqSample]) -> anyhow::Result<(Vec<f32>, f32)> {
        use anyhow::Context as _;

        match self {
            Self::Legacy(demod) => Ok((demod.process(iq_block), demod.pre_agc_rms())),
            Self::Am(chain) => {
                let block = chain
                    .process(iq_block)
                    .context("AM chain processing failed")?;
                let weight = am_mixing_weight(&block);
                Ok((block.samples, weight))
            }
        }
    }

    /// Flushes this chain's residual tail at end of stream.
    ///
    /// A legacy chain's tail belongs to the shared resampler, which the pump flushes itself, so
    /// only an AM chain flushes here.
    ///
    /// # Errors
    /// Returns an error when the AM chain cannot flush.
    fn finish(&mut self) -> anyhow::Result<Vec<f32>> {
        use anyhow::Context as _;

        match self {
            Self::Legacy(_) => Ok(Vec::new()),
            Self::Am(chain) => Ok(chain
                .finish()
                .context("AM chain tail flush failed")?
                .samples),
        }
    }

    /// Whether this chain already emits the sink rate without help.
    fn is_am(&self) -> bool {
        matches!(self, Self::Am(_))
    }

    /// Decimation factor of a legacy chain, or `None` for a sink-rate AM chain.
    fn decimation_factor(&self) -> Option<usize> {
        match self {
            Self::Legacy(demod) => Some(demod.decimation_factor()),
            Self::Am(_) => None,
        }
    }
}

/// Mixing weight of one AM block: its linear pre-demodulation channel power.
///
/// A closed squelch weighs nothing, since a gated chain contributes silence rather than a share of
/// the mix.
#[allow(clippy::cast_possible_truncation)] // channel power is a bounded f32 diagnostic
fn am_mixing_weight(block: &am::AmAudioBlock) -> f32 {
    if !block.squelch_open {
        return 0.0;
    }
    let weight = 10.0_f32.powf(block.channel_power_dbfs / 10.0);
    if weight.is_finite() {
        weight
    } else {
        0.0
    }
}

/// Maps a requested channel bandwidth, in hertz, onto the nearest supported AM RF profile.
///
/// The request is clamped to `[AM_RF_MIN_BW_HZ, AM_RF_MAX_BW_HZ]` first, so an out-of-range or
/// negative value selects the narrowest supported profile instead of failing. The result is always
/// one of the four profiles, and a non-finite request selects the narrowest, so this never panics.
fn am_rf_profile_for_channel_bw(channel_bw: f64) -> AmRfProfile {
    const CHOICES: [AmRfProfile; 4] = [
        AmRfProfile::Khz6,
        AmRfProfile::Khz8,
        AmRfProfile::Khz10,
        AmRfProfile::Khz12,
    ];

    let clamped = if channel_bw.is_finite() {
        channel_bw.clamp(AM_RF_MIN_BW_HZ, AM_RF_MAX_BW_HZ)
    } else {
        AM_RF_MIN_BW_HZ
    };

    let mut best = CHOICES[0];
    let mut best_distance = f64::from(best.bandwidth_hz()) - clamped;
    for choice in CHOICES {
        let distance = (f64::from(choice.bandwidth_hz()) - clamped).abs();
        if distance < best_distance.abs() {
            best = choice;
            best_distance = distance;
        }
    }
    best
}

/// One live chain plus what is needed to rebuild it when the stream's path changes.
struct ChainSlot {
    /// Demodulation stage currently serving this channel.
    chain: AudioChain,
    /// Tuning, bandwidth, and mode requested for this channel.
    spec: ChainSpec,
    /// Clocks every rebuild must reuse.
    context: ChainContext,
}

impl ChainSlot {
    /// Builds a chain from `context` and `spec`, remembering both for later rebuilds.
    fn new(context: &ChainContext, spec: ChainSpec, squelch: SquelchMode) -> Self {
        Self {
            chain: AudioChain::build(context, &spec, squelch),
            spec,
            context: *context,
        }
    }

    /// Applies a tuning change in place, so oscillator, filter, AGC, and squelch history survive.
    fn retune(&mut self, center_freq: f64, cursor_freq: f64) {
        self.spec.center_freq = center_freq;
        self.spec.cursor_freq = cursor_freq;
        match &mut self.chain {
            AudioChain::Legacy(demod) => {
                demod.set_center_freq(center_freq);
                demod.set_cursor_freq(cursor_freq);
            }
            AudioChain::Am(chain) => retune_am(chain, center_freq, cursor_freq),
        }
    }

    /// Rebuilds this chain only when its kind differs from the path the stream must use.
    ///
    /// Converting to legacy ignores any AM request, because that is what the mixed-path fallback
    /// asks for.
    fn conform_to(&mut self, want_am: bool, squelch: SquelchMode) {
        if self.chain.is_am() == want_am {
            return;
        }
        self.chain = if want_am {
            AudioChain::build(&self.context, &self.spec, squelch)
        } else {
            AudioChain::legacy(&self.context, &self.spec)
        };
    }
    /// Flushes this slot's chain tail at end of stream.
    ///
    /// # Errors
    /// Returns an error when the AM chain cannot flush.
    fn finish(&mut self) -> anyhow::Result<Vec<f32>> {
        self.chain.finish()
    }
}

/// Retunes an AM chain, reporting a rejection instead of failing the stream.
///
/// A tuning control has no error path in the audio thread, and [`AmDemodulator::retune`] keeps the
/// previous tuning when it rejects a request.
fn retune_am(chain: &mut AmDemodulator, center_freq: f64, cursor_freq: f64) {
    if let Err(error) = chain.retune(center_freq, cursor_freq) {
        tracing::warn!(%error, "AM chain retune rejected; keeping previous tuning");
    }
}

/// Applies `channel_bw` to an AM chain as the nearest supported RF profile.
///
/// Voice shape, squelch, and prebuffer come from the profile already in effect; only the RF
/// bandwidth changes, and [`AmDemodulator::set_profile`] keeps AGC and squelch history.
fn apply_am_channel_bw(chain: &mut AmDemodulator, channel_bw: f64) {
    let profile = AmAudioProfile {
        rf: am_rf_profile_for_channel_bw(channel_bw),
        ..chain.profile().clone()
    };
    if let Err(error) = chain.set_profile(profile) {
        tracing::warn!(%error, "AM profile change rejected; keeping previous profile");
    }
}

/// Applies the pipeline's squelch selection to an AM chain.
///
/// `Some(db)` is a fixed threshold and `None` is unconditionally open, the same meaning
/// [`AudioControl::SetSquelch`] carries for the legacy audio-level gate.
fn apply_am_squelch(chain: &mut AmDemodulator, squelch: Option<f32>) {
    let mode = am_squelch_mode(squelch);
    if chain.profile().squelch == mode {
        return;
    }
    let profile = AmAudioProfile {
        squelch: mode,
        ..chain.profile().clone()
    };
    if let Err(error) = chain.set_profile(profile) {
        tracing::warn!(%error, "AM squelch change rejected; keeping previous squelch");
    }
}

/// Translates the pipeline's squelch selection into an AM squelch mode.
fn am_squelch_mode(squelch: Option<f32>) -> SquelchMode {
    match squelch {
        Some(threshold_dbfs) => SquelchMode::Manual { threshold_dbfs },
        None => SquelchMode::Open,
    }
}

/// Applies the AGC and bandpass controls that every legacy chain shares in place.
fn apply_legacy_shared_control(demod: &mut Demodulator, ctrl: &AudioControl) {
    match *ctrl {
        AudioControl::SetAgcEnabled(enabled) => demod.set_agc_enabled(enabled),
        AudioControl::SetAgcAttack(ms) => demod.set_agc_attack(ms),
        AudioControl::SetAgcDecay(ms) => demod.set_agc_decay(ms),
        AudioControl::SetAgcTarget(level) => demod.set_agc_target(level),
        AudioControl::SetAgcMaxGain(db) => demod.set_agc_max_gain(db),
        AudioControl::SetBandpassEnabled(enabled) => demod.set_bandpass_enabled(enabled),
        AudioControl::SetBandpassLowCut(hz) => demod.set_bandpass_low_cut(hz),
        AudioControl::SetBandpassHighCut(hz) => demod.set_bandpass_high_cut(hz),
        _ => {}
    }
}

/// Chain operations [`apply_control`] needs from whichever chain type it drives.
///
/// The control kernel stays generic so one implementation serves both the AM-aware [`ChainSlot`]
/// the pump owns and a bare legacy [`Demodulator`], whose in-place semantics are unchanged.
trait ChainTarget: Sized {
    /// Builds the chain `spec` selects, honoring the pipeline's `squelch` selection.
    fn build(context: &ChainContext, spec: &ChainSpec, squelch: Option<f32>) -> Self;

    /// Aligns a newly created chain to `other`'s decimation grid where that grid exists.
    fn align_decimation_phase(&mut self, other: &Self);

    /// Applies a cursor-frequency change, in hertz.
    fn set_cursor_freq(&mut self, freq: f64);

    /// Applies an RF center-frequency change, in hertz.
    fn set_center_freq(&mut self, freq: f64);

    /// Applies a channel-bandwidth change, in hertz.
    fn set_channel_bw(&mut self, bw: f64);

    /// Applies a demodulation-mode change.
    fn set_mode(&mut self, mode: DemodMode);

    /// Applies the pipeline's squelch selection.
    fn set_squelch(&mut self, squelch: Option<f32>);

    /// Applies the AGC and bandpass controls every chain shares.
    fn apply_shared(&mut self, ctrl: &AudioControl);
}

impl ChainTarget for Demodulator {
    fn build(context: &ChainContext, spec: &ChainSpec, _squelch: Option<f32>) -> Self {
        match AudioChain::legacy(context, spec) {
            AudioChain::Legacy(demod) => demod,
            AudioChain::Am(_) => {
                unreachable!("the legacy constructor never builds an AM chain")
            }
        }
    }

    fn align_decimation_phase(&mut self, other: &Self) {
        Demodulator::align_decimation_phase(self, other);
    }

    fn set_cursor_freq(&mut self, freq: f64) {
        Demodulator::set_cursor_freq(self, freq);
    }

    fn set_center_freq(&mut self, freq: f64) {
        Demodulator::set_center_freq(self, freq);
    }

    fn set_channel_bw(&mut self, bw: f64) {
        Demodulator::set_channel_bw(self, bw);
    }

    fn set_mode(&mut self, mode: DemodMode) {
        Demodulator::set_mode(self, mode);
    }

    /// Legacy squelch is an audio-level gate in the pump, so a chain has nothing to configure.
    fn set_squelch(&mut self, _squelch: Option<f32>) {}

    fn apply_shared(&mut self, ctrl: &AudioControl) {
        apply_legacy_shared_control(self, ctrl);
    }
}

impl ChainTarget for ChainSlot {
    fn build(context: &ChainContext, spec: &ChainSpec, squelch: Option<f32>) -> Self {
        Self::new(context, *spec, am_squelch_mode(squelch))
    }

    fn align_decimation_phase(&mut self, other: &Self) {
        if let (AudioChain::Legacy(chain), AudioChain::Legacy(reference)) =
            (&mut self.chain, &other.chain)
        {
            chain.align_decimation_phase(reference);
        }
    }

    fn set_cursor_freq(&mut self, freq: f64) {
        self.retune(self.spec.center_freq, freq);
    }

    fn set_center_freq(&mut self, freq: f64) {
        self.retune(freq, self.spec.cursor_freq);
    }

    fn set_channel_bw(&mut self, bw: f64) {
        self.spec.channel_bw = bw;
        match &mut self.chain {
            AudioChain::Legacy(demod) => demod.set_channel_bw(bw),
            AudioChain::Am(chain) => apply_am_channel_bw(chain, bw),
        }
    }

    fn set_mode(&mut self, mode: DemodMode) {
        self.spec.mode = mode;
        self.spec.am_requested = mode == DemodMode::AM;
        // A legacy chain changes mode in place, exactly as before. A chain-kind change is applied
        // by the pump's uniform-path rebuild once the whole control batch is in.
        if let AudioChain::Legacy(demod) = &mut self.chain {
            demod.set_mode(mode);
        }
    }

    fn set_squelch(&mut self, squelch: Option<f32>) {
        if let AudioChain::Am(chain) = &mut self.chain {
            apply_am_squelch(chain, squelch);
        }
    }

    fn apply_shared(&mut self, ctrl: &AudioControl) {
        if let AudioChain::Legacy(demod) = &mut self.chain {
            apply_legacy_shared_control(demod, ctrl);
        }
    }
}

/// Re-establishes the stream's uniform-path invariant after a chain changed.
///
/// Every live chain must be the same kind: a mixed AM/legacy stream uses the legacy demodulator
/// for all channels, so any AM chain is rebuilt as legacy at that moment. When every chain asks for
/// AM, the opposite conversion happens; if such a rebuild cannot be designed the stream falls back
/// to legacy rather than mixing two output clocks.
fn conform_chains(
    cursor: &mut ChainSlot,
    vfo_chains: &mut HashMap<usize, ChainSlot>,
    squelch: Option<f32>,
) {
    let mode = am_squelch_mode(squelch);
    let all_am_requested = cursor.spec.am_requested
        && cursor.spec.mode == DemodMode::AM
        && vfo_chains
            .values()
            .all(|slot| slot.spec.am_requested && slot.spec.mode == DemodMode::AM);

    if all_am_requested {
        cursor.conform_to(true, mode);
        for slot in vfo_chains.values_mut() {
            slot.conform_to(true, mode);
        }
        if chains_are_all_am(cursor, vfo_chains) {
            return;
        }
    }

    cursor.conform_to(false, mode);
    for slot in vfo_chains.values_mut() {
        slot.conform_to(false, mode);
    }
}

/// Whether every live chain already emits the sink rate, so the shared resampler is bypassed.
fn chains_are_all_am(cursor: &ChainSlot, vfo_chains: &HashMap<usize, ChainSlot>) -> bool {
    cursor.chain.is_am() && vfo_chains.values().all(|slot| slot.chain.is_am())
}

/// Decimation factor of any live legacy chain, used to design the shared resampler on demand.
fn legacy_decimation_factor(
    cursor: &ChainSlot,
    vfo_chains: &HashMap<usize, ChainSlot>,
) -> Option<usize> {
    cursor.chain.decimation_factor().or_else(|| {
        vfo_chains
            .values()
            .find_map(|slot| slot.chain.decimation_factor())
    })
}

/// Shared RF-clock resampler that only legacy chains need.
enum SharedResampler {
    /// No legacy output has been produced yet, so nothing has been designed.
    Pending,
    /// The RF clock already equals the legacy backend clock, so no conversion happens.
    NotRequired,
    /// Exact RF-clock conversion from the decimated legacy rate to the sink rate.
    Active(Box<dyn RealResampler>),
}

impl SharedResampler {
    /// Designs the conversion for `decimation`, or records that none is needed.
    ///
    /// # Errors
    /// Returns an error when the decimation factor does not fit `u32`, the backend rate overflows,
    /// or the resampler cannot be designed for this clock pair.
    fn design(
        rf_sample_rate: u32,
        sink_sample_rate: u32,
        decimation: usize,
    ) -> anyhow::Result<Self> {
        use anyhow::Context as _;

        let decimation_factor =
            u32::try_from(decimation).context("audio decimation factor does not fit u32")?;
        let backend_output_rate = sink_sample_rate
            .checked_mul(decimation_factor)
            .context("audio resampler rate overflow")?;
        if rf_sample_rate == backend_output_rate {
            return Ok(Self::NotRequired);
        }
        let resampler =
            create_decimated_real_resampler(rf_sample_rate, sink_sample_rate, decimation_factor)
                .context("failed to create exact RF-clock audio resampler")?;
        Ok(Self::Active(Box::new(resampler)))
    }
}

/// Converts one block of mixed legacy audio to the sink rate.
///
/// Designs the shared resampler on the first legacy block, so an all-AM stream never constructs
/// it, and then feeds it the exact RF frame count of this partition.
///
/// # Errors
/// Returns an error when no legacy chain reported a decimation factor, the resampler cannot be
/// designed for this clock pair, or backend processing fails.
#[allow(clippy::too_many_arguments)] // one call site; each argument is a distinct clock or buffer
fn to_sink_rate<'a>(
    resampler: &mut SharedResampler,
    cursor_chain: &ChainSlot,
    vfo_chains: &HashMap<usize, ChainSlot>,
    mixed: &'a [f32],
    source_frames: usize,
    rf_sample_rate: u32,
    sink_sample_rate: u32,
    staged: &'a mut Vec<f32>,
) -> anyhow::Result<Cow<'a, [f32]>> {
    use anyhow::Context as _;

    if matches!(resampler, SharedResampler::Pending) {
        let decimation = legacy_decimation_factor(cursor_chain, vfo_chains)
            .context("legacy audio chain reported no decimation factor")?;
        *resampler = SharedResampler::design(rf_sample_rate, sink_sample_rate, decimation)?;
    }
    match resampler {
        SharedResampler::Active(shared_resampler) => {
            staged.clear();
            shared_resampler
                .process_with_source_frames(mixed, source_frames, staged)
                .context("shared audio resampler processing failed")?;
            Ok(Cow::Borrowed(staged.as_slice()))
        }
        SharedResampler::NotRequired => Ok(Cow::Borrowed(mixed)),
        SharedResampler::Pending => {
            anyhow::bail!("shared audio resampler was not designed for a legacy stream")
        }
    }
}

// ============================================================================
// Audio Thread
// ============================================================================

/// Apply a control message to the audio thread state.
/// Returns `true` if the thread should shut down.
#[allow(clippy::too_many_arguments)] // audio thread state is tightly coupled
fn apply_control<C: ChainTarget>(
    ctrl: &AudioControl,
    cursor_chain: &mut C,
    vfo_chains: &mut HashMap<usize, C>,
    volume: &mut f32,
    squelch: &mut Option<f32>,
    muted: &mut bool,
    center_freq: &mut f64,
    input_sample_rate: f64,
    output_sample_rate: f64,
) -> bool {
    let context = ChainContext {
        input_sample_rate,
        output_sample_rate,
    };

    match *ctrl {
        AudioControl::SetCursorFreq(f) => cursor_chain.set_cursor_freq(f),
        AudioControl::SetCenterFreq(f) => {
            *center_freq = f;
            cursor_chain.set_center_freq(f);
            for chain in vfo_chains.values_mut() {
                chain.set_center_freq(f);
            }
        }
        AudioControl::SetChannelBw(bw) => cursor_chain.set_channel_bw(bw),
        AudioControl::SetVolume(v) => *volume = f32::from(v.min(100)) / 100.0,
        AudioControl::SetSquelch(ref s) => {
            *squelch = *s;
            cursor_chain.set_squelch(*s);
            for chain in vfo_chains.values_mut() {
                chain.set_squelch(*s);
            }
        }
        AudioControl::Mute(m) => *muted = m,
        AudioControl::AddVfo {
            id,
            freq,
            channel_bw,
            mode,
        } => {
            let spec = ChainSpec::new(*center_freq, freq, channel_bw, mode);
            let mut chain = C::build(&context, &spec, *squelch);
            chain.align_decimation_phase(cursor_chain);
            vfo_chains.insert(id, chain);
        }
        AudioControl::RemoveVfo(id) => {
            vfo_chains.remove(&id);
        }
        AudioControl::SetDemodMode(mode) => {
            cursor_chain.set_mode(mode);
        }
        AudioControl::SetVfoDemodMode { id, mode } => {
            if let Some(chain) = vfo_chains.get_mut(&id) {
                chain.set_mode(mode);
            }
        }
        AudioControl::SetAgcEnabled(_)
        | AudioControl::SetAgcAttack(_)
        | AudioControl::SetAgcDecay(_)
        | AudioControl::SetAgcTarget(_)
        | AudioControl::SetAgcMaxGain(_)
        | AudioControl::SetBandpassEnabled(_)
        | AudioControl::SetBandpassLowCut(_)
        | AudioControl::SetBandpassHighCut(_) => {
            cursor_chain.apply_shared(ctrl);
            for chain in vfo_chains.values_mut() {
                chain.apply_shared(ctrl);
            }
        }
        AudioControl::Shutdown => return true,
    }
    false
}

/// Demodulate IQ samples, mix outputs, and apply volume and squelch.
fn demod_and_mix(
    iq_block: &[IqSample],
    cursor_chain: &mut ChainSlot,
    vfo_chains: &mut HashMap<usize, ChainSlot>,
    muted: bool,
    volume: f32,
    squelch: Option<f32>,
) -> anyhow::Result<Vec<f32>> {
    // Collect outputs from all active chains with their pre-AGC levels
    let mut outputs: Vec<(Vec<f32>, f32)> = Vec::new();

    if !muted {
        outputs.push(cursor_chain.chain.process(iq_block)?);
    }

    for slot in vfo_chains.values_mut() {
        outputs.push(slot.chain.process(iq_block)?);
    }

    let mut audio = mix_outputs(outputs, &mut cursor_chain.chain, iq_block)?;

    // Apply volume
    for s in &mut audio {
        *s *= volume;
    }

    // Squelch
    if check_squelch(&audio, squelch) {
        audio.fill(0.0);
    }
    Ok(audio)
}

/// Mix demodulator outputs using power-weighted mixing.
fn mix_outputs(
    outputs: Vec<(Vec<f32>, f32)>,
    cursor_chain: &mut AudioChain,
    iq_block: &[IqSample],
) -> anyhow::Result<Vec<f32>> {
    let num_active = outputs.len();
    if num_active == 0 {
        let (silent, _weight) = cursor_chain.process(iq_block)?;
        Ok(vec![0.0f32; silent.len()])
    } else if num_active == 1 {
        Ok(outputs
            .into_iter()
            .next()
            .expect("invariant: num_active == 1")
            .0)
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
        Ok(mixed)
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
    let rounded_input_rate = input_sample_rate.round();
    if (rounded_input_rate - input_sample_rate).abs() > f64::EPSILON
        || rounded_input_rate > f64::from(u32::MAX)
    {
        anyhow::bail!("RF sample rate {input_sample_rate} Hz is not an exact u32 clock");
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rf_sample_rate = rounded_input_rate as u32;

    let context = ChainContext {
        input_sample_rate,
        output_sample_rate,
    };
    // The pump is entered with no mode or squelch selection of its own, so the cursor starts on the
    // legacy AM default it has always used. Only an explicit mode control selects the AM chain.
    let cursor_spec = ChainSpec::legacy_default(center_freq, cursor_freq, channel_bw);
    let mut cursor_chain = ChainSlot::new(&context, cursor_spec, SquelchMode::Open);
    // The shared resampler is designed on the first legacy output, so an all-AM stream never
    // constructs or feeds it.
    let mut resampler = SharedResampler::Pending;
    let mut resampled = Vec::new();
    let mut vfo_chains: HashMap<usize, ChainSlot> = HashMap::new();
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
                        let shutdown = apply_control(
                            &ctrl,
                            &mut cursor_chain,
                            &mut vfo_chains,
                            &mut volume,
                            &mut squelch,
                            &mut muted,
                            &mut center_freq,
                            input_sample_rate,
                            output_sample_rate,
                        );
                        conform_chains(&mut cursor_chain, &mut vfo_chains, squelch);
                        if shutdown {
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
                Err(
                    crossbeam_channel::TryRecvError::Empty
                    | crossbeam_channel::TryRecvError::Disconnected,
                ) => {
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
                            let shutdown = apply_control(&ctrl, &mut cursor_chain, &mut vfo_chains, &mut volume, &mut squelch, &mut muted, &mut center_freq, input_sample_rate, output_sample_rate);
                            conform_chains(&mut cursor_chain, &mut vfo_chains, squelch);
                            if shutdown {
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
            &mut cursor_chain,
            &mut vfo_chains,
            muted,
            volume,
            squelch,
        )
        .context("audio demodulation failed")?;
        let samples = if chains_are_all_am(&cursor_chain, &vfo_chains) {
            // Every chain already emits the sink rate, so the shared resampler is bypassed.
            Cow::Borrowed(mixed.as_slice())
        } else {
            to_sink_rate(
                &mut resampler,
                &cursor_chain,
                &vfo_chains,
                &mixed,
                iq_block.len(),
                rf_sample_rate,
                sink_sample_rate,
                &mut resampled,
            )?
        };
        if !samples.is_empty() {
            sink.write(&samples).context("audio sink write failed")?;
        }
    }

    if let SharedResampler::Active(shared_resampler) = &mut resampler {
        resampled.clear();
        shared_resampler
            .flush(&mut resampled)
            .context("failed to flush shared audio resampler")?;
        if !resampled.is_empty() {
            sink.write(&resampled)
                .context("final audio sink write failed")?;
        }
    }

    // Every AM chain owns its own resampler, so the end of the stream must flush those tails too.
    // The shared resampler above is inactive while every chain is AM, so this cannot double-write.
    if chains_are_all_am(&cursor_chain, &vfo_chains) {
        let mut chains = std::iter::once(&mut cursor_chain).chain(vfo_chains.values_mut());
        for chain in chains.by_ref() {
            let tail = chain
                .finish()
                .context("failed to flush AM chain tail at end of stream")?;
            if !tail.is_empty() {
                sink.write(&tail).context("final AM tail write failed")?;
            }
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
    use crate::audio::profile::{AmAudioProfile, AmRfProfile, SquelchMode, VoiceProfile};
    use crate::audio::test_signal::generate_am_signal;
    use crate::audio::wav_sink::WavSink;
    use crossbeam_channel::{bounded, unbounded};
    use std::f64::consts::PI;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    fn make_test_iq(
        cursor_freq: f64,
        center_freq: f64,
        sample_rate: f64,
        num: usize,
    ) -> Vec<IqSample> {
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
        let mean =
            settled.iter().map(|&sample| f64::from(sample)).sum::<f64>() / settled.len() as f64;
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
            let rms = rms.sqrt();
            assert!(
                rms <= 1.0e-5,
                "partition case {} RMS {rms:.3e} exceeds 1e-5",
                index + 1
            );
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
            fn sample_rate(&self) -> u32 {
                self.sr
            }
        }

        let sink = Box::new(SharedSink {
            recorded: recorded2,
            sr: 48000,
        });

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

            struct SharedSink {
                recorded: Arc<Mutex<Vec<f32>>>,
                sr: u32,
            }
            impl AudioSink for SharedSink {
                fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
                    self.recorded.lock().unwrap().extend_from_slice(samples);
                    Ok(())
                }
                fn sample_rate(&self) -> u32 {
                    self.sr
                }
            }

            // Set volume before sending data
            ctrl_tx.send(AudioControl::SetVolume(vol)).unwrap();

            let sink = Box::new(SharedSink {
                recorded: recorded2,
                sr: 48000,
            });
            let handle = std::thread::spawn(move || {
                audio_thread(&iq_rx, &ctrl_rx, sink, center, cursor, bw, sample_rate);
            });

            let iq = make_test_iq(cursor, center, sample_rate, 8192);
            iq_tx.send(iq).unwrap();
            std::thread::sleep(Duration::from_millis(50));
            ctrl_tx.send(AudioControl::Shutdown).unwrap();
            handle.join().unwrap();

            let samples = recorded.lock().unwrap();
            let rms: f64 = samples
                .iter()
                .map(|&s| (s as f64) * (s as f64))
                .sum::<f64>()
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

        struct SharedSink {
            recorded: Arc<Mutex<Vec<f32>>>,
            sr: u32,
        }
        impl AudioSink for SharedSink {
            fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
                self.recorded.lock().unwrap().extend_from_slice(samples);
                Ok(())
            }
            fn sample_rate(&self) -> u32 {
                self.sr
            }
        }

        ctrl_tx.send(AudioControl::Mute(true)).unwrap();

        let sink = Box::new(SharedSink {
            recorded: recorded2,
            sr: 48000,
        });
        let handle = std::thread::spawn(move || {
            audio_thread(
                &iq_rx,
                &ctrl_rx,
                sink,
                center,
                cursor,
                25_000.0,
                sample_rate,
            );
        });

        let iq = make_test_iq(cursor, center, sample_rate, 8192);
        iq_tx.send(iq).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        ctrl_tx.send(AudioControl::Shutdown).unwrap();
        handle.join().unwrap();

        let samples = recorded.lock().unwrap();
        assert!(!samples.is_empty(), "Should still write silence when muted");
        assert!(
            samples.iter().all(|&s| s == 0.0),
            "All samples should be zero when muted"
        );
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

        struct SharedSink {
            recorded: Arc<Mutex<Vec<f32>>>,
            sr: u32,
        }
        impl AudioSink for SharedSink {
            fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
                self.recorded.lock().unwrap().extend_from_slice(samples);
                Ok(())
            }
            fn sample_rate(&self) -> u32 {
                self.sr
            }
        }

        // Set squelch high enough to silence the weak signal
        ctrl_tx.send(AudioControl::SetSquelch(Some(0.0))).unwrap();

        let sink = Box::new(SharedSink {
            recorded: recorded2,
            sr: 48000,
        });
        let handle = std::thread::spawn(move || {
            audio_thread(
                &iq_rx,
                &ctrl_rx,
                sink,
                center,
                cursor,
                25_000.0,
                sample_rate,
            );
        });

        // Signal at 100.1MHz but cursor at 100.5MHz → out of band, very weak
        let iq = generate_am_signal(100.1e6, 1000.0, 0.8, sample_rate, center, 8192);
        iq_tx.send(iq).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        ctrl_tx.send(AudioControl::Shutdown).unwrap();
        handle.join().unwrap();

        let samples = recorded.lock().unwrap();
        assert!(!samples.is_empty());
        assert!(
            samples.iter().all(|&s| s == 0.0),
            "Squelch should silence weak signal"
        );
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

        struct SharedSink {
            recorded: Arc<Mutex<Vec<f32>>>,
            sr: u32,
        }
        impl AudioSink for SharedSink {
            fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
                self.recorded.lock().unwrap().extend_from_slice(samples);
                Ok(())
            }
            fn sample_rate(&self) -> u32 {
                self.sr
            }
        }

        let sink = Box::new(SharedSink {
            recorded: recorded2,
            sr: 48000,
        });
        let handle = std::thread::spawn(move || {
            audio_thread(
                &iq_rx,
                &ctrl_rx,
                sink,
                center,
                100.1e6,
                25_000.0,
                sample_rate,
            );
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

        struct SharedSink {
            recorded: Arc<Mutex<Vec<f32>>>,
            sr: u32,
        }
        impl AudioSink for SharedSink {
            fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
                self.recorded.lock().unwrap().extend_from_slice(samples);
                Ok(())
            }
            fn sample_rate(&self) -> u32 {
                self.sr
            }
        }

        // Add a VFO
        ctrl_tx
            .send(AudioControl::AddVfo {
                id: 1,
                freq: 100.2e6,
                channel_bw: 25_000.0,
                mode: DemodMode::AM,
            })
            .unwrap();

        let sink = Box::new(SharedSink {
            recorded: recorded2,
            sr: 48000,
        });
        let handle = std::thread::spawn(move || {
            audio_thread(
                &iq_rx,
                &ctrl_rx,
                sink,
                center,
                cursor,
                25_000.0,
                sample_rate,
            );
        });

        let iq = make_test_iq(cursor, center, sample_rate, 8192);
        iq_tx.send(iq).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        ctrl_tx.send(AudioControl::Shutdown).unwrap();
        handle.join().unwrap();

        let samples = recorded.lock().unwrap();
        assert!(
            !samples.is_empty(),
            "Should produce mixed audio from cursor + VFO"
        );
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

        struct SharedSink {
            recorded: Arc<Mutex<Vec<f32>>>,
            sr: u32,
        }
        impl AudioSink for SharedSink {
            fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
                self.recorded.lock().unwrap().extend_from_slice(samples);
                Ok(())
            }
            fn sample_rate(&self) -> u32 {
                self.sr
            }
        }

        // Mute cursor, add VFO at signal frequency
        ctrl_tx.send(AudioControl::Mute(true)).unwrap();
        ctrl_tx
            .send(AudioControl::AddVfo {
                id: 1,
                freq: cursor,
                channel_bw: 25_000.0,
                mode: DemodMode::AM,
            })
            .unwrap();

        let sink = Box::new(SharedSink {
            recorded: recorded2,
            sr: 48000,
        });
        let handle = std::thread::spawn(move || {
            audio_thread(
                &iq_rx,
                &ctrl_rx,
                sink,
                center,
                cursor,
                25_000.0,
                sample_rate,
            );
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

        ctrl_tx
            .send(AudioControl::AddVfo {
                id: 1,
                freq: 100.1e6,
                channel_bw: 25_000.0,
                mode: DemodMode::AM,
            })
            .unwrap();
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

    // ========================================================================
    // Task 10B — AM pipeline wiring with legacy FM/NFM preserved
    // ========================================================================
    //
    // Frozen contract (Task 10B). Nothing below decides anything beyond this:
    //
    // ```text
    // pub(crate) enum AudioChain {
    //     Legacy(Demodulator),
    //     Am(Box<AmDemodulator>),
    // }
    // ```
    //
    // * `AudioChain` owns both the cursor chain and every VFO chain.
    // * Selection: `DemodMode::AM` builds `AudioChain::Am` from
    //   `AmAudioProfile::default()`-equivalent (Khz8 / Standard / Auto 6 dB / 100 ms) with the
    //   request's center and cursor; every other mode builds `AudioChain::Legacy` exactly as
    //   today.
    // * `AmDemodulator` already emits the sink rate, so `pump_audio` bypasses the shared
    //   RF-clock resampler when every active chain is `Am`. If any active chain is `Legacy`,
    //   the legacy path and shared resampler stay exactly as today and no AM chain is mixed
    //   into it: a mixed AM/FM stream uses the legacy demodulator for all chains.
    // * `SetCursorFreq` → `retune`, `SetCenterFreq` → `retune`, `SetChannelBw` → `set_profile`
    //   with the nearest supported `AmRfProfile`, `SetDemodMode`/`AddVfo`/`RemoveVfo` rebuild
    //   only the chain that changed.
    // * `SetChannelBw` mapping, pinned literally: 8_000 → Khz8, 25_000 → Khz12, and both an
    //   out-of-range 3_000 and a non-positive value clamp to Khz6 (the ends of the literal
    //   6/8/10/12 kHz set).
    // * `SetSquelch(Some(db))` reaches an AM chain as `SquelchMode::Manual { threshold_dbfs:
    //   db }` and `SetSquelch(None)` as `SquelchMode::Open`; legacy squelch behavior is
    //   unchanged.
    //
    // The numbers frozen in these tests were captured from the current implementation and the
    // standalone chain: the legacy AM path rejects the 8.33 kHz adjacent carrier by −20.4 dB
    // while the AM chain rejects it by +63.5 dB, so AM selectivity is the discriminator that
    // only the wired chain can satisfy.

    /// RF clock every Task 10B fixture runs at, in hertz.
    const T10B_RF_HZ: f64 = 2_048_000.0;
    /// Sink rate every Task 10B fixture runs at, in hertz.
    const T10B_SINK_HZ: u32 = 48_000;
    /// RF frames in one 0.25 s Task 10B fixture.
    const T10B_FRAMES_250MS: usize = 512_000;
    /// Audio samples one 0.25 s Task 10B fixture must deliver at 48 kHz.
    const T10B_SAMPLES_250MS: usize = 12_000;
    /// Capture-sized IQ partition the audio thread actually receives, in frames.
    const T10B_PARTITION: usize = 2_048;
    /// Reported latency of the default AM chain at 2.048 MHz → 48 kHz, in output samples.
    const T10B_AM_LATENCY: usize = 634;
    /// Adjacent carrier offset used by every selectivity fixture, in hertz.
    const T10B_ADJACENT_HZ: f64 = 8_330.0;

    /// Sink that records audio and injects queued controls at a chosen write index.
    ///
    /// Injection is synchronous inside [`AudioSink::write`], so the pump's next loop-top
    /// control drain has already seen the control before it processes the next IQ block. That
    /// keeps mid-stream control timing deterministic without sleeps.
    struct InjectingSink {
        recorded: std::sync::Arc<std::sync::Mutex<Vec<f32>>>,
        control_tx: crossbeam_channel::Sender<AudioControl>,
        plan: Vec<(usize, AudioControl)>,
        writes: usize,
    }

    impl AudioSink for InjectingSink {
        fn write(&mut self, samples: &[f32]) -> anyhow::Result<()> {
            self.recorded.lock().unwrap().extend_from_slice(samples);
            for (index, control) in &self.plan {
                if *index == self.writes {
                    self.control_tx.send(control.clone()).unwrap();
                }
            }
            self.writes += 1;
            Ok(())
        }

        fn sample_rate(&self) -> u32 {
            T10B_SINK_HZ
        }
    }

    /// Runs IQ through the real pump, with `initial` controls drained before the first block and
    /// `plan` controls injected after the given sink write.
    #[allow(clippy::needless_pass_by_value)] // the plan is owned so injection timing is explicit
    fn run_pipeline(
        mode: DemodMode,
        channel_bw: f64,
        iq: &[IqSample],
        squelch: Option<f32>,
        plan: Vec<(usize, AudioControl)>,
    ) -> Vec<f32> {
        let (iq_tx, iq_rx) = unbounded();
        let (control_tx, control_rx) = unbounded();
        control_tx.send(AudioControl::SetDemodMode(mode)).unwrap();
        control_tx.send(AudioControl::SetSquelch(squelch)).unwrap();
        let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = InjectingSink {
            recorded: recorded.clone(),
            control_tx: control_tx.clone(),
            plan,
            writes: 0,
        };
        for chunk in iq.chunks(T10B_PARTITION) {
            iq_tx.send(chunk.to_vec()).unwrap();
        }
        drop(iq_tx);
        let mut sink: Box<dyn AudioSink> = Box::new(sink);
        pump_audio(
            &iq_rx,
            &control_rx,
            &mut *sink,
            100_000_000.0,
            100_000_000.0,
            channel_bw,
            T10B_RF_HZ,
        )
        .expect("finite Task 10B fixture must pump without error");
        drop(control_tx);
        drop(control_rx);
        let samples = recorded.lock().unwrap().clone();
        samples
    }

    /// Builds the standalone reference AM chain the pipeline must reproduce.
    fn am_reference_chain(rf: AmRfProfile) -> crate::audio::am::AmDemodulator {
        crate::audio::am::AmDemodulator::new(
            100_000_000.0,
            100_000_000.0,
            T10B_RF_HZ,
            f64::from(T10B_SINK_HZ),
            AmAudioProfile {
                rf,
                voice: VoiceProfile::Standard,
                squelch: SquelchMode::Open,
                prebuffer_ms: 100,
            },
        )
        .expect("literal default-profile AM chain must construct")
    }

    /// Drives a reference chain with the same partition and the same block-boundary controls.
    fn drive_reference(
        chain: &mut crate::audio::am::AmDemodulator,
        iq: &[IqSample],
        plan: &[(usize, AudioControl)],
    ) -> Vec<f32> {
        let mut output = Vec::new();
        for (block, chunk) in iq.chunks(T10B_PARTITION).enumerate() {
            for (index, control) in plan {
                if *index == block {
                    match control {
                        AudioControl::SetCursorFreq(freq) => chain
                            .retune(100_000_000.0, *freq)
                            .expect("reference retune must accept a finite frequency"),
                        other => panic!("reference chain cannot mirror {other:?}"),
                    }
                }
            }
            output.extend(
                chain
                    .process(chunk)
                    .expect("finite reference input must process")
                    .samples,
            );
        }
        output
    }

    /// 0.25 s of AM: a desired carrier at the center plus an adjacent AM carrier.
    #[allow(clippy::cast_possible_truncation)] // fixture amplitudes are bounded well below 1.0
    fn t10b_two_carrier(adjacent_hz: f64) -> Vec<IqSample> {
        use std::f64::consts::TAU;
        (0..T10B_FRAMES_250MS)
            .map(|index| {
                #[allow(clippy::cast_precision_loss)]
                let time = index as f64 / T10B_RF_HZ;
                let desired = 0.20 * (1.0 + 0.65 * (TAU * 1_000.0 * time).sin());
                let adjacent = 0.70 * (1.0 + 0.85 * (TAU * 2_000.0 * time).sin());
                let phase = TAU * adjacent_hz * time;
                IqSample::new(
                    (desired + adjacent * phase.cos()) as f32,
                    (adjacent * phase.sin()) as f32,
                )
            })
            .collect()
    }

    /// 0.25 s of a single 1 kHz AM carrier at the center frequency.
    fn t10b_single_carrier() -> Vec<IqSample> {
        generate_am_signal(
            100_000_000.0,
            1_000.0,
            0.70,
            T10B_RF_HZ,
            100_000_000.0,
            T10B_FRAMES_250MS,
        )
    }

    /// Deterministic noise-only IQ, band-limited to the channel by the chain itself.
    #[allow(clippy::cast_precision_loss)] // 24-bit fixture mantissa is exact in f32
    fn t10b_noise(amplitude: f32) -> Vec<IqSample> {
        let mut state = 0x9e37_79b9_u32;
        (0..T10B_FRAMES_250MS)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let real = (state >> 9) as f32 / 8_388_607.0 - 1.0;
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let imaginary = (state >> 9) as f32 / 8_388_607.0 - 1.0;
                IqSample::new(real * amplitude, imaginary * amplitude)
            })
            .collect()
    }

    /// Single-bin amplitude of `samples` at `frequency_hz`, in linear full scale.
    fn t10b_tone_amplitude(samples: &[f32], frequency_hz: f64) -> f64 {
        use std::f64::consts::TAU;
        let mut real = 0.0_f64;
        let mut imaginary = 0.0_f64;
        for (index, sample) in samples.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let phase = TAU * frequency_hz * index as f64 / f64::from(T10B_SINK_HZ);
            real += f64::from(*sample) * phase.cos();
            imaginary -= f64::from(*sample) * phase.sin();
        }
        #[allow(clippy::cast_precision_loss)]
        let scale = 2.0 / samples.len() as f64;
        scale * real.hypot(imaginary)
    }

    /// Desired-over-adjacent-modulation ratio in decibels for the two-carrier fixture.
    fn t10b_adjacent_rejection(samples: &[f32]) -> f64 {
        assert!(
            samples.len() > 4_800,
            "selectivity needs a settled measurement window, got {} samples",
            samples.len()
        );
        let wanted = t10b_tone_amplitude(samples, 1_000.0);
        let leaked = t10b_tone_amplitude(samples, 2_000.0);
        20.0 * (wanted / leaked.max(1e-12)).log10()
    }

    #[allow(clippy::cast_precision_loss)] // sample counts here are far below 2^52
    fn t10b_rms(samples: &[f32]) -> f64 {
        assert!(!samples.is_empty(), "RMS needs a non-empty fixture");
        (samples
            .iter()
            .map(|sample| f64::from(*sample) * f64::from(*sample))
            .sum::<f64>()
            / samples.len() as f64)
            .sqrt()
    }

    fn t10b_assert_all_points_identical(actual: &[f32], expected: &[f32], context: &str) {
        assert_eq!(
            actual.len(),
            expected.len(),
            "{context}: sample count {} versus {}",
            actual.len(),
            expected.len()
        );
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "{context}: sample {index} differs ({actual} versus {expected})"
            );
        }
    }

    #[test]
    fn am_mode_uses_the_new_chain_and_bypasses_the_shared_resampler() {
        // 0.25 s of 2.048 MS/s input must arrive as exactly 12,000 sink samples. The AM chain
        // already emits the sink rate, so any second resampling stage either changes that count
        // or adds a fractional-clock error of its own; both are visible here.
        let single = t10b_single_carrier();
        for partition in [T10B_PARTITION, 4_096] {
            let input: Vec<IqSample> = single.chunks(partition).flatten().copied().collect();
            let samples = run_pipeline(DemodMode::AM, 8_000.0, &input, None, Vec::new());
            assert_eq!(
                samples.len(),
                T10B_SAMPLES_250MS,
                "AM through the pump must deliver exactly 0.25 s at 48 kHz for {partition}-frame partitions"
            );
            assert!(
                samples.iter().all(|sample| sample.is_finite()),
                "AM through the pump must stay finite"
            );
        }

        // The 1 kHz tone must survive at the sink rate, measured only after the reported latency.
        let samples = run_pipeline(DemodMode::AM, 8_000.0, &single, None, Vec::new());
        let settled = &samples[T10B_AM_LATENCY + 4_800..];
        let measured_hz = estimate_tone_hz(settled, T10B_SINK_HZ);
        assert!(
            (measured_hz - 1_000.0).abs() <= 1.0,
            "AM chain must recover 1 kHz within 1 Hz after discarding {T10B_AM_LATENCY} latency samples, measured {measured_hz} Hz"
        );

        // Selectivity is the proof that the new chain, not the legacy envelope path, served the
        // stream: the legacy path rejects this 8.33 kHz adjacent carrier by only −20.4 dB.
        let adjacent = t10b_two_carrier(T10B_ADJACENT_HZ);
        let samples = run_pipeline(DemodMode::AM, 8_000.0, &adjacent, None, Vec::new());
        assert_eq!(
            samples.len(),
            T10B_SAMPLES_250MS,
            "the selectivity fixture must retain the exact RF-clock duration"
        );
        let settled = &samples[T10B_AM_LATENCY + 4_800..];
        let rejection_db = t10b_adjacent_rejection(settled);
        assert!(
            rejection_db >= 40.0,
            "AM mode must run the wired AmDemodulator (>= 40 dB adjacent rejection); measured {rejection_db:.2} dB, which is the legacy envelope path"
        );
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )] // fixture counts and rates are tiny positive literals
    fn am_clock_is_exact_at_every_receiver_rate() {
        // 2.048 MHz divides into 48 kHz evenly enough that a missing end-of-stream flush hides in
        // the rounding. 960 kHz and 1.2 MHz are rates the CLI accepts and both leave a remainder,
        // so they expose a stream that ends short of its own duration.
        for input_rate_hz in [960_000.0_f64, 1_024_000.0, 1_200_000.0, 2_048_000.0] {
            let seconds = 0.25_f64;
            let frames = (input_rate_hz * seconds) as usize;
            let expected =
                ((frames as f64) * f64::from(T10B_SINK_HZ) / input_rate_hz).round() as usize;
            let iq: Vec<IqSample> = (0..frames)
                .map(|index| {
                    let time = index as f64 / input_rate_hz;
                    let envelope = 1.0 + 0.70 * (std::f64::consts::TAU * 1_000.0 * time).sin();
                    IqSample::new(envelope as f32, 0.0)
                })
                .collect();

            let (iq_tx, iq_rx) = unbounded();
            let (control_tx, control_rx) = unbounded();
            control_tx
                .send(AudioControl::SetDemodMode(DemodMode::AM))
                .unwrap();
            let mut sink = NullSink::new(T10B_SINK_HZ);
            for chunk in iq.chunks(2_048) {
                iq_tx.send(chunk.to_vec()).unwrap();
            }
            drop(iq_tx);
            pump_audio(
                &iq_rx,
                &control_rx,
                &mut sink,
                100_000_000.0,
                100_000_000.0,
                8_000.0,
                input_rate_hz,
            )
            .expect("finite AM input must pump without error");

            let recorded = sink.samples.len();
            assert_eq!(
                recorded, expected,
                "{input_rate_hz} Hz of AM must deliver exactly {expected} sink samples for \
                 {seconds} s, but delivered {recorded}"
            );
        }
    }

    #[test]
    fn fm_and_nfm_paths_are_unchanged() {
        use crate::audio::test_signal::generate_fm_signal;

        // Captured from the current implementation for this exact fixture (same controls, same
        // partition, same 0.25 s of input): the legacy path emits 12,000 samples and these levels.
        // An AM chain leaking into FM/NFM would emit squelch-closed zeros or AM levels instead.
        const CASES: [(DemodMode, f64, f64, f64, f64); 2] = [
            (
                DemodMode::FM,
                200_000.0,
                75_000.0,
                0.279_358_327,
                4.857_953_548,
            ),
            (
                DemodMode::NFM,
                25_000.0,
                5_000.0,
                0.313_574_390,
                4.358_976_841,
            ),
        ];

        for (mode, channel_bw, deviation_hz, expected_rms, expected_peak) in CASES {
            let iq = generate_fm_signal(
                100_000_000.0,
                1_000.0,
                deviation_hz,
                T10B_RF_HZ,
                100_000_000.0,
                T10B_FRAMES_250MS,
            );
            let samples = run_pipeline(mode, channel_bw, &iq, None, Vec::new());
            assert_eq!(
                samples.len(),
                T10B_SAMPLES_250MS,
                "{mode:?} must keep the legacy RF-clock duration"
            );
            assert!(
                samples.iter().all(|sample| sample.is_finite()),
                "{mode:?} audio must stay finite"
            );
            let rms = t10b_rms(&samples);
            assert!(
                (rms - expected_rms).abs() <= 1.0e-4,
                "{mode:?} RMS changed from {expected_rms} to {rms}; the AM branch must not touch FM/NFM"
            );
            let peak = samples
                .iter()
                .fold(0.0_f32, |max, sample| max.max(sample.abs()));
            assert!(
                (f64::from(peak) - expected_peak).abs() <= 1.0e-3,
                "{mode:?} peak changed from {expected_peak} to {peak}; the AM branch must not touch FM/NFM"
            );
        }
    }

    #[test]
    fn am_squelch_closed_emits_exact_zeros_through_the_pump() {
        // Noise whose channel power reads −42.99 dBFS: below a −35 dBFS manual threshold, above
        // −60 dBFS. The chain's own gate, not the legacy audio-RMS squelch, must decide this.
        let noise = t10b_noise(1.0e-2);

        let closed = run_pipeline(DemodMode::AM, 8_000.0, &noise, Some(-35.0), Vec::new());
        assert_eq!(
            closed.len(),
            T10B_SAMPLES_250MS,
            "a closed squelch must still keep the exact output clock"
        );
        assert!(
            closed
                .iter()
                .all(|sample| sample.to_bits() == 0.0_f32.to_bits()),
            "a closed AM squelch must emit positive-zero bits for every sample, not denormals, silence-shaped AGC output, or negative zero"
        );

        let open = run_pipeline(DemodMode::AM, 8_000.0, &noise, Some(-60.0), Vec::new());
        assert_eq!(
            open.len(),
            T10B_SAMPLES_250MS,
            "an open squelch must keep the same output clock"
        );
        assert!(
            open.iter().any(|sample| sample.to_bits() != 0.0_f32.to_bits()),
            "the same noise above a −60 dBFS threshold must pass audio, so the zeros above come from the threshold and not from a permanently muted chain"
        );
        assert!(
            open.iter().all(|sample| sample.is_finite()),
            "open-squelch AM audio must stay finite"
        );
    }

    #[test]
    fn am_control_retune_and_profile_change_preserve_streaming_state() {
        let single = t10b_single_carrier();
        let untouched = run_pipeline(DemodMode::AM, 8_000.0, &single, None, Vec::new());

        // (a) A same-frequency retune must be a bit-exact no-op: no NCO, filter, squelch, AGC, or
        // resampler reset. Any rebuild shows up as a transient against the untouched stream.
        let retuned = run_pipeline(
            DemodMode::AM,
            8_000.0,
            &single,
            None,
            vec![(40, AudioControl::SetCursorFreq(100_000_000.0))],
        );
        t10b_assert_all_points_identical(&retuned, &untouched, "same-frequency AM retune");

        // (b) The post-control continuation must match an independently driven chain retuned at
        // the same block boundary, which pins that the control reaches the chain in order and
        // preserves AGC/squelch/filter history rather than restarting it.
        let mut reference = am_reference_chain(AmRfProfile::Khz8);
        let reference_latency_before = reference.latency_samples();
        let reference_audio = drive_reference(
            &mut reference,
            &single,
            &[(40, AudioControl::SetCursorFreq(100_000_000.0))],
        );
        assert_eq!(
            reference.latency_samples(),
            reference_latency_before,
            "a retune must not change the AM chain's reported latency"
        );
        // 40 sink writes at 2,048 RF frames each is 81,920 RF frames, i.e. 1,920 output samples,
        // so the settled comparison window starts well past the control point.
        let settled_start = 3_000;
        #[allow(clippy::cast_precision_loss)] // sample count is far below 2^52
        let difference_rms = retuned[settled_start..]
            .iter()
            .zip(&reference_audio[settled_start..])
            .map(|(left, right)| {
                let delta = f64::from(*left) - f64::from(*right);
                delta * delta
            })
            .sum::<f64>()
            / (retuned.len() - settled_start) as f64;
        assert!(
            difference_rms.sqrt() <= 1.0e-4,
            "the pump's retuned AM continuation must match a reference chain retuned at the same block (RMS {:.3e} exceeds 1e-4)",
            difference_rms.sqrt()
        );

        // (c) A channel-bandwidth change must translate to the nearest supported RF profile:
        // 25,000 Hz → Khz12, and both an out-of-range 3,000 Hz and a non-positive value → Khz6.
        // A 5 kHz adjacent carrier modulated at 2 kHz separates them: Khz12 leaks ~0.256 of the
        // 2 kHz component, Khz8 ~0.065, Khz6 ~0.035, and the legacy 25 kHz path leaks far more.
        let adjacent = t10b_two_carrier(5_000.0);
        let wide = run_pipeline(
            DemodMode::AM,
            8_000.0,
            &adjacent,
            None,
            vec![(40, AudioControl::SetChannelBw(25_000.0))],
        );
        assert_eq!(
            wide.len(),
            T10B_SAMPLES_250MS,
            "a bandwidth change must not disturb the output clock"
        );
        let wide_leak = t10b_tone_amplitude(&wide[T10B_AM_LATENCY + 4_800..], 2_000.0);
        assert!(
            wide_leak > 0.15,
            "SetChannelBw(25_000) must map literally to AmRfProfile::Khz12 (2 kHz leak ~0.256); measured {wide_leak:.4}"
        );

        for out_of_range in [3_000.0_f64, 0.0, -1.0] {
            let narrow = run_pipeline(
                DemodMode::AM,
                8_000.0,
                &adjacent,
                None,
                vec![(40, AudioControl::SetChannelBw(out_of_range))],
            );
            assert_eq!(
                narrow.len(),
                T10B_SAMPLES_250MS,
                "SetChannelBw({out_of_range}) must not panic or disturb the clock"
            );
            assert!(
                narrow.iter().all(|sample| sample.is_finite()),
                "SetChannelBw({out_of_range}) must leave finite audio"
            );
            let narrow_leak = t10b_tone_amplitude(&narrow[T10B_AM_LATENCY + 4_800..], 2_000.0);
            assert!(
                narrow_leak < 0.06,
                "SetChannelBw({out_of_range}) must clamp to AmRfProfile::Khz6 (2 kHz leak ~0.035); measured {narrow_leak:.4}"
            );
        }
    }

    #[test]
    fn adding_and_removing_am_vfos_keeps_cursor_state() {
        let single = t10b_single_carrier();
        let untouched = run_pipeline(DemodMode::AM, 8_000.0, &single, None, Vec::new());

        let churned = run_pipeline(
            DemodMode::AM,
            8_000.0,
            &single,
            None,
            vec![
                (
                    40,
                    AudioControl::AddVfo {
                        id: 7,
                        freq: 100_500_000.0,
                        channel_bw: 8_000.0,
                        mode: DemodMode::AM,
                    },
                ),
                (41, AudioControl::RemoveVfo(7)),
            ],
        );

        // The cursor chain must be untouched by VFO churn: same clock, same bits. A rebuild, a
        // shared-resampler flush, or an AM/Legacy flip shows up as a transient here.
        t10b_assert_all_points_identical(&churned, &untouched, "AM VFO add/remove");
        assert_eq!(
            churned.len(),
            T10B_SAMPLES_250MS,
            "adding and removing an AM VFO must not change the RF-clock duration"
        );

        // A VFO that survives must not disturb the cursor either, and must stay finite.
        let with_vfo = run_pipeline(
            DemodMode::AM,
            8_000.0,
            &single,
            None,
            vec![(
                40,
                AudioControl::AddVfo {
                    id: 8,
                    freq: 100_020_000.0,
                    channel_bw: 8_000.0,
                    mode: DemodMode::AM,
                },
            )],
        );
        assert_eq!(
            with_vfo.len(),
            T10B_SAMPLES_250MS,
            "a live AM VFO must keep the RF-clock duration"
        );
        assert!(
            with_vfo.iter().all(|sample| sample.is_finite()),
            "mixing an AM VFO must stay finite"
        );

        // The added VFO must itself be an AM chain: with the cursor muted and the VFO tuned to
        // the desired channel, the surviving audio must show AM selectivity. The legacy envelope
        // VFO measures −18.6 dB here, the wired chain ~63 dB.
        let adjacent = t10b_two_carrier(T10B_ADJACENT_HZ);
        let vfo_only = run_pipeline(
            DemodMode::AM,
            8_000.0,
            &adjacent,
            None,
            vec![
                (0, AudioControl::Mute(true)),
                (
                    0,
                    AudioControl::AddVfo {
                        id: 9,
                        freq: 100_000_000.0,
                        channel_bw: 8_000.0,
                        mode: DemodMode::AM,
                    },
                ),
            ],
        );
        assert_eq!(
            vfo_only.len(),
            T10B_SAMPLES_250MS,
            "a muted cursor with an AM VFO must keep the RF-clock duration"
        );
        let vfo_rejection_db = t10b_adjacent_rejection(&vfo_only[T10B_AM_LATENCY..]);
        assert!(
            vfo_rejection_db >= 40.0,
            "an AM VFO must run the wired AmDemodulator (>= 40 dB adjacent rejection); measured {vfo_rejection_db:.2} dB, which is the legacy envelope path"
        );
    }

    #[test]
    fn mixed_am_and_fm_falls_back_to_the_legacy_path_without_panicking() {
        // Documented fallback: a mixed AM/FM stream uses the legacy demodulator for every chain,
        // so no AM-specific telemetry or effective-cutoff accessor is available on this path and
        // none is required by it.
        let adjacent = t10b_two_carrier(T10B_ADJACENT_HZ);

        let cursor_fm_am_vfo = run_pipeline(
            DemodMode::FM,
            200_000.0,
            &adjacent,
            None,
            vec![(
                0,
                AudioControl::AddVfo {
                    id: 1,
                    freq: 100_000_000.0,
                    channel_bw: 8_000.0,
                    mode: DemodMode::AM,
                },
            )],
        );
        assert_eq!(
            cursor_fm_am_vfo.len(),
            T10B_SAMPLES_250MS,
            "a mixed FM cursor with an AM VFO must still deliver the exact RF-clock duration"
        );
        assert!(
            cursor_fm_am_vfo.iter().all(|sample| sample.is_finite()),
            "a mixed FM cursor with an AM VFO must not emit NaN or infinity"
        );

        let cursor_am_fm_vfo = run_pipeline(
            DemodMode::AM,
            8_000.0,
            &adjacent,
            None,
            vec![(
                0,
                AudioControl::AddVfo {
                    id: 2,
                    freq: 100_000_000.0,
                    channel_bw: 200_000.0,
                    mode: DemodMode::FM,
                },
            )],
        );
        assert_eq!(
            cursor_am_fm_vfo.len(),
            T10B_SAMPLES_250MS,
            "an AM cursor with an FM VFO must still deliver the exact RF-clock duration"
        );
        assert!(
            cursor_am_fm_vfo.iter().all(|sample| sample.is_finite()),
            "an AM cursor with an FM VFO must not emit NaN or infinity"
        );

        // Legacy fallback evidence: the AM VFO must NOT contribute AM-chain selectivity, because
        // the mixed stream runs the legacy envelope path throughout.
        for (name, samples) in [
            ("FM cursor + AM VFO", &cursor_fm_am_vfo),
            ("AM cursor + FM VFO", &cursor_am_fm_vfo),
        ] {
            let rejection_db = t10b_adjacent_rejection(&samples[T10B_AM_LATENCY..]);
            assert!(
                rejection_db < 0.0,
                "{name} must fall back to the legacy path (< 0 dB adjacent rejection); measured {rejection_db:.2} dB, which means an AM chain was mixed into a legacy stream"
            );
        }
    }
}
