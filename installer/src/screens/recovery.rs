//! Recovery tool (outside the install flow). Scans block devices, lets the user
//! pick the root partition, optionally unlocks LUKS (passphrase or USB key),
//! mounts the system (root + boot/EFI + anything in its fstab), detects the
//! installed bootloader — and then offers NAMED REPAIRS.
//!
//! It started as "mount it and hand over a chroot", which is a tool for someone
//! who already knows both what broke and the words that fix it. Most people
//! arriving here know neither: they know the machine will not boot. So the
//! first action only LOOKS and reports, the next ones are the repairs that
//! actually come up — bootloader, initramfs, fstab, permissions — and the
//! chroot is last, for whatever the list does not cover.
//!
//! Focus rows (recovery_focus):
//!   0 — target root partition list
//!   1 — unlock method (none / passphrase / USB key)
//!   2 — passphrase entry (only meaningful when method = passphrase)
//!   3 — action list (see ACTIONS)

use crate::app::App;
use crate::i18n::t;
use crate::system::recovery;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use std::sync::OnceLock;

/// Partitions on the machine, scanned once (path, size, fstype, label).
/// Step the role of the partition under the cursor through the list that makes
/// sense FOR THAT PARTITION (`recovery::roles_for`) — a 512 MiB vfat is never
/// offered "root", because offering it only invites the mis-selection this
/// screen then has to explain.
///
/// The root is exclusive: giving it to one partition takes it from whichever
/// had it. Two roots would mean two candidate /mnt mounts and a silent choice
/// between them.
/// The mount path shown for a data partition: what the user chose, or the first
/// preset when they have not chosen yet.
/// "Where should this be mounted?" — the presets, plus a field to type any path.
///
/// The four presets are the ones the installer itself offers when it mounts an
/// extra disk, so a partition set up there is recognised here under the name it
/// was given. But a preset ring alone silently tells anyone who chose their own
/// folder that their case is not supported, so the last row is free text.
fn draw_path_modal(f: &mut Frame, app: &App) {
    let custom = app.recovery_path_cursor == recovery::DATA_PATHS.len();
    let mut lines = vec![Line::from("")];
    for (i, m) in recovery::DATA_PATHS.iter().enumerate() {
        let sel = i == app.recovery_path_cursor;
        lines.push(Line::from(Span::styled(
            format!("{}{m}", if sel { " > " } else { "   " }),
            if sel {
                theme::selected()
            } else {
                theme::normal()
            },
        )));
    }
    lines.push(Line::from(Span::styled(
        format!(
            "{}{}  {}|",
            if custom { " > " } else { "   " },
            t(app.lang, "rec.path_custom"),
            app.recovery_path_input
        ),
        if custom {
            theme::selected()
        } else {
            theme::normal()
        },
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(" {}", t(app.lang, "rec.hint_path")),
        theme::mute(),
    )));
    // Sized through the zoom helper like every other overlay: `+`/`-` change the
    // console font, and a box that ignored that would keep the same small frame
    // around bigger letters. A guard test enforces this.
    let rect = crate::screens::widgets::modal_rect_fit(
        f,
        60.max(crate::screens::widgets::text_width(&lines)),
        lines.len() as u16 + 2,
        app.modal_zoom,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {} ", t(app.lang, "rec.path_title")));
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

/// "Which of these should stop starting at boot?", newest at the top.
///
/// The case it is for: a service written by hand — or written for you — that
/// works well enough to be enabled and badly enough to stop the machine coming
/// up. From inside that machine there is nothing to be done, and the thing that
/// tells the culprit from the twenty services that have been fine for a year is
/// simply WHEN each was switched on.
///
/// It only ever removes the boot entry. The service description stays where it
/// is, so putting it back is one `dinitctl enable`, and every line of the run
/// says so.
fn draw_services_modal(f: &mut Frame, app: &App) {
    // How wide the box asks to be. The note is WRAPPED to it rather than left
    // to run off the end: the sentence saying this is reversible is the one
    // that makes it safe to try, and it was being cut in half.
    const BOX: usize = 78;
    let mut lines: Vec<Line<'static>> = wrap_words(
        &t(app.lang, "rec.svc_note").replace("{dir}", "/etc/dinit.d/boot.d"),
        BOX - 2,
    )
    .into_iter()
    .map(|l| Line::from(Span::styled(format!(" {l}"), theme::mute())))
    .collect();
    lines.push(Line::from(""));
    if app.recovery_svc.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(" {}", t(app.lang, "rec.svc_none")),
            theme::warn(),
        )));
    }
    // A window onto the list: a machine with forty services would otherwise
    // draw a box taller than the screen.
    const VIEW: usize = 12;
    let first = app.recovery_svc_cursor.saturating_sub(VIEW - 1).min(
        app.recovery_svc
            .len()
            .saturating_sub(VIEW.min(app.recovery_svc.len())),
    );
    for (i, s) in app.recovery_svc.iter().enumerate().skip(first).take(VIEW) {
        let sel = i == app.recovery_svc_cursor;
        let marked = app.recovery_svc_marked.get(i).copied().unwrap_or(false);
        // The same pair the repair panel uses for its verdict, and the only two
        // ticks any console font here can actually draw — a guard test keeps
        // that honest, because an undrawable glyph is a BLANK on the screen.
        let mark = if marked { "[✘]" } else { "[✔]" };
        let arrow = if s.target.is_empty() {
            String::new()
        } else {
            format!("  → {}", s.target)
        };
        // The name is padded so the targets line up: a column of arrows is what
        // makes "this one points somewhere I wrote myself" readable at a glance.
        lines.push(Line::from(Span::styled(
            format!(
                "{}{mark} {}  {:<18}{arrow}",
                if sel { " > " } else { "   " },
                s.stamp,
                s.name
            ),
            if marked {
                theme::warn()
            } else if sel {
                theme::selected()
            } else {
                theme::normal()
            },
        )));
    }
    if app.recovery_svc.len() > VIEW {
        lines.push(Line::from(Span::styled(
            format!(
                "   … {} / {}",
                app.recovery_svc_cursor + 1,
                app.recovery_svc.len()
            ),
            theme::mute(),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(" {}", t(app.lang, "rec.svc_hint")),
        theme::mute(),
    )));
    let rect = crate::screens::widgets::modal_rect_fit(
        f,
        (BOX as u16).max(crate::screens::widgets::text_width(&lines)),
        lines.len() as u16 + 2,
        app.modal_zoom,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {} ", t(app.lang, "rec.svc_title")));
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

/// Keys for the service picker. Space marks, Enter applies, Esc changes nothing.
fn services_modal_key(app: &mut App, key: KeyEvent) {
    let n = app.recovery_svc.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.recovery_svc_cursor = app.recovery_svc_cursor.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.recovery_svc_cursor = (app.recovery_svc_cursor + 1).min(n.saturating_sub(1));
        }
        KeyCode::Char(' ') => {
            if let Some(m) = app.recovery_svc_marked.get_mut(app.recovery_svc_cursor) {
                *m = !*m;
            }
        }
        KeyCode::Esc => app.recovery_svc_open = false,
        KeyCode::Enter => {
            let names: Vec<String> = app
                .recovery_svc
                .iter()
                .zip(app.recovery_svc_marked.iter())
                .filter(|(_, m)| **m)
                .map(|(s, _)| s.name.clone())
                .collect();
            // Enter never refuses silently: with nothing marked it says which
            // key marks a row, because Space on a list that also takes Enter is
            // exactly the thing people do not guess.
            if names.is_empty() {
                app.recovery_status = t(app.lang, "rec.svc_nothing_marked");
                return;
            }
            app.recovery_svc_open = false;
            start_job(
                app,
                ACT_SERVICES,
                crate::system::recovery::disable_services_script(&names),
            );
        }
        _ => {}
    }
}

