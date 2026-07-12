//! Step 3 — network. Flow:
//!   Choose  → skip (wired Ethernet)  OR  configure Wi-Fi
//!   Adapter → pick which Wi-Fi device to use (built-in vs external dongle)
//!   Networks→ pick an SSID found by that adapter
//!   Password→ enter passphrase, connect via the chosen adapter
//!
//! Everything goes through `nmcli` in machine-readable (`-t`) mode, which
//! requires the NetworkManager daemon to be RUNNING. The live ISO enables it
//! as a dinit boot service, and `ensure_nm_running()` below is the belt-and-
//! suspenders fallback that starts it by hand (covers older ISOs and exotic
//! live media).
//!
//! Design rule learned from a real field bug: Enter must NEVER be a silent
//! no-op. Early versions guarded Enter behind `!list.is_empty()`, so when the
//! daemon wasn't running the adapter list stayed empty and the key appeared
//! dead — users were stuck with no explanation. Now an Enter on an empty list
//! RETRIES the step (restart daemon / rescan) and a status line always says
//! what happened and what to do next.

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

/// Which sub-step of the network screen the user is on. Stored in `App` so the
/// draw/key handlers stay in sync across frames.
#[derive(PartialEq, Clone, Copy)]
pub enum Stage {
    Choose,   // skip vs configure wifi
    Adapter,  // pick the Wi-Fi device
    Networks, // list of SSIDs
    Password, // enter passphrase
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    // Four rows: hint, the main list/input, a one-line live status, actions.
    // The status row is where retry instructions and connection errors show up
    // — it's the difference between "Enter does nothing" and "ah, I should
    // press r to rescan".
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
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
            hint(
                f,
                rows[0],
                &format!("{}: {}", t(app.lang, "wifi.network"), app.wifi_ssid),
            );
            widgets::input(
                f,
                rows[1],
                &t(app.lang, "wifi.password"),
                &app.wifi_password,
                true,
                true,
            );
        }
    }

    // Live status line: errors in the warn colour, plain info dimmed. Empty
    // string renders as an empty row (harmless).
    if !app.wifi_status.is_empty() {
        let style = if app.wifi_status_is_error {
            theme::warn()
        } else {
            theme::dim()
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(app.wifi_status.clone(), style))),
            rows[2],
        );
    }

    // Advancing forward off this screen is allowed only from Choose (skip) —
    // the other stages advance via Enter once a selection is made.
    app.can_advance = app.wifi_stage == Stage::Choose;
    widgets::action_row(
        f,
        rows[3],
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        true,
    );
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
                    // Wired Ethernet: assume we're online now — fetch the
                    // live-environment prerequisites in the background.
                    start_prereqs(app);
                    app.goto_next(); // skip — wired
                } else {
                    app.wifi_stage = Stage::Adapter;
                    app.cursor = 0;
                    set_status(app, "", false);
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
            KeyCode::Enter => {
                if app.wifi_adapters.is_empty() {
                    // No adapters found. NOT a dead key: retry the whole
                    // detection path — (re)start NetworkManager, unblock
                    // rfkill, enumerate again. The status line explains the
                    // outcome either way.
                    load_adapters(app);
                } else {
                    app.wifi_adapter =
                        app.wifi_adapters[app.cursor.min(app.wifi_adapters.len() - 1)].clone();
                    app.wifi_stage = Stage::Networks;
                    app.cursor = 0;
                    set_status(app, "", false);
                    scan(app);
                }
            }
            KeyCode::Esc => {
                app.wifi_stage = Stage::Choose;
                app.cursor = 0;
                set_status(app, "", false);
            }
            _ => {}
        },
        Stage::Networks => match key.code {
            KeyCode::Up => app.cursor = app.cursor.saturating_sub(1),
            KeyCode::Down => {
                app.cursor = (app.cursor + 1).min(app.wifi_networks.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if app.wifi_networks.is_empty() {
                    // Empty list → Enter rescans instead of doing nothing.
                    scan(app);
                } else {
                    app.wifi_ssid =
                        app.wifi_networks[app.cursor.min(app.wifi_networks.len() - 1)].clone();
                    app.wifi_stage = Stage::Password;
                    set_status(app, "", false);
                }
            }
            KeyCode::Char('r') => scan(app), // rescan
            KeyCode::Esc => {
                app.wifi_stage = Stage::Adapter;
                app.cursor = 0;
                set_status(app, "", false);
            }
            _ => {}
        },
        Stage::Password => match key.code {
            KeyCode::Char(c) => app.wifi_password.push(c),
            KeyCode::Backspace => {
                app.wifi_password.pop();
            }
            KeyCode::Enter => {
                // Try to connect and VERIFY the result. Early versions fired
                // nmcli blind and advanced unconditionally — a wrong password
                // sailed through and the install then died much later on the
                // mirror step with a confusing error. Now: only advance once
                // nmcli reports the device as actually connected; otherwise
                // stay here, keep the typed password for editing, and show
                // what went wrong.
                match connect(app) {
                    Ok(()) => {
                        set_status(app, "", false);
                        // Connected — fetch the live-environment prerequisites
                        // in the background so they're ready when needed.
                        start_prereqs(app);
                        app.goto_next();
                    }
                    Err(detail) => {
                        let mut msg = t(app.lang, "wifi.err_connect");
                        if !detail.is_empty() {
                            // First line of nmcli's stderr, trimmed — enough to
                            // tell auth failure from "no such SSID".
                            msg.push_str(" — ");
                            msg.push_str(&detail);
                        }
                        set_status_owned(app, msg, true);
                    }
                }
            }
            KeyCode::Esc => {
                app.wifi_stage = Stage::Networks;
                set_status(app, "", false);
            }
            _ => {}
        },
    }
}

