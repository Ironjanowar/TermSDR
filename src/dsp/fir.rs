//! Reusable stateful FIR primitives designed from pass/stop specifications.
//!
//! Filters are designed from a [`LowPassSpec`] at the rate they actually run at. The order is
//! computed from the requested transition width and stopband attenuation (Kaiser-windowed sinc or
//! an independently implemented equivalent), so a narrow transition costs more taps than a wide
//! one and no fixed tap count is assumed.
//!
//! Both filters are linear phase: a real-valued impulse response that is symmetric about its
//! centre tap. That makes the group delay exactly `(taps_len - 1) / 2` samples and keeps the
//! phase response of a complex baseband signal straight, not warped.
//!
//! All units are explicit: hertz for frequencies, decibels for attenuation, and samples for the
//! delay and buffer lengths.
//!
//! # Examples
//!
//! ```ignore
//! let spec = LowPassSpec {
//!     sample_rate_hz: 48_000.0,
//!     pass_edge_hz: 3_000.0,
//!     stop_edge_hz: 4_000.0,
//!     stopband_db: 80.0,
//! };
//! let mut fir = RealFir::design(spec)?;
//! let filtered = fir.process(&samples);
//! assert_eq!(filtered.len(), samples.len());
//! ```

use thiserror::Error;

use crate::types::IqSample;

/// Largest stopband attenuation a specification may request, in decibels.
///
/// `f32` tap coefficients bottom out near -155 dB, so any deeper request is wasted precision. This
/// bound also keeps the Kaiser `beta` far below the point where the Bessel power series overflows
/// (around 114,647 dB) and the design math would emit `NaN` taps.
pub(crate) const MAX_STOPBAND_DB: f64 = 200.0;

/// Design specification for a linear-phase low-pass filter.
///
/// The response is flat (within the designed ripple) up to `pass_edge_hz`, within about 1 dB of
/// `stopband_db` down from `stop_edge_hz` up to Nyquist, and unconstrained in the transition band
/// between the two edges. All frequencies are in hertz; `stopband_db` is a positive attenuation in
/// decibels (for example `80.0` means 10^-4 amplitude). The Kaiser estimate is accurate to roughly
/// +-1 dB, so callers needing a hard floor should design with margin; `f32` taps themselves bottom
/// out near -155 dB.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LowPassSpec {
    /// Rate the filter will process samples at, in hertz. Must be finite and positive.
    pub sample_rate_hz: f64,
    /// Upper edge of the passband, in hertz. Must be finite, positive, and below `stop_edge_hz`.
    pub pass_edge_hz: f64,
    /// Lower edge of the stopband, in hertz. Must be finite, above `pass_edge_hz`, and at or
    /// below Nyquist.
    pub stop_edge_hz: f64,
    /// Minimum stopband attenuation in decibels. Must be finite, positive, and at most
    /// [`MAX_STOPBAND_DB`].
    pub stopband_db: f64,
}

/// Reasons a [`LowPassSpec`] cannot be turned into a filter.
///
/// Every rejected specification produces one of these variants; no invalid input reaches the
/// design math, so callers never see a panic or a filter full of `NaN` taps.
#[derive(Debug, Clone, PartialEq, Error)]
pub(crate) enum FirSpecError {
    /// The sample rate was zero or negative, so no frequency can be normalized against it.
    #[error("sample rate must be positive, got {sample_rate_hz} Hz")]
    NonPositiveSampleRate {
        /// Rejected sample rate in hertz.
        sample_rate_hz: f64,
    },
    /// The sample rate was `NaN` or infinite.
    #[error("sample rate must be finite, got {sample_rate_hz} Hz")]
    NonFiniteSampleRate {
        /// Rejected sample rate in hertz.
        sample_rate_hz: f64,
    },
    /// A band edge was `NaN` or infinite. `edge` names the offending field.
    #[error("{edge} must be finite, got {value_hz} Hz")]
    NonFiniteEdge {
        /// Field name of the rejected edge, e.g. `"pass_edge_hz"`.
        edge: &'static str,
        /// Rejected frequency in hertz.
        value_hz: f64,
    },
    /// A band edge was zero or negative. `edge` names the offending field.
    #[error("{edge} must be positive, got {value_hz} Hz")]
    NonPositiveEdge {
        /// Field name of the rejected edge, e.g. `"stop_edge_hz"`.
        edge: &'static str,
        /// Rejected frequency in hertz.
        value_hz: f64,
    },
    /// The passband did not end strictly below the stopband, leaving no transition to design.
    #[error(
        "pass edge must be below stop edge, got pass={pass_edge_hz} Hz and stop={stop_edge_hz} Hz"
    )]
    PassEdgeNotBelowStopEdge {
        /// Requested passband edge in hertz.
        pass_edge_hz: f64,
        /// Requested stopband edge in hertz.
        stop_edge_hz: f64,
    },
    /// The stopband edge exceeded the Nyquist frequency of the processing rate.
    #[error("stop edge {stop_edge_hz} Hz exceeds the Nyquist frequency of {nyquist_hz} Hz")]
    StopEdgeAboveNyquist {
        /// Requested stopband edge in hertz.
        stop_edge_hz: f64,
        /// Nyquist frequency (`sample_rate_hz / 2`) in hertz.
        nyquist_hz: f64,
    },
    /// The requested stopband attenuation was zero or negative.
    #[error("stopband attenuation must be positive decibels, got {stopband_db} dB")]
    NonPositiveStopbandDb {
        /// Rejected attenuation in decibels.
        stopband_db: f64,
    },
    /// The requested stopband attenuation was `NaN` or infinite.
    #[error("stopband attenuation must be finite decibels, got {stopband_db} dB")]
    NonFiniteStopbandDb {
        /// Rejected attenuation in decibels.
        stopband_db: f64,
    },
    /// The requested stopband attenuation exceeded what the design math can represent.
    #[error("stopband attenuation must be at most {max_stopband_db} dB, got {stopband_db} dB")]
    StopbandDbAboveLimit {
        /// Rejected attenuation in decibels.
        stopband_db: f64,
        /// Largest attenuation the designer accepts, in decibels.
        max_stopband_db: f64,
    },
}

