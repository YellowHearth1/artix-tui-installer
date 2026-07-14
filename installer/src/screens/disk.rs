//! Step 7 — disk & partitions. Three focus areas, top to bottom:
//!   1) boot mode segmented toggle: BIOS (VM testing) | UEFI (recommended),
//!   2) disk selection list (lsblk),
//!   3) swap: enabled toggle + GiB amount (default 4).
//! The actual partition plan is built later by system::disk::build_plan.

use crate::app::{App, BootMode};
use crate::i18n::t;
use crate::screens::widgets;
use crate::system::disk::{self, Disk};
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};
use std::sync::OnceLock;

pub(crate) fn disks() -> &'static Vec<Disk> {
    static D: OnceLock<Vec<Disk>> = OnceLock::new();
    D.get_or_init(|| disk::list().unwrap_or_default())
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    // The per-filesystem options row only takes space when the selected
    // filesystem actually has features (btrfs/f2fs); otherwise it's zero-height.
    let fs_opts = fs_features(&app.config.root_fs);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // boot mode (two bordered cards)
            Constraint::Min(4),    // disk list (bordered box)
            Constraint::Length(3), // swap (bordered box)
            Constraint::Length(3), // filesystem (bordered box)
            Constraint::Length(2), // filesystem description (what it's good for + [o])
            Constraint::Length(1), // filesystem-options summary (one line)
            Constraint::Length(1), // warning
            Constraint::Length(3), // actions
        ])
        .split(area);

    // 1) Boot mode — two big side-by-side bordered CARDS (UEFI | BIOS), each
    //    with its own title and a short description, so the choice reads like a
    //    proper either/or instead of two small pills. The selected card gets
    //    the accent border + a ● mark; focus brightens everything.
    let bm_is_bios = app.config.boot_mode == BootMode::Bios;
    let bm_focused = app.disk_focus == 0;
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .spacing(1)
        .split(rows[0]);
    boot_mode_card(
        f,
        halves[0],
        "UEFI",
        &t(app.lang, "disk.uefi_desc"),
        !bm_is_bios,
        bm_focused,
    );
    boot_mode_card(
        f,
        halves[1],
        "BIOS",
        &t(app.lang, "disk.bios_desc"),
        bm_is_bios,
        bm_focused,
    );

    // 2) Disk list.
    let d = disks_selectable(app);
    let disk_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if app.disk_focus == 1 {
            theme::border()
        } else {
            theme::border_dim()
        })
        .title(" Disk ")
        .title_style(theme::dim());
    let inner = disk_block.inner(rows[1]);
    f.render_widget(disk_block, rows[1]);
    if d.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  no disks detected",
                theme::mute(),
            ))),
            inner,
        );
        app.can_advance = false;
    } else {
        // Each row: path · size · model, with an inline flag when the disk is
        // the live boot medium or looks too small. The flags are advisory — the
        // user can still select any disk; the detailed warning appears below.
        let items: Vec<String> = d
            .iter()
            .map(|x| {
                let mut row = format!("  {}   {}   {}", x.path, x.size, x.model);
                if x.is_live {
                    row.push_str(&format!("   ⚠ {}", t(app.lang, "disk.flag_live")));
                }
                if is_too_small(x) {
                    row.push_str(&format!("   ⚠ {}", t(app.lang, "disk.flag_small")));
                }
                row
            })
            .collect();
        widgets::select_list(f, inner, &items, app.disk_cursor);
        app.config.disk = d[app.disk_cursor.min(d.len() - 1)].path.clone();
        app.can_advance = true;
    }

    // 3) Swap — bordered, titled box with the ON/OFF pill + size stepper.
    let on = app.config.swap_gib > 0;
    let sw_focused = app.disk_focus == 2;
    let sw_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if sw_focused {
            theme::border()
        } else {
            theme::border_dim()
        })
        .title(format!(" {} ", t(app.lang, "disk.swap_q")))
        .title_style(if sw_focused {
            theme::title()
        } else {
            theme::dim()
        });
    let sw_inner = sw_block.inner(rows[2]);
    f.render_widget(sw_block, rows[2]);
    let mut sw_spans: Vec<Span> = vec![
        Span::raw(" "),
        pill(if on { "ON" } else { "OFF" }, on, sw_focused),
    ];
    if on {
        let arrow_style = if sw_focused {
            theme::gold()
        } else {
            theme::mute()
        };
        sw_spans.push(Span::raw("   "));
        sw_spans.push(Span::styled("‹ ", arrow_style));
        sw_spans.push(Span::styled(
            format!("{} GiB", app.config.swap_gib),
            if sw_focused {
                theme::normal()
            } else {
                theme::dim()
            },
        ));
        sw_spans.push(Span::styled(" ›", arrow_style));
    }
    f.render_widget(Paragraph::new(Line::from(sw_spans)), sw_inner);

    // 4) Filesystem — bordered, titled box; the title carries the full label so
    //    it never gets cut off, and the options use the whole width.
    let fs_focused = app.disk_focus == 3;
    let fs_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if fs_focused {
            theme::border()
        } else {
            theme::border_dim()
        })
        .title(format!(" {} ", t(app.lang, "disk.fs")))
        .title_style(if fs_focused {
            theme::title()
        } else {
            theme::dim()
        });
    let fs_inner = fs_block.inner(rows[3]);
    f.render_widget(fs_block, rows[3]);
    let fs_idx = FS_LIST
        .iter()
        .position(|(id, _)| *id == app.config.root_fs)
        .unwrap_or(0);
    // The selected filesystem is highlighted in ‹ › arrows; the others dimmed.
    let mut fs_spans: Vec<Span> = vec![Span::raw(" ")];
    for (i, (_, label)) in FS_LIST.iter().enumerate() {
        if i == fs_idx {
            let st = if fs_focused {
                theme::gold()
            } else {
                theme::normal()
            };
            fs_spans.push(Span::styled(format!("‹ {label} ›"), st));
        } else {
            fs_spans.push(Span::styled(format!(" {label} "), theme::mute()));
        }
    }
    f.render_widget(Paragraph::new(Line::from(fs_spans)), fs_inner);

    // 4a) Filesystem description: what the SELECTED filesystem is good for and,
    //     crucially, what pressing [o] would unlock for it — so a user who needs
    //     snapshots/compression on btrfs doesn't walk past without noticing.
    use ratatui::widgets::Wrap;
    let has_opts = !fs_opts.is_empty();
    let fschar = t(app.lang, &format!("disk.fschar_{}", app.config.root_fs));
    let mut char_spans = vec![Span::styled(format!("  {fschar} "), theme::normal())];
    if has_opts {
        // The call-to-action (bold accent) only when there's actually something
        // to configure; filesystems without options don't advertise [o].
        char_spans.push(Span::styled(t(app.lang, "disk.fsopt_cta"), theme::gold()));
    }
    f.render_widget(
        Paragraph::new(Line::from(char_spans)).wrap(Wrap { trim: true }),
        rows[4],
    );

    // 4b) Filesystem-options summary (one line): the enabled options at a glance
    //     plus a hint that `o` opens the full picker. Shown only when the
    //     filesystem actually has options (btrfs); otherwise the row stays blank.
    if has_opts {
        let enabled: Vec<String> = fs_opts
            .iter()
            .filter(|(id, _)| feature_on(app, &app.config.root_fs, id))
            .map(|(id, _)| t(app.lang, &format!("disk.fsopt_{id}")))
            .collect();
        let summary_val = if enabled.is_empty() {
            t(app.lang, "disk.fsopt_none")
        } else {
            enabled.join(", ")
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("  {}: ", t(app.lang, "disk.fsopt_summary")),
                    theme::dim(),
                ),
                Span::styled(summary_val, theme::normal()),
                Span::styled(
                    format!("   ·   {}", t(app.lang, "disk.fsopt_open_hint")),
                    theme::accent(),
                ),
            ])),
            rows[5],
        );
    }

    // 5) Warning line. Always shows the "everything on this disk is erased"
    //    reminder; when the selected disk trips any pre-flight check (live
    //    medium, too small, UEFI/BIOS mismatch) it also shows how many advisories
    //    apply and how to open the detailed modal.
    let warns = preflight_warnings(app);
    let warn_line = if warns.is_empty() {
        Line::from(Span::styled(
            format!("  {}: {}", t(app.lang, "disk.warn"), app.config.disk),
            theme::warn(),
        ))
    } else {
        Line::from(vec![
            Span::styled(
                format!("  ⚠ {}: {}   ", t(app.lang, "disk.warn"), app.config.disk),
                theme::warn(),
            ),
            Span::styled(
                format!(
                    "{} {} · {}",
                    warns.len(),
                    t(app.lang, "disk.warn_count"),
                    t(app.lang, "disk.warn_open_hint")
                ),
                theme::accent(),
            ),
        ])
    };
    f.render_widget(Paragraph::new(warn_line), rows[6]);

    widgets::action_row(
        f,
        rows[7],
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        app.can_advance,
    );

    // The filesystem-options modal renders on top of everything when open.
    if app.fs_opts_modal_open {
        draw_fs_opts_modal(f, app, area);
    }
    // The pre-flight warnings modal renders on top when open.
    if app.disk_warn_modal_open {
        draw_warn_modal(f, app, area);
    }
}

