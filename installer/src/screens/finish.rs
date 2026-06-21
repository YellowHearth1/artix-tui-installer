//! Step 13 — completion. Congratulations, thanks, a donation QR code with the
//! link below it, and a Continue action (Enter reboots into the new system).

use crate::app::App;
use crate::i18n::t;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, BorderType, Paragraph},
    Frame,
};

/// The fundraiser the QR code and the printed link both point to.
const DONATE_URL: &str = "https://www.sternenkofund.org/fundraisings";

/// QR code for DONATE_URL, half-block encoded so each text row carries TWO
/// module rows: '█' both dark · '▀' top dark · '▄' bottom dark · ' ' both light.
/// Drawn with fg=Black on bg=White, so dark modules are black on a white field —
/// the orientation scanners expect (an inverted code often won't scan). The two
/// blank module-rings around it are the mandatory quiet zone. Generated offline
/// for this fixed URL and verified to decode back to it.
const QR: &[&str] = &[
    "                                 ",
    "  █▀▀▀▀▀█ ▄▄▀██ ▀ ▄▀    █▀▀▀▀▀█  ",
    "  █ ███ █ █ █▄█▄▀▀▄▄ ▀█ █ ███ █  ",
    "  █ ▀▀▀ █  ███▄▄▀ ▀█▀▀█ █ ▀▀▀ █  ",
    "  ▀▀▀▀▀▀▀ █▄█ ▀ █▄▀ █▄█ ▀▀▀▀▀▀▀  ",
    "  ▀▄ ▄▄ ▀▄▀▀  ▀███▀▀▄█ █▀▄▄▀██   ",
    "  ▄▄█▄█▀▀███ █▀ ▄▀▄ ▄▄▀█▄▄ ▄     ",
    "   ▀▄▄█▀▀▄ ▄▄█▄▄ ██▄█▄█▄█▀▄  ▄█  ",
    "  ▀█▄▄▄▀▀▄ ▀▄▄█▀██ ▀▄▄  ▄█▄▀█ ▄  ",
    "  █▄▀ ▄▀▀▀█▄▄  ▀▀▄▄█▄█▄ ▄█▄▀█▄▄  ",
    "  █▀▄▄█ ▀▄ █▄▀█ ▄▀▄  ▀▄▄▀▄▄▀  ▀  ",
    "  ▀ ▀  ▀▀▀█▄▄▄ ▀▀█▀▀  █▀▀▀█▄▀▀▀  ",
    "  █▀▀▀▀▀█  ▀▄▄▄▀▄█▄▀███ ▀ █▀▀ ▄  ",
    "  █ ███ █  ▀▄▀ █▀██ ▄▀██▀█▀█▄▀▄  ",
    "  █ ▀▀▀ █    ▀ ▄██▀▀ ▀▀ ██▀██▀▄  ",
    "  ▀▀▀▀▀▀▀ ▀   ▀▀▀  ▀▀▀ ▀▀▀▀▀▀    ",
    "                                 ",
];

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    // Content fills the top; the Continue button is pinned to the bottom so it
    // stays visible even if the content above is tall.
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);

    // The QR block needs ~17 rows plus the surrounding text and the button.
    // On a short terminal, drop the QR and keep the link so nothing important is
    // pushed off-screen; the printed URL still gets the message across.
    let show_qr = v[0].height >= 24;

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("[ OK ]", theme::ok())));
    lines.push(Line::from(Span::styled(t(app.lang, "fin.title"), theme::title())));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(t(app.lang, "fin.thanks"), theme::gold())));
    lines.push(Line::from(Span::styled(t(app.lang, "fin.donate"), theme::heading())));
    lines.push(Line::from(""));
    if show_qr {
        let qr_style = Style::default().fg(Color::Black).bg(Color::White);
        for row in QR {
            lines.push(Line::from(Span::styled(*row, qr_style)));
        }
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(t(app.lang, "fin.support"), theme::dim())));
    lines.push(Line::from(Span::styled(DONATE_URL, theme::accent())));

    // Center every line; since the QR rows are all the same width they line up
    // into a centered block.
    let para = Paragraph::new(lines).alignment(Alignment::Center);
    f.render_widget(para, v[0]);

    let cont = Paragraph::new(Line::from(Span::styled(
        format!("[Enter] {}", t(app.lang, "fin.reboot")),
        theme::selected(),
    )))
    .alignment(Alignment::Center)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme::border()),
    );
    let btn = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(28), Constraint::Min(0)])
        .split(v[1]);
    f.render_widget(cont, btn[1]);

    app.can_advance = false;
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    if key.code == KeyCode::Enter {
        // Reboot the machine. On a real install this powers the box; off-target
        // it simply exits cleanly.
        let _ = crate::system::runner::capture("reboot", &[]);
        app.should_quit = true;
    }
}
