//! Candidate-neutral dependency-gate contract for the RF-to-audio I/Q resampler.
//!
//! The tests intentionally name only TermSDR's project-owned API. Candidate crate APIs must stay
//! behind that production boundary so selecting or replacing a backend never changes this file.

use std::f64::consts::PI;
use std::time::Instant;

use proptest::prelude::*;

use termsdr::dsp::resampler::{
    create_complex_resampler, create_decimated_real_resampler, create_real_resampler,
    ComplexResampler, RealResampler, ResamplerCreationError,
};
use termsdr::types::IqSample;

const OUTPUT_RATE_HZ: u32 = 48_000;
const RMS_LIMIT: f64 = 1.0e-5;
// Spectral quality is measured after a fixed window because a finite stream starts with unknown
// filter prehistory. That startup transient is not an I/Q synchronization defect, and this window
// deliberately remains independent of the adapter's reported residual latency.
const QUALITY_SETTLING_OUTPUT_SAMPLES: usize = 128;

#[derive(Debug)]
struct IqOutput {
    samples: Vec<IqSample>,
    latency: usize,
}

fn selected_resampler(input_rate_hz: u32) -> impl ComplexResampler {
    let result: anyhow::Result<_> = create_complex_resampler(input_rate_hz, OUTPUT_RATE_HZ);
    let resampler = result.expect("dependency-gate rates must produce a valid resampler");
    assert_eq!(resampler.input_rate(), input_rate_hz);
    assert_eq!(resampler.output_rate(), OUTPUT_RATE_HZ);
    resampler
}

fn push_partition(
    resampler: &mut impl ComplexResampler,
    input: &[IqSample],
    output: &mut Vec<IqSample>,
) {
    let result: anyhow::Result<()> = resampler.process(input, output);
    result.expect("candidate adapter rejected a valid input partition");
}

fn finish(resampler: &mut impl ComplexResampler, output: &mut Vec<IqSample>) {
    let result: anyhow::Result<()> = resampler.flush(output);
    result.expect("candidate adapter failed to flush delayed output at EOF");
}

fn resample_partitions(
    input_rate_hz: u32,
    input: &[IqSample],
    partitions: impl Iterator<Item = usize>,
) -> IqOutput {
    let mut resampler = selected_resampler(input_rate_hz);
    let latency = resampler.latency_output_samples();
    let expected_output = input
        .len()
        .saturating_mul(OUTPUT_RATE_HZ as usize)
        .div_ceil(input_rate_hz as usize);
    let mut output = Vec::with_capacity(expected_output + 1024);
    let mut offset = 0;

    for requested in partitions {
        if offset == input.len() {
            break;
        }
        assert!(requested > 0, "partitions must contain at least one sample");
        let end = offset.saturating_add(requested).min(input.len());
        push_partition(&mut resampler, &input[offset..end], &mut output);
        offset = end;
    }
    assert_eq!(
        offset,
        input.len(),
        "partition sequence did not cover input"
    );
    finish(&mut resampler, &mut output);

    IqOutput {
        samples: output,
        latency,
    }
}

fn fixed_partitions(total: usize, block_size: usize) -> impl Iterator<Item = usize> {
    std::iter::repeat(block_size).take(total.div_ceil(block_size))
}

fn seeded_partitions(total: usize, seed: u32, max_size: usize) -> Vec<usize> {
    // Fixed seed and integer LCG make this workload reproducible without another dependency.
    let mut state = seed | 1;
    let mut remaining = total;
    let mut partitions = Vec::new();
    while remaining > 0 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let span = u32::try_from(max_size).expect("partition cap fits u32");
        let size = 1 + usize::try_from(state % span).expect("partition fits usize");
        let size = size.min(remaining);
        partitions.push(size);
        remaining -= size;
    }
    partitions
}

fn randomized_partitions(total: usize) -> Vec<usize> {
    seeded_partitions(total, 0x6d2b_79f5, 4096)
}

fn complex_tone(sample_rate_hz: u32, frequency_hz: f64, samples: usize) -> Vec<IqSample> {
    let angular_step = 2.0 * PI * frequency_hz / f64::from(sample_rate_hz);
    let mut output = Vec::with_capacity(samples);
    for index in 0..samples {
        #[allow(clippy::cast_precision_loss)]
        let phase = angular_step * index as f64;
        #[allow(clippy::cast_possible_truncation)]
        output.push(IqSample::new(phase.cos() as f32, phase.sin() as f32));
    }
    output
}

