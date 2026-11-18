//! Native, source-independent `TermSDR` session persistence.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::audio::demod::DemodMode;

/// Current native session schema version.
pub const SESSION_VERSION: u32 = 1;

/// Smallest receiver tuning position accepted from a session, in Hz.
///
/// Zero and negative tunings are physically meaningless (there is no negative RF
/// frequency here), and 1 Hz is the finest step any supported hardware can select.
const MIN_CENTER_FREQUENCY_HZ: f64 = 1.0;

/// Largest receiver tuning position accepted from a session, in Hz.
///
/// 100 GHz is two orders of magnitude above the 6 GHz ceiling of the widest-coverage
/// RTL-SDR tuner, so it rejects the finite-but-nonsense values (`f64::MAX`) that
/// would otherwise be handed to a tuner driver or used as an FFT axis origin.
const MAX_CENTER_FREQUENCY_HZ: f64 = 100_000_000_000.0;

/// Smallest sample rate accepted from a session, in Hz.
///
/// A rate below 1 Hz cannot represent any signal the DSP chain demodulates.
const MIN_SAMPLE_RATE_HZ: f64 = 1.0;

/// Largest sample rate accepted from a session, in Hz.
///
/// 100 MS/s is well past any rate the capture layer can sustain (the RTL-SDR tops
/// out at 3.2 MS/s), and bounding it keeps the derived FFT/decimation buffer sizes
/// finite.
const MAX_SAMPLE_RATE_HZ: f64 = 100_000_000.0;

/// Smallest FFT size accepted from a session, in bins.
///
/// The spectrum display needs enough bins to resolve a signal; 16 is the smallest
/// power-of-two the renderer treats as a usable frame.
const MIN_FFT_SIZE: usize = 16;

/// Largest FFT size accepted from a session, in bins.
///
/// An FFT allocates two `Vec<Complex<f32>>` of this length. 4 Mi bins is already
/// 32 MiB per buffer, so anything larger (`usize::MAX` included) would abort on
/// allocation instead of failing validation.
const MAX_FFT_SIZE: usize = 4_194_304;

/// Smallest manual RF gain accepted from a session, in dB.
///
/// Tuner gain stages bottom out around -10 dB; -100 dB leaves a generous margin
/// while still rejecting `f32::MIN`-scale values.
const MIN_GAIN_DB: f32 = -100.0;

/// Largest manual RF gain accepted from a session, in dB.
///
/// The strongest RTL-SDR gain step is ~50 dB, so 100 dB is a safe upper sanity
/// bound that rejects `f32::MAX` without narrowing real hardware.
const MAX_GAIN_DB: f32 = 100.0;

/// Smallest PPM correction accepted from a session.
///
/// Real crystals drift by tens of PPM; +/-1000 PPM is far outside any useful
/// correction and would shift a 100 MHz tuning by 100 kHz.
const MIN_PPM: i32 = -1_000;

/// Largest PPM correction accepted from a session.
///
/// See [`MIN_PPM`].
const MAX_PPM: i32 = 1_000;

/// Largest VFO id that leaves room for the next two allocations.
///
/// The UI and server both derive the next id as `max(id) + 1` and then increment
/// it per VFO. An id above `usize::MAX - 2` cannot support that arithmetic, so such
/// a session is rejected instead of being allowed to overflow after load.
const MAX_VFO_ID: usize = usize::MAX - 2;

