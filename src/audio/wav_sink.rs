//! Streaming mono 32-bit float WAV sink.
//!
//! [`WavSink`] exports exactly the `f32` samples the audio pipeline hands to
//! playback, so a capture can be judged by ear and by measurement instead of by
//! trusting the demodulator.
//!
//! # On-disk format
//!
//! A canonical 44-byte RIFF/WAVE header — one `fmt ` chunk of 16 bytes with
//! audio format code 3 (IEEE float), 1 channel, 32 bits per sample, block align
//! 4 and byte rate `sample_rate * 4` — followed by a single `data` chunk of raw
//! little-endian `f32` samples. No `fact`, `bext` or `LIST` chunk and no pad
//! byte, so an empty capture is exactly 44 bytes.
//!
//! # Streaming behavior
//!
//! * The header is written by [`WavSink::new`] with placeholder lengths, and
//!   every [`AudioSink::write`] appends its samples to the file immediately. A
//!   capture stays tail-able while it runs, and a run killed mid-flight leaves a
//!   file whose lengths can be repaired from the real byte count.
//! * [`WavSink::finish`] rewrites only the two length fields — RIFF size at
//!   offset 4 (`file_len - 8`) and `data` size at offset 40 (`samples * 4`) — so
//!   a reader sees a complete file while the sink is still alive.
//! * `finish` is idempotent, and writing after it is an error: the patched
//!   lengths are already on disk and accepting more samples would make them lie.
//! * `Drop` patches the lengths through its own error-swallowing path (it only
//!   logs), because dropping a sink must never panic.
//! * Samples are written verbatim — no clipping, scaling or dithering — so the
//!   export is bit-identical to what the pipeline produced, including negative
//!   zero and out-of-range values.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::AudioSink;

/// Length of the canonical RIFF/WAVE header: `RIFF` framing (12) + `fmt ` chunk
/// (24) + `data` chunk header (8).
const HEADER_LEN: usize = 44;

/// RIFF size field of an empty capture: [`HEADER_LEN`] minus the 8 bytes of the
/// `RIFF` id and its own size field, which a RIFF size does not count.
const EMPTY_RIFF_SIZE: u32 = 36;

/// Bytes per sample in a mono 32-bit float stream; the same value as
/// [`BLOCK_ALIGN`], in the width byte-count arithmetic uses.
const BYTES_PER_SAMPLE: usize = 4;

/// [`BYTES_PER_SAMPLE`] as a `u64`, for the sample-count product.
const BYTES_PER_SAMPLE_U64: u64 = 4;

/// WAVE format code for IEEE float samples.
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

/// Channel count of the exported stream.
const MONO_CHANNELS: u16 = 1;

/// Bits per sample of the exported stream.
const BITS_PER_SAMPLE: u16 = 32;

/// Block alignment of the exported stream: one mono `f32` sample, i.e. 4 bytes.
const BLOCK_ALIGN: u16 = 4;

/// Payload length of the single `fmt ` chunk.
const FMT_CHUNK_LEN: u32 = 16;

/// Offset of the RIFF size field, which `finish` patches.
const OFFSET_RIFF_SIZE: usize = 4;

/// Offset of the `fmt ` chunk payload length.
const OFFSET_FMT_CHUNK_LEN: usize = 16;

/// Offset of the audio format code inside the `fmt ` chunk.
const OFFSET_AUDIO_FORMAT: usize = 20;

/// Offset of the channel-count field.
const OFFSET_CHANNELS: usize = 22;

/// Offset of the sample-rate field.
const OFFSET_SAMPLE_RATE: usize = 24;

/// Offset of the byte-rate field.
const OFFSET_BYTE_RATE: usize = 28;

/// Offset of the block-align field.
const OFFSET_BLOCK_ALIGN: usize = 32;

/// Offset of the bits-per-sample field.
const OFFSET_BITS_PER_SAMPLE: usize = 34;

/// Offset of the `data` chunk id.
const OFFSET_DATA_ID: usize = 36;

/// Offset of the `data` chunk size field, which `finish` patches.
const OFFSET_DATA_SIZE: usize = 40;

