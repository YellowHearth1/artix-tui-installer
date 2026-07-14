//! Step 5 — desktop environment. A scrollable list of every desktop available
//! in the stable Artix repositories, with the packages each one installs shown
//! below the highlighted entry.

use crate::app::{App, Desktop, SeatProvider};
use crate::i18n::t;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{HighlightSpacing, List, ListItem, ListState, Paragraph},
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
    let info_h = 5;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(6),         // desktop list
            Constraint::Length(info_h), // info strip (about the desktop)
            Constraint::Length(dm_h),   // login-screen list
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
            let dname = format!("{:?}", d);
            let selected = if *d == Desktop::None {
                app.config.desktops.is_empty()
            } else {
                app.config.desktops.contains(&dname)
            };
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
        .highlight_style(if de_panel_focused {
            theme::selected()
        } else {
            theme::mute()
        })
        .highlight_symbol(if de_panel_focused { "▎ " } else { "  " })
        .highlight_spacing(HighlightSpacing::Always);
    let mut de_state = ListState::default();
    de_state.select(Some(app.cursor.min(opts.len().saturating_sub(1))));
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
            let name_style = if selected {
                theme::gold()
            } else {
                theme::normal()
            };
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
        .highlight_style(if !de_panel_focused {
            theme::selected()
        } else {
            theme::mute()
        })
        .highlight_symbol(if !de_panel_focused { "▎ " } else { "  " })
        // Always reserve the highlight-symbol column so every row's [ ]/[✓] mark
        // sits at the same x whether or not a row is highlighted.
        .highlight_spacing(HighlightSpacing::Always);
    let mut dm_state = ListState::default();
    // Only highlight a row while the DM list is the focused panel. When it was
    // passive, highlighting the configured row let ratatui's highlight overpaint
    // its [✓] mark, so that row rendered as blank space. With no selection each
    // row just draws its own mark, and the configured DM still reads from its [✓].
    dm_state.select(if !de_panel_focused {
        Some(dm_idx)
    } else {
        None
    });
    f.render_stateful_widget(dm_list, dm_area, &mut dm_state);

    // ---- Info strip (bottom) ----
    let info_lines = {
        let d = opts[app.cursor.min(opts.len() - 1)];
        let pkgs = d.packages();
        let sub = if pkgs.is_empty() {
            t(app.lang, "de.minimal_note")
        } else {
            format!("pacman -S {}", pkgs.join(" "))
        };
        let mut v = vec![
            Line::from(Span::styled(d.label().to_string(), theme::accent())),
            Line::from(Span::styled(format!("  {sub}"), theme::dim())),
        ];
        let note = d.note();
        if !note.is_empty() {
            v.push(Line::from(Span::styled(
                format!("  ⚠ {note}"),
                theme::warn(),
            )));
        }
        if d.supports_wayland() && d.supports_x11() {
            let wl = app.config.session == "wayland";
            v.push(Line::from(vec![
                Span::styled(
                    format!("  {} ", t(app.lang, "de.session_label")),
                    theme::normal(),
                ),
                Span::styled(
                    "‹ Wayland ›",
                    if wl { theme::gold() } else { theme::mute() },
                ),
                Span::styled("  ", theme::dim()),
                Span::styled("‹ X11 ›", if !wl { theme::gold() } else { theme::mute() }),
                Span::styled(
                    format!("   ({})", t(app.lang, "de.session_hint")),
                    theme::dim(),
                ),
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
    use ratatui::widgets::BorderType;
    use ratatui::widgets::{Block, Borders, Clear};

    // Center a box ~64 wide within the area. Height grows by 2 when the X11
    // warning is shown so it never clips.
    let x11_session = app
        .config
        .desktops
        .iter()
        .map(|s| desktop_from_cfg(s))
        .any(|d| d != Desktop::None && d.supports_x11());
    let w = 64u16.min(area.width.saturating_sub(4));
    let h = (if x11_session { 18 } else { 13 }).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let modal = Rect {
        x,
        y,
        width: w,
        height: h,
    };

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
            Span::styled(
                mark(elogind_sel),
                if elogind_sel {
                    theme::ok()
                } else {
                    theme::mute()
                },
            ),
            Span::styled(
                "elogind",
                if elogind_sel {
                    theme::gold()
                } else {
                    theme::normal()
                },
            ),
            Span::styled(
                format!("  — {}", t(app.lang, "seat.elogind_rec")),
                theme::dim(),
            ),
        ]),
        Line::from(Span::styled(
            format!("      {}", t(app.lang, "seat.elogind_desc")),
            theme::dim(),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                mark(seatd_sel),
                if seatd_sel {
                    theme::ok()
                } else {
                    theme::mute()
                },
            ),
            Span::styled(
                "seatd",
                if seatd_sel {
                    theme::gold()
                } else {
                    theme::normal()
                },
            ),
        ]),
        Line::from(Span::styled(
            format!("      {}", t(app.lang, "seat.seatd_desc")),
            theme::dim(),
        )),
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
    lines.push(Line::from(Span::styled(
        t(app.lang, "seat.footer"),
        theme::mute(),
    )));

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
        // ↑/↓ move WITHIN the current section only. Moving BETWEEN sections
        // (DE list → seat modal → DM list) happens exclusively via Enter, so the
        // seat modal can never be skipped by scrolling past the end of a list.
        // de_focus picks which list the cursor is in.
        KeyCode::Up | KeyCode::Char('k') => {
            if app.de_focus == 1 {
                // In the DM list: cycle the login-manager choice, clamped at the
                // top (no jump back to the DE list — use Esc/Enter to move).
                let i = DM_ORDER
                    .iter()
                    .position(|m| *m == app.config.display_manager)
                    .unwrap_or(0);
                if i > 0 {
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
                // Walk down through the desktops, clamped at the last one (no
                // drop into the DM list — Enter opens the seat modal, which then
                // moves focus to the DM list).
                if app.cursor + 1 < opts.len() {
                    app.cursor += 1;
                    apply_desktop_defaults(app, options()[app.cursor]);
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
        // Space toggles the highlighted desktop in/out of the multi-select set.
        // "None" is the headless choice: ticking it clears every other pick.
        // (In the DM list the selection follows the cursor, so Space is inert.)
        KeyCode::Char(' ') => {
            if app.de_focus == 0 {
                let name = format!("{:?}", cur);
                if cur == Desktop::None {
                    app.config.desktops.clear();
                } else if let Some(i) = app.config.desktops.iter().position(|x| *x == name) {
                    app.config.desktops.remove(i);
                } else {
                    app.config.desktops.push(name);
                }
                // The set changed → refresh the seat/DM defaults from it.
                apply_desktop_defaults(app, cur);
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
                // If nothing is ticked yet, Enter adopts the highlighted desktop
                // so the common "highlight one + Enter" path still installs it.
                // With desktops already ticked via Space, Enter just proceeds.
                if app.config.desktops.is_empty() && cur != Desktop::None {
                    app.config.desktops.push(format!("{:?}", cur));
                    apply_desktop_defaults(app, cur);
                }
                app.seat_modal_cursor = if app.config.seat_provider == SeatProvider::Seatd {
                    1
                } else {
                    0
                };
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
            app.config.seat_provider = if app.seat_modal_cursor == 1 {
                SeatProvider::Seatd
            } else {
                SeatProvider::Elogind
            };
            // Lock the choice: from now on apply_desktop_defaults() must not
            // revert it when the desktop set changes.
            app.seat_chosen = true;
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
        app.config.session = if app.config.session == "wayland" {
            "x11".into()
        } else {
            "wayland".into()
        };
        // The seat backend is NOT forced here: the user chooses it in the seat
        // modal (with an X11 warning). We only nudge the DEFAULT toward elogind
        // when switching to X11 — but ONLY while the user hasn't confirmed a
        // seat yet. Once seat_chosen is set, the explicit pick is never touched.
        if app.config.session == "x11" && !app.seat_chosen {
            app.config.seat_provider = SeatProvider::Elogind;
        }
    }
}

/// When the highlighted desktop changes, refresh the info panel's session and
/// the seat/DM DEFAULTS. The session preview follows the highlighted desktop;
/// the seat backend and display-manager defaults follow the whole SET, so
/// merely scrolling over "None" while desktops are ticked never disables them.
fn apply_desktop_defaults(app: &mut App, d: Desktop) {
    // Session preview for the highlighted desktop (cosmetic: all of a desktop's
    // sessions are installed; this just picks which one the info panel shows).
    if d != Desktop::None {
        app.config.session = d.default_session().into();
    }
    let any_desktop = !app.config.desktops.is_empty();
    // Default seat backend, applied ONLY when the user hasn't chosen yet
    // (seat_provider empty). This is a DEFAULT, never an override: once the seat
    // modal has set a value, changing the desktop set must NOT silently revert
    // it. Preference when still unset: elogind if ANY ticked desktop has an X11
    // session (safe with vanilla Xorg out of the box), otherwise seatd. The
    // modal lets the user pick either regardless, with an X11+seatd warning —
    // and X11+seatd is fully supported (the installer writes Xwrapper.config so
    // Xorg runs rootful without logind).
    if !app.seat_chosen {
        let any_x11 = app
            .config
            .desktops
            .iter()
            .map(|s| desktop_from_cfg(s))
            .any(|x| x != Desktop::None && x.supports_x11());
        app.config.seat_provider = if any_x11 {
            SeatProvider::Elogind
        } else {
            SeatProvider::Seatd
        };
    }
    // Display-manager DEFAULT tracks whether ANY desktop is selected: desktops →
    // SDDM, headless → none. Only flips between those two defaults; an explicit
    // greetd choice (made on the Options screen) is never touched.
    if any_desktop {
        if app.config.display_manager == "none" {
            app.config.display_manager = "sddm".into();
        }
    } else if app.config.display_manager == "sddm" {
        app.config.display_manager = "none".into();
    }
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "de.footer")
}
