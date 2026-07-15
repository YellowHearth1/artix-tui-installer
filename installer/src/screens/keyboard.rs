//! Step 4 — keyboard layouts. Multi-select from the console keymaps available
//! on the live system (`localectl list-keymaps`), excluding Russian layouts and
//! any whose code/description references Russia. First checked = primary.

use crate::app::App;
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
use std::sync::OnceLock;

fn is_russian(code: &str) -> bool {
    let c = code.to_lowercase();
    // console keymap codes like "ru", "ruwin_*", "russian"; xkb "ru"
    c == "ru"
        || c.starts_with("ru-")
        || c.starts_with("ru_")
        || c.starts_with("ruwin")
        || c.contains("russia")
}

/// Layouts whose base script is NOT Latin (Cyrillic / Greek / etc.) among the
/// codes we ever show. These must not become the console keymap — see the
/// LUKS-passphrase note in `handle_key`.
fn is_nonlatin(code: &str) -> bool {
    let c = code.to_lowercase();
    matches!(
        c.as_str(),
        "ua" | "by" | "bg" | "gr" | "mk" | "rs" | "ge" | "am" | "il"
    ) || c.starts_with("ua-")
        || c.starts_with("by-")
        || c.starts_with("bg-")
        || c.starts_with("gr-")
}

fn keymaps() -> &'static Vec<String> {
    static K: OnceLock<Vec<String>> = OnceLock::new();
    K.get_or_init(|| {
        // Layouts pinned to the top, in this exact order, since they're the most
        // likely choices for this distro's audience.
        let pinned = ["ua", "gb", "us"];

        // A broad, curated set of common layouts shown after the pinned ones.
        // All are standard console keymap codes present on Artix live images.
        let common = [
            "de", "fr", "es", "it", "pl", "cz", "pt", "nl", "se", "fi", "no", "dk", "be", "ch",
            "at", "hu", "ro", "sk", "si", "hr", "gr", "tr", "bg", "lt", "lv", "ee", "by", "ca",
            "br", "la", "is", "ie",
        ];

        // Try the live system first for the full universe of keymaps; we'll
        // intersect it with our curated set so we only show ones that exist.
        let raw =
            crate::system::runner::capture("localectl", &["list-keymaps"]).unwrap_or_default();
        let available: Vec<String> = raw
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !is_russian(s))
            .collect();
        let exists = |code: &str| -> bool {
            // If localectl gave us nothing (off-target), assume the curated code
            // is fine; otherwise only keep codes the live system actually has.
            available.is_empty() || available.iter().any(|a| a == code)
        };

        let mut v: Vec<String> = Vec::new();
        // 1) pinned, in order, skipping Russian and missing ones.
        for code in pinned {
            if !is_russian(code) && exists(code) {
                v.push(code.to_string());
            }
        }
        // 2) common curated layouts, in the order listed (NOT alphabetized so the
        //    pinned ones stay on top), de-duplicated against pinned.
        for code in common {
            if !is_russian(code) && exists(code) && !v.iter().any(|x| x == code) {
                v.push(code.to_string());
            }
        }
        // 3) anything else the live system offers, appended alphabetically, so
        //    nothing is lost for users who need an exotic layout via the filter.
        let mut rest: Vec<String> = available
            .into_iter()
            .filter(|a| !v.iter().any(|x| x == a))
            .collect();
        rest.sort();
        rest.dedup();
        v.extend(rest);
        v
    })
}