/// Bytes below which a full desktop install is likely to run out of space.
/// 20 GiB — enough headroom that the warning is meaningful without nagging on
/// reasonable disks. A disk reporting 0 bytes (lsblk gave nothing) is treated
/// as unknown and never flagged.
const MIN_DISK_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// True when a disk is smaller than the recommended minimum (and its size is
/// actually known).
fn is_too_small(d: &Disk) -> bool {
    d.size_bytes > 0 && d.size_bytes < MIN_DISK_BYTES
}

/// Collect the advisory pre-flight warnings for the currently selected disk and
/// boot-mode choice. Each entry is a fully-formed, localized sentence. Order:
/// most destructive first (live medium), then space, then firmware mismatch.
/// None of these block installation — they inform the user before they commit.
/// The disks the user may actually install onto.
///
/// Excludes the stick chosen as the LUKS USB key. Two reasons, and the second
/// is the dangerous one:
///
///   * it's contradictory — you can't hold the unlock key on the very disk the
///     key is supposed to unlock;
///   * `app.config.disk` is assigned from the cursor position on EVERY draw
///     pass, so merely scrolling PAST the stick would silently make it the
///     install target. The install would then wipe the key stick — and with a
///     key-only setup, that's an unbootable system.
///
/// The options screen already refuses to offer the install disk as a key
/// (`d.removable && d.path != app.config.disk`); this is the same guard from
/// the other side, for when the user walks Back into the disk screen after
/// picking a key.
fn disks_selectable(app: &App) -> Vec<Disk> {
    disks()
        .iter()
        .filter(|x| app.config.usb_key_device.is_empty() || x.path != app.config.usb_key_device)
        .cloned()
        .collect()
}