pub fn tick(_app: &mut App) {}

/// One-line status under the list: what just happened / what to press next.
fn set_status(app: &mut App, key_or_empty: &str, is_error: bool) {
    app.wifi_status = if key_or_empty.is_empty() {
        String::new()
    } else {
        t(app.lang, key_or_empty)
    };
    app.wifi_status_is_error = is_error;
}

fn set_status_owned(app: &mut App, msg: String, is_error: bool) {
    app.wifi_status = msg;
    app.wifi_status_is_error = is_error;
}

/// Make sure the NetworkManager daemon is actually up before talking to it.
///
/// `nmcli` is just a client: with the daemon down every query returns an
/// error and every list comes back empty. The live ISO enables the dinit
/// service, but older ISOs (and stock Artix media) don't — so if the daemon
/// isn't running we start it ourselves via dinitctl and poll briefly for it
/// to come up. Also unblocks Wi-Fi in rfkill: many laptops boot with the
/// radio soft-blocked, which looks exactly like "no adapters".
fn ensure_nm_running() -> bool {
    let running = |out: Result<String, String>| matches!(out, Ok(s) if s.trim().eq_ignore_ascii_case("running"));
    if running(crate::system::runner::capture(
        "nmcli",
        &["-t", "-f", "RUNNING", "general"],
    )) {
        let _ = crate::system::runner::capture("rfkill", &["unblock", "wifi"]);
        return true;
    }
    // Not running — try to start the dinit service (best effort; the name is
    // what networkmanager-dinit ships).
    let _ = crate::system::runner::capture("dinitctl", &["start", "NetworkManager"]);
    // Give the daemon a moment to come up; poll instead of one blind sleep.
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if running(crate::system::runner::capture(
            "nmcli",
            &["-t", "-f", "RUNNING", "general"],
        )) {
            let _ = crate::system::runner::capture("rfkill", &["unblock", "wifi"]);
            return true;
        }
    }
    false
}

/// Enumerate Wi-Fi devices: `nmcli -t -f DEVICE,TYPE device` → keep type==wifi.
/// This is what lets a user pick a stronger external dongle over the built-in
/// card. Sets the status line to a retry hint when nothing is found, and to a
/// daemon error when NetworkManager can't be started at all.
fn load_adapters(app: &mut App) {
    if !ensure_nm_running() {
        app.wifi_adapters.clear();
        set_status(app, "wifi.err_nm", true);
        return;
    }
    match crate::system::runner::capture("nmcli", &["-t", "-f", "DEVICE,TYPE", "device"]) {
        Ok(out) => {
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
            if adapters.is_empty() {
                set_status(app, "wifi.retry_adapters", true);
            } else {
                set_status(app, "", false);
            }
            app.wifi_adapters = adapters;
        }
        Err(_) => {
            app.wifi_adapters.clear();
            set_status(app, "wifi.err_nm", true);
        }
    }
}

