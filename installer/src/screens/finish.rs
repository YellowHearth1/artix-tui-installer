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

/// The same code drawn with FULL blocks only, for a font that has no half ones.
///
/// The compact form packs two module rows into every text row, using `▀ ▄ █`.
/// Terminus — the default console font — has only `█`, so on it the code came
/// out as a field of holes that no scanner could read.
///
/// This expands it back out: one text row per module row, and TWO columns per
/// module so the modules stay square. A character cell is about twice as tall as
/// it is wide, and a QR code with rectangular modules is one scanners refuse.
/// The result is 74 columns by 38 rows instead of 37 by 19 — it needs a taller
/// console, which is why the compact form is still preferred when the font can
/// draw it.
fn qr_full_blocks(rows: &[&str]) -> Vec<String> {
    let mut out = Vec::with_capacity(rows.len() * 2);
    for row in rows {
        let mut top = String::new();
        let mut bottom = String::new();
        for ch in row.chars() {
            let (t, b) = match ch {
                '\u{2588}' => (true, true),  // █ both halves dark
                '\u{2580}' => (true, false), // ▀ upper half
                '\u{2584}' => (false, true), // ▄ lower half
                _ => (false, false),
            };
            top.push_str(if t { "\u{2588}\u{2588}" } else { "  " });
            bottom.push_str(if b { "\u{2588}\u{2588}" } else { "  " });
        }
        out.push(top);
        out.push(bottom);
    }
    out
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    // Content fills the top; the Continue button is pinned to the bottom so it
    // stays visible even if the content above is tall.
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(11)])
        .split(area);

    // The donation QR, in whichever form this console can actually show.
    //
    // Preferred: the compact half-block code, 19 rows. If the loaded font has no
    // half blocks — Terminus, the default, does not — it is drawn with full
    // blocks instead, which works on every font but needs twice the height and
    // twice the width. Only if NEITHER fits is it dropped, and the printed link
    // below still gets the message across.
    //
    // It used to be dropped outright on a font without half blocks, which meant
    // the code was missing on the default console font: the one place it was
    // most likely to be looked at.
    let qr = donate_qr(app.lang);
    let panel_h = v[0].height as usize;
    let panel_w = v[0].width as usize;
    let compact_fits = panel_h >= qr.len() + 10
        && crate::screens::fontpick::can_draw_half_blocks(&app.config.console_font);
    let wide = qr_full_blocks(qr);
    let wide_fits = panel_h >= wide.len() + 10 && panel_w >= wide[0].chars().count();
    let qr_rows: Vec<String> = if compact_fits {
        qr.iter().map(|r| (*r).to_string()).collect()
    } else if wide_fits {
        wide
    } else {
        Vec::new()
    };
    let show_qr = !qr_rows.is_empty();

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
        for row in &qr_rows {
            lines.push(Line::from(Span::styled(row.clone(), qr_style)));
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
        let prefix = if sel { "> " } else { "  " };
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

#[cfg(test)]
mod tests {

    /// The full-block code carries the SAME modules as the compact one.
    ///
    /// It is the compact art re-drawn, so if the expansion is wrong the code
    /// still looks like a QR and simply does not scan — the worst kind of bug to
    /// find by eye. Decoding both back to a module grid is the check.
    #[test]
    fn the_wide_qr_is_the_same_code_as_the_compact_one() {
        for rows in [QR_UK, QR_EN] {
            let wide = qr_full_blocks(rows);
            assert_eq!(wide.len(), rows.len() * 2, "one text row per module row");

            for (r, row) in rows.iter().enumerate() {
                for (c, ch) in row.chars().enumerate() {
                    let (want_top, want_bottom) = match ch {
                        '\u{2588}' => (true, true),
                        '\u{2580}' => (true, false),
                        '\u{2584}' => (false, true),
                        _ => (false, false),
                    };
                    // Each module became two columns; either tells the truth.
                    let dark = |line: &str| -> bool {
                        line.chars().nth(c * 2) == Some('\u{2588}')
                            && line.chars().nth(c * 2 + 1) == Some('\u{2588}')
                    };
                    assert_eq!(
                        dark(&wide[r * 2]),
                        want_top,
                        "row {r} col {c}: upper module differs"
                    );
                    assert_eq!(
                        dark(&wide[r * 2 + 1]),
                        want_bottom,
                        "row {r} col {c}: lower module differs"
                    );
                }
            }
        }
    }

    /// A font without half blocks still gets a code, given the room for it.
    ///
    /// Terminus is the default and has no `▀ ▄`. Dropping the code there left it
    /// missing on the console most people see it on.
    #[test]
    fn the_qr_survives_a_font_that_has_no_half_blocks() {
        let wide = qr_full_blocks(QR_UK);
        assert!(
            wide.iter()
                .all(|r| !r.contains('\u{2580}') && !r.contains('\u{2584}')),
            "the fallback still uses half blocks, which is the whole problem"
        );
        assert!(
            wide[0].chars().count() == QR_UK[0].chars().count() * 2,
            "modules must stay square: two columns per module"
        );
    }

    use super::*;

    /// Decode the half-block art back into a module matrix:
    /// each text row carries two module rows.
    fn modules(rows: &[&str]) -> Vec<Vec<bool>> {
        let mut out = Vec::new();
        for r in rows {
            let mut top = Vec::new();
            let mut bot = Vec::new();
            for ch in r.chars() {
                top.push(ch == '█' || ch == '▀');
                bot.push(ch == '█' || ch == '▄');
            }
            out.push(top);
            out.push(bot);
        }
        out
    }

    /// The QR art is hand-pasted block characters, so a stray edit — one
    /// glyph, one trimmed trailing space — silently produces a code that
    /// still *looks* fine and scans to nothing, or to somewhere else. Both
    /// codes were generated for their exact URL and verified to decode back
    /// to it; this pins the structure that verification relied on: a square
    /// 29×29 symbol (QR version 3), the spec's 4-module quiet zone on every
    /// side, and only the four legal half-block glyphs.
    #[test]
    fn the_donate_qr_codes_are_structurally_intact() {
        for (name, art) in [("QR_UK", QR_UK), ("QR_EN", QR_EN)] {
            for (i, row) in art.iter().enumerate() {
                assert!(
                    row.chars().all(|c| matches!(c, ' ' | '█' | '▀' | '▄')),
                    "{name} row {i} has a glyph that is not a half-block"
                );
                assert_eq!(
                    row.chars().count(),
                    art[0].chars().count(),
                    "{name} row {i} is a different width — the code is skewed"
                );
            }

            let m = modules(art);
            let dark: Vec<(usize, usize)> = m
                .iter()
                .enumerate()
                .flat_map(|(y, row)| {
                    row.iter()
                        .enumerate()
                        .filter(|(_, &d)| d)
                        .map(move |(x, _)| (y, x))
                })
                .collect();
            assert!(!dark.is_empty(), "{name} is blank");

            let (y0, y1) = (
                dark.iter().map(|p| p.0).min().unwrap(),
                dark.iter().map(|p| p.0).max().unwrap(),
            );
            let (x0, x1) = (
                dark.iter().map(|p| p.1).min().unwrap(),
                dark.iter().map(|p| p.1).max().unwrap(),
            );
            assert_eq!(y1 - y0 + 1, 29, "{name} is not 29 modules tall");
            assert_eq!(x1 - x0 + 1, 29, "{name} is not 29 modules wide");

            let w = m[0].len();
            assert!(y0 >= 4, "{name} quiet zone above is {y0}, needs 4");
            assert!(m.len() - 1 - y1 >= 4, "{name} quiet zone below is too thin");
            assert!(x0 >= 4, "{name} quiet zone left is {x0}, needs 4");
            assert!(w - 1 - x1 >= 4, "{name} quiet zone right is too thin");
        }

        assert_ne!(
            QR_UK, QR_EN,
            "each language must keep its own code — one was pasted over the other"
        );
    }

    /// FNV-1a over the art, newline after each row. Hand-rolled because
    /// `DefaultHasher` is explicitly not stable across Rust releases, and a
    /// pinned constant has to outlive the toolchain.
    fn fnv1a(rows: &[&str]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |b: u8| {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        };
        for r in rows {
            r.bytes().for_each(&mut mix);
            mix(b'\n');
        }
        h
    }

    /// The structural test above catches skew, stray glyphs and a missing
    /// quiet zone — but NOT a single flipped module deep inside the payload,
    /// which is the one corruption that stays invisible and sends a scan
    /// somewhere else. Nothing short of decoding catches that, and a decoder
    /// is not a dependency worth taking, so the verified art is pinned by
    /// content instead.
    ///
    /// If this fails, the QR was edited. That is only correct when the URL
    /// changed: regenerate with `segno.make(url, error='m')`, confirm the new
    /// code decodes back to the exact URL, then update these hashes in the
    /// same commit — never update them to make a red test go away.
    #[test]
    fn the_donate_qr_codes_still_encode_the_urls_they_were_verified_for() {
        assert_eq!(
            fnv1a(QR_UK),
            0x1090_518f_3e0e_c323,
            "QR_UK changed — does it still decode to {DONATE_URL_UK} ?"
        );
        assert_eq!(
            fnv1a(QR_EN),
            0x1350_e49e_8dec_2faf,
            "QR_EN changed — does it still decode to {DONATE_URL_EN} ?"
        );
    }

    /// Both links must stay on the fund's permanent donate page: individual
    /// fundraisings close, and a dead link on the last screen is a donation
    /// that never happens.
    #[test]
    fn the_donate_links_point_at_the_permanent_page() {
        use crate::i18n::Lang;
        assert_eq!(donate_url(Lang::En), DONATE_URL_EN);
        assert_eq!(donate_url(Lang::Uk), DONATE_URL_UK);
        assert_ne!(donate_url(Lang::Uk), donate_url(Lang::En));
        for url in [DONATE_URL_UK, DONATE_URL_EN] {
            assert!(
                url.starts_with("https://www.sternenkofund.org/"),
                "{url} is not the fund's site"
            );
            assert!(url.ends_with("/donate"), "{url} is not the donate page");
        }
    }
}
