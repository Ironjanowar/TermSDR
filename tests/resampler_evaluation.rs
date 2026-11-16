//! Candidate-neutral dependency-gate contract for the RF-to-audio I/Q resampler.
//!
//! The tests intentionally name only TermSDR's project-owned API. Candidate crate APIs must stay
//! behind that production boundary so selecting or replacing a backend never changes this file.

use std::f64::consts::PI;
use std::time::Instant;

use termsdr::dsp::resampler::{create_complex_resampler, ComplexResampler};
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

fn randomized_partitions(total: usize) -> Vec<usize> {
    // Fixed seed and integer LCG make this workload reproducible without another dependency.
    let mut state = 0x6d2b_79f5_u32;
    let mut remaining = total;
    let mut partitions = Vec::new();
    while remaining > 0 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let size = 1 + usize::try_from(state % 4096).expect("partition fits usize");
        let size = size.min(remaining);
        partitions.push(size);
        remaining -= size;
    }
    partitions
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

fn rms_difference(
    left: &[IqSample],
    right: &[IqSample],
    component: impl Fn(IqSample) -> f32,
) -> f64 {
    assert_eq!(left.len(), right.len(), "aligned lengths differ");
    assert!(!left.is_empty(), "RMS comparison requires output samples");
    let sum_squared = left
        .iter()
        .zip(right)
        .map(|(&a, &b)| {
            let difference = f64::from(component(a)) - f64::from(component(b));
            difference * difference
        })
        .sum::<f64>();
    (sum_squared / left.len() as f64).sqrt()
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
        let error = output.len().abs_diff(expected);
        assert!(
            error <= 1,
            "{input_rate_hz} Hz -> {OUTPUT_RATE_HZ} Hz produced {} samples; expected {expected} ±1",
            output.len()
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