/// Streaming audio sink that writes a mono IEEE-float WAV file.
///
/// The sink owns its file and appends samples as they arrive, so it serves a
/// live capture as well as an offline render. [`AudioSink::finish`] patches the
/// lengths on the shutdown path; `Drop` does the same as a safety net.
#[derive(Debug)]
pub struct WavSink {
    /// Open capture file; the write cursor always sits at the end of the data.
    file: File,
    /// Destination path, kept for error messages.
    path: PathBuf,
    /// Sample rate declared in the header and reported by
    /// [`AudioSink::sample_rate`].
    sample_rate: u32,
    /// Number of `f32` samples written so far, i.e. what the `data` size covers.
    samples_written: u64,
    /// Whether the length fields have already been patched.
    finished: bool,
    /// Reusable little-endian staging buffer for encoded samples.
    scratch: Vec<u8>,
}

impl WavSink {
    /// Create a sink that streams to `path` and declares `sample_rate` in its header.
    ///
    /// # Errors
    ///
    /// * `sample_rate` is zero: a zero byte rate is an undecodable WAV, and the
    ///   check runs before the file is created, so a rejected construction leaves
    ///   nothing behind.
    /// * The path cannot be created, for example because its parent directory
    ///   does not exist or is not writable.
    /// * The sample rate is too large for a `u32` byte rate, or the header cannot
    ///   be written.
    pub fn new(path: impl AsRef<Path>, sample_rate: u32) -> Result<Self> {
        let path = path.as_ref();

        if sample_rate == 0 {
            bail!("WAV sink needs a non-zero sample rate, got 0 Hz");
        }

        let header = build_header(sample_rate).with_context(|| {
            format!("sample rate {sample_rate} Hz does not fit the WAV byte-rate field")
        })?;

        let mut file = File::create(path)
            .with_context(|| format!("failed to create WAV file {}", path.display()))?;
        file.write_all(&header)
            .with_context(|| format!("failed to write the WAV header to {}", path.display()))?;

        Ok(Self {
            file,
            path: path.to_path_buf(),
            sample_rate,
            samples_written: 0,
            finished: false,
            scratch: Vec::with_capacity(BYTES_PER_SAMPLE * 1024),
        })
    }

    /// Patch the RIFF and `data` lengths and flush the file.
    ///
    /// Idempotent: a second call returns `Ok(())` and leaves the file
    /// byte-identical. The shutdown path calls this through
    /// [`AudioSink::finish`], so a finished capture is playable without waiting
    /// for the sink to be dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the sample count does not fit the 4 GiB RIFF limit,
    /// or if seeking, writing or flushing fails.
    pub fn finish(&mut self) -> Result<()> {
        self.patch_lengths()
    }

    /// Rewrite the two length fields; shared by [`WavSink::finish`] and `Drop`.
    fn patch_lengths(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }

        let data_bytes = self
            .samples_written
            .checked_mul(BYTES_PER_SAMPLE_U64)
            .context("WAV data size overflow")?;
        let data_size =
            u32::try_from(data_bytes).context("WAV capture exceeds the 4 GiB RIFF limit")?;
        let riff_size = EMPTY_RIFF_SIZE
            .checked_add(data_size)
            .context("WAV capture exceeds the 4 GiB RIFF limit")?;

        // Only the two length fields are rewritten; every other header byte was
        // already final when `new` wrote it.
        self.write_field(OFFSET_RIFF_SIZE, &riff_size.to_le_bytes(), "RIFF size")?;
        self.write_field(OFFSET_DATA_SIZE, &data_size.to_le_bytes(), "data size")?;
        self.file
            .flush()
            .with_context(|| format!("failed to flush {}", self.path.display()))?;

        self.finished = true;
        Ok(())
    }

    /// Seek to `offset` and overwrite it with `bytes`.
    fn write_field(&mut self, offset: usize, bytes: &[u8], label: &str) -> Result<()> {
        let position =
            u64::try_from(offset).context("header offset does not fit a file position")?;
        self.file
            .seek(SeekFrom::Start(position))
            .with_context(|| format!("failed to seek to the {label} in {}", self.path.display()))?;
        self.file
            .write_all(bytes)
            .with_context(|| format!("failed to patch the {label} in {}", self.path.display()))
    }
}