/// Keys for the picker above. Esc abandons without changing anything.
fn path_modal_key(app: &mut App, key: KeyEvent) {
    let len = recovery::DATA_PATHS.len() + 1;
    let custom = app.recovery_path_cursor == recovery::DATA_PATHS.len();
    match key.code {
        KeyCode::Up => {
            app.recovery_path_cursor = app.recovery_path_cursor.saturating_sub(1);
        }
        KeyCode::Down => {
            app.recovery_path_cursor = (app.recovery_path_cursor + 1).min(len - 1);
        }
        KeyCode::Esc => {
            app.recovery_path_open = false;
            app.recovery_path_input.clear();
        }
        // TYPING IS THE POINT, so typing works from anywhere in the picker.
        // The presets are a convenience for the common paths; a path of your
        // own is the general case, and having to arrow down to a field first
        // made the box read as "pick one of these four".
        KeyCode::Char(c) => {
            if !custom {
                app.recovery_path_cursor = recovery::DATA_PATHS.len();
                app.recovery_path_input.clear();
            }
            app.recovery_path_input.push(c);
        }
        KeyCode::Backspace if custom => {
            app.recovery_path_input.pop();
        }
        KeyCode::Enter => {
            let chosen = if custom {
                app.recovery_path_input.trim().to_string()
            } else {
                recovery::DATA_PATHS[app.recovery_path_cursor].to_string()
            };
            // A path that is not absolute would be joined onto /mnt as a
            // relative name and land somewhere nobody asked for, so it is
            // refused here rather than surprising anyone at mount time.
            if !chosen.starts_with('/') {
                app.recovery_status = t(app.lang, "rec.err_path_abs");
                return;
            }
            let i = app.recovery_disk_cursor;
            if app.recovery_data_path.len() <= i {
                app.recovery_data_path.resize(i + 1, String::new());
            }
            app.recovery_data_path[i] = chosen;
            app.recovery_path_open = false;
            app.recovery_path_input.clear();
        }
        _ => {}
    }
}

fn data_path_of(app: &App, i: usize) -> String {
    app.recovery_data_path
        .get(i)
        .filter(|p| p.starts_with('/'))
        .cloned()
        .unwrap_or_else(|| recovery::DATA_PATHS[0].to_string())
}

fn cycle_role(app: &mut App, parts: &[recovery::Partition], forward: bool) {
    let i = app.recovery_disk_cursor;
    let Some(part) = parts.get(i) else { return };
    if app.recovery_roles.len() < parts.len() {
        app.recovery_roles.resize(parts.len(), recovery::ROLE_NONE);
    }
    if app.recovery_data_path.len() < parts.len() {
        app.recovery_data_path.resize(parts.len(), String::new());
    }
    let ring = recovery::roles_for(part);
    let cur = ring
        .iter()
        .position(|r| *r == app.recovery_roles[i])
        .unwrap_or(0);
    let next = if forward {
        (cur + 1) % ring.len()
    } else {
        (cur + ring.len() - 1) % ring.len()
    };
    let role = ring[next];
    if role == recovery::ROLE_ROOT {
        for (j, r) in app.recovery_roles.iter_mut().enumerate() {
            if j != i && *r == recovery::ROLE_ROOT {
                *r = recovery::ROLE_NONE;
            }
        }
    }
    app.recovery_roles[i] = role;
}

fn partitions() -> &'static Vec<recovery::Partition> {
    static P: OnceLock<Vec<recovery::Partition>> = OnceLock::new();
    P.get_or_init(|| recovery::list_partitions().unwrap_or_default())
}

const UNLOCK_LABELS: [&str; 3] = ["none", "passphrase", "usbkey"];

/// What recovery can do once a system is mounted, in the order a person needs
/// them: understand first, then the specific repairs, then a shell for anything
/// this list does not cover.
///
/// Named repairs rather than "here is a chroot, good luck": somebody whose
/// bootloader is gone knows WHAT broke and not the six words that fix it, and
/// somebody who does not know what broke needs to be told before touching
/// anything. Both were left to a bare shell before.
const ACTIONS: [&str; 8] = [
    "rec.act_diagnose",
    "rec.act_bootloader",
    "rec.act_initramfs",
    "rec.act_fstab",
    "rec.fix_perms",
    "rec.act_services",
    "rec.open_shell",
    "rec.act_reboot",
];
const ACTION_DESCS: [&str; 8] = [
    "rec.act_diagnose_desc",
    "rec.act_bootloader_desc",
    "rec.act_initramfs_desc",
    "rec.act_fstab_desc",
    "rec.fix_perms_desc",
    "rec.act_services_desc",
    "rec.open_shell_desc",
    "rec.act_reboot_desc",
];
/// Services opens a picker rather than running something; the last two are not
/// repairs at all — a shell, and the way out.
const ACT_SERVICES: usize = 5;
const ACT_SHELL: usize = 6;
const ACT_REBOOT: usize = 7;

/// WHAT a repair button runs — worked out separately from RUNNING it, so a test
/// can check every button on a machine that must not be chrooted into.
///
/// One argv element per script: `/tmp` inside the chroot is a fresh tmpfs on
/// every `artix-chroot` call, so a script written there would not exist by the
/// time it ran.
fn repair_script(action: usize) -> Option<String> {
    match action {
        1 => Some(crate::system::recovery::REINSTALL_BOOTLOADER.to_string()),
        2 => Some(crate::system::recovery::REBUILD_INITRAMFS.to_string()),
        3 => Some(crate::system::recovery::regenerate_fstab()),
        4 => Some(crate::system::install::FIX_PERMISSIONS.to_string()),
        _ => None,
    }
}