/// `nmcli -t` escapes `:` inside field values as `\:` (and `\` as `\\`);
/// SSIDs may legitimately contain both, so undo that before display/use.
fn nmcli_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Scan on the chosen adapter only (`ifname <dev>`). `--rescan yes` forces a
/// fresh probe: right after the daemon starts its cache is empty, and without
/// the flag `list` happily returns that empty cache — which used to leave the
/// screen stuck on "scanning…" forever. Deduplicates by SSID while keeping
/// nmcli's signal-strength order (one network broadcast by several APs shows
/// up once).
fn scan(app: &mut App) {
    let mut args = vec!["-t", "-f", "SSID", "dev", "wifi", "list", "--rescan", "yes"];
    if !app.wifi_adapter.is_empty() {
        args.push("ifname");
        args.push(&app.wifi_adapter);
    }
    if let Ok(out) = crate::system::runner::capture("nmcli", &args) {
        let mut seen = std::collections::HashSet::new();
        let nets: Vec<String> = out
            .lines()
            .map(|s| nmcli_unescape(s.trim()))
            .filter(|s| !s.is_empty() && seen.insert(s.clone()))
            .collect();
        if nets.is_empty() {
            set_status(app, "wifi.retry_networks", true);
        } else {
            set_status(app, "", false);
        }
        app.wifi_networks = nets;
    } else {
        set_status(app, "wifi.retry_networks", true);
    }
}

/// Connect using the chosen adapter (`ifname <dev>`) and report the outcome.
///
/// Open networks: the `password` argument is only passed when the user typed
/// one — nmcli rejects an empty password on open APs. After nmcli returns
/// success we double-check the device really reached `connected` state, so a
/// half-finished association can't slip through as a false positive.
fn connect(app: &mut App) -> Result<(), String> {
    let mut args = vec!["dev", "wifi", "connect", app.wifi_ssid.as_str()];
    if !app.wifi_password.is_empty() {
        args.push("password");
        args.push(app.wifi_password.as_str());
    }
    if !app.wifi_adapter.is_empty() {
        args.push("ifname");
        args.push(&app.wifi_adapter);
    }
    match crate::system::runner::capture("nmcli", &args) {
        Ok(_) => {
            // Belt and suspenders: confirm the chosen device (or any wifi
            // device when none was pinned) is in the `connected` state.
            if device_connected(app) {
                Ok(())
            } else {
                Err(String::new())
            }
        }
        Err(e) => Err(first_line_trimmed(&e)),
    }
}

/// True when the selected Wi-Fi device reports STATE == connected.
fn device_connected(app: &App) -> bool {
    if let Ok(out) =
        crate::system::runner::capture("nmcli", &["-t", "-f", "DEVICE,STATE", "device"])
    {
        for l in out.lines() {
            let mut parts = l.splitn(2, ':');
            let dev = parts.next().unwrap_or("").trim();
            let state = parts.next().unwrap_or("").trim();
            let dev_matches = app.wifi_adapter.is_empty() || dev == app.wifi_adapter;
            if dev_matches && state == "connected" {
                return true;
            }
        }
    }
    false
}

/// First line of an error blob, clipped to something that fits a status row.
fn first_line_trimmed(e: &str) -> String {
    let line = e
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let mut s: String = line.chars().take(80).collect();
    if line.chars().count() > 80 {
        s.push('…');
    }
    s
}

/// As soon as we have internet, pull the few things the installer needs into the
/// (ephemeral) live environment — so it runs on a stock Artix base image without
/// them baked into the ISO. Fire-and-forget in a background thread: the work
/// finishes regardless of whether anyone reads the result. Runs at most once
/// (guarded).
fn start_prereqs(app: &mut App) {
    if app.prereq_started {
        return;
    }
    app.prereq_started = true;
    std::thread::spawn(|| {
        use std::process::Command;
        // git is the one tool a stock Artix base image lacks but the installer
        // needs (post-install setup). Sync the db once, then install it.
        let _ = Command::new("pacman")
            .args(["-Sy", "--needed", "--noconfirm", "git"])
            .output();
        // Best-effort: the partitioning / chroot / mkfs tools are present on any
        // Artix install medium, but on a truly minimal image pull them too.
        // --needed skips whatever is already installed; all names are valid Artix
        // packages, so the transaction never aborts on "target not found".
        let _ = Command::new("pacman")
            .args([
                "-S",
                "--needed",
                "--noconfirm",
                "gptfdisk",
                "dosfstools",
                "e2fsprogs",
                "util-linux",
                "parted",
                "arch-install-scripts",
            ])
            .output();
    });
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "wifi.footer")
}
