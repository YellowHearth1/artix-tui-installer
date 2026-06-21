//! Step 3 — network. Flow:
//!   Choose  → skip (wired Ethernet)  OR  configure Wi-Fi
//!   Adapter → pick which Wi-Fi device to use (built-in vs external dongle)
//!   Networks→ pick an SSID found by that adapter
//!   Password→ enter passphrase, connect via the chosen adapter
//! Everything goes through nmcli in machine-readable (-t) mode.

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

#[derive(PartialEq, Clone, Copy)]
pub enum Stage {
    Choose,   // skip vs configure wifi
    Adapter,  // pick the Wi-Fi device
    Networks, // list of SSIDs
    Password, // enter passphrase
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0), Constraint::Length(3)])
        .spacing(1)
        .split(area);

    match app.wifi_stage {
        Stage::Choose => {
            hint(f, rows[0], &t(app.lang, "wifi.intro"));
            let items = vec![
                format!("  {}", t(app.lang, "wifi.skip_wired")),
                format!("  {}", t(app.lang, "wifi.scan")),
            ];
            widgets::select_list(f, rows[1], &items, app.cursor);
        }
        Stage::Adapter => {
            hint(f, rows[0], &t(app.lang, "wifi.adapter_pick"));
            let items: Vec<String> = if app.wifi_adapters.is_empty() {
                vec![format!("  {}", t(app.lang, "wifi.no_adapter"))]
            } else {
                app.wifi_adapters.iter().map(|s| format!("  {s}")).collect()
            };
            widgets::select_list(f, rows[1], &items, app.cursor);
        }
        Stage::Networks => {
            let header = if app.wifi_adapter.is_empty() {
                t(app.lang, "wifi.pick")
            } else {
                format!("{} ({})", t(app.lang, "wifi.pick"), app.wifi_adapter)
            };
            hint(f, rows[0], &header);
            let items: Vec<String> = if app.wifi_networks.is_empty() {
                vec![format!("  {}", t(app.lang, "wifi.scanning"))]
            } else {
                app.wifi_networks.iter().map(|s| format!("  {s}")).collect()
            };
            widgets::select_list(f, rows[1], &items, app.cursor);
        }
        Stage::Password => {
            hint(f, rows[0], &format!("{}: {}", t(app.lang, "wifi.network"), app.wifi_ssid));
            widgets::input(f, rows[1], &t(app.lang, "wifi.password"), &app.wifi_password, true, true);
        }
    }

    // Advancing forward off this screen is allowed only from Choose (skip) —
    // the other stages advance via Enter once a selection is made.
    app.can_advance = app.wifi_stage == Stage::Choose;
    widgets::action_row(f, rows[2], &t(app.lang, "app.back"), &t(app.lang, "app.next"), true);
}

fn hint(f: &mut Frame, area: Rect, text: &str) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(text.to_string(), theme::dim()))),
        area,
    );
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match app.wifi_stage {
        Stage::Choose => match key.code {
            KeyCode::Up => app.cursor = app.cursor.saturating_sub(1),
            KeyCode::Down => app.cursor = (app.cursor + 1).min(1),
            KeyCode::Enter => {
                if app.cursor == 0 {
                    app.goto_next(); // skip — wired
                } else {
                    app.wifi_stage = Stage::Adapter;
                    app.cursor = 0;
                    load_adapters(app);
                }
            }
            _ => {}
        },
        Stage::Adapter => match key.code {
            KeyCode::Up => app.cursor = app.cursor.saturating_sub(1),
            KeyCode::Down => {
                app.cursor = (app.cursor + 1).min(app.wifi_adapters.len().saturating_sub(1))
            }
            KeyCode::Enter if !app.wifi_adapters.is_empty() => {
                app.wifi_adapter = app.wifi_adapters[app.cursor.min(app.wifi_adapters.len() - 1)].clone();
                app.wifi_stage = Stage::Networks;
                app.cursor = 0;
                scan(app);
            }
            KeyCode::Esc => {
                app.wifi_stage = Stage::Choose;
                app.cursor = 0;
            }
            _ => {}
        },
        Stage::Networks => match key.code {
            KeyCode::Up => app.cursor = app.cursor.saturating_sub(1),
            KeyCode::Down => {
                app.cursor = (app.cursor + 1).min(app.wifi_networks.len().saturating_sub(1))
            }
            KeyCode::Enter if !app.wifi_networks.is_empty() => {
                app.wifi_ssid = app.wifi_networks[app.cursor.min(app.wifi_networks.len() - 1)].clone();
                app.wifi_stage = Stage::Password;
            }
            KeyCode::Char('r') => scan(app), // rescan
            KeyCode::Esc => {
                app.wifi_stage = Stage::Adapter;
                app.cursor = 0;
            }
            _ => {}
        },
        Stage::Password => match key.code {
            KeyCode::Char(c) => app.wifi_password.push(c),
            KeyCode::Backspace => {
                app.wifi_password.pop();
            }
            KeyCode::Enter => {
                connect(app);
                app.goto_next();
            }
            KeyCode::Esc => app.wifi_stage = Stage::Networks,
            _ => {}
        },
    }
}

pub fn tick(_app: &mut App) {}

/// Enumerate Wi-Fi devices: `nmcli -t -f DEVICE,TYPE device` → keep type==wifi.
/// This is what lets a user pick a stronger external dongle over the built-in
/// card.
fn load_adapters(app: &mut App) {
    if let Ok(out) = crate::system::runner::capture("nmcli", &["-t", "-f", "DEVICE,TYPE", "device"]) {
        let adapters: Vec<String> = out
            .lines()
            .filter_map(|l| {
                let mut parts = l.splitn(2, ':');
                let dev = parts.next()?.trim();
                let ty = parts.next()?.trim();
                if ty == "wifi" && !dev.is_empty() {
                    Some(dev.to_string())
                } else {
                    None
                }
            })
            .collect();
        app.wifi_adapters = adapters;
    }
}

/// Scan on the chosen adapter only (`ifname <dev>`).
fn scan(app: &mut App) {
    let mut args = vec!["-t", "-f", "SSID", "dev", "wifi", "list"];
    if !app.wifi_adapter.is_empty() {
        args.push("ifname");
        args.push(&app.wifi_adapter);
    }
    if let Ok(out) = crate::system::runner::capture("nmcli", &args) {
        let mut nets: Vec<String> = out
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        nets.dedup();
        app.wifi_networks = nets;
    }
}

/// Connect using the chosen adapter (`ifname <dev>`).
fn connect(app: &mut App) {
    let mut args = vec!["dev", "wifi", "connect", app.wifi_ssid.as_str(), "password", app.wifi_password.as_str()];
    if !app.wifi_adapter.is_empty() {
        args.push("ifname");
        args.push(&app.wifi_adapter);
    }
    let _ = crate::system::runner::capture("nmcli", &args);
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "wifi.footer")
}