impl LowPassSpec {
    /// Returns the Nyquist frequency implied by [`sample_rate_hz`](Self::sample_rate_hz).
    #[must_use]
    pub(crate) fn nyquist_hz(&self) -> f64 {
        self.sample_rate_hz / 2.0
    }

    /// Checks every field and returns the first violation.
    ///
    /// The order is fixed so that a specification with several faults always reports the same
    /// one: sample rate, pass edge, stop edge, edge ordering, Nyquist, stopband attenuation.
    ///
    /// # Errors
    ///
    /// Returns a [`FirSpecError`] variant describing the first rejected field, in this order:
    /// sample rate, pass edge, stop edge, edge ordering, Nyquist, stopband attenuation.
    pub(crate) fn validate(&self) -> Result<(), FirSpecError> {
        if !self.sample_rate_hz.is_finite() {
            return Err(FirSpecError::NonFiniteSampleRate {
                sample_rate_hz: self.sample_rate_hz,
            });
        }
        if self.sample_rate_hz <= 0.0 {
            return Err(FirSpecError::NonPositiveSampleRate {
                sample_rate_hz: self.sample_rate_hz,
            });
        }
        Self::check_edge("pass_edge_hz", self.pass_edge_hz)?;
        Self::check_edge("stop_edge_hz", self.stop_edge_hz)?;
        if self.pass_edge_hz >= self.stop_edge_hz {
            return Err(FirSpecError::PassEdgeNotBelowStopEdge {
                pass_edge_hz: self.pass_edge_hz,
                stop_edge_hz: self.stop_edge_hz,
            });
        }
        let nyquist_hz = self.nyquist_hz();
        if self.stop_edge_hz > nyquist_hz {
            return Err(FirSpecError::StopEdgeAboveNyquist {
                stop_edge_hz: self.stop_edge_hz,
                nyquist_hz,
            });
        }
        if !self.stopband_db.is_finite() {
            return Err(FirSpecError::NonFiniteStopbandDb {
                stopband_db: self.stopband_db,
            });
        }
        if self.stopband_db <= 0.0 {
            return Err(FirSpecError::NonPositiveStopbandDb {
                stopband_db: self.stopband_db,
            });
        }
        if self.stopband_db > MAX_STOPBAND_DB {
            return Err(FirSpecError::StopbandDbAboveLimit {
                stopband_db: self.stopband_db,
                max_stopband_db: MAX_STOPBAND_DB,
            });
        }
        Ok(())
    }

    /// Rejects a band edge that is unusable for normalization, in hertz.
    ///
    /// `edge` is the field name reported in the error, e.g. `"pass_edge_hz"`.
    fn check_edge(edge: &'static str, value_hz: f64) -> Result<(), FirSpecError> {
        if !value_hz.is_finite() {
            return Err(FirSpecError::NonFiniteEdge { edge, value_hz });
        }
        if value_hz <= 0.0 {
            return Err(FirSpecError::NonPositiveEdge { edge, value_hz });
        }
        Ok(())
    }
}

/// Kaiser-windowed sinc design maths for [`LowPassSpec`].
///
/// Kept private and separate from the stateful filter types so the public surface stays small and
/// the tap-count / window helpers are only reachable through [`design_taps`](self::design_taps).
///
/// Only [`design_taps`](self::design_taps) leaves the module; the order and window helpers stay
/// internal so a caller cannot design half a filter.
mod design {
    use std::f64::consts::{PI, TAU};

    use super::LowPassSpec;

    /// Smallest tap count the designer emits: odd, so the group delay is a whole number of samples,
    /// and above one, so the filter is never a bare wire.
    const MIN_TAPS: usize = 3;

    /// Largest tap count the designer emits, in taps.
    ///
    /// A specification narrow enough to demand more taps is clamped instead of being allocated. The
    /// ceiling is set by throughput, not memory: `100_001` taps is 50,000 samples of group delay
    /// (about 1.04 s at 48 kHz) and costs roughly 4.8 GMAC/s at that rate, around ten seconds of
    /// CPU per second of audio. A real-time channel filter of this width should fail its spec
    /// loudly rather than pass it by accident, so callers keep transitions wide enough to stay
    /// far below this clamp.
    const MAX_TAPS: usize = 100_001;