/// Human-friendly label for a keymap code, e.g. "ua" → "Ukraine (ua)". The code
/// is always shown in parentheses since that's what's written to the system
/// (vconsole keymap). Unknown codes fall back to just the code. Note: "ua" is
/// the keyboard *layout* code (country = Ukraine); the Ukrainian *language*
/// code is "uk", but the system keymap is "ua", so that's what we keep.
fn label_for(code: &str) -> String {
    let name = match code {
        "ua" => "Ukraine",
        "gb" => "United Kingdom",
        "us" => "United States",
        "de" => "Germany",
        "fr" => "France",
        "es" => "Spain",
        "it" => "Italy",
        "pl" => "Poland",
        "cz" => "Czechia",
        "pt" => "Portugal",
        "nl" => "Netherlands",
        "se" => "Sweden",
        "fi" => "Finland",
        "no" => "Norway",
        "dk" => "Denmark",
        "be" => "Belgium",
        "ch" => "Switzerland",
        "at" => "Austria",
        "hu" => "Hungary",
        "ro" => "Romania",
        "sk" => "Slovakia",
        "si" => "Slovenia",
        "hr" => "Croatia",
        "gr" => "Greece",
        "tr" => "Turkey",
        "bg" => "Bulgaria",
        "lt" => "Lithuania",
        "lv" => "Latvia",
        "ee" => "Estonia",
        "by" => "Belarus",
        "ca" => "Canada",
        "br" => "Brazil",
        "la" => "Latin America",
        "is" => "Iceland",
        "ie" => "Ireland",
        _ => return code.to_string(),
    };
    format!("{name} ({code})")
}

fn filtered(query: &str) -> Vec<String> {
    let q = query.to_lowercase();
    keymaps()
        .iter()
        // Match the code OR the human label, so "germany" finds "de" and "de"
        // finds it too.
        .filter(|k| {
            q.is_empty()
                || k.to_lowercase().contains(&q)
                || label_for(k).to_lowercase().contains(&q)
        })
        .cloned()
        .collect()
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // hint
            Constraint::Length(3), // filter box
            Constraint::Min(0),    // list
            Constraint::Length(3), // actions
        ])
        .spacing(1)
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            t(app.lang, "kb.hint"),
            theme::dim(),
        ))),
        rows[0],
    );

    // Filter box (type to narrow the list).
    let filter = Paragraph::new(Line::from(vec![
        Span::styled("  ", theme::dim()),
        Span::styled(
            if app.kb_query.is_empty() {
                t(app.lang, "kb.filter")
            } else {
                app.kb_query.clone()
            },
            if app.kb_query.is_empty() {
                theme::mute()
            } else {
                theme::normal()
            },
        ),
        Span::styled("▏", theme::accent()),
    ]))
    .block(theme::box_rounded());
    f.render_widget(filter, rows[1]);

    let list = filtered(&app.kb_query);
    let chosen = app.config.xkb_layouts.clone();
    // Display the friendly labels ("Ukraine (ua)"), while selection logic still
    // keys off the underlying code stored in `list`.
    let items: Vec<String> = list.iter().map(|c| label_for(c)).collect();
    let checked = |i: usize| -> bool { chosen.contains(&list[i]) };
    widgets::multi_list(f, rows[2], &items, &checked, app.cursor);

    app.can_advance = !app.config.xkb_layouts.is_empty();
    widgets::action_row(
        f,
        rows[3],
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        app.can_advance,
    );
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    let list = filtered(&app.kb_query);
    if super::nav::move_cursor(key.code, &mut app.cursor, list.len()) {
        return;
    }
    match key.code {
        KeyCode::Char(' ') => {
            if let Some(k) = list.get(app.cursor) {
                if let Some(pos) = app.config.xkb_layouts.iter().position(|x| x == k) {
                    app.config.xkb_layouts.remove(pos);
                } else {
                    app.config.xkb_layouts.push(k.clone());
                }
                // Console keymap (vconsole KEYMAP) = first LATIN layout chosen.
                // It must never be a non-Latin one: the initramfs `keymap` hook
                // loads it before the `encrypt` hook, so a Cyrillic/Greek
                // console keymap would silently corrupt LUKS passphrase entry
                // (and early-console logins). X/Wayland still get the full
                // ordered list in xkb_layouts. Fallback: "us".
                app.config.keymap = app
                    .config
                    .xkb_layouts
                    .iter()
                    .find(|x| !is_nonlatin(x))
                    .cloned()
                    .unwrap_or_else(|| "us".into());
            }
        }
        KeyCode::Enter if !app.config.xkb_layouts.is_empty() => app.goto_next(),
        KeyCode::Char(c) => {
            app.kb_query.push(c);
            app.cursor = 0;
        }
        KeyCode::Backspace => {
            app.kb_query.pop();
            app.cursor = 0;
        }
        _ => {}
    }
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "kb.footer")
}
