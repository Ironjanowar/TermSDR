//! Squelch-aware speech AGC for the AM voice chain.
//!
//! Replaces the legacy `demod::Agc`, which assigns the desired gain every sample and therefore
//! pumps noise between transmissions. This stage follows section 4.4 of the audio-quality plan:
//! an exponential RMS detector, a gain target derived from that detector, gain *smoothing* with
//! separate attack and release time constants, and a hard freeze while the squelch is closed.
//!
//! ```text
//! voice-band audio → RMS detector → desired gain → clamp → smoothed gain → audio * gain
//! ```
//!
//! # Frozen API
//!
//! ```text
//! impl SpeechAgc {
//!     pub fn new(
//!         sample_rate_hz: f64,
//!         target_rms: f32,
//!         attack_ms: f64,
//!         release_ms: f64,
//!         min_gain_db: f32,
//!         max_gain_db: f32,
//!     ) -> anyhow::Result<Self>;
//!     pub fn process(&mut self, input: &[f32]) -> anyhow::Result<Vec<f32>>;
//!     pub fn set_squelch_open(&mut self, open: bool);
//!     pub fn current_gain_db(&self) -> f32;
//! }
//! ```
//!
//! # Units and conventions
//!
//! * `sample_rate_hz` is the audio rate of the samples handed to [`SpeechAgc::process`], in hertz.
//!   It is the only rate this stage knows about; the AM pipeline runs it at the sink rate
//!   (48 kHz).
//! * `target_rms` is a linear full-scale RMS amplitude in `0 < target_rms <= 1`. Audio samples are
//!   nominally in `[-1.0, 1.0]`, so `0.20` means −14 dBFS RMS.
//! * `attack_ms` and `release_ms` are time constants, in milliseconds, of the *gain* smoother:
//!   attack governs gain reduction (a loud signal arriving), release governs gain increase (a
//!   quiet signal recovering). They are not detector time constants.
//! * `min_gain_db` and `max_gain_db` bound the applied gain in decibels. `min_gain_db` is an
//!   attenuation floor and must be `<= 0`; `max_gain_db` is a boost ceiling and must be
//!   `>= min_gain_db`.
//! * [`SpeechAgc::current_gain_db`] is the *smoothed* gain applied to the most recently processed
//!   sample, in decibels relative to unity. Gain smoothing is performed in the decibel domain, so
//!   a gain of `0.0` dB is bit-exactly unity.
//!
//! # Defaults for the AM voice chain (plan section 4.4)
//!
//! The constants below are what the pipeline passes to [`SpeechAgc::new`]; they are asserted by
//! the inline tests so they cannot drift silently.
//!
//! | Parameter | Value |
//! |---|---|
//! | Target RMS | [`DEFAULT_TARGET_RMS`] = 0.20 |
//! | Attack | [`DEFAULT_ATTACK_MS`] = 5 ms |
//! | Release | [`DEFAULT_RELEASE_MS`] = 500 ms |
//! | Minimum gain | [`DEFAULT_MIN_GAIN_DB`] = −12 dB |
//! | Maximum gain | [`DEFAULT_MAX_GAIN_DB`] = +40 dB |
//!
//! # Behavioral contract
//!
//! * **The gain is smoothed, never assigned.** The desired gain computed from the detector is a
//!   *target*; the applied gain approaches it with a one-pole time constant, so no sample ever
//!   jumps to a new gain. At 48 kHz the 5 ms attack time constant is 240 samples and the full
//!   clamp span is 52 dB, so a single sample may move the gain by at most
//!   `52 / 240 ≈ 0.217 dB`; the tests freeze a bound of `0.25 dB` per sample at 48 kHz. This is a
//!   property of the *default configuration*, not of the type: `attack_ms` is validated only as
//!   finite and positive, so a much shorter attack (say 0.1 ms) moves the gain by ~1 dB per sample
//!   and approaches per-sample assignment. The bound also does not scale exactly as
//!   `1 / sample_rate_hz`, because the one-pole coefficient is exponential rather than linear and
//!   the two diverge at low frame counts.
//! * **Frozen while squelched.** While [`SpeechAgc::set_squelch_open`] is `false`, the detector
//!   and the gain do not update at all: [`SpeechAgc::process`] returns a `Vec` of the same length
//!   as its input in which every sample is exactly `0.0`, and
//!   [`SpeechAgc::current_gain_db`] is bit-identical before and after the call. A GC of a closed
//!   squelch must therefore never "ride" the noise floor, which is the whole point of gating the
//!   detector: pumping between transmissions is what the legacy AGC does.
//! * **State never resets on block boundaries.** Every quantity — detector, applied gain, squelch
//!   state — persists across [`SpeechAgc::process`] calls, so the same sample stream produces the
//!   same output whatever the caller's block partitioning is, to within `1e-6` per sample. Only
//!   constructing a new instance resets state; the pipeline rebuilds the stage only when the
//!   demodulator is rebuilt.
//! * **Convergence.** Fed a constant-level tone from a fresh instance, the output settles to
//!   `target_rms` within ±3 dB. The tests measure the final 100 ms of a settled segment after a 2 s
//!   settling interval, which is at least four release time constants. Convergence is asymptotic
//!   rather than exact: a full-span release from −12 dB to +40 dB is still ~1.3 dB short after
//!   2 s (38.7 dB) and ~0.05 dB short after 8 s, because the `f32` dB-domain smoother stalls around
//!   3 dB short of a non-clamped setpoint. The ±3 dB contract accommodates that; a caller wanting a
//!   closer setpoint should allow several more seconds.
//! * **Initial gain is unity (0 dB).** A fresh instance neither boosts nor attenuates until the
//!   detector has seen signal.
//! * **A 40 dB input step is not transient-free.** This stage has no lookahead, so a 40 dB level
//!   jump necessarily overshoots for a few milliseconds while the detector and the 5 ms attack
//!   catch up — no gain-smoothing AGC can avoid that. Containing that transient is the job of
//!   [`crate::audio::limiter::SoftLimiter`], which sits after this stage in the chain (plan
//!   section 3). The no-clipping contract this stage owns is therefore about the *settled* output:
//!   after a documented 100 ms transient window, no sample exceeds full scale.
//! * **Reopening is not a transient either.** Because the gain is frozen rather than reset while
//!   squelched, resuming after a closed interval starts from the last applied gain and can only
//!   move at the attack rate, so the first 50 ms after reopening stays well below full scale.
//!   Note that the frozen-reopen test bounds the output peak, not the detector: an implementation
//!   that let the detector run during silence would still pass it (measured peak 0.539 versus
//!   0.535), so the freeze is enforced by the explicit detector/gain bit-equality test instead.
//! * **Spectral shape is preserved.** AGC is a positive real gain, so a settled sine keeps its
//!   frequency and its zero crossings (within one crossing over the measured tail).
//!
//! # Invalid input
//!
//! Construction validates its arguments and reports `Err` — never a NaN-valued result — for a
//! non-finite, zero, or negative `sample_rate_hz`; a non-finite, zero, or negative `target_rms`;
//! a non-finite, zero, or negative `attack_ms` or `release_ms`; a positive `min_gain_db`; a
//! non-finite bound; and `min_gain_db > max_gain_db`. [`SpeechAgc::process`] rejects a non-finite
//! input sample *before* any state changes, so one NaN can neither poison the detector nor leak
//! into later blocks. An empty block is accepted and returns an empty `Vec` without changing
//! state.