    /// Builds the symmetric, unity-DC-gain impulse response for `spec`.
    ///
    /// The order is computed from the Kaiser estimate: the transition width in normalized frequency
    /// and the requested stopband attenuation set the tap count, so a narrow transition or a deeper
    /// stopband costs proportionally more taps. The result is normalized so the taps sum to exactly
    /// `1.0`, which fixes DC gain at unity, and mirrored so it is bit-exactly symmetric about the
    /// centre tap (and therefore exactly linear phase).
    pub(super) fn design_taps(spec: &LowPassSpec) -> Vec<f32> {
        let taps_len = tap_count(spec);
        let centre = (taps_len - 1) / 2;
        let beta = kaiser_beta(spec.stopband_db);
        let denominator = bessel_i0(beta);
        // Cutoff at the midpoint of the transition band: the windowed sinc is -6 dB there, putting
        // half the transition on each side of the requested edges.
        let cutoff_normalized =
            f64::midpoint(spec.pass_edge_hz, spec.stop_edge_hz) / spec.sample_rate_hz;

        let mut half = Vec::with_capacity(centre + 1);
        let mut sum = 0.0_f64;
        #[allow(clippy::cast_precision_loss)] // tap indices stay far below 2^53
        let centre_f64 = centre as f64;
        for index in 0..=centre {
            #[allow(clippy::cast_precision_loss)] // tap indices stay far below 2^53
            let offset = index as f64 - centre_f64;
            let window = if beta == 0.0 {
                1.0
            } else {
                let ratio = offset / centre_f64;
                bessel_i0(beta * (1.0 - ratio * ratio).max(0.0).sqrt()) / denominator
            };
            let ideal = if offset == 0.0 {
                2.0 * cutoff_normalized
            } else {
                (TAU * cutoff_normalized * offset).sin() / (PI * offset)
            };
            let tap = ideal * window;
            // The mirrored copy is pushed before the centre, so only the centre counts once.
            if index < centre {
                sum += 2.0 * tap;
            } else {
                sum += tap;
            }
            half.push(tap);
        }

        // A spec whose normalized cutoff underflows to zero (an absurdly large rate against a tiny
        // edge) yields an all-zero response whose sum is zero, and normalizing it would emit NaN
        // taps. Fall back to a pure delay: finite, unity DC gain, and still linear phase.
        if !sum.is_finite() || sum.abs() <= f64::MIN_POSITIVE {
            let mut taps = vec![0.0_f32; taps_len];
            taps[centre] = 1.0;
            return taps;
        }

        let mut taps = Vec::with_capacity(taps_len);
        // `half[i]` holds the tap at offset `i - centre`, i.e. it runs from the left edge inward to
        // the centre tap. Emit left-to-centre, then mirror the inner taps back out to the right edge.
        for &tap in &half {
            #[allow(clippy::cast_possible_truncation)] // taps live at f32 precision by design
            taps.push((tap / sum) as f32);
        }
        for index in (0..centre).rev() {
            #[allow(clippy::cast_possible_truncation)]
            taps.push((half[index] / sum) as f32);
        }
        taps
    }

    /// Odd tap count computed from `spec`'s transition width and stopband attenuation, in taps.
    ///
    /// Uses the standard Kaiser order estimate `N = (A - 8) / (2.285 * 2*pi * df) + 1`, where `A` is
    /// the stopband attenuation in decibels and `df` is the transition width normalized to the
    /// sample rate. The estimate is clamped to `[MIN_TAPS, MAX_TAPS]` and forced odd so the group
    /// delay is a whole number of samples.
    fn tap_count(spec: &LowPassSpec) -> usize {
        let transition_normalized = (spec.stop_edge_hz - spec.pass_edge_hz) / spec.sample_rate_hz;
        let estimate = (spec.stopband_db - 8.0) / (2.285 * TAU * transition_normalized) + 1.0;

        // A transition so wide (or attenuation so low) that the estimate collapses still needs a
        // usable filter, and an absurdly narrow one is clamped rather than allocated.
        let rounded = if estimate.is_finite() && estimate > 0.0 {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let ceil = estimate.ceil() as usize;
            ceil.clamp(MIN_TAPS, MAX_TAPS)
        } else {
            MIN_TAPS
        };
        if rounded % 2 == 0 {
            rounded + 1
        } else {
            rounded
        }
    }

    /// Kaiser window shape parameter `beta` for a stopband attenuation of `attenuation_db`.
    ///
    /// Piecewise fit from Kaiser's original tables: `0.1102 * (A - 8.7)` above 50 dB, an empirical
    /// rational form between 21 dB and 50 dB, and a rectangular window (beta 0) below 21 dB.
    fn kaiser_beta(attenuation_db: f64) -> f64 {
        if attenuation_db > 50.0 {
            0.1102 * (attenuation_db - 8.7)
        } else if attenuation_db >= 21.0 {
            0.5842 * (attenuation_db - 21.0).powf(0.4) + 0.07886 * (attenuation_db - 21.0)
        } else {
            0.0
        }
    }

    /// Zeroth-order modified Bessel function of the first kind, `I0(x)`.
    ///
    /// Evaluated by its power series `sum_k ((x/2)^k / k!)^2`, which converges in a few dozen terms
    /// for the beta values a Kaiser window uses (below about 20).
    fn bessel_i0(x: f64) -> f64 {
        let mut sum = 1.0;
        let mut term = 1.0;
        let half_x = x / 2.0;
        for k in 1..=64_u32 {
            let factor = half_x / f64::from(k);
            term *= factor * factor;
            sum += term;
            if term < sum * 1e-17 {
                break;
            }
        }
        sum
    }
}