fn preflight_warnings(app: &App) -> Vec<String> {
    let mut out = Vec::new();
    let d = disks_selectable(app);
    if let Some(sel) = d.get(app.disk_cursor.min(d.len().saturating_sub(1))) {
        if sel.is_live {
            out.push(t(app.lang, "disk.warnmodal_live"));
        }
        if is_too_small(sel) {
            out.push(t(app.lang, "disk.warnmodal_small"));
        }
    }
    // UEFI/BIOS vs actual firmware. /sys/firmware/efi exists only when booted in
    // UEFI mode; a mismatch produces an unbootable system, so warn either way.
    let firmware_is_uefi = std::path::Path::new("/sys/firmware/efi").exists();
    let chose_uefi = app.config.boot_mode.is_uefi();
    if firmware_is_uefi && !chose_uefi {
        out.push(t(app.lang, "disk.warnmodal_bios_on_uefi"));
    } else if !firmware_is_uefi && chose_uefi {
        out.push(t(app.lang, "disk.warnmodal_uefi_on_bios"));
    }
    out
}

/// Rows `text` needs when word-wrapped to `width` columns. Mirrors ratatui's
/// Wrap{trim:true} closely enough to size the scrollable description area (an
/// off-by-one only costs one line of slack at the very bottom).
fn wrapped_height(text: &str, width: u16) -> u16 {
    let width = width.max(1) as usize;
    let mut lines: u16 = 0;
    for para in text.split('\n') {
        let mut col = 0usize;
        for word in para.split_whitespace() {
            let wlen = word.chars().count();
            if col == 0 {
                col = wlen;
            } else if col + 1 + wlen <= width {
                col += 1 + wlen;
            } else {
                lines = lines.saturating_add(1);
                col = wlen;
            }
        }
        lines = lines.saturating_add(1); // last (or only/empty) line of the paragraph
    }
    lines.max(1)
}