impl AudioSink for WavSink {
    /// Append `samples` to the `data` chunk, verbatim and immediately.
    ///
    /// # Errors
    ///
    /// Returns an error if the sink was already finished, or if the write fails.
    fn write(&mut self, samples: &[f32]) -> Result<()> {
        if self.finished {
            bail!(
                "WAV sink for {} is already finished and cannot take more samples",
                self.path.display()
            );
        }
        if samples.is_empty() {
            return Ok(());
        }

        self.scratch.clear();
        self.scratch.reserve(samples.len() * BYTES_PER_SAMPLE);
        for sample in samples {
            self.scratch.extend_from_slice(&sample.to_le_bytes());
        }

        self.file
            .write_all(&self.scratch)
            .with_context(|| format!("failed to write samples to {}", self.path.display()))?;
        // Nothing buffers between this call and the kernel, so the file grows
        // while the capture runs: the export stays readable mid-flight.
        self.file
            .flush()
            .with_context(|| format!("failed to flush {}", self.path.display()))?;

        self.samples_written += u64::try_from(samples.len()).context("sample count overflow")?;
        Ok(())
    }

    /// Sample rate declared in the header; see [`AudioSink::sample_rate`].
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Patch the lengths; see [`WavSink::finish`].
    fn finish(&mut self) -> Result<()> {
        self.patch_lengths()
    }
}

impl Drop for WavSink {
    /// Patch the lengths so an interrupted run still leaves a playable file.
    ///
    /// Failures are logged only: dropping a sink must never panic, and a caller
    /// that needs the error calls [`WavSink::finish`] instead.
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if let Err(error) = self.patch_lengths() {
            tracing::error!(
                "Audio: failed to finalize WAV file {}: {error:#}",
                self.path.display()
            );
        }
    }
}

/// Build the canonical 44-byte header with placeholder chunk lengths.
///
/// The RIFF size starts at its empty-capture value and the `data` size at zero;
/// both are patched by `finish`/`Drop` once the real sample count is known.
fn build_header(sample_rate: u32) -> Result<[u8; HEADER_LEN]> {
    let byte_rate = sample_rate
        .checked_mul(u32::from(BLOCK_ALIGN))
        .context("byte rate overflow")?;

    let mut header = [0u8; HEADER_LEN];
    header[0..4].copy_from_slice(b"RIFF");
    header[OFFSET_RIFF_SIZE..OFFSET_RIFF_SIZE + 4].copy_from_slice(&EMPTY_RIFF_SIZE.to_le_bytes());
    header[8..12].copy_from_slice(b"WAVE");
    header[12..16].copy_from_slice(b"fmt ");
    header[OFFSET_FMT_CHUNK_LEN..OFFSET_FMT_CHUNK_LEN + 4]
        .copy_from_slice(&FMT_CHUNK_LEN.to_le_bytes());
    header[OFFSET_AUDIO_FORMAT..OFFSET_AUDIO_FORMAT + 2]
        .copy_from_slice(&WAVE_FORMAT_IEEE_FLOAT.to_le_bytes());
    header[OFFSET_CHANNELS..OFFSET_CHANNELS + 2].copy_from_slice(&MONO_CHANNELS.to_le_bytes());
    header[OFFSET_SAMPLE_RATE..OFFSET_SAMPLE_RATE + 4].copy_from_slice(&sample_rate.to_le_bytes());
    header[OFFSET_BYTE_RATE..OFFSET_BYTE_RATE + 4].copy_from_slice(&byte_rate.to_le_bytes());
    header[OFFSET_BLOCK_ALIGN..OFFSET_BLOCK_ALIGN + 2].copy_from_slice(&BLOCK_ALIGN.to_le_bytes());
    header[OFFSET_BITS_PER_SAMPLE..OFFSET_BITS_PER_SAMPLE + 2]
        .copy_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    header[OFFSET_DATA_ID..OFFSET_DATA_ID + 4].copy_from_slice(b"data");
    header[OFFSET_DATA_SIZE..OFFSET_DATA_SIZE + 4].copy_from_slice(&0u32.to_le_bytes());

    Ok(header)
}

