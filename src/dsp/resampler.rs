//! Stateful complex sample-rate conversion.
//!
//! The project-owned interface in this module keeps the selected resampling backend private and
//! presents complex I/Q as two synchronized real-valued channels.

use anyhow::{anyhow, Context, Result};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use thiserror::Error;

use crate::types::IqSample;

const INPUT_CHUNK_FRAMES: usize = 4096;
// Rubato rounds fixed-both FFT blocks to a multiple of the reduced rate components. Limiting
// either side to sixteen staging chunks keeps construction allocations bounded while admitting
// the receiver rates and conventional audio-rate conversions (including 44.1 <-> 48 kHz).
const MAX_BACKEND_BLOCK_FRAMES: usize = INPUT_CHUNK_FRAMES * 16;
const CHANNELS: usize = 2;

struct BackendLayout {
    input_rate: usize,
    output_rate: usize,
}

fn validate_rate_pair(input_rate_hz: u32, output_rate_hz: u32) -> Result<BackendLayout> {
    if input_rate_hz == 0 || output_rate_hz == 0 {
        return Err(ResamplerCreationError::ZeroRate {
            input_rate_hz,
            output_rate_hz,
        }
        .into());
    }

    let gcd = greatest_common_divisor(input_rate_hz, output_rate_hz);
    let reduced_input = usize::try_from(input_rate_hz / gcd)
        .context("reduced input sample rate does not fit this platform's usize")?;
    let reduced_output = usize::try_from(output_rate_hz / gcd)
        .context("reduced output sample rate does not fit this platform's usize")?;
    let backend_chunks = INPUT_CHUNK_FRAMES.div_ceil(reduced_input);
    let input_block_frames = reduced_input
        .checked_mul(backend_chunks)
        .context("resampler input block size overflow")?;
    let output_block_frames = reduced_output
        .checked_mul(backend_chunks)
        .context("resampler output block size overflow")?;
    if input_block_frames > MAX_BACKEND_BLOCK_FRAMES
        || output_block_frames > MAX_BACKEND_BLOCK_FRAMES
    {
        return Err(ResamplerCreationError::BackendBlockTooLarge {
            input_rate_hz,
            output_rate_hz,
            input_block_frames,
            output_block_frames,
            max_block_frames: MAX_BACKEND_BLOCK_FRAMES,
        }
        .into());
    }

    Ok(BackendLayout {
        input_rate: usize::try_from(input_rate_hz)
            .context("input sample rate does not fit this platform's usize")?,
        output_rate: usize::try_from(output_rate_hz)
            .context("output sample rate does not fit this platform's usize")?,
    })
}

/// Errors detected before constructing the resampling backend.
#[derive(Debug, Error)]
pub enum ResamplerCreationError {
    /// One or both sample rates were zero.
    #[error("resampler rates must be non-zero (input={input_rate_hz}, output={output_rate_hz})")]
    ZeroRate {
        /// Requested input sample rate.
        input_rate_hz: u32,
        /// Requested output sample rate.
        output_rate_hz: u32,
    },
    /// The reduced ratio would make Rubato allocate an excessive fixed FFT block.
    #[error(
        "resampler rate pair {input_rate_hz} Hz -> {output_rate_hz} Hz requires backend blocks of {input_block_frames} input and {output_block_frames} output frames; maximum is {max_block_frames}"
    )]
    BackendBlockTooLarge {
        /// Requested input sample rate.
        input_rate_hz: u32,
        /// Requested output sample rate.
        output_rate_hz: u32,
        /// Input frames in the block Rubato would construct.
        input_block_frames: usize,
        /// Output frames in the block Rubato would construct.
        output_block_frames: usize,
        /// Maximum accepted block size on either side.
        max_block_frames: usize,
    },
    /// The decimation factor was zero, so no RF frame could produce a backend sample.
    #[error("decimation factor must be non-zero to decimate an RF stream")]
    ZeroDecimationFactor {
        /// Requested decimation factor.
        decimation_factor: u32,
    },
    /// Scaling the sink rate by the decimation factor overflowed `u32`.
    #[error(
        "decimated resampler cannot scale {output_rate_hz} Hz output by decimation factor {decimation_factor}"
    )]
    DecimatedRateOverflow {
        /// Requested output sample rate.
        output_rate_hz: u32,
        /// Integer decimation factor applied before resampling.
        decimation_factor: u32,
    },
}