/// Stateful real-valued low-pass FIR filter.
///
/// Output sample `n` corresponds to input sample `n - group_delay_samples()`, so processing
/// introduces no length change: a call with `k` samples returns exactly `k` samples. Filter state
/// carries across calls, which makes the result independent of how the stream is partitioned.
#[derive(Debug)]
pub(crate) struct RealFir {
    /// Symmetric real-valued impulse response, in processing order.
    taps: Vec<f32>,
    /// Circular history of the last `taps.len()` input samples, in samples.
    history: Vec<f32>,
    /// Index of the newest sample in `history`.
    cursor: usize,
    /// Rate the taps were designed for, in hertz.
    sample_rate_hz: f64,
}

// Plan Task 7 (channelizer) is the first non-test consumer of this filter.
#[allow(dead_code)]
impl RealFir {
    /// Designs a filter that meets `spec`.
    ///
    /// # Errors
    ///
    /// Returns the [`FirSpecError`] produced by [`LowPassSpec::validate`].
    pub(crate) fn design(spec: LowPassSpec) -> Result<Self, FirSpecError> {
        spec.validate()?;
        let taps = design::design_taps(&spec);
        let history = vec![0.0_f32; taps.len()];
        Ok(Self {
            taps,
            history,
            cursor: 0,
            sample_rate_hz: spec.sample_rate_hz,
        })
    }

    /// Number of taps in the impulse response. Always odd, so the group delay is a whole number
    /// of samples.
    #[must_use]
    pub(crate) fn taps_len(&self) -> usize {
        self.taps.len()
    }

    /// The real-valued tap coefficients, in processing order.
    #[must_use]
    pub(crate) fn taps(&self) -> &[f32] {
        &self.taps
    }

    /// The rate this filter was designed for, in hertz.
    #[must_use]
    pub(crate) fn sample_rate_hz(&self) -> f64 {
        self.sample_rate_hz
    }

    /// Group delay in samples, `(taps_len - 1) / 2`.
    #[must_use]
    pub(crate) fn group_delay_samples(&self) -> usize {
        (self.taps.len() - 1) / 2
    }

    /// Number of samples a freshly designed filter needs before its output reflects the steady
    /// state of the input, in samples.
    ///
    /// Two impulse-response lengths comfortably fill the delay line; callers measuring gain
    /// should discard at least this many leading output samples.
    #[must_use]
    pub(crate) fn settle_samples(&self) -> usize {
        self.taps.len() * 2
    }

    /// Filters `input`, returning one output sample per input sample.
    ///
    /// Any block size is accepted, including empty slices. Output is delayed by
    /// [`group_delay_samples`](Self::group_delay_samples) relative to the input stream.
    pub(crate) fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let taps_len = self.taps.len();
        let mut output = Vec::with_capacity(input.len());
        for &sample in input {
            self.history[self.cursor] = sample;
            let mut acc = 0.0_f64;
            let mut index = self.cursor;
            for &tap in &self.taps {
                acc += f64::from(tap) * f64::from(self.history[index]);
                index = if index == 0 { taps_len - 1 } else { index - 1 };
            }
            #[allow(clippy::cast_possible_truncation)] // deliberate f64 accumulator -> f32 sink
            output.push(acc as f32);
            self.cursor = if self.cursor + 1 == taps_len {
                0
            } else {
                self.cursor + 1
            };
        }
        output
    }
}

/// Stateful complex (I/Q) low-pass FIR filter.
///
/// Applies a real-valued, linear-phase impulse response to both I and Q, so passband phase and
/// relative I/Q gain are preserved while out-of-band energy is rejected.
#[derive(Debug)]
pub(crate) struct ComplexFir {
    /// Symmetric real-valued impulse response, in processing order.
    taps: Vec<f32>,
    /// Circular history of the last `taps.len()` input samples, in samples.
    history: Vec<IqSample>,
    /// Index of the newest sample in `history`.
    cursor: usize,
    /// Rate the taps were designed for, in hertz.
    sample_rate_hz: f64,
}

// Plan Task 7 (channelizer) is the first non-test consumer of this filter.
#[allow(dead_code)]
impl ComplexFir {
    /// Designs a filter that meets `spec`.
    ///
    /// # Errors
    ///
    /// Returns the [`FirSpecError`] produced by [`LowPassSpec::validate`].
    pub(crate) fn design(spec: LowPassSpec) -> Result<Self, FirSpecError> {
        spec.validate()?;
        let taps = design::design_taps(&spec);
        let history = vec![IqSample::new(0.0, 0.0); taps.len()];
        Ok(Self {
            taps,
            history,
            cursor: 0,
            sample_rate_hz: spec.sample_rate_hz,
        })
    }

    /// Number of taps in the impulse response. Always odd.
    #[must_use]
    pub(crate) fn taps_len(&self) -> usize {
        self.taps.len()
    }

    /// The real-valued tap coefficients, in processing order.
    #[must_use]
    pub(crate) fn taps(&self) -> &[f32] {
        &self.taps
    }

    /// The rate this filter was designed for, in hertz.
    #[must_use]
    pub(crate) fn sample_rate_hz(&self) -> f64 {
        self.sample_rate_hz
    }

    /// Group delay in samples, `(taps_len - 1) / 2`.
    #[must_use]
    pub(crate) fn group_delay_samples(&self) -> usize {
        (self.taps.len() - 1) / 2
    }

