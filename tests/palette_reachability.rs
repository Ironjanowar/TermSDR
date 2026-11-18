//! Reviewer-blocker regression: keyboard navigation in the command palette
//! must reach every registered command.
//!
//! `select_down` is the only Down-arrow path, so this drives `App::handle_key`
//! with real key events rather than calling the palette method directly.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use termsdr::types::AppConfig;
use termsdr::ui::App;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn palette_down_reaches_all_31_commands_including_zoom_in_and_zoom_out() {
    let mut app = App::new(AppConfig::default(), None);
    app.handle_key(key(KeyCode::Char('/')));

    let expected: Vec<String> = app
        .command_palette
        .registry
        .all()
        .iter()
        .map(|command| command.name.to_string())
        .collect();
    assert_eq!(
        expected.len(),
        31,
        "the palette registry must expose 31 commands"
    );
    assert_eq!(
        app.command_palette.selected_command_name(),
        Some(expected[0].as_str()),
        "opening the palette must select the first command"
    );

    // Walk past the end: the selection must stop at the last command, not wrap.
    let mut visited = vec![expected[0].clone()];
    for _ in 0..(expected.len() + 4) {
        app.handle_key(key(KeyCode::Down));
        let selected = app
            .command_palette
            .selected_command_name()
            .expect("selection must stay inside the filtered list")
            .to_string();
        if visited.last() != Some(&selected) {
            visited.push(selected);
        }
    }

    assert_eq!(
        visited, expected,
        "Down-arrow navigation skipped commands; zoom-in/zoom-out are the last two \
         alphabetically and were previously unreachable"
    );
    assert!(
        visited.iter().any(|name| name == "zoom-in"),
        "zoom-in must be selectable from the palette"
    );
    assert!(
        visited.iter().any(|name| name == "zoom-out"),
        "zoom-out must be selectable from the palette"
    );
}