/// A stateful resampler for complex I/Q samples.
///
/// Calls to [`process`](Self::process) may contain any positive or empty partition size. Input is
/// staged internally until the fixed-rate backend can consume it. Call [`flush`](Self::flush)
/// exactly once at end of stream to emit the final partial block and delayed filter tail.
pub trait ComplexResampler {
    /// Returns the configured input sample rate in hertz.
    fn input_rate(&self) -> u32;

    /// Returns the configured output sample rate in hertz.
    fn output_rate(&self) -> u32;

    /// Returns residual latency in the samples appended by this adapter.
    ///
    /// Backend startup delay is removed internally, so compensated output reports
    /// zero. Removal is exact when the backend output block length
    /// (`fft_size_out`) is even, because its startup delay is then an integer
    /// number of output samples. When `fft_size_out` is odd the delay is
    /// `fft_size_out / 2` truncated, leaving a residual time offset of exactly
    /// 0.5 output samples. Parity depends on both rates, not the input rate
    /// alone: 2,048,000 -> 48,000 and 1,024,000 -> 48,000 are exact (96 and 192
    /// frames), 44,100 -> 48,000 is also exact (4480), while 2,048,000 ->
    /// 44,100 and 960,000 -> 48,000 leave 0.5 samples (441 and 205). The
    /// reported value is zero regardless: it describes this adapter's own
    /// buffering, not the backend's residual offset.
    fn latency_output_samples(&self) -> usize;

    /// Stages and resamples an arbitrary input partition, appending results to `output`.
    ///
    /// Existing elements and capacity in `output` are preserved and reused.
    ///
    /// # Errors
    ///
    /// Returns an error after EOF flush, on a length overflow, or if backend processing fails.
    fn process(&mut self, input: &[IqSample], output: &mut Vec<IqSample>) -> Result<()>;

    /// Finishes the stream, appending the delayed tail and exact-duration output to `output`.
    ///
    /// # Errors
    ///
    /// Returns an error when called more than once, on a length overflow, or if backend processing
    /// fails.
    fn flush(&mut self, output: &mut Vec<IqSample>) -> Result<()>;
}

/// Creates a fixed-rate complex resampler.
///
/// # Errors
///
/// Returns an error when either rate is zero, the reduced rate ratio would require a backend block
/// above 65,536 frames, a rate cannot be represented on this platform, or the rubato FFT backend
/// rejects the requested rate pair. The block-size check occurs before backend construction and
/// allocation.
pub fn create_complex_resampler(
    input_rate_hz: u32,
    output_rate_hz: u32,
) -> Result<impl ComplexResampler> {
    let layout = validate_rate_pair(input_rate_hz, output_rate_hz)?;
    let backend = Fft::<f32>::new(
        layout.input_rate,
        layout.output_rate,
        INPUT_CHUNK_FRAMES,
        CHANNELS,
        FixedSync::Both,
    )
    .with_context(|| {
        format!(
            "failed to create rubato FFT resampler for {input_rate_hz} Hz -> {output_rate_hz} Hz"
        )
    })?;

    let input_frames = backend.input_frames_next();
    let output_frames = backend.output_frames_next();
    let backend_delay = backend.output_delay();

    Ok(RubatoComplexResampler {
        backend,
        input_rate_hz,
        output_rate_hz,
        input_frames,
        output_frames,
        backend_delay,
        pending: Vec::with_capacity(input_frames),
        input_buffer: vec![0.0; input_frames * CHANNELS],
        output_buffer: vec![0.0; output_frames * CHANNELS],
        total_input: 0,
        emitted: 0,
        backend_output_seen: 0,
        flushed: false,
    })
}

