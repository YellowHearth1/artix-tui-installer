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
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

/// The fundraiser the QR code and the printed link point to — the permanent
/// donate page (stable URL, unlike individual fundraisings). Language-aware:
/// Ukrainian users get the native page, English users the /en/ page. Both QR
/// codes were generated with segno (error level M) for their exact URL and
/// machine-verified (pyzbar) to decode back to it, including at terminal-cell
/// aspect ratios.
const DONATE_URL_UK: &str = "https://www.sternenkofund.org/donate";
const DONATE_URL_EN: &str = "https://www.sternenkofund.org/en/donate";

/// Returns the donate URL for the active language.
fn donate_url(lang: crate::i18n::Lang) -> &'static str {
    match lang {
        crate::i18n::Lang::En => DONATE_URL_EN,
        _ => DONATE_URL_UK,
    }
}

/// QR code, half-block encoded so each text row carries TWO module rows:
/// '█' both dark · '▀' top dark · '▄' bottom dark · ' ' both light. Drawn with
/// fg=Black on bg=White, so dark modules are black on a white field — the
/// orientation scanners expect (an inverted code often won't scan). The
/// 4-module quiet zone around it is the spec-mandated margin.
const QR_UK: &[&str] = &[
    "                                     ",
    "                                     ",
    "    █▀▀▀▀▀█  █▀█▀  █▄  ▀▀ █▀▀▀▀▀█    ",
    "    █ ███ █ █▀▀▄▄  █▀▄ ▀▀ █ ███ █    ",
    "    █ ▀▀▀ █ █  ▀██▄▄▀█▀▀█ █ ▀▀▀ █    ",
    "    ▀▀▀▀▀▀▀ █▄▀▄▀▄▀▄█ █▄█ ▀▀▀▀▀▀▀    ",
    "    ▀ ████▀▄▄▄▀ █ ▀▀█▀▄█▄ ███▀▀ ▄    ",
    "    █▄▄▀█▀▀█▀█▄ █ ▄█▀ ▄▄▀▀▄▄ ▄ ▄     ",
    "    ▄▀  ▄ ▀█ █▀▄▄▄▄▀▄███▄ ▄ ▄▀▀ ▄    ",
    "    ▄▀ ▀ █▀▀ ▄██▀█▀▀▄▀▄▄▄▄ █▄▀▀▄     ",
    "    ▄▄██▄ ▀▀ █ █  ▀ ▄█▄█▄▄▄█▄▀█ ▄    ",
    "    █ ▀█▀█▀█▀  ▀█ ▄▀█▀  █  █▄ ▀▄     ",
    "    ▀ ▀▀ ▀▀▀█▀██▀▄▄ ▄▀  █▀▀▀█▄███    ",
    "    █▀▀▀▀▀█ ▄██▀██▀█▄▀███ ▀ █▀▀▄     ",
    "    █ ███ █ █▄▄▀▄ ▀▀▄▀▄ █▀▀█▀▄█▄▄    ",
    "    █ ▀▀▀ █ ▀  ▄▄▀█▀█▀ ▀█▄▀█▀█▀█     ",
    "    ▀▀▀▀▀▀▀ ▀  ▀▀    ▀▀▀ ▀▀▀▀▀▀      ",
    "                                     ",
    "                                     ",
];

const QR_EN: &[&str] = &[
    "                                     ",
    "                                     ",
    "    █▀▀▀▀▀█  ▄█ ▄  ▄▀  ▀▀ █▀▀▀▀▀█    ",
    "    █ ███ █ █▀█ ▀▄█ ▀▄ ▀▀ █ ███ █    ",
    "    █ ▀▀▀ █ █   █ █▄██▀▀█ █ ▀▀▀ █    ",
    "    ▀▀▀▀▀▀▀ █▄▀ █ ▀ █ █▄█ ▀▀▀▀▀▀▀    ",
    "    █▄████▀▄▄▀▄▄█▀▀ ▄▀▄█▄ ███▀▀ ▄    ",
    "     ▄▄ ▀▄▀▄ ▄ ▄  ██▄ ▄▄▀▀▄▄ ▄ ▄     ",
    "    ▀▀ █▄▀▀▀▀▀ ▄   ▀▄███▄ ▄ ▄▀▀ ▄    ",
    "    █▄▄▄█ ▀▀█ █ █  ▀█▀▄▄▄▄ █▄▀▀▄     ",
    "    █▄▄█ █▀▄▄▄███▄   █▄█▄▄▄█▄▀█ ▄    ",
    "    █ ▄▄▄ ▀ █ ▀█ █▀▀▄▀  █  █▄ ▀▄     ",
    "    ▀  ▀▀▀▀ █▀▄▀  ▀ ▄▀  █▀▀▀█▄███    ",
    "    █▀▀▀▀▀█ ▄███▀ ▄██▀███ ▀ █▀▀ ▄    ",
    "    █ ███ █ █ ▀▄▀▄▄▀▄▀▄ █▀▀█▀▄█▄▄    ",
    "    █ ▀▀▀ █ ▀▀▀▄  ▀▀█▀ ▀█▄▀█▀█▀█     ",
    "    ▀▀▀▀▀▀▀ ▀ ▀▀▀▀▀  ▀▀▀ ▀▀▀▀▀▀      ",
    "                                     ",
    "                                     ",
];

