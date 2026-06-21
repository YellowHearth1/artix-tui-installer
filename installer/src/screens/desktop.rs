//! Step 5 — desktop environment. A scrollable list of every desktop available
//! in the stable Artix repositories, with the packages each one installs shown
//! below the highlighted entry.

use crate::app::{App, Desktop};
use crate::i18n::t;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

/// All desktops, sourced from the single source of truth in app.rs.
fn options() -> &'static [Desktop] {
    Desktop::ALL
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let opts = options();
    if app.de_focus > 1 {
        app.de_focus = 1;
    }
    let de_panel_focused = app.de_focus == 0;

    use crate::screens::options::{dm_label, DM_ORDER};

    // Vertical stack: Desktop list on top, Login-screen list beneath it, then a
    // short info strip — the natural top-to-bottom reading order. The DM panel
    // is sized to its (fixed, short) list; the DE list takes the rest.
    let dm_h = (DM_ORDER.len() as u16) + 2; // rows + borders
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(6),           // desktop list
            Constraint::Length(5),        // info strip (about the desktop)
            Constraint::Length(dm_h),     // login-screen list
        ])
        .spacing(1)
        .split(area);
    // Index map after the reorder: DE list = rows[0], info = rows[1], DM = rows[2].
    let de_area = rows[0];
    let info_area = rows[1];
    let dm_area = rows[2];

    // ---- Desktop environments (top) ----
    let de_items: Vec<ListItem> = opts
        .iter()
        .map(|d| {
            let selected = app.config.desktop == format!("{:?}", d);
            let (mark, mark_style) = if selected {
                ("[✓] ", theme::ok())
            } else {
                ("[ ] ", theme::mute())
            };
            let name_style = if !d.note().is_empty() {
                theme::warn()
            } else if selected {
                theme::gold()
            } else {
                theme::normal()
            };
            ListItem::new(Line::from(vec![
                Span::styled(mark, mark_style),
                Span::styled(d.label().to_string(), name_style),
                Span::styled(
                    if d.session_tag().is_empty() {
                        String::new()
                    } else {
                        format!("  [{}]", d.session_tag())
                    },
                    theme::dim(),
                ),
            ]))
        })
        .collect();
    let de_title = t(app.lang, "de.title");
    let de_block = if de_panel_focused {
        theme::panel(&de_title)
    } else {
        theme::panel_dim(&de_title)
    };
    let de_list = List::new(de_items)
        .block(de_block)
        .highlight_style(if de_panel_focused { theme::selected() } else { theme::mute() })
        .highlight_symbol(if de_panel_focused { "▎ " } else { "  " });
    let mut de_state = ListState::default();
    de_state.select(Some(app.cursor.min(opts.len() - 1)));
    f.render_stateful_widget(de_list, de_area, &mut de_state);

    // ---- Login screen / display manager (middle) ----
    let dm_idx = DM_ORDER
        .iter()
        .position(|m| *m == app.config.display_manager)
        .unwrap_or(0);
    let dm_items: Vec<ListItem> = DM_ORDER
        .iter()
        .map(|id| {
            let selected = *id == app.config.display_manager;
            let (mark, mark_style) = if selected {
                ("[✓] ", theme::ok())
            } else {
                ("[ ] ", theme::mute())
            };
            let name_style = if selected { theme::gold() } else { theme::normal() };
            ListItem::new(Line::from(vec![
                Span::styled(mark, mark_style),
                Span::styled(dm_label(id).to_string(), name_style),
            ]))
        })
        .collect();
    let dm_title = t(app.lang, "de.dm_label");
    let dm_block = if !de_panel_focused {
        theme::panel(&dm_title)
    } else {
        theme::panel_dim(&dm_title)
    };
    let dm_list = List::new(dm_items)
        .block(dm_block)
        .highlight_style(if !de_panel_focused { theme::selected() } else { theme::mute() })
        .highlight_symbol(if !de_panel_focused { "▎ " } else { "  " });
    let mut dm_state = ListState::default();
    dm_state.select(Some(dm_idx));
    f.render_stateful_widget(dm_list, dm_area, &mut dm_state);

    // ---- Info strip (bottom) ----
    let d = opts[app.cursor.min(opts.len() - 1)];
    let pkgs = d.packages();
    let sub = if pkgs.is_empty() {
        t(app.lang, "de.minimal_note")
    } else {
        format!("pacman -S {}", pkgs.join(" "))
    };
    let info_lines = {
        let mut v = vec![
            Line::from(Span::styled(d.label().to_string(), theme::accent())),
            Line::from(Span::styled(format!("  {sub}"), theme::dim())),
        ];
        let note = d.note();
        if !note.is_empty() {
            v.push(Line::from(Span::styled(format!("  ⚠ {note}"), theme::warn())));
        }
        if d.supports_wayland() && d.supports_x11() {
            let wl = app.config.session == "wayland";
            v.push(Line::from(vec![
                Span::styled(format!("  {} ", t(app.lang, "de.session_label")), theme::normal()),
                Span::styled("‹ Wayland ›", if wl { theme::gold() } else { theme::mute() }),
                Span::styled("  ", theme::dim()),
                Span::styled("‹ X11 ›", if !wl { theme::gold() } else { theme::mute() }),
                Span::styled(format!("   ({})", t(app.lang, "de.session_hint")), theme::dim()),
            ]));
        } else if !d.session_tag().is_empty() {
            v.push(Line::from(Span::styled(
                format!("  {}: {}", t(app.lang, "de.session_label"), d.session_tag()),
                theme::dim(),
            )));
        }
        v
    };
    let info = Paragraph::new(info_lines)
        .wrap(ratatui::widgets::Wrap { trim: true })
        .block(theme::box_rounded());
    f.render_widget(info, info_area);

    app.can_advance = true;

    // Seat/login-manager modal, drawn last so it overlays everything.
    if app.seat_modal_open {
        draw_seat_modal(f, app, area);
    }
}

