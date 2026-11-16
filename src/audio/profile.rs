//! Typed AM audio configuration profiles and their invariants.

use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Minimum supported playback prebuffer duration, in milliseconds.
pub const MIN_PREBUFFER_MS: u16 = 50;
/// Maximum supported playback prebuffer duration, in milliseconds.
pub const MAX_PREBUFFER_MS: u16 = 150;
/// Default playback prebuffer duration, in milliseconds.
pub const DEFAULT_PREBUFFER_MS: u16 = 100;

/// Nominal RF bandwidth profile for AM reception.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AmRfProfile {
    /// 6 kHz nominal RF bandwidth.
    #[serde(rename = "6khz")]
    Khz6,
    /// 8 kHz nominal RF bandwidth.
    #[default]
    #[serde(rename = "8khz")]
    Khz8,
    /// 10 kHz nominal RF bandwidth.
    #[serde(rename = "10khz")]
    Khz10,
    /// 12 kHz nominal RF bandwidth.
    #[serde(rename = "12khz")]
    Khz12,
}

impl AmRfProfile {
    /// Returns the nominal two-sided RF bandwidth in hertz.
    #[must_use]
    pub const fn bandwidth_hz(self) -> f32 {
        match self {
            Self::Khz6 => 6_000.0,
            Self::Khz8 => 8_000.0,
            Self::Khz10 => 10_000.0,
            Self::Khz12 => 12_000.0,
        }
    }

    /// Returns the stable human-readable profile name.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Khz6 => "6 kHz",
            Self::Khz8 => "8 kHz",
            Self::Khz10 => "10 kHz",
            Self::Khz12 => "12 kHz",
        }
    }

    /// Derives the RF filter edges and usable voice high cutoff, all in hertz.
    ///
    /// The pass edge is the lesser of 200 Hz above the selected voice cutoff
    /// and 45% of the nominal RF bandwidth. The stop edge is 55% of the RF
    /// bandwidth. If those edges would overlap, a 200 Hz transition guard is
    /// retained below the stop edge. The usable voice edge remains 100 Hz
    /// below the resulting pass edge.
    #[must_use]
    pub fn effective_edges_hz(self, voice: VoiceProfile) -> EffectiveEdgesHz {
        let bandwidth_hz = self.bandwidth_hz();
        let stop_edge_hz = 0.55 * bandwidth_hz;
        let requested_pass_hz = (voice.high_cutoff_hz() + 200.0).min(0.45 * bandwidth_hz);
        let pass_edge_hz = if requested_pass_hz >= stop_edge_hz {
            stop_edge_hz - 200.0
        } else {
            requested_pass_hz
        };
        let effective_voice_high_hz = voice.high_cutoff_hz().min(pass_edge_hz - 100.0);

        EffectiveEdgesHz {
            pass_edge_hz,
            stop_edge_hz,
            effective_voice_high_hz,
        }
    }
}

/// Voice-band filter profile for demodulated AM audio.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceProfile {
    /// 300–2,800 Hz voice band.
    Narrow,
    /// 250–3,400 Hz voice band.
    #[default]
    Standard,
    /// 150–4,000 Hz voice band.
    Wide,
}

impl VoiceProfile {
    /// Returns the voice-band low cutoff in hertz.
    #[must_use]
    pub const fn low_cutoff_hz(self) -> f32 {
        match self {
            Self::Narrow => 300.0,
            Self::Standard => 250.0,
            Self::Wide => 150.0,
        }
    }

    /// Returns the requested voice-band high cutoff in hertz.
    #[must_use]
    pub const fn high_cutoff_hz(self) -> f32 {
        match self {
            Self::Narrow => 2_800.0,
            Self::Standard => 3_400.0,
            Self::Wide => 4_000.0,
        }
    }

    /// Returns the stable human-readable profile name.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Narrow => "Narrow",
            Self::Standard => "Standard",
            Self::Wide => "Wide",
        }
    }
}