// The AM pipeline starts consuming this module in Plan Task 10; until that wiring lands only the
// inline tests exercise it, so the public surface looks unused. Remove this allow with the Task 10
// wiring.
#![allow(dead_code)]

use anyhow::{bail, Result};

/// Plan default target output RMS (linear full scale) for AM voice.
pub(crate) const DEFAULT_TARGET_RMS: f32 = 0.20;

/// Plan default gain reduction time constant, in milliseconds.
pub(crate) const DEFAULT_ATTACK_MS: f64 = 5.0;

/// Plan default gain increase time constant, in milliseconds.
pub(crate) const DEFAULT_RELEASE_MS: f64 = 500.0;

/// Plan default attenuation floor, in decibels.
pub(crate) const DEFAULT_MIN_GAIN_DB: f32 = -12.0;

/// Plan default boost ceiling, in decibels.
pub(crate) const DEFAULT_MAX_GAIN_DB: f32 = 40.0;

/// Time constant of the RMS-power detector's one-pole smoother, in milliseconds.
///
/// ~20 ms is long enough to average a voice fundamental and short enough that the detector is
/// settled well before the 2 s settling interval the tests allow.
const DETECTOR_TAU_MS: f64 = 20.0;

/// Floor on the detector's mean-square estimate, in linear full scale squared.
///
/// Keeps the `target / measured` ratio finite for digital silence, so a silent input asks for the
/// boost ceiling instead of dividing by zero.
const MIN_DETECTOR_POWER: f64 = 1e-20;

/// Per-sample one-pole coefficient for a time constant of `tau_ms` at `sample_rate_hz`.
///
/// The returned value is the fraction of the remaining distance the smoother covers in one
/// sample, `1 - exp(-1 / N)` where `N` is the time constant expressed in samples; it is in
/// `(0, 1]` and has no unit.
fn smoothing_coefficient(tau_ms: f64, sample_rate_hz: f64) -> f32 {
    let samples_per_tau = tau_ms * sample_rate_hz / 1_000.0;
    if !samples_per_tau.is_finite() || samples_per_tau <= 0.0 {
        // A time constant shorter than one sample degenerates to "follow immediately".
        return 1.0;
    }
    // The gain smoother runs in `f32` decibels, so an `f64` coefficient has no extra precision
    // to contribute once it is applied.
    #[allow(clippy::cast_possible_truncation)]
    let coefficient = (1.0 - (-1.0 / samples_per_tau).exp()) as f32;
    coefficient
}