fn chroot_argv(script: String) -> (String, Vec<String>) {
    (
        "artix-chroot".to_string(),
        vec!["/mnt".into(), "sh".into(), "-c".into(), script],
    )
}

/// Run something in the target and watch it in the status panel.
///
/// Nothing started this way may ask a question — `fix-permissions.sh` passes
/// `--noconfirm`, the rest never prompt — because a panel that streams output
/// has no way to answer one. Anything interactive keeps the terminal instead.
fn start_job(app: &mut App, action: usize, script: String) {
    let (prog, args) = chroot_argv(script);
    app.recovery_job = Some(action);
    app.recovery_log.clear();
    app.recovery_done = None;
    app.recovery_scroll = 0;
    let argref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    app.recovery_rx = Some(crate::system::runner::spawn_with_mode(
        &prog, &argref, false,
    ));
}

fn start_repair(app: &mut App, action: usize) {
    if let Some(script) = repair_script(action) {
        start_job(app, action, script);
    }
}

/// Drain a running repair into the panel; when it ends, judge it and LOOK AGAIN.
///
/// The re-check is the point. "Did that work?" is the question every repair
/// leaves behind, and the screen already knows how to answer it — so a finished
/// repair is followed by the same read-only diagnosis, and the cursor lands on
/// whatever is still broken. Two faults get fixed the same way one does: repair,
/// read, repair, read, until nothing is left and the way out is the next button.
pub fn tick(app: &mut App) {
    let Some(rx) = app.recovery_rx.clone() else {
        return;
    };
    use crate::system::runner::LogLine;
    while let Ok(line) = rx.try_recv() {
        match line {
            LogLine::Out(s) | LogLine::Err(s) | LogLine::Prompt(s) => {
                app.recovery_log.push(s);
                if app.recovery_log.len() > 500 {
                    app.recovery_log.remove(0);
                }
            }
            LogLine::Done(res) => {
                app.recovery_rx = None;
                let ok = res.is_ok();
                app.recovery_done = Some(res.clone());
                app.recovery_log.push(String::new());
                app.recovery_log.push(match &res {
                    Ok(()) => t(app.lang, "rec.run_ok"),
                    Err(e) => t(app.lang, "rec.run_fail").replace("{err}", e.trim()),
                });
                if ok {
                    app.recovery_log.push(String::new());
                    app.recovery_log.push(t(app.lang, "rec.run_recheck"));
                    let (report, suggested) = crate::system::recovery::diagnose(app.lang);
                    for l in report.lines() {
                        app.recovery_log.push(l.to_string());
                    }
                    // Point at what is STILL wrong, or at the way out when
                    // nothing is.
                    app.recovery_action = if suggested == 0 {
                        ACT_REBOOT
                    } else {
                        suggested.min(ACTIONS.len() - 1)
                    };
                }
                return;
            }
        }
    }
}