/// A centered modal that forces an explicit seat/login-manager choice, with
/// clear explanations. elogind (universal, recommended) is the default; seatd
/// is the minimal Wayland-only option.
fn draw_seat_modal(f: &mut Frame, app: &App, area: Rect) {
    use ratatui::widgets::{Block, Borders, Clear};
    use ratatui::widgets::BorderType;

    // Center a box ~64 wide within the area. Height grows by 2 when the X11
    // warning is shown so it never clips.
    let chosen = desktop_from_cfg(&app.config.desktop);
    let x11_session = chosen != Desktop::None && !chosen.session_is_wayland(&app.config.session);
    let w = 64u16.min(area.width.saturating_sub(4));
    let h = (if x11_session { 18 } else { 13 }).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let modal = Rect { x, y, width: w, height: h };

    // Clear the area behind the modal so the list doesn't show through.
    f.render_widget(Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::accent())
        .title(format!(" {} ", t(app.lang, "seat.title")));
    f.render_widget(block.clone(), modal);

    // Inner content area.
    let inner = Rect {
        x: modal.x + 2,
        y: modal.y + 1,
        width: modal.width.saturating_sub(4),
        height: modal.height.saturating_sub(2),
    };

    let elogind_sel = app.seat_modal_cursor == 0;
    let seatd_sel = app.seat_modal_cursor == 1;
    let mark = |on: bool| if on { "(•) " } else { "( ) " };

    let mut lines = vec![
        Line::from(Span::styled(t(app.lang, "seat.prompt"), theme::normal())),
        Line::from(""),
        Line::from(vec![
            Span::styled(mark(elogind_sel), if elogind_sel { theme::ok() } else { theme::mute() }),
            Span::styled("elogind", if elogind_sel { theme::gold() } else { theme::normal() }),
            Span::styled(format!("  — {}", t(app.lang, "seat.elogind_rec")), theme::dim()),
        ]),
        Line::from(Span::styled(format!("      {}", t(app.lang, "seat.elogind_desc")), theme::dim())),
        Line::from(""),
        Line::from(vec![
            Span::styled(mark(seatd_sel), if seatd_sel { theme::ok() } else { theme::mute() }),
            Span::styled("seatd", if seatd_sel { theme::gold() } else { theme::normal() }),
        ]),
        Line::from(Span::styled(format!("      {}", t(app.lang, "seat.seatd_desc")), theme::dim())),
    ];
    // X11 warning: vanilla Xorg has no libseat backend, so seatd + an X11 DE
    // can mean a black screen / a desktop that won't start. Always shown for an
    // X11 desktop so the choice is informed, but it STANDS OUT (bold ⚠) when the
    // risky option — seatd — is the highlighted one, and stays a muted reminder
    // while elogind (the safe pick) is highlighted.
    if x11_session {
        lines.push(Line::from(""));
        let (prefix, style) = if seatd_sel {
            ("⚠ ", theme::warn()) // risky choice highlighted → bold warning
        } else {
            ("ⓘ ", theme::dim()) // elogind highlighted → gentle reminder
        };
        lines.push(Line::from(Span::styled(
            format!("{}{}", prefix, t(app.lang, "seat.x11_warn")),
            style,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(t(app.lang, "seat.footer"), theme::mute())));

    let para = Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true });
    f.render_widget(para, inner);
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    // When the seat/login-manager modal is open, it captures all keys.
    if app.seat_modal_open {
        handle_modal_key(app, key);
        return;
    }

    let opts = options();
    let cur = opts[app.cursor.min(opts.len() - 1)];
    use crate::screens::options::DM_ORDER;

    match key.code {
        // ↑/↓ flow continuously: through the DE list, then into the DM list,
        // and back. de_focus picks which list the cursor is in.
        KeyCode::Up | KeyCode::Char('k') => {
            if app.de_focus == 1 {
                let i = DM_ORDER
                    .iter()
                    .position(|m| *m == app.config.display_manager)
                    .unwrap_or(0);
                if i == 0 {
                    // Top of the DM list → jump back up into the DE list.
                    app.de_focus = 0;
                } else {
                    cycle_dm(app, false);
                }
            } else {
                app.cursor = app.cursor.saturating_sub(1);
                let d = options()[app.cursor.min(options().len() - 1)];
                apply_desktop_defaults(app, d);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.de_focus == 0 {
                if app.cursor + 1 < opts.len() {
                    app.cursor += 1;
                    let d = options()[app.cursor.min(options().len() - 1)];
                    apply_desktop_defaults(app, d);
                } else {
                    // Bottom of the DE list → drop into the DM list.
                    app.de_focus = 1;
                }
            } else {
                cycle_dm(app, true);
            }
        }
        // ←/→ toggles the session (Wayland/X11) for the highlighted desktop
        // when it supports both. (The DM list is reached with ↓, not ←/→.)
        KeyCode::Right => {
            if app.de_focus == 0 {
                toggle_session(app, cur, true);
            }
        }
        KeyCode::Left => {
            if app.de_focus == 0 && cur.supports_wayland() && cur.supports_x11() {
                toggle_session(app, cur, false);
            }
        }
        // Space marks the highlighted DE (in the DE list). In the DM list the
        // selection already follows the cursor, so Space is a harmless confirm.
        KeyCode::Char(' ') => {
            if app.de_focus == 0 {
                app.config.desktop = format!("{:?}", cur);
            }
        }
        // Enter walks the screen's three picks in this order, then advances:
        //   • DE list  → record the desktop and open the seat modal RIGHT AWAY,
        //     so the seat/login-manager choice appears immediately after picking
        //     a desktop. Confirming the modal then drops focus to the DM list
        //     (see handle_modal_key) — it does NOT advance yet.
        //   • DM list  → the desktop + seat are chosen and the user has picked
        //     the login screen; advance to the next screen.
        // Net flow: desktop → seat → login screen → next. All three picks are
        // captured, with seat shown right after the desktop.
        KeyCode::Enter => {
            if app.de_focus == 0 {
                app.config.desktop = format!("{:?}", cur);
                app.seat_modal_cursor = if app.config.seat_provider == "seatd" { 1 } else { 0 };
                app.seat_modal_open = true;
            } else {
                app.goto_next();
            }
        }
        KeyCode::Esc => {
            // Esc steps focus up: from the login-screen list back to the
            // desktop list. (On the desktop list — focus 0 — the global handler
            // intercepts Esc and leaves to the previous screen, so this branch
            // only runs while focus is on the DM list.)
            app.de_focus = 0;
        }
        _ => {}
    }
}

/// Modal key handling: ↑/↓ or ←/→ to pick, Enter to confirm and advance, Esc to
/// cancel back to the list.
fn handle_modal_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Left | KeyCode::Char('k') => app.seat_modal_cursor = 0,
        KeyCode::Down | KeyCode::Right | KeyCode::Char('j') => app.seat_modal_cursor = 1,
        KeyCode::Enter => {
            app.config.seat_provider =
                if app.seat_modal_cursor == 1 { "seatd".into() } else { "elogind".into() };
            app.seat_modal_open = false;
            // Don't advance yet: drop focus to the login-screen (DM) list so the
            // user picks the login screen as the last step. Enter there advances.
            app.de_focus = 1;
        }
        KeyCode::Esc => {
            app.seat_modal_open = false;
        }
        _ => {}
    }
}