struct RubatoComplexResampler {
    backend: Fft<f32>,
    input_rate_hz: u32,
    output_rate_hz: u32,
    input_frames: usize,
    output_frames: usize,
    backend_delay: usize,
    pending: Vec<IqSample>,
    input_buffer: Vec<f32>,
    output_buffer: Vec<f32>,
    total_input: usize,
    emitted: usize,
    backend_output_seen: usize,
    flushed: bool,
}

impl RubatoComplexResampler {
    fn process_block(
        &mut self,
        block: &[IqSample],
        output: &mut Vec<IqSample>,
        limit: usize,
    ) -> Result<()> {
        if block.len() > self.input_frames {
            return Err(anyhow!(
                "internal resampler block has {} frames; maximum is {}",
                block.len(),
                self.input_frames
            ));
        }

        self.input_buffer.fill(0.0);
        for (frame, sample) in block.iter().enumerate() {
            self.input_buffer[frame * CHANNELS] = sample.re;
            self.input_buffer[frame * CHANNELS + 1] = sample.im;
        }
        self.output_buffer.fill(0.0);

        let input_adapter = InterleavedSlice::new(&self.input_buffer, CHANNELS, self.input_frames)
            .context("failed to wrap preallocated resampler input")?;
        let mut output_adapter =
            InterleavedSlice::new_mut(&mut self.output_buffer, CHANNELS, self.output_frames)
                .context("failed to wrap preallocated resampler output")?;
        let (_, produced) = self
            .backend
            .process_into_buffer(&input_adapter, &mut output_adapter, None)
            .context("rubato FFT processing failed")?;

        let skip = self
            .backend_delay
            .saturating_sub(self.backend_output_seen)
            .min(produced);
        self.backend_output_seen = self.backend_output_seen.saturating_add(produced);
        let available = produced.saturating_sub(skip);
        let wanted = limit.saturating_sub(self.emitted).min(available);
        output.reserve(wanted);
        for frame in skip..skip + wanted {
            output.push(IqSample::new(
                self.output_buffer[frame * CHANNELS],
                self.output_buffer[frame * CHANNELS + 1],
            ));
        }
        self.emitted = self.emitted.saturating_add(wanted);
        Ok(())
    }

    fn exact_output_len(&self) -> Result<usize> {
        let numerator = self
            .total_input
            .checked_mul(self.output_rate_hz as usize)
            .context("resampler output length overflow")?;
        Ok(numerator.div_ceil(self.input_rate_hz as usize))
    }
}

impl ComplexResampler for RubatoComplexResampler {
    fn input_rate(&self) -> u32 {
        self.input_rate_hz
    }

    fn output_rate(&self) -> u32 {
        self.output_rate_hz
    }

    fn latency_output_samples(&self) -> usize {
        0
    }

    fn process(&mut self, input: &[IqSample], output: &mut Vec<IqSample>) -> Result<()> {
        if self.flushed {
            return Err(anyhow!("cannot process input after resampler flush"));
        }
        self.total_input = self
            .total_input
            .checked_add(input.len())
            .context("resampler input length overflow")?;
        self.pending.extend_from_slice(input);

        let complete_frames = self.pending.len() / self.input_frames * self.input_frames;
        let streaming_limit = self
            .total_input
            .saturating_mul(self.output_rate_hz as usize)
            / self.input_rate_hz as usize;
        let mut pending = std::mem::take(&mut self.pending);
        let mut offset = 0;
        while offset < complete_frames {
            let end = offset + self.input_frames;
            self.process_block(&pending[offset..end], output, streaming_limit)?;
            offset = end;
        }
        if complete_frames > 0 {
            pending.drain(..complete_frames);
        }
        self.pending = pending;
        Ok(())
    }

    fn flush(&mut self, output: &mut Vec<IqSample>) -> Result<()> {
        if self.flushed {
            return Err(anyhow!("resampler was already flushed"));
        }
        self.flushed = true;
        let target = self.exact_output_len()?;

        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            self.process_block(&pending, output, target)?;
        }
        while self.emitted < target {
            self.process_block(&[], output, target)?;
        }
        Ok(())
    }
}

