//! Frozen public contract for AM audio profile configuration.
//!
//! The profile API is intentionally pure and unit-explicit: every frequency
//! accessor/field ends in `_hz`, and every buffering accessor ends in `_ms`.

use termsdr::audio::profile::{
    clamp_prebuffer_ms, validate_prebuffer_ms, AmAudioProfile, AmRfProfile, SquelchMode,
    VoiceProfile, DEFAULT_PREBUFFER_MS, MAX_PREBUFFER_MS, MIN_PREBUFFER_MS,
};

#[test]
fn rf_profiles_map_to_nominal_bandwidth_hz() {
    let cases = [
        (AmRfProfile::Khz6, 6_000.0),
        (AmRfProfile::Khz8, 8_000.0),
        (AmRfProfile::Khz10, 10_000.0),
        (AmRfProfile::Khz12, 12_000.0),
    ];

    for (profile, expected_hz) in cases {
        assert_eq!(profile.bandwidth_hz(), expected_hz);
    }
}

#[test]
fn rf_profile_default_is_8_khz() {
    assert_eq!(AmRfProfile::default(), AmRfProfile::Khz8);
}

#[test]
fn rf_profile_names_are_stable_for_users() {
    let cases = [
        (AmRfProfile::Khz6, "6 kHz"),
        (AmRfProfile::Khz8, "8 kHz"),
        (AmRfProfile::Khz10, "10 kHz"),
        (AmRfProfile::Khz12, "12 kHz"),
    ];

    for (profile, expected_name) in cases {
        assert_eq!(profile.display_name(), expected_name);
    }
}

#[test]
fn voice_profiles_map_to_low_and_high_cutoffs_hz() {
    let cases = [
        (VoiceProfile::Narrow, 300.0, 2_800.0),
        (VoiceProfile::Standard, 250.0, 3_400.0),
        (VoiceProfile::Wide, 150.0, 4_000.0),
    ];

    for (profile, expected_low_hz, expected_high_hz) in cases {
        assert_eq!(profile.low_cutoff_hz(), expected_low_hz);
        assert_eq!(profile.high_cutoff_hz(), expected_high_hz);
    }
}

#[test]
fn voice_profile_default_is_standard() {
    assert_eq!(VoiceProfile::default(), VoiceProfile::Standard);
}

#[test]
fn voice_profile_names_are_stable_for_users() {
    let cases = [
        (VoiceProfile::Narrow, "Narrow"),
        (VoiceProfile::Standard, "Standard"),
        (VoiceProfile::Wide, "Wide"),
    ];

    for (profile, expected_name) in cases {
        assert_eq!(profile.display_name(), expected_name);
    }
}

#[test]
fn squelch_default_is_automatic_with_six_db_margin() {
    assert_eq!(SquelchMode::default(), SquelchMode::Auto { margin_db: 6.0 });
}

#[test]
fn squelch_modes_preserve_manual_threshold_and_open_state() {
    let manual = SquelchMode::Manual {
        threshold_dbfs: -72.5,
    };

    assert_eq!(
        manual,
        SquelchMode::Manual {
            threshold_dbfs: -72.5
        }
    );
    assert_eq!(SquelchMode::Open, SquelchMode::Open);
}

#[test]
fn am_audio_profile_default_composes_authoritative_defaults() {
    assert_eq!(
        AmAudioProfile::default(),
        AmAudioProfile {
            rf: AmRfProfile::Khz8,
            voice: VoiceProfile::Standard,
            squelch: SquelchMode::Auto { margin_db: 6.0 },
            prebuffer_ms: 100,
        }
    );
    assert_eq!(DEFAULT_PREBUFFER_MS, 100);
}

#[test]
fn prebuffer_validation_accepts_both_boundaries_and_interior() {
    assert_eq!(MIN_PREBUFFER_MS, 50);
    assert_eq!(MAX_PREBUFFER_MS, 150);

    for valid_ms in [50, 51, 100, 149, 150] {
        assert_eq!(validate_prebuffer_ms(valid_ms).unwrap(), valid_ms);
    }
}

#[test]
fn prebuffer_validation_rejects_values_outside_range() {
    for invalid_ms in [0, 49, 151, u16::MAX] {
        assert!(
            validate_prebuffer_ms(invalid_ms).is_err(),
            "{invalid_ms} ms must be rejected"
        );
    }
}

#[test]
fn prebuffer_clamping_saturates_at_boundaries() {
    let cases = [
        (0, 50),
        (49, 50),
        (50, 50),
        (51, 51),
        (100, 100),
        (149, 149),
        (150, 150),
        (151, 150),
        (u16::MAX, 150),
    ];

    for (configured_ms, expected_ms) in cases {
        assert_eq!(clamp_prebuffer_ms(configured_ms), expected_ms);
    }
}

