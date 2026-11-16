//! Signal-aware AM squelch: pre-demodulation channel power to an open/closed gate.
//!
//! This is the gate described in section 4.3 of the audio-quality plan. It consumes the
//! per-block channel power already computed by [`crate::audio::channelizer`] and decides whether
//! audio should pass, so squelch no longer depends on AGC-normalized audio amplitude.
//!
//! ```text
//! ChannelizedBlock.power_dbfs → SquelchState::process → SquelchUpdate { open, threshold, floor }
//! ```
//!
//! # Frozen API
//!
//! ```text
//! pub(crate) struct ChannelPowerSample { pub power_dbfs: f32, pub frames: usize }
//! pub(crate) struct SquelchTransition { pub at_frame: u64, pub open: bool }
//! pub(crate) struct SquelchUpdate {
//!     pub open: bool,
//!     pub threshold_dbfs: f32,
//!     pub noise_floor_dbfs: f32,
//!     pub frames_elapsed: u64,
//!     pub transition: Option<SquelchTransition>,
//! }
//!
//! impl SquelchState {
//!     pub fn new(mode: SquelchMode, sample_rate_hz: f64) -> anyhow::Result<Self>;
//!     pub fn process(&mut self, sample: ChannelPowerSample) -> anyhow::Result<SquelchUpdate>;
//!     pub fn set_mode(&mut self, mode: SquelchMode) -> anyhow::Result<()>;
//!     pub fn is_open(&self) -> bool;
//!     pub fn threshold_dbfs(&self) -> f32;
//!     pub fn noise_floor_dbfs(&self) -> f32;
//! }
//! ```
//!
//! # Units and conventions
//!
//! * Every power is pre-demodulation channel power in dBFS, the same quantity
//!   `ChannelizedBlock::power_dbfs` reports: `10 * log10(mean(norm_sqr))` of the channelized I/Q,
//!   floored at [`POWER_FLOOR_DBFS`]. A unit-amplitude complex tone reads 0 dBFS.
//! * Every *frame* is one channelized I/Q sample at the rate passed to [`SquelchState::new`],
//!   which must be the channelizer's output rate (48 kHz in the AM pipeline). A frame is one
//!   power measurement slot, not one RF sample.
//! * Every duration is expressed in milliseconds by the constants below and converted to frames
//!   exactly once, at construction, by `round(milliseconds * sample_rate_hz / 1000)`, clamped to
//!   at least one frame. All internal timing is therefore integer frame counting: the state
//!   machine never reads a wall clock, and it is deterministic for a given power timeline.
//! * `frames_elapsed` is the stream clock: the number of frames consumed since construction, so
//!   the next frame to be measured has index `frames_elapsed`.
//! * `SquelchTransition::at_frame` is a stream clock value in frames consumed, not an offset: the
//!   change is complete once exactly `at_frame` frames have been consumed since construction. The
//!   frame that completes the confirmation interval is therefore index `at_frame - 1`, and the
//!   first frame the new state governs is index `at_frame`. A gate that opens after 480 frames of
//!   signal starting at stream frame 0 reports `at_frame == 480`, and `SquelchUpdate` never
//!   reports `at_frame` ahead of the `frames_elapsed` of the block that carried it.
//! * The documented timing granularity is one frame: at 48 kHz, 21 µs. Transitions are resolved
//!   to that granularity regardless of how the caller partitioned the stream into blocks.
//!
//! # Defaults encoded here (plan section 4.3)
//!
//! * Open threshold: `noise_floor + margin`. The margin comes from [`SquelchMode::Auto`] and
//!   defaults to 6 dB in [`crate::audio::profile`].
//! * Close threshold: open threshold − [`CLOSE_HYSTERESIS_DB`] (3 dB of hysteresis).
//! * Open confirmation: [`OPEN_CONFIRM_MS`] (10 ms) of *consecutive* frames at or above the open
//!   threshold. Any frame below it resets the confirmation counter.
//! * Close hang: [`CLOSE_HANG_MS`] (250 ms) of *consecutive* frames below the close threshold.
//!   Any frame at or above it resets the hang counter.
//! * Noise floor: exponential tracker with the per-frame coefficient derived from
//!   [`NOISE_FLOOR_TIME_CONSTANT_MS`]. It is updated only while the gate is closed *and* the
//!   block power is below the close threshold, so an active transmission cannot drag the floor
//!   upward and slowly release the gate.
//! * [`SquelchMode::Manual`] replaces the derived open threshold with the configured dBFS value
//!   verbatim; hysteresis and the timers still apply. The floor is still tracked for telemetry
//!   but never feeds the threshold.
//! * [`SquelchMode::Open`] never squelches: the gate is open from construction, every update
//!   reports open, and switching to it releases an already closed gate.
//!
//! # Why a block's power is used as-is for timing
//!
//! A [`ChannelPowerSample`] carries one power measurement for a whole block, so the only
//! self-consistent assumption is that the power is constant across the block's frames. The
//! timers are driven by that value directly, *without a second detector filter*: an extra
//! smoothing stage would add a lag that depends on how the caller partitioned the stream, which
//! is exactly what the partition-invariance test forbids, and it would make the 10 ms / 250 ms
//! contract unverifiable. The block mean is already the measurement smoothing — the channelizer
//! averages `norm_sqr` over every frame it reports.
//!
//! Because the power is constant within a block, the gate can change at most once per block, and
//! the exact frame of that change is computable rather than quantized to the block boundary. This
//! is what makes `at_frame` independent of block partitioning.
//!
//! The same piecewise treatment applies to the noise floor: a block that carries a gate change
//! updates the floor only for the frames *after* the change, so its contribution does not depend
//! on how the caller grouped those frames into blocks. A block that is open for its whole duration
//! never updates the floor at all.
//!
//! # Initial state
//!
//! The gate starts closed in [`SquelchMode::Auto`] and [`SquelchMode::Manual`], and the first
//! accepted block seeds the noise floor instead of requiring a silent lead-in. Before any block,
//! [`SquelchState::noise_floor_dbfs`] reports [`POWER_FLOOR_DBFS`].
//!
//! # Invalid input
//!
//! Configuration is validated at construction and on every [`SquelchState::set_mode`]: a
//! non-finite auto margin or manual threshold, and a non-finite, zero, or negative sample rate,
//! are errors. A non-finite block power is rejected by [`SquelchState::process`] *before* any
//! state changes, so a NaN can never poison the noise floor. A zero-frame block is accepted and
//! changes nothing.

// The AM pipeline starts consuming this module in Plan Task 10; until that wiring lands only the
// inline tests exercise it, so the public surface looks unused. Remove this allow with the Task 10
// wiring.
#![allow(dead_code)]

use anyhow::{Context as _, Result};

use crate::audio::profile::SquelchMode;

/// Power reported for silent channelized input, in dBFS.
///
/// Mirrors `channelizer::POWER_FLOOR_DBFS`; a floor rather than `-inf` keeps every value in this
/// module finite so telemetry and JSON never carry an infinity.
pub(crate) const POWER_FLOOR_DBFS: f32 = -200.0;

/// Hysteresis between the open and close thresholds, in decibels.
pub(crate) const CLOSE_HYSTERESIS_DB: f32 = 3.0;