/// Centered modal listing the selected filesystem's options as a checklist, with
/// a full description of the option under the cursor — what it gains and what it
/// costs — so the choice is informed rather than a cryptic flag. The modal grows
/// to fit the description on roomy screens; on short ones the description scrolls
/// with w/s, so it works across screen sizes without ever truncating silently.
fn draw_fs_opts_modal(f: &mut Frame, app: &mut App, area: Rect) {
    use ratatui::widgets::{Clear, Wrap};

    let fs = app.config.root_fs.clone();
    let opts = fs_features(&fs);
    if opts.is_empty() {
        return;
    }
    let cursor = app.fs_opt_cursor.min(opts.len() - 1);
    let (cur_id, _) = opts[cursor];
    let desc = t(app.lang, &format!("disk.fsdesc_{cur_id}"));

    let w = 72u16.min(area.width.saturating_sub(4));
    let inner_w = w.saturating_sub(2); // inside the borders
    let desc_full = wrapped_height(&desc, inner_w);
    // Chrome around the description: 2 border rows + the checklist + 1 spacer
    // + 1 hint row. Grow the box to show the whole description, capped by the
    // screen; never collapse the description below two visible rows.
    let chrome = opts.len() as u16 + 4;
    let max_h = area.height.saturating_sub(2);
    let h = (chrome + desc_full).min(max_h).max((chrome + 2).min(max_h));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let modal = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border())
        .title(format!(
            " {} — {} ",
            t(app.lang, "disk.fsopt_modal_title"),
            fs
        ))
        .title_style(theme::title());
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    // Split inner into: checklist, spacer, description (fills), hint.
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(opts.len() as u16),
            Constraint::Length(1), // spacer
            Constraint::Min(1),    // description (takes the remaining height)
            Constraint::Length(1), // hint
        ])
        .split(inner);

    // Checklist.
    let mut lines: Vec<Line> = Vec::new();
    for (i, (id, _)) in opts.iter().enumerate() {
        let on = feature_on(app, &fs, id);
        let mark = if on { "[x]" } else { "[ ]" };
        let is_cur = i == cursor;
        let st = if is_cur {
            theme::gold()
        } else if on {
            theme::normal()
        } else {
            theme::mute()
        };
        let prefix = if is_cur { "›" } else { " " };
        let label = t(app.lang, &format!("disk.fsopt_{id}"));
        lines.push(Line::from(Span::styled(
            format!(" {prefix} {mark} {label}"),
            st,
        )));
    }
    f.render_widget(Paragraph::new(lines), parts[0]);

    // Description of the cursored option (wrapped + scrollable). Clamp the scroll
    // offset to the real bottom so w/s can't run past the end.
    let desc_area_h = parts[2].height;
    let max_scroll = desc_full.saturating_sub(desc_area_h);
    let scroll = app.fs_opt_desc_scroll.min(max_scroll);
    app.fs_opt_desc_scroll = scroll; // clamp the stored offset so w/s never go dead
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(desc, theme::dim())))
            .wrap(Wrap { trim: true })
            .scroll((scroll, 0)),
        parts[2],
    );

    // Hint — prefixed with an arrow when there's more text off-screen, so the
    // scroll affordance is discoverable only when it's actually needed.
    let base = t(app.lang, "disk.fsopt_modal_hint");
    let hint = if max_scroll > 0 {
        let arrows = if scroll == 0 {
            "▼"
        } else if scroll >= max_scroll {
            "▲"
        } else {
            "▲▼"
        };
        format!("{arrows}  {base}")
    } else {
        base
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, theme::mute()))),
        parts[3],
    );
}

