//! Unity-slope soft limiter that bounds AM voice audio to full scale.
//!
//! Last stage before volume (plan section 3): the speech AGC has no lookahead, so a fast level
//! jump can momentarily overshoot full scale. This limiter is transparent to normal speech and
//! only bends the peaks, which keeps the output inside `[-1.0, 1.0]` without clipping artifacts.
//!
//! ```text
//! |x| <= KNEE:  y = x                                (unity slope, bit-exact)
//! |x| >  KNEE:  y = sign(x) * (KNEE + WIDTH * tanh((|x| - KNEE) / WIDTH))
//! ```
//!
//! # Frozen API
//!
//! ```text
//! pub(crate) fn soft_limit(sample: f32) -> f32;
//!
//! impl SoftLimiter {
//!     pub fn new() -> Self;
//!     pub fn process(&mut self, input: &[f32]) -> Vec<f32>;
//!     pub fn reduction_db(&self) -> f32;
//! }
//! ```
//!
//! # Units and conventions (plan section 4.5)
//!
//! * Samples are linear full scale; nominal audio is in `[-1.0, 1.0]`.
//! * [`KNEE`] is 0.9: below that magnitude the limiter is bit-exactly the identity, so normal
//!   speech is untouched.
//! * [`WIDTH`] is 0.1: the soft region spans one tenth of full scale above the knee and the curve
//!   asymptotes to `KNEE + WIDTH = 1.0`. Output is bounded to `|y| <= 1.0`. In `f32` the bound is
//!   reached, not merely approached: `tanh` saturates, so `|y|` is exactly `1.0` from
//!   `|x| >= 1.7461607` upwards (`0.976159` at `|x| = 1.0`). The plan's contract is
//!   `[-1.0, 1.0]` within floating-point tolerance, which this satisfies; it is not strictly
//!   below full scale.
//! * At the knee the curve is `C¹`-continuous by construction, since `tanh(0) == 0` makes both
//!   branches meet at `KNEE`, and the derivative of the upper branch, `sech²(0) == 1`, matches the
//!   unit slope of the lower branch.
//! * [`SoftLimiter::reduction_db`] is the largest gain reduction the limiter applied since it was
//!   constructed (or since the last [`SoftLimiter::reset_reduction`]), in decibels, reported as a
//!   non-negative magnitude: `0.0` means "no sample above the knee". It is the quantity the
//!   pipeline publishes as `limiter_reduction_db` telemetry.
//! * A `SoftLimiter` is stateless in the signal path — `process` is `soft_limit` mapped over the
//!   block — and stateful only in the reduction meter, so output is invariant to block
//!   partitioning by construction.
//!
//! # Invalid input
//!
//! `soft_limit` is total: NaN propagates as NaN and an infinity saturates to ±1.0, but the
//! pipeline rejects non-finite audio before this stage, so neither case is a supported input. No
//! function here allocates per sample, panics, or unwraps.

// The AM pipeline starts consuming this module in Plan Task 10; until that wiring lands only the
// inline tests exercise it, so the public surface looks unused. Remove this allow with the Task 10
// wiring.
#![allow(dead_code)]

/// Magnitude at which the limiter stops being the identity, in linear full scale.
pub(crate) const KNEE: f32 = 0.9;

/// Width of the soft region above the knee, in linear full scale.
///
/// Together with [`KNEE`] it sets the asymptote `KNEE + WIDTH == 1.0`, which is why the output is
/// bounded to full scale.
pub(crate) const WIDTH: f32 = 0.1;

/// Apply the unity-slope soft limiter to one sample, in linear full scale.
///
/// Returns `sample` unchanged, bit for bit, when `|sample| <= KNEE`; above that it compresses
/// towards ±1.0 with a `tanh` shoulder.
#[must_use]
pub(crate) fn soft_limit(sample: f32) -> f32 {
    let magnitude = sample.abs();
    if magnitude <= KNEE {
        // Bit-exact identity: normal speech must pass through untouched.
        return sample;
    }
    // Above the knee the shoulder asymptotes to KNEE + WIDTH == 1.0, so the magnitude stays
    // below full scale for every finite input (`tanh` saturates long before `f32::MAX`).
    let excess = (magnitude - KNEE) / WIDTH;
    let shoulder = KNEE + WIDTH * excess.tanh();
    // `copysign` reproduces the input's sign without a branch on zero.
    shoulder.copysign(sample)
}

/// Soft limiter with a peak-reduction meter.
///
/// Holds the worst-case gain reduction applied so far, in decibels, for telemetry.
#[derive(Debug, Default)]
pub(crate) struct SoftLimiter {
    /// Largest gain reduction applied since construction or the last reset, in decibels.
    reduction_db: f32,
}