/// Signal duration required before the gate opens, in milliseconds.
pub(crate) const OPEN_CONFIRM_MS: f64 = 10.0;

/// Silence duration required before the gate closes, in milliseconds.
pub(crate) const CLOSE_HANG_MS: f64 = 250.0;

/// Time constant of the noise-floor tracker, in milliseconds.
///
/// The tracker must be fast enough to follow aircraft-band noise that drifts a few decibels per
/// second — a 5 dB/s ramp lags about `5 dB/s * 0.25 s = 1.25 dB` — and slow enough not to move
/// measurably during the 10 ms a real transmission needs to open the gate.
pub(crate) const NOISE_FLOOR_TIME_CONSTANT_MS: f64 = 250.0;

/// Largest block a single [`ChannelPowerSample`] may describe, in frames.
///
/// One second at 48 kHz. The gate counts its intervals frame by frame, so an absurd block would
/// stall the audio thread inside one call; the real channelizer emits about 96 frames per block
/// and the bound only rejects caller-side mistakes.
pub(crate) const MAX_BLOCK_FRAMES: usize = 48_000;

/// One channel power measurement: the block mean of a `ChannelizedBlock`, in dBFS.
///
/// `frames` is the number of channelized frames the measurement covers, at the sample rate given
/// to [`SquelchState::new`]. Frames rather than a duration keeps the milliseconds-to-sample
/// conversion in exactly one place (the state's rate, at construction) and avoids a second
/// floating-point conversion in the audio hot path, where the caller already knows
/// `block.iq.len()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ChannelPowerSample {
    /// Block-mean channel power, in dBFS, floored at [`POWER_FLOOR_DBFS`]. Must be finite.
    pub power_dbfs: f32,
    /// Number of channelized frames this measurement covers, in frames. `0` is a valid no-op.
    pub frames: usize,
}

/// One change of the gate state, reported by [`SquelchState::process`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SquelchTransition {
    /// Stream clock of the change, in frames consumed: the change is complete once this many
    /// frames have been consumed, and frame index `at_frame` is the first frame it governs.
    pub at_frame: u64,
    /// Gate state from `at_frame` onward.
    pub open: bool,
}

/// Snapshot of the gate and its thresholds after one [`SquelchState::process`] call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SquelchUpdate {
    /// Whether audio should pass from this point in the stream.
    pub open: bool,
    /// Effective open threshold, in dBFS. Auto: `noise_floor + margin`; manual: the configured
    /// value; [`SquelchMode::Open`]: the gate is open and the threshold is not consulted.
    pub threshold_dbfs: f32,
    /// Current noise-floor estimate, in dBFS.
    pub noise_floor_dbfs: f32,
    /// Frames consumed since construction, in frames: the stream clock.
    pub frames_elapsed: u64,
    /// The single change this block caused, if any. At most one transition can occur per block
    /// because a block carries one constant power value.
    pub transition: Option<SquelchTransition>,
}

/// Hysteretic squelch state machine driven by pre-demodulation channel power.
#[derive(Debug)]
pub(crate) struct SquelchState {
    /// Threshold source: auto margin, fixed manual threshold, or unconditionally open.
    mode: SquelchMode,
    /// Frames of consecutive signal needed to open, in frames.
    open_confirm_frames: u64,
    /// Frames of consecutive silence needed to close, in frames.
    close_hang_frames: u64,
    /// Per-frame exponential coefficient of the noise-floor tracker, dimensionless in `0..=1`.
    noise_floor_coefficient: f32,
    /// Current noise-floor estimate, in dBFS. Seeded by the first accepted block.
    noise_floor_dbfs: f32,
    /// Whether the tracker has been seeded by a block yet.
    noise_floor_seeded: bool,
    /// Current gate state.
    open: bool,
    /// Consecutive frames at or above the open threshold so far, in frames.
    confirmed_frames: u64,
    /// Consecutive frames below the close threshold so far, in frames.
    hung_frames: u64,
    /// Frames consumed since construction, in frames: the stream clock.
    frames_elapsed: u64,
}

