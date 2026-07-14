//! Step 10 — additional disks & existing partitions.
//!
//! Lists every storage target other than the system disk and the live medium,
//! each as a small card with its own description of exactly what will happen.
//! Two kinds, decided by what's on the device:
//!   • an EMPTY disk -> "format & mount": wiped, one partition, formatted (the
//!     filesystem is cycled with `f`), mounted. For a separate /home or storage.
//!   • an EXISTING partition with a filesystem (e.g. an NTFS Windows volume) ->
//!     "mount, keep data": mounted as-is, never formatted.
//!
//! Pseudo devices (floppy/cdrom/loop/zram) and the live ISO disk are filtered
//! out, so a disk with filesystems is only ever shown via its partitions —
//! Windows can never be formatted by accident.

use crate::app::{App, ExtraDisk};
use crate::i18n::t;
use crate::screens::widgets;
use crate::system::disk::{self, Partition};
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};
use std::sync::OnceLock;

// Mount destinations are no longer fixed presets: the user picks a base and
// types a folder name. Bases: skip / a folder under the user's home / the WHOLE
// /home on this disk / under /mnt / a full custom path.
const BASES: &[&str] = &["", "home", "homedisk", "mnt", "custom"];
const FS_OPTS: &[&str] = &["ext4", "btrfs", "xfs", "f2fs", "jfs", "ext3", "ext2"];

fn parts_cached() -> &'static Vec<Partition> {
    static P: OnceLock<Vec<Partition>> = OnceLock::new();
    P.get_or_init(|| disk::list_partitions().unwrap_or_default())
}

/// Floppy / optical / loop / ram-ish devices that are never install targets.
fn is_pseudo(name: &str) -> bool {
    name.starts_with("fd")
        || name.starts_with("sr")
        || name.starts_with("loop")
        || name.starts_with("zram")
        || name.starts_with("ram")
}

struct Cand {
    dev: String,
    size: String,
    info: String,     // model (format) or "fstype \"label\"" (existing)
    format: bool,     // default action: format (empty disk) vs mount (existing)
    whole_disk: bool, // empty whole disk (repartition) vs an existing partition
    fs_fixed: String, // detected fs for existing partitions
}

/// Build the candidate rows, excluding the system disk, the USB key stick, the
/// live ISO medium and pseudo devices. Empty disks become format rows; disks
/// that already carry filesystems contribute their (non-ISO) partitions as
/// mount rows.
fn candidates(app: &App) -> Vec<Cand> {
    let sys = app.config.disk.clone();
    // The stick chosen as the LUKS key must never show up here either. It gets
    // wiped and rewritten by the install (FAT32, label ARTIXKEY) as the key
    // carrier — offering it as an extra disk to format and mount would let the
    // user schedule a SECOND, conflicting format of the very device that holds
    // the only thing able to unlock the system. The disk screen hides it for
    // the same reason; this is the other place a device list is drawn.
    let key = app.config.usb_key_device.clone();
    let parts = parts_cached();
    // Disks that hold the live ISO (iso9660) are the boot medium — never offer.
    let live: Vec<String> = parts
        .iter()
        .filter(|p| p.fstype.eq_ignore_ascii_case("iso9660"))
        .map(|p| p.parent.clone())
        .collect();
    let mut out = Vec::new();
    for d in disk::disks_list() {
        let name = d.path.trim_start_matches("/dev/");
        if d.path == sys
            || (!key.is_empty() && d.path == key)
            || is_pseudo(name)
            || live.contains(&d.path)
        {
            continue;
        }
        let pof: Vec<&Partition> = parts
            .iter()
            .filter(|p| p.parent == d.path && !p.fstype.eq_ignore_ascii_case("iso9660"))
            .collect();
        if pof.is_empty() {
            out.push(Cand {
                dev: d.path.clone(),
                size: d.size.clone(),
                info: if d.model.is_empty() {
                    "disk".into()
                } else {
                    d.model.clone()
                },
                // Default to NOT formatting. The old default (format: true) was
                // harmless in effect — nothing happens without a mountpoint —
                // but it SHOWED "Format: ext4" next to the user's data disk, and
                // a cautious person reads that as a countdown, not a suggestion.
                format: false,
                whole_disk: true,
                fs_fixed: String::new(),
            });
        } else {
            for p in pof {
                let info = if p.label.is_empty() {
                    p.fstype.clone()
                } else {
                    format!("{} \u{201c}{}\u{201d}", p.fstype, p.label)
                };
                out.push(Cand {
                    dev: p.path.clone(),
                    size: p.size.clone(),
                    info,
                    format: false,
                    whole_disk: false,
                    fs_fixed: p.fstype.clone(),
                });
            }
        }
    }
    out
}