/// A stateful resampler for mono real-valued samples.
pub trait RealResampler {
    /// Stage and resample an arbitrary input partition, appending to `output`.
    ///
    /// # Errors
    /// Returns an error after flush, on length overflow, or on backend failure.
    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) -> Result<()>;

    /// Stage decimated samples while advancing an independent source clock.
    ///
    /// `source_frames` is the number of pre-decimation frames represented by
    /// this partition. It preserves exact stream duration when the backend
    /// operates on an integer-decimated signal.
    ///
    /// # Errors
    /// Returns an error after flush, on length overflow, or on backend failure.
    fn process_with_source_frames(
        &mut self,
        input: &[f32],
        source_frames: usize,
        output: &mut Vec<f32>,
    ) -> Result<()>;

    /// Emit the compensated tail and exact rational output length at EOF.
    ///
    /// # Errors
    /// Returns an error when called more than once, on length overflow, or on
    /// backend failure.
    fn flush(&mut self, output: &mut Vec<f32>) -> Result<()>;
}

/// Create a one-channel real-valued fixed-rate resampler.
///
/// The same bounded reduced-ratio validation used by the complex adapter runs
/// before Rubato construction and allocation.
///
/// # Errors
/// Returns an error for invalid or excessively large rate pairs, allocation
/// size overflow, or backend construction failure.
pub fn create_real_resampler(
    input_rate_hz: u32,
    output_rate_hz: u32,
) -> Result<impl RealResampler> {
    create_real_resampler_with_clock(input_rate_hz, output_rate_hz, input_rate_hz, output_rate_hz)
}

/// Create a real resampler for an integer-decimated RF stream.
///
/// The backend consumes one sample per `decimation_factor` RF frames, while
/// output duration follows the exact `rf_rate_hz` to `output_rate_hz` clock.
///
/// Drive the returned adapter with
/// [`process_with_source_frames`](RealResampler::process_with_source_frames).
/// Plain [`process`](RealResampler::process) reports the decimated buffer length
/// as the source frame count, which advances the backend clock by that length
/// instead of `decimation_factor` times it and therefore shortens the stream
/// duration it reports at EOF.
///
/// # Errors
/// Returns [`ResamplerCreationError::ZeroDecimationFactor`] when
/// `decimation_factor` is zero, plus the same invalid or excessively large rate
/// pair, allocation overflow, and backend construction errors as
/// [`create_real_resampler`].
pub fn create_decimated_real_resampler(
    rf_rate_hz: u32,
    output_rate_hz: u32,
    decimation_factor: u32,
) -> Result<impl RealResampler> {
    if decimation_factor == 0 {
        return Err(ResamplerCreationError::ZeroDecimationFactor { decimation_factor }.into());
    }
    let backend_output_rate = output_rate_hz.checked_mul(decimation_factor).ok_or(
        ResamplerCreationError::DecimatedRateOverflow {
            output_rate_hz,
            decimation_factor,
        },
    )?;
    create_real_resampler_with_clock(rf_rate_hz, backend_output_rate, rf_rate_hz, output_rate_hz)
}

fn create_real_resampler_with_clock(
    input_rate_hz: u32,
    output_rate_hz: u32,
    source_rate_hz: u32,
    clock_output_rate_hz: u32,
) -> Result<RubatoRealResampler> {
    let layout = validate_rate_pair(input_rate_hz, output_rate_hz)?;
    let backend = Fft::<f32>::new(
        layout.input_rate,
        layout.output_rate,
        INPUT_CHUNK_FRAMES,
        1,
        FixedSync::Both,
    )
    .with_context(|| {
        format!(
            "failed to create rubato FFT resampler for {input_rate_hz} Hz -> {output_rate_hz} Hz"
        )
    })?;
    let input_frames = backend.input_frames_next();
    let output_frames = backend.output_frames_next();
    let backend_delay = backend.output_delay();

    Ok(RubatoRealResampler {
        backend,
        source_rate_hz,
        clock_output_rate_hz,
        input_frames,
        output_frames,
        backend_delay,
        pending: Vec::with_capacity(input_frames),
        input_buffer: vec![0.0; input_frames],
        output_buffer: vec![0.0; output_frames],
        total_input: 0,
        total_source_frames: 0,
        emitted: 0,
        backend_output_seen: 0,
        flushed: false,
    })
}