fn deterministic_multitone(sample_rate_hz: u32, samples: usize) -> Vec<IqSample> {
    let mut output = complex_tone(sample_rate_hz, 1_137.0, samples);
    let second = complex_tone(sample_rate_hz, 7_913.0, samples);
    for (sample, other) in output.iter_mut().zip(second) {
        *sample = 0.7 * *sample + 0.2 * other;
    }
    output
}

fn real_tone(sample_rate_hz: u32, frequency_hz: f64, samples: usize) -> Vec<f32> {
    complex_tone(sample_rate_hz, frequency_hz, samples)
        .into_iter()
        .map(|sample| sample.re)
        .collect()
}

#[allow(clippy::cast_precision_loss)]
fn rms_of_differences(
    left_len: usize,
    right_len: usize,
    pairs: impl Iterator<Item = (f32, f32)>,
) -> f64 {
    assert_eq!(left_len, right_len, "aligned lengths differ");
    assert!(left_len > 0, "RMS comparison requires output samples");
    let sum_squared = pairs
        .map(|(left, right)| {
            let difference = f64::from(left) - f64::from(right);
            difference * difference
        })
        .sum::<f64>();
    (sum_squared / left_len as f64).sqrt()
}

fn real_rms_difference(left: &[f32], right: &[f32]) -> f64 {
    rms_of_differences(
        left.len(),
        right.len(),
        left.iter().zip(right).map(|(&a, &b)| (a, b)),
    )
}

fn resample_real_partitions(
    input_rate_hz: u32,
    input: &[f32],
    partitions: impl Iterator<Item = usize>,
) -> Vec<f32> {
    let result: anyhow::Result<_> = create_real_resampler(input_rate_hz, OUTPUT_RATE_HZ);
    let mut resampler = result.expect("dependency-gate rates must produce a real resampler");
    let mut output = Vec::new();
    let mut offset = 0;
    for requested in partitions {
        if offset == input.len() {
            break;
        }
        assert!(requested > 0, "partitions must contain at least one sample");
        let end = offset.saturating_add(requested).min(input.len());
        resampler
            .process(&input[offset..end], &mut output)
            .expect("real adapter rejected a valid input partition");
        offset = end;
    }
    assert_eq!(
        offset,
        input.len(),
        "partition sequence did not cover input"
    );
    resampler
        .flush(&mut output)
        .expect("real adapter failed to flush delayed output at EOF");
    output
}

fn rms_difference(
    left: &[IqSample],
    right: &[IqSample],
    component: impl Fn(IqSample) -> f32,
) -> f64 {
    rms_of_differences(
        left.len(),
        right.len(),
        left.iter()
            .zip(right)
            .map(|(&a, &b)| (component(a), component(b))),
    )
}

fn tone_coefficient(
    samples: &[IqSample],
    frequency_hz: f64,
    component: impl Fn(IqSample) -> f32,
) -> (f64, f64) {
    let angular_step = 2.0 * PI * frequency_hz / f64::from(OUTPUT_RATE_HZ);
    let (real, imaginary) =
        samples
            .iter()
            .enumerate()
            .fold((0.0, 0.0), |(real, imaginary), (index, &sample)| {
                #[allow(clippy::cast_precision_loss)]
                let phase = angular_step * index as f64;
                (
                    real + f64::from(component(sample)) * phase.cos(),
                    imaginary - f64::from(component(sample)) * phase.sin(),
                )
            });
    let normalization = 2.0 / samples.len() as f64;
    (real * normalization, imaginary * normalization)
}

fn magnitude(coefficient: (f64, f64)) -> f64 {
    coefficient.0.hypot(coefficient.1)
}

fn phase(coefficient: (f64, f64)) -> f64 {
    coefficient.1.atan2(coefficient.0)
}

fn wrap_phase(mut radians: f64) -> f64 {
    while radians > PI {
        radians -= 2.0 * PI;
    }
    while radians < -PI {
        radians += 2.0 * PI;
    }
    radians
}

