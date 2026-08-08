//! Step 5 — Linux kernel. Single-select cards: Linux / Zen / Hardened / LTS.
//! The chosen kernel + its headers are installed during basestrap, so the
//! system boots with that kernel and can build DKMS modules (e.g. NVIDIA).

use crate::app::{App, Kernel};
use crate::i18n::t;
use crate::screens::widgets;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

const OPTIONS: [Kernel; 4] = [Kernel::Linux, Kernel::Zen, Kernel::Hardened, Kernel::Lts];

fn label(app: &App, k: Kernel) -> String {
    match k {
        Kernel::Linux => t(app.lang, "kern.linux"),
        Kernel::Zen => t(app.lang, "kern.zen"),
        Kernel::Hardened => t(app.lang, "kern.hardened"),
        Kernel::Lts => t(app.lang, "kern.lts"),
    }
}

fn desc(app: &App, k: Kernel) -> String {
    match k {
        Kernel::Linux => t(app.lang, "kern.linux_d"),
        Kernel::Zen => t(app.lang, "kern.zen_d"),
        Kernel::Hardened => t(app.lang, "kern.hardened_d"),
        Kernel::Lts => t(app.lang, "kern.lts_d"),
    }
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    app.can_advance = true;
    // Three height tiers. Four bordered cards (3 rows each) + hint + the action
    // box need ~16 rows; the real content panel on an 80x24 console is only ~15,
    // so a bordered layout overflows and clips the [Enter] button. Fall back by
    // height: roomy → 4-row cards with a package line; medium → 3-row cards;
    // tight → one borderless line per kernel, which always fits.
    let full = area.height >= 26;
    let bordered = area.height >= 18;

    if !bordered {
        draw_tight(f, app, area);
        return;
    }

    let card_h = if full { 4 } else { 3 };
    let hint_h = if full { 2 } else { 1 };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(hint_h),
            Constraint::Length(card_h),
            Constraint::Length(card_h),
            Constraint::Length(card_h),
            Constraint::Length(card_h),
            Constraint::Min(0),
            Constraint::Length(3), // actions
        ])
        .spacing(if full { 1 } else { 0 })
        .split(area);

    widgets::hint_line(f, rows[0], &t(app.lang, "kern.hint"));

    for (i, k) in OPTIONS.iter().enumerate() {
        let active = i == app.kernel_cursor;
        let (bs, ts) = if active {
            (theme::border(), theme::selected())
        } else {
            (theme::border_dim(), theme::normal())
        };
        let mut lines = vec![Line::from(vec![
            Span::styled(if active { "> " } else { "  " }, theme::accent()),
            Span::styled(label(app, *k), ts),
            Span::styled(format!("   {}", desc(app, *k)), theme::dim()),
        ])];
        // The package-names line only when there's vertical room for it.
        if full {
            let pkgs = k.packages().join(" ");
            lines.push(Line::from(Span::styled(
                format!("    {pkgs}"),
                theme::mute(),
            )));
        }
        let card = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(bs),
        );
        f.render_widget(card, rows[i + 1]);
    }

    widgets::action_row(
        f,
        rows[6],
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        true,
    );
}

/// Very short panels (e.g. an 80x24 console → ~15 content rows): one borderless
/// line per kernel. The active one is marked with the accent bar and highlight,
/// exactly like the other list screens — no boxes, so nothing clips.
fn draw_tight(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // hint
            Constraint::Length(1), // spacer
            Constraint::Min(0),    // list
            Constraint::Length(3), // actions
        ])
        .split(area);

    widgets::hint_line(f, rows[0], &t(app.lang, "kern.hint"));

    let mut lines: Vec<Line> = Vec::new();
    for (i, k) in OPTIONS.iter().enumerate() {
        let active = i == app.kernel_cursor;
        let ts = if active {
            theme::selected()
        } else {
            theme::normal()
        };
        lines.push(Line::from(vec![
            Span::styled(if active { "> " } else { "  " }, theme::accent()),
            Span::styled(label(app, *k), ts),
            Span::styled(format!("   {}", desc(app, *k)), theme::dim()),
        ]));
    }
    f.render_widget(Paragraph::new(lines), rows[2]);

    widgets::action_row(
        f,
        rows[3],
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        true,
    );
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.kernel_cursor = app.kernel_cursor.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            app.kernel_cursor = (app.kernel_cursor + 1).min(OPTIONS.len() - 1)
        }
        KeyCode::Enter => {
            let k = OPTIONS[app.kernel_cursor.min(OPTIONS.len() - 1)];
            app.config.kernel = k;
            app.goto_next();
        }
        _ => {}
    }
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "kern.footer")
}