/// A complete durable `TermSDR` session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Schema version.
    pub version: u32,
    /// Receiver settings that do not select an input source.
    pub receiver: ReceiverSession,
    /// Main cursor state.
    pub cursor: TunedChannelSession,
    /// Pinned VFO state.
    pub vfos: Vec<VfoSession>,
    /// Audio processing state.
    pub audio: AudioSession,
    /// Spectrum and waterfall display state.
    pub display: DisplaySession,
    /// Scanner state.
    pub scan: ScanSession,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiverSession {
    pub center_frequency_hz: f64,
    pub sample_rate_hz: f64,
    pub fft_size: usize,
    pub gain_db: Option<f32>,
    pub ppm: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunedChannelSession {
    pub name: String,
    pub frequency_hz: f64,
    pub mode: SessionMode,
    pub channel_bandwidth_hz: f64,
    pub muted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfoSession {
    pub id: usize,
    pub name: String,
    pub frequency_hz: f64,
    pub mode: SessionMode,
    pub channel_bandwidth_hz: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSession {
    pub volume: u8,
    pub global_squelch_dbfs: Option<f32>,
    pub agc: AgcSession,
    pub bandpass: BandpassSession,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgcSession {
    pub enabled: bool,
    pub attack_ms: f64,
    pub decay_ms: f64,
    pub target_level: f32,
    pub max_gain_db: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandpassSession {
    pub enabled: bool,
    pub low_hz: f64,
    pub high_hz: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplaySession {
    pub min_db: f32,
    pub max_db: f32,
    pub averaging: usize,
    pub waterfall_span_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanSession {
    pub active: bool,
    pub interval_ms: u64,
}

/// Demodulator spelling used by the native JSON schema.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionMode {
    Am,
    Fm,
    Nfm,
}

impl From<DemodMode> for SessionMode {
    fn from(value: DemodMode) -> Self {
        match value {
            DemodMode::AM => Self::Am,
            DemodMode::FM => Self::Fm,
            DemodMode::NFM => Self::Nfm,
        }
    }
}

impl From<SessionMode> for DemodMode {
    fn from(value: SessionMode) -> Self {
        match value {
            SessionMode::Am => Self::AM,
            SessionMode::Fm => Self::FM,
            SessionMode::Nfm => Self::NFM,
        }
    }
}

/// Load and validate a native session without mutating application state.
///
/// # Errors
/// Returns an error for I/O, malformed JSON, unsupported versions, or invalid values.
pub fn load(path: impl AsRef<Path>) -> Result<Session> {
    let path = path.as_ref();
    let bytes =
        fs::read(path).with_context(|| format!("failed to read session {}", path.display()))?;
    let session: Session = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse session {}", path.display()))?;
    if session.version != SESSION_VERSION {
        bail!("unsupported session version {}", session.version);
    }
    session.validate()?;
    Ok(session)
}

impl Session {
    fn validate(&self) -> Result<()> {
        let finite = [
            self.receiver.center_frequency_hz,
            self.receiver.sample_rate_hz,
            self.cursor.frequency_hz,
            self.cursor.channel_bandwidth_hz,
            self.audio.agc.attack_ms,
            self.audio.agc.decay_ms,
            f64::from(self.audio.agc.target_level),
            f64::from(self.audio.agc.max_gain_db),
            self.audio.bandpass.low_hz,
            self.audio.bandpass.high_hz,
            f64::from(self.display.min_db),
            f64::from(self.display.max_db),
            self.display.waterfall_span_seconds,
        ];
        if finite.iter().any(|value| !value.is_finite())
            || self
                .vfos
                .iter()
                .any(|vfo| !vfo.frequency_hz.is_finite() || !vfo.channel_bandwidth_hz.is_finite())
            || self.receiver.gain_db.is_some_and(|gain| !gain.is_finite())
            || self
                .audio
                .global_squelch_dbfs
                .is_some_and(|squelch| !squelch.is_finite())
        {
            bail!("session contains a non-finite numeric value");
        }
        let valid_bandwidth = |bandwidth: f64| (1_000.0..=250_000.0).contains(&bandwidth);
        let mut vfo_ids = HashSet::with_capacity(self.vfos.len());
        if !(MIN_CENTER_FREQUENCY_HZ..=MAX_CENTER_FREQUENCY_HZ)
            .contains(&self.receiver.center_frequency_hz)
            || !(MIN_SAMPLE_RATE_HZ..=MAX_SAMPLE_RATE_HZ).contains(&self.receiver.sample_rate_hz)
            || !(MIN_FFT_SIZE..=MAX_FFT_SIZE).contains(&self.receiver.fft_size)
            || self
                .receiver
                .gain_db
                .is_some_and(|gain| !(MIN_GAIN_DB..=MAX_GAIN_DB).contains(&gain))
            || !(MIN_PPM..=MAX_PPM).contains(&self.receiver.ppm)
            || self.cursor.frequency_hz <= 0.0
            || !valid_bandwidth(self.cursor.channel_bandwidth_hz)
            || self.vfos.len() > 8
            || self.vfos.iter().any(|vfo| {
                vfo.id == 0
                    || vfo.id > MAX_VFO_ID
                    || !vfo_ids.insert(vfo.id)
                    || vfo.frequency_hz <= 0.0
                    || !valid_bandwidth(vfo.channel_bandwidth_hz)
            })
            || self.audio.volume > 100
            || self
                .audio
                .global_squelch_dbfs
                .is_some_and(|squelch| !(-120.0..=0.0).contains(&squelch))
            || !(1.0..=100.0).contains(&self.audio.agc.attack_ms)
            || !(50.0..=2_000.0).contains(&self.audio.agc.decay_ms)
            || !(0.05..=1.0).contains(&self.audio.agc.target_level)
            || !(1.0..=100.0).contains(&self.audio.agc.max_gain_db)
            || !(50.0..=1_000.0).contains(&self.audio.bandpass.low_hz)
            || !(1_000.0..=8_000.0).contains(&self.audio.bandpass.high_hz)
            || self.audio.bandpass.low_hz >= self.audio.bandpass.high_hz
            || self.display.min_db >= self.display.max_db
            || !(1..=20).contains(&self.display.averaging)
            || !(5.0..=120.0).contains(&self.display.waterfall_span_seconds)
            || !(50..=5_000).contains(&self.scan.interval_ms)
        {
            bail!("session contains an out-of-range value");
        }
        Ok(())
    }
}

/// Atomically write a session to the exact requested path.
///
/// Parent directories are created, but no extension is appended.
///
/// # Errors
/// Returns an error if serialization, directory creation, writing, or replacement fails.
pub fn save(path: impl AsRef<Path>, session: &Session) -> Result<()> {
    session.validate()?;
    let path = path.as_ref();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create session directory {}", parent.display()))?;

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut attempt = 0_u32;
    let (temp_path, file) = loop {
        let temp_name = format!(
            ".termsdr-session-{}-{stamp}-{attempt}.tmp",
            std::process::id()
        );
        let temp_path: PathBuf = parent.join(temp_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => break (temp_path, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt = attempt
                    .checked_add(1)
                    .context("temporary session name exhausted")?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to create temporary session {}", temp_path.display())
                });
            }
        }
    };
    let result = (|| -> Result<()> {
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, session)
            .context("failed to serialize session")?;
        writer
            .write_all(b"\n")
            .context("failed to finish session JSON")?;
        writer.flush().context("failed to flush session")?;
        writer
            .get_ref()
            .sync_all()
            .context("failed to sync session")?;
        fs::rename(&temp_path, path)
            .with_context(|| format!("failed to replace session {}", path.display()))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("failed to sync session directory {}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}