#[test]
fn required_receiver_rates_are_accepted() {
    for input_rate_hz in [2_048_000, 1_024_000] {
        let result = create_complex_resampler(input_rate_hz, OUTPUT_RATE_HZ);
        assert!(
            result.is_ok(),
            "required {input_rate_hz} Hz -> {OUTPUT_RATE_HZ} Hz rate pair was rejected"
        );
    }
}

#[test]
fn compensated_stream_reports_zero_residual_latency() {
    for input_rate_hz in [2_048_000, 1_024_000] {
        let resampler = selected_resampler(input_rate_hz);
        assert_eq!(
            resampler.latency_output_samples(),
            0,
            "returned stream has startup padding removed, so no residual latency remains"
        );
    }
}

#[test]
fn empty_input_then_flush_produces_no_output() {
    let mut resampler = selected_resampler(2_048_000);
    let mut output = Vec::new();

    push_partition(&mut resampler, &[], &mut output);
    finish(&mut resampler, &mut output);

    assert!(output.is_empty());
}

#[test]
fn one_input_sample_produces_one_ceil_rounded_output_sample() {
    let input_rate_hz = 2_048_000;
    let mut resampler = selected_resampler(input_rate_hz);
    let mut output = Vec::new();

    let input = [IqSample::new(1.0, -1.0)];
    push_partition(&mut resampler, &input, &mut output);
    finish(&mut resampler, &mut output);

    let expected = input
        .len()
        .saturating_mul(OUTPUT_RATE_HZ as usize)
        .div_ceil(input_rate_hz as usize);
    assert_eq!(expected, 1, "ceil formula must retain a non-empty stream");
    assert_eq!(output.len(), expected);
}

#[test]
fn non_integral_output_count_is_rounded_up() {
    let input_rate_hz = 2_048_000;
    let input = vec![IqSample::new(0.0, 0.0); 43];
    let mut resampler = selected_resampler(input_rate_hz);
    let mut output = Vec::new();

    push_partition(&mut resampler, &input, &mut output);
    finish(&mut resampler, &mut output);

    let expected = input
        .len()
        .saturating_mul(OUTPUT_RATE_HZ as usize)
        .div_ceil(input_rate_hz as usize);
    assert_eq!(
        expected, 2,
        "test setup must exercise non-integral rounding"
    );
    assert_eq!(output.len(), expected);
}

#[test]
fn second_flush_is_rejected() {
    let mut resampler = selected_resampler(2_048_000);
    let mut output = Vec::new();

    finish(&mut resampler, &mut output);
    let second_flush: anyhow::Result<()> = resampler.flush(&mut output);

    assert!(second_flush.is_err());
}

#[test]
fn process_after_flush_is_rejected() {
    let mut resampler = selected_resampler(2_048_000);
    let mut output = Vec::new();

    finish(&mut resampler, &mut output);
    let process_after_flush: anyhow::Result<()> =
        resampler.process(&[IqSample::new(0.0, 0.0)], &mut output);

    assert!(process_after_flush.is_err());
}

#[test]
fn nearly_coprime_rate_pair_is_rejected() {
    let result = create_complex_resampler(65_537, OUTPUT_RATE_HZ);

    assert!(
        result.is_err(),
        "nearly-coprime rates must be rejected before backend allocation"
    );
}

#[test]
fn ten_seconds_produces_exact_rational_output_count() {
    for input_rate_hz in [2_048_000_u32, 1_024_000_u32] {
        let input_samples = usize::try_from(input_rate_hz).expect("rate fits usize") * 10;
        let mut resampler = selected_resampler(input_rate_hz);
        let mut output = Vec::with_capacity(OUTPUT_RATE_HZ as usize * 10 + 1024);
        let zeros = vec![IqSample::new(0.0, 0.0); 4096];
        let mut remaining = input_samples;
        while remaining > 0 {
            let chunk = remaining.min(zeros.len());
            push_partition(&mut resampler, &zeros[..chunk], &mut output);
            remaining -= chunk;
        }
        finish(&mut resampler, &mut output);

        let expected = usize::try_from(OUTPUT_RATE_HZ).expect("rate fits usize") * 10;
        assert_eq!(
            output.len(),
            expected,
            "{input_rate_hz} Hz -> {OUTPUT_RATE_HZ} Hz must produce exactly {expected} samples"
        );
    }
}