fn entry<'a>(app: &'a App, dev: &str) -> Option<&'a ExtraDisk> {
    app.config.extra_disks.iter().find(|e| e.disk == dev)
}
fn cur_mp(app: &App, dev: &str) -> String {
    entry(app, dev)
        .map(|e| e.mountpoint.clone())
        .unwrap_or_default()
}
fn cur_fs(app: &App, c: &Cand) -> String {
    entry(app, &c.dev).map(|e| e.fs.clone()).unwrap_or_else(|| {
        if c.format {
            "ext4".into()
        } else {
            c.fs_fixed.clone()
        }
    })
}
/// Effective format decision for this device: the stored entry (which the user
/// may have toggled, e.g. an existing partition switched to reformat) overrides
/// the candidate's default. Drives the action tag, options, descriptions.
fn will_format(app: &App, c: &Cand) -> bool {
    entry(app, &c.dev).map(|e| e.format).unwrap_or(c.format)
}

// The storage cursor addresses a flat list of focusable control rows. Every
// disk has a filesystem row and a mountpoint row; disks that will be formatted
// also get an encryption row (you can't encrypt a partition kept as-is). ↑/↓
// walk these rows; ←/→ (or Space on the encryption row) change the value.
#[derive(Clone, Copy, PartialEq)]
enum Row {
    Fs,
    Mount,
    Encrypt,
}
fn rows_list(app: &App, cands: &[Cand]) -> Vec<(usize, Row)> {
    let mut v = Vec::new();
    for (i, c) in cands.iter().enumerate() {
        v.push((i, Row::Fs));
        v.push((i, Row::Mount));
        if will_format(app, c) {
            v.push((i, Row::Encrypt));
        }
    }
    v
}

/// The filesystem strip for a disk: the choices and which one is selected.
/// Empty disks cycle the filesystems; existing partitions get a leading
/// "keep data" slot (mount as-is) before the reformat choices.
fn fs_pills(app: &App, c: &Cand) -> (Vec<String>, usize) {
    if c.whole_disk {
        // Slot 0 is "leave it alone", exactly as for existing partitions. An
        // empty disk showing only "Format: ‹ ext4 ›" reads as a THREAT to a
        // cautious user — nothing on screen says the disk can be left untouched,
        // so people quit the installer rather than risk their data. The option
        // was always there implicitly (no mountpoint ⇒ no action), but implicit
        // safety is worthless if it isn't visible. Now it's the DEFAULT.
        let mut opts = vec![t(app.lang, "storage.no_format")];
        opts.extend(FS_OPTS.iter().map(|s| s.to_string()));
        let sel = if !will_format(app, c) {
            0
        } else {
            let cur = cur_fs(app, c);
            FS_OPTS
                .iter()
                .position(|f| *f == cur)
                .map(|p| p + 1)
                .unwrap_or(1)
        };
        (opts, sel)
    } else {
        let mut opts = vec![t(app.lang, "storage.keep")];
        opts.extend(FS_OPTS.iter().map(|s| s.to_string()));
        let sel = if !will_format(app, c) {
            0
        } else {
            let cur = cur_fs(app, c);
            FS_OPTS
                .iter()
                .position(|f| *f == cur)
                .map(|p| p + 1)
                .unwrap_or(1)
        };
        (opts, sel)
    }
}

/// The mount-destination strip for a disk: skip, the user's home, /mnt, or a
/// full custom path. The folder name is typed in the input line beneath the
/// strip when a base needs one.
fn base_pills(app: &App, c: &Cand) -> (Vec<String>, usize) {
    let opts: Vec<String> = BASES.iter().map(|b| base_label(app, b)).collect();
    let cur = cur_base(app, &c.dev);
    let sel = BASES.iter().position(|b| *b == cur).unwrap_or(0);
    (opts, sel)
}
fn base_label(app: &App, base: &str) -> String {
    match base {
        "home" => t(app.lang, "storage.base_home"),
        "homedisk" => t(app.lang, "storage.base_homedisk"),
        "mnt" => "/mnt".to_string(),
        "custom" => t(app.lang, "storage.base_custom"),
        _ => "\u{2014}".to_string(), // skip
    }
}
fn cur_base(app: &App, dev: &str) -> String {
    entry(app, dev)
        .map(|e| e.mount_base.clone())
        .unwrap_or_default()
}
fn base_needs_name(base: &str) -> bool {
    matches!(base, "home" | "mnt" | "custom")
}

