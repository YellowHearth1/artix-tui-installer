//! Mode chooser, shown right after the language screen and OUTSIDE the linear
//! 13-step install flow. Two choices: install the system (the normal flow) or
//! enter the recovery tool (mount an existing install + drop into a chroot).

use crate::app::{App, Screen};
use crate::i18n::t;
use crate::screens::widgets;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // hint
            Constraint::Min(0),    // list
            Constraint::Length(3), // actions
        ])
        .spacing(1)
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            t(app.lang, "mode.hint"),
            theme::dim(),
        ))),
        rows[0],
    );

    let items = vec![
        format!("  {}", t(app.lang, "mode.install")),
        format!("  {}", t(app.lang, "mode.recovery")),
    ];
    widgets::select_list(f, rows[1], &items, app.mode_cursor);

    widgets::action_row(
        f,
        rows[2],
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        true,
    );
    app.can_advance = true;
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.mode_cursor = app.mode_cursor.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => app.mode_cursor = (app.mode_cursor + 1).min(1),
        KeyCode::Enter => {
            if app.mode_cursor == 0 {
                // Install: enter the normal flow at its first post-language
                // step (Timezone). goto_next() from Language lands there.
                app.screen = Screen::Timezone;
            } else {
                // Recovery: jump to the recovery tool and start a fresh scan.
                app.recovery_focus = 0;
                app.recovery_unlock = 0;
                app.recovery_passphrase.clear();
                app.recovery_status.clear();
                app.recovery_mounted = false;
                app.screen = Screen::Recovery;
            }
        }
        KeyCode::Esc => {
            // Back to the language screen.
            app.screen = Screen::Language;
        }
        _ => {}
    }
}