    /// Number of samples a freshly designed filter needs before its output reflects the steady
    /// state of the input, in samples.
    #[must_use]
    pub(crate) fn settle_samples(&self) -> usize {
        self.taps.len() * 2
    }

    /// Filters `input`, returning one output sample per input sample.
    ///
    /// Any block size is accepted, including empty slices. Output is delayed by
    /// [`group_delay_samples`](Self::group_delay_samples) relative to the input stream.
    pub(crate) fn process(&mut self, input: &[IqSample]) -> Vec<IqSample> {
        let taps_len = self.taps.len();
        let mut output = Vec::with_capacity(input.len());
        for &sample in input {
            self.history[self.cursor] = sample;
            let mut acc_i = 0.0_f64;
            let mut acc_q = 0.0_f64;
            let mut index = self.cursor;
            for &tap in &self.taps {
                let delayed = self.history[index];
                let tap_f64 = f64::from(tap);
                acc_i += tap_f64 * f64::from(delayed.re);
                acc_q += tap_f64 * f64::from(delayed.im);
                index = if index == 0 { taps_len - 1 } else { index - 1 };
            }
            #[allow(clippy::cast_possible_truncation)] // deliberate f64 accumulator -> f32 sink
            output.push(IqSample::new(acc_i as f32, acc_q as f32));
            self.cursor = if self.cursor + 1 == taps_len {
                0
            } else {
                self.cursor + 1
            };
        }
        output
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::too_many_lines
)]
mod tests {
    use std::f64::consts::TAU;

    use approx::assert_abs_diff_eq;

    use super::*;

    /// 48 kHz voice-channel low pass: 3 kHz passband, 4 kHz stop edge, comfortably beyond the
    /// 60 dB the tests assert so that design ripple never eats into the margin.
    fn voice_spec() -> LowPassSpec {
        LowPassSpec {
            sample_rate_hz: 48_000.0,
            pass_edge_hz: 3_000.0,
            stop_edge_hz: 4_000.0,
            stopband_db: 80.0,
        }
    }

    fn real_tone(len: usize, freq_hz: f64, sample_rate_hz: f64, phase_rad: f64) -> Vec<f32> {
        (0..len)
            .map(|n| {
                let phase = TAU * freq_hz * n as f64 / sample_rate_hz + phase_rad;
                phase.sin() as f32
            })
            .collect()
    }