impl SoftLimiter {
    /// Build a limiter with a reduction meter at 0 dB.
    #[must_use]
    pub fn new() -> Self {
        Self { reduction_db: 0.0 }
    }

    /// Limit one block, returning exactly `input.len()` samples.
    ///
    /// Updates the reduction meter with the largest reduction the block required.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(input.len());
        let mut worst_db = 0.0_f32;
        for &sample in input {
            let limited = soft_limit(sample);
            // Reduction in decibels is only defined where the limiter bent the sample; computing
            // it in `f64` keeps the ratio finite even for `f32::MAX`.
            let input_magnitude = f64::from(sample.abs());
            if input_magnitude > f64::from(KNEE) && limited != 0.0 {
                let output_magnitude = f64::from(limited.abs());
                if output_magnitude > 0.0 {
                    #[allow(clippy::cast_possible_truncation)]
                    let reduction_db = (20.0 * (input_magnitude / output_magnitude).log10()) as f32;
                    if reduction_db.is_finite() && reduction_db > worst_db {
                        worst_db = reduction_db;
                    }
                }
            }
            output.push(limited);
        }
        // Peak hold: a quiet block never clears an earlier, larger reduction.
        if worst_db > self.reduction_db {
            self.reduction_db = worst_db;
        }
        output
    }

    /// Largest gain reduction applied so far, in decibels, as a non-negative magnitude.
    ///
    /// Zero when no sample has exceeded [`KNEE`].
    #[must_use]
    pub fn reduction_db(&self) -> f32 {
        self.reduction_db
    }

    /// Clear the reduction meter back to 0 dB.
    pub fn reset_reduction(&mut self) {
        self.reduction_db = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use approx::assert_abs_diff_eq;

    use super::*;

    /// Audio rate of the built-in fixtures, in hertz.
    const RATE_HZ: f64 = 48_000.0;

    /// Frequency of the phase/amplitude fixture, in hertz.
    const TONE_HZ: f64 = 1_000.0;

    /// Amplitude of the small-signal fixture, linear full scale (below the knee).
    const SMALL_AMPLITUDE: f32 = 0.5;

    /// Amplitude of the hard-driven fixture, linear full scale (far above the knee).
    const LOUD_AMPLITUDE: f32 = 20.0;

    /// Tolerance for continuity, bounds, and monotonicity checks.
    /// Probe offset used to step just across the knee, in amplitude.
    const EPSILON: f32 = 1e-6;

    /// Tolerance for comparing values that sit one probe offset apart, in amplitude.
    ///
    /// One ULP at 0.9 is 5.96e-8, while `0.9f32 - 1e-6` differs from `0.9f32` by 1.0133e-6: the
    /// probe offset itself dominates f32 rounding. Comparing with `EPSILON` would therefore demand
    /// that a bit-exact identity branch move a sample by less than the offset that separated it,
    /// which no implementation can satisfy. Two offsets still reject a real discontinuity, since
    /// the tanh shoulder only advances by the offset across the knee.
    const CONTINUITY_TOLERANCE: f32 = 2.0 * EPSILON;

    /// Step of the monotonicity sweep, in linear full scale.
    const SWEEP_STEP: f32 = 1e-3;

    /// Converts a duration in milliseconds to a sample count at [`RATE_HZ`].
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn samples(milliseconds: f64) -> usize {
        (milliseconds * RATE_HZ / 1_000.0).round() as usize
    }

    /// A 1 kHz tone of `amplitude`, deterministic and phase-continuous from sample 0.
    fn tone(amplitude: f32) -> Vec<f32> {
        let amplitude = f64::from(amplitude);
        (0..samples(20.0))
            .map(|index| {
                #[allow(clippy::cast_precision_loss)]
                let phase = 2.0 * std::f64::consts::PI * TONE_HZ * index as f64 / RATE_HZ;
                #[allow(clippy::cast_possible_truncation)]
                let sample = (amplitude * phase.sin()) as f32;
                sample
            })
            .collect()
    }

    #[test]
    fn soft_limit_is_identity_below_the_knee() {
        // Exact equality, not approximate: normal speech must be bit-identical, so the limiter can
        // never colour a signal that does not need it.
        for sample in [
            0.0_f32,
            0.1,
            -0.1,
            0.25,
            -0.25,
            0.5,
            -0.5,
            KNEE - EPSILON,
            -(KNEE - EPSILON),
            KNEE,
            -KNEE,
        ] {
            assert_eq!(
                soft_limit(sample).to_bits(),
                sample.to_bits(),
                "|{sample}| <= {KNEE} must pass through bit-exactly"
            );
        }

        // A whole small-signal block: every sample unchanged.
        let limiter_input = tone(SMALL_AMPLITUDE);
        let mut limiter = SoftLimiter::new();
        let output = limiter.process(&limiter_input);
        assert_eq!(
            output.len(),
            limiter_input.len(),
            "the limiter must return exactly one output sample per input sample"
        );
        for (index, (expected, actual)) in limiter_input.iter().zip(output.iter()).enumerate() {
            assert_eq!(
                expected.to_bits(),
                actual.to_bits(),
                "sample {index} below the knee changed from {expected} to {actual}"
            );
        }
        assert_eq!(
            limiter.reduction_db(),
            0.0,
            "a signal entirely below the knee must report no reduction"
        );
    }

    #[test]
    fn soft_limit_is_continuous_and_monotonic_at_the_knee() {
        // Left and right limits at the knee must agree: a discontinuity would be an audible click,
        // and would also make the limiter non-monotonic around the knee.
        let below = soft_limit(KNEE - EPSILON);
        let above = soft_limit(KNEE + EPSILON);
        let at = soft_limit(KNEE);
        assert!(
            (below - at).abs() <= CONTINUITY_TOLERANCE,
            "the curve must be continuous at the knee: {below} below versus {at} at the knee"
        );
        assert!(
            (above - at).abs() <= CONTINUITY_TOLERANCE,
            "the curve must be continuous at the knee: {above} above versus {at} at the knee"
        );
        assert!(
            (at - KNEE).abs() <= EPSILON,
            "the knee value must be {KNEE} (unity slope up to the knee), got {at}"
        );

        // Slope just below the knee is unity, which is what makes the shoulder inaudible.
        let slope_below = (at - soft_limit(KNEE - SWEEP_STEP)) / SWEEP_STEP;
        assert!(
            (slope_below - 1.0).abs() <= 0.01,
            "the slope below the knee must be unity, measured {slope_below}"
        );

        // Monotonic over a sweep that crosses the knee and runs well past full scale: strictly
        // non-decreasing, with no step larger than the around-the-knee tolerance.
        let mut previous = soft_limit(-40.0);
        let mut sample = -40.0_f32;
        while sample <= 40.0 {
            let current = soft_limit(sample);
            assert!(
                current >= previous - EPSILON,
                "soft_limit must be non-decreasing: it fell from {previous} to {current} at {sample}"
            );
            previous = current;
            sample += SWEEP_STEP;
        }
    }

    #[test]
    fn soft_limit_is_bounded_to_unit_range() {
        let mut values = vec![
            0.0_f32,
            KNEE,
            -KNEE,
            KNEE + WIDTH,
            -(KNEE + WIDTH),
            1.0,
            -1.0,
            1.5,
            -1.5,
            10.0,
            -10.0,
            1_000.0,
            -1_000.0,
            1e6,
            -1e6,
            f32::MAX,
            f32::MIN,
        ];
        // A fine sweep on both sides, including the whole soft region.
        let mut sample = -2.0_f32;
        while sample <= 2.0 {
            values.push(sample);
            sample += SWEEP_STEP;
        }

        let mut limiter = SoftLimiter::new();
        let output = limiter.process(&values);
        assert_eq!(
            output.len(),
            values.len(),
            "the limiter must return exactly one output sample per input sample"
        );
        for (input, actual) in values.iter().zip(output.iter()) {
            assert!(
                actual.is_finite(),
                "soft_limit({input}) must be finite, got {actual}"
            );
            assert!(
                *actual >= -1.0 - EPSILON && *actual <= 1.0 + EPSILON,
                "soft_limit({input}) must stay within [-1, 1], got {actual}"
            );
        }

        // Huge driving must still report a finite, positive reduction.
        let reduction = limiter.reduction_db();
        assert!(
            reduction.is_finite() && reduction > 0.0,
            "driving f32::MAX through the limiter must report a finite positive reduction, got \
             {reduction}"
        );
        // `f32::MAX` genuinely needs ~770 dB of reduction, so the meter is only required to be
        // finite and positive; it must not saturate to infinity or clamp to a fake ceiling.
        assert!(
            reduction > 26.0,
            "driving full-scale-magnitude input needs more than 26 dB, got {reduction}"
        );
    }

    #[test]
    fn soft_limit_preserves_small_signal_phase_and_amplitude() {
        // A sine at half scale sits entirely below the knee, so phase and amplitude must be
        // untouched — not merely close, exactly equal.
        let limiter_input = tone(SMALL_AMPLITUDE);
        let mut limiter = SoftLimiter::new();
        let output = limiter.process(&limiter_input);

        let mut input_crossings = 0;
        let mut output_crossings = 0;
        for index in 1..limiter_input.len() {
            if (limiter_input[index] >= 0.0) != (limiter_input[index - 1] >= 0.0) {
                input_crossings += 1;
            }
            if (output[index] >= 0.0) != (output[index - 1] >= 0.0) {
                output_crossings += 1;
            }
        }
        assert_eq!(
            input_crossings, output_crossings,
            "a below-knee sine must keep every zero crossing"
        );
        assert!(
            input_crossings > 0,
            "fixture bug: the tone must cross zero at least once"
        );

        // Partition invariance falls out of statelessness, and is the cheapest proof that the
        // limiter holds no hidden per-sample phase state.
        for block in [1_usize, 7, 257, 1_024, 4_096] {
            let mut limiter = SoftLimiter::new();
            let mut partitioned = Vec::with_capacity(limiter_input.len());
            for chunk in limiter_input.chunks(block) {
                partitioned.extend(limiter.process(chunk));
            }
            for (index, (expected, actual)) in output.iter().zip(partitioned.iter()).enumerate() {
                assert!(
                    (expected - actual).abs() <= EPSILON,
                    "block size {block} changed sample {index}: {expected} vs {actual}"
                );
            }
        }
    }

    #[test]
    fn limiter_reports_reduction_only_when_limiting() {
        let mut limiter = SoftLimiter::new();
        assert_eq!(
            limiter.reduction_db(),
            0.0,
            "a fresh limiter has applied no reduction"
        );

        // Below the knee: still nothing to report.
        let quiet = tone(SMALL_AMPLITUDE);
        limiter.process(&quiet);
        assert_eq!(
            limiter.reduction_db(),
            0.0,
            "a below-knee block must not report reduction"
        );

        // Driven far past the knee: the meter must jump, and by an amount that matches the gain
        // the limiter actually applied to the peak.
        let loud = tone(LOUD_AMPLITUDE);
        let output = limiter.process(&loud);
        let output_peak = output.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()));
        let reduction = limiter.reduction_db();
        assert!(
            reduction > 0.0,
            "driving a {LOUD_AMPLITUDE} amplitude tone through the limiter must report positive \
             reduction, got {reduction}"
        );
        let expected_reduction = 20.0 * (output_peak / LOUD_AMPLITUDE).log10();
        assert!(
            (reduction + expected_reduction).abs() <= 0.5,
            "reported reduction {reduction} dB must match the applied gain \
             {} dB for a peak of {output_peak}",
            -expected_reduction
        );

        // The meter is a peak hold: a second quiet block must not clear it.
        limiter.process(&quiet);
        assert!(
            (limiter.reduction_db() - reduction).abs() <= EPSILON,
            "a quiet block must not clear the accumulated reduction meter"
        );

        // An explicit reset clears it.
        limiter.reset_reduction();
        assert_eq!(
            limiter.reduction_db(),
            0.0,
            "reset_reduction must clear the meter"
        );
    }

    #[test]
    fn knee_and_width_match_the_planned_shape() {
        // The other limiter tests derive their comparisons from KNEE and WIDTH, so moving either
        // constant would move both sides and survive. Pin the plan's literal values (section 4.5)
        // and the two consequences that define the curve's shape.
        assert_abs_diff_eq!(KNEE, 0.9, epsilon = f32::EPSILON);
        assert_abs_diff_eq!(WIDTH, 0.1, epsilon = f32::EPSILON);
        assert_abs_diff_eq!(KNEE + WIDTH, 1.0, epsilon = f32::EPSILON);

        // Bit-exact identity holds at the literal knee, not merely at `KNEE - epsilon`.
        assert_eq!(soft_limit(0.9).to_bits(), 0.9_f32.to_bits());
        assert_eq!(soft_limit(-0.9).to_bits(), (-0.9_f32).to_bits());

        // Half a width above the knee must land at exactly the tanh midpoint, which pins the
        // shoulder's actual width: a narrower window would already be saturated there, and a wider
        // one would still be near unity.
        let shoulder = soft_limit(KNEE + WIDTH / 2.0);
        let expected = KNEE + WIDTH * (0.5_f32).tanh();
        assert_abs_diff_eq!(shoulder, expected, epsilon = 1e-7);
        // The literal is the exact-real value; f32 subtraction at `0.95 - 0.9` shifts it by ~5e-5,
        // so the tolerance is set just above that cancellation error. It still rejects a window of
        // 0.05 (expected 0.984) or 0.2 (expected 0.890) by a wide margin.
        assert_abs_diff_eq!(expected, 0.946_158_24, epsilon = 1e-4);

        // The f32 saturation point is documented and measurable.
        assert_abs_diff_eq!(soft_limit(1.746_160_7), 1.0, epsilon = f32::EPSILON);
        assert!(
            soft_limit(1.7) < 1.0,
            "below the saturation point the output must stay strictly under full scale"
        );
    }
}