#[test]
fn one_khz_complex_tone_frequency_is_accurate() {
    // At 48 kHz a 1 kHz tone repeats every 48 output samples, so a 480-cycle window is 23,040
    // samples long and contains an integer number of cycles. Two consecutive windows therefore
    // give leakage-free coefficients at the nominal frequency, and the coefficient phase
    // difference across the contiguous windows is the frequency offset times the window spacing.
    // Phase differencing over T = 0.48 s is unambiguous only within +/-1/(2T) = +/-1.0417 Hz, so
    // the +/-1 Hz tolerance uses 96% of the resolvable half-range; the gain assertion below is the
    // sharper detector, since a 480-cycle window loses ~1.6e-3 Hz of apparent frequency per unit
    // of gain error.
    const WINDOW_SAMPLES: usize = 23_040;
    const WINDOW_SECONDS: f64 = 23_040.0 / 48_000.0;
    let input_rate_hz = 2_048_000;
    let tone_hz = 1_000.0;
    let sample_count = usize::try_from(input_rate_hz).expect("rate fits usize");
    let input = complex_tone(input_rate_hz, tone_hz, sample_count);
    let output = resample_partitions(input_rate_hz, &input, fixed_partitions(sample_count, 2048));

    let start = QUALITY_SETTLING_OUTPUT_SAMPLES;
    assert!(
        start + 2 * WINDOW_SAMPLES <= output.samples.len(),
        "one second of output must hold two full 480-cycle windows"
    );
    let first = tone_coefficient(
        &output.samples[start..start + WINDOW_SAMPLES],
        tone_hz,
        |sample| sample.re,
    );
    let second = tone_coefficient(
        &output.samples[start + WINDOW_SAMPLES..start + 2 * WINDOW_SAMPLES],
        tone_hz,
        |sample| sample.re,
    );

    let gain = f64::midpoint(magnitude(first), magnitude(second));
    let phase_offset = wrap_phase(phase(second) - phase(first));
    let measured_hz = tone_hz + phase_offset / (2.0 * PI * WINDOW_SECONDS);

    assert!(
        (measured_hz - tone_hz).abs() <= 1.0,
        "1 kHz tone measured at {measured_hz:.6} Hz; require |error| <= 1 Hz"
    );
    assert!(
        (gain - 1.0).abs() <= 1.0e-6,
        "1 kHz tone gain measured at {gain:.9}; require |gain - 1| <= 1e-6"
    );
}

#[test]
fn real_resampler_flush_lifecycle() {
    let result: anyhow::Result<_> = create_real_resampler(2_048_000, OUTPUT_RATE_HZ);
    let mut resampler = result.expect("real resampler must be created for a required rate pair");
    let mut output = Vec::new();

    resampler
        .process(&[], &mut output)
        .expect("empty real partition must be accepted");
    resampler
        .flush(&mut output)
        .expect("first real flush must succeed");
    assert!(
        output.is_empty(),
        "empty real input must not synthesize output"
    );

    let second_flush = resampler.flush(&mut output);
    assert!(second_flush.is_err(), "second real flush must be rejected");

    let process_after_flush = resampler.process(&[0.25], &mut output);
    assert!(
        process_after_flush.is_err(),
        "real processing after flush must be rejected"
    );
}

#[test]
fn real_resampler_output_count_is_ceiled_not_truncated() {
    // 1001 input frames at 44.1 kHz -> 48 kHz is 1089.52 output samples, so the
    // exact duration must round up to 1090. Truncating would silently shorten
    // the stream by one sample and is detectable only on a non-integral ratio.
    const INPUT_RATE_HZ: u32 = 44_100;
    const INPUT_FRAMES: usize = 1_001;
    let result: anyhow::Result<_> = create_real_resampler(INPUT_RATE_HZ, OUTPUT_RATE_HZ);
    let mut resampler = result.expect("44.1 kHz to 48 kHz must be a supported real rate pair");
    let mut output = Vec::new();
    let input = real_tone(INPUT_RATE_HZ, 1_000.0, INPUT_FRAMES);
    resampler
        .process(&input, &mut output)
        .expect("real partition must be accepted");
    resampler
        .flush(&mut output)
        .expect("real flush must succeed");

    let expected = (INPUT_FRAMES * usize::try_from(OUTPUT_RATE_HZ).expect("rate fits usize"))
        .div_ceil(usize::try_from(INPUT_RATE_HZ).expect("rate fits usize"));
    assert_eq!(
        expected, 1_090,
        "test setup must exercise a non-integral ratio"
    );
    assert_eq!(
        output.len(),
        expected,
        "real stream length must be the ceiling of the exact rational duration"
    );
}

