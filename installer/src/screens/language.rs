//! Step 1 — language. Big, centered, two cards-ish rows. Sets UI language and a
//! default system locale.

use crate::app::App;
use crate::i18n::{t, Lang};
use crate::screens::widgets;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

const OPTIONS: [Lang; 2] = [Lang::Uk, Lang::En];

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
            t(app.lang, "lang.hint"),
            theme::dim(),
        ))),
        rows[0],
    );

    let items: Vec<String> = OPTIONS
        .iter()
        .map(|l| match l {
            Lang::En => format!("  {}", t(app.lang, "lang.en")),
            Lang::Uk => format!("  {}", t(app.lang, "lang.uk")),
        })
        .collect();
    widgets::select_list(f, rows[1], &items, app.cursor);

    widgets::action_row(f, rows[2], &t(app.lang, "app.back"), &t(app.lang, "app.next"), true);
    app.can_advance = true;
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.cursor = app.cursor.saturating_sub(1);
            apply(app); // live preview: switch UI language as you move
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.cursor = (app.cursor + 1).min(OPTIONS.len() - 1);
            apply(app);
        }
        KeyCode::Enter => {
            apply(app);
            // After choosing the language, present the mode chooser (Install /
            // Recovery) rather than dropping straight into the install flow.
            app.screen = crate::app::Screen::Mode;
        }
        _ => {}
    }
}

fn apply(app: &mut App) {
    let lang = OPTIONS[app.cursor.min(OPTIONS.len() - 1)];
    app.lang = lang;
    match lang {
        Lang::En => {
            app.config.lang = "en".into();
            app.config.locale = "en_US.UTF-8".into();
            // Drop the Ukrainian layout only when the layouts are still on the
            // Ukrainian-interface default ([gb, ua]) or an old [ua]-first list,
            // so a deliberate custom choice isn't clobbered.
            if app.config.xkb_layouts == vec!["gb".to_string(), "ua".to_string()]
                || app.config.xkb_layouts == vec!["ua".to_string(), "gb".to_string()]
                || app.config.xkb_layouts == vec!["ua".to_string()]
            {
                app.config.keymap = "gb".into();
                app.config.xkb_layouts = vec!["gb".into()];
            }
        }
        Lang::Uk => {
            app.config.lang = "uk".into();
            app.config.locale = "uk_UA.UTF-8".into();
            // Picking Ukrainian as the interface language selects BOTH layouts,
            // but ENGLISH FIRST (primary) and Ukrainian second. The primary
            // layout is what the console keymap (vconsole KEYMAP) is set to, and
            // it must be Latin: the initramfs `keymap` hook loads it BEFORE the
            // `encrypt` hook, so a Cyrillic-primary layout would silently break
            // typing the LUKS passphrase. A Latin primary also keeps keyboard
            // shortcuts (which are defined against Latin keysyms) working in
            // every DE out of the box; Ukrainian is one toggle away as the
            // second layout. Only applied while still on the untouched default,
            // so a deliberate choice isn't overwritten.
            if app.config.xkb_layouts == vec!["gb".to_string()] {
                app.config.keymap = "gb".into();
                app.config.xkb_layouts = vec!["gb".into(), "ua".into()];
            }
        }
    }
}