/// Render a horizontal strip of choices, bracketing the selected one. The
/// focused strip highlights its selection in bold; an unfocused strip still
/// shows its selection (so every disk's current choice is visible at a glance).
fn pill_line(options: &[String], selected: usize, focused: bool) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, opt) in options.iter().enumerate() {
        if i == selected {
            let st = if focused {
                theme::gold()
            } else {
                theme::accent()
            };
            spans.push(Span::styled(format!("\u{2039} {} \u{203a}", opt), st));
        } else {
            spans.push(Span::styled(opt.clone(), theme::mute()));
        }
        spans.push(Span::raw(" "));
    }
    spans
}
/// Recompute the canonical mountpoint (and the bookmark flag) from the chosen
/// base and name. Home is stored as "~/name" and resolved to /home/<user>/name
/// at install time; /mnt and custom resolve to absolute paths.
fn sync_mountpoint(e: &mut ExtraDisk) {
    e.mountpoint = match e.mount_base.as_str() {
        "home" if !e.mount_name.is_empty() => format!("~/{}", e.mount_name),
        // The whole home folder lives on this disk: a fixed /home, no name.
        "homedisk" => "/home".to_string(),
        "mnt" if !e.mount_name.is_empty() => format!("/mnt/{}", e.mount_name),
        "custom" if !e.mount_name.is_empty() => {
            if e.mount_name.starts_with('/') {
                e.mount_name.clone()
            } else {
                format!("/{}", e.mount_name)
            }
        }
        _ => String::new(),
    };
    // Any mounted folder gets a file-manager sidebar bookmark so it's easy to find.
    e.bookmark = !e.mountpoint.is_empty();
}

/// Get the entry for a device, creating one (with sensible defaults, mountpoint
/// empty = not yet in use) if it doesn't exist. Lets `f` set the filesystem even
/// before a mountpoint is chosen.
fn entry_mut<'a>(app: &'a mut App, c: &Cand) -> &'a mut ExtraDisk {
    if !app.config.extra_disks.iter().any(|e| e.disk == c.dev) {
        // A whole (empty) disk starts on "don't format", but still needs a
        // filesystem parked in `fs` — that's what the pills cycle back to when
        // the user moves off slot 0. An existing partition keeps its detected fs.
        let fs = if c.whole_disk {
            "ext4".to_string()
        } else {
            c.fs_fixed.clone()
        };
        app.config.extra_disks.push(ExtraDisk {
            disk: c.dev.clone(),
            mountpoint: String::new(),
            fs,
            format: c.format,
            whole_disk: c.whole_disk,
            noatime: false,
            compress: false,
            encrypt: false,
            bookmark: false,
            mount_base: String::new(),
            mount_name: String::new(),
        });
    }
    app.config
        .extra_disks
        .iter_mut()
        .find(|e| e.disk == c.dev)
        .unwrap()
}

fn cycle_base(app: &mut App, c: &Cand, dir: i32) {
    let e = entry_mut(app, c);
    step_base(e, dir);
}

/// Advance the mountpoint picker by one slot. Pure: it touches only the entry,
/// so it can be tested without an App or a disk probe.
///
/// `format` is deliberately NOT derived from the mountpoint here.
///
/// It used to be: `e.format = !e.mountpoint.is_empty()`. That reads as
/// reasonable — an empty disk with nowhere to mount has no reason to be
/// formatted — but it silently destroyed choices the user had already made,
/// because the mountpoint is EMPTY on intermediate steps of this very cycle:
///
///   "" → "home" → "homedisk"
///          ↑
///          mount_name is still blank here (no folder name typed yet), so
///          sync_mountpoint() yields "" — and `format` flipped to false ON THE
///          WAY PAST, taking `compress` with it. By the time "homedisk" set the
///          mountpoint back to /home, the flag was already gone.
///
/// Cycling forward through one control must not destroy state that belongs to a
/// DIFFERENT control. The filesystem strip decides `format`, explicitly and in
/// one place; nothing else recomputes it behind the user's back.
pub(crate) fn step_base(e: &mut ExtraDisk, dir: i32) {
    let i = BASES.iter().position(|b| *b == e.mount_base).unwrap_or(0) as i32;
    let n = BASES.len() as i32;
    e.mount_base = BASES[(((i + dir) % n + n) % n) as usize].to_string();
    sync_mountpoint(e);
    sync_compress(e);
}

/// Keep `compress` consistent with what the disk is actually going to be.
///
/// zstd compression is a btrfs mount option: it means nothing on ext4, and it
/// means nothing on a disk that isn't being formatted at all. But it must NOT
/// be dropped for any other reason — in particular not because some transient
/// step of a UI cycle happened to pass through "no mountpoint" or "no format".
/// One rule, one place, applied after every edit.
pub(crate) fn sync_compress(e: &mut ExtraDisk) {
    if !e.format || e.fs != "btrfs" {
        e.compress = false;
    }
}