/// Squelch threshold selection for AM audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SquelchMode {
    /// Track the measured noise floor with the given margin in decibels.
    Auto {
        /// Margin above the measured noise floor, in decibels.
        margin_db: f32,
    },
    /// Use a fixed full-scale-relative threshold.
    Manual {
        /// Fixed opening threshold in dBFS.
        threshold_dbfs: f32,
    },
    /// Keep squelch unconditionally open.
    Open,
}

/// Error returned when a squelch threshold is not a finite number.
#[derive(Debug, Clone, Copy, PartialEq, Error)]
pub enum SquelchValidationError {
    /// The automatic noise-floor margin was NaN or infinite.
    #[error("automatic squelch margin {margin_db} dB is not finite")]
    NonFiniteAutoMargin {
        /// Rejected automatic margin in decibels.
        margin_db: f32,
    },
    /// The fixed manual threshold was NaN or infinite.
    #[error("manual squelch threshold {threshold_dbfs} dBFS is not finite")]
    NonFiniteManualThreshold {
        /// Rejected manual threshold in dBFS.
        threshold_dbfs: f32,
    },
}

impl SquelchMode {
    /// Validates that any configured numeric threshold is finite.
    ///
    /// # Errors
    ///
    /// Returns [`SquelchValidationError`] when an automatic margin or manual
    /// threshold is NaN or infinite.
    pub fn validate(self) -> Result<(), SquelchValidationError> {
        match self {
            Self::Auto { margin_db } if !margin_db.is_finite() => {
                Err(SquelchValidationError::NonFiniteAutoMargin { margin_db })
            }
            Self::Manual { threshold_dbfs } if !threshold_dbfs.is_finite() => {
                Err(SquelchValidationError::NonFiniteManualThreshold { threshold_dbfs })
            }
            Self::Auto { .. } | Self::Manual { .. } | Self::Open => Ok(()),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum SquelchModeSerde {
    Auto { margin_db: f32 },
    Manual { threshold_dbfs: f32 },
    Open,
}

impl From<SquelchMode> for SquelchModeSerde {
    fn from(value: SquelchMode) -> Self {
        match value {
            SquelchMode::Auto { margin_db } => Self::Auto { margin_db },
            SquelchMode::Manual { threshold_dbfs } => Self::Manual { threshold_dbfs },
            SquelchMode::Open => Self::Open,
        }
    }
}

impl From<SquelchModeSerde> for SquelchMode {
    fn from(value: SquelchModeSerde) -> Self {
        match value {
            SquelchModeSerde::Auto { margin_db } => Self::Auto { margin_db },
            SquelchModeSerde::Manual { threshold_dbfs } => Self::Manual { threshold_dbfs },
            SquelchModeSerde::Open => Self::Open,
        }
    }
}

impl Serialize for SquelchMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        SquelchModeSerde::from(*self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SquelchMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Self::from(SquelchModeSerde::deserialize(deserializer)?);
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

impl Default for SquelchMode {
    fn default() -> Self {
        Self::Auto { margin_db: 6.0 }
    }
}

/// Complete user-selected AM audio profile.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AmAudioProfile {
    /// Nominal AM RF bandwidth.
    pub rf: AmRfProfile,
    /// Demodulated voice-band shape.
    pub voice: VoiceProfile,
    /// Squelch threshold behavior.
    pub squelch: SquelchMode,
    /// Playback prebuffer duration in milliseconds; valid range is 50–150 ms.
    pub prebuffer_ms: u16,
}

/// Error returned when an AM audio profile violates a public invariant.
#[derive(Debug, Clone, Copy, PartialEq, Error)]
pub enum AmAudioProfileValidationError {
    /// The playback prebuffer is outside the supported range.
    #[error(transparent)]
    Prebuffer(#[from] PrebufferValidationError),
    /// The squelch configuration contains a non-finite value.
    #[error(transparent)]
    Squelch(#[from] SquelchValidationError),
}

impl AmAudioProfile {
    /// Validates the playback prebuffer and squelch invariants.
    ///
    /// # Errors
    ///
    /// Returns [`AmAudioProfileValidationError`] when the prebuffer is outside
    /// the supported 50–150 ms range or a squelch value is not finite.
    pub fn validate(&self) -> Result<(), AmAudioProfileValidationError> {
        validate_prebuffer_ms(self.prebuffer_ms)?;
        self.squelch.validate()?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct AmAudioProfileSerde {
    rf: AmRfProfile,
    voice: VoiceProfile,
    squelch: SquelchMode,
    prebuffer_ms: u16,
}

impl<'de> Deserialize<'de> for AmAudioProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = AmAudioProfileSerde::deserialize(deserializer)?;
        let profile = Self {
            rf: value.rf,
            voice: value.voice,
            squelch: value.squelch,
            prebuffer_ms: value.prebuffer_ms,
        };
        profile.validate().map_err(D::Error::custom)?;
        Ok(profile)
    }
}

impl Default for AmAudioProfile {
    fn default() -> Self {
        Self {
            rf: AmRfProfile::default(),
            voice: VoiceProfile::default(),
            squelch: SquelchMode::default(),
            prebuffer_ms: DEFAULT_PREBUFFER_MS,
        }
    }
}

/// Effective AM filter edges, expressed explicitly in hertz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectiveEdgesHz {
    /// End of the RF filter pass band, in hertz from the channel center.
    pub pass_edge_hz: f32,
    /// Start of the RF filter stop band, in hertz from the channel center.
    pub stop_edge_hz: f32,
    /// Highest usable voice frequency after RF constraints, in hertz.
    pub effective_voice_high_hz: f32,
}

/// Error returned when a playback prebuffer duration is outside 50–150 ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "prebuffer duration {configured_ms} ms is outside the supported {min_ms}–{max_ms} ms range"
)]
pub struct PrebufferValidationError {
    /// Rejected prebuffer duration in milliseconds.
    pub configured_ms: u16,
    /// Minimum supported prebuffer duration in milliseconds.
    pub min_ms: u16,
    /// Maximum supported prebuffer duration in milliseconds.
    pub max_ms: u16,
}

/// Validates that a playback prebuffer duration is within 50–150 ms inclusive.
///
/// # Errors
///
/// Returns [`PrebufferValidationError`] with the rejected value and supported
/// boundaries when `configured_ms` is outside the valid range.
pub const fn validate_prebuffer_ms(configured_ms: u16) -> Result<u16, PrebufferValidationError> {
    if configured_ms < MIN_PREBUFFER_MS || configured_ms > MAX_PREBUFFER_MS {
        Err(PrebufferValidationError {
            configured_ms,
            min_ms: MIN_PREBUFFER_MS,
            max_ms: MAX_PREBUFFER_MS,
        })
    } else {
        Ok(configured_ms)
    }
}

/// Clamps a playback prebuffer duration to the inclusive 50–150 ms range.
#[must_use]
pub const fn clamp_prebuffer_ms(configured_ms: u16) -> u16 {
    if configured_ms < MIN_PREBUFFER_MS {
        MIN_PREBUFFER_MS
    } else if configured_ms > MAX_PREBUFFER_MS {
        MAX_PREBUFFER_MS
    } else {
        configured_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;
    use serde_json::{json, Value};

    /// Rounding tolerance, in hertz, for values derived from f32 arithmetic.
    const EDGE_EPSILON_HZ: f32 = 0.01;

    fn profile_json(prebuffer_ms: u16, squelch: Value) -> Value {
        json!({
            "rf": "8khz",
            "voice": "standard",
            "squelch": squelch,
            "prebuffer_ms": prebuffer_ms,
        })
    }

    #[test]
    fn profile_deserialization_accepts_prebuffer_boundaries() {
        for valid_ms in [MIN_PREBUFFER_MS, MAX_PREBUFFER_MS] {
            let profile = serde_json::from_value::<AmAudioProfile>(profile_json(
                valid_ms,
                json!({"mode": "open"}),
            ))
            .expect("inclusive prebuffer boundary must deserialize");

            assert_eq!(profile.prebuffer_ms, valid_ms);
        }
    }

    #[test]
    fn profile_deserialization_rejects_prebuffer_outside_supported_range() {
        for invalid_ms in [0, 49, 151, u16::MAX] {
            let result = serde_json::from_value::<AmAudioProfile>(profile_json(
                invalid_ms,
                json!({"mode": "open"}),
            ));

            assert!(result.is_err(), "{invalid_ms} ms must be rejected");
        }
    }

    #[test]
    fn profile_deserialization_error_mentions_supported_prebuffer_range() {
        let error = serde_json::from_value::<AmAudioProfile>(profile_json(
            MIN_PREBUFFER_MS - 1,
            json!({"mode": "open"}),
        ))
        .expect_err("out-of-range prebuffer must fail deserialization")
        .to_string();

        assert!(
            error.contains(&MIN_PREBUFFER_MS.to_string()),
            "error must mention minimum {MIN_PREBUFFER_MS} ms: {error}"
        );
        assert!(
            error.contains(&MAX_PREBUFFER_MS.to_string()),
            "error must mention maximum {MAX_PREBUFFER_MS} ms: {error}"
        );
        let lowercase_error = error.to_lowercase();
        assert!(
            lowercase_error.contains("supported") && lowercase_error.contains("range"),
            "error must identify the supported prebuffer range: {error}"
        );
    }

    #[test]
    fn prebuffer_validation_error_reports_rejected_value_and_boundaries() {
        let error = validate_prebuffer_ms(MIN_PREBUFFER_MS - 1)
            .expect_err("helper must reject a value below the minimum");

        assert_eq!(error.configured_ms, MIN_PREBUFFER_MS - 1);
        assert_eq!(error.min_ms, MIN_PREBUFFER_MS);
        assert_eq!(error.max_ms, MAX_PREBUFFER_MS);
    }

    #[test]
    fn non_finite_squelch_values_cannot_be_serialized_as_json() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                serde_json::to_string(&SquelchMode::Auto { margin_db: value }).is_err(),
                "auto margin {value:?} must not serialize (including as null)"
            );
            assert!(
                serde_json::to_string(&SquelchMode::Manual {
                    threshold_dbfs: value,
                })
                .is_err(),
                "manual threshold {value:?} must not serialize (including as null)"
            );
        }
    }