#[cfg(test)]
mod tests {
    // `super::*` is what brings `WavSink` into scope once it exists; until then
    // the only item this file defines is this test module.
    #[allow(unused_imports)]
    use super::*;
    use crate::audio::AudioSink;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Length of the canonical RIFF/WAVE header written by `WavSink::new`.
    const CANONICAL_HEADER_LEN: usize = 44;
    /// Bytes per sample in a 32-bit float mono stream.
    const BYTES_PER_SAMPLE: usize = 4;
    /// The `fmt ` chunk payload length fixed by this contract.
    const FMT_CHUNK_LEN: u32 = 16;
    /// WAVE format code for IEEE float samples.
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    /// Number of channels in the exported stream.
    const MONO_CHANNELS: u16 = 1;
    /// Bits per sample in the exported stream.
    const BITS_PER_SAMPLE: u16 = 32;
    /// Offset of the RIFF length field.
    const OFFSET_RIFF_SIZE: usize = 4;
    /// Offset of the `fmt ` chunk length field.
    const OFFSET_FMT_SIZE: usize = 16;
    /// Offset of the format code inside the `fmt ` chunk.
    const OFFSET_AUDIO_FORMAT: usize = 20;
    /// Offset of the channel-count field.
    const OFFSET_CHANNELS: usize = 22;
    /// Offset of the sample-rate field.
    const OFFSET_SAMPLE_RATE: usize = 24;
    /// Offset of the byte-rate field.
    const OFFSET_BYTE_RATE: usize = 28;
    /// Offset of the block-align field.
    const OFFSET_BLOCK_ALIGN: usize = 32;
    /// Offset of the bits-per-sample field.
    const OFFSET_BITS_PER_SAMPLE: usize = 34;
    /// Offset of the `data` chunk length field.
    const OFFSET_DATA_SIZE: usize = 40;

    /// Distinctive sample values covering zero, negative zero, both rails, and
    /// a value with a non-trivial mantissa.
    const DISTINCTIVE_SAMPLES: [f32; 8] = [
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        -0.5,
        f32::MIN_POSITIVE,
        -0.123_456_79,
    ];

    /// The header fields this suite parses out by hand.
    struct ParsedWav {
        riff_size: u32,
        fmt_size: u32,
        audio_format: u16,
        channels: u16,
        sample_rate: u32,
        byte_rate: u32,
        block_align: u16,
        bits_per_sample: u16,
        data_size: u32,
        samples: Vec<f32>,
    }