    fn complex_tone(
        len: usize,
        freq_hz: f64,
        sample_rate_hz: f64,
        phase_rad: f64,
    ) -> Vec<IqSample> {
        (0..len)
            .map(|n| {
                let phase = TAU * freq_hz * n as f64 / sample_rate_hz + phase_rad;
                IqSample::new(phase.cos() as f32, phase.sin() as f32)
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        let energy: f64 = samples.iter().map(|ample| f64::from(*ample).powi(2)).sum();
        (energy / samples.len() as f64).sqrt()
    }

    /// Number of leading output samples to discard so measurements see steady state only.
    ///
    /// Two impulse-response lengths are plenty for the delay line to be full of the test tone.
    fn settle_samples(fir: &RealFir, len: usize) -> usize {
        let settled = fir.taps_len() * 2;
        assert!(
            settled < len / 2,
            "a {} tap design leaves no steady state in a {len} sample measurement window; the              order computed from the spec is unreasonably large",
            fir.taps_len()
        );
        settled
    }

    /// Steady-state gain in dB at `freq_hz`, measured well past the transient.
    fn measured_gain_db(fir: &mut RealFir, freq_hz: f64, spec: LowPassSpec) -> f64 {
        let len = 8_192;
        let settled = settle_samples(fir, len);
        let input = real_tone(len, freq_hz, spec.sample_rate_hz, 0.3);
        let output = fir.process(&input);
        let gain = rms(&output[settled..]) / rms(&input[settled..]);
        20.0 * gain.log10()
    }

    /// Smallest signed difference between two angles, in radians, in `(-pi, pi]`.
    fn phase_difference(a: f32, b: f32) -> f32 {
        use std::f32::consts::{PI, TAU as TAU_F32};
        let mut diff = a - b;
        while diff > PI {
            diff -= TAU_F32;
        }
        while diff <= -PI {
            diff += TAU_F32;
        }
        diff
    }

    /// Deterministic partition generator: no `rand` dependency, same partitions every run.
    struct Lcg(u64);

    impl Lcg {
        fn next_bounded(&mut self, upper: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let span = u64::try_from(upper).expect("upper bound fits u64");
            usize::try_from((self.0 >> 33) % span).expect("bounded value fits usize")
        }
    }

    fn partition_sizes(total: usize, max_block: usize) -> Vec<usize> {
        let mut lcg = Lcg(0x5EED_1234_ABCD_0001);
        let mut sizes = Vec::new();
        let mut remaining = total;
        while remaining > 0 {
            let block = (lcg.next_bounded(max_block) + 1).min(remaining);
            sizes.push(block);
            remaining -= block;
        }
        sizes
    }

    /// Fixed-size partition that covers `total` exactly, with a short final block.
    fn fixed_partitions(total: usize, block: usize) -> Vec<usize> {
        let mut sizes = vec![block; total / block];
        let remainder = total % block;
        if remainder > 0 {
            sizes.push(remainder);
        }
        sizes
    }

    fn process_partitioned(fir: &mut RealFir, input: &[f32], sizes: &[usize]) -> Vec<f32> {
        let mut output = Vec::with_capacity(input.len());
        let mut offset = 0;
        for &size in sizes {
            output.extend_from_slice(&fir.process(&input[offset..offset + size]));
            offset += size;
        }
        assert_eq!(
            offset,
            input.len(),
            "partitions must cover the input exactly"
        );
        output
    }

    #[test]
    fn real_fir_dc_gain_is_near_unity() {
        let spec = voice_spec();
        let mut fir = RealFir::design(spec).expect("valid voice-channel spec designs");
        let input = vec![1.0_f32; 4_096];
        let output = fir.process(&input);
        let settled = &output[settle_samples(&fir, input.len())..];
        let dc_gain: f64 =
            settled.iter().map(|sample| f64::from(*sample)).sum::<f64>() / settled.len() as f64;
        assert_abs_diff_eq!(dc_gain, 1.0, epsilon = 1e-3);
    }

    #[test]
    fn real_fir_passband_ripple_is_within_one_db() {
        let spec = voice_spec();
        let mut fir = RealFir::design(spec).expect("valid voice-channel spec designs");
        // Sweep from just above DC to the passband edge in equal steps.
        for step in 1..=12 {
            let freq_hz = spec.pass_edge_hz * f64::from(step) / 12.0;
            let gain_db = measured_gain_db(&mut fir, freq_hz, spec);
            assert!(
                gain_db.abs() <= 1.0,
                "passband gain at {freq_hz} Hz was {gain_db} dB, outside +/-1 dB"
            );
        }
    }

    #[test]
    fn real_fir_stopband_attenuation_is_at_least_sixty_db() {
        let spec = voice_spec();
        let mut fir = RealFir::design(spec).expect("valid voice-channel spec designs");
        let top_hz = spec.nyquist_hz() * 0.98;
        for step in 0..=40 {
            let freq_hz = spec.stop_edge_hz + (top_hz - spec.stop_edge_hz) * f64::from(step) / 40.0;
            let gain_db = measured_gain_db(&mut fir, freq_hz, spec);
            assert!(
                gain_db <= -60.0,
                "stopband gain at {freq_hz} Hz was {gain_db} dB, less than 60 dB of attenuation"
            );
        }
    }

    #[test]
    fn complex_fir_preserves_phase() {
        let spec = voice_spec();
        let mut fir = ComplexFir::design(spec).expect("valid voice-channel spec designs");
        let delay = fir.group_delay_samples();
        let freq_hz = 2_000.0;
        let input = complex_tone(8_192, freq_hz, spec.sample_rate_hz, 0.7);
        let output = fir.process(&input);
        assert_eq!(output.len(), input.len());

        let start = delay + fir.taps_len() * 2;
        let mut worst_rad = 0.0_f32;
        for n in start..input.len() {
            let expected = input[n - delay].arg();
            let measured = output[n].arg();
            let error = phase_difference(measured, expected).abs();
            worst_rad = worst_rad.max(error);
        }
        assert!(
            worst_rad <= 1e-3,
            "passband tone at {freq_hz} Hz showed up to {worst_rad} rad of phase error, expected the \
             pure {delay}-sample delay"
        );
    }

    #[test]
    fn real_fir_is_partition_invariant() {
        let spec = voice_spec();
        let len = 20_000;
        let input: Vec<f32> = (0..len)
            .map(|n| {
                let t = n as f64 / spec.sample_rate_hz;
                ((TAU * 700.0 * t).sin() * 0.5 + (TAU * 2_100.0 * t).sin() * 0.3) as f32
            })
            .collect();

        let mut reference = RealFir::design(spec).expect("valid voice-channel spec designs");
        let whole = reference.process(&input);

        let compare = |sizes: Vec<usize>, label: &str| {
            let mut fir = RealFir::design(spec).expect("valid voice-channel spec designs");
            let partitioned = process_partitioned(&mut fir, &input, &sizes);
            assert_eq!(partitioned.len(), whole.len(), "{label}: length changed");
            for (index, (a, b)) in whole.iter().zip(partitioned.iter()).enumerate() {
                assert!(
                    (f64::from(*a) - f64::from(*b)).abs() <= 1e-6,
                    "{label}: sample {index} was {a} in one block but {b} when partitioned"
                );
            }
        };

        compare(vec![len], "single block");
        compare(fixed_partitions(len, 257), "257-sample blocks");
        compare(fixed_partitions(len, 1_024), "1024-sample blocks");
        compare(fixed_partitions(len, 4_096), "4096-sample blocks");
        compare(partition_sizes(len, 4_096), "random partitions");
    }

    #[test]
    fn invalid_low_pass_spec_is_a_typed_error() {
        type Matcher = fn(&FirSpecError) -> bool;
        let valid = voice_spec();
        let cases: Vec<(&str, LowPassSpec, Matcher)> = vec![
            (
                "zero sample rate",
                LowPassSpec {
                    sample_rate_hz: 0.0,
                    ..valid
                },
                |err| matches!(err, FirSpecError::NonPositiveSampleRate { .. }),
            ),
            (
                "negative sample rate",
                LowPassSpec {
                    sample_rate_hz: -48_000.0,
                    ..valid
                },
                |err| matches!(err, FirSpecError::NonPositiveSampleRate { .. }),
            ),
            (
                "NaN sample rate",
                LowPassSpec {
                    sample_rate_hz: f64::NAN,
                    ..valid
                },
                |err| matches!(err, FirSpecError::NonFiniteSampleRate { .. }),
            ),
            (
                "infinite sample rate",
                LowPassSpec {
                    sample_rate_hz: f64::INFINITY,
                    ..valid
                },
                |err| matches!(err, FirSpecError::NonFiniteSampleRate { .. }),
            ),
            (
                "NaN pass edge",
                LowPassSpec {
                    pass_edge_hz: f64::NAN,
                    ..valid
                },
                |err| matches!(err, FirSpecError::NonFiniteEdge { .. }),
            ),
            (
                "infinite stop edge",
                LowPassSpec {
                    stop_edge_hz: f64::INFINITY,
                    ..valid
                },
                |err| matches!(err, FirSpecError::NonFiniteEdge { .. }),
            ),
            (
                "zero pass edge",
                LowPassSpec {
                    pass_edge_hz: 0.0,
                    ..valid
                },
                |err| matches!(err, FirSpecError::NonPositiveEdge { .. }),
            ),
            (
                "pass edge equals stop edge",
                LowPassSpec {
                    pass_edge_hz: 4_000.0,
                    stop_edge_hz: 4_000.0,
                    ..valid
                },
                |err| matches!(err, FirSpecError::PassEdgeNotBelowStopEdge { .. }),
            ),
            (
                "pass edge above stop edge",
                LowPassSpec {
                    pass_edge_hz: 5_000.0,
                    stop_edge_hz: 4_000.0,
                    ..valid
                },
                |err| matches!(err, FirSpecError::PassEdgeNotBelowStopEdge { .. }),
            ),
            (
                "stop edge above Nyquist",
                LowPassSpec {
                    stop_edge_hz: 30_000.0,
                    ..valid
                },
                |err| matches!(err, FirSpecError::StopEdgeAboveNyquist { .. }),
            ),
            (
                "zero stopband attenuation",
                LowPassSpec {
                    stopband_db: 0.0,
                    ..valid
                },
                |err| matches!(err, FirSpecError::NonPositiveStopbandDb { .. }),
            ),
            (
                "negative stopband attenuation",
                LowPassSpec {
                    stopband_db: -60.0,
                    ..valid
                },
                |err| matches!(err, FirSpecError::NonPositiveStopbandDb { .. }),
            ),
            (
                "NaN stopband attenuation",
                LowPassSpec {
                    stopband_db: f64::NAN,
                    ..valid
                },
                |err| matches!(err, FirSpecError::NonFiniteStopbandDb { .. }),
            ),
        ];

        for (label, spec, matches_variant) in cases {
            let real_err = RealFir::design(spec).expect_err(label);
            assert!(
                matches_variant(&real_err),
                "{label}: RealFir::design returned the wrong variant: {real_err:?}"
            );
            let complex_err = ComplexFir::design(spec).expect_err(label);
            assert!(
                matches_variant(&complex_err),
                "{label}: ComplexFir::design returned the wrong variant: {complex_err:?}"
            );
            assert!(
                matches_variant(&spec.validate().expect_err(label)),
                "{label}: LowPassSpec::validate returned the wrong variant"
            );
        }
    }

    #[test]
    fn excessive_stopband_attenuation_is_a_typed_error() {
        // Above roughly 114,647 dB the Kaiser beta overflows the Bessel series and the design math
        // would emit all-NaN taps. Validation must stop that before the math runs.
        let cases = [200.1_f64, 114_647.0, f64::MAX];
        for stopband_db in cases {
            let spec = LowPassSpec {
                stopband_db,
                ..voice_spec()
            };
            let error = RealFir::design(spec).expect_err("absurd attenuation must be rejected");
            assert!(
                matches!(
                    error,
                    FirSpecError::StopbandDbAboveLimit {
                        stopband_db: rejected,
                        ..
                    } if rejected == stopband_db
                ),
                "{stopband_db} dB returned the wrong variant: {error:?}"
            );
            assert!(ComplexFir::design(spec).is_err());
            assert!(spec.validate().is_err());
        }
    }

    #[test]
    fn complex_fir_is_partition_invariant() {
        let spec = voice_spec();
        let len = 20_000;
        let input = complex_tone(len, 1_700.0, spec.sample_rate_hz, 0.2);

        let mut reference = ComplexFir::design(spec).expect("valid voice-channel spec designs");
        let whole = reference.process(&input);

        let compare = |sizes: Vec<usize>, label: &str| {
            let mut fir = ComplexFir::design(spec).expect("valid voice-channel spec designs");
            let mut partitioned = Vec::with_capacity(input.len());
            let mut offset = 0;
            for &size in &sizes {
                partitioned.extend_from_slice(&fir.process(&input[offset..offset + size]));
                offset += size;
            }
            assert_eq!(
                offset,
                input.len(),
                "{label}: partitions must cover the input"
            );
            assert_eq!(partitioned.len(), whole.len(), "{label}: length changed");
            for (index, (a, b)) in whole.iter().zip(partitioned.iter()).enumerate() {
                assert_eq!(
                    a.re.to_bits(),
                    b.re.to_bits(),
                    "{label}: I at sample {index} differed between block sizes"
                );
                assert_eq!(
                    a.im.to_bits(),
                    b.im.to_bits(),
                    "{label}: Q at sample {index} differed between block sizes"
                );
            }
        };

        compare(vec![len], "single block");
        compare(fixed_partitions(len, 257), "257-sample blocks");
        compare(fixed_partitions(len, 1_024), "1024-sample blocks");
        compare(fixed_partitions(len, 4_096), "4096-sample blocks");
        compare(partition_sizes(len, 4_096), "random partitions");
    }

    #[test]
    fn real_fir_and_complex_fir_have_odd_tap_counts() {
        let real = RealFir::design(voice_spec()).expect("valid voice-channel spec designs");
        let complex = ComplexFir::design(voice_spec()).expect("valid voice-channel spec designs");
        assert!(
            real.taps_len() > 1,
            "voice-channel filter must have tap support"
        );
        assert_eq!(
            real.taps_len() % 2,
            1,
            "real tap count must be odd for integer group delay"
        );
        assert_eq!(
            complex.taps_len() % 2,
            1,
            "complex tap count must be odd for integer group delay"
        );
        assert_eq!(real.taps().len(), real.taps_len());
        assert_eq!(complex.taps().len(), complex.taps_len());
        assert_eq!(
            real.group_delay_samples(),
            (real.taps_len() - 1) / 2,
            "group delay must follow from the tap count"
        );
        assert_eq!(
            complex.group_delay_samples(),
            (complex.taps_len() - 1) / 2,
            "complex group delay must follow from the tap count"
        );
        assert_abs_diff_eq!(
            real.sample_rate_hz(),
            voice_spec().sample_rate_hz,
            epsilon = f64::EPSILON
        );
        assert_abs_diff_eq!(
            complex.sample_rate_hz(),
            voice_spec().sample_rate_hz,
            epsilon = f64::EPSILON
        );

        // Computed order: a narrow transition at high attenuation needs many more taps than a
        // wide, low-attenuation one. A fixed tap count cannot satisfy both.
        let narrow = LowPassSpec {
            sample_rate_hz: 48_000.0,
            pass_edge_hz: 3_000.0,
            stop_edge_hz: 3_400.0,
            stopband_db: 100.0,
        };
        let wide = LowPassSpec {
            sample_rate_hz: 48_000.0,
            pass_edge_hz: 3_000.0,
            stop_edge_hz: 20_000.0,
            stopband_db: 40.0,
        };
        let narrow_taps = RealFir::design(narrow)
            .expect("narrow transition spec designs")
            .taps_len();
        let wide_taps = RealFir::design(wide)
            .expect("wide transition spec designs")
            .taps_len();
        assert!(
            narrow_taps > wide_taps,
            "order must follow the spec: narrow transition needed {narrow_taps} taps but wide \
             transition needed {wide_taps}"
        );
        assert_eq!(narrow_taps % 2, 1);
        assert_eq!(wide_taps % 2, 1);
        assert!(real.taps().iter().all(|tap| tap.is_finite()));
        assert!(complex.taps().iter().all(|tap| tap.is_finite()));
    }

    #[test]
    fn real_fir_impulse_response_is_symmetric() {
        let spec = voice_spec();
        let mut fir = RealFir::design(spec).expect("valid voice-channel spec designs");
        let taps_len = fir.taps_len();
        let mut impulse = vec![0.0_f32; taps_len];
        impulse[0] = 1.0;
        let response = fir.process(&impulse);
        assert_eq!(response.len(), taps_len);

        for index in 0..taps_len / 2 {
            let mirror = taps_len - 1 - index;
            assert_abs_diff_eq!(
                f64::from(response[index]),
                f64::from(response[mirror]),
                epsilon = 1e-6
            );
        }
        let tap_sum: f64 = fir.taps().iter().map(|tap| f64::from(*tap)).sum();
        assert_abs_diff_eq!(tap_sum, 1.0, epsilon = 1e-4);
    }

    #[test]
    fn real_fir_processed_output_length_equals_input_length() {
        let spec = voice_spec();
        let mut fir = RealFir::design(spec).expect("valid voice-channel spec designs");
        for size in [0_usize, 1, 7, 33, 1_000, 4_096] {
            let input = vec![0.25_f32; size];
            let output = fir.process(&input);
            assert_eq!(output.len(), size, "block of {size} samples changed length");
        }

        // Streaming N samples as arbitrary blocks yields exactly N output samples.
        let total = 3_000;
        let mut streamed = 0;
        for &size in &[0, 1, 257, 1_024, 1_718] {
            let input = vec![0.1_f32; size];
            streamed += fir.process(&input).len();
        }
        assert_eq!(streamed, total);
    }

    #[test]
    fn complex_fir_processing_does_not_change_iq_gain_in_passband() {
        let spec = voice_spec();
        let mut fir = ComplexFir::design(spec).expect("valid voice-channel spec designs");
        let freq_hz = 1_500.0;
        let input = complex_tone(8_192, freq_hz, spec.sample_rate_hz, 0.0);
        let settled = {
            let settled = fir.taps_len() * 2;
            assert!(
                settled < input.len() / 2,
                "a {} tap design leaves no steady state in a {} sample window",
                fir.taps_len(),
                input.len()
            );
            settled
        };
        let output = fir.process(&input);

        let sample_count = output.len() - settled;
        let gain: f64 = output[settled..]
            .iter()
            .map(|sample| f64::from(sample.norm()))
            .sum::<f64>()
            / sample_count as f64;
        assert_abs_diff_eq!(gain, 1.0, epsilon = 1e-3);
    }
}