/// Inline editing of the folder name (or full path for "custom"). Allowed:
/// letters/digits/-/_ (plus "/" for a custom path); length-capped so the name
/// stays tidy on disk and in the file manager.
fn name_push(app: &mut App, c: &Cand, ch: char) {
    let base = cur_base(app, &c.dev);
    if !base_needs_name(&base) {
        return;
    }
    let custom = base == "custom";
    let ok = ch.is_alphanumeric() || ch == '-' || ch == '_' || (custom && ch == '/');
    let max = if custom { 48 } else { 32 };
    let e = entry_mut(app, c);
    if ok && e.mount_name.chars().count() < max {
        e.mount_name.push(ch);
        sync_mountpoint(e);
    }
}
fn name_pop(app: &mut App, c: &Cand) {
    let e = entry_mut(app, c);
    e.mount_name.pop();
    sync_mountpoint(e);
}

fn cycle_fs(app: &mut App, c: &Cand, dir: i32) {
    if c.whole_disk {
        // Empty disk: [no-format, ext4, btrfs, …]. Slot 0 leaves the disk alone
        // entirely — no partitioning, no mkfs, no mount. Clearing `format` also
        // clears the mountpoint and the format-only options (encrypt/compress),
        // so a disk parked on "don't touch" carries no half-set state that could
        // resurrect a format later.
        let cur_idx: i32 = if !will_format(app, c) {
            0
        } else {
            let cur = cur_fs(app, c);
            FS_OPTS
                .iter()
                .position(|f| *f == cur)
                .map(|p| p as i32 + 1)
                .unwrap_or(1)
        };
        let n = FS_OPTS.len() as i32 + 1; // +1 for the no-format slot
        let new_idx = (cur_idx + dir + n) % n;
        let e = entry_mut(app, c);
        if new_idx == 0 {
            e.format = false;
            // Clear mount_base too, not just mountpoint: the UI renders from
            // mount_base, so leaving it set would show a mountpoint next to a
            // disk that isn't being touched.
            e.mount_base.clear();
            e.mountpoint.clear();
            e.encrypt = false;
        } else {
            e.format = true;
            e.fs = FS_OPTS[(new_idx - 1) as usize].to_string();
        }
        sync_compress(e);
        return;
    }
    // Existing partition: cycle [keep-data, ext4, btrfs, xfs, f2fs, jfs, ext3,
    // ext2]. Slot 0 keeps the data (mount as-is); any real filesystem reformats
    // the partition IN PLACE (mkfs on the partition, no repartition) — wiping it.
    let cur_idx: i32 = if !will_format(app, c) {
        0
    } else {
        let cur = cur_fs(app, c);
        FS_OPTS
            .iter()
            .position(|f| *f == cur)
            .map(|p| p as i32 + 1)
            .unwrap_or(1)
    };
    let n = FS_OPTS.len() as i32 + 1; // +1 for the keep-data slot
    let new_idx = (cur_idx + dir + n) % n;
    let fs_fixed = c.fs_fixed.clone();
    let e = entry_mut(app, c);
    if new_idx == 0 {
        e.format = false;
        e.fs = fs_fixed;
        e.encrypt = false;
    } else {
        e.format = true;
        e.fs = FS_OPTS[(new_idx - 1) as usize].to_string();
    }
    sync_compress(e);
}

