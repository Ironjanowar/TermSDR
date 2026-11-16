//! Deterministic IQ file replay for offline analysis.
//!
//! Two input shapes are supported:
//!
//! * **`SigMF` dataset pairs** (`*.sigmf-meta` + `*.sigmf-data`): the datatype, sample
//!   rate, and center frequency are read from the JSON sidecar, so a capture replays
//!   with the metadata it was recorded with. Either member of the pair may be passed;
//!   the sibling is resolved automatically.
//! * **Raw interleaved IQ files**: the caller must supply the format, sample rate, and
//!   center frequency (CLI flags `--iq-format`, `--file-sample-rate`, and
//!   `--file-center-frequency`). Missing metadata is an error, never a silent default.
//!
//! Replay is deterministic: the source never loops, never paces itself, and never
//! invents samples. [`FileSource::read_samples`] reports the exact number of samples
//! produced and returns `Ok(0)` at end of file, which is the [`Source`] end-of-stream
//! convention used to end a capture thread cleanly.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::ValueEnum;

use super::rtlsdr::convert_u8_to_iq;
use super::Source;
use crate::types::IqSample;

/// File extension of a `SigMF` metadata sidecar.
const SIGMF_META_EXTENSION: &str = "sigmf-meta";

/// File extension of a `SigMF` dataset payload.
const SIGMF_DATA_EXTENSION: &str = "sigmf-data";

/// Lowest sample rate, in Hz, this source will replay.
///
/// A capture slower than one sample per second cannot drive a spectrum display at a
/// usable frame rate, and it inflates the pipeline's block duration (`fft_size / rate`)
/// far enough to leave `Duration`'s representable range, which panics the capture
/// thread. Rejecting sub-hertz rates at open time turns that panic into an actionable
/// message. Exactly one sample per second is allowed.
const MIN_SAMPLE_RATE_HZ: f64 = 1.0;

/// Supported on-disk IQ sample encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IqFileFormat {
    /// Interleaved unsigned 8-bit IQ pairs (RTL-SDR native layout).
    Cu8,
    /// Interleaved little-endian 32-bit float IQ pairs.
    ///
    /// The CLI spelling is `cf32le`; the `SigMF` `core:datatype` spelling is `cf32_le`.
    #[value(name = "cf32le")]
    Cf32Le,
}

impl IqFileFormat {
    /// Number of on-disk bytes taken by one complete complex sample.
    #[must_use]
    pub(crate) const fn bytes_per_sample(self) -> usize {
        match self {
            Self::Cu8 => 2,
            Self::Cf32Le => 8,
        }
    }

    /// Short human-readable description used in error messages.
    #[must_use]
    pub(crate) const fn describe(self) -> &'static str {
        match self {
            Self::Cu8 => "cu8",
            Self::Cf32Le => "cf32le",
        }
    }

    /// Map a `SigMF` `global.core:datatype` value onto a supported format.
    ///
    /// `meta_path` is the sidecar that declared `datatype`; it is only used to make the
    /// rejection message name the offending file.
    ///
    /// # Errors
    /// Returns a descriptive error naming the datatype and the sidecar when the datatype
    /// is not one of the formats this source can decode.
    fn from_sigmf_datatype(datatype: &str, meta_path: &Path) -> Result<Self> {
        match datatype {
            "cu8" => Ok(Self::Cu8),
            "cf32_le" => Ok(Self::Cf32Le),
            other => bail!(
                "unsupported SigMF datatype `{other}` in {}: this build supports `cu8` and \
                 `cf32_le` interleaved IQ only",
                meta_path.display()
            ),
        }
    }
}

/// IQ file replay source.
///
/// Samples are converted on read; the file itself is never loaded into memory as a
/// whole, so captures of any size replay at constant memory cost.
#[derive(Debug)]
pub struct FileSource {
    /// Path of the file the samples are read from (the `.sigmf-data` member for `SigMF`
    /// datasets), kept for diagnostics.
    path: PathBuf,
    /// On-disk sample encoding.
    format: IqFileFormat,
    /// Sample rate in Hz.
    sample_rate: f64,
    /// Center frequency in Hz.
    center_freq: f64,
    /// Buffered reader over the IQ payload.
    reader: BufReader<File>,
    /// Reused byte scratch buffer for the interleaved on-disk representation.
    scratch: Vec<u8>,
}

impl FileSource {
    /// Open an IQ recording for replay.
    ///
    /// `path` may point at a raw IQ file, at a `SigMF` `.sigmf-meta` sidecar, or at the
    /// `.sigmf-data` payload of a `SigMF` pair.
    ///
    /// `format`, `sample_rate`, and `center_freq` are overrides: `None` means "take the
    /// value from the `SigMF` metadata". A raw file needs all three, because nothing else
    /// can supply them.
    ///
    /// # Errors
    /// Returns an error when
    ///
    /// * a raw file is missing any of the three metadata items,
    /// * the `SigMF` sidecar is missing, unreadable, or lacks `core:datatype`,
    ///   `core:sample_rate`, or a first-capture `core:frequency`,
    /// * the `SigMF` datatype is not `cu8` or `cf32_le`,
    /// * the file length in bytes is not a whole number of samples, or
    /// * the data file cannot be opened, is not a regular file, its sample rate is not
    ///   finite and at least 1 Hz, or its center frequency is not finite.
    pub fn new(
        path: impl AsRef<Path>,
        format: Option<IqFileFormat>,
        sample_rate: Option<f64>,
        center_freq: Option<f64>,
    ) -> Result<Self> {
        let path = path.as_ref();
        if is_sigmf_path(path) {
            Self::open_sigmf(path, format, sample_rate, center_freq)
        } else {
            Self::open_raw(path, format, sample_rate, center_freq)
        }
    }

    /// On-disk format this source decodes.
    #[must_use]
    pub fn format(&self) -> IqFileFormat {
        self.format
    }

    /// Open a raw (non-`SigMF`) IQ file, which cannot supply metadata on its own.
    fn open_raw(
        path: &Path,
        format: Option<IqFileFormat>,
        sample_rate: Option<f64>,
        center_freq: Option<f64>,
    ) -> Result<Self> {
        let Some(format) = format else {
            bail!(
                "raw IQ file {} needs an explicit sample format: pass --iq-format cu8 or \
                 --iq-format cf32le (SigMF datasets carry this metadata instead)",
                path.display()
            );
        };
        let Some(sample_rate) = sample_rate else {
            bail!(
                "raw IQ file {} needs an explicit sample rate: pass --file-sample-rate <Hz> \
                 (SigMF datasets carry this metadata instead)",
                path.display()
            );
        };
        let Some(center_freq) = center_freq else {
            bail!(
                "raw IQ file {} needs an explicit center frequency: pass \
                 --file-center-frequency <Hz> (SigMF datasets carry this metadata instead)",
                path.display()
            );
        };
        Self::open_dataset(path.to_path_buf(), format, sample_rate, center_freq)
    }

