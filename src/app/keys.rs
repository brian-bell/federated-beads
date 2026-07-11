//! Keypress → [`Msg`] mapping. The **only** file that imports `crossterm`, so
//! [`super::App::reduce`] stays backend-agnostic. Unmapped keys and key-release
//! events yield `None` (the runtime ignores them).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use super::Msg;

/// Decode a crossterm key event into a [`Msg`], or `None` for an unmapped key or
/// a key-release event (so a press+release fires a single message).
pub fn map_key(event: KeyEvent) -> Option<Msg> {
    // Ignore key releases; map presses and repeats (holding `j` scrolls). On
    // terminals without the kitty keyboard protocol crossterm reports `Press`.
    if event.kind == KeyEventKind::Release {
        return None;
    }
    match event.code {
        KeyCode::Char('q') => Some(Msg::Quit),
        KeyCode::Char('/') => Some(Msg::OpenSearch),
        KeyCode::Char('r') => Some(Msg::Refresh),
        KeyCode::Char('y') => Some(Msg::CopyContext),
        KeyCode::Char('f') => Some(Msg::CycleRepoFilter),
        KeyCode::Char('p') => Some(Msg::TogglePriorityFilter),
        KeyCode::Char('j') | KeyCode::Down => Some(Msg::SelectNext),
        KeyCode::Char('k') | KeyCode::Up => Some(Msg::SelectPrev),
        KeyCode::Enter => Some(Msg::OpenDetail),
        KeyCode::Esc => Some(Msg::Back),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    /// A press event with no modifiers (crossterm's `new` sets kind = Press).
    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn maps_command_keys() {
        assert_eq!(map_key(press(KeyCode::Char('q'))), Some(Msg::Quit));
        assert_eq!(map_key(press(KeyCode::Char('/'))), Some(Msg::OpenSearch));
        assert_eq!(map_key(press(KeyCode::Char('r'))), Some(Msg::Refresh));
        assert_eq!(map_key(press(KeyCode::Char('y'))), Some(Msg::CopyContext));
        assert_eq!(map_key(press(KeyCode::Enter)), Some(Msg::OpenDetail));
        assert_eq!(map_key(press(KeyCode::Esc)), Some(Msg::Back));
    }

    #[test]
    fn maps_navigation_keys() {
        assert_eq!(map_key(press(KeyCode::Char('j'))), Some(Msg::SelectNext));
        assert_eq!(map_key(press(KeyCode::Down)), Some(Msg::SelectNext));
        assert_eq!(map_key(press(KeyCode::Char('k'))), Some(Msg::SelectPrev));
        assert_eq!(map_key(press(KeyCode::Up)), Some(Msg::SelectPrev));
    }

    #[test]
    fn maps_filter_keys() {
        assert_eq!(
            map_key(press(KeyCode::Char('f'))),
            Some(Msg::CycleRepoFilter)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('p'))),
            Some(Msg::TogglePriorityFilter)
        );
    }

    #[test]
    fn ignores_unmapped_and_release() {
        // An unmapped character.
        assert_eq!(map_key(press(KeyCode::Char('z'))), None);
        // A release event for an otherwise-mapped key: ignored, so a press+release
        // does not fire the message twice.
        let release = KeyEvent::new_with_kind_and_state(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
            KeyEventState::NONE,
        );
        assert_eq!(map_key(release), None);
    }
}