#[test]
fn decimated_rate_overflow_is_a_typed_error() {
    let result: anyhow::Result<_> = create_decimated_real_resampler(1_024_000, 48_000, 100_000);
    let error = result
        .err()
        .expect("scaling 48 kHz by 100,000 must overflow the backend rate");
    let typed = error.downcast_ref::<ResamplerCreationError>();
    assert!(
        typed.is_some_and(|candidate| matches!(
            candidate,
            ResamplerCreationError::DecimatedRateOverflow {
                output_rate_hz: OUTPUT_RATE_HZ,
                decimation_factor: 100_000
            }
        )),
        "decimated rate overflow must be a typed ResamplerCreationError::DecimatedRateOverflow; got {error:?}"
    );
}

#[test]
fn decimated_real_resampler_reports_exact_source_duration() {
    const RF_RATE_HZ: u32 = 1_024_000;
    const DECIMATION_FACTOR: u32 = 8;
    const SOURCE_FRAMES: usize = 1_024_000;
    let result: anyhow::Result<_> =
        create_decimated_real_resampler(RF_RATE_HZ, OUTPUT_RATE_HZ, DECIMATION_FACTOR);
    let mut resampler = result.expect("decimated resampler must accept a required rate pair");
    let mut output = Vec::new();

    let decimation = usize::try_from(DECIMATION_FACTOR).expect("factor fits usize");
    let mut remaining_decimated = SOURCE_FRAMES / decimation;
    let pattern = [1000_usize, 4096, 777];
    let mut index = 0;
    while remaining_decimated > 0 {
        let requested = pattern[index % pattern.len()];
        index += 1;
        let decimated = requested.min(remaining_decimated);
        remaining_decimated -= decimated;
        let input = vec![0.5_f32; decimated];
        resampler
            .process_with_source_frames(&input, decimated * decimation, &mut output)
            .expect("decimated partition must be accepted");
    }
    resampler
        .flush(&mut output)
        .expect("decimated flush must succeed");

    let expected = (SOURCE_FRAMES * usize::try_from(OUTPUT_RATE_HZ).expect("rate fits usize"))
        .div_ceil(usize::try_from(RF_RATE_HZ).expect("rate fits usize"));
    assert_eq!(
        expected, 48_000,
        "test setup must expect one second of output"
    );
    assert_eq!(
        output.len(),
        expected,
        "decimated stream duration must follow the RF clock exactly"
    );
}

#[test]
fn real_resampler_partition_invariance() {
    let input_rate_hz = 1_024_000;
    let sample_count = 40_000;
    let input = real_tone(input_rate_hz, 1_000.0, sample_count);
    let reference = resample_real_partitions(input_rate_hz, &input, std::iter::once(sample_count));

    for block_size in [1_usize, 257, 4096, 8192] {
        let candidate = resample_real_partitions(
            input_rate_hz,
            &input,
            fixed_partitions(sample_count, block_size),
        );
        assert_eq!(
            candidate.len(),
            reference.len(),
            "block {block_size}: real output count changed by partition"
        );
        let rms = real_rms_difference(&reference, &candidate);
        assert!(
            rms <= RMS_LIMIT,
            "block {block_size}: real partition RMS {rms:.3e} exceeds {RMS_LIMIT:.1e}"
        );
    }

    let random = seeded_partitions(sample_count, 0x9e37_79b9, 8192);
    let candidate = resample_real_partitions(input_rate_hz, &input, random.iter().copied().cycle());
    assert_eq!(candidate.len(), reference.len());
    let rms = real_rms_difference(&reference, &candidate);
    assert!(
        rms <= RMS_LIMIT,
        "random partitions: real RMS {rms:.3e} exceeds {RMS_LIMIT:.1e}"
    );
}