pub fn footer_hint(app: &App) -> String {
    if app.storage_opts_modal_open {
        return t(app.lang, "disk.fsopt_modal_hint");
    }
    t(app.lang, "storage.footer")
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let cands = candidates(app);
    app.can_advance = true; // additional storage is always optional

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // intro
            Constraint::Min(3),    // cards
            Constraint::Length(3), // warnings (wrapped)
            Constraint::Length(3), // actions
        ])
        .split(area);

    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                t(app.lang, "storage.intro1"),
                theme::heading(),
            )),
            Line::from(Span::styled(t(app.lang, "storage.intro2"), theme::dim())),
            Line::from(Span::styled(t(app.lang, "storage.intro3"), theme::dim())),
        ]),
        rows[0],
    );

    if cands.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("   {}", t(app.lang, "storage.none")),
                    theme::mute(),
                )),
            ]),
            rows[1],
        );
    } else {
        let rlist = rows_list(app, &cands);
        let cur = app.storage_cursor.min(rlist.len().saturating_sub(1));
        let (fdisk, fkind) = rlist.get(cur).copied().unwrap_or((0, Row::Fs));
        let mut lines: Vec<Line> = Vec::new();
        let mut focused_line = 0usize; // line index of the focused control row

        for (i, c) in cands.iter().enumerate() {
            let is_sel = i == fdisk;
            let mp = cur_mp(app, &c.dev);
            let fs = cur_fs(app, c);

            // Header: ▌ device   size · model/fstype.
            let title_style = if is_sel {
                theme::title()
            } else {
                theme::normal()
            };
            lines.push(Line::from(vec![
                if is_sel {
                    Span::styled("\u{258c} ", theme::accent())
                } else {
                    Span::raw("  ")
                },
                Span::styled(c.dev.clone(), title_style),
                Span::styled(format!("   {} \u{00b7} {}", c.size, c.info), theme::dim()),
            ]));

            // Row 1 — filesystem strip + the [o] options button (btrfs only).
            let fs_here = is_sel && fkind == Row::Fs;
            if fs_here {
                focused_line = lines.len();
            }
            let (fs_opts, fs_sel) = fs_pills(app, c);
            // The label tracks the choice: an existing partition kept as-is reads
            // "Дані: ‹ зберегти файли ›" (clearly NOT formatting); a disk that will
            // be formatted reads "Формат: ‹ ext4 ›".
            let fs_label = if c.whole_disk || will_format(app, c) {
                t(app.lang, "storage.lbl_format")
            } else {
                t(app.lang, "storage.lbl_data")
            };
            let mut fs_spans = vec![
                Span::styled(
                    if fs_here { "   \u{203a} " } else { "     " },
                    theme::accent(),
                ),
                Span::styled(format!("{}  ", fs_label), theme::dim()),
            ];
            fs_spans.extend(pill_line(&fs_opts, fs_sel, fs_here));
            if !fmt_opts(app, c).is_empty() {
                fs_spans.push(Span::styled(
                    format!("  [o] {}", t(app.lang, "storage.btn_opts")),
                    theme::accent(),
                ));
            }
            lines.push(Line::from(fs_spans));

            // Row 2 — mount destination: base selector (— / home / /mnt / custom).
            let mp_here = is_sel && fkind == Row::Mount;
            if mp_here {
                focused_line = lines.len();
            }
            let (base_opts, base_sel) = base_pills(app, c);
            let mut mp_spans = vec![
                Span::styled(
                    if mp_here { "   \u{203a} " } else { "     " },
                    theme::accent(),
                ),
                Span::styled(
                    format!("{}  ", t(app.lang, "storage.lbl_mount")),
                    theme::dim(),
                ),
            ];
            mp_spans.extend(pill_line(&base_opts, base_sel, mp_here));
            lines.push(Line::from(mp_spans));

            // Name input line — shown only when the chosen base needs a folder
            // name. The user types it inline while this row is focused.
            let base = cur_base(app, &c.dev);
            if base_needs_name(&base) {
                let custom = base == "custom";
                let name = entry(app, &c.dev)
                    .map(|e| e.mount_name.clone())
                    .unwrap_or_default();
                let label = if custom {
                    t(app.lang, "storage.custom_path")
                } else {
                    t(app.lang, "storage.custom_name")
                };
                let cursor = if mp_here { "\u{2588}" } else { "" };
                let mut nspans = vec![
                    Span::raw("       "),
                    Span::styled(format!("{}  ", label), theme::dim()),
                    Span::styled(name, theme::normal()),
                    Span::styled(cursor.to_string(), theme::accent()),
                ];
                // Show the resulting path as a preview.
                let preview = cur_mp(app, &c.dev);
                if !preview.is_empty() {
                    nspans.push(Span::styled(
                        format!("    \u{2192} {}", preview),
                        theme::mute(),
                    ));
                }
                lines.push(Line::from(nspans));
            }

            // Row 3 — encryption checkbox (only for disks that will be formatted).
            if will_format(app, c) {
                let enc_here = is_sel && fkind == Row::Encrypt;
                if enc_here {
                    focused_line = lines.len();
                }
                let on = entry(app, &c.dev).map(|e| e.encrypt).unwrap_or(false);
                let box_ = if on { "[x]" } else { "[ ]" };
                let state = t(
                    app.lang,
                    if on {
                        "storage.enc_on"
                    } else {
                        "storage.enc_off"
                    },
                );
                let box_style = if enc_here {
                    theme::gold()
                } else if on {
                    theme::accent()
                } else {
                    theme::mute()
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        if enc_here { "   \u{203a} " } else { "     " },
                        theme::accent(),
                    ),
                    Span::styled(
                        format!("{}  ", t(app.lang, "storage.lbl_encrypt")),
                        theme::dim(),
                    ),
                    Span::styled(format!("{} {}", box_, state), box_style),
                ]));
            }

            // What this choice does (wrapped, at most two lines).
            let desc = description(app, c, &mp, &fs);
            for dl in wrap_text(&desc, 72).into_iter().take(2) {
                lines.push(Line::from(vec![
                    Span::raw("       "),
                    Span::styled(dl, theme::dim()),
                ]));
            }
            lines.push(Line::from("")); // gap between cards
        }

        // Scroll so the focused control row stays in view (kept roughly centred),
        // which lets the list grow past the screen when there are many disks.
        let view_h = rows[1].height as usize;
        let total = lines.len();
        let off = if total > view_h {
            focused_line.saturating_sub(view_h / 2).min(total - view_h)
        } else {
            0
        };
        f.render_widget(Paragraph::new(lines).scroll((off as u16, 0)), rows[1]);
    }

    // Warnings (up to two lines).
    let mut warns: Vec<Line> = Vec::new();
    let any_ntfs = app
        .config
        .extra_disks
        .iter()
        .any(|e| !e.format && !e.mountpoint.is_empty() && e.fs.eq_ignore_ascii_case("ntfs"));
    if any_ntfs {
        warns.push(Line::from(Span::styled(
            format!("  {}", t(app.lang, "storage.ntfs_warn")),
            theme::warn(),
        )));
    }
    if !app.config.encrypt_disk
        && app
            .config
            .extra_disks
            .iter()
            .any(|e| e.encrypt && !e.mountpoint.is_empty())
    {
        warns.push(Line::from(Span::styled(
            format!("  {}", t(app.lang, "storage.enc_warn")),
            theme::warn(),
        )));
    }
    if !warns.is_empty() {
        f.render_widget(Paragraph::new(warns).wrap(Wrap { trim: true }), rows[2]);
    }

    widgets::action_row(
        f,
        rows[3],
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        app.can_advance,
    );

    if app.storage_opts_modal_open {
        draw_storage_opts_modal(f, app, area);
    }
}