/// Lay spans out across `width`, breaking BETWEEN them and never inside one.
///
/// A button that gets split across two lines stops looking like a button, and
/// `Wrap` cannot be told not to. Packing them here also gives the exact number
/// of rows the strip needs, which is what lets the row ask the layout for that
/// much instead of hoping three is enough.
fn pack_spans(spans: Vec<Span<'static>>, width: u16) -> Vec<Line<'static>> {
    let w = width.max(1) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for s in spans {
        let n = s.content.chars().count();
        // A separator that would start a line is dropped rather than indenting
        // the line after it by four spaces.
        if used == 0 && s.content.trim().is_empty() {
            continue;
        }
        if used + n > w && used > 0 {
            lines.push(Line::from(std::mem::take(&mut cur)));
            used = 0;
            if s.content.trim().is_empty() {
                continue;
            }
        }
        used += n;
        cur.push(s);
    }
    if !cur.is_empty() {
        lines.push(Line::from(cur));
    }
    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

/// Break `text` into lines of at most `width`, on spaces.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let w = width.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let n = word.chars().count();
        let cur_len = cur.chars().count();
        if cur_len == 0 {
            cur = word.to_string();
        } else if cur_len + 1 + n <= w {
            cur.push(' ');
            cur.push_str(word);
        } else {
            out.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// The buttons, as spans. Built in one place because the layout has to know how
/// tall the strip will be BEFORE it can hand it a rectangle to draw in.
fn action_spans(app: &App, act_focused: bool) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    if app.recovery_mounted {
        for (i, key) in ACTIONS.iter().enumerate() {
            let picked = act_focused && app.recovery_action == i;
            spans.push(Span::styled(
                format!("  [ {} ]", t(app.lang, key)),
                if picked {
                    theme::selected()
                } else {
                    theme::normal()
                },
            ));
        }
    } else {
        spans.push(Span::styled(
            format!("  [ {} ]", t(app.lang, "rec.mount")),
            if act_focused {
                theme::selected()
            } else {
                theme::normal()
            },
        ));
    }
    spans.push(Span::raw("    "));
    spans.push(Span::styled(t(app.lang, "rec.back_to_mode"), theme::mute()));
    spans
}

/// How many rows the action strip and its description need here.
///
/// WHEN IT CANNOT ALL FIT, THE DESCRIPTION GOES AND THE BUTTONS STAY. On a
/// small console the sections below add up to more than the screen has, and
/// something is losing rows either way; the only question is what. A hint about
/// the highlighted button is worth less than the button — and the last one in
/// the strip is the way out of the screen.
fn action_height(app: &App, width: u16, height: u16, pp_shown: bool) -> u16 {
    let buttons = pack_spans(action_spans(app, false), width).len() as u16;
    let desc = if app.recovery_mounted {
        let note = t(
            app.lang,
            ACTION_DESCS[app.recovery_action.min(ACTIONS.len() - 1)],
        );
        wrap_words(&note, width.saturating_sub(2).max(1) as usize).len() as u16
    } else {
        0
    };
    // Everything above this row: the hint (2), the unlock box (3), the
    // passphrase box when it is there (3), the gaps between the sections, and
    // the smallest the partition list and the status panel may be (4 and 3).
    let above = 2 + 3 + 4 + 3 + if pp_shown { 3 + 1 } else { 0 } + 4;
    let room = height.saturating_sub(above);
    (buttons + desc).min(room.max(buttons)).max(1)
}

/// How far to scroll a Paragraph so its LAST lines are the visible ones.
///
/// Wrapping is what makes this more than a subtraction: a status panel 70
/// columns wide turns one 200-character line of `grub-install` output into
/// three, and a log that looks eight lines long is twenty. Counting the wrapped
/// height is the difference between following the output and staring at its
/// beginning while the interesting part scrolls past unseen.
fn tail_scroll(lines: &[String], width: u16, height: u16, back: u16) -> u16 {
    if width == 0 || height == 0 {
        return 0;
    }
    let w = width as usize;
    let total: usize = lines
        .iter()
        .map(|l| {
            let n = l.chars().count();
            if n == 0 {
                1
            } else {
                n.div_ceil(w)
            }
        })
        .sum();
    let max = total.saturating_sub(height as usize) as u16;
    max.saturating_sub(back)
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    // THE PASSPHRASE BOX ONLY EXISTS WHEN A PASSPHRASE CAN BE TYPED. It used to
    // stand there on every unencrypted system saying "not needed for the chosen
    // method" — a box whose whole content is that it does nothing, costing four
    // rows on a screen that runs out of them. On a small console those four rows
    // came off the bottom, and the bottom is where the buttons are.
    let pp_shown = app.recovery_unlock == 1;
    let mut constraints = vec![
        Constraint::Length(2), // hint
        Constraint::Min(4),    // partition list
        Constraint::Length(3), // unlock method
    ];
    if pp_shown {
        constraints.push(Constraint::Length(3)); // passphrase
    }
    constraints.push(Constraint::Min(3)); // status / repair log
    constraints.push(Constraint::Length(action_height(
        app,
        area.width,
        area.height,
        pp_shown,
    )));
    let laid = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .spacing(1)
        .split(area);
    // Keep the old row numbering whatever is on screen, so every section below
    // still reads as rows[0..5] rather than as arithmetic.
    let rows: Vec<Rect> = if pp_shown {
        laid.to_vec()
    } else {
        vec![
            laid[0],
            laid[1],
            laid[2],
            Rect::new(laid[2].x, laid[2].y, laid[2].width, 0),
            laid[3],
            laid[4],
        ]
    };

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
        .border_style(if list_focused {
            theme::border()
        } else {
            theme::border_dim()
        })
        .title(format!(" {} ", t(app.lang, "rec.parts_head")))
        .title_style(theme::dim());
    let pinner = pblock.inner(rows[1]);
    f.render_widget(pblock, rows[1]);
    if parts.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("  —", theme::mute()))),
            pinner,
        );
    } else {
        // First sight of this list: fill in a suggested role for each partition
        // from what it looks like. Only ever done when the two are out of step,
        // so a role the user has set is never overwritten by a redraw.
        if app.recovery_roles.len() != parts.len() {
            // THE INSTALL'S OWN RECORD FIRST, guessing only as a fallback.
            //
            // The copy on the ESP is readable without unlocking anything, so
            // the roles can be filled in from what the install actually did
            // rather than from partition sizes. This is what makes "mount the
            // root and the rest is worked out for you" true instead of a
            // promise: everything below the root is already correct, and the
            // person only has to confirm it.
            let (record, loader) = recovery::read_layout_record(parts);
            app.recovery_roles = recovery::roles_from_record(parts, &record)
                .unwrap_or_else(|| recovery::suggest_all(parts));
            app.recovery_data_path = vec![String::new(); parts.len()];
            app.recovery_record_loader = loader;
            app.recovery_has_record = !record.is_empty();
        }

        // EVERY CHOICE IS ON SCREEN, next to the partition it belongs to.
        //
        // The roles used to be a single word that ←/→ cycled, which meant the
        // options existed only in the head of somebody who already knew they
        // were there. This is the same "reveal strip" the partition editor uses
        // for filesystems and wipe methods, and for the same reason: a ←/→
        // toggle must never hide what it can toggle to.
        let mut lines: Vec<Line> = Vec::new();
        // SAY WHERE THESE ANSWERS CAME FROM. Roles that were read and roles
        // that were guessed look identical on screen, and only one of them is
        // worth trusting without checking.
        lines.push(Line::from(Span::styled(
            format!(
                "  {}",
                if app.recovery_has_record {
                    t(app.lang, "rec.roles_from_record")
                        .replace("{boot}", &app.recovery_record_loader)
                } else {
                    t(app.lang, "rec.roles_guessed")
                }
            ),
            if app.recovery_has_record {
                theme::ok()
            } else {
                theme::mute()
            },
        )));
        lines.push(Line::from(""));
        for (i, p) in parts.iter().enumerate() {
            let sel = i == app.recovery_disk_cursor;
            let role = app.recovery_roles.get(i).copied().unwrap_or(0);
            // An encrypted partition cannot say what is inside it until it is
            // opened, so it says exactly that instead of showing a blank column
            // that reads as "nothing here".
            let fs = if p.fstype == "crypto_LUKS" {
                t(app.lang, "rec.fs_locked")
            } else if p.fstype.is_empty() {
                t(app.lang, "rec.fs_none")
            } else {
                p.fstype.clone()
            };
            let head = format!(
                "{} {:<14} {:>7}  {:<12}",
                if sel { ">" } else { " " },
                p.path,
                p.size,
                fs
            );
            let mut spans = vec![Span::styled(
                head.clone(),
                if sel {
                    theme::selected()
                } else {
                    theme::normal()
                },
            )];
            // Labels: what the roles are called, in this language.
            let ring = recovery::roles_for(p);
            let labels: Vec<String> = ring
                .iter()
                .map(|r| {
                    let base = t(app.lang, &format!("rec.role_{}", recovery::ROLE_KEYS[*r]));
                    if *r == recovery::ROLE_DATA {
                        format!("{base} {}", data_path_of(app, i))
                    } else {
                        base
                    }
                })
                .collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let cur = ring
                .iter()
                .position(|r| *r == role)
                .map(|k| refs[k])
                .unwrap_or(refs[0]);
            let budget = (pinner.width as usize).saturating_sub(head.chars().count() + 2);
            spans.push(Span::raw("  "));
            spans.extend(crate::screens::parts::option_strip_fit(&refs, cur, budget));
            lines.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(lines), pinner);
    }

    // 2) Unlock method (segmented pills).
    let um_focused = app.recovery_focus == 1;
    let um_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if um_focused {
            theme::border()
        } else {
            theme::border_dim()
        })
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
            let style = if sel {
                theme::selected()
            } else {
                theme::normal()
            };
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
        .border_style(if pp_focused && pp_active {
            theme::border()
        } else {
            theme::border_dim()
        })
        .title(format!(" {} ", t(app.lang, "rec.passphrase")))
        .title_style(theme::dim());
    let pp_inner = pp_block.inner(rows[3]);
    f.render_widget(pp_block, rows[3]);
    let pp_text = if !pp_active {
        Span::styled(
            format!("  {}", t(app.lang, "rec.passphrase_na")),
            theme::mute(),
        )
    } else {
        Span::styled(
            format!("  {}", "•".repeat(app.recovery_passphrase.chars().count())),
            theme::normal(),
        )
    };
    f.render_widget(Paragraph::new(Line::from(pp_text)), pp_inner);

    // 4) Status / detected bootloader — or, while a repair runs, the repair.
    //
    // A running repair OWNS this panel: its output is what the person is waiting
    // on, and the mount report underneath it is not. The VERDICT goes in the
    // title, where it cannot scroll away: the last line of a log is exactly what
    // a stream of output pushes out of sight.
    let (st_title, st_title_style) = match (app.recovery_job, &app.recovery_done) {
        (Some(i), None) => (
            t(app.lang, "rec.run_head").replace("{name}", &t(app.lang, ACTIONS[i])),
            theme::accent(),
        ),
        (Some(i), Some(Ok(()))) => (
            t(app.lang, "rec.run_head_ok").replace("{name}", &t(app.lang, ACTIONS[i])),
            theme::ok(),
        ),
        (Some(i), Some(Err(_))) => (
            t(app.lang, "rec.run_head_fail").replace("{name}", &t(app.lang, ACTIONS[i])),
            theme::warn(),
        ),
        (None, _) => (t(app.lang, "rec.status"), theme::dim()),
    };
    let st_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border_dim())
        .title(format!(" {st_title} "))
        .title_style(st_title_style);
    let st_inner = st_block.inner(rows[4]);
    f.render_widget(st_block, rows[4]);
    if app.recovery_job.is_some() {
        // Follow the tail while it runs; a scrolled-back reader keeps their
        // place. The whole output stays in the log either way.
        let scroll = tail_scroll(
            &app.recovery_log,
            st_inner.width,
            st_inner.height,
            app.recovery_scroll,
        );
        f.render_widget(
            Paragraph::new(app.recovery_log.join("\n"))
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0))
                .style(theme::normal()),
            st_inner,
        );
    } else {
        let status = if app.recovery_status.is_empty() {
            t(app.lang, "rec.status_idle")
        } else {
            app.recovery_status.clone()
        };
        f.render_widget(
            Paragraph::new(status)
                .wrap(Wrap { trim: true })
                .style(theme::normal()),
            st_inner,
        );
    }

    // 5) Action row. Before mounting there is one action; once mounted there
    //    are two — open a shell, or repair permissions. The repair is offered as
    //    a button rather than left to the shell because someone recovering from
    //    `chmod 777 /` has no working sudo to look the commands up with.
    let act_focused = app.recovery_focus == 3;
    // PACKED BY HAND, not left to Wrap. The buttons are wider than any console,
    // so `Wrap` would put them on as many lines as it liked and ratatui would
    // quietly clip whatever did not fit — which is always the LAST button. That
    // button is now the way out of the screen, and a way out you cannot see is
    // not one.
    let mut action_lines = pack_spans(action_spans(app, act_focused), rows[5].width);
    // What the highlighted action will actually do — the repair rewrites modes
    // and owners across the system, so it says so before it is pressed.
    if app.recovery_mounted {
        let note = t(
            app.lang,
            ACTION_DESCS[app.recovery_action.min(ACTIONS.len() - 1)],
        );
        for l in wrap_words(&note, rows[5].width.saturating_sub(2).max(1) as usize) {
            action_lines.push(Line::from(Span::styled(format!("  {l}"), theme::mute())));
        }
    }
    f.render_widget(Paragraph::new(action_lines), rows[5]);

    // The pickers float over everything, like every other modal in the wizard.
    if app.recovery_path_open {
        draw_path_modal(f, app);
    }
    if app.recovery_svc_open {
        draw_services_modal(f, app);
    }

    // Recovery is its own flow: never let the global "next" advance install.
    app.can_advance = false;
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    let parts = partitions();
    // A modal swallows every key while it is up, Esc included — otherwise Esc
    // would close the picker AND walk the screen back in the same press.
    if app.recovery_path_open {
        path_modal_key(app, key);
        return;
    }
    if app.recovery_svc_open {
        services_modal_key(app, key);
        return;
    }
    // Once mounted, the screen just offers the root chroot — Enter hands off,
    // Esc unmounts back to the mode chooser (handled in event.rs / main loop).
    if app.recovery_mounted {
        match key.code {
            // Two actions once mounted; ←/→ (and ↑/↓, for muscle memory) pick.
            KeyCode::Left
            | KeyCode::Char('h')
            | KeyCode::Right
            | KeyCode::Char('l')
            | KeyCode::Up
            | KeyCode::Char('k')
            | KeyCode::Down
            | KeyCode::Char('j') => {
                // A LIST now, not a pair. Recovery grew from "open a shell" to a
                // set of named repairs, and `^= 1` silently kept flipping
                // between the first two of six.
                let n = ACTIONS.len();
                let back = matches!(key.code, KeyCode::Left | KeyCode::Char('h'))
                    || matches!(key.code, KeyCode::Up | KeyCode::Char('k'));
                app.recovery_action = if back {
                    (app.recovery_action + n - 1) % n
                } else {
                    (app.recovery_action + 1) % n
                };
            }
            // A repair is running: the log is the screen, so these scroll it.
            KeyCode::PageUp if app.recovery_job.is_some() => {
                app.recovery_scroll = app.recovery_scroll.saturating_add(5);
            }
            KeyCode::PageDown if app.recovery_job.is_some() => {
                app.recovery_scroll = app.recovery_scroll.saturating_sub(5);
            }
            KeyCode::Enter => {
                // NOTHING ELSE WHILE IT RUNS. Half of these write to the target;
                // starting a second one on top of the first is how a repair
                // turns into damage. Enter still answers, because an Enter that
                // does nothing and says nothing reads as a hung program.
                if app.recovery_rx.is_some() {
                    app.recovery_log.push(t(app.lang, "rec.run_busy"));
                    return;
                }
                // The way out, and the reason this screen can be finished with:
                // unmount cleanly and boot the system that was just repaired.
                // The deed itself belongs to main.rs, after the TUI is down —
                // so this stays a flag, and no test can reboot the machine it
                // is running on.
                if app.recovery_action == ACT_REBOOT {
                    app.pending_reboot = true;
                    app.should_quit = true;
                    return;
                }
                // Diagnosis is the one action that changes nothing, so it runs
                // HERE and prints into the status pane — no terminal handover,
                // nothing to undo, and you can read it and then pick a repair.
                if app.recovery_action == 0 {
                    // Back to the report: a finished repair's log has served its
                    // purpose once you ask the question again.
                    app.recovery_job = None;
                    app.recovery_done = None;
                    app.recovery_log.clear();
                    app.recovery_scroll = 0;
                    // Diagnosis also MOVES THE CURSOR to the repair it
                    // recommends, so the next key is Enter. Telling somebody
                    // what is broken and leaving them to pick which of six
                    // buttons addresses it is half an answer, and the person who
                    // needs this screen is the one who does not know.
                    let (report, suggested) = crate::system::recovery::diagnose(app.lang);
                    app.recovery_status = report;
                    if suggested != 0 {
                        app.recovery_action = suggested.min(ACTIONS.len() - 1);
                    }
                    return;
                }
                // Services open a picker: WHICH ones is the whole question, and
                // it is answered against the list on the mounted system.
                if app.recovery_action == ACT_SERVICES {
                    app.recovery_svc = crate::system::recovery::boot_services();
                    app.recovery_svc_marked = vec![false; app.recovery_svc.len()];
                    app.recovery_svc_cursor = 0;
                    if app.recovery_svc.is_empty() {
                        // Say WHERE it looked. An empty box on a system that
                        // does have services means the wrong thing is mounted,
                        // and the directory name is what makes that visible.
                        app.recovery_status = t(app.lang, "rec.svc_none")
                            .replace("{dir}", crate::system::recovery::BOOT_D);
                        return;
                    }
                    app.recovery_svc_open = true;
                    return;
                }
                // The shell is a HANDOVER and stays one: it is interactive, and
                // a panel that streams output cannot answer a prompt.
                if app.recovery_action == ACT_SHELL {
                    app.pending_interactive = Some(("artix-chroot".into(), vec!["/mnt".into()]));
                    return;
                }
                // The repairs run here, in the panel, and end with a verdict.
                start_repair(app, app.recovery_action);
            }
            // Esc while a repair is mid-write would leave the target half
            // repaired and the screen gone. Everything else it does is still
            // available the moment the run ends.
            KeyCode::Esc if app.recovery_rx.is_some() => {
                app.recovery_log.push(t(app.lang, "rec.run_busy"));
            }
            KeyCode::Esc => app.goto(crate::app::Screen::Mode),
            _ => {}
        }
        return;
    }
    match key.code {
        // ↑/↓ (k/j): move the selection WITHIN the focused field — the disk
        // list on the partition row, the method on the unlock row. This matches
        // how list/option screens elsewhere in the installer behave.
        // ↑/↓ WALK THE WHOLE SCREEN, not just the field you happen to be in.
        //
        // They used to move only inside the focused field, so the only way to
        // reach the next section was Enter — which is not how anything else in
        // this installer behaves, and the person driving it said so. Inside the
        // partition list they still step through partitions; at its edge they
        // cross into the next section. The unlock row is a pill strip, so ←/→
        // pick the method there and ↑/↓ are free to keep walking.
        KeyCode::Up | KeyCode::Char('k') => match app.recovery_focus {
            0 => {
                if app.recovery_disk_cursor == 0 {
                    // Already at the top of the first section: nowhere above.
                } else {
                    app.recovery_disk_cursor -= 1;
                }
            }
            1 => {
                app.recovery_focus = 0;
                app.recovery_disk_cursor = parts.len().saturating_sub(1);
            }
            2 => app.recovery_focus = 1,
            _ => app.recovery_focus = if app.recovery_unlock == 1 { 2 } else { 1 },
        },
        KeyCode::Down | KeyCode::Char('j') => match app.recovery_focus {
            0 => {
                if parts.is_empty() || app.recovery_disk_cursor + 1 >= parts.len() {
                    app.recovery_focus = 1;
                } else {
                    app.recovery_disk_cursor += 1;
                }
            }
            // The passphrase field is skipped unless it can be typed into.
            1 => app.recovery_focus = if app.recovery_unlock == 1 { 2 } else { 3 },
            2 => app.recovery_focus = 3,
            _ => {}
        },
        // ←/→ (h/l): on the partition list this SETS WHAT THE PARTITION IS —
        // the screen's main verb now, not a decoration. On the unlock row it
        // switches the method, for muscle memory.
        KeyCode::Left | KeyCode::Char('h') => match app.recovery_focus {
            0 => cycle_role(app, parts, false),
            1 => app.recovery_unlock = app.recovery_unlock.saturating_sub(1),
            _ => {}
        },
        KeyCode::Right | KeyCode::Char('l') => match app.recovery_focus {
            0 => cycle_role(app, parts, true),
            1 => app.recovery_unlock = (app.recovery_unlock + 1).min(2),
            _ => {}
        },
        // `m`: WHERE a "data" partition gets mounted. Opens the picker rather
        // than cycling a fixed ring — the presets are the paths the installer
        // itself offers (recognisable months later), but somebody who chose
        // their own folder has to be able to type it.
        KeyCode::Char('m') if app.recovery_focus == 0 => {
            let i = app.recovery_disk_cursor;
            // A key that does nothing must say why. `m` only means anything on
            // a partition already marked as data, and pressing it anywhere else
            // used to be silently ignored — which reads exactly like "the
            // custom path option does not exist".
            if app.recovery_roles.get(i).copied() != Some(recovery::ROLE_DATA) {
                app.recovery_status = t(app.lang, "rec.err_m_needs_data");
                return;
            }
            {
                let cur = data_path_of(app, i);
                app.recovery_path_cursor = recovery::DATA_PATHS
                    .iter()
                    .position(|p| *p == cur)
                    .unwrap_or(recovery::DATA_PATHS.len());
                app.recovery_path_input = if app.recovery_path_cursor == recovery::DATA_PATHS.len()
                {
                    cur
                } else {
                    String::new()
                };
                app.recovery_path_open = true;
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
            0 => app.goto(crate::app::Screen::Mode),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A person who mounted a disk at a folder of their own must be able to say
    /// so. The presets cover the paths the installer offers, but a ring of four
    /// fixed choices tells everyone else their case is unsupported — which was
    /// the complaint that produced this picker.
    #[test]
    fn a_data_partition_can_be_given_any_absolute_path() {
        let mut app = App::new();
        app.recovery_focus = 0;
        app.recovery_disk_cursor = 0;
        app.recovery_roles = vec![crate::system::recovery::ROLE_DATA];
        app.recovery_data_path = vec![String::new()];
        handle_key(&mut app, key(KeyCode::Char('m')));
        assert!(app.recovery_path_open, "`m` opens the picker");

        // Walk past the presets to the free-text row and type a path.
        for _ in 0..crate::system::recovery::DATA_PATHS.len() {
            handle_key(&mut app, key(KeyCode::Down));
        }
        for c in "/mnt/photos".chars() {
            handle_key(&mut app, key(KeyCode::Char(c)));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(!app.recovery_path_open);
        assert_eq!(app.recovery_data_path[0], "/mnt/photos");
    }

    /// A data disk can live under a home directory, and often does: the
    /// installer's own "extra disks" step offers mounting one into the user's
    /// home. A picker that only accepted /mnt-style paths would refuse the
    /// layout this very installer produces.
    #[test]
    fn a_data_disk_under_a_home_directory_is_accepted() {
        let mut app = App::new();
        app.recovery_focus = 0;
        app.recovery_roles = vec![crate::system::recovery::ROLE_DATA];
        app.recovery_data_path = vec![String::new()];
        handle_key(&mut app, key(KeyCode::Char('m')));
        for _ in 0..crate::system::recovery::DATA_PATHS.len() {
            handle_key(&mut app, key(KeyCode::Down));
        }
        for c in "/home/user/photos".chars() {
            handle_key(&mut app, key(KeyCode::Char(c)));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.recovery_data_path[0], "/home/user/photos");
        // Nested paths must mount AFTER their parent, or /home would shadow it.
        let deep = crate::system::recovery::mount_point(
            crate::system::recovery::ROLE_DATA,
            "/home/user/photos",
        )
        .unwrap();
        let home =
            crate::system::recovery::mount_point(crate::system::recovery::ROLE_HOME, "").unwrap();
        assert!(home.matches('/').count() < deep.matches('/').count());
    }

    /// ↑/↓ must cross between the screen's sections, like everywhere else in
    /// the wizard. Requiring Enter to leave a field made this screen behave
    /// unlike the launcher it is reached from.
    #[test]
    fn up_and_down_walk_the_whole_screen_without_enter() {
        let mut app = App::new();
        app.screen = crate::app::Screen::Recovery;
        app.recovery_focus = 0;
        app.recovery_disk_cursor = 0;
        // Walk down past the end of the partition list into the next section.
        for _ in 0..12 {
            handle_key(&mut app, key(KeyCode::Down));
        }
        assert!(
            app.recovery_focus > 0,
            "↓ leaves the partition list on its own"
        );
        // And back up again.
        for _ in 0..12 {
            handle_key(&mut app, key(KeyCode::Up));
        }
        assert_eq!(app.recovery_focus, 0, "↑ returns to the partition list");
    }

    /// The picker is a PATH FIELD that happens to offer four shortcuts, not a
    /// menu of four paths. Typing anywhere in it starts entering a path, so the
    /// general case never hides behind an arrow key.
    #[test]
    fn typing_anywhere_in_the_picker_enters_a_path() {
        let mut app = App::new();
        app.recovery_focus = 0;
        app.recovery_roles = vec![crate::system::recovery::ROLE_DATA];
        app.recovery_data_path = vec![String::new()];
        handle_key(&mut app, key(KeyCode::Char('m')));
        // Cursor is on the first PRESET, not the text field.
        assert_eq!(app.recovery_path_cursor, 0);
        for c in "/srv/backups".chars() {
            handle_key(&mut app, key(KeyCode::Char(c)));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.recovery_data_path[0], "/srv/backups");
    }

    /// `m` on a partition that is not marked as data must SAY it does nothing,
    /// not simply do nothing — silence there reads as a missing feature.
    #[test]
    fn pressing_m_on_a_non_data_partition_explains_itself() {
        let mut app = App::new();
        app.recovery_focus = 0;
        app.recovery_roles = vec![crate::system::recovery::ROLE_ROOT];
        handle_key(&mut app, key(KeyCode::Char('m')));
        assert!(!app.recovery_path_open);
        assert!(
            !app.recovery_status.is_empty(),
            "it explains why nothing opened"
        );
    }

    /// A relative path would be joined onto /mnt and land somewhere nobody
    /// asked for, so it is refused at the picker rather than at mount time.
    #[test]
    fn a_relative_mount_path_is_refused_with_a_reason() {
        let mut app = App::new();
        app.recovery_focus = 0;
        app.recovery_roles = vec![crate::system::recovery::ROLE_DATA];
        app.recovery_data_path = vec![String::new()];
        handle_key(&mut app, key(KeyCode::Char('m')));
        for _ in 0..crate::system::recovery::DATA_PATHS.len() {
            handle_key(&mut app, key(KeyCode::Down));
        }
        for c in "photos".chars() {
            handle_key(&mut app, key(KeyCode::Char(c)));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            app.recovery_path_open,
            "the picker stays open on a bad path"
        );
        assert!(
            !app.recovery_status.is_empty(),
            "Enter never refuses silently"
        );
        assert_eq!(app.recovery_data_path[0], "");
    }

    /// Esc belongs to the modal while it is up. Letting it through would close
    /// the picker and walk the screen back in one press.
    #[test]
    fn esc_closes_the_picker_without_leaving_the_screen() {
        let mut app = App::new();
        // Assigning the field is fine here: this is a test, and the point is to
        // start ON the screen so a stray navigation would be visible.
        app.screen = crate::app::Screen::Recovery;
        app.recovery_focus = 0;
        app.recovery_roles = vec![crate::system::recovery::ROLE_DATA];
        app.recovery_data_path = vec![String::new()];
        handle_key(&mut app, key(KeyCode::Char('m')));
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(!app.recovery_path_open);
        assert_eq!(app.screen, crate::app::Screen::Recovery);
    }

    /// Once the system is mounted the screen offers TWO actions, and Enter on
    /// the second one hands the permission repair to the chroot. Getting back
    /// from `chmod 777 /` must not require knowing the commands — that is
    /// precisely the state in which sudo no longer works to look them up.
    /// Every repair the screen offers reaches the right script, and the one
    /// that only LOOKS changes nothing.
    ///
    /// Recovery used to be two buttons flipped with `^= 1`. It is a list of six
    /// now, and the same flip would have quietly toggled between the first two
    /// of them forever.
    #[test]
    fn every_recovery_action_runs_what_it_says() {
        let mut app = App::new();
        app.recovery_mounted = true;
        app.recovery_focus = 3;
        assert_eq!(
            app.recovery_action, 0,
            "the first action must be the one that changes nothing"
        );

        // Diagnosis reports and does NOT hand over the terminal: it is
        // read-only, and you are meant to read it and then choose a repair.
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            app.pending_interactive.is_none(),
            "diagnosis took over the terminal"
        );
        assert!(
            !app.recovery_status.is_empty(),
            "diagnosis reported nothing"
        );

        // The repairs, in order, each with its own script. Checked through
        // `repair_command` and NOT by pressing Enter: Enter now starts the
        // thing, and a test that chroots into /mnt on the machine running the
        // suite is a test that repairs the developer's laptop.
        for (i, needle) in [
            (1, "grub-install"),
            (2, "mkinitcpio -P"),
            (3, "/etc/fstab"),
            (4, "chmod"),
        ] {
            // The cursor still has to REACH each one.
            let mut a = App::new();
            a.recovery_mounted = true;
            a.recovery_focus = 3;
            for _ in 0..i {
                handle_key(&mut a, key(KeyCode::Right));
            }
            assert_eq!(a.recovery_action, i);

            let (prog, args) =
                chroot_argv(repair_script(i).expect("no script for a repair button"));
            assert_eq!(prog, "artix-chroot");
            assert_eq!(args[0], "/mnt");
            assert!(
                args.last().is_some_and(|s| s.contains(needle)),
                "action {i} does not run {needle}"
            );
        }

        // The shell is still a handover — it is interactive, and a streamed
        // panel has no way to answer a prompt.
        let mut a = App::new();
        a.recovery_mounted = true;
        a.recovery_focus = 3;
        a.recovery_action = ACT_SHELL;
        handle_key(&mut a, key(KeyCode::Enter));
        let (prog, args) = a.pending_interactive.take().expect("no action");
        assert_eq!(prog, "artix-chroot");
        assert_eq!(
            args,
            vec!["/mnt".to_string()],
            "the shell action is not a plain shell"
        );
        assert!(
            repair_script(ACT_SHELL).is_none()
                && repair_script(ACT_REBOOT).is_none()
                && repair_script(ACT_SERVICES).is_none(),
            "the shell, the picker and the way out are not repairs to be streamed"
        );
    }

    /// The way out reboots through main.rs, never from here.
    ///
    /// It is a FLAG on purpose: the alternative is calling `reboot` from a key
    /// handler, and this suite presses Enter on every button on a real machine.
    /// The unmount rides along in main.rs, so a repaired filesystem is not left
    /// dirty on its way to its first boot.
    #[test]
    fn the_way_out_is_a_flag_and_not_a_reboot_from_a_key_handler() {
        let mut app = App::new();
        app.recovery_mounted = true;
        app.recovery_focus = 3;
        app.recovery_action = ACT_REBOOT;
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            app.pending_reboot,
            "the reboot button did not ask to reboot"
        );
        assert!(app.should_quit, "it must also end the TUI");
        assert!(
            app.pending_interactive.is_none() && app.recovery_rx.is_none(),
            "the way out must not run anything itself"
        );
        let main = include_str!("../main.rs");
        assert!(
            main.contains("recovery::cleanup();"),
            "main.rs must unmount before rebooting, or recovery leaves the \
             system it just repaired mounted and its LUKS mapping open"
        );
    }

    /// A second repair started on top of a running one is how a repair becomes
    /// damage — they write to the same target. Refused, and it says so.
    #[test]
    fn nothing_else_starts_while_a_repair_is_running() {
        let (_tx, rx) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.recovery_mounted = true;
        app.recovery_focus = 3;
        app.recovery_job = Some(1);
        app.recovery_rx = Some(rx);
        app.screen = crate::app::Screen::Recovery;

        app.recovery_action = 3;
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            app.recovery_rx.is_some() && app.pending_interactive.is_none(),
            "a second repair started while the first was running"
        );
        assert!(
            !app.recovery_log.is_empty(),
            "Enter refused silently, which reads as a hung program"
        );

        // Nor does Esc walk off the screen mid-write.
        handle_key(&mut app, key(KeyCode::Esc));
        assert_eq!(app.screen, crate::app::Screen::Recovery);

        // And the way out cannot reboot out from under it either.
        app.recovery_action = ACT_REBOOT;
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            !app.pending_reboot,
            "the machine rebooted with a repair still writing to the target"
        );
    }

    /// THE LAST BUTTON IS THE WAY OUT, so it has to be on screen.
    ///
    /// Seven buttons do not fit on one line at any console width this runs at,
    /// and a fixed-height row plus `Wrap` clips the overflow without a word.
    /// What gets clipped is the end of the strip — which is where "reboot into
    /// the system" lives. Rendered here at the widths that matter and read back
    /// out of the buffer, in every language, because the Ukrainian labels are
    /// the long ones and English is the one I would have checked.
    #[test]
    fn the_reboot_button_is_visible_at_every_width() {
        use ratatui::{backend::TestBackend, Terminal};
        for lang in [
            crate::i18n::Lang::Uk,
            crate::i18n::Lang::En,
            crate::i18n::Lang::Es,
        ] {
            for (w, h) in [(80u16, 24u16), (100, 30), (120, 40), (60, 24)] {
                let mut app = App::new();
                app.lang = lang;
                app.recovery_mounted = true;
                app.recovery_focus = 3;
                let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
                term.draw(|f| draw(f, &mut app, f.area())).unwrap();
                let text: String = term
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                // The label may itself be wrapped across the strip's own lines,
                // so look for its first word rather than the whole phrase.
                let label = t(lang, "rec.act_reboot");
                let head = label.split_whitespace().next().unwrap();
                assert!(
                    text.contains(head),
                    "{lang:?} at {w}x{h}: the way out is not on screen — \
                     the action strip was clipped"
                );
            }
        }
    }

    /// The log follows its own tail, counting WRAPPED lines.
    ///
    /// A panel 20 columns wide turns one line of `grub-install` output into
    /// three, so a plain "lines minus height" leaves the reader staring at the
    /// beginning while the part they are waiting for scrolls past.
    #[test]
    fn a_long_line_counts_as_the_rows_it_actually_takes() {
        let short: Vec<String> = (0..4).map(|i| format!("line {i}")).collect();
        assert_eq!(
            tail_scroll(&short, 20, 10, 0),
            0,
            "output that fits must not scroll at all"
        );
        let wide = vec!["x".repeat(60), "y".repeat(60)];
        // Six wrapped rows in a panel three tall: the last three are the ones
        // worth showing.
        assert_eq!(tail_scroll(&wide, 20, 3, 0), 3);
        assert_eq!(
            tail_scroll(&wide, 20, 3, 2),
            1,
            "scrolling back must move away from the tail, not past the start"
        );
        assert_eq!(tail_scroll(&wide, 20, 3, 99), 0);
    }

    /// The script is shell shipped as an asset, so a syntax slip would only
    /// surface on the live ISO — where the user is already in trouble.
    #[test]
    fn the_repair_script_is_valid_posix_sh() {
        let out = std::process::Command::new("sh")
            .args(["-n", "-c", crate::system::install::FIX_PERMISSIONS])
            .output()
            .expect("run sh -n");
        assert!(
            out.status.success(),
            "the permission-repair script is not valid sh:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