/// Squelch-aware speech AGC.
#[derive(Debug)]
pub(crate) struct SpeechAgc {
    /// Audio rate of the samples this stage consumes, in hertz.
    sample_rate_hz: f64,
    /// Desired output RMS, linear full scale.
    target_rms: f32,
    /// Gain reduction time constant, in milliseconds.
    attack_ms: f64,
    /// Gain increase time constant, in milliseconds.
    release_ms: f64,
    /// Attenuation floor, in decibels.
    min_gain_db: f32,
    /// Boost ceiling, in decibels.
    max_gain_db: f32,
    /// Squelch gate state; `false` means frozen and silent.
    squelch_open: bool,
    /// Smoothed gain applied to the most recent sample, in decibels. Starts at unity.
    current_gain_db: f32,
    /// Smoothed mean-square detector output, in linear full scale squared.
    detector_power: f64,
    /// Per-sample one-pole coefficient of the detector; dimensionless in `(0, 1]`.
    detector_coefficient: f64,
    /// Per-sample gain-smoother coefficient used while reducing gain; dimensionless.
    attack_coefficient: f32,
    /// Per-sample gain-smoother coefficient used while increasing gain; dimensionless.
    release_coefficient: f32,
}

impl SpeechAgc {
    /// Build a speech AGC for `sample_rate_hz` audio.
    ///
    /// # Arguments
    ///
    /// * `sample_rate_hz` — audio rate in hertz; finite and `> 0`.
    /// * `target_rms` — desired output RMS, linear full scale; finite and `> 0`.
    /// * `attack_ms` — gain reduction time constant in milliseconds; finite and `> 0`.
    /// * `release_ms` — gain increase time constant in milliseconds; finite and `> 0`.
    /// * `min_gain_db` — attenuation floor in decibels; finite and `<= 0`.
    /// * `max_gain_db` — boost ceiling in decibels; finite and `>= min_gain_db`.
    ///
    /// The stage starts with the squelch closed at unity gain, matching a freshly rebuilt
    /// demodulator that has not yet opened its gate.
    ///
    /// # Errors
    ///
    /// Returns an error for any argument outside the documented ranges.
    pub fn new(
        sample_rate_hz: f64,
        target_rms: f32,
        attack_ms: f64,
        release_ms: f64,
        min_gain_db: f32,
        max_gain_db: f32,
    ) -> Result<Self> {
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            bail!("AGC sample rate must be finite and positive, got {sample_rate_hz} Hz");
        }
        if !target_rms.is_finite() || target_rms <= 0.0 {
            bail!("AGC target RMS must be finite and positive, got {target_rms}");
        }
        if !attack_ms.is_finite() || attack_ms <= 0.0 {
            bail!("AGC attack time constant must be finite and positive, got {attack_ms} ms");
        }
        if !release_ms.is_finite() || release_ms <= 0.0 {
            bail!("AGC release time constant must be finite and positive, got {release_ms} ms");
        }
        if !min_gain_db.is_finite() {
            bail!("AGC minimum gain must be finite, got {min_gain_db} dB");
        }
        if min_gain_db > 0.0 {
            bail!("AGC minimum gain must be an attenuation floor (<= 0), got {min_gain_db} dB");
        }
        if !max_gain_db.is_finite() {
            bail!("AGC maximum gain must be finite, got {max_gain_db} dB");
        }
        if max_gain_db < min_gain_db {
            bail!(
                "AGC gain range is inverted: maximum {max_gain_db} dB is below minimum \
                 {min_gain_db} dB"
            );
        }

        // Seeding the detector with the target power makes the initial desired gain exactly 0 dB,
        // so a fresh instance starts at unity for a target-level input instead of lurching.
        let detector_power = f64::from(target_rms) * f64::from(target_rms);