/// Format-disk options offered in the storage options modal. noatime applies to
/// any filesystem; compression is btrfs-only. Existing partitions get none.
fn fmt_opts(app: &App, c: &Cand) -> Vec<&'static str> {
    if !will_format(app, c) {
        return Vec::new();
    }
    // Compression is btrfs-only. noatime was dropped (novice safety) and
    // encryption now has its own visible row, so this modal is btrfs-only.
    let mut v: Vec<&'static str> = Vec::new();
    if cur_fs(app, c) == "btrfs" {
        v.push("compress");
    }
    v
}
fn opt_on(app: &App, dev: &str, id: &str) -> bool {
    entry(app, dev)
        .map(|e| match id {
            "noatime" => e.noatime,
            "compress" => e.compress,
            "encrypt" => e.encrypt,
            _ => false,
        })
        .unwrap_or(false)
}
fn toggle_opt(app: &mut App, c: &Cand, id: &str) {
    let e = entry_mut(app, c);
    match id {
        "noatime" => e.noatime = !e.noatime,
        "compress" => e.compress = !e.compress,
        "encrypt" => e.encrypt = !e.encrypt,
        _ => {}
    }
    // The modal only offers `compress` on btrfs, but go through the same rule
    // anyway rather than trusting the caller: one place decides, everywhere
    // else defers to it.
    sync_compress(e);
}

/// Centered modal of per-disk format options for the selected format candidate,
/// reusing the same labels and gain/loss descriptions as the root disk screen.
fn draw_storage_opts_modal(f: &mut Frame, app: &App, area: Rect) {
    use ratatui::widgets::{Block, BorderType, Borders, Clear, Wrap};
    let cands = candidates(app);
    if cands.is_empty() {
        return;
    }
    let sel = {
        let rl = rows_list(app, &cands);
        rl.get(app.storage_cursor.min(rl.len().saturating_sub(1)))
            .map(|(d, _)| *d)
            .unwrap_or(0)
    };
    let c = &cands[sel];
    let opts = fmt_opts(app, c);
    if opts.is_empty() {
        return;
    }
    let cursor = app.storage_opt_cursor.min(opts.len() - 1);

    let w = 72u16.min(area.width.saturating_sub(4));
    let desc_h = 6u16;
    let h = (opts.len() as u16 + desc_h + 5).min(area.height.saturating_sub(2));
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
            " {} \u{2014} {} ",
            t(app.lang, "disk.fsopt_modal_title"),
            c.dev
        ))
        .title_style(theme::title());
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(opts.len() as u16),
            Constraint::Length(1),
            Constraint::Min(desc_h),
            Constraint::Length(1),
        ])
        .split(inner);

    let mut lines: Vec<Line> = Vec::new();
    for (i, id) in opts.iter().enumerate() {
        let on = opt_on(app, &c.dev, id);
        let mark = if on { "[x]" } else { "[ ]" };
        let is_cur = i == cursor;
        let st = if is_cur {
            theme::gold()
        } else if on {
            theme::normal()
        } else {
            theme::mute()
        };
        let prefix = if is_cur { "\u{203a}" } else { " " };
        let label = t(app.lang, &format!("disk.fsopt_{id}"));
        lines.push(Line::from(Span::styled(
            format!(" {prefix} {mark} {label}"),
            st,
        )));
    }
    f.render_widget(Paragraph::new(lines), parts[0]);

    let desc = t(app.lang, &format!("disk.fsdesc_{}", opts[cursor]));
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(desc, theme::dim()))).wrap(Wrap { trim: true }),
        parts[2],
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            t(app.lang, "disk.fsopt_modal_hint"),
            theme::mute(),
        ))),
        parts[3],
    );
}