    /// A uniquely named temp directory that removes itself when dropped.
    struct TempWav {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempWav {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "termsdr-wav-{tag}-{}-{seq}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join("capture.wav");
            Self { dir, path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempWav {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn read_u16(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
    }

    fn read_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    /// Parse the 44-byte canonical header and every sample after it, by hand.
    fn parse_wav(bytes: &[u8]) -> ParsedWav {
        assert!(
            bytes.len() >= CANONICAL_HEADER_LEN,
            "file shorter than a canonical header: {} bytes",
            bytes.len()
        );
        assert_eq!(&bytes[0..4], b"RIFF", "chunk id must be RIFF");
        assert_eq!(&bytes[8..12], b"WAVE", "form type must be WAVE");
        assert_eq!(&bytes[12..16], b"fmt ", "first subchunk must be fmt ");
        assert_eq!(
            &bytes[36..40],
            b"data",
            "fmt chunk must be followed by data"
        );

        let data_size = read_u32(bytes, OFFSET_DATA_SIZE);
        let samples = bytes[CANONICAL_HEADER_LEN..]
            .chunks_exact(BYTES_PER_SAMPLE)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();

        ParsedWav {
            riff_size: read_u32(bytes, OFFSET_RIFF_SIZE),
            fmt_size: read_u32(bytes, OFFSET_FMT_SIZE),
            audio_format: read_u16(bytes, OFFSET_AUDIO_FORMAT),
            channels: read_u16(bytes, OFFSET_CHANNELS),
            sample_rate: read_u32(bytes, OFFSET_SAMPLE_RATE),
            byte_rate: read_u32(bytes, OFFSET_BYTE_RATE),
            block_align: read_u16(bytes, OFFSET_BLOCK_ALIGN),
            bits_per_sample: read_u16(bytes, OFFSET_BITS_PER_SAMPLE),
            data_size,
            samples,
        }
    }

    /// Read the file, parse it, and assert the two length fields agree with the
    /// real byte count (this is what `finish`/`Drop` patching has to achieve).
    fn read_and_validate(path: &Path) -> (Vec<u8>, ParsedWav) {
        let bytes = fs::read(path).unwrap();
        let parsed = parse_wav(&bytes);

        assert_eq!(
            usize::try_from(parsed.data_size).unwrap(),
            bytes.len() - CANONICAL_HEADER_LEN,
            "data chunk size must equal the bytes actually after the header"
        );
        assert_eq!(
            usize::try_from(parsed.riff_size).unwrap(),
            bytes.len() - 8,
            "RIFF size must equal file length minus 8"
        );
        (bytes, parsed)
    }

    /// Assert every invariant that holds for any mono 32-bit float stream.
    fn assert_stream_format(parsed: &ParsedWav, expected_rate: u32) {
        assert_eq!(parsed.fmt_size, FMT_CHUNK_LEN, "fmt chunk size must be 16");
        assert_eq!(
            parsed.audio_format, WAVE_FORMAT_IEEE_FLOAT,
            "audio format must be 3 (IEEE float)"
        );
        assert_eq!(parsed.channels, MONO_CHANNELS, "stream must be mono");
        assert_eq!(
            parsed.bits_per_sample, BITS_PER_SAMPLE,
            "samples must be 32-bit"
        );
        assert_eq!(parsed.sample_rate, expected_rate, "declared sample rate");
        assert_eq!(
            parsed.block_align,
            u16::try_from(BYTES_PER_SAMPLE).unwrap(),
            "mono f32 block align must be 4"
        );
        assert_eq!(
            parsed.byte_rate,
            expected_rate * u32::try_from(BYTES_PER_SAMPLE).unwrap(),
            "byte rate must be sample_rate * 4"
        );
    }

    #[test]
    fn wav_sink_empty_file_is_canonical_header_only() {
        let temp = TempWav::new("empty");
        let mut sink = WavSink::new(temp.path(), 48_000).unwrap();
        // An empty slice is a legal write and must not disturb the data chunk.
        sink.write(&[]).unwrap();
        sink.finish().unwrap();

        let (bytes, parsed) = read_and_validate(temp.path());
        assert_stream_format(&parsed, 48_000);
        assert_eq!(
            bytes.len(),
            CANONICAL_HEADER_LEN,
            "an empty capture is exactly one 44-byte header (12+24+8 RIFF/fmt /data framing)"
        );
        assert_eq!(parsed.data_size, 0, "no samples written, so data size is 0");
        assert_eq!(parsed.samples.len(), 0, "no samples after the header");
    }

    #[test]
    fn wav_sink_header_declares_ieee_float_mono() {
        let temp = TempWav::new("header");
        let mut sink = WavSink::new(temp.path(), 48_000).unwrap();
        sink.write(&[0.25]).unwrap();
        sink.finish().unwrap();

        let (bytes, parsed) = read_and_validate(temp.path());
        assert_stream_format(&parsed, 48_000);
        assert_eq!(bytes.len(), CANONICAL_HEADER_LEN + BYTES_PER_SAMPLE);
        assert_eq!(
            usize::try_from(parsed.data_size).unwrap(),
            BYTES_PER_SAMPLE,
            "one sample is four bytes"
        );
    }

    #[test]
    fn wav_sink_writes_samples_bit_exactly() {
        let temp = TempWav::new("bit-exact");
        let mut sink = WavSink::new(temp.path(), 48_000).unwrap();
        sink.write(&DISTINCTIVE_SAMPLES).unwrap();
        sink.finish().unwrap();

        let (_, parsed) = read_and_validate(temp.path());
        assert_eq!(parsed.samples.len(), DISTINCTIVE_SAMPLES.len());
        for (index, (written, expected)) in parsed
            .samples
            .iter()
            .zip(DISTINCTIVE_SAMPLES.iter())
            .enumerate()
        {
            assert_eq!(
                written.to_bits(),
                expected.to_bits(),
                "sample {index} must round-trip bit-exactly, including the sign of zero"
            );
        }
    }

    #[test]
    fn wav_sink_multiple_writes_accumulate_in_order() {
        let temp = TempWav::new("append");
        let mut sink = WavSink::new(temp.path(), 48_000).unwrap();
        sink.write(&[0.1, -0.2, 0.3]).unwrap();
        sink.write(&[0.4]).unwrap();
        sink.write(&[0.5, -0.6]).unwrap();
        sink.finish().unwrap();

        let (_, parsed) = read_and_validate(temp.path());
        let expected = [0.1f32, -0.2, 0.3, 0.4, 0.5, -0.6];
        assert_eq!(
            usize::try_from(parsed.data_size).unwrap(),
            expected.len() * BYTES_PER_SAMPLE,
            "data size must count every sample from every write"
        );
        assert_eq!(parsed.samples.len(), expected.len());
        for (index, (written, wanted)) in parsed.samples.iter().zip(expected.iter()).enumerate() {
            assert_eq!(
                written.to_bits(),
                wanted.to_bits(),
                "write order must be preserved at index {index}"
            );
        }
    }

    #[test]
    fn wav_sink_finish_patches_chunk_lengths() {
        let temp = TempWav::new("patch");
        let mut sink = WavSink::new(temp.path(), 48_000).unwrap();
        let samples = [0.1f32; 5];
        sink.write(&samples).unwrap();

        // Before finishing, the header still carries its construction-time
        // placeholders; the lengths must not already be final.
        let before = fs::read(temp.path()).unwrap();
        assert_eq!(
            read_u32(&before, OFFSET_DATA_SIZE),
            0,
            "data size placeholder"
        );

        sink.finish().unwrap();

        let (bytes, parsed) = read_and_validate(temp.path());
        assert_eq!(
            usize::try_from(parsed.data_size).unwrap(),
            samples.len() * BYTES_PER_SAMPLE,
            "finish must rewrite the data size, not leave the placeholder"
        );
        assert_eq!(
            usize::try_from(parsed.riff_size).unwrap(),
            CANONICAL_HEADER_LEN + samples.len() * BYTES_PER_SAMPLE - 8,
            "finish must rewrite the RIFF size to total length minus 8"
        );
        assert_eq!(
            bytes.len(),
            CANONICAL_HEADER_LEN + samples.len() * BYTES_PER_SAMPLE
        );
    }

    #[test]
    fn wav_sink_drop_without_finish_patches_lengths() {
        let temp = TempWav::new("drop");
        let samples = [0.1f32, 0.2, -0.3, 0.4, -0.5, 0.6, 0.7];
        {
            let mut sink = WavSink::new(temp.path(), 48_000).unwrap();
            sink.write(&samples).unwrap();
            // Deliberately no finish(): the Drop impl owns patching here.
        }

        let (bytes, parsed) = read_and_validate(temp.path());
        assert_eq!(
            usize::try_from(parsed.data_size).unwrap(),
            samples.len() * BYTES_PER_SAMPLE,
            "Drop must patch the data size so the file has a playable length"
        );
        assert_eq!(
            bytes.len(),
            CANONICAL_HEADER_LEN + samples.len() * BYTES_PER_SAMPLE
        );
        assert_eq!(parsed.samples.len(), samples.len());
        for (index, (written, wanted)) in parsed.samples.iter().zip(samples.iter()).enumerate() {
            assert_eq!(
                written.to_bits(),
                wanted.to_bits(),
                "dropped sink must still have written sample {index}"
            );
        }
    }

    #[test]
    fn wav_sink_streams_samples_before_finish() {
        let temp = TempWav::new("stream");
        let mut sink = WavSink::new(temp.path(), 48_000).unwrap();
        // 4096 samples = 16 KiB: far more than any sane internal buffer, so at
        // least part of the payload must already be on disk while the capture
        // is still running.
        sink.write(&[0.5f32; 4096]).unwrap();

        let live_len = fs::metadata(temp.path()).unwrap().len();
        assert!(
            live_len > CANONICAL_HEADER_LEN as u64,
            "a streaming sink must reach the file during capture, got {live_len} bytes"
        );

        sink.finish().unwrap();
        let (_, parsed) = read_and_validate(temp.path());
        assert_eq!(parsed.samples.len(), 4096);
    }

    #[test]
    fn wav_sink_uses_actual_sample_rate_22050() {
        let temp = TempWav::new("rate-22050");
        let mut sink = WavSink::new(temp.path(), 22_050).unwrap();
        assert_eq!(sink.sample_rate(), 22_050, "sink must report its own rate");
        sink.write(&[0.1, 0.2]).unwrap();
        sink.finish().unwrap();

        let (_, parsed) = read_and_validate(temp.path());
        assert_stream_format(&parsed, 22_050);
        assert_eq!(parsed.byte_rate, 88_200, "22050 * 4 = 88200 bytes/s");
    }

    #[test]
    fn wav_sink_uses_actual_sample_rate_96000() {
        let temp = TempWav::new("rate-96000");
        let mut sink = WavSink::new(temp.path(), 96_000).unwrap();
        assert_eq!(sink.sample_rate(), 96_000);
        sink.write(&[0.1, 0.2]).unwrap();
        sink.finish().unwrap();

        let (_, parsed) = read_and_validate(temp.path());
        assert_stream_format(&parsed, 96_000);
        assert_eq!(parsed.byte_rate, 384_000, "96000 * 4 = 384000 bytes/s");
    }

    #[test]
    fn wav_sink_rejects_zero_sample_rate() {
        let temp = TempWav::new("zero-rate");
        let result = WavSink::new(temp.path(), 0);
        assert!(result.is_err(), "sample rate 0 must be rejected by new()");
        assert!(
            !temp.path().exists(),
            "a rejected construction must not leave a stray file behind"
        );
    }

    #[test]
    fn wav_sink_new_fails_for_missing_parent_directory() {
        let temp = TempWav::new("missing-parent");
        let path = temp
            .path()
            .with_file_name("no-such-dir")
            .join("capture.wav");
        let result = WavSink::new(&path, 48_000);
        assert!(
            result.is_err(),
            "creating a sink under a missing directory must report an error"
        );
    }

    #[test]
    fn wav_sink_finish_is_idempotent() {
        let temp = TempWav::new("idempotent");
        let mut sink = WavSink::new(temp.path(), 48_000).unwrap();
        sink.write(&[0.1, 0.2, 0.3]).unwrap();
        sink.finish().unwrap();
        let after_first = fs::read(temp.path()).unwrap();

        sink.finish().unwrap();
        let after_second = fs::read(temp.path()).unwrap();

        assert_eq!(
            after_first, after_second,
            "a second finish must not rewrite the header or re-patch the sizes"
        );
    }

    #[test]
    fn wav_sink_write_after_finish_is_error() {
        let temp = TempWav::new("write-after-finish");
        let mut sink = WavSink::new(temp.path(), 48_000).unwrap();
        sink.write(&[0.1]).unwrap();
        sink.finish().unwrap();

        let result = sink.write(&[0.2]);
        assert!(
            result.is_err(),
            "writing after finish must fail: the patched lengths are already on disk"
        );

        // The failed write must not corrupt what was already exported.
        let (_, parsed) = read_and_validate(temp.path());
        assert_eq!(
            usize::try_from(parsed.data_size).unwrap(),
            BYTES_PER_SAMPLE,
            "a rejected write must not change the data chunk size"
        );
    }

    #[test]
    fn wav_sink_is_usable_as_boxed_audio_sink() {
        fn assert_send<T: Send>() {}
        assert_send::<WavSink>();

        let temp = TempWav::new("boxed");
        let sink = WavSink::new(temp.path(), 48_000).unwrap();
        let mut boxed: Box<dyn AudioSink> = Box::new(sink);
        assert_eq!(boxed.sample_rate(), 48_000);
        // Written through the trait object, then dropped: the Drop patching must
        // still produce a valid file.
        boxed.write(&[0.25, -0.75]).unwrap();
        drop(boxed);

        let (_, parsed) = read_and_validate(temp.path());
        assert_eq!(parsed.samples.len(), 2);
    }
}