#[test]
fn zero_decimation_factor_is_a_typed_error() {
    let result: anyhow::Result<_> = create_decimated_real_resampler(1_024_000, OUTPUT_RATE_HZ, 0);
    let error = result
        .err()
        .expect("a zero decimation factor must be rejected before backend construction");
    let typed = error.downcast_ref::<ResamplerCreationError>();
    assert!(
        typed.is_some_and(|candidate| matches!(
            candidate,
            ResamplerCreationError::ZeroDecimationFactor {
                decimation_factor: 0
            }
        )),
        "zero decimation must be a typed ResamplerCreationError::ZeroDecimationFactor; got {error:?}"
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 16, ..ProptestConfig::default() })]

    #[test]
    fn complex_resampler_output_length_and_shape_are_exact_for_arbitrary_partitions(
        rate_index in 0usize..4,
        total in 1usize..=200_000,
        partitions in prop::collection::vec(1usize..=8192, 1..64),
    ) {
        let input_rate_hz = [2_048_000_u32, 1_024_000, 960_000, 44_100][rate_index];
        let input = deterministic_multitone(input_rate_hz, total);
        let expected = total
            .saturating_mul(OUTPUT_RATE_HZ as usize)
            .div_ceil(input_rate_hz as usize);

        let reference = resample_partitions(input_rate_hz, &input, std::iter::once(total));
        prop_assert_eq!(reference.samples.len(), expected);

        let candidate =
            resample_partitions(input_rate_hz, &input, partitions.iter().copied().cycle());
        prop_assert_eq!(candidate.samples.len(), expected);
        prop_assert_eq!(candidate.latency, reference.latency);

        let i_rms = rms_difference(&reference.samples, &candidate.samples, |sample| sample.re);
        let q_rms = rms_difference(&reference.samples, &candidate.samples, |sample| sample.im);
        prop_assert!(
            i_rms <= RMS_LIMIT && q_rms <= RMS_LIMIT,
            "{} Hz -> {} Hz, {} partitions: RMS differs (I={:.3e}, Q={:.3e})",
            input_rate_hz,
            OUTPUT_RATE_HZ,
            partitions.len(),
            i_rms,
            q_rms
        );
    }
}

#[test]
fn compensated_output_is_partition_invariant_from_sample_zero() {
    let input_rate_hz = 2_048_000;
    let sample_count = usize::try_from(input_rate_hz).expect("rate fits usize");
    let input = deterministic_multitone(input_rate_hz, sample_count);
    let reference = resample_partitions(
        input_rate_hz,
        &input,
        fixed_partitions(sample_count, sample_count),
    );

    let fixed_sizes = [257, 1024, 2048, 4096];
    for block_size in fixed_sizes {
        let candidate = resample_partitions(
            input_rate_hz,
            &input,
            fixed_partitions(sample_count, block_size),
        );
        assert_eq!(
            candidate.latency, reference.latency,
            "latency changed by partition"
        );
        assert_eq!(
            candidate.samples.len(),
            reference.samples.len(),
            "complex output count changed by partition"
        );
        let i_rms = rms_difference(&reference.samples, &candidate.samples, |sample| sample.re);
        let q_rms = rms_difference(&reference.samples, &candidate.samples, |sample| sample.im);
        assert!(
            i_rms <= RMS_LIMIT && q_rms <= RMS_LIMIT,
            "block {block_size}: partition RMS differs (I={i_rms:.3e}, Q={q_rms:.3e}, limit={RMS_LIMIT:.1e})"
        );
    }

    let random = randomized_partitions(sample_count);
    let candidate = resample_partitions(input_rate_hz, &input, random.into_iter());
    assert_eq!(candidate.latency, reference.latency);
    assert_eq!(candidate.samples.len(), reference.samples.len());
    let i_rms = rms_difference(&reference.samples, &candidate.samples, |sample| sample.re);
    let q_rms = rms_difference(&reference.samples, &candidate.samples, |sample| sample.im);
    assert!(
        i_rms <= RMS_LIMIT && q_rms <= RMS_LIMIT,
        "random partitions: RMS differs (I={i_rms:.3e}, Q={q_rms:.3e}, limit={RMS_LIMIT:.1e})"
    );
}