    #[test]
    fn profiles_with_non_finite_squelch_cannot_be_serialized_as_json() {
        for squelch in [
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
            let profile = AmAudioProfile {
                squelch,
                ..AmAudioProfile::default()
            };

            assert!(
                serde_json::to_string(&profile).is_err(),
                "profile with {squelch:?} must not serialize (including as null)"
            );
        }
    }

    #[test]
    fn finite_squelch_modes_pass_typed_validation() {
        for squelch in [
            SquelchMode::Auto { margin_db: 0.0 },
            SquelchMode::Manual {
                threshold_dbfs: -72.5,
            },
            SquelchMode::Open,
        ] {
            assert!(
                squelch.validate().is_ok(),
                "finite squelch mode must validate: {squelch:?}"
            );
        }
    }

    #[test]
    fn non_finite_squelch_modes_fail_typed_validation() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                SquelchMode::Auto { margin_db: value }.validate().is_err(),
                "auto margin {value:?} must fail validation"
            );
            assert!(
                SquelchMode::Manual {
                    threshold_dbfs: value,
                }
                .validate()
                .is_err(),
                "manual threshold {value:?} must fail validation"
            );
        }
    }

    #[test]
    fn valid_profiles_pass_typed_validation() {
        for squelch in [
            SquelchMode::Auto { margin_db: 6.0 },
            SquelchMode::Manual {
                threshold_dbfs: -68.5,
            },
            SquelchMode::Open,
        ] {
            let profile = AmAudioProfile {
                squelch,
                ..AmAudioProfile::default()
            };

            assert!(
                profile.validate().is_ok(),
                "valid profile must pass validation: {profile:?}"
            );
        }
    }

    #[test]
    fn profile_typed_validation_rejects_invalid_public_invariants() {
        for prebuffer_ms in [0, 49, 151, u16::MAX] {
            let profile = AmAudioProfile {
                prebuffer_ms,
                ..AmAudioProfile::default()
            };
            assert!(
                profile.validate().is_err(),
                "profile must reject prebuffer {prebuffer_ms} ms"
            );
        }

        for squelch in [
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
            let profile = AmAudioProfile {
                squelch,
                ..AmAudioProfile::default()
            };
            assert!(
                profile.validate().is_err(),
                "profile must reject non-finite squelch: {squelch:?}"
            );
        }
    }

    #[test]
    fn valid_squelch_and_profile_json_shapes_remain_unchanged() {
        let squelch = SquelchMode::Manual {
            threshold_dbfs: -68.5,
        };
        assert_eq!(
            serde_json::to_value(squelch).expect("finite squelch must serialize"),
            json!({"mode": "manual", "threshold_dbfs": -68.5})
        );

        let profile = AmAudioProfile {
            rf: AmRfProfile::Khz10,
            voice: VoiceProfile::Wide,
            squelch,
            prebuffer_ms: 125,
        };
        assert_eq!(
            serde_json::to_value(profile).expect("valid profile must serialize"),
            json!({
                "rf": "10khz",
                "voice": "wide",
                "squelch": {"mode": "manual", "threshold_dbfs": -68.5},
                "prebuffer_ms": 125,
            })
        );
    }

    #[test]
    fn rf_profiles_map_to_nominal_bandwidth_hz() {
        let cases = [
            (AmRfProfile::Khz6, 6_000.0_f32, "6 kHz"),
            (AmRfProfile::Khz8, 8_000.0, "8 kHz"),
            (AmRfProfile::Khz10, 10_000.0, "10 kHz"),
            (AmRfProfile::Khz12, 12_000.0, "12 kHz"),
        ];

        for (rf, expected_hz, expected_name) in cases {
            assert_abs_diff_eq!(rf.bandwidth_hz(), expected_hz, epsilon = f32::EPSILON);
            assert_eq!(rf.display_name(), expected_name);
        }
    }

    #[test]
    fn voice_profiles_map_to_low_and_high_cutoffs_hz() {
        let cases = [
            (VoiceProfile::Narrow, 300.0_f32, 2_800.0, "Narrow"),
            (VoiceProfile::Standard, 250.0, 3_400.0, "Standard"),
            (VoiceProfile::Wide, 150.0, 4_000.0, "Wide"),
        ];

        for (voice, expected_low_hz, expected_high_hz, expected_name) in cases {
            assert_abs_diff_eq!(
                voice.low_cutoff_hz(),
                expected_low_hz,
                epsilon = f32::EPSILON
            );
            assert_abs_diff_eq!(
                voice.high_cutoff_hz(),
                expected_high_hz,
                epsilon = f32::EPSILON
            );
            assert_eq!(voice.display_name(), expected_name);
        }
    }

    #[test]
    fn defaults_match_authoritative_plan_values() {
        assert_eq!(MIN_PREBUFFER_MS, 50);
        assert_eq!(MAX_PREBUFFER_MS, 150);
        assert_eq!(DEFAULT_PREBUFFER_MS, 100);

        assert_eq!(AmRfProfile::default(), AmRfProfile::Khz8);
        assert_eq!(VoiceProfile::default(), VoiceProfile::Standard);

        match SquelchMode::default() {
            SquelchMode::Auto { margin_db } => {
                assert_abs_diff_eq!(margin_db, 6.0, epsilon = f32::EPSILON);
            }
            other => panic!("default squelch must be automatic with 6 dB margin: {other:?}"),
        }

        let profile = AmAudioProfile::default();
        assert_eq!(profile.rf, AmRfProfile::Khz8);
        assert_eq!(profile.voice, VoiceProfile::Standard);
        assert_eq!(
            profile.squelch,
            SquelchMode::Auto { margin_db: 6.0 },
            "default profile must carry the automatic 6 dB squelch margin"
        );
        assert_eq!(profile.prebuffer_ms, 100);
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn prebuffer_validation_accepts_inclusive_range_only() {
        for valid_ms in [
            MIN_PREBUFFER_MS,
            MIN_PREBUFFER_MS + 1,
            75,
            DEFAULT_PREBUFFER_MS,
            MAX_PREBUFFER_MS - 1,
            MAX_PREBUFFER_MS,
        ] {
            assert_eq!(
                validate_prebuffer_ms(valid_ms),
                Ok(valid_ms),
                "{valid_ms} ms is inside the inclusive range"
            );
        }

        for invalid_ms in [0, MIN_PREBUFFER_MS - 1, MAX_PREBUFFER_MS + 1, u16::MAX] {
            let error = validate_prebuffer_ms(invalid_ms)
                .expect_err("out-of-range prebuffer must be rejected");

            assert_eq!(error.configured_ms, invalid_ms);
            assert_eq!(error.min_ms, MIN_PREBUFFER_MS);
            assert_eq!(error.max_ms, MAX_PREBUFFER_MS);
        }
    }

    #[test]
    fn prebuffer_clamping_saturates_to_inclusive_range() {
        let cases = [
            (0, MIN_PREBUFFER_MS),
            (MIN_PREBUFFER_MS - 1, MIN_PREBUFFER_MS),
            (MIN_PREBUFFER_MS, MIN_PREBUFFER_MS),
            (MIN_PREBUFFER_MS + 1, MIN_PREBUFFER_MS + 1),
            (DEFAULT_PREBUFFER_MS, DEFAULT_PREBUFFER_MS),
            (MAX_PREBUFFER_MS - 1, MAX_PREBUFFER_MS - 1),
            (MAX_PREBUFFER_MS, MAX_PREBUFFER_MS),
            (MAX_PREBUFFER_MS + 1, MAX_PREBUFFER_MS),
            (u16::MAX, MAX_PREBUFFER_MS),
        ];

        for (configured_ms, expected_ms) in cases {
            let clamped_ms = clamp_prebuffer_ms(configured_ms);

            assert_eq!(clamped_ms, expected_ms, "clamping {configured_ms} ms");
            assert!(
                validate_prebuffer_ms(clamped_ms).is_ok(),
                "clamped value {clamped_ms} ms must be valid"
            );
        }
    }

    #[test]
    fn rf_profiles_round_trip_through_json_tokens() {
        let cases = [
            (AmRfProfile::Khz6, "6khz"),
            (AmRfProfile::Khz8, "8khz"),
            (AmRfProfile::Khz10, "10khz"),
            (AmRfProfile::Khz12, "12khz"),
        ];

        for (value, token) in cases {
            let encoded = serde_json::to_value(value).expect("rf profile must serialize");

            assert_eq!(encoded, json!(token));
            assert_eq!(
                serde_json::from_value::<AmRfProfile>(encoded).expect("rf token must deserialize"),
                value
            );
        }
    }

    #[test]
    fn voice_profiles_round_trip_through_json_tokens() {
        let cases = [
            (VoiceProfile::Narrow, "narrow"),
            (VoiceProfile::Standard, "standard"),
            (VoiceProfile::Wide, "wide"),
        ];

        for (value, token) in cases {
            let encoded = serde_json::to_value(value).expect("voice profile must serialize");

            assert_eq!(encoded, json!(token));
            assert_eq!(
                serde_json::from_value::<VoiceProfile>(encoded)
                    .expect("voice token must deserialize"),
                value
            );
        }
    }

    #[test]
    fn every_squelch_mode_round_trips_through_json() {
        let cases = [
            SquelchMode::Auto { margin_db: 6.0 },
            SquelchMode::Manual {
                threshold_dbfs: -83.25,
            },
            SquelchMode::Open,
        ];

        for value in cases {
            let encoded = serde_json::to_value(value).expect("finite squelch must serialize");
            let decoded = serde_json::from_value::<SquelchMode>(encoded)
                .expect("serialized squelch must deserialize");

            assert_eq!(decoded, value);
        }
    }

    #[test]
    fn automatic_squelch_margin_round_trips_numerically() {
        let decoded = serde_json::from_value::<SquelchMode>(json!({
            "mode": "auto",
            "margin_db": 6.0,
        }))
        .expect("automatic squelch JSON must deserialize");

        match decoded {
            SquelchMode::Auto { margin_db } => {
                assert_abs_diff_eq!(margin_db, 6.0, epsilon = f32::EPSILON);
            }
            other => panic!("expected automatic squelch, got {other:?}"),
        }
    }

    #[test]
    fn every_profile_combination_round_trips_through_json() {
        let rf_profiles = [
            AmRfProfile::Khz6,
            AmRfProfile::Khz8,
            AmRfProfile::Khz10,
            AmRfProfile::Khz12,
        ];
        let voice_profiles = [
            VoiceProfile::Narrow,
            VoiceProfile::Standard,
            VoiceProfile::Wide,
        ];
        let squelches = [
            SquelchMode::Auto { margin_db: 6.0 },
            SquelchMode::Manual {
                threshold_dbfs: -83.25,
            },
            SquelchMode::Open,
        ];

        let mut index = 0_usize;
        for rf in rf_profiles {
            for voice in voice_profiles {
                let prebuffer_ms = if index.is_multiple_of(2) {
                    MIN_PREBUFFER_MS
                } else {
                    MAX_PREBUFFER_MS
                };
                let profile = AmAudioProfile {
                    rf,
                    voice,
                    squelch: squelches[index % squelches.len()],
                    prebuffer_ms,
                };

                let encoded = serde_json::to_value(&profile).expect("valid profile must serialize");
                let decoded = serde_json::from_value::<AmAudioProfile>(encoded)
                    .expect("serialized profile must deserialize");

                assert_eq!(decoded, profile, "{rf:?}/{voice:?}");
                index += 1;
            }
        }

        assert_eq!(index, 12, "all RF x voice combinations must be covered");
    }

    /// Authoritative expected edges for every RF x voice combination.
    const EDGE_CASES: [(AmRfProfile, VoiceProfile, f32, f32, f32); 12] = [
        (
            AmRfProfile::Khz6,
            VoiceProfile::Narrow,
            2_700.0,
            3_300.0,
            2_600.0,
        ),
        (
            AmRfProfile::Khz6,
            VoiceProfile::Standard,
            2_700.0,
            3_300.0,
            2_600.0,
        ),
        (
            AmRfProfile::Khz6,
            VoiceProfile::Wide,
            2_700.0,
            3_300.0,
            2_600.0,
        ),
        (
            AmRfProfile::Khz8,
            VoiceProfile::Narrow,
            3_000.0,
            4_400.0,
            2_800.0,
        ),
        (
            AmRfProfile::Khz8,
            VoiceProfile::Standard,
            3_600.0,
            4_400.0,
            3_400.0,
        ),
        (
            AmRfProfile::Khz8,
            VoiceProfile::Wide,
            3_600.0,
            4_400.0,
            3_500.0,
        ),
        (
            AmRfProfile::Khz10,
            VoiceProfile::Narrow,
            3_000.0,
            5_500.0,
            2_800.0,
        ),
        (
            AmRfProfile::Khz10,
            VoiceProfile::Standard,
            3_600.0,
            5_500.0,
            3_400.0,
        ),
        (
            AmRfProfile::Khz10,
            VoiceProfile::Wide,
            4_200.0,
            5_500.0,
            4_000.0,
        ),
        (
            AmRfProfile::Khz12,
            VoiceProfile::Narrow,
            3_000.0,
            6_600.0,
            2_800.0,
        ),
        (
            AmRfProfile::Khz12,
            VoiceProfile::Standard,
            3_600.0,
            6_600.0,
            3_400.0,
        ),
        (
            AmRfProfile::Khz12,
            VoiceProfile::Wide,
            4_200.0,
            6_600.0,
            4_000.0,
        ),
    ];

    fn assert_effective_edges(
        rf: AmRfProfile,
        voice: VoiceProfile,
        expected_pass_hz: f32,
        expected_stop_hz: f32,
        expected_voice_high_hz: f32,
    ) {
        let edges = rf.effective_edges_hz(voice);
        let context = format!("{rf:?}/{voice:?}");

        assert_abs_diff_eq!(
            edges.pass_edge_hz,
            expected_pass_hz,
            epsilon = EDGE_EPSILON_HZ
        );
        assert_abs_diff_eq!(
            edges.stop_edge_hz,
            expected_stop_hz,
            epsilon = EDGE_EPSILON_HZ
        );
        assert_abs_diff_eq!(
            edges.effective_voice_high_hz,
            expected_voice_high_hz,
            epsilon = EDGE_EPSILON_HZ
        );

        // Derived-bandwidth invariants.
        assert_abs_diff_eq!(
            edges.stop_edge_hz,
            0.55 * rf.bandwidth_hz(),
            epsilon = EDGE_EPSILON_HZ
        );
        assert!(
            edges.pass_edge_hz <= 0.45 * rf.bandwidth_hz() + EDGE_EPSILON_HZ,
            "{context}: pass edge must not exceed 45% of RF bandwidth"
        );
        assert!(
            edges.pass_edge_hz < edges.stop_edge_hz,
            "{context}: pass edge must stay below stop edge"
        );
        assert!(
            edges.pass_edge_hz <= edges.stop_edge_hz - 200.0 + EDGE_EPSILON_HZ,
            "{context}: a 200 Hz transition guard must be retained below the stop edge"
        );
        assert!(
            edges.effective_voice_high_hz <= voice.high_cutoff_hz(),
            "{context}: RF clamping must never raise the requested voice cutoff"
        );
        assert!(
            edges.effective_voice_high_hz <= edges.pass_edge_hz - 100.0 + EDGE_EPSILON_HZ,
            "{context}: usable voice edge must stay 100 Hz below the pass edge"
        );
    }

    #[test]
    fn every_rf_and_voice_combination_has_consistent_effective_edges() {
        assert_eq!(
            EDGE_CASES.len(),
            12,
            "all 12 RF x voice combinations must be covered"
        );

        for (rf, voice, pass_hz, stop_hz, voice_high_hz) in EDGE_CASES {
            assert_effective_edges(rf, voice, pass_hz, stop_hz, voice_high_hz);
        }
    }

    #[test]
    fn narrow_rf_profiles_report_rf_clamped_voice_edges() {
        // A 6 kHz RF profile clamps every voice selection to the same edges.
        for voice in [
            VoiceProfile::Narrow,
            VoiceProfile::Standard,
            VoiceProfile::Wide,
        ] {
            let edges = AmRfProfile::Khz6.effective_edges_hz(voice);

            assert_abs_diff_eq!(
                edges.effective_voice_high_hz,
                2_600.0,
                epsilon = EDGE_EPSILON_HZ
            );
            assert!(
                edges.effective_voice_high_hz < voice.high_cutoff_hz(),
                "{voice:?}: 6 kHz RF must clamp below the requested cutoff"
            );
        }

        // 8 kHz RF clamps only the Wide voice profile.
        let eight_khz_wide = AmRfProfile::Khz8.effective_edges_hz(VoiceProfile::Wide);
        assert_abs_diff_eq!(
            eight_khz_wide.effective_voice_high_hz,
            3_500.0,
            epsilon = EDGE_EPSILON_HZ
        );
        assert!(eight_khz_wide.effective_voice_high_hz < VoiceProfile::Wide.high_cutoff_hz());
    }
}
