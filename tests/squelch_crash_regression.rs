//! Regression coverage for the reported "moving squelch crashes the program" bug.
//!
//! Investigation result: the squelch slider itself never panicked on any
//! terminal size. The reproducible abort was an unsigned-underflow panic in the
//! shared draw path — `size.height / 2 - 1` in the frequency and bandwidth input
//! overlays — which fires on a pane 0–1 rows tall. Moving the squelch slider
//! triggers a redraw on every key press, so it surfaced as "squelch crashes the
//! program" whenever the terminal or the pane was degenerate (tiny split, a
//! pty with no window size, a resize mid-drag).
//!
//! These tests pin both halves: the draw path must survive every degenerate
//! size, and the squelch slider must only ever emit finite, in-range thresholds.

use crossbeam_channel::unbounded;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use termsdr::audio::AudioControl;
use termsdr::types::AppConfig;
use termsdr::ui::{draw_ui, App};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn render(app: &App, width: u16, height: u16) {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal.draw(|frame| draw_ui(frame, app)).expect("render");
}

fn drain_squelch_values(rx: &crossbeam_channel::Receiver<AudioControl>) -> Vec<Option<f32>> {
    let mut values = Vec::new();
    while let Ok(control) = rx.try_recv() {
        if let AudioControl::SetSquelch(value) = control {
            values.push(value);
        }
    }
    values
}

#[test]
fn draw_survives_degenerate_terminal_heights() {
    for (width, height) in [(80_u16, 0_u16), (80, 1), (80, 2), (1, 1), (0, 0), (1, 0)] {
        let mut app = App::new(AppConfig::default(), None);
        app.squelch = Some(-60.0);
        render(&app, width, height);

        let mut app = App::new(AppConfig::default(), None);
        app.squelch = Some(-60.0);
        app.freq_input_mode = true;
        render(&app, width, height);

        let mut app = App::new(AppConfig::default(), None);
        app.bw_input_mode = true;
        render(&app, width, height);

        let mut app = App::new(AppConfig::default(), None);
        app.settings_modal.visible = true;
        render(&app, width, height);
    }
}

#[test]
fn moving_the_squelch_slider_never_emits_a_non_finite_or_out_of_range_threshold() {
    let (audio_tx, audio_rx) = unbounded();
    let mut app = App::new(AppConfig::default(), Some(audio_tx));

    app.handle_key(key(KeyCode::Char('s')));
    app.handle_key(key(KeyCode::Down));
    for direction in [KeyCode::Left, KeyCode::Right] {
        for _ in 0..150 {
            app.handle_key(key(direction));
        }
    }
    for _ in 0..40 {
        app.handle_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT));
        app.handle_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
    }

    let emitted = drain_squelch_values(&audio_rx);
    assert!(!emitted.is_empty(), "moving squelch must emit controls");
    assert!(
        emitted.iter().flatten().all(|value| value.is_finite()),
        "squelch slider emitted a non-finite threshold: {emitted:?}"
    );
    assert!(
        emitted
            .iter()
            .flatten()
            .all(|value| (-120.0..=0.0).contains(value)),
        "squelch slider left the documented range: {emitted:?}"
    );
}

#[test]
fn squelch_floor_disables_the_gate_and_the_ceiling_stays_finite() {
    let (audio_tx, audio_rx) = unbounded();
    let mut app = App::new(AppConfig::default(), Some(audio_tx));

    app.handle_key(key(KeyCode::Char('s')));
    app.handle_key(key(KeyCode::Down));
    for _ in 0..200 {
        app.handle_key(key(KeyCode::Left));
    }
    let silenced = drain_squelch_values(&audio_rx);
    assert!(
        silenced.last().is_some_and(Option::is_none),
        "dragging squelch to its floor must disable the gate, got {silenced:?}"
    );

    for _ in 0..200 {
        app.handle_key(key(KeyCode::Right));
    }
    let opened = drain_squelch_values(&audio_rx);
    assert!(
        matches!(opened.last(), Some(Some(value)) if value.is_finite() && *value <= 0.0),
        "dragging squelch to its ceiling must leave a finite threshold, got {opened:?}"
    );
}