#[test]
fn synchronized_channels_preserve_iq_quadrature_and_gain() {
    let input_rate_hz = 2_048_000;
    let tone_hz = 3_000.0;
    let sample_count = usize::try_from(input_rate_hz).expect("rate fits usize");
    let input = complex_tone(input_rate_hz, tone_hz, sample_count);
    let output = resample_partitions(input_rate_hz, &input, fixed_partitions(sample_count, 2048));
    let settled = &output.samples[QUALITY_SETTLING_OUTPUT_SAMPLES..];
    let i_coefficient = tone_coefficient(settled, tone_hz, |sample| sample.re);
    let q_coefficient = tone_coefficient(settled, tone_hz, |sample| sample.im);
    let gain_mismatch = (magnitude(i_coefficient) - magnitude(q_coefficient)).abs();
    let phase_error = wrap_phase(phase(q_coefficient) - phase(i_coefficient) + PI / 2.0).abs();

    assert!(
        gain_mismatch <= 1.0e-5,
        "I/Q gain mismatch {gain_mismatch:.3e}"
    );
    assert!(
        phase_error <= 1.0e-5,
        "I/Q quadrature phase error {phase_error:.3e} rad"
    );
}

#[test]
fn stopband_sweep_rejects_aliases_into_voice_band_by_sixty_db() {
    let input_rate_hz = 2_048_000;
    let voice_frequency_hz = 3_000.0;
    let sample_count = usize::try_from(input_rate_hz).expect("rate fits usize") / 4;
    let pass_input = complex_tone(input_rate_hz, voice_frequency_hz, sample_count);
    let pass = resample_partitions(
        input_rate_hz,
        &pass_input,
        fixed_partitions(sample_count, 2048),
    );
    let pass_amplitude = magnitude(tone_coefficient(
        &pass.samples[QUALITY_SETTLING_OUTPUT_SAMPLES..],
        voice_frequency_hz,
        |sample| sample.re,
    ));

    for alias_multiple in [1.0, 5.0, 10.0, 15.0, 20.0] {
        let alias_source_hz = alias_multiple * f64::from(OUTPUT_RATE_HZ) + voice_frequency_hz;
        let stop_input = complex_tone(input_rate_hz, alias_source_hz, sample_count);
        let stop = resample_partitions(
            input_rate_hz,
            &stop_input,
            fixed_partitions(sample_count, 2048),
        );
        let alias_amplitude = magnitude(tone_coefficient(
            &stop.samples[QUALITY_SETTLING_OUTPUT_SAMPLES..],
            voice_frequency_hz,
            |sample| sample.re,
        ));
        let attenuation_db = 20.0 * (alias_amplitude / pass_amplitude).log10();

        assert!(
            attenuation_db <= -60.0,
            "{alias_source_hz} Hz aliases to {voice_frequency_hz} Hz at {attenuation_db:.2} dB; require <= -60 dB"
        );
    }
}

#[test]
#[ignore = "deterministic release-only host measurement; run with --release --ignored --nocapture"]
fn one_second_processing_uses_less_than_half_of_one_cpu_core() {
    assert!(
        !cfg!(debug_assertions),
        "performance gate must be measured with cargo test --release"
    );
    let input_rate_hz = 2_048_000;
    let sample_count = usize::try_from(input_rate_hz).expect("rate fits usize");
    let input = deterministic_multitone(input_rate_hz, sample_count);
    let started = Instant::now();
    let output = resample_partitions(input_rate_hz, &input, fixed_partitions(sample_count, 2048));
    let elapsed = started.elapsed();
    let core_fraction = elapsed.as_secs_f64();
    println!(
        "resampler_perf input_complex_samples={sample_count} output_complex_samples={} elapsed_ms={:.3} one_core_fraction={core_fraction:.6}",
        output.samples.len(),
        elapsed.as_secs_f64() * 1000.0
    );
    assert!(
        core_fraction < 0.5,
        "one second of 2.048 Msps input consumed {:.2}% of one CPU core; require <50%",
        core_fraction * 100.0
    );
}