    /// Open a `SigMF` dataset pair, preferring explicit overrides over sidecar metadata.
    fn open_sigmf(
        path: &Path,
        format: Option<IqFileFormat>,
        sample_rate: Option<f64>,
        center_freq: Option<f64>,
    ) -> Result<Self> {
        let (meta_path, data_path) = sigmf_pair(path);
        let meta = read_sigmf_metadata(&meta_path)?;

        // Explicit CLI values win; the sidecar fills in whatever is left.
        let format = if let Some(explicit) = format {
            explicit
        } else {
            let datatype = meta.datatype.as_deref().with_context(|| {
                format!(
                    "`SigMF` metadata {} has no string `global.core:datatype`",
                    meta_path.display()
                )
            })?;
            IqFileFormat::from_sigmf_datatype(datatype, &meta_path)?
        };

        let sample_rate = match sample_rate {
            Some(explicit) => explicit,
            None => meta.sample_rate.with_context(|| {
                format!(
                    "SigMF metadata {} has no `global.core:sample_rate`",
                    meta_path.display()
                )
            })?,
        };

        let center_freq = match center_freq {
            Some(explicit) => explicit,
            None => meta.center_freq.with_context(|| {
                format!(
                    "SigMF metadata {} has no `captures[0].core:frequency`",
                    meta_path.display()
                )
            })?,
        };

        Self::open_dataset(data_path, format, sample_rate, center_freq)
    }

    /// Open the payload file and validate that it holds whole samples.
    fn open_dataset(
        path: PathBuf,
        format: IqFileFormat,
        sample_rate: f64,
        center_freq: f64,
    ) -> Result<Self> {
        // Single validation choke point: both the raw path and the SigMF path (sidecar
        // metadata or explicit override) arrive here, so validating the values that are
        // actually stored covers every route. A non-finite center frequency would poison
        // every frequency label and the shift for the whole session; a sub-hertz rate
        // would panic later in the pipeline's `Duration` arithmetic.
        if !(sample_rate.is_finite() && sample_rate >= MIN_SAMPLE_RATE_HZ) {
            bail!(
                "sample rate for {} must be a finite number of Hz at least {MIN_SAMPLE_RATE_HZ} \
                 (one sample per second; pass --file-sample-rate <Hz>), got {sample_rate}",
                path.display()
            );
        }
        if !center_freq.is_finite() {
            bail!(
                "center frequency for {} must be a finite number of Hz (pass \
                 --file-center-frequency <Hz>), got {center_freq}",
                path.display()
            );
        }

        let file = File::open(&path)
            .with_context(|| format!("failed to open IQ data file {}", path.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("failed to stat IQ data file {}", path.display()))?;
        // `File::open` succeeds on a directory on Linux and `metadata().len()` reports a
        // plausible non-zero size, so a directory would otherwise pass the length check
        // and only fail on the first read, deep inside the capture loop.
        if !metadata.is_file() {
            bail!(
                "IQ data file {} is not a regular file (a directory and other special files \
                 cannot hold an IQ recording)",
                path.display()
            );
        }
        let byte_len = usize::try_from(metadata.len())
            .with_context(|| format!("IQ data file {} is too large to map", path.display()))?;

        let bytes_per_sample = format.bytes_per_sample();
        if byte_len % bytes_per_sample != 0 {
            bail!(
                "IQ data file {} is {byte_len} bytes long, which is not a whole number of {} \
                 samples ({bytes_per_sample} bytes per sample)",
                path.display(),
                format.describe()
            );
        }

        Ok(Self {
            path,
            format,
            sample_rate,
            center_freq,
            reader: BufReader::new(file),
            scratch: Vec::new(),
        })
    }
}

impl Source for FileSource {
    /// Read up to `buffer.len()` samples, returning the number actually produced.
    ///
    /// Only `buffer[..count]` is written, so a short final block never leaves stale
    /// samples from a previous block in the tail. End of file is reported as `Ok(0)`
    /// and stays `Ok(0)` on later calls. An empty buffer returns `Ok(0)` without
    /// consuming any input.
    ///
    /// # Errors
    /// Returns an error if the underlying file cannot be read, or if it ends part-way
    /// through a sample.
    fn read_samples(&mut self, buffer: &mut [IqSample]) -> Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }

        let bytes_per_sample = self.format.bytes_per_sample();
        let wanted_bytes = buffer.len() * bytes_per_sample;
        self.scratch.resize(wanted_bytes, 0);

        let filled = read_fully(&mut self.reader, &mut self.scratch)
            .with_context(|| format!("failed to read IQ data file {}", self.path.display()))?;
        if filled == 0 {
            return Ok(0);
        }
        if filled % bytes_per_sample != 0 {
            bail!(
                "IQ data file {} ended mid-sample: read {filled} bytes, which is not a \
                 multiple of {bytes_per_sample} bytes per sample",
                self.path.display()
            );
        }

        let count = filled / bytes_per_sample;
        match self.format {
            // Same centered u8 mapping the RTL-SDR path uses, so a recorded and a live
            // capture of the same signal produce identical samples.
            IqFileFormat::Cu8 => Ok(convert_u8_to_iq(
                &self.scratch[..filled],
                &mut buffer[..count],
            )),
            IqFileFormat::Cf32Le => {
                let bytes = &self.scratch[..filled];
                // Take the stride from the format table rather than repeating the
                // `cf32le` width as a literal here.
                let pair_len = IqFileFormat::Cf32Le.bytes_per_sample();
                let pairs = bytes.chunks_exact(pair_len);

                // Validate the whole block before writing a single sample: this source's
                // contract is "a complete block or an error", so a failing call must leave
                // the caller's buffer untouched rather than half-filled. The pass is over
                // the byte scratch, not an allocation per sample.
                for pair in pairs.clone() {
                    let re = f32::from_le_bytes([pair[0], pair[1], pair[2], pair[3]]);
                    let im = f32::from_le_bytes([pair[4], pair[5], pair[6], pair[7]]);
                    if !(re.is_finite() && im.is_finite()) {
                        bail!(
                            "IQ data file {} contains a non-finite (NaN or infinite) cf32le \
                             sample: replaying it would poison the spectrum and audio chain, so \
                             the block is rejected instead",
                            self.path.display()
                        );
                    }
                }

                for (slot, pair) in buffer[..count].iter_mut().zip(pairs) {
                    let re = f32::from_le_bytes([pair[0], pair[1], pair[2], pair[3]]);
                    let im = f32::from_le_bytes([pair[4], pair[5], pair[6], pair[7]]);
                    *slot = IqSample::new(re, im);
                }
                Ok(count)
            }
        }
    }

    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn center_freq(&self) -> f64 {
        self.center_freq
    }
}