/// Converts a duration in milliseconds to whole frames at `sample_rate_hz`.
///
/// The conversion is `round(milliseconds * sample_rate_hz / 1000)` clamped to at least one frame,
/// so every timing constant becomes an integer frame count exactly once, at construction. A
/// duration that rounds to zero frames still yields one frame: a confirmation interval of zero
/// frames would open the gate on a single frame and make the 10 ms contract meaningless at rates
/// below 100 Hz.
fn frames_for_ms(milliseconds: f64, sample_rate_hz: f64) -> u64 {
    let frames = (milliseconds * sample_rate_hz / 1_000.0).round();
    if !frames.is_finite() || frames < 1.0 {
        return 1;
    }
    if frames >= 18_446_744_073_709_551_615.0 {
        return u64::MAX;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let whole_frames = frames as u64;
    whole_frames.max(1)
}

impl SquelchState {
    /// Builds a squelch state for `mode` whose frame timing is derived from `sample_rate_hz`.
    ///
    /// `sample_rate_hz` must be the channelizer's output rate, in hertz, because
    /// [`ChannelPowerSample::frames`] counts frames at that rate.
    ///
    /// The gate starts open in [`SquelchMode::Open`] and closed otherwise, with the noise floor at
    /// [`POWER_FLOOR_DBFS`] until the first accepted block seeds it.
    ///
    /// # Errors
    ///
    /// Returns an error when the mode carries a non-finite margin or manual threshold, or when
    /// `sample_rate_hz` is non-finite, zero, or negative.
    pub fn new(mode: SquelchMode, sample_rate_hz: f64) -> Result<Self> {
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            anyhow::bail!("squelch frame rate {sample_rate_hz} Hz is not a positive finite rate");
        }
        mode.validate()
            .with_context(|| format!("invalid squelch mode {mode:?}"))?;

        let open_confirm_frames = frames_for_ms(OPEN_CONFIRM_MS, sample_rate_hz);
        let close_hang_frames = frames_for_ms(CLOSE_HANG_MS, sample_rate_hz);
        let time_constant_frames =
            (NOISE_FLOOR_TIME_CONSTANT_MS * sample_rate_hz / 1_000.0).max(1.0);
        // Per-frame exponential coefficient of a first-order tracker with the 250 ms time
        // constant: alpha = 1 - exp(-dt / tau) with dt = 1 frame. A 250 ms constant at 48 kHz is
        // 12 000 frames, so alpha is about 8.3e-5 and the tracker lags a 5 dB/s ramp by 1.25 dB.
        #[allow(clippy::cast_possible_truncation)]
        let noise_floor_coefficient = (1.0 - (-1.0 / time_constant_frames).exp()) as f32;

        Ok(Self {
            open: matches!(mode, SquelchMode::Open),
            mode,
            open_confirm_frames,
            close_hang_frames,
            noise_floor_coefficient: noise_floor_coefficient.clamp(f32::MIN_POSITIVE, 1.0),
            noise_floor_dbfs: POWER_FLOOR_DBFS,
            noise_floor_seeded: false,
            confirmed_frames: 0,
            hung_frames: 0,
            frames_elapsed: 0,
        })
    }

    /// Advances the state machine by one channel power block and reports the resulting gate.
    ///
    /// The block's power is treated as constant across its `frames`, so any transition is
    /// resolved to the exact frame rather than to the block boundary. Callers pass one sample per
    /// [`crate::audio::channelizer::ChannelizedBlock`]; a zero-frame sample changes nothing.
    ///
    /// # Errors
    ///
    /// A block's whole extent is taken as one constant power, so at most one transition can fall
    /// inside it. [`SquelchUpdate::open`] is the gate state at the *end* of the block: when a
    /// transition is reported, the frames before [`SquelchTransition::at_frame`] belong to the old
    /// state, so a caller that must not pass pre-confirmation audio splits the block there.
    ///
    /// # Errors
    ///
    /// Returns an error, leaving every value reported by this state unchanged, when the frame
    /// clock would overflow, `sample.power_dbfs` is not finite, or the block exceeds
    /// [`MAX_BLOCK_FRAMES`].
    pub fn process(&mut self, sample: ChannelPowerSample) -> Result<SquelchUpdate> {
        if !sample.power_dbfs.is_finite() {
            anyhow::bail!(
                "squelch channel power {} dBFS is not finite",
                sample.power_dbfs
            );
        }
        if sample.frames > MAX_BLOCK_FRAMES {
            anyhow::bail!(
                "squelch block of {} frames exceeds the {MAX_BLOCK_FRAMES}-frame limit; a single \
                 measurement should cover a fraction of a second, not minutes",
                sample.frames
            );
        }
        let block_frames = u64::try_from(sample.frames)
            .with_context(|| format!("squelch block of {} frames exceeds u64", sample.frames))?;
        self.frames_elapsed
            .checked_add(block_frames)
            .with_context(|| {
                format!(
                    "squelch frame clock overflow: {} frames plus {block_frames} frames",
                    self.frames_elapsed
                )
            })?;

        if block_frames > 0 && !self.noise_floor_seeded {
            // Cold start: seed the tracker with the first accepted block's power so an auto
            // stream that begins in silence does not need a lead-in, and so a very quiet stream
            // does not open the gate against the -200 dBFS placeholder floor.
            self.noise_floor_dbfs = sample.power_dbfs;
            self.noise_floor_seeded = true;
        }

        let mut transition = None;
        for _ in 0..block_frames {
            if let Some(open) = self.advance_one_frame(sample.power_dbfs) {
                if transition.is_none() {
                    transition = Some(SquelchTransition {
                        at_frame: self.frames_elapsed,
                        open,
                    });
                }
            }
        }

        Ok(self.snapshot(transition))
    }

    /// Replaces the threshold source, preserving the gate, the timers, and the noise floor.
    ///
    /// Switching to [`SquelchMode::Open`] releases an already closed gate immediately; switching
    /// to [`SquelchMode::Auto`] re-derives the open threshold from the current noise floor.
    ///
    /// The switch itself never reports a transition — the caller initiated it and already knows —
    /// but it never fabricates one either: the gate is only released when the new mode is
    /// [`SquelchMode::Open`]. Leaving [`SquelchMode::Open`] keeps the gate open and lets the hang
    /// timer close it from the next blocks on.
    ///
    /// Both interval counters restart. A new mode or threshold redefines what counts as signal, so
    /// partial progress measured against the previous rule must not let the gate open (or close)
    /// without a full [`OPEN_CONFIRM_MS`] of signal under the new one.
    ///
    /// # Errors
    ///
    /// Returns an error, leaving the previous mode and every other value unchanged, when `mode`
    /// carries a non-finite margin or manual threshold.
    pub fn set_mode(&mut self, mode: SquelchMode) -> Result<()> {
        mode.validate()
            .with_context(|| format!("invalid squelch mode {mode:?}"))?;

        // Both interval counters restart unconditionally. A mode or threshold change redefines
        // what "above the threshold" means, so credit accumulated against the previous rule must
        // not make the gate open (or close) without a full confirmation interval under the new one.
        self.mode = mode;
        self.confirmed_frames = 0;
        self.hung_frames = 0;
        if matches!(mode, SquelchMode::Open) {
            self.open = true;
        }
        Ok(())
    }

    /// Stream clock value: frames consumed since construction.
    ///
    /// Exposed for tests that assert a rejected measurement leaves the clock untouched.
    #[cfg(test)]
    #[must_use]
    pub fn frames_elapsed_for_test(&self) -> u64 {
        self.frames_elapsed
    }

    /// Whether audio should pass in the current state.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Current open threshold, in dBFS.
    ///
    /// Auto mode reports `noise_floor + margin`; manual mode reports the configured threshold
    /// verbatim; [`SquelchMode::Open`] reports [`POWER_FLOOR_DBFS`] because no threshold is
    /// consulted and every frame opens the gate.
    #[must_use]
    pub fn threshold_dbfs(&self) -> f32 {
        match self.mode {
            SquelchMode::Auto { margin_db } => {
                (self.noise_floor_dbfs + margin_db).clamp(-f32::MAX, f32::MAX)
            }
            SquelchMode::Manual { threshold_dbfs } => threshold_dbfs,
            SquelchMode::Open => POWER_FLOOR_DBFS,
        }
    }

    /// Current noise-floor estimate, in dBFS.
    ///
    /// [`POWER_FLOOR_DBFS`] before the first accepted block. In [`SquelchMode::Open`] the floor is
    /// seeded by the first accepted block and then held, because the tracker only follows power
    /// while the gate is closed and an open gate never closes in that mode.
    #[must_use]
    pub fn noise_floor_dbfs(&self) -> f32 {
        self.noise_floor_dbfs
    }

    /// Close threshold, in dBFS: the open threshold less [`CLOSE_HYSTERESIS_DB`].
    fn close_threshold_dbfs(&self) -> f32 {
        (self.threshold_dbfs() - CLOSE_HYSTERESIS_DB).clamp(-f32::MAX, f32::MAX)
    }

    /// Advances the machine by exactly one frame of constant `power_dbfs`.
    ///
    /// Returns the new gate state when this frame completed a confirmation or hang interval, so
    /// the caller can stamp the transition with the stream clock of that frame. The frame that
    /// completes the interval is the last frame of the old state; the new state governs the next
    /// frame, which is exactly the `at_frame` convention in the module docs.
    fn advance_one_frame(&mut self, power_dbfs: f32) -> Option<bool> {
        self.frames_elapsed += 1;

        if matches!(self.mode, SquelchMode::Open) {
            // The gate is open unconditionally, so no interval can complete and the floor stays
            // where the first block seeded it.
            return None;
        }

        if self.open {
            if power_dbfs < self.close_threshold_dbfs() {
                self.hung_frames += 1;
                if self.hung_frames >= self.close_hang_frames {
                    self.open = false;
                    self.hung_frames = 0;
                    self.confirmed_frames = 0;
                    return Some(false);
                }
            } else {
                self.hung_frames = 0;
            }
            return None;
        }

        if power_dbfs >= self.threshold_dbfs() {
            self.confirmed_frames += 1;
            if self.confirmed_frames >= self.open_confirm_frames {
                self.open = true;
                self.confirmed_frames = 0;
                self.hung_frames = 0;
                return Some(true);
            }
        } else {
            self.confirmed_frames = 0;
        }

        // Noise floor tracking: only for a frame the gate observed as closed and whose power is
        // strictly below the close threshold, so an active transmission can never drag the floor
        // upward and slowly release the gate. The same strict comparison as the hang counter keeps
        // both rules describing the identical region, and a power parked exactly on the close
        // threshold neither counts toward the hang nor feeds the floor.
        //
        // The frame that completes the open confirmation never reaches this point: the `return`
        // above fires first, and confirmation also requires power at or above the open threshold,
        // which is a full hysteresis width above the close threshold.
        let close_threshold_dbfs = self.close_threshold_dbfs();
        if power_dbfs < close_threshold_dbfs {
            self.noise_floor_dbfs +=
                self.noise_floor_coefficient * (power_dbfs - self.noise_floor_dbfs);
        }
        None
    }

    /// Snapshot of the current gate, thresholds, clock, and the block's single transition.
    fn snapshot(&self, transition: Option<SquelchTransition>) -> SquelchUpdate {
        SquelchUpdate {
            open: self.open,
            threshold_dbfs: self.threshold_dbfs(),
            noise_floor_dbfs: self.noise_floor_dbfs,
            frames_elapsed: self.frames_elapsed,
            transition,
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use approx::assert_abs_diff_eq;

    use super::*;

    /// Channelized frame rate used by every test, in hertz (the AM pipeline's sink rate).
    const RATE_HZ: f64 = 48_000.0;
    /// Plan default automatic margin above the noise floor, in decibels.
    const AUTO_MARGIN_DB: f32 = 6.0;
    /// Noise floor the timing fixtures settle on, in dBFS.
    const NOISE_FLOOR_DBFS: f32 = -80.0;
    /// Power tolerance allowed while the floor tracks a slow ramp, in decibels.
    const TRACKING_TOLERANCE_DB: f32 = 3.0;
    /// Power tolerance allowed while the floor must not chase a transmission, in decibels.
    const NON_CHASING_TOLERANCE_DB: f32 = 3.0;
    /// Documented transition-time granularity across partitions, in frames.
    const PARTITION_TIMING_GRANULARITY_FRAMES: u64 = 1;

    /// Converts a duration in milliseconds to a channelized frame count at [`RATE_HZ`].
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn frames(milliseconds: f64) -> usize {
        (milliseconds * RATE_HZ / 1_000.0).round() as usize
    }

    /// Converts a duration in milliseconds to a stream frame index at [`RATE_HZ`].
    fn frame_index(milliseconds: f64) -> u64 {
        u64::try_from(frames(milliseconds)).expect("test durations fit in u64 frames")
    }

    /// A state in automatic mode with the plan's 6 dB margin.
    fn auto_state() -> SquelchState {
        SquelchState::new(
            SquelchMode::Auto {
                margin_db: AUTO_MARGIN_DB,
            },
            RATE_HZ,
        )
        .expect("a finite margin at a positive rate must construct")
    }

    /// Feeds `frame_count` frames of constant `power_dbfs` in `block_frames`-frame blocks.
    ///
    /// Returns the update from the final block. The whole fixture is one constant-power segment,
    /// so splitting it is exactly the partition axis the block-boundary contract is about.
    fn feed(
        state: &mut SquelchState,
        power_dbfs: f32,
        frame_count: usize,
        block_frames: usize,
    ) -> SquelchUpdate {
        assert!(
            frame_count > 0 && block_frames > 0,
            "the helper feeds at least one frame in at least one block"
        );
        let mut remaining = frame_count;
        let mut last = None;
        while remaining > 0 {
            let count = remaining.min(block_frames);
            let update = state
                .process(ChannelPowerSample {
                    power_dbfs,
                    frames: count,
                })
                .expect("a finite channel power must be accepted");
            last = Some(update);
            remaining -= count;
        }
        last.expect("the loop feeds at least one block")
    }

    /// One second of quiet at [`NOISE_FLOOR_DBFS`], which seeds and settles the floor.
    fn settle_noise_floor(state: &mut SquelchState) {
        let update = feed(state, NOISE_FLOOR_DBFS, frames(1_000.0), frames(10.0));
        assert!(
            !update.open,
            "the fixture's noise floor must not open the gate: {} dBFS with threshold {} dBFS",
            update.noise_floor_dbfs, update.threshold_dbfs
        );
    }

    #[test]
    fn opens_only_after_ten_milliseconds_above_open_threshold() {
        let mut state = auto_state();
        settle_noise_floor(&mut state);
        assert_abs_diff_eq!(
            state.threshold_dbfs(),
            NOISE_FLOOR_DBFS + AUTO_MARGIN_DB,
            epsilon = 1.0
        );
        let signal_dbfs = state.threshold_dbfs() + 10.0;

        // One frame short of the confirmation interval: still closed.
        let update = feed(&mut state, signal_dbfs, frames(OPEN_CONFIRM_MS) - 1, 1);
        assert!(
            !update.open,
            "the gate must stay closed one frame short of {OPEN_CONFIRM_MS} ms of signal"
        );
        assert_eq!(
            update.transition, None,
            "no gate change may be reported before the confirmation interval elapses"
        );

        // The frame that completes 10 ms of signal opens the gate.
        let update = feed(&mut state, signal_dbfs, 1, 1);
        assert!(
            update.open,
            "the gate must open on the frame that completes {OPEN_CONFIRM_MS} ms above the open threshold"
        );
        assert_eq!(
            update.transition,
            Some(SquelchTransition {
                at_frame: frame_index(1_000.0) + frame_index(OPEN_CONFIRM_MS),
                open: true,
            }),
            "the gate must open exactly {OPEN_CONFIRM_MS} ms after the signal starts"
        );
    }

    #[test]
    fn closes_only_after_250_milliseconds_below_close_threshold() {
        let mut state = auto_state();
        settle_noise_floor(&mut state);
        let close_threshold_dbfs = state.threshold_dbfs() - CLOSE_HYSTERESIS_DB;

        let opened = feed(
            &mut state,
            close_threshold_dbfs + 10.0,
            frames(100.0),
            frames(10.0),
        );
        assert!(
            opened.open,
            "the fixture must open the gate before it drops"
        );
        let drop_frame = opened.frames_elapsed;

        // One frame short of the hang: still open.
        let update = feed(
            &mut state,
            close_threshold_dbfs - 10.0,
            frames(CLOSE_HANG_MS) - 1,
            frames(10.0),
        );
        assert!(
            update.open,
            "the gate must stay open one frame short of {CLOSE_HANG_MS} ms below the close threshold"
        );
        assert_eq!(
            update.transition, None,
            "no gate change may be reported before the hang elapses"
        );

        // The frame that completes 250 ms below the close threshold closes it.
        let update = feed(&mut state, close_threshold_dbfs - 10.0, 1, 1);
        assert!(
            !update.open,
            "the gate must close after {CLOSE_HANG_MS} ms below the close threshold"
        );
        assert_eq!(
            update.transition,
            Some(SquelchTransition {
                at_frame: drop_frame + frame_index(CLOSE_HANG_MS),
                open: false,
            }),
            "the gate must close exactly {CLOSE_HANG_MS} ms after the signal drops below the close threshold"
        );
    }

    #[test]
    fn does_not_chatter_for_power_oscillating_inside_hysteresis() {
        let mut state = auto_state();
        settle_noise_floor(&mut state);
        let open_threshold_dbfs = state.threshold_dbfs();
        let close_threshold_dbfs = open_threshold_dbfs - CLOSE_HYSTERESIS_DB;

        let opened = feed(
            &mut state,
            open_threshold_dbfs + 12.0,
            frames(100.0),
            frames(10.0),
        );
        assert!(
            opened.open,
            "a sustained signal must open the gate before the oscillation starts"
        );

        // A 1 kHz square wave strictly inside the hysteresis band for 2 s: one dwell level sits
        // just above the close threshold, the other just below the open threshold, so neither
        // threshold is ever crossed and a 500 µs dwell is far shorter than the 250 ms hang.
        let half_period_frames = frames(0.5);
        assert!(
            half_period_frames > 0,
            "half a millisecond must be at least one frame at 48 kHz"
        );
        let mut transitions = Vec::new();
        for cycle in 0..2_000_usize {
            for level_dbfs in [close_threshold_dbfs + 0.5, open_threshold_dbfs - 0.5] {
                let update = feed(
                    &mut state,
                    level_dbfs,
                    half_period_frames,
                    half_period_frames,
                );
                if let Some(transition) = update.transition {
                    transitions.push((cycle, transition));
                }
                assert!(
                    update.open,
                    "the gate closed during in-band oscillation at cycle {cycle}"
                );
            }
        }

        assert!(
            transitions.is_empty(),
            "power oscillating inside the hysteresis band must never toggle the gate, but it \
             reported {} transition(s), first at cycle {}",
            transitions.len(),
            transitions
                .first()
                .map_or(u64::MAX, |(cycle, _)| u64::try_from(*cycle)
                    .unwrap_or(u64::MAX))
        );
        assert!(
            state.is_open(),
            "the gate must still be open after 2 s of in-band oscillation"
        );
    }

    #[test]
    fn noise_floor_follows_slow_noise_changes_while_closed() {
        const START_DBFS: f32 = -100.0;
        const RAMP_DB: f32 = 20.0;
        const RAMP_SECONDS: f64 = 4.0;
        const STEP_MS: f64 = 100.0;

        let mut state = auto_state();
        let seeded = feed(&mut state, START_DBFS, frames(STEP_MS), frames(STEP_MS));
        assert_abs_diff_eq!(seeded.noise_floor_dbfs, START_DBFS, epsilon = 0.5);

        // A 5 dB/s ramp: slow compared with the floor's time constant, so an exponential tracker
        // lags by well under the tolerance instead of failing to follow.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let steps = (RAMP_SECONDS * 1_000.0 / STEP_MS) as usize;
        assert!(steps > 1, "the ramp must span several blocks");
        for step in 1..=steps {
            #[allow(clippy::cast_precision_loss)]
            let fraction = step as f32 / steps as f32;
            let level_dbfs = START_DBFS + RAMP_DB * fraction;
            let update = feed(&mut state, level_dbfs, frames(STEP_MS), frames(STEP_MS));
            assert!(
                !update.open,
                "rising noise must not open the gate at {level_dbfs} dBFS"
            );
            assert_abs_diff_eq!(
                update.threshold_dbfs,
                level_dbfs + AUTO_MARGIN_DB,
                epsilon = TRACKING_TOLERANCE_DB
            );
        }
    }

    #[test]
    fn noise_floor_does_not_chase_an_active_transmission() {
        let mut state = auto_state();
        settle_noise_floor(&mut state);
        let baseline_floor_dbfs = state.noise_floor_dbfs();

        let update = feed(
            &mut state,
            baseline_floor_dbfs + 30.0,
            frames(2_000.0),
            frames(10.0),
        );
        assert!(
            update.open,
            "a 30 dB signal must open the gate for the whole transmission"
        );
        assert_abs_diff_eq!(
            update.noise_floor_dbfs,
            baseline_floor_dbfs,
            epsilon = NON_CHASING_TOLERANCE_DB
        );
    }

    #[test]
    fn manual_mode_uses_the_exact_dbfs_threshold() {
        const MANUAL_THRESHOLD_DBFS: f32 = -55.0;

        let mut state = SquelchState::new(
            SquelchMode::Manual {
                threshold_dbfs: MANUAL_THRESHOLD_DBFS,
            },
            RATE_HZ,
        )
        .expect("a finite manual threshold at a positive rate must construct");
        assert_abs_diff_eq!(
            state.threshold_dbfs(),
            MANUAL_THRESHOLD_DBFS,
            epsilon = f32::EPSILON
        );

        // 1 dB below the manual threshold: closed for a full second, and the threshold stays put.
        let update = feed(
            &mut state,
            MANUAL_THRESHOLD_DBFS - 1.0,
            frames(1_000.0),
            frames(10.0),
        );
        assert!(
            !update.open,
            "{MANUAL_THRESHOLD_DBFS} dBFS manual threshold must keep a {} dBFS signal closed",
            MANUAL_THRESHOLD_DBFS - 1.0
        );
        assert_abs_diff_eq!(
            state.threshold_dbfs(),
            MANUAL_THRESHOLD_DBFS,
            epsilon = f32::EPSILON
        );

        // 1 dB above the manual threshold: the same 10 ms confirmation applies.
        let update = feed(
            &mut state,
            MANUAL_THRESHOLD_DBFS + 1.0,
            frames(OPEN_CONFIRM_MS) - 1,
            1,
        );
        assert!(
            !update.open,
            "manual mode must not open one frame short of the confirmation interval"
        );
        let update = feed(&mut state, MANUAL_THRESHOLD_DBFS + 1.0, 1, 1);
        assert!(
            update.open,
            "a signal above the manual threshold must open after {OPEN_CONFIRM_MS} ms"
        );
    }

    #[test]
    fn open_mode_never_squelches() {
        let mut state =
            SquelchState::new(SquelchMode::Open, RATE_HZ).expect("Open mode is always valid");
        assert!(
            state.is_open(),
            "Open mode must not squelch before the first block"
        );

        for block in 0..200_usize {
            let update = feed(&mut state, POWER_FLOOR_DBFS, frames(10.0), frames(10.0));
            assert!(
                update.open,
                "Open mode squelched a {POWER_FLOOR_DBFS} dBFS block (block {block})"
            );
            assert_eq!(
                update.transition, None,
                "Open mode must not report gate transitions for silent input"
            );
        }

        // Switching an already closed gate into Open mode releases it too.
        let mut manual = SquelchState::new(
            SquelchMode::Manual {
                threshold_dbfs: -40.0,
            },
            RATE_HZ,
        )
        .expect("a finite manual threshold at a positive rate must construct");
        feed(&mut manual, -100.0, frames(100.0), frames(10.0));
        assert!(!manual.is_open(), "the fixture must start closed");
        manual
            .set_mode(SquelchMode::Open)
            .expect("Open mode is always valid");
        assert!(
            manual.is_open(),
            "switching to Open mode must release a closed gate"
        );
    }

    /// One constant-power segment of the partition-invariance timeline, in dBFS and milliseconds.
    ///
    /// Segment boundaries are real power steps and each is delivered as its own block; the
    /// partition axis only subdivides *within* a segment, where the power genuinely is constant.
    const TIMELINE: [(f32, f64); 4] = [
        (NOISE_FLOOR_DBFS, 400.0),        // quiet: floor settles at -80 dBFS
        (NOISE_FLOOR_DBFS + 30.0, 300.0), // transmission: opens 10 ms in
        (NOISE_FLOOR_DBFS - 15.0, 400.0), // silence: closes 250 ms in
        (NOISE_FLOOR_DBFS + 28.0, 200.0), // second transmission: opens 10 ms in
    ];

    /// Splits `frame_count` frames into blocks of at most `size` frames.
    fn fixed_partition(frame_count: usize, size: usize) -> Vec<usize> {
        let mut sizes = Vec::new();
        let mut remaining = frame_count;
        while remaining > 0 {
            let count = remaining.min(size);
            sizes.push(count);
            remaining -= count;
        }
        sizes
    }

    /// Splits `frame_count` frames into one block, i.e. the coarsest legal partition.
    fn single_partition(frame_count: usize) -> Vec<usize> {
        vec![frame_count]
    }

    /// Splits `frame_count` frames into irregular blocks from a fixed-seed generator.
    ///
    /// Deterministic on purpose: the partition axis must be reproducible, so no RNG crate and no
    /// clock is involved.
    fn irregular_partition(frame_count: usize) -> Vec<usize> {
        let mut state = 0x6d2b_79f5_u32;
        let mut remaining = frame_count;
        let mut sizes = Vec::new();
        while remaining > 0 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let requested = 1 + usize::try_from(state % 4_096).expect("masked to fit usize");
            let count = requested.min(remaining);
            sizes.push(count);
            remaining -= count;
        }
        sizes
    }

    /// Runs [`TIMELINE`] through a fresh state, subdividing each segment with `partition`.
    ///
    /// Returns every gate transition in stream order.
    fn run_timeline(partition: &dyn Fn(usize) -> Vec<usize>) -> Vec<SquelchTransition> {
        let mut state = auto_state();
        let mut transitions = Vec::new();
        for &(power_dbfs, milliseconds) in &TIMELINE {
            for block_frames in partition(frames(milliseconds)) {
                let update = state
                    .process(ChannelPowerSample {
                        power_dbfs,
                        frames: block_frames,
                    })
                    .expect("a finite channel power must be accepted");
                if let Some(transition) = update.transition {
                    transitions.push(transition);
                }
            }
        }
        transitions
    }

    #[test]
    fn behavior_is_independent_of_block_partitioning() {
        let one_millisecond = frames(1.0);
        let ten_milliseconds = frames(10.0);
        assert!(
            one_millisecond > 0 && ten_milliseconds > one_millisecond,
            "the fixture must exercise real 1 ms and 10 ms block sizes"
        );

        let reference = run_timeline(&|frame_count| fixed_partition(frame_count, one_millisecond));
        assert_eq!(
            reference.len(),
            3,
            "the fixture timeline must produce open, close, open: {reference:?}"
        );

        for (name, partition) in [
            (
                "10 ms",
                &(|frame_count: usize| fixed_partition(frame_count, ten_milliseconds))
                    as &dyn Fn(usize) -> Vec<usize>,
            ),
            (
                "irregular",
                &(|frame_count: usize| irregular_partition(frame_count))
                    as &dyn Fn(usize) -> Vec<usize>,
            ),
            (
                "single block per segment",
                &(|frame_count: usize| single_partition(frame_count))
                    as &dyn Fn(usize) -> Vec<usize>,
            ),
        ] {
            let candidate = run_timeline(partition);
            assert_eq!(
                candidate.len(),
                reference.len(),
                "{name} partitions changed the number of gate transitions: {candidate:?} \
                 versus {reference:?}"
            );
            for (index, (wanted, got)) in reference.iter().zip(&candidate).enumerate() {
                assert_eq!(
                    got.open, wanted.open,
                    "{name} partitions changed transition {index} direction"
                );
                assert!(
                    got.at_frame.abs_diff(wanted.at_frame) <= PARTITION_TIMING_GRANULARITY_FRAMES,
                    "{name} partitions moved transition {index} by {} frames, beyond the \
                     documented {PARTITION_TIMING_GRANULARITY_FRAMES}-frame granularity",
                    got.at_frame.abs_diff(wanted.at_frame)
                );
            }
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one test per frozen name: the rejection matrix is the test
    fn invalid_configuration_is_a_typed_or_contextual_error() {
        // Non-finite configuration is rejected at construction.
        for mode in [
            SquelchMode::Auto {
                margin_db: f32::NAN,
            },
            SquelchMode::Auto {
                margin_db: f32::INFINITY,
            },
            SquelchMode::Auto {
                margin_db: f32::NEG_INFINITY,
            },
            SquelchMode::Manual {
                threshold_dbfs: f32::NAN,
            },
            SquelchMode::Manual {
                threshold_dbfs: f32::INFINITY,
            },
            SquelchMode::Manual {
                threshold_dbfs: f32::NEG_INFINITY,
            },
        ] {
            assert!(
                SquelchState::new(mode, RATE_HZ).is_err(),
                "a non-finite squelch configuration must be rejected: {mode:?}"
            );
        }

        // A rate that cannot express frames is rejected.
        for rate_hz in [
            0.0,
            -1.0,
            -48_000.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            assert!(
                SquelchState::new(SquelchMode::Open, rate_hz).is_err(),
                "sample rate {rate_hz} Hz must be rejected"
            );
        }

        // A rejected mode switch leaves the previous mode, gate, and threshold untouched.
        let mut state = SquelchState::new(
            SquelchMode::Manual {
                threshold_dbfs: -55.0,
            },
            RATE_HZ,
        )
        .expect("a finite manual threshold at a positive rate must construct");
        feed(&mut state, -100.0, frames(100.0), frames(10.0));
        let threshold_before = state.threshold_dbfs();
        let open_before = state.is_open();
        assert!(
            state
                .set_mode(SquelchMode::Auto {
                    margin_db: f32::NAN
                })
                .is_err(),
            "set_mode must reject a non-finite auto margin"
        );
        assert_eq!(
            state.threshold_dbfs().to_bits(),
            threshold_before.to_bits(),
            "a rejected set_mode must not change the threshold"
        );
        assert_eq!(
            state.is_open(),
            open_before,
            "a rejected set_mode must not change the gate"
        );

        // A non-finite power measurement is rejected without poisoning the noise floor.
        let mut state = auto_state();
        settle_noise_floor(&mut state);
        let clock_before = feed(&mut state, NOISE_FLOOR_DBFS, 1, 1);
        let floor_before = clock_before.noise_floor_dbfs;
        for bad_power_dbfs in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                state
                    .process(ChannelPowerSample {
                        power_dbfs: bad_power_dbfs,
                        frames: frames(10.0),
                    })
                    .is_err(),
                "a {bad_power_dbfs} dBFS power measurement must be rejected"
            );
        }
        let recovered = feed(&mut state, NOISE_FLOOR_DBFS, 1, 1);
        assert_eq!(
            recovered.frames_elapsed,
            clock_before.frames_elapsed + 1,
            "a rejected measurement must not advance the stream clock"
        );
        assert!(
            recovered.noise_floor_dbfs.is_finite(),
            "the noise floor must stay finite after rejected measurements"
        );
        assert_abs_diff_eq!(
            recovered.noise_floor_dbfs,
            floor_before,
            epsilon = NON_CHASING_TOLERANCE_DB
        );
        assert_eq!(
            state
                .process(ChannelPowerSample {
                    power_dbfs: NOISE_FLOOR_DBFS,
                    frames: 0,
                })
                .expect("a zero-frame measurement is a valid no-op")
                .frames_elapsed,
            recovered.frames_elapsed,
            "a zero-frame measurement must not advance the stream clock"
        );
    }

    #[test]
    fn plan_timing_constants_hold_their_documented_values() {
        // The other timing fixtures derive their expectations from these same constants, so a
        // mutation that changed one would move both sides and survive. Pin the plan's numbers
        // literally here, and pin the frame conversions they produce at 48 kHz.
        assert_abs_diff_eq!(OPEN_CONFIRM_MS, 10.0, epsilon = f64::EPSILON);
        assert_abs_diff_eq!(CLOSE_HANG_MS, 250.0, epsilon = f64::EPSILON);
        assert_abs_diff_eq!(CLOSE_HYSTERESIS_DB, 3.0, epsilon = f32::EPSILON);
        assert_abs_diff_eq!(NOISE_FLOOR_TIME_CONSTANT_MS, 250.0, epsilon = f64::EPSILON);
        assert_abs_diff_eq!(POWER_FLOOR_DBFS, -200.0, epsilon = f32::EPSILON);

        // Timings measured in absolute frames, not in units of the constants under test.
        let mut state = auto_state();
        settle_noise_floor(&mut state);
        let signal_dbfs = state.threshold_dbfs() + 10.0;
        let opened = feed(&mut state, signal_dbfs, 480, 480);
        assert!(
            opened.open,
            "480 frames (10 ms at 48 kHz) above the threshold must open the gate"
        );
        assert_eq!(
            opened.transition,
            Some(SquelchTransition {
                at_frame: 48_480,
                open: true,
            }),
            "the gate must open exactly 480 frames after a 48000-frame silent lead-in"
        );

        let drop_dbfs = state.threshold_dbfs() - CLOSE_HYSTERESIS_DB - 10.0;
        let still_open = feed(&mut state, drop_dbfs, 11_999, 11_999);
        assert!(
            still_open.open && still_open.transition.is_none(),
            "11999 frames below the close threshold must not close a 250 ms hang"
        );
        let closed = feed(&mut state, drop_dbfs, 1, 1);
        assert_eq!(
            closed.transition,
            Some(SquelchTransition {
                at_frame: 48_480 + 11_999 + 1,
                open: false,
            }),
            "the gate must close exactly 12000 frames after the signal dropped"
        );
    }

    #[test]
    fn hysteresis_band_is_three_decibels_wide() {
        // With a settled floor at -80 dBFS and the plan's 6 dB margin the open threshold is
        // -74 dBFS, so the close threshold is -77 dBFS. Open the gate with a strong signal first,
        // then drop into the band: -76 dBFS sits above the close threshold and must hold the gate
        // open, while -79 dBFS sits below it and must let the gate close.
        let mut held = auto_state();
        settle_noise_floor(&mut held);
        assert_abs_diff_eq!(held.threshold_dbfs(), -74.0, epsilon = 1.0);
        let opened = feed(&mut held, -68.0, 4_800, 4_800);
        assert!(
            opened.open,
            "the fixture must open the gate before dropping"
        );

        // 12,000 frames = 250 ms, so a gate that does close has had the full hang.
        let inside = feed(&mut held, -76.0, 12_000, 12_000);
        assert!(
            inside.open,
            "a signal 1 dB above the close threshold must keep the gate open, got threshold \
             {} dBFS",
            inside.threshold_dbfs
        );

        let mut dropped = auto_state();
        settle_noise_floor(&mut dropped);
        feed(&mut dropped, -68.0, 4_800, 4_800);
        let below_band = feed(&mut dropped, -79.0, 12_000, 12_000);
        assert!(
            !below_band.open,
            "a signal 2 dB below the close threshold must close the gate"
        );
    }

    #[test]
    fn open_confirmation_resets_when_power_falls_back_inside_the_band() {
        // Nine milliseconds above the threshold, then one millisecond below it, then nine more
        // above: the confirmation counter must restart, so the gate stays closed past the point
        // where an uninterrupted 10 ms above the threshold would have opened it.
        let mut state = auto_state();
        settle_noise_floor(&mut state);
        let open_dbfs = state.threshold_dbfs() + 10.0;
        let quiet_dbfs = state.threshold_dbfs() - 10.0;

        feed(&mut state, open_dbfs, 432, 432);
        let dip = feed(&mut state, quiet_dbfs, 48, 48);
        assert!(!dip.open, "a dip must not open the gate");
        assert_eq!(dip.transition, None, "a dip must not report a transition");

        let resume = feed(&mut state, open_dbfs, 432, 432);
        assert!(
            !resume.open,
            "the confirmation counter must restart after the dip: 432 frames is short of 480"
        );
        let finished = feed(&mut state, open_dbfs, 48, 48);
        assert!(
            finished.open,
            "480 consecutive frames after the dip must open the gate"
        );
    }

    #[test]
    fn close_hang_resets_when_the_signal_returns_above_the_close_threshold() {
        // 200 ms below the close threshold, one loud block, then 200 ms below again: the hang
        // counter restarts, so 400 ms of accumulated low power spread across an interruption
        // must not close the gate.
        let mut state = auto_state();
        settle_noise_floor(&mut state);
        let open_dbfs = state.threshold_dbfs() + 10.0;
        feed(&mut state, open_dbfs, 4_800, 4_800);
        let close_dbfs = state.threshold_dbfs() - CLOSE_HYSTERESIS_DB - 10.0;

        let first = feed(&mut state, close_dbfs, 9_600, 9_600);
        assert!(
            first.open,
            "200 ms below the close threshold is short of the 250 ms hang"
        );
        feed(&mut state, open_dbfs, 480, 480);
        let second = feed(&mut state, close_dbfs, 9_600, 9_600);
        assert!(
            second.open,
            "the hang counter must restart when the signal returns above the close threshold"
        );
        let finished = feed(&mut state, close_dbfs, 2_400, 2_400);
        assert!(
            !finished.open,
            "250 ms of continuous low power after the restart must close the gate"
        );
    }

    #[test]
    fn noise_floor_updates_per_frame_so_block_size_does_not_change_tracking() {
        // A single 1 s block and 100 blocks of 10 ms must leave the floor at the same value: an
        // implementation that applied the tracker once per block would land far away.
        let mut whole = auto_state();
        let whole_update = feed(&mut whole, -100.0, 48_000, 48_000);

        let mut split = auto_state();
        let mut split_update = None;
        for _ in 0..100 {
            split_update = Some(feed(&mut split, -100.0, 480, 480));
        }
        let split_update = split_update.expect("the fixture feeds at least one block");

        assert_abs_diff_eq!(
            whole_update.noise_floor_dbfs,
            split_update.noise_floor_dbfs,
            epsilon = 0.01
        );
        assert_abs_diff_eq!(whole_update.noise_floor_dbfs, -100.0, epsilon = 0.5);
    }

    #[test]
    fn noise_floor_time_constant_is_hundreds_of_milliseconds_not_tens() {
        // The floor only tracks while the gate is closed AND the power is at or below the close
        // threshold, so the step must stay under it: a floor at -120 dBFS puts the close threshold
        // at -117 dBFS, and -117.5 dBFS therefore tracks without ever opening the gate.
        //
        // A one-pole tracker covers 1 - 1/e = 63.2% of a step after one time constant, 95% after
        // three. At 250 ms that makes 250 ms of exposure land near 63% and 1 s near 98%, whereas a
        // 25 ms constant would already be at 99.99% after 250 ms.
        let mut state = auto_state();
        feed(&mut state, -120.0, 48_000, 4_800);
        let step_db = 2.5_f64;

        let early = feed(&mut state, -117.5, 12_000, 4_800);
        let early_coverage = (f64::from(early.noise_floor_dbfs) + 120.0) / step_db;
        assert!(
            (0.45..0.80).contains(&early_coverage),
            "250 ms must cover about 63% of the step (a 25 ms constant would show ~100%), \
             covered {:.1}%",
            early_coverage * 100.0
        );

        let late = feed(&mut state, -117.5, 180_000, 4_800);
        let late_coverage = (f64::from(late.noise_floor_dbfs) + 120.0) / step_db;
        assert!(
            late_coverage > 0.95,
            "4 s of exposure must nearly reach the new floor, covered {:.1}%",
            late_coverage * 100.0
        );
        assert!(
            !late.open,
            "the tracking fixture must never open the gate: {} dBFS against threshold {} dBFS",
            late.noise_floor_dbfs, late.threshold_dbfs
        );
    }

    #[test]
    fn power_exactly_at_the_open_threshold_opens_the_gate() {
        // The threshold comparison is inclusive: `>=`, not `>`. A measurement sitting exactly on
        // the open threshold is a real signal at the agreed boundary, so it must confirm.
        let mut state = auto_state();
        settle_noise_floor(&mut state);
        let threshold_dbfs = state.threshold_dbfs();
        let update = feed(&mut state, threshold_dbfs, 480, 480);
        assert!(
            update.open,
            "a measurement exactly at the open threshold ({threshold_dbfs} dBFS) must open the gate"
        );
    }

    #[test]
    fn power_exactly_at_the_close_threshold_keeps_the_gate_open() {
        // The hang comparison is strict: `<`, not `<=`. A signal exactly at the close threshold has
        // not fallen below it, so it must hold the gate open indefinitely.
        let mut state = auto_state();
        settle_noise_floor(&mut state);
        let open_dbfs = state.threshold_dbfs() + 10.0;
        feed(&mut state, open_dbfs, 4_800, 4_800);
        let close_threshold_dbfs = state.threshold_dbfs() - CLOSE_HYSTERESIS_DB;

        let update = feed(&mut state, close_threshold_dbfs, 48_000, 4_800);
        assert!(
            update.open,
            "1 s at exactly the close threshold ({close_threshold_dbfs} dBFS) must not close the \
             gate: the hang only counts power strictly below it"
        );
        assert_eq!(
            update.transition, None,
            "sitting on the close threshold must report no transition"
        );
    }

    #[test]
    fn noise_floor_never_tracks_power_above_the_close_threshold() {
        // Direct test of the guard: power just above the close threshold must leave the floor
        // untouched even though the gate is still closed, so a slowly rising interferer cannot
        // drag the floor up until it deafens the receiver.
        let mut state = auto_state();
        settle_noise_floor(&mut state);
        let floor_before = state.noise_floor_dbfs();
        let close_threshold_dbfs = state.threshold_dbfs() - CLOSE_HYSTERESIS_DB;

        // Half a decibel above the close threshold, and below the open threshold: no opening, and
        // no tracking either.
        let between_dbfs = close_threshold_dbfs + 0.5;
        assert!(
            between_dbfs < state.threshold_dbfs(),
            "the fixture must sit below the open threshold"
        );
        let update = feed(&mut state, between_dbfs, 48_000, 4_800);
        assert!(!update.open, "the fixture must not open the gate");
        assert_abs_diff_eq!(update.noise_floor_dbfs, floor_before, epsilon = 0.01);
    }

    #[test]
    fn a_mode_change_restarts_the_confirmation_and_hang_counters() {
        // 479 frames of signal against a manual threshold, then the threshold is raised above that
        // signal. The stale credit must not open the gate on the first frame under the new rule:
        // a full 10 ms of signal at or above the *new* threshold is required.
        let mut state = SquelchState::new(
            SquelchMode::Manual {
                threshold_dbfs: -60.0,
            },
            RATE_HZ,
        )
        .expect("a finite manual threshold must construct");

        feed(&mut state, -50.0, 479, 479);
        assert!(!state.is_open(), "479 frames is short of the confirmation");

        state
            .set_mode(SquelchMode::Manual {
                threshold_dbfs: -40.0,
            })
            .expect("raising the threshold is a valid mode");

        let first = feed(&mut state, -39.0, 1, 1);
        assert!(
            !first.open,
            "a mode change must restart the confirmation counter, not open on stale credit"
        );
        let completed = feed(&mut state, -39.0, 479, 479);
        assert!(
            completed.open,
            "480 frames under the new threshold must open the gate"
        );

        // The hang counter follows the same rule when leaving a closed state.
        let mut dropping = SquelchState::new(
            SquelchMode::Manual {
                threshold_dbfs: -60.0,
            },
            RATE_HZ,
        )
        .expect("a finite manual threshold must construct");
        feed(&mut dropping, -50.0, 480, 480);
        assert!(dropping.is_open(), "the fixture must open the gate");
        feed(&mut dropping, -70.0, 11_999, 11_999);
        assert!(dropping.is_open(), "11999 frames is short of the hang");
        dropping
            .set_mode(SquelchMode::Manual {
                threshold_dbfs: -90.0,
            })
            .expect("lowering the threshold is a valid mode");
        let after = feed(&mut dropping, -95.0, 1, 1);
        assert!(
            after.open,
            "a mode change must restart the hang counter, not close on stale credit"
        );
    }

    #[test]
    fn absurd_block_sizes_are_rejected_without_touching_state() {
        let mut state = auto_state();
        settle_noise_floor(&mut state);
        let clock_before = state.frames_elapsed_for_test();
        let floor_before = state.noise_floor_dbfs();

        let error = state
            .process(ChannelPowerSample {
                power_dbfs: -80.0,
                frames: MAX_BLOCK_FRAMES + 1,
            })
            .expect_err("a block longer than the documented limit must be rejected");
        assert!(
            error.to_string().contains("limit"),
            "the rejection must name the frame limit, got: {error}"
        );
        assert_eq!(
            state.frames_elapsed_for_test(),
            clock_before,
            "a rejected oversized block must not advance the clock"
        );
        assert_abs_diff_eq!(
            state.noise_floor_dbfs(),
            floor_before,
            epsilon = f32::EPSILON
        );
        assert!(
            state
                .process(ChannelPowerSample {
                    power_dbfs: -80.0,
                    frames: MAX_BLOCK_FRAMES,
                })
                .is_ok(),
            "a block exactly at the limit must be accepted"
        );
    }
}