struct RubatoRealResampler {
    backend: Fft<f32>,
    source_rate_hz: u32,
    clock_output_rate_hz: u32,
    input_frames: usize,
    output_frames: usize,
    backend_delay: usize,
    pending: Vec<f32>,
    input_buffer: Vec<f32>,
    output_buffer: Vec<f32>,
    total_input: usize,
    total_source_frames: usize,
    emitted: usize,
    backend_output_seen: usize,
    flushed: bool,
}

impl RubatoRealResampler {
    fn process_block(&mut self, block: &[f32], output: &mut Vec<f32>, limit: usize) -> Result<()> {
        if block.len() > self.input_frames {
            return Err(anyhow!(
                "internal resampler block has {} frames; maximum is {}",
                block.len(),
                self.input_frames
            ));
        }

        self.input_buffer.fill(0.0);
        self.input_buffer[..block.len()].copy_from_slice(block);
        self.output_buffer.fill(0.0);
        let input_adapter = InterleavedSlice::new(&self.input_buffer, 1, self.input_frames)
            .context("failed to wrap preallocated resampler input")?;
        let mut output_adapter =
            InterleavedSlice::new_mut(&mut self.output_buffer, 1, self.output_frames)
                .context("failed to wrap preallocated resampler output")?;
        let (_, produced) = self
            .backend
            .process_into_buffer(&input_adapter, &mut output_adapter, None)
            .context("rubato FFT processing failed")?;

        let skip = self
            .backend_delay
            .saturating_sub(self.backend_output_seen)
            .min(produced);
        self.backend_output_seen = self.backend_output_seen.saturating_add(produced);
        let wanted = limit
            .saturating_sub(self.emitted)
            .min(produced.saturating_sub(skip));
        output.extend_from_slice(&self.output_buffer[skip..skip + wanted]);
        self.emitted = self.emitted.saturating_add(wanted);
        Ok(())
    }

    fn exact_output_len(&self) -> Result<usize> {
        let numerator = self
            .total_source_frames
            .checked_mul(self.clock_output_rate_hz as usize)
            .context("resampler output length overflow")?;
        Ok(numerator.div_ceil(self.source_rate_hz as usize))
    }
}

impl RealResampler for RubatoRealResampler {
    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) -> Result<()> {
        self.process_with_source_frames(input, input.len(), output)
    }

    fn process_with_source_frames(
        &mut self,
        input: &[f32],
        source_frames: usize,
        output: &mut Vec<f32>,
    ) -> Result<()> {
        if self.flushed {
            return Err(anyhow!("cannot process input after resampler flush"));
        }
        self.total_input = self
            .total_input
            .checked_add(input.len())
            .context("resampler input length overflow")?;
        self.total_source_frames = self
            .total_source_frames
            .checked_add(source_frames)
            .context("resampler source frame count overflow")?;
        self.pending.extend_from_slice(input);

        let complete_frames = self.pending.len() / self.input_frames * self.input_frames;
        let streaming_limit = self
            .total_source_frames
            .saturating_mul(self.clock_output_rate_hz as usize)
            / self.source_rate_hz as usize;
        let mut pending = std::mem::take(&mut self.pending);
        let mut offset = 0;
        while offset < complete_frames {
            let end = offset + self.input_frames;
            self.process_block(&pending[offset..end], output, streaming_limit)?;
            offset = end;
        }
        if complete_frames > 0 {
            pending.drain(..complete_frames);
        }
        self.pending = pending;
        Ok(())
    }

    fn flush(&mut self, output: &mut Vec<f32>) -> Result<()> {
        if self.flushed {
            return Err(anyhow!("resampler was already flushed"));
        }
        self.flushed = true;
        let target = self.exact_output_len()?;
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            self.process_block(&pending, output, target)?;
        }
        while self.emitted < target {
            self.process_block(&[], output, target)?;
        }
        Ok(())
    }
}

fn greatest_common_divisor(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}