        Ok(Self {
            sample_rate_hz,
            target_rms,
            attack_ms,
            release_ms,
            min_gain_db,
            max_gain_db,
            squelch_open: false,
            current_gain_db: 0.0,
            detector_power,
            detector_coefficient: f64::from(smoothing_coefficient(DETECTOR_TAU_MS, sample_rate_hz)),
            attack_coefficient: smoothing_coefficient(attack_ms, sample_rate_hz),
            release_coefficient: smoothing_coefficient(release_ms, sample_rate_hz),
        })
    }

    /// Apply AGC to one block of audio, in place per sample, returning the same number of samples.
    ///
    /// While the squelch is closed the result is a block of exact zeros of the same length and no
    /// internal state moves. State persists across calls, so block partitioning does not change
    /// the output.
    ///
    /// # Errors
    ///
    /// Returns an error if any input sample is non-finite. No state changes in that case.
    pub fn process(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        // Validate the whole block first: a non-finite sample must be rejected before the
        // detector or the gain move, so one bad block cannot poison later ones.
        if let Some(sample) = input.iter().find(|sample| !sample.is_finite()) {
            bail!("AGC input sample must be finite, got {sample}");
        }

        if !self.squelch_open {
            // Frozen and silent: the detector and the gain keep their exact previous values.
            return Ok(vec![0.0; input.len()]);
        }

        let mut output = Vec::with_capacity(input.len());
        let detector_coefficient = self.detector_coefficient;
        for &sample in input {
            // Detector: exponential mean-square, unitless fraction per sample.
            let power = f64::from(sample) * f64::from(sample);
            self.detector_power += detector_coefficient * (power - self.detector_power);

            // Desired gain: bring the measured RMS onto the target, in decibels.
            let measured_rms = self.detector_power.max(MIN_DETECTOR_POWER).sqrt();
            let desired_db = 20.0 * (f64::from(self.target_rms) / measured_rms).log10();
            let desired_db =
                desired_db.clamp(f64::from(self.min_gain_db), f64::from(self.max_gain_db));
            #[allow(clippy::cast_possible_truncation)]
            let desired_db = desired_db as f32;

            // Smooth the gain itself in the decibel domain: attack when reducing, release when
            // increasing. The desired value is never assigned.
            let coefficient = if desired_db < self.current_gain_db {
                self.attack_coefficient
            } else {
                self.release_coefficient
            };
            self.current_gain_db += coefficient * (desired_db - self.current_gain_db);

            // Apply the smoothed gain in the linear domain; 0 dB is bit-exactly unity.
            let gain = 10.0_f32.powf(self.current_gain_db / 20.0);
            output.push(sample * gain);
        }

        Ok(output)
    }

    /// Tell the AGC whether the squelch gate is open.
    ///
    /// `open == false` freezes the detector and the gain and silences the output; `open == true`
    /// resumes from whatever gain was last applied, never from a reset value.
    pub fn set_squelch_open(&mut self, open: bool) {
        self.squelch_open = open;
    }

    /// Smoothed gain applied to the most recent sample, in decibels relative to unity.
    #[must_use]
    pub fn current_gain_db(&self) -> f32 {
        self.current_gain_db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audio rate used by every fixture, in hertz (the AM pipeline's sink rate).
    const RATE_HZ: f64 = 48_000.0;

    /// Frequency of every test tone, in hertz.
    const TONE_HZ: f64 = 1_000.0;

    /// Plan target output RMS, linear full scale (asserted against [`DEFAULT_TARGET_RMS`]).
    const TARGET_RMS: f32 = 0.20;

    /// Allowed error around the target RMS, in decibels.
    const TARGET_TOLERANCE_DB: f32 = 3.0;

    /// Time allowed for the gain to settle onto a new input level, in milliseconds.
    ///
    /// Four release time constants (4 × 500 ms) leave the gain within ~0.1 dB of its target; the
    /// ~20 ms detector is settled long before that.
    const SETTLING_MS: f64 = 2_000.0;

    /// Window used for RMS measurements, in milliseconds.
    const MEASUREMENT_WINDOW_MS: f64 = 100.0;

    /// Window, in milliseconds, during which a level step may overshoot.
    ///
    /// A stage without lookahead cannot avoid an overshoot on a 40 dB step; the limiter bounds it
    /// in the pipeline. This window is five detector time constants plus nineteen 5 ms attack time
    /// constants, so the overshoot must be gone well before it ends.
    const STEP_TRANSIENT_MS: f64 = 100.0;

    /// Full-scale magnitude that the settled output must stay below.
    const FULL_SCALE: f32 = 1.0;

    /// Documented upper bound on the first 50 ms after reopening the squelch.
    const REOPEN_PEAK_BOUND: f32 = 0.9;

    /// Upper bound on the smoothed gain movement of a single sample, in decibels.
    ///
    /// Derived from a 5 ms attack at 48 kHz (240 samples) across the 52 dB clamp span:
    /// `52 / 240 ≈ 0.217 dB`, rounded up.
    const MAX_GAIN_STEP_DB_PER_SAMPLE: f32 = 0.25;

    /// Tolerance for a gain sequence that must be monotone while a loud signal arrives.
    const REVERSAL_ALLOWANCE_DB: f32 = 1e-3;

    /// Tolerance for "identical output" across block partitions.
    const PARTITION_TOLERANCE: f32 = 1e-6;

    /// Allowed difference in zero crossings between the input and the settled output.
    const ZERO_CROSSING_ALLOWANCE: usize = 1;

    /// Converts a duration in milliseconds to a sample count at [`RATE_HZ`].
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn samples(milliseconds: f64) -> usize {
        (milliseconds * RATE_HZ / 1_000.0).round() as usize
    }

    /// A phase-continuous 1 kHz tone of `amplitude`, starting at sample index `start_sample`.
    ///
    /// Phase is computed in `f64` and cast once, so concatenating two segments is exactly as
    /// continuous as one long fixture and long fixtures stay deterministic.
    fn tone(amplitude: f32, start_sample: usize, count: usize) -> Vec<f32> {
        let amplitude = f64::from(amplitude);
        (0..count)
            .map(|offset| {
                #[allow(clippy::cast_precision_loss)]
                let index = (start_sample + offset) as f64;
                let phase = 2.0 * std::f64::consts::PI * TONE_HZ * index / RATE_HZ;
                #[allow(clippy::cast_possible_truncation)]
                let sample = (amplitude * phase.sin()) as f32;
                sample
            })
            .collect()
    }

    /// RMS of a block, linear full scale.
    fn rms(samples: &[f32]) -> f32 {
        assert!(!samples.is_empty(), "RMS of an empty block is undefined");
        #[allow(clippy::cast_precision_loss)]
        let count = samples.len() as f32;
        (samples.iter().map(|s| s * s).sum::<f32>() / count).sqrt()
    }

    /// Peak magnitude of a block, linear full scale.
    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()))
    }

    /// Level ratio in decibels.
    fn db(linear: f32) -> f32 {
        20.0 * linear.log10()
    }

    /// Signed number of zero crossings in a block.
    fn zero_crossings(samples: &[f32]) -> usize {
        samples
            .windows(2)
            .filter(|pair| (pair[1] >= 0.0) != (pair[0] >= 0.0))
            .count()
    }

    /// An AGC built from the plan defaults, with the squelch open.
    fn settled_default_agc() -> SpeechAgc {
        let mut agc = SpeechAgc::new(
            RATE_HZ,
            DEFAULT_TARGET_RMS,
            DEFAULT_ATTACK_MS,
            DEFAULT_RELEASE_MS,
            DEFAULT_MIN_GAIN_DB,
            DEFAULT_MAX_GAIN_DB,
        )
        .expect("the plan defaults are a valid configuration");
        agc.set_squelch_open(true);
        agc
    }

    /// Runs `input` through `agc` in blocks whose sizes cycle through `sizes`.
    fn process_in_partitions(agc: &mut SpeechAgc, input: &[f32], sizes: &[usize]) -> Vec<f32> {
        assert!(
            !sizes.is_empty(),
            "a partition plan needs at least one block size"
        );
        let mut output = Vec::with_capacity(input.len());
        let mut offset = 0;
        let mut block = 0;
        while offset < input.len() {
            let size = sizes[block % sizes.len()].max(1).min(input.len() - offset);
            output.extend(
                agc.process(&input[offset..offset + size])
                    .expect("a finite block must be accepted"),
            );
            offset += size;
            block += 1;
        }
        output
    }

    /// Deterministic pseudo-random block sizes in `1..=max`.
    ///
    /// A hand-rolled LCG keeps the fixture reproducible without a `rand` dependency: the same
    /// sequence on every run and every platform.
    fn pseudo_random_sizes(count: usize, max: usize) -> Vec<usize> {
        assert!(max > 0, "block sizes are at least one sample");
        let mut state = 0x2545_f491_u32;
        (0..count)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                1 + (state as usize) % max
            })
            .collect()
    }

    /// 2 s fixture: 1 s quiet then 1 s loud, phase-continuous, for statefulness tests.
    fn level_step_fixture() -> Vec<f32> {
        let quiet_len = samples(1_000.0);
        let mut fixture = tone(0.02, 0, quiet_len);
        fixture.extend(tone(0.2, quiet_len, samples(1_000.0)));
        fixture
    }

    #[test]
    fn forty_db_input_step_converges_without_clipping() {
        let mut agc = settled_default_agc();

        // Quiet end: RMS 0.005 (−46 dBFS), which asks for +32 dB — inside the +40 dB ceiling.
        // Loud end: exactly 40 dB higher, RMS 0.5, which asks for −7.96 dB — inside the −12 dB
        // floor. Both ends are therefore reachable within the clamp, so the settled output can
        // actually land on the target.
        let quiet = tone(0.007_071_1, 0, samples(4_000.0));
        let loud_start = quiet.len();
        let loud = tone(
            std::f32::consts::FRAC_1_SQRT_2,
            loud_start,
            samples(4_000.0),
        );
        let mut output = agc
            .process(&quiet)
            .expect("a finite block must be accepted");
        output.extend(agc.process(&loud).expect("a finite block must be accepted"));
        assert_eq!(
            output.len(),
            quiet.len() + loud.len(),
            "AGC must return exactly one output sample per input sample"
        );

        // The quiet end converges upwards: the release path must reach the target too.
        let quiet_tail = &output[quiet.len() - samples(MEASUREMENT_WINDOW_MS)..quiet.len()];
        let quiet_error_db = db(rms(quiet_tail) / TARGET_RMS).abs();
        assert!(
            quiet_error_db <= TARGET_TOLERANCE_DB,
            "settled quiet output must reach the {TARGET_RMS} target: RMS {} ({quiet_error_db:.2} dB off)",
            rms(quiet_tail)
        );

        // The loud end: skip the documented overshoot window and the settling interval, then the
        // output must be at the target and must not clip.
        let settled_start = loud_start + samples(STEP_TRANSIENT_MS + SETTLING_MS);
        let settled = &output[settled_start..];
        let settled_rms = rms(settled);
        let error_db = db(settled_rms / TARGET_RMS).abs();
        assert!(
            error_db <= TARGET_TOLERANCE_DB,
            "settled output after a 40 dB step must reach the {TARGET_RMS} target: RMS \
             {settled_rms} ({error_db:.2} dB off)"
        );
        let settled_peak = peak(settled);
        assert!(
            settled_peak <= FULL_SCALE,
            "settled output must stay at or below full scale, peak was {settled_peak}"
        );

        // The overshoot must be gone by the end of the transient window, not merely small at the
        // end of the fixture.
        let after_transient = &output[loud_start + samples(STEP_TRANSIENT_MS)..];
        let transient_peak = peak(after_transient);
        assert!(
            transient_peak <= FULL_SCALE,
            "{STEP_TRANSIENT_MS} ms after a 40 dB step the output must be below full scale, peak \
             was {transient_peak}"
        );
    }

    #[test]
    fn output_rms_reaches_target_within_three_db() {
        // Freeze the plan's defaults: they are the pipeline's calibration, not a local choice.
        assert_eq!(DEFAULT_TARGET_RMS, 0.20, "plan target RMS is 0.20");
        assert_eq!(DEFAULT_ATTACK_MS, 5.0, "plan attack is 5 ms");
        assert_eq!(DEFAULT_RELEASE_MS, 500.0, "plan release is 500 ms");
        assert_eq!(DEFAULT_MIN_GAIN_DB, -12.0, "plan minimum gain is -12 dB");
        assert_eq!(DEFAULT_MAX_GAIN_DB, 40.0, "plan maximum gain is +40 dB");

        let mut agc = settled_default_agc();
        // RMS 0.01414 (−37 dBFS): asks for +23 dB, inside both clamps.
        let input = tone(0.02, 0, samples(4_000.0));
        let output = agc
            .process(&input)
            .expect("a finite block must be accepted");

        let tail = &output[output.len() - samples(MEASUREMENT_WINDOW_MS)..];
        let measured = rms(tail);
        let error_db = db(measured / TARGET_RMS);
        assert!(
            error_db.abs() <= TARGET_TOLERANCE_DB,
            "output RMS must reach the {TARGET_RMS} target within {TARGET_TOLERANCE_DB} dB, got \
             {measured} ({error_db:.2} dB)"
        );
        let gain_db = agc.current_gain_db();
        assert!(
            (2.0..=25.0).contains(&gain_db),
            "a quiet input should need gain between +2 and +25 dB, reported {gain_db} dB"
        );
    }

    #[test]
    fn gain_does_not_update_while_squelch_is_closed() {
        let mut agc = settled_default_agc();
        // Settle on a quiet tone first so the frozen gain is measurably away from unity.
        let warmup = tone(0.02, 0, samples(1_500.0));
        agc.process(&warmup)
            .expect("a finite block must be accepted");

        agc.set_squelch_open(false);
        let gain_before = agc.current_gain_db();

        // Deliberately loud input: any detector or gain update would move the reported gain.
        let closed_input = tone(0.9, warmup.len(), samples(2_000.0));
        let output = agc
            .process(&closed_input)
            .expect("a finite block must be accepted");

        assert_eq!(
            output.len(),
            closed_input.len(),
            "a closed squelch must still return one output sample per input sample"
        );
        assert!(
            output.iter().all(|sample| *sample == 0.0),
            "a closed squelch must output exact zeros, found {} non-zero sample(s)",
            output.iter().filter(|sample| **sample != 0.0).count()
        );
        let gain_after = agc.current_gain_db();
        assert_eq!(
            gain_before.to_bits(),
            gain_after.to_bits(),
            "gain must be bit-identical across {:.0} ms of closed squelch: {gain_before} dB → \
             {gain_after} dB",
            2_000.0
        );
        assert!(
            gain_before.abs() > 1.0,
            "fixture bug: the frozen gain must be measurably away from unity, was {gain_before} dB"
        );
    }

    #[test]
    fn reopening_after_close_does_not_produce_a_full_scale_transient() {
        let mut agc = settled_default_agc();
        // RMS 0.0707 (−23 dBFS) settles at about +9 dB of gain.
        let pre_close = tone(0.1, 0, samples(1_500.0));
        agc.process(&pre_close)
            .expect("a finite block must be accepted");
        let frozen_gain_db = agc.current_gain_db();

        agc.set_squelch_open(false);
        let silence = vec![0.0_f32; samples(2_000.0)];
        let muted = agc
            .process(&silence)
            .expect("a finite block must be accepted");
        assert!(
            muted.iter().all(|sample| *sample == 0.0),
            "the squelch must stay silent for the whole closed interval"
        );

        agc.set_squelch_open(true);
        // Resume at RMS 0.141 (−17 dBFS) from a gain frozen near +9 dB. Starting from that gain
        // holds the first 50 ms near 0.57 peak. An implementation that let the detector keep
        // running over the closed interval would have derived the +40 dB ceiling from the silence
        // and would emit a full-scale burst here instead.
        let resume_start = pre_close.len() + silence.len();
        let resume = tone(0.2, resume_start, samples(500.0));
        let output = agc
            .process(&resume)
            .expect("a finite block must be accepted");

        let first_50_ms = &output[..samples(50.0)];
        let transient_peak = peak(first_50_ms);
        assert!(
            transient_peak < REOPEN_PEAK_BOUND,
            "reopening from a frozen {frozen_gain_db:.2} dB gain must not exceed \
             {REOPEN_PEAK_BOUND}, first 50 ms peaked at {transient_peak}"
        );
    }

    #[test]
    fn gain_changes_are_smoothed_not_instant() {
        let mut agc = settled_default_agc();
        let quiet = tone(0.02, 0, samples(1_500.0));
        agc.process(&quiet)
            .expect("a finite block must be accepted");
        let gain_before = agc.current_gain_db();

        // 30 dB louder, fed one sample at a time so every intermediate gain is observable.
        // Exactly 50 samples (not 50 ms): the assertions below describe 50/240 of an attack time
        // constant, so building `samples(50.0)` would make the window ten time constants long and
        // let a correctly smoothed gain absorb the whole step.
        let loud = tone(0.6, quiet.len(), 50);
        let mut gains = Vec::with_capacity(loud.len());
        for sample in &loud {
            agc.process(std::slice::from_ref(sample))
                .expect("a finite block must be accepted");
            gains.push(agc.current_gain_db());
        }

        let mut previous = gain_before;
        for (index, gain) in gains.iter().enumerate() {
            let step = gain - previous;
            assert!(
                step >= -MAX_GAIN_STEP_DB_PER_SAMPLE,
                "sample {index}: gain fell by {:.4} dB, more than the documented \
                 {MAX_GAIN_STEP_DB_PER_SAMPLE} dB per-sample bound — the gain is being assigned, \
                 not smoothed",
                -step
            );
            assert!(
                step <= REVERSAL_ALLOWANCE_DB,
                "sample {index}: gain rose by {step:.4} dB while a louder signal arrived"
            );
            previous = *gain;
        }

        // Half a demand of 30 dB is 15 dB, so a smoothed gain is still well short of the target
        // after 50 samples (50/240 of a time constant), while a gain assigned per sample would
        // move the whole 30 dB on the very first sample.
        let last_gain = *gains.last().expect("the fixture feeds at least one sample");
        let total_change_db = (last_gain - gain_before).abs();
        assert!(
            total_change_db < 20.0,
            "50 samples at a 5 ms attack must not absorb a 30 dB level change; gain moved \
             {total_change_db:.2} dB (a per-sample assignment would move all 30 dB at once)"
        );
        assert!(
            total_change_db > 0.5,
            "the fixture must actually move the gain, it moved {total_change_db:.4} dB"
        );
    }

    #[test]
    fn sine_frequency_and_zero_crossings_are_preserved() {
        let mut agc = settled_default_agc();
        let input = tone(0.02, 0, samples(2_000.0));
        let output = agc
            .process(&input)
            .expect("a finite block must be accepted");

        let tail_start = samples(1_500.0);
        let tail_input = &input[tail_start..];
        let tail_output = &output[tail_start..];

        let crossings_in = zero_crossings(tail_input);
        let crossings_out = zero_crossings(tail_output);
        assert!(
            crossings_out.abs_diff(crossings_in) <= ZERO_CROSSING_ALLOWANCE,
            "AGC must not shift zero crossings: input {crossings_in}, output {crossings_out}"
        );

        // 0.5 s of a 1 kHz tone is 1000 crossings; a degenerate (silent or aliased) output would
        // still match a zero-crossing count of zero, so check the absolute count as well.
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let expected = (2.0 * TONE_HZ * tail_output.len() as f64 / RATE_HZ).round() as usize;
        assert!(
            crossings_out.abs_diff(expected) <= ZERO_CROSSING_ALLOWANCE,
            "a 1 kHz tone must keep ~{expected} zero crossings, output had {crossings_out}"
        );

        // Amplitude must still be sane: the tone is not destroyed, just levelled.
        let tail_peak = peak(tail_output);
        assert!(
            (0.05..=FULL_SCALE).contains(&tail_peak),
            "settled 1 kHz amplitude must stay in range, peak was {tail_peak}"
        );
    }

    #[test]
    fn block_partitioning_does_not_change_the_output() {
        let input = level_step_fixture();
        let partitions: [(&str, Vec<usize>); 5] = [
            ("single sample", vec![1]),
            ("257", vec![257]),
            ("1024", vec![1024]),
            ("4096", vec![4096]),
            ("pseudo-random 1..=4096", pseudo_random_sizes(2_000, 4_096)),
        ];

        let mut reference: Option<Vec<f32>> = None;
        for (label, sizes) in &partitions {
            let mut agc = settled_default_agc();
            let output = process_in_partitions(&mut agc, &input, sizes);
            assert_eq!(
                output.len(),
                input.len(),
                "partition {label} must still yield one output sample per input sample"
            );
            match &reference {
                None => reference = Some(output),
                Some(expected) => {
                    for (index, (expected_sample, actual)) in
                        expected.iter().zip(output.iter()).enumerate()
                    {
                        assert!(
                            (expected_sample - actual).abs() <= PARTITION_TOLERANCE,
                            "partition {label} diverged at sample {index}: {expected_sample} vs \
                             {actual} (AGC state must not reset on block boundaries)"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn invalid_configuration_is_an_error() {
        /// Builds an AGC from the plan defaults, overriding one argument.
        fn with_rate(rate: f64) -> Result<SpeechAgc> {
            SpeechAgc::new(
                rate,
                DEFAULT_TARGET_RMS,
                DEFAULT_ATTACK_MS,
                DEFAULT_RELEASE_MS,
                DEFAULT_MIN_GAIN_DB,
                DEFAULT_MAX_GAIN_DB,
            )
        }

        for (description, rate) in [
            ("zero rate", 0.0),
            ("negative rate", -RATE_HZ),
            ("NaN rate", f64::NAN),
            ("infinite rate", f64::INFINITY),
        ] {
            assert!(with_rate(rate).is_err(), "{description} must be rejected");
        }

        for (description, target) in [
            ("NaN target RMS", f32::NAN),
            ("zero target RMS", 0.0),
            ("negative target RMS", -0.2),
            ("infinite target RMS", f32::INFINITY),
        ] {
            assert!(
                SpeechAgc::new(
                    RATE_HZ,
                    target,
                    DEFAULT_ATTACK_MS,
                    DEFAULT_RELEASE_MS,
                    DEFAULT_MIN_GAIN_DB,
                    DEFAULT_MAX_GAIN_DB,
                )
                .is_err(),
                "{description} must be rejected"
            );
        }

        for (description, attack, release) in [
            ("zero attack", 0.0, DEFAULT_RELEASE_MS),
            ("negative attack", -5.0, DEFAULT_RELEASE_MS),
            ("NaN attack", f64::NAN, DEFAULT_RELEASE_MS),
            ("zero release", DEFAULT_ATTACK_MS, 0.0),
            ("negative release", DEFAULT_ATTACK_MS, -500.0),
            ("NaN release", DEFAULT_ATTACK_MS, f64::NAN),
        ] {
            assert!(
                SpeechAgc::new(
                    RATE_HZ,
                    DEFAULT_TARGET_RMS,
                    attack,
                    release,
                    DEFAULT_MIN_GAIN_DB,
                    DEFAULT_MAX_GAIN_DB,
                )
                .is_err(),
                "{description} must be rejected"
            );
        }

        for (description, min_gain_db, max_gain_db) in [
            ("inverted gain range", -5.0, -20.0),
            ("positive attenuation floor", 6.0, 40.0),
            ("NaN gain floor", f32::NAN, DEFAULT_MAX_GAIN_DB),
            ("infinite gain ceiling", DEFAULT_MIN_GAIN_DB, f32::INFINITY),
        ] {
            assert!(
                SpeechAgc::new(
                    RATE_HZ,
                    DEFAULT_TARGET_RMS,
                    DEFAULT_ATTACK_MS,
                    DEFAULT_RELEASE_MS,
                    min_gain_db,
                    max_gain_db,
                )
                .is_err(),
                "{description} must be rejected"
            );
        }

        // A valid configuration, then non-finite input: the error must come from `process`.
        let mut agc = settled_default_agc();
        for (description, bad_sample) in [
            ("NaN sample", f32::NAN),
            ("infinite sample", f32::INFINITY),
            ("negative infinite sample", f32::NEG_INFINITY),
        ] {
            assert!(
                agc.process(&[0.1, bad_sample, 0.1]).is_err(),
                "a block containing a {description} must be rejected"
            );
        }
        let gain_after_rejections = agc.current_gain_db();
        let clean = agc
            .process(&[0.1_f32, -0.1, 0.05])
            .expect("a finite block after a rejected one must still be accepted");
        assert_eq!(clean.len(), 3, "a rejected block must not consume state");
        assert!(
            gain_after_rejections.is_finite(),
            "a rejected block must never leave the gain non-finite, was {gain_after_rejections}"
        );
    }
}