/// `SigMF` metadata fields this source consumes.
#[derive(Debug, Default)]
struct SigmfMetadata {
    /// `global.core:datatype`, when present.
    datatype: Option<String>,
    /// `global.core:sample_rate` in Hz, when present.
    sample_rate: Option<f64>,
    /// First capture `core:frequency` in Hz, when present.
    center_freq: Option<f64>,
}

/// Read and extract the `SigMF` fields `TermSDR` needs.
///
/// # Errors
/// Returns an error when the sidecar cannot be read or parsed, or when its `global`
/// object is missing or not an object.
fn read_sigmf_metadata(meta_path: &Path) -> Result<SigmfMetadata> {
    let text = std::fs::read_to_string(meta_path).with_context(|| {
        format!(
            "failed to read SigMF metadata {} (a .sigmf-data file needs its .sigmf-meta \
             sidecar to supply format, sample rate, and center frequency)",
            meta_path.display()
        )
    })?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse SigMF metadata {}", meta_path.display()))?;

    let global = value
        .get("global")
        .and_then(serde_json::Value::as_object)
        .with_context(|| {
            format!(
                "SigMF metadata {} has no `global` object",
                meta_path.display()
            )
        })?;

    Ok(SigmfMetadata {
        datatype: global
            .get("core:datatype")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        sample_rate: global
            .get("core:sample_rate")
            .and_then(serde_json::Value::as_f64),
        // Only the first capture segment is used: its start offset is zero, so its
        // frequency describes the whole recording from the first sample on.
        center_freq: value
            .get("captures")
            .and_then(serde_json::Value::as_array)
            .and_then(|captures| captures.first())
            .and_then(|capture| capture.get("core:frequency"))
            .and_then(serde_json::Value::as_f64),
    })
}

/// Resolve both members of a `SigMF` pair from either one of them.
fn sigmf_pair(path: &Path) -> (PathBuf, PathBuf) {
    if path.extension().and_then(|ext| ext.to_str()) == Some(SIGMF_DATA_EXTENSION) {
        (
            path.with_extension(SIGMF_META_EXTENSION),
            path.to_path_buf(),
        )
    } else {
        (
            path.to_path_buf(),
            path.with_extension(SIGMF_DATA_EXTENSION),
        )
    }
}

/// Whether a path names a member of a `SigMF` dataset pair.
fn is_sigmf_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some(SIGMF_META_EXTENSION | SIGMF_DATA_EXTENSION)
    )
}