/// A sentence describing exactly what happens to this device with its current
/// choice. Templates carry %FS% / %MP% placeholders so they stay bilingual.
/// Plain-language word wrap (counts characters, so it's correct for Cyrillic).
fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        if line.is_empty() {
            line = word.to_string();
        } else if line.chars().count() + 1 + word.chars().count() <= width {
            line.push(' ');
            line.push_str(word);
        } else {
            out.push(std::mem::take(&mut line));
            line = word.to_string();
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// What a chosen mountpoint actually MEANS for a regular user, in plain words —
/// so "/home" reads as "your home folder where downloads and settings live" and
/// "/mnt/storage" reads as "extra storage", not as cryptic paths.
fn mp_meaning(app: &App, mp: &str) -> String {
    // Custom user-named folders: home-based, or a /mnt path that isn't a preset.
    let custom = mp.starts_with("~/")
        || (mp.starts_with("/mnt/")
            && mp != "/mnt/storage"
            && mp != "/mnt/windows"
            && mp != "/mnt/media");
    if custom {
        let name = mp.trim_start_matches("~/").trim_start_matches("/mnt/");
        let where_ = if mp.starts_with("~/") {
            t(app.lang, "storage.mean_custom_home")
        } else {
            t(app.lang, "storage.mean_custom_mnt")
        };
        return t(app.lang, "storage.mean_custom")
            .replace("%NAME%", name)
            .replace("%WHERE%", &where_);
    }
    let key = match mp {
        "/home" => "storage.mean_home",
        "/data" => "storage.mean_data",
        "/srv" => "storage.mean_srv",
        "/mnt/storage" => "storage.mean_storage",
        "/mnt/windows" | "/windows" => "storage.mean_windows",
        "/mnt/media" => "storage.mean_media",
        _ => "storage.mean_generic",
    };
    t(app.lang, key).replace("%MP%", mp)
}

/// A friendly, mountpoint-aware sentence: what this device becomes for the user,
/// plus whether it's wiped or kept. Templates carry %FS% / %MP% placeholders.
fn description(app: &App, c: &Cand, mp: &str, fs: &str) -> String {
    let formatting = will_format(app, c);
    let reformat_partition = formatting && !c.whole_disk; // existing partition wiped in place
    if mp.is_empty() {
        let key = if formatting {
            "storage.desc_fmt_off"
        } else {
            "storage.desc_mnt_off"
        };
        return t(app.lang, key).replace("%FS%", if formatting { fs } else { &c.fs_fixed });
    }
    let meaning = mp_meaning(app, mp);
    let action = if reformat_partition {
        t(app.lang, "storage.act_reformat")
            .replace("%FS%", fs)
            .replace("%OLD%", &c.fs_fixed)
    } else if formatting {
        t(app.lang, "storage.act_format").replace("%FS%", fs)
    } else {
        t(app.lang, "storage.act_keep").replace("%FS%", &c.fs_fixed)
    };
    format!("{meaning} {action}")
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    // The per-disk options modal captures keys while open.
    if app.storage_opts_modal_open {
        let cands = candidates(app);
        if cands.is_empty() {
            app.storage_opts_modal_open = false;
            return;
        }
        let sel = {
            let rl = rows_list(app, &cands);
            rl.get(app.storage_cursor.min(rl.len().saturating_sub(1)))
                .map(|(d, _)| *d)
                .unwrap_or(0)
        };
        let opts = fmt_opts(app, &cands[sel]);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                app.storage_opt_cursor = app.storage_opt_cursor.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = opts.len().saturating_sub(1);
                if app.storage_opt_cursor < max {
                    app.storage_opt_cursor += 1;
                }
            }
            KeyCode::Char(' ') => {
                if let Some(id) = opts
                    .get(app.storage_opt_cursor.min(opts.len().saturating_sub(1)))
                    .copied()
                {
                    toggle_opt(app, &cands[sel], id);
                }
            }
            KeyCode::Enter | KeyCode::Esc => app.storage_opts_modal_open = false,
            _ => {}
        }
        return;
    }
    if key.code == KeyCode::Esc {
        app.goto_prev();
        return;
    }
    let cands = candidates(app);
    if cands.is_empty() {
        if key.code == KeyCode::Enter {
            app.goto_next();
        }
        return;
    }
    let rlist = rows_list(app, &cands);
    let cur = app.storage_cursor.min(rlist.len() - 1);
    let (disk, kind) = rlist[cur];
    // On the mountpoint row, when the chosen base needs a folder name, the row
    // captures typing for that name — so letters like j/k/o go into the name
    // instead of triggering navigation/shortcuts.
    let editing_name = kind == Row::Mount && base_needs_name(&cur_base(app, &cands[disk].dev));
    match key.code {
        KeyCode::Up => app.storage_cursor = cur.saturating_sub(1),
        KeyCode::Down => app.storage_cursor = (cur + 1).min(rlist.len() - 1),
        KeyCode::Left => match kind {
            Row::Fs => cycle_fs(app, &cands[disk], -1),
            Row::Mount => cycle_base(app, &cands[disk], -1),
            Row::Encrypt => toggle_opt(app, &cands[disk], "encrypt"),
        },
        KeyCode::Right => match kind {
            Row::Fs => cycle_fs(app, &cands[disk], 1),
            Row::Mount => cycle_base(app, &cands[disk], 1),
            Row::Encrypt => toggle_opt(app, &cands[disk], "encrypt"),
        },
        KeyCode::Char(' ') if kind == Row::Encrypt => toggle_opt(app, &cands[disk], "encrypt"),
        KeyCode::Backspace if editing_name => name_pop(app, &cands[disk]),
        KeyCode::Enter => app.goto_next(),
        KeyCode::Char('o') | KeyCode::Char('O') if !editing_name => {
            if !fmt_opts(app, &cands[disk]).is_empty() {
                app.storage_opt_cursor = 0;
                app.storage_opts_modal_open = true;
            }
        }
        KeyCode::Char('k') if !editing_name => app.storage_cursor = cur.saturating_sub(1),
        KeyCode::Char('j') if !editing_name => app.storage_cursor = (cur + 1).min(rlist.len() - 1),
        KeyCode::Char(ch) if editing_name => name_push(app, &cands[disk], ch),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Lang;

    fn ext_disk() -> ExtraDisk {
        ExtraDisk {
            disk: "/dev/sdb1".into(),
            mountpoint: String::new(),
            fs: "ext4".into(),
            format: true,
            whole_disk: false,
            noatime: false,
            compress: false,
            encrypt: false,
            bookmark: false,
            mount_base: String::new(),
            mount_name: String::new(),
        }
    }

    #[test]
    fn sync_mountpoint_derives_path_from_base_and_name() {
        let mut e = ext_disk();
        e.mount_base = "home".into();
        e.mount_name = "data".into();
        sync_mountpoint(&mut e);
        assert_eq!(e.mountpoint, "~/data");
        assert!(e.bookmark);

        e.mount_base = "mnt".into();
        e.mount_name = "games".into();
        sync_mountpoint(&mut e);
        assert_eq!(e.mountpoint, "/mnt/games");

        // A relative custom path gets a leading slash; an absolute one is kept.
        e.mount_base = "custom".into();
        e.mount_name = "srv/web".into();
        sync_mountpoint(&mut e);
        assert_eq!(e.mountpoint, "/srv/web");

        e.mount_base = "custom".into();
        e.mount_name = "/opt/x".into();
        sync_mountpoint(&mut e);
        assert_eq!(e.mountpoint, "/opt/x");
    }

    #[test]
    fn sync_mountpoint_empty_without_name_or_base() {
        let mut e = ext_disk();
        e.mount_base = "home".into();
        e.mount_name = String::new();
        sync_mountpoint(&mut e);
        assert_eq!(e.mountpoint, "");
        assert!(!e.bookmark);

        e.mount_base = String::new();
        e.mount_name = "x".into();
        sync_mountpoint(&mut e);
        assert_eq!(e.mountpoint, "");
    }

    // Placeholder safety: no %FS%/%MP%/%OLD%/%NAME%/%WHERE% may survive in a
    // rendered storage description, in either language.
    #[test]
    fn description_leaves_no_placeholders() {
        let cand = Cand {
            dev: "/dev/sdb1".into(),
            size: "500G".into(),
            info: "ntfs".into(),
            format: false,
            whole_disk: false,
            fs_fixed: "ntfs".into(),
        };
        for lang in [Lang::Uk, Lang::En] {
            let mut a = App::new();
            a.lang = lang;
            for (mp, fs) in [("", "ext4"), ("/mnt/data", "btrfs"), ("~/files", "ext4")] {
                let d = description(&a, &cand, mp, fs);
                for ph in ["%FS%", "%MP%", "%OLD%", "%NAME%", "%WHERE%"] {
                    assert!(!d.contains(ph), "placeholder left in description: {}", ph);
                }
            }
        }
    }
}
