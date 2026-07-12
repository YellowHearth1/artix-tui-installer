//! Virtual-Wi-Fi test harness, reachable from the mode chooser (third entry).
//!
//! Why this exists: the network screen can't be exercised in a VM, because a
//! VM has no wireless hardware — which is exactly how a real Wi-Fi bug went
//! unnoticed for many releases. This screen loads `mac80211_hwsim` (simulated
//! radios), puts hostapd on one of them as an access point, and leaves the
//! other for NetworkManager. The installer's Wi-Fi screen then sees a genuine
//! adapter and a genuine network, and the whole flow — scan, pick, wrong
//! password, right password — can be walked for real.
//!
//! The script is EMBEDDED in the binary (scripts::WIFI_TEST_SCRIPT), so it
//! works on any live medium regardless of what the ISO overlay shipped.
//! On real hardware it's a harmless no-op: the module simply won't load.

use crate::app::{App, Screen};
use crate::i18n::t;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // hint
            Constraint::Min(0),    // output
            Constraint::Length(1), // action line
        ])
        .spacing(1)
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            t(app.lang, "wt.hint"),
            theme::dim(),
        )))
        .wrap(ratatui::widgets::Wrap { trim: true }),
        rows[0],
    );

    // Output pane: empty until the user presses Enter, then the script's stdout
    // and stderr, line by line.
    let body: Vec<Line> = if app.wifitest_log.is_empty() {
        vec![Line::from(Span::styled(
            t(app.lang, "wt.run"),
            theme::accent(),
        ))]
    } else {
        app.wifitest_log
            .iter()
            .map(|l| Line::from(Span::styled(l.clone(), theme::normal())))
            .collect()
    };
    // The harness output is longer than a short console can show (a stock
    // 80×25 text console fits ~18 lines here, the output is ~24). The TAIL is
    // the part that matters — the verdict plus the box with the adapter,
    // network and password to type — so when the content overflows, anchor
    // the view to the bottom. Wrapping can add extra visual rows on narrow
    // consoles, so count wrapped rows, not just lines.
    let inner_w = rows[1].width.saturating_sub(2).max(1) as usize;
    let inner_h = rows[1].height.saturating_sub(2) as usize;
    let total_rows: usize = app
        .wifitest_log
        .iter()
        .map(|l| {
            let w = l.chars().count().max(1);
            w.div_ceil(inner_w)
        })
        .sum::<usize>()
        .max(1);
    let scroll = total_rows.saturating_sub(inner_h) as u16;
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default().borders(Borders::ALL))
            .wrap(ratatui::widgets::Wrap { trim: false })
            .scroll((scroll, 0)),
        rows[1],
    );

    let action = if app.wifitest_running {
        t(app.lang, "wt.running")
    } else if app.wifitest_log.is_empty() {
        t(app.lang, "wt.back")
    } else {
        t(app.lang, "wt.done")
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(action, theme::dim()))),
        rows[2],
    );

    app.can_advance = false;
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            if app.wifitest_running {
                return; // already going — ignore a double press
            }
            run(app);
        }
        KeyCode::Esc => {
            app.screen = Screen::Mode;
        }
        _ => {}
    }
}

/// Write the embedded script to a temp file and run it, collecting the output.
///
/// Run synchronously: the whole thing takes a couple of seconds (module load +
/// hostapd start), and blocking briefly keeps the code simple — no channel, no
/// background thread, no partial-render states to reason about. The screen is
/// a developer/testing tool, not part of the install flow.
fn run(app: &mut App) {
    use std::io::Write;
    use std::process::Command;

    app.wifitest_running = true;
    app.wifitest_log.clear();

    let path = "/tmp/artix-wifi-test.sh";
    let write_ok = std::fs::File::create(path)
        .and_then(|mut f| f.write_all(crate::system::install::WIFI_TEST_SCRIPT.as_bytes()));
    if let Err(e) = write_ok {
        app.wifitest_log.push(format!("!! {e}"));
        app.wifitest_running = false;
        return;
    }

    match Command::new("sh").arg(path).output() {
        Ok(out) => {
            let push = |v: &mut Vec<String>, bytes: &[u8]| {
                for l in String::from_utf8_lossy(bytes).lines() {
                    // Strip the ANSI colour codes the script prints — the TUI
                    // draws its own styling and raw escapes would show as junk.
                    v.push(strip_ansi(l));
                }
            };
            push(&mut app.wifitest_log, &out.stdout);
            push(&mut app.wifitest_log, &out.stderr);
            if app.wifitest_log.is_empty() {
                app.wifitest_log.push("(no output)".into());
            }
        }
        Err(e) => app.wifitest_log.push(format!("!! sh: {e}")),
    }
    app.wifitest_running = false;
}

/// Remove ANSI SGR escape sequences (`ESC [ … m`) from a line.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip until the terminating letter of the escape sequence.
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "wt.back")
}
