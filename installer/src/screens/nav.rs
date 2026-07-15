//! List-cursor navigation — the one interaction every list screen shares.
//!
//! Four screens used to carry their own copy of the same six match arms
//! (Up/Down/PageUp/PageDown/Home/End), differing only in which cursor field
//! they moved. Copies of one behaviour are how screens drift apart: change the
//! page size in one and the others silently keep the old feel. This module is
//! the single definition of "how a list moves" for the whole installer.
//!
//! Deliberately NOT here: the install-log scroll on the summary screen. It
//! looks similar but behaves differently (it scrolls a viewport, tracks a
//! follow-the-tail mode, and saturates against a line count that grows while
//! you watch). Forcing it through this helper would mean flags and special
//! cases — an abstraction that has to ask which caller it serves is worse than
//! two honest pieces of code.

use crossterm::event::KeyCode;

/// How far PageUp/PageDown jump: roughly a screenful of a dense list.
const PAGE: usize = 10;

/// Apply one of the standard list-navigation keys to `cursor` over a list of
/// `len` items. Returns `true` if the key was handled (the caller can stop),
/// `false` if it wasn't a navigation key and is the caller's to interpret.
///
/// Safe on an empty list: every key clamps the cursor to 0.
pub fn move_cursor(code: KeyCode, cursor: &mut usize, len: usize) -> bool {
    let last = len.saturating_sub(1);
    match code {
        KeyCode::Up => *cursor = cursor.saturating_sub(1),
        KeyCode::Down => *cursor = (*cursor + 1).min(last),
        KeyCode::PageUp => *cursor = cursor.saturating_sub(PAGE),
        KeyCode::PageDown => *cursor = (*cursor + PAGE).min(last),
        KeyCode::Home => *cursor = 0,
        KeyCode::End => *cursor = last,
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_and_clamps_at_both_ends() {
        let mut c = 0;
        assert!(move_cursor(KeyCode::Up, &mut c, 5));
        assert_eq!(c, 0, "Up at the top stays at the top");

        assert!(move_cursor(KeyCode::Down, &mut c, 5));
        assert_eq!(c, 1);

        assert!(move_cursor(KeyCode::End, &mut c, 5));
        assert_eq!(c, 4);
        assert!(move_cursor(KeyCode::Down, &mut c, 5));
        assert_eq!(c, 4, "Down at the bottom stays at the bottom");

        assert!(move_cursor(KeyCode::Home, &mut c, 5));
        assert_eq!(c, 0);
    }

    #[test]
    fn page_jumps_do_not_overshoot() {
        let mut c = 0;
        assert!(move_cursor(KeyCode::PageDown, &mut c, 25));
        assert_eq!(c, 10);
        assert!(move_cursor(KeyCode::PageDown, &mut c, 25));
        assert!(move_cursor(KeyCode::PageDown, &mut c, 25));
        assert_eq!(c, 24, "a page jump clamps to the last item");
        assert!(move_cursor(KeyCode::PageUp, &mut c, 25));
        assert_eq!(c, 14);
    }

    /// The classic TUI panic is indexing an empty list. Screens draw before
    /// their background data arrives (Wi-Fi scan, package search), so the
    /// component must be safe when there is nothing to move over.
    #[test]
    fn an_empty_list_pins_the_cursor_to_zero() {
        let mut c = 0;
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Home,
            KeyCode::End,
        ] {
            assert!(move_cursor(code, &mut c, 0));
            assert_eq!(c, 0, "{code:?} on an empty list must leave the cursor at 0");
        }
    }

    #[test]
    fn non_navigation_keys_are_left_to_the_caller() {
        let mut c = 3;
        assert!(!move_cursor(KeyCode::Enter, &mut c, 5));
        assert!(!move_cursor(KeyCode::Char('x'), &mut c, 5));
        assert!(!move_cursor(KeyCode::Esc, &mut c, 5));
        assert_eq!(c, 3, "an unhandled key must not move the cursor");
    }
}