#[test]
fn enum_json_names_are_stable_protocol_tokens() {
    // Source requirement: Task 15's additive protocol commands use number-first
    // RF profile tokens (`6khz`, `8khz`, `10khz`, and `12khz`).
    let rf_cases = [
        (AmRfProfile::Khz6, "\"6khz\""),
        (AmRfProfile::Khz8, "\"8khz\""),
        (AmRfProfile::Khz10, "\"10khz\""),
        (AmRfProfile::Khz12, "\"12khz\""),
    ];
    for (value, expected_json) in rf_cases {
        assert_eq!(serde_json::to_string(&value).unwrap(), expected_json);
        assert_eq!(
            serde_json::from_str::<AmRfProfile>(expected_json).unwrap(),
            value
        );
    }

    let voice_cases = [
        (VoiceProfile::Narrow, "\"narrow\""),
        (VoiceProfile::Standard, "\"standard\""),
        (VoiceProfile::Wide, "\"wide\""),
    ];
    for (value, expected_json) in voice_cases {
        assert_eq!(serde_json::to_string(&value).unwrap(), expected_json);
        assert_eq!(
            serde_json::from_str::<VoiceProfile>(expected_json).unwrap(),
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
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(serde_json::from_str::<SquelchMode>(&json).unwrap(), value);
    }
}

#[test]
fn squelch_json_shape_is_stable_for_protocol_clients() {
    assert_eq!(
        serde_json::to_value(SquelchMode::Auto { margin_db: 6.0 }).unwrap(),
        serde_json::json!({"mode": "auto", "margin_db": 6.0})
    );
    assert_eq!(
        serde_json::to_value(SquelchMode::Manual {
            threshold_dbfs: -70.0,
        })
        .unwrap(),
        serde_json::json!({"mode": "manual", "threshold_dbfs": -70.0})
    );
    assert_eq!(
        serde_json::to_value(SquelchMode::Open).unwrap(),
        serde_json::json!({"mode": "open"})
    );
}

#[test]
fn complete_profile_json_shape_is_stable_and_round_trips() {
    let profile = AmAudioProfile {
        rf: AmRfProfile::Khz10,
        voice: VoiceProfile::Wide,
        squelch: SquelchMode::Manual {
            threshold_dbfs: -68.5,
        },
        prebuffer_ms: 125,
    };
    let expected = serde_json::json!({
        "rf": "10khz",
        "voice": "wide",
        "squelch": {"mode": "manual", "threshold_dbfs": -68.5},
        "prebuffer_ms": 125,
    });

    let json = serde_json::to_value(&profile).unwrap();
    assert_eq!(json, expected);
    assert_eq!(
        serde_json::from_value::<AmAudioProfile>(json).unwrap(),
        profile
    );
}

#[test]
fn every_rf_and_voice_pair_has_authoritative_effective_edges_hz() {
    // Expected values apply the plan rules:
    // pass = min(selected voice high + 200, 0.45 * RF bandwidth),
    // stop = 0.55 * RF bandwidth, and the usable voice passband is bounded
    // by pass - 100 (the edge through which <= 1 dB ripple is required).
    let cases = [
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

    for (rf, voice, expected_pass_hz, expected_stop_hz, expected_voice_high_hz) in cases {
        let edges = rf.effective_edges_hz(voice);
        assert_eq!(edges.pass_edge_hz, expected_pass_hz, "{rf:?}/{voice:?}");
        assert_eq!(edges.stop_edge_hz, expected_stop_hz, "{rf:?}/{voice:?}");
        assert_eq!(
            edges.effective_voice_high_hz, expected_voice_high_hz,
            "{rf:?}/{voice:?}"
        );
        assert!(edges.pass_edge_hz < edges.stop_edge_hz, "{rf:?}/{voice:?}");
        assert!(
            edges.effective_voice_high_hz <= voice.high_cutoff_hz(),
            "{rf:?}/{voice:?}"
        );
    }
}

#[test]
fn effective_voice_high_reports_rf_clamping() {
    let six_khz = AmRfProfile::Khz6.effective_edges_hz(VoiceProfile::Standard);
    assert_eq!(six_khz.effective_voice_high_hz, 2_600.0);
    assert!(six_khz.effective_voice_high_hz < VoiceProfile::Standard.high_cutoff_hz());

    let eight_khz_wide = AmRfProfile::Khz8.effective_edges_hz(VoiceProfile::Wide);
    assert_eq!(eight_khz_wide.effective_voice_high_hz, 3_500.0);
    assert!(eight_khz_wide.effective_voice_high_hz < VoiceProfile::Wide.high_cutoff_hz());
}