/// The pre-flight warnings modal: a centered bordered box listing every
/// advisory for the selected disk (live medium, too small, UEFI/BIOS mismatch),
/// each as a full wrapped sentence, with a scrollable body when the list is
/// taller than the box. Modeled on the filesystem-options modal so the two look
/// like one product. Purely informational — Enter/Esc both close it and let the
/// user proceed; nothing here blocks the install.
fn draw_warn_modal(f: &mut Frame, app: &mut App, area: Rect) {
    use ratatui::widgets::{Clear, Wrap};

    let warns = preflight_warnings(app);
    if warns.is_empty() {
        return;
    }

    // Build the body: each warning as a "• sentence" block, blank line between.
    let mut body: Vec<Line> = Vec::new();
    for (i, w) in warns.iter().enumerate() {
        if i > 0 {
            body.push(Line::from(""));
        }
        body.push(Line::from(Span::styled(format!("• {w}"), theme::warn())));
    }

    let wbox = 76u16.min(area.width.saturating_sub(4));
    let inner_w = wbox.saturating_sub(2);
    // Height the wrapped body needs (each warning may wrap; blanks add 1 each).
    let mut body_h: u16 = 0;
    for (i, w) in warns.iter().enumerate() {
        if i > 0 {
            body_h = body_h.saturating_add(1); // blank spacer
        }
        body_h = body_h.saturating_add(wrapped_height(&format!("• {w}"), inner_w));
    }
    // Chrome: 2 borders + 1 title-gap + 1 spacer + 1 hint.
    let chrome = 5u16;
    let max_h = area.height.saturating_sub(2);
    let h = (chrome + body_h).min(max_h).max((chrome + 2).min(max_h));
    let x = area.x + (area.width.saturating_sub(wbox)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let modal = Rect {
        x,
        y,
        width: wbox,
        height: h,
    };
    f.render_widget(Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::warn())
        .title(format!(" {} ", t(app.lang, "disk.warnmodal_title")))
        .title_style(theme::warn());
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // body (scrollable)
            Constraint::Length(1), // hint
        ])
        .split(inner);

    // Scrollable body — clamp offset so w/s can't run past the bottom.
    let body_area_h = parts[0].height;
    let max_scroll = body_h.saturating_sub(body_area_h);
    let scroll = app.disk_warn_scroll.min(max_scroll);
    app.disk_warn_scroll = scroll;
    f.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: true })
            .scroll((scroll, 0)),
        parts[0],
    );

    // Hint, with scroll arrows only when there's more off-screen.
    let base = t(app.lang, "disk.warnmodal_hint");
    let hint = if max_scroll > 0 {
        let arrows = if scroll == 0 {
            "▼"
        } else if scroll >= max_scroll {
            "▲"
        } else {
            "▲▼"
        };
        format!("{arrows}  {base}")
    } else {
        base
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, theme::mute()))),
        parts[1],
    );
}

/// One big boot-mode card: a rounded bordered box titled with the mode name,
/// containing a selection line and a dim description. The SELECTED card shows
/// its name as a reversed-video pill (works on any console, no palette
/// dependence) with an accent border; the unselected one is dim. Focus on the
/// row brightens the selected card further (bold border).
fn boot_mode_card(
    f: &mut Frame,
    area: Rect,
    name: &str,
    desc: &str,
    selected: bool,
    focused: bool,
) {
    let (border_style, title_style) = if selected {
        let st = if focused {
            theme::gold()
        } else {
            theme::accent()
        };
        (st, st)
    } else {
        (theme::border_dim(), theme::dim())
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(format!(" {name} "))
        .title_style(title_style);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let name_span = if selected {
        // Bright bold cyan ● + name — glows, no background fill (reversed
        // video reads as a muddy grey-on-cyan slab on real fbcon palettes).
        Span::styled(format!(" ● {name} "), theme::gold())
    } else {
        Span::styled(format!(" ○ {name} "), theme::mute())
    };
    let lines = vec![
        Line::from(vec![Span::raw(" "), name_span]),
        Line::from(Span::styled(format!("   {desc}"), theme::dim())),
    ];
    f.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }),
        inner,
    );
}

/// A prominent "pill" toggle option: reversed-video ` ● LABEL ` when selected
/// (readable on any console, no palette dependence), dim `[ LABEL ]` when not.
fn pill(label: &str, selected: bool, focused: bool) -> Span<'static> {
    if selected {
        // Bright bold cyan when the row is focused, plain bright when not —
        // no reversed fill (muddy on fbcon).
        let st = if focused {
            theme::gold()
        } else {
            theme::normal()
        };
        Span::styled(format!("[ ● {label} ]"), st)
    } else {
        Span::styled(format!("[ {label} ]"), theme::mute())
    }
}

/// Supported root filesystems: (config id, display label). ext4 first/default.
const FS_LIST: &[(&str, &str)] = &[
    ("ext4", "ext4"),
    ("btrfs", "btrfs"),
    ("xfs", "xfs"),
    ("f2fs", "f2fs"),
    ("jfs", "jfs"),
    ("ext3", "ext3"),
    ("ext2", "ext2"),
];

