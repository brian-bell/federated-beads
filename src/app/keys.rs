//! Keypress → [`Msg`] mapping. The **only** file that imports `crossterm`, so
//! [`super::App::reduce`] stays backend-agnostic. Unmapped keys and key-release
//! events yield `None` (the runtime ignores them).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use super::Msg;

/// Decode a crossterm key event into a [`Msg`], or `None` for an unmapped key or
/// a key-release event (so a press+release fires a single message).
///
/// `editing` selects the mapping: while the search query input is focused, keys
/// edit text (any `Char` appends, `Backspace` deletes, `Enter` submits, `Esc`
/// cancels); otherwise the command/navigation mapping applies (so `/` opens
/// search, `j`/`k` move, `Enter` opens the detail, etc.). The runtime supplies
/// `editing` from the app's current search phase.
pub fn map_key(event: KeyEvent, editing: bool) -> Option<Msg> {
    // Ignore key releases; map presses and repeats (holding `j` scrolls). On
    // terminals without the kitty keyboard protocol crossterm reports `Press`.
    if event.kind == KeyEventKind::Release {
        return None;
    }
    if editing {
        return match event.code {
            KeyCode::Char(c) => Some(Msg::SearchInput(c)),
            KeyCode::Backspace => Some(Msg::SearchBackspace),
            KeyCode::Enter => Some(Msg::SubmitSearch),
            KeyCode::Esc => Some(Msg::Back),
            _ => None,
        };
    }
    match event.code {
        KeyCode::Char('q') => Some(Msg::Quit),
        KeyCode::Char('/') => Some(Msg::OpenSearch),
        KeyCode::Char('r') => Some(Msg::Refresh),
        KeyCode::Char('y') => Some(Msg::CopyContext),
        KeyCode::Char('Y') => Some(Msg::CopyMarkdown),
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
        assert_eq!(map_key(press(KeyCode::Char('q')), false), Some(Msg::Quit));
        assert_eq!(
            map_key(press(KeyCode::Char('/')), false),
            Some(Msg::OpenSearch)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('r')), false),
            Some(Msg::Refresh)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('y')), false),
            Some(Msg::CopyContext)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('Y')), false),
            Some(Msg::CopyMarkdown),
            "shifted Y copies the markdown block"
        );
        assert_eq!(map_key(press(KeyCode::Enter), false), Some(Msg::OpenDetail));
        assert_eq!(map_key(press(KeyCode::Esc), false), Some(Msg::Back));
    }

    #[test]
    fn maps_navigation_keys() {
        assert_eq!(
            map_key(press(KeyCode::Char('j')), false),
            Some(Msg::SelectNext)
        );
        assert_eq!(map_key(press(KeyCode::Down), false), Some(Msg::SelectNext));
        assert_eq!(
            map_key(press(KeyCode::Char('k')), false),
            Some(Msg::SelectPrev)
        );
        assert_eq!(map_key(press(KeyCode::Up), false), Some(Msg::SelectPrev));
    }

    #[test]
    fn maps_filter_keys() {
        assert_eq!(
            map_key(press(KeyCode::Char('f')), false),
            Some(Msg::CycleRepoFilter)
        );
        assert_eq!(
            map_key(press(KeyCode::Char('p')), false),
            Some(Msg::TogglePriorityFilter)
        );
    }

    #[test]
    fn maps_search_input_keys() {
        // While editing the query, every char is text — including keys that are
        // commands otherwise (`q`, `/`, `j`) — and the special keys drive submit/
        // edit/cancel.
        assert_eq!(
            map_key(press(KeyCode::Char('f')), true),
            Some(Msg::SearchInput('f'))
        );
        assert_eq!(
            map_key(press(KeyCode::Char('q')), true),
            Some(Msg::SearchInput('q')),
            "a command key is literal text while editing"
        );
        assert_eq!(
            map_key(press(KeyCode::Char('Y')), true),
            Some(Msg::SearchInput('Y')),
            "a shifted copy key is literal text while editing"
        );
        assert_eq!(
            map_key(press(KeyCode::Backspace), true),
            Some(Msg::SearchBackspace)
        );
        assert_eq!(
            map_key(press(KeyCode::Enter), true),
            Some(Msg::SubmitSearch)
        );
        assert_eq!(map_key(press(KeyCode::Esc), true), Some(Msg::Back));
        // Not editing: the same `j` is a navigation command.
        assert_eq!(
            map_key(press(KeyCode::Char('j')), false),
            Some(Msg::SelectNext)
        );
    }

    #[test]
    fn ignores_unmapped_and_release() {
        // An unmapped character.
        assert_eq!(map_key(press(KeyCode::Char('z')), false), None);
        // A release event for an otherwise-mapped key: ignored, so a press+release
        // does not fire the message twice.
        let release = KeyEvent::new_with_kind_and_state(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
            KeyEventState::NONE,
        );
        assert_eq!(map_key(release, false), None);
        assert_eq!(
            map_key(release, true),
            None,
            "release ignored while editing too"
        );
    }
}