/// Returns the QR rows for the active language.
fn donate_qr(lang: crate::i18n::Lang) -> &'static [&'static str] {
    match lang {
        crate::i18n::Lang::En => QR_EN,
        _ => QR_UK,
    }
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    // Content fills the top; the Continue button is pinned to the bottom so it
    // stays visible even if the content above is tall.
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(11)])
        .split(area);

    // The QR needs its own rows plus 7 lines of text above and 3 below. On a
    // short terminal, drop the QR and keep the link so nothing important is
    // pushed off-screen; the printed URL still gets the message across.
    let qr = donate_qr(app.lang);
    let show_qr = (v[0].height as usize) >= qr.len() + 10;

    let mut lines: Vec<Line> = vec![Line::from("")];
    lines.push(Line::from(Span::styled("[ OK ]", theme::ok())));
    lines.push(Line::from(Span::styled(
        t(app.lang, "fin.title"),
        theme::title(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        t(app.lang, "fin.thanks"),
        theme::gold(),
    )));
    lines.push(Line::from(Span::styled(
        t(app.lang, "fin.donate"),
        theme::heading(),
    )));
    lines.push(Line::from(""));
    if show_qr {
        let qr_style = Style::default().fg(Color::Black).bg(Color::White);
        for row in qr {
            lines.push(Line::from(Span::styled(*row, qr_style)));
        }
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        t(app.lang, "fin.support"),
        theme::dim(),
    )));
    lines.push(Line::from(Span::styled(
        donate_url(app.lang),
        theme::accent(),
    )));

    // Secure Boot reminder: if the user opted to PREPARE Secure Boot, make it
    // impossible to miss that it isn't finished — enabling it needs manual BIOS
    // steps on the running system (brick risk). Full steps are in the
    // ~/SECURE-BOOT.txt file the installer wrote to their home.
    if app.config.prepare_secureboot && app.config.bootloader.supports_secureboot_prep() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            t(app.lang, "fin.sb_title"),
            theme::warn(),
        )));
        lines.push(Line::from(Span::styled(
            t(app.lang, "fin.sb_body"),
            theme::normal(),
        )));
    }

    // Center every line; since the QR rows are all the same width they line up
    // into a centered block.
    let para = Paragraph::new(lines).alignment(Alignment::Center);
    f.render_widget(para, v[0]);

    // ---- end-of-install menu + safe-shutdown note ----
    let user = if app.config.username.trim().is_empty() {
        "root"
    } else {
        app.config.username.as_str()
    };
    let opts: [String; 3] = [
        t(app.lang, "fin.reboot"),
        t(app.lang, "fin.poweroff"),
        format!("{}  [{}]", t(app.lang, "fin.enter_user"), user),
    ];
    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        t(app.lang, "fin.choose"),
        theme::heading(),
    ))];
    for (i, o) in opts.iter().enumerate() {
        let sel = i == app.finish_cursor;
        let prefix = if sel { "▸ " } else { "  " };
        let style = if sel {
            theme::selected()
        } else {
            theme::normal()
        };
        lines.push(Line::from(Span::styled(format!("{prefix}{o}"), style)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        t(app.lang, "fin.unmount_note"),
        theme::dim(),
    )));
    lines.push(Line::from(Span::styled(
        t(app.lang, "fin.nav"),
        theme::mute(),
    )));
    let menu = Paragraph::new(lines)
        .alignment(Alignment::Left)
        .wrap(ratatui::widgets::Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme::border()),
        );
    f.render_widget(menu, v[1]);

    app.can_advance = false;
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.finish_cursor = app.finish_cursor.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.finish_cursor = (app.finish_cursor + 1).min(2);
        }
        KeyCode::Enter => match app.finish_cursor {
            0 => {
                // Reboot into the new system: unmount the target cleanly first
                // (umount -R /mnt + close LUKS), then reboot. Off-target this is
                // a harmless no-op + clean exit.
                crate::system::recovery::cleanup();
                let _ = crate::system::runner::capture("reboot", &[]);
                app.should_quit = true;
            }
            1 => {
                // Power off: same clean unmount, then poweroff.
                crate::system::recovery::cleanup();
                let _ = crate::system::runner::capture("poweroff", &[]);
                app.should_quit = true;
            }
            _ => {
                // Drop into the installed system for final manual steps, as the
                // user (or root if no user was created). Copy the live
                // resolv.conf in first so DNS works inside the chroot. The run
                // loop suspends the TUI, runs the shell, then — on exit —
                // unmounts cleanly and reboots (with a cancel window).
                let _ = std::fs::copy("/etc/resolv.conf", "/mnt/etc/resolv.conf");
                let args = if app.config.username.trim().is_empty() {
                    vec!["/mnt".to_string()]
                } else {
                    vec![
                        "/mnt".to_string(),
                        "su".to_string(),
                        "-".to_string(),
                        app.config.username.clone(),
                    ]
                };
                app.pending_interactive = Some(("artix-chroot".to_string(), args));
            }
        },
        _ => {}
    }
}
