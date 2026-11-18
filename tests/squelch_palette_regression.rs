//! Regression coverage for the TUI `/squelch <dB>` command path.

use crossbeam_channel::unbounded;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use termsdr::audio::AudioControl;
use termsdr::types::AppConfig;
use termsdr::ui::App;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn palette_command(app: &mut App, command: &str) {
    app.handle_key(key(KeyCode::Char('/')));
    for character in command.chars() {
        app.handle_key(key(KeyCode::Char(character)));
    }
    app.handle_key(key(KeyCode::Enter));
}

#[test]
fn squelch_palette_command_dispatches_to_app_and_audio_thread_without_panicking() {
    let (audio_tx, audio_rx) = unbounded();
    let mut app = App::new(AppConfig::default(), Some(audio_tx));

    palette_command(&mut app, "squelch -60");

    assert_eq!(app.squelch, Some(-60.0));
    assert!(!app.command_palette_active);
    match audio_rx.try_recv() {
        Ok(AudioControl::SetSquelch(Some(db))) => assert_eq!(db, -60.0),
        Ok(other) => panic!("expected SetSquelch(Some(-60)), got {other:?}"),
        Err(error) => panic!("expected an audio control message, got {error}"),
    }
    assert!(
        audio_rx.try_recv().is_err(),
        "squelch dispatch must emit exactly one message"
    );
}

#[test]
fn squelch_palette_rejects_nonfinite_and_out_of_range_values_atomically() {
    for value in ["NaN", "inf", "-inf", "-120.1", "0.1"] {
        let (audio_tx, audio_rx) = unbounded();
        let mut app = App::new(AppConfig::default(), Some(audio_tx));
        app.squelch = Some(-45.0);

        palette_command(&mut app, &format!("squelch {value}"));

        assert_eq!(
            app.squelch,
            Some(-45.0),
            "invalid value {value} mutated App"
        );
        assert!(
            app.command_palette_active,
            "invalid value {value} must leave the error visible"
        );
        assert!(
            app.command_palette.error.is_some(),
            "invalid value {value} must explain the rejection"
        );
        assert!(
            audio_rx.try_recv().is_err(),
            "invalid value {value} sent an audio update"
        );
    }
}
