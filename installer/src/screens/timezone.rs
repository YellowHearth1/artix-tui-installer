//! Step 2 — time zone. Full IANA list from chrono-tz with a live type-to-filter
//! search box. Russian zones are excluded per spec.

use crate::app::App;
use crate::i18n::t;
use crate::screens::widgets;
use crate::theme;
use chrono_tz::TZ_VARIANTS;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, BorderType, Paragraph},
    Frame,
};
use std::sync::OnceLock;

/// Zones to drop entirely: the Russian Federation + Moscow, plus deprecated
/// IANA aliases for Ukrainian zones. IANA keeps the old "Kiev" (Russian
/// transliteration) and the collapsed "Uzhgorod"/"Zaporozhye" zones as aliases
/// of Europe/Kyiv — we hide all of them and keep only the correct Europe/Kyiv.
fn is_excluded(name: &str) -> bool {
    const BLOCKED: &[&str] = &[
        // Deprecated Ukrainian aliases — keep only Europe/Kyiv.
        "Europe/Kiev",
        "Europe/Uzhgorod",
        "Europe/Zaporozhye",
        // Russian Federation.
        "Europe/Moscow",
        "Europe/Kaliningrad",
        "Europe/Samara",
        "Europe/Volgograd",
        "Europe/Saratov",
        "Europe/Astrakhan",
        "Europe/Ulyanovsk",
        "Europe/Kirov",
        "Asia/Yekaterinburg",
        "Asia/Omsk",
        "Asia/Novosibirsk",
        "Asia/Novokuznetsk",
        "Asia/Krasnoyarsk",
        "Asia/Barnaul",
        "Asia/Tomsk",
        "Asia/Irkutsk",
        "Asia/Chita",
        "Asia/Yakutsk",
        "Asia/Vladivostok",
        "Asia/Khandyga",
        "Asia/Ust-Nera",
        "Asia/Magadan",
        "Asia/Sakhalin",
        "Asia/Srednekolymsk",
        "Asia/Kamchatka",
        "Asia/Anadyr",
        "W-SU",
    ];
    BLOCKED.contains(&name)
}

fn all_zones() -> &'static Vec<String> {
    static Z: OnceLock<Vec<String>> = OnceLock::new();
    Z.get_or_init(|| {
        TZ_VARIANTS
            .iter()
            .map(|tz| tz.name().to_string())
            .filter(|n| !is_excluded(n))
            .collect()
    })
}

fn filtered(query: &str) -> Vec<String> {
    let q = query.to_lowercase();
    all_zones()
        .iter()
        .filter(|z| q.is_empty() || z.to_lowercase().contains(&q))
        .cloned()
        .collect()
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // search box
            Constraint::Min(0),    // list
            Constraint::Length(3), // actions
        ])
        .spacing(1)
        .split(area);

    // Search box.
    let q = &app.tz_query;
    let search = Paragraph::new(Line::from(vec![
        Span::styled("  ", theme::dim()),
        Span::styled(if q.is_empty() { t(app.lang, "tz.hint") } else { q.clone() },
            if q.is_empty() { theme::mute() } else { theme::normal() }),
        Span::styled("▏", theme::accent()),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme::border())
            .title(format!(" {} ", t(app.lang, "app.search")))
            .title_style(theme::dim()),
    );
    f.render_widget(search, rows[0]);

    let list = filtered(q);
    let items: Vec<String> = list.iter().map(|z| format!("  {z}")).collect();
    if items.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("  —", theme::mute()))),
            rows[1],
        );
        app.can_advance = false;
    } else {
        widgets::select_list(f, rows[1], &items, app.cursor);
        app.can_advance = true;
        app.config.timezone = list[app.cursor.min(list.len() - 1)].clone();
    }

    widgets::action_row(f, rows[2], &t(app.lang, "app.back"), &t(app.lang, "app.next"), app.can_advance);
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    let len = filtered(&app.tz_query).len();
    match key.code {
        KeyCode::Up => app.cursor = app.cursor.saturating_sub(1),
        KeyCode::Down => app.cursor = (app.cursor + 1).min(len.saturating_sub(1)),
        KeyCode::Char(c) => {
            app.tz_query.push(c);
            app.cursor = 0;
        }
        KeyCode::Backspace => {
            app.tz_query.pop();
            app.cursor = 0;
        }
        KeyCode::Enter if len > 0 => app.goto_next(),
        _ => {}
    }
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "tz.footer")
}