/// Optional features offered for a given filesystem, as (id, label) pairs. All
/// are off by default (plain filesystem); the user toggles any they want. Empty
/// slice = the filesystem has no extra options, so the options row never shows.
fn fs_features(fs: &str) -> &'static [(&'static str, &'static str)] {
    match fs {
        "btrfs" => &[
            (
                "subvolumes",
                "Subvolumes (@, @home, @snapshots, @log, @cache)",
            ),
            ("snapshots", "Auto-snapshots (snapper + snap-pac)"),
            ("compress", "Compression (zstd)"),
            ("discard", "SSD TRIM (discard=async)"),
        ],
        // Other filesystems expose no extra options (noatime was dropped: its
        // benefit is marginal and it can surprise non-experts).
        _ => &[],
    }
}

fn feature_on(app: &App, fs: &str, id: &str) -> bool {
    match (fs, id) {
        ("btrfs", "subvolumes") => app.config.btrfs_subvolumes,
        ("btrfs", "snapshots") => app.config.btrfs_snapshots,
        ("btrfs", "compress") => app.config.btrfs_compress,
        ("btrfs", "discard") => app.config.btrfs_discard,
        _ => false,
    }
}

fn toggle_feature(app: &mut App, fs: &str, id: &str) {
    match (fs, id) {
        // Snapshots need the @/@snapshots layout, so the two are kept in sync:
        // enabling snapshots turns subvolumes on; turning subvolumes off turns
        // snapshots off. This makes an invalid combination unreachable.
        ("btrfs", "subvolumes") => {
            app.config.btrfs_subvolumes = !app.config.btrfs_subvolumes;
            if !app.config.btrfs_subvolumes {
                app.config.btrfs_snapshots = false;
            }
        }
        ("btrfs", "snapshots") => {
            app.config.btrfs_snapshots = !app.config.btrfs_snapshots;
            if app.config.btrfs_snapshots {
                app.config.btrfs_subvolumes = true;
            }
        }
        ("btrfs", "compress") => app.config.btrfs_compress = !app.config.btrfs_compress,
        ("btrfs", "discard") => app.config.btrfs_discard = !app.config.btrfs_discard,
        _ => {}
    }
}