/// Resolve the serialized desktop string in config back to a `Desktop`.
/// (Matches against the canonical `{:?}` names used when storing the choice.)
fn desktop_from_cfg(s: &str) -> Desktop {
    Desktop::ALL
        .iter()
        .copied()
        .find(|d| format!("{d:?}") == s)
        .unwrap_or(Desktop::None)
}

/// Cycle the login screen (display manager) in the given direction.
fn cycle_dm(app: &mut App, forward: bool) {
    use crate::screens::options::DM_ORDER;
    let i = DM_ORDER
        .iter()
        .position(|m| *m == app.config.display_manager)
        .unwrap_or(0);
    let n = if forward {
        (i + 1) % DM_ORDER.len()
    } else {
        (i + DM_ORDER.len() - 1) % DM_ORDER.len()
    };
    app.config.display_manager = DM_ORDER[n].into();
}

/// Switch a dual-session desktop between X11 and Wayland (no seat change here).
fn toggle_session(app: &mut App, d: Desktop, _forward: bool) {
    if d.supports_wayland() && d.supports_x11() {
        app.config.session = if app.config.session == "wayland" { "x11".into() } else { "wayland".into() };
        // The seat backend is NOT forced here any more: the user chooses it in
        // the seat modal at the end (with an X11 warning). We only nudge the
        // DEFAULT toward elogind when switching to X11, since that's the safe
        // pick for vanilla Xorg — but it stays overridable.
        if app.config.session == "x11" && app.config.seat_provider.is_empty() {
            app.config.seat_provider = "elogind".into();
        }
    }
}

/// When the highlighted desktop changes, reset session/seat to that desktop's
/// sensible default so the info panel shows a valid combination.
fn apply_desktop_defaults(app: &mut App, d: Desktop) {
    app.config.session = d.default_session().into();
    // Default seat backend: elogind for an X11 desktop (safe with vanilla
    // Xorg), seatd otherwise. This is only a DEFAULT — the user can still pick
    // either backend in the seat modal (X11 + seatd shows a warning there).
    if d != Desktop::None && !d.session_is_wayland(&app.config.session) {
        app.config.seat_provider = "elogind".into();
    } else if app.config.seat_provider.is_empty() {
        app.config.seat_provider = "seatd".into();
    }
    // Keep the display-manager DEFAULT in sync with the desktop: no desktop →
    // no DM, a graphical desktop → SDDM. Only flip between those two defaults;
    // an explicit greetd choice (made on the Options screen) is never touched.
    match d {
        Desktop::None => {
            if app.config.display_manager == "sddm" {
                app.config.display_manager = "none".into();
            }
        }
        _ => {
            if app.config.display_manager == "none" {
                app.config.display_manager = "sddm".into();
            }
        }
    }
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "de.footer")
}
