//! Recovery tool (outside the install flow). Scans block devices, lets the user
//! pick the root partition, optionally unlocks LUKS (passphrase or USB key),
//! mounts the system (root + boot/EFI + anything in its fstab), detects the
//! installed bootloader, and then hands the user an interactive chroot shell to
//! repair the system by hand.
//!
//! Focus rows (recovery_focus):
//!   0 — target root partition list
//!   1 — unlock method (none / passphrase / USB key)
//!   2 — passphrase entry (only meaningful when method = passphrase)
//!   3 — action: mount & open chroot

use crate::app::App;
use crate::i18n::t;
use crate::screens::widgets;
use crate::system::recovery;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};
use std::sync::OnceLock;

/// Partitions on the machine, scanned once (path, size, fstype, label).
fn partitions() -> &'static Vec<recovery::Partition> {
    static P: OnceLock<Vec<recovery::Partition>> = OnceLock::new();
    P.get_or_init(|| recovery::list_partitions().unwrap_or_default())
}

const UNLOCK_LABELS: [&str; 3] = ["none", "passphrase", "usbkey"];

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // hint
            Constraint::Min(4),    // partition list
            Constraint::Length(3), // unlock method
            Constraint::Length(3), // passphrase
            Constraint::Min(3),    // status / detected bootloader
            Constraint::Length(3), // action row
        ])
        .spacing(1)
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            t(app.lang, "rec.hint"),
            theme::dim(),
        ))),
        rows[0],
    );

    // 1) Root partition list.
    let parts = partitions();
    let list_focused = app.recovery_focus == 0;
    let pblock = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if list_focused { theme::border() } else { theme::border_dim() })
        .title(format!(" {} ", t(app.lang, "rec.root_part")))
        .title_style(theme::dim());
    let pinner = pblock.inner(rows[1]);
    f.render_widget(pblock, rows[1]);
    if parts.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("  —", theme::mute()))),
            pinner,
        );
    } else {
        let items: Vec<String> = parts
            .iter()
            .map(|p| {
                let label = if p.label.is_empty() { "" } else { &p.label };
                format!("  {}   {}   {}   {}", p.path, p.size, p.fstype, label)
            })
            .collect();
        widgets::select_list(f, pinner, &items, app.recovery_disk_cursor);
    }

    // 2) Unlock method (segmented pills).
    let um_focused = app.recovery_focus == 1;
    let um_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if um_focused { theme::border() } else { theme::border_dim() })
        .title(format!(" {} ", t(app.lang, "rec.unlock")))
        .title_style(theme::dim());
    let um_inner = um_block.inner(rows[2]);
    f.render_widget(um_block, rows[2]);
    let pills: Vec<Span> = UNLOCK_LABELS
        .iter()
        .enumerate()
        .flat_map(|(i, key)| {
            let sel = i == app.recovery_unlock;
            let label = t(app.lang, &format!("rec.unlock_{key}"));
            let style = if sel { theme::selected() } else { theme::normal() };
            vec![Span::styled(format!(" {label} "), style), Span::raw("  ")]
        })
        .collect();
    f.render_widget(Paragraph::new(Line::from(pills)), um_inner);

    // 3) Passphrase entry — only active when method = passphrase.
    let pp_focused = app.recovery_focus == 2;
    let pp_active = app.recovery_unlock == 1;
    let pp_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if pp_focused && pp_active { theme::border() } else { theme::border_dim() })
        .title(format!(" {} ", t(app.lang, "rec.passphrase")))
        .title_style(theme::dim());
    let pp_inner = pp_block.inner(rows[3]);
    f.render_widget(pp_block, rows[3]);
    let pp_text = if !pp_active {
        Span::styled(format!("  {}", t(app.lang, "rec.passphrase_na")), theme::mute())
    } else {
        Span::styled(
            format!("  {}", "•".repeat(app.recovery_passphrase.chars().count())),
            theme::normal(),
        )
    };
    f.render_widget(Paragraph::new(Line::from(pp_text)), pp_inner);

    // 4) Status / detected bootloader (or instructions).
    let st_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border_dim())
        .title(format!(" {} ", t(app.lang, "rec.status")))
        .title_style(theme::dim());
    let st_inner = st_block.inner(rows[4]);
    f.render_widget(st_block, rows[4]);
    let status = if app.recovery_status.is_empty() {
        t(app.lang, "rec.status_idle")
    } else {
        app.recovery_status.clone()
    };
    f.render_widget(
        Paragraph::new(status).wrap(Wrap { trim: true }).style(theme::normal()),
        st_inner,
    );

    // 5) Action row — label changes once mounted (mount → open root shell).
    let act_focused = app.recovery_focus == 3;
    let action_label = if app.recovery_mounted {
        t(app.lang, "rec.open_shell")
    } else {
        t(app.lang, "rec.mount")
    };
    let act_style = if act_focused { theme::selected() } else { theme::normal() };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("  [ {action_label} ]"), act_style),
            Span::raw("    "),
            Span::styled(t(app.lang, "rec.back_to_mode"), theme::mute()),
        ])),
        rows[5],
    );

    // Recovery is its own flow: never let the global "next" advance install.
    app.can_advance = false;
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    let parts = partitions();
    // Once mounted, the screen just offers the root chroot — Enter hands off,
    // Esc unmounts back to the mode chooser (handled in event.rs / main loop).
    if app.recovery_mounted {
        match key.code {
            KeyCode::Enter => {
                app.pending_interactive =
                    Some(("artix-chroot".into(), vec!["/mnt".into()]));
            }
            KeyCode::Esc => app.screen = crate::app::Screen::Mode,
            _ => {}
        }
        return;
    }
    match key.code {
        // ↑/↓ (k/j): move the selection WITHIN the focused field — the disk
        // list on the partition row, the method on the unlock row. This matches
        // how list/option screens elsewhere in the installer behave.
        KeyCode::Up | KeyCode::Char('k') => match app.recovery_focus {
            0 => app.recovery_disk_cursor = app.recovery_disk_cursor.saturating_sub(1),
            1 => app.recovery_unlock = app.recovery_unlock.saturating_sub(1),
            _ => {}
        },
        KeyCode::Down | KeyCode::Char('j') => match app.recovery_focus {
            0 => {
                if !parts.is_empty() {
                    app.recovery_disk_cursor =
                        (app.recovery_disk_cursor + 1).min(parts.len() - 1);
                }
            }
            1 => app.recovery_unlock = (app.recovery_unlock + 1).min(2),
            _ => {}
        },
        // ←/→ (h/l): also switch the unlock method, for muscle memory.
        KeyCode::Left | KeyCode::Char('h') => {
            if app.recovery_focus == 1 {
                app.recovery_unlock = app.recovery_unlock.saturating_sub(1);
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if app.recovery_focus == 1 {
                app.recovery_unlock = (app.recovery_unlock + 1).min(2);
            }
        }
        // Typing into the passphrase field (only when focused + method=passphrase).
        KeyCode::Char(c) if app.recovery_focus == 2 && app.recovery_unlock == 1 => {
            app.recovery_passphrase.push(c);
        }
        KeyCode::Backspace if app.recovery_focus == 2 && app.recovery_unlock == 1 => {
            app.recovery_passphrase.pop();
        }
        // Enter: confirm the current field and ADVANCE to the next — exactly
        // like Enter elsewhere in the installer. The passphrase field is skipped
        // unless the unlock method is "passphrase". On the last field (action)
        // Enter performs the mount + bootloader detection.
        KeyCode::Enter => match app.recovery_focus {
            0 => {
                if !parts.is_empty() {
                    app.recovery_focus = 1; // disk chosen → unlock method
                }
            }
            1 => {
                // method chosen: passphrase → entry field; none/usbkey → action
                app.recovery_focus = if app.recovery_unlock == 1 { 2 } else { 3 };
            }
            2 => app.recovery_focus = 3, // passphrase entered → action
            3 => recovery::mount_and_detect(app, parts),
            _ => {}
        },
        // Esc: step BACK to the previous field; from the first field, leave
        // recovery to the mode chooser. (Mirrors the installer's back-nav.)
        KeyCode::Esc => match app.recovery_focus {
            0 => app.screen = crate::app::Screen::Mode,
            2 => app.recovery_focus = 1,
            3 => app.recovery_focus = if app.recovery_unlock == 1 { 2 } else { 1 },
            _ => app.recovery_focus = app.recovery_focus.saturating_sub(1),
        },
        _ => {}
    }
}

pub fn footer_hint(app: &App) -> String {
    if app.recovery_mounted {
        t(app.lang, "rec.footer_mounted")
    } else {
        t(app.lang, "rec.footer")
    }
}