pub fn footer_hint(app: &App) -> String {
    if app.fs_opts_modal_open {
        return t(app.lang, "disk.fsopt_modal_hint");
    }
    // The four areas drive ←/→ differently (boot mode, swap size, filesystem),
    // so the hint follows the focused area instead of cramming every key into
    // one line that's wrong for three of the four rows.
    let key = match app.disk_focus {
        0 => "disk.footer_boot",
        1 => "disk.footer_disk",
        2 => "disk.footer_swap",
        _ => "disk.footer_fs",
    };
    t(app.lang, key)
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    // The pre-flight warnings modal captures all keys while it's open. It's
    // advisory: w/s scroll, Enter/Esc close. Closing never changes any choice.
    if app.disk_warn_modal_open {
        match key.code {
            KeyCode::Char('w') | KeyCode::Char('W') | KeyCode::Up | KeyCode::Char('k') => {
                app.disk_warn_scroll = app.disk_warn_scroll.saturating_sub(1)
            }
            KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Down | KeyCode::Char('j') => {
                app.disk_warn_scroll = app.disk_warn_scroll.saturating_add(1)
            }
            KeyCode::Enter | KeyCode::Esc => {
                app.disk_warn_modal_open = false;
                app.disk_warn_scroll = 0;
            }
            _ => {}
        }
        return;
    }
    // The filesystem-options modal captures all keys while it's open.
    if app.fs_opts_modal_open {
        let fs = app.config.root_fs.clone();
        let opts = fs_features(&fs);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                app.fs_opt_cursor = app.fs_opt_cursor.saturating_sub(1);
                app.fs_opt_desc_scroll = 0; // new option → fresh description
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = opts.len().saturating_sub(1);
                if app.fs_opt_cursor < max {
                    app.fs_opt_cursor += 1;
                }
                app.fs_opt_desc_scroll = 0;
            }
            // w / s scroll the description, for screens too short to show it all.
            // (Over-scrolling is clamped to the real bottom at render time.)
            KeyCode::Char('w') | KeyCode::Char('W') => {
                app.fs_opt_desc_scroll = app.fs_opt_desc_scroll.saturating_sub(1)
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                app.fs_opt_desc_scroll = app.fs_opt_desc_scroll.saturating_add(1)
            }
            KeyCode::Char(' ') => {
                if let Some((id, _)) = opts.get(app.fs_opt_cursor) {
                    toggle_feature(app, &fs, id);
                }
            }
            KeyCode::Enter | KeyCode::Esc => app.fs_opts_modal_open = false,
            _ => {}
        }
        return;
    }
    // Esc steps focus to the previous area (boot mode ← disk ← swap ← fs).
    // When already on the first area (focus 0) the global handler intercepts
    // Esc and leaves to the previous screen, so this only fires for focus > 0.
    if key.code == KeyCode::Esc {
        app.disk_focus = app.disk_focus.saturating_sub(1);
        return;
    }
    // Focus moves between the areas via Up/Down (handled per-area below).
    match app.disk_focus {
        0 => match key.code {
            // The pills are shown horizontally as [ UEFI ] [ BIOS ], so Left
            // picks UEFI (left) and Right picks BIOS (right); Space toggles.
            // Up leaves to the previous page (global handler, this is the top
            // row); Down moves on to the disk list, like every other area.
            KeyCode::Left => app.config.boot_mode = BootMode::Uefi,
            KeyCode::Right => app.config.boot_mode = BootMode::Bios,
            KeyCode::Char(' ') => {
                app.config.boot_mode = app.config.boot_mode.toggled();
            }
            KeyCode::Down | KeyCode::Enter => app.disk_focus = 1,
            _ => {}
        },
        1 => match key.code {
            KeyCode::Up => {
                if app.disk_cursor == 0 {
                    app.disk_focus = 0;
                } else {
                    app.disk_cursor -= 1;
                }
            }
            KeyCode::Down => {
                let max = disks().len().saturating_sub(1);
                if app.disk_cursor >= max {
                    app.disk_focus = 2;
                } else {
                    app.disk_cursor += 1;
                }
            }
            KeyCode::Enter => app.disk_focus = 2,
            // 'w' opens the pre-flight warnings modal for the selected disk,
            // but only when there's actually something to warn about.
            KeyCode::Char('w') | KeyCode::Char('W') => {
                if !preflight_warnings(app).is_empty() {
                    app.disk_warn_scroll = 0;
                    app.disk_warn_modal_open = true;
                }
            }
            _ => {}
        },
        2 => match key.code {
            KeyCode::Char(' ') => {
                // toggle swap on/off
                app.config.swap_gib = if app.config.swap_gib > 0 { 0 } else { 4 };
            }
            KeyCode::Char('+') | KeyCode::Right => {
                if app.config.swap_gib > 0 && app.config.swap_gib < 64 {
                    app.config.swap_gib += 1;
                }
            }
            KeyCode::Char('-') | KeyCode::Left => {
                if app.config.swap_gib > 1 {
                    app.config.swap_gib -= 1;
                }
            }
            KeyCode::Up => app.disk_focus = 1,
            KeyCode::Down => app.disk_focus = 3,
            KeyCode::Enter => app.disk_focus = 3,
            _ => {}
        },
        3 => match key.code {
            // Cycle the root filesystem with Left/Right (reset the options
            // cursor, since the available options change with the filesystem).
            KeyCode::Right => {
                let i = FS_LIST
                    .iter()
                    .position(|(id, _)| *id == app.config.root_fs)
                    .unwrap_or(0);
                let n = (i + 1) % FS_LIST.len();
                app.config.root_fs = FS_LIST[n].0.into();
                app.fs_opt_cursor = 0;
            }
            KeyCode::Left => {
                let i = FS_LIST
                    .iter()
                    .position(|(id, _)| *id == app.config.root_fs)
                    .unwrap_or(0);
                let n = if i == 0 { FS_LIST.len() - 1 } else { i - 1 };
                app.config.root_fs = FS_LIST[n].0.into();
                app.fs_opt_cursor = 0;
            }
            KeyCode::Up => app.disk_focus = 2,
            // `o` opens the filesystem-options picker (checklist + descriptions),
            // but only for filesystems that actually have options (e.g. btrfs).
            KeyCode::Char('o') | KeyCode::Char('O') => {
                if !fs_features(&app.config.root_fs).is_empty() {
                    app.fs_opt_cursor = 0;
                    app.fs_opts_modal_open = true;
                }
            }
            KeyCode::Enter if app.can_advance => app.goto_next(),
            _ => {}
        },
        _ => {}
    }
}