/// Fill `buffer` from `reader`, stopping at end of file.
///
/// Returns the number of bytes read, which is smaller than `buffer.len()` only at end
/// of file. Interrupted reads are retried rather than reported as short data.
fn read_fully(reader: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    //! Frozen contract tests for deterministic IQ file replay (plan Task 3).
    //!
    //! API shape these tests require (must be provided by the implementation):
    //!
    //! ```text
    //! pub enum IqFileFormat { Cu8, Cf32Le }   // Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum
    //! impl FileSource {                        // Debug (AGENTS.md: Debug on all types)
    //!     pub fn new(path: impl AsRef<Path>, format: Option<IqFileFormat>,
    //!                sample_rate: Option<f64>, center_freq: Option<f64>) -> Result<Self>;
    //!     pub fn format(&self) -> IqFileFormat;
    //! }
    //! ```
    //!
    //! Decisions frozen here where the plan left a choice:
    //!
    //! * One constructor covers both inputs. `None` metadata means "take it from the
    //!   `SigMF` pair"; explicit `Some` values override `SigMF` metadata. A raw file
    //!   with any of the three metadata items missing is an error (no silent defaults).
    //! * `SigMF` detection is by path shape: either member of the pair (`.sigmf-meta`
    //!   or `.sigmf-data`) is accepted and resolves to its sibling.
    //! * `IqFileFormat` stays at the two formats the plan's CLI line enumerates
    //!   (`--iq-format cu8|cf32le`). `ci8` (complex *signed* int8) is out of scope:
    //!   the plan's prose attaches "unsigned 8-bit" to both `cu8` and `ci8`, which is
    //!   a wording defect rather than a contradiction, and it specifies no scaling
    //!   convention for signed 8-bit. Both `ci8` and any other unsupported datatype
    //!   must be rejected with the datatype named — that rejection is enforced below.
    //! * The CLI spelling for the float format is exactly `cf32le`, as the plan
    //!   writes it. A plain `ValueEnum` derive would emit clap's kebab-case default
    //!   `cf32-le`, so the variant needs an explicit `#[value(name = "cf32le")]`.
    //!   The `SigMF` side keeps the `SigMF` spelling `cf32_le`; the two namespaces
    //!   differ on purpose and must not be conflated.
    //! * Malformed byte counts are rejected by `new()`, not by `read_samples()`. The
    //!   dataset length is known at open time, so failing fast keeps the replay
    //!   stream from ever yielding a half sample. The plan only requires it to be an
    //!   error, so the earlier of the two legal points is frozen.
    //! * Error messages are asserted only on the discriminating words required by the
    //!   plan ("descriptive error"): the offending datatype, "byte", "sample",
    //!   "frequen", "format", "sigmf". Wording beyond that is free.

    use super::*;
    use approx::assert_abs_diff_eq;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Number of RTL-SDR u8 centers, mirrored from `src/capture/rtlsdr.rs`.
    const CU8_DC_OFFSET: f32 = 127.5;

    static NEXT_TEST_DIR: AtomicUsize = AtomicUsize::new(0);

    /// Self-cleaning temporary directory holding generated IQ fixtures.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        /// Create a unique temporary directory for one test.
        fn new(label: &str) -> Self {
            let index = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "termsdr-file-source-{}-{index}-{label}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        /// Write a fixture file and return its path as a `&str`-compatible `String`.
        fn write(&self, name: &str, bytes: &[u8]) -> String {
            let file_path = self.path.join(name);
            fs::write(&file_path, bytes).expect("write fixture");
            file_path
                .to_str()
                .expect("invariant: temp paths are UTF-8")
                .to_string()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Interleave raw u8 IQ pairs into an on-disk byte buffer.
    fn cu8_bytes(samples: &[(u8, u8)]) -> Vec<u8> {
        samples.iter().flat_map(|(i, q)| [*i, *q]).collect()
    }

    /// Interleave float IQ pairs into little-endian on-disk bytes.
    fn cf32le_bytes(samples: &[(f32, f32)]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(samples.len() * 8);
        for (i, q) in samples {
            bytes.extend_from_slice(&i.to_le_bytes());
            bytes.extend_from_slice(&q.to_le_bytes());
        }
        bytes
    }

    /// Expected CU8 conversion value, copied verbatim from the RTL-SDR mapping.
    fn expected_cu8(byte: u8) -> f32 {
        (f32::from(byte) - CU8_DC_OFFSET) / CU8_DC_OFFSET
    }

    /// Build a `SigMF` dataset pair and return `(meta_path, data_path)`.
    fn write_sigmf_pair(
        dir: &TempDir,
        stem: &str,
        datatype: &str,
        sample_rate: f64,
        frequency: f64,
        data: &[u8],
    ) -> (String, String) {
        // serde_json builds the metadata so the fixture stays valid JSON without
        // hand-escaped braces; no extra dependency, serde_json is already a dep.
        let meta = serde_json::json!({
            "global": {
                "core:datatype": datatype,
                "core:sample_rate": sample_rate,
                "core:version": "1.2.0"
            },
            "captures": [
                {"core:sample_start": 0, "core:frequency": frequency}
            ],
            "annotations": []
        });
        let meta_path = dir.write(&format!("{stem}.sigmf-meta"), meta.to_string().as_bytes());
        let data_path = dir.write(&format!("{stem}.sigmf-data"), data);
        (meta_path, data_path)
    }

    // === Conversion ===

    #[test]
    fn cu8_conversion_matches_rtlsdr_centered_mapping() {
        let dir = TempDir::new("cu8-convert");
        let raw = cu8_bytes(&[(0, 255), (127, 128), (255, 0)]);
        let path = dir.write("capture.cu8", &raw);

        let mut source = FileSource::new(
            path.as_str(),
            Some(IqFileFormat::Cu8),
            Some(2.048e6),
            Some(118.1e6),
        )
        .expect("open raw cu8 file");
        let mut buffer = [IqSample::new(0.0, 0.0); 3];
        let count = source.read_samples(&mut buffer).expect("read cu8");

        assert_eq!(count, 3);
        assert_abs_diff_eq!(buffer[0].re, expected_cu8(0), epsilon = 1e-6);
        assert_abs_diff_eq!(buffer[0].im, expected_cu8(255), epsilon = 1e-6);
        assert!((buffer[0].re - (-1.0)).abs() < 1e-6);
        assert!((buffer[0].im - 1.0).abs() < 1e-6);
        // 127/128 straddle the 127.5 DC center, exactly as the RTL-SDR path does.
        assert!((buffer[1].re - (-0.003_921_569)).abs() < 1e-6);
        assert!((buffer[1].im - 0.003_921_569).abs() < 1e-6);
        assert!((buffer[2].re - 1.0).abs() < 1e-6);
        assert!((buffer[2].im - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn cf32le_conversion_reads_little_endian_floats() {
        let dir = TempDir::new("cf32le-convert");
        let raw = cf32le_bytes(&[(-0.5, 0.25), (1.0, -1.0)]);
        let path = dir.write("capture.cf32le", &raw);

        let mut source = FileSource::new(
            path.as_str(),
            Some(IqFileFormat::Cf32Le),
            Some(2.048e6),
            Some(118.1e6),
        )
        .expect("open raw cf32le file");
        let mut buffer = [IqSample::new(0.0, 0.0); 2];
        let count = source.read_samples(&mut buffer).expect("read cf32le");

        assert_eq!(count, 2);
        assert!((buffer[0].re - (-0.5)).abs() < 1e-6);
        assert!((buffer[0].im - 0.25).abs() < 1e-6);
        assert!((buffer[1].re - 1.0).abs() < 1e-6);
        assert!((buffer[1].im - (-1.0)).abs() < 1e-6);
    }

    // === Partial reads, EOF, no stale tail ===

    #[test]
    fn partial_final_read_reports_actual_count() {
        let dir = TempDir::new("partial-count");
        let raw = cu8_bytes(&[(0, 0), (64, 64), (128, 128), (192, 192), (255, 255)]);
        let path = dir.write("five.cu8", &raw);

        let mut source = FileSource::new(
            path.as_str(),
            Some(IqFileFormat::Cu8),
            Some(1.0e6),
            Some(100.0e6),
        )
        .expect("open raw cu8 file");
        let mut buffer = [IqSample::new(0.0, 0.0); 3];

        assert_eq!(source.read_samples(&mut buffer).expect("first read"), 3);
        // The final block is short: the count must reflect it, not the buffer length.
        assert_eq!(source.read_samples(&mut buffer).expect("partial read"), 2);
        assert_eq!(source.read_samples(&mut buffer).expect("eof read"), 0);
    }

    #[test]
    fn partial_final_read_leaves_tail_untouched() {
        let dir = TempDir::new("stale-tail");
        let sentinel = IqSample::new(9.0, -9.0);
        let raw = cu8_bytes(&[(1, 2), (3, 4), (5, 6), (7, 8), (9, 10), (11, 12)]);
        let path = dir.write("six.cu8", &raw);

        let mut source = FileSource::new(
            path.as_str(),
            Some(IqFileFormat::Cu8),
            Some(1.0e6),
            Some(100.0e6),
        )
        .expect("open raw cu8 file");
        let mut buffer = [sentinel; 4];

        assert_eq!(source.read_samples(&mut buffer).expect("full block"), 4);
        // Re-poison the tail so the check below can only pass if the short read
        // truly stops at `count`.
        buffer[2] = sentinel;
        buffer[3] = sentinel;

        let count = source.read_samples(&mut buffer).expect("partial block");
        assert_eq!(count, 2);
        // Only output[..count] may be written; the stale tail must survive so that
        // callers slicing buffer[..count] cannot pick up samples from the last block.
        assert_eq!(buffer[2], sentinel);
        assert_eq!(buffer[3], sentinel);
        assert_abs_diff_eq!(buffer[0].re, expected_cu8(9), epsilon = 1e-6);
        assert_abs_diff_eq!(buffer[0].im, expected_cu8(10), epsilon = 1e-6);
        assert_abs_diff_eq!(buffer[1].re, expected_cu8(11), epsilon = 1e-6);
        assert_abs_diff_eq!(buffer[1].im, expected_cu8(12), epsilon = 1e-6);
    }

    #[test]
    fn eof_returns_zero_and_stays_zero() {
        let dir = TempDir::new("eof");
        let raw = cu8_bytes(&[(0, 255), (255, 0)]);
        let path = dir.write("two.cu8", &raw);

        let mut source = FileSource::new(
            path.as_str(),
            Some(IqFileFormat::Cu8),
            Some(1.0e6),
            Some(100.0e6),
        )
        .expect("open raw cu8 file");
        let mut buffer = [IqSample::new(0.0, 0.0); 2];

        assert_eq!(source.read_samples(&mut buffer).expect("first read"), 2);
        // EOF convention: Ok(0), never an error, and stable across repeated calls.
        assert_eq!(source.read_samples(&mut buffer).expect("eof"), 0);
        assert_eq!(source.read_samples(&mut buffer).expect("eof repeated"), 0);
    }

    #[test]
    fn empty_file_returns_zero_immediately() {
        let dir = TempDir::new("empty");
        let path = dir.write("empty.cu8", &[]);

        let mut source = FileSource::new(
            path.as_str(),
            Some(IqFileFormat::Cu8),
            Some(1.0e6),
            Some(100.0e6),
        )
        .expect("open empty raw cu8 file");
        let mut buffer = [IqSample::new(0.0, 0.0); 4];

        assert_eq!(source.read_samples(&mut buffer).expect("empty read"), 0);
    }

    #[test]
    fn read_samples_into_empty_buffer_returns_zero() {
        let dir = TempDir::new("empty-buffer");
        let raw = cu8_bytes(&[(0, 0)]);
        let path = dir.write("one.cu8", &raw);

        let mut source = FileSource::new(
            path.as_str(),
            Some(IqFileFormat::Cu8),
            Some(1.0e6),
            Some(100.0e6),
        )
        .expect("open raw cu8 file");
        let mut buffer: [IqSample; 0] = [];

        assert_eq!(
            source
                .read_samples(&mut buffer)
                .expect("zero-capacity read"),
            0
        );
    }

    // === Malformed byte counts ===

    #[test]
    fn cu8_trailing_odd_byte_is_an_error() {
        let dir = TempDir::new("cu8-odd");
        // 5 bytes can never be whole cu8 IQ pairs.
        let raw = cu8_bytes(&[(0, 0), (128, 128)]);
        let mut trailing = raw.clone();
        trailing.push(7);
        let path = dir.write("odd.cu8", &trailing);

        let error = FileSource::new(
            path.as_str(),
            Some(IqFileFormat::Cu8),
            Some(1.0e6),
            Some(100.0e6),
        )
        .expect_err("odd trailing byte must not be silently dropped");
        let message = error.to_string().to_lowercase();
        assert!(
            message.contains("byte"),
            "error must describe the malformed byte count, got: {message}"
        );
    }

    #[test]
    fn cf32le_truncated_sample_is_an_error() {
        let dir = TempDir::new("cf32le-truncated");
        // 12 bytes = one whole sample plus half of the next.
        let mut raw = cf32le_bytes(&[(0.5, -0.5)]);
        raw.extend_from_slice(&1.0_f32.to_le_bytes());
        let path = dir.write("truncated.cf32le", &raw);

        let error = FileSource::new(
            path.as_str(),
            Some(IqFileFormat::Cf32Le),
            Some(1.0e6),
            Some(100.0e6),
        )
        .expect_err("truncated cf32le sample must be an error");
        let message = error.to_string().to_lowercase();
        assert!(
            message.contains("byte"),
            "error must describe the malformed byte count, got: {message}"
        );
    }

    // === Missing metadata for raw files ===

    #[test]
    fn raw_file_without_format_is_an_error() {
        let dir = TempDir::new("no-format");
        let raw = cu8_bytes(&[(0, 0)]);
        let path = dir.write("raw.iq", &raw);

        let error = FileSource::new(path.as_str(), None, Some(1.0e6), Some(100.0e6))
            .expect_err("raw file without --iq-format must be rejected");
        let message = error.to_string().to_lowercase();
        // The user can only fix this by naming a flag, so the message must name the exact
        // flag; a bare "format" leaves them guessing between --iq-format and nothing else.
        assert!(
            message.contains("--iq-format"),
            "error must name the exact flag --iq-format, got: {message}"
        );
    }

    #[test]
    fn raw_file_without_sample_rate_is_an_error() {
        let dir = TempDir::new("no-rate");
        let raw = cu8_bytes(&[(0, 0)]);
        let path = dir.write("raw.iq", &raw);

        let error = FileSource::new(path.as_str(), Some(IqFileFormat::Cu8), None, Some(100.0e6))
            .expect_err("raw file without --file-sample-rate must be rejected");
        let message = error.to_string().to_lowercase();
        // Exact flag name: --sample-rate is a different flag on a different scale.
        assert!(
            message.contains("--file-sample-rate"),
            "error must name the exact flag --file-sample-rate, got: {message}"
        );
    }

    #[test]
    fn raw_file_without_center_frequency_is_an_error() {
        let dir = TempDir::new("no-freq");
        let raw = cu8_bytes(&[(0, 0)]);
        let path = dir.write("raw.iq", &raw);

        let error = FileSource::new(path.as_str(), Some(IqFileFormat::Cu8), Some(1.0e6), None)
            .expect_err("raw file without --file-center-frequency must be rejected");
        let message = error.to_string().to_lowercase();
        // Exact flag name: --frequency is a different flag on a different scale.
        assert!(
            message.contains("--file-center-frequency"),
            "error must name the exact flag --file-center-frequency, got: {message}"
        );
    }

    // === SigMF inputs ===

    #[test]
    fn sigmf_metadata_is_extracted_from_pair() {
        let dir = TempDir::new("sigmf-meta");
        let raw = cu8_bytes(&[(0, 255), (255, 0)]);
        let (meta_path, _data_path) =
            write_sigmf_pair(&dir, "aviation", "cu8", 2_048_000.0, 118_100_000.0, &raw);

        let mut source = FileSource::new(meta_path.as_str(), None, None, None)
            .expect("open SigMF dataset via .sigmf-meta");
        assert_eq!(source.format(), IqFileFormat::Cu8);
        assert_abs_diff_eq!(source.sample_rate(), 2_048_000.0, epsilon = 1e-6);
        assert_abs_diff_eq!(source.center_freq(), 118_100_000.0, epsilon = 1e-6);

        let mut buffer = [IqSample::new(0.0, 0.0); 2];
        assert_eq!(
            source
                .read_samples(&mut buffer)
                .expect("read SigMF cu8 data"),
            2
        );
    }

    #[test]
    fn sigmf_dataset_data_path_is_accepted() {
        let dir = TempDir::new("sigmf-data-path");
        let raw = cu8_bytes(&[(0, 255)]);
        let (_meta_path, data_path) =
            write_sigmf_pair(&dir, "airband", "cu8", 1_024_000.0, 120_000_000.0, &raw);

        let mut source = FileSource::new(data_path.as_str(), None, None, None)
            .expect("open SigMF dataset via .sigmf-data");
        assert_abs_diff_eq!(source.sample_rate(), 1_024_000.0, epsilon = 1e-6);

        let mut buffer = [IqSample::new(0.0, 0.0); 1];
        assert_eq!(
            source
                .read_samples(&mut buffer)
                .expect("read SigMF cu8 data"),
            1
        );
        assert!((buffer[0].re - (-1.0)).abs() < 1e-6);
        assert!((buffer[0].im - 1.0).abs() < 1e-6);
    }

    #[test]
    fn sigmf_cf32le_datatype_is_accepted() {
        let dir = TempDir::new("sigmf-cf32le");
        let raw = cf32le_bytes(&[(0.5, -0.5)]);
        let (meta_path, _data_path) = write_sigmf_pair(
            &dir,
            "float-capture",
            "cf32_le",
            480_000.0,
            118_000_000.0,
            &raw,
        );

        let mut source = FileSource::new(meta_path.as_str(), None, None, None)
            .expect("open SigMF cf32_le dataset");
        assert_eq!(source.format(), IqFileFormat::Cf32Le);

        let mut buffer = [IqSample::new(0.0, 0.0); 1];
        assert_eq!(
            source.read_samples(&mut buffer).expect("read float data"),
            1
        );
        assert!((buffer[0].re - 0.5).abs() < 1e-6);
        assert!((buffer[0].im - (-0.5)).abs() < 1e-6);
    }

    #[test]
    fn unsupported_sigmf_datatype_is_rejected() {
        let dir = TempDir::new("sigmf-unsupported");
        let raw = vec![0_u8; 8];
        let (meta_path, _data_path) =
            write_sigmf_pair(&dir, "s16", "cs16_le", 1_000_000.0, 100_000_000.0, &raw);

        let error = FileSource::new(meta_path.as_str(), None, None, None)
            .expect_err("unsupported SigMF datatype must be rejected");
        let message = error.to_string().to_lowercase();
        assert!(
            message.contains("cs16_le"),
            "error must name the unsupported datatype, got: {message}"
        );
        assert!(
            message.contains("support"),
            "error must state the datatype is unsupported, got: {message}"
        );
    }

    #[test]
    fn missing_sigmf_meta_is_an_error() {
        let dir = TempDir::new("sigmf-no-meta");
        let raw = cu8_bytes(&[(0, 0)]);
        // A bare .sigmf-data file with no sibling .sigmf-meta cannot supply metadata.
        let data_path = dir.write("orphan.sigmf-data", &raw);

        let error = FileSource::new(data_path.as_str(), None, None, None)
            .expect_err("SigMF data file without metadata must be rejected");
        let message = error.to_string().to_lowercase();
        assert!(
            message.contains("sigmf"),
            "error must mention the missing SigMF metadata, got: {message}"
        );
    }

    // === Blocker-fix round (review findings) ===
    //
    // Contract decided for non-finite *sample values*: `read_samples` returns an error
    // that names the file and the reason ("non-finite sample"). Rationale: silently
    // clamping or zeroing invents data, and this source's whole promise is that it
    // replays exactly what is on disk. A `cf32le` payload containing `inf`/NaN is a
    // corrupt recording, so it is reported, not repaired.
    //
    // Block atomicity: the error is raised *before* `buffer[..count]` is written, so a
    // caller that ignores the error still sees its own pre-call sentinel values rather
    // than a half-valid block of decoded samples. Decoding happens into a check first,
    // then a fill; the test below pins both halves of that (error returned AND buffer
    // untouched).

    /// Non-finite center frequencies must be rejected for a **raw** file.
    ///
    /// A NaN/inf center passes `open_dataset` today because only the sample rate is
    /// validated one line above, then poisons the frequency axis (`frequency_shift`)
    /// and every bin label for the whole session.
    #[test]
    fn raw_file_with_non_finite_center_frequency_is_rejected() {
        let dir = TempDir::new("nonfinite-center-raw");
        let raw = cu8_bytes(&[(0, 0)]);
        let path = dir.write("raw.iq", &raw);

        for (label, bad_freq) in [
            ("NaN", f64::NAN),
            ("+inf", f64::INFINITY),
            ("-inf", f64::NEG_INFINITY),
        ] {
            let error = FileSource::new(
                path.as_str(),
                Some(IqFileFormat::Cu8),
                Some(1.0e6),
                Some(bad_freq),
            )
            .expect_err("non-finite center frequency must be rejected, not stored");
            let message = error.to_string().to_lowercase();
            // Must name the offending item so the operator knows which input to fix.
            assert!(
                message.contains("center") || message.contains("frequen"),
                "{label}: error must name the offending center frequency, got: {message}"
            );
            // Must name the flag that carries it, so the fix is unambiguous.
            assert!(
                message.contains("--file-center-frequency"),
                "{label}: error must name --file-center-frequency, got: {message}"
            );
        }
    }

    /// Non-finite center frequencies must be rejected on the **`SigMF`** path too.
    ///
    /// Two distinct routes reach `open_dataset` from a `SigMF` dataset, and both are
    /// covered here:
    ///
    /// 1. An explicit override (`--file-center-frequency`) applied to a `.sigmf-meta`
    ///    dataset. The sidecar is bypassed entirely, so this is the same reachable hole
    ///    as the raw-file case and is RED today.
    /// 2. A value *inside* the sidecar. This one cannot actually carry NaN/inf: JSON has
    ///    no such literal, and `serde_json` already rejects out-of-range magnitudes with
    ///    "number out of range", so the sidecar read fails loudly on its own. The
    ///    assertions below pin that existing defence rather than demanding new code —
    ///    worth keeping because it is the reason the sidecar route is not reachable, and
    ///    a future `serde_json` or sidecar hand-parse change could silently remove it.
    #[test]
    fn sigmf_non_finite_center_frequency_is_rejected() {
        let dir = TempDir::new("nonfinite-center-sigmf");
        let raw = cu8_bytes(&[(0, 0)]);
        let (meta_path, _data_path) = write_sigmf_pair(&dir, "airband", "cu8", 1.0e6, 1.0e8, &raw);

        // Route 1: explicit non-finite override on a SigMF dataset.
        for (label, bad_freq) in [
            ("NaN", f64::NAN),
            ("+inf", f64::INFINITY),
            ("-inf", f64::NEG_INFINITY),
        ] {
            let error = FileSource::new(meta_path.as_str(), None, None, Some(bad_freq))
                .expect_err("a non-finite center override on a SigMF dataset must be rejected");
            let message = error.to_string().to_lowercase();
            assert!(
                message.contains("center") || message.contains("frequen"),
                "{label}: error must name the offending center frequency, got: {message}"
            );
        }

        // Route 2: a magnitude f64 cannot hold. serde_json refuses the document itself.
        let overflow_meta = r#"{
            "global": {"core:datatype": "cu8", "core:sample_rate": 1000000.0},
            "captures": [{"core:sample_start": 0, "core:frequency": 1e400}]
        }"#;
        let meta_path = dir.write("overflow.sigmf-meta", overflow_meta.as_bytes());
        let _data = dir.write("overflow.sigmf-data", &raw);
        assert!(
            FileSource::new(meta_path.as_str(), None, None, None).is_err(),
            "SigMF center frequency 1e400 must be rejected, not coerced into a usable value"
        );

        // Route 2b: a non-numeric frequency is not a number; the source must not invent
        // one by falling back to a default.
        let stringy_meta = r#"{
            "global": {"core:datatype": "cu8", "core:sample_rate": 1000000.0},
            "captures": [{"core:sample_start": 0, "core:frequency": "not-a-number"}]
        }"#;
        let meta_path = dir.write("stringy.sigmf-meta", stringy_meta.as_bytes());
        let _data = dir.write("stringy.sigmf-data", &raw);
        let stringy = FileSource::new(meta_path.as_str(), None, None, None)
            .expect_err("SigMF center frequency `not-a-number` must be rejected");
        let message = stringy.to_string().to_lowercase();
        assert!(
            message.contains("frequency") || message.contains("frequen"),
            "error must name the missing/invalid capture frequency, got: {message}"
        );
    }

    /// Non-finite **sample rates** must be rejected for both raw files and `SigMF` pairs.
    ///
    /// The existing guard already checks `is_finite() && > 0.0`; this test pins it so a
    /// future refactor cannot drop the finiteness half. `inf` is the dangerous case: it
    /// is `> 0`, so a positivity-only check would let it through.
    #[test]
    fn non_finite_sample_rate_is_rejected() {
        let dir = TempDir::new("nonfinite-rate");
        let raw = cu8_bytes(&[(0, 0)]);
        let path = dir.write("raw.iq", &raw);

        for (label, bad_rate) in [("NaN", f64::NAN), ("+inf", f64::INFINITY)] {
            let error = FileSource::new(
                path.as_str(),
                Some(IqFileFormat::Cu8),
                Some(bad_rate),
                Some(1.0e8),
            )
            .expect_err("non-finite sample rate must be rejected");
            let message = error.to_string().to_lowercase();
            assert!(
                message.contains("sample rate") || message.contains("sample_rate"),
                "{label}: error must name the offending sample rate, got: {message}"
            );
        }

        // Same for the sidecar route: 1e400 overflows f64, so serde_json refuses the
        // document and the open fails instead of coercing the rate.
        let meta_overflow = r#"{
            "global": {"core:datatype": "cu8", "core:sample_rate": 1e400},
            "captures": [{"core:sample_start": 0, "core:frequency": 1.0e8}]
        }"#;
        let meta_path = dir.write("rate.sigmf-meta", meta_overflow.as_bytes());
        let _data = dir.write("rate.sigmf-data", &raw);
        assert!(
            FileSource::new(meta_path.as_str(), None, None, None).is_err(),
            "SigMF sample rate 1e400 must be rejected, not coerced"
        );
    }

    /// An absurdly small but finite sample rate must produce a **named error**, not a
    /// panic later in the pipeline.
    ///
    /// `1e-20` Hz passes `is_finite() && > 0.0`, then `capture_thread` computes
    /// `Duration::from_secs_f64(fft_size / sample_rate)`, which panics ("cannot convert
    /// float seconds to Duration"). Rejecting below a documented 1 Hz lower bound turns a
    /// thread panic into an actionable message. 1 Hz is the floor because a capture
    /// slower than one sample per second cannot drive a spectrum display at any useful
    /// frame rate, and it keeps the block duration (`fft_size / rate`) far inside
    /// `Duration`'s representable range for any sane `fft_size`.
    #[test]
    fn absurdly_small_sample_rate_is_a_named_error_not_a_panic() {
        let dir = TempDir::new("tiny-rate");
        let raw = cu8_bytes(&[(0, 0)]);
        let path = dir.write("raw.iq", &raw);

        for bad_rate in [1.0e-20_f64, 0.5] {
            let error = FileSource::new(
                path.as_str(),
                Some(IqFileFormat::Cu8),
                Some(bad_rate),
                Some(1.0e8),
            )
            .expect_err("sample rate below 1 Hz must be rejected at open time");
            let message = error.to_string().to_lowercase();
            assert!(
                message.contains("sample rate") || message.contains("sample_rate"),
                "error must name the offending sample rate for {bad_rate}, got: {message}"
            );
        }

        // 1 Hz itself is the documented floor and must remain usable.
        FileSource::new(
            path.as_str(),
            Some(IqFileFormat::Cu8),
            Some(1.0),
            Some(1.0e8),
        )
        .expect("1 Hz is the documented lower bound and must be accepted");
    }

    /// `cf32le` payloads containing NaN/inf must be rejected, naming the file and the
    /// reason, and must not leave a half-decoded block in the caller's buffer.
    ///
    /// Why an error and not sanitisation: this source's contract is "replay exactly what
    /// is on disk". Replacing a non-finite sample with 0.0 would silently invent data,
    /// and `inf` in particular is unrecoverable downstream — `max_db` in the FFT
    /// processor is an exponential moving average, so a single `inf` pinning it never
    /// decays and the waterfall stays destroyed for the rest of the session.
    ///
    /// The buffer-unmodified assertion is the block-atomicity half: because the contract
    /// is an error, the caller must not be handed a partially valid block on the failing
    /// call. It either gets a complete block or an error.
    #[test]
    fn cf32le_non_finite_sample_is_rejected_without_partial_block() {
        let dir = TempDir::new("cf32le-nonfinite");
        let sentinel = IqSample::new(7.5, -7.5);

        for (label, bad) in [
            ("pos-inf", f32::INFINITY),
            ("neg-inf", f32::NEG_INFINITY),
            ("nan", f32::NAN),
        ] {
            // A finite sample first, then the offending one: a lax implementation that
            // decodes as it goes would have already written buffer[0] by the time it
            // noticed, which is exactly the half-valid block this test forbids.
            let raw = cf32le_bytes(&[(0.5, -0.5), (bad, 0.0)]);
            let path = dir.write(&format!("{label}.cf32le"), &raw);

            let mut source = FileSource::new(
                path.as_str(),
                Some(IqFileFormat::Cf32Le),
                Some(1.0e6),
                Some(1.0e8),
            )
            .expect("open cf32le file with a non-finite payload sample");

            let mut buffer = [sentinel; 2];
            let error = source
                .read_samples(&mut buffer)
                .expect_err("a non-finite cf32le sample must be an error, not silent data");
            let message = error.to_string().to_lowercase();

            assert!(
                message.contains("non-finite"),
                "{label}: error must state the reason `non-finite sample`, got: {message}"
            );
            // Naming the file is what makes a multi-file replay debuggable.
            assert!(
                message.contains(label),
                "{label}: error must name the offending file, got: {message}"
            );

            // Block atomicity: the failing call must not have written a partial block.
            // What happens on a *second* read after this error is deliberately not
            // asserted: the capture thread stops the pipeline on any error, so post-error
            // read behavior is unconstrained by contract.
            assert_eq!(
                buffer[0], sentinel,
                "{label}: first sample was written before the non-finite check, caller \
                 would process a half-valid block"
            );
            assert_eq!(
                buffer[1], sentinel,
                "{label}: offending slot was mutated despite the error"
            );
        }
    }

    /// A `cf32le` payload that is entirely finite keeps working — the non-finite check
    /// must not become a blanket rejection of the format.
    #[test]
    fn cf32le_finite_payload_still_reads_after_non_finite_guard() {
        let dir = TempDir::new("cf32le-finite-after-guard");
        let raw = cf32le_bytes(&[(0.25, -0.75), (1.5, -2.5)]);
        let path = dir.write("finite.cf32le", &raw);

        let mut source = FileSource::new(
            path.as_str(),
            Some(IqFileFormat::Cf32Le),
            Some(1.0e6),
            Some(1.0e8),
        )
        .expect("open finite cf32le file");
        let mut buffer = [IqSample::new(0.0, 0.0); 2];

        assert_eq!(
            source.read_samples(&mut buffer).expect("finite read"),
            2,
            "finite payload must still produce its samples"
        );
        assert_abs_diff_eq!(buffer[0].re, 0.25, epsilon = 1e-6);
        assert_abs_diff_eq!(buffer[1].im, -2.5, epsilon = 1e-6);
    }

    /// `ci8` is a `SigMF` datatype this build deliberately does not support, and the
    /// rejection must name it.
    ///
    /// Ruling recorded here because the plan's wording is ambiguous ("cu8/ci8
    /// interleaved unsigned 8-bit IQ" is self-contradictory): the plan enumerates only
    /// `--iq-format cu8|cf32le` and specifies no scaling convention for *signed* 8-bit
    /// samples, so `ci8` (complex signed int8) is out of scope. A signed 8-bit capture
    /// therefore fails loudly with its datatype named, rather than being decoded with
    /// the unsigned cu8 DC offset and silently producing a distorted spectrum.
    #[test]
    fn ci8_sigmf_datatype_is_rejected_by_name() {
        let dir = TempDir::new("sigmf-ci8");
        let raw = vec![0_u8; 8];
        let (meta_path, _data_path) =
            write_sigmf_pair(&dir, "signed", "ci8", 1_000_000.0, 100_000_000.0, &raw);

        let error = FileSource::new(meta_path.as_str(), None, None, None)
            .expect_err("ci8 is out of scope and must be rejected, not guessed at");
        let message = error.to_string().to_lowercase();
        assert!(
            message.contains("ci8"),
            "error must name the unsupported datatype `ci8`, got: {message}"
        );
        assert!(
            message.contains("support"),
            "error must state that ci8 is unsupported, got: {message}"
        );
    }

    /// A directory passed as the IQ file must be rejected at **open** time.
    ///
    /// This is the reachable trigger for the pipeline hang: `File::open` on a directory
    /// succeeds on Linux (verified: `Ok`, `metadata().len() == 40`), so `open_dataset`
    /// accepts it, and the *first* `read` then fails with `EISDIR`. Because the raw-file
    /// flags let any path through, `termsdr --file <a directory>` is a real invocation,
    /// and the resulting read error leaves the capture loop's `running` flag set, so a
    /// headless server never exits (measured: exit 124 after a 20 s timeout, versus
    /// ~106 ms for a clean EOF).
    ///
    /// Failing fast at open is the correct place: the dataset is known to be unusable
    /// before a single sample is read, and the existing "malformed byte count" rule
    /// already rejects unusable datasets in `new()` rather than on first read. This does
    /// not by itself fix the `running` flag — a file truncated mid-sample while open
    /// still errors during a read — so it complements, rather than replaces, the
    /// read-outcome stop contract.
    #[test]
    fn directory_path_is_rejected_at_open_time() {
        let dir = TempDir::new("dir-as-file");
        let dir_path = dir.path.to_str().expect("invariant: temp paths are UTF-8");

        let error = FileSource::new(
            dir_path,
            Some(IqFileFormat::Cu8),
            Some(1_024_000.0),
            Some(1.0e8),
        )
        .expect_err("a directory is not an IQ dataset and must be rejected at open time");
        let message = error.to_string().to_lowercase();
        assert!(
            message.contains("directory") || message.contains("not a file"),
            "error must state that the path is not a file, got: {message}"
        );
    }
}
