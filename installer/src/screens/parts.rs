//! The MANUAL face of the disk step — a small partition editor (v3).
//!
//! archinstall-inspired: one table shows every disk's partitions — existing
//! ones AND the new ones planned here — each with size, filesystem and role.
//! The user can:
//!
//!   * assign a role to an existing partition (Enter → role modal): the
//!     dual-boot path, where a Windows ESP is reused as-is and "the rest of
//!     the disk" belongs to another OS and is never touched;
//!   * CREATE a partition in the free space with a chosen size (the create
//!     row → role → size in MiB or GiB, ←/→ switches the unit; 0/empty = rest
//!     of the disk for / and /home). The device path is predicted from the
//!     partition numbering at once, so the whole downstream pipeline stays
//!     path-based;
//!   * DELETE a partition ('d'), planned or pre-existing, on any disk — which
//!     is how an existing 50 GiB partition gets re-made at a different size;
//!   * open the same filesystem-options modal the automatic face has ([o] —
//!     btrfs subvolumes/snapshots/compression), which was unreachable from
//!     manual mode before;
//!   * still make partitions externally (cfdisk on console 2) and rescan (r).
//!
//! Deleting real partitions was refused until v3, on the grounds that an
//! installer editing somebody's table is one off-by-one away from eating an OS.
//! It is allowed now because manual partitioning is not only for dual boot —
//! resizing an existing partition is impossible without it. The safety comes
//! from WHERE the destruction lives, not from refusing to do it:
//!
//!   * 'd' sets a MARK. The editor never touches a disk; deletion is an
//!     `sgdisk -d` in the plan, run with every other destructive step after the
//!     final confirmation. Until then it is a toggle, so a user who reaches the
//!     summary and changes their mind can walk back and restore the partition.
//!   * the disk a deletion targets comes from the SCAN (`Partition.parent`),
//!     never from parsing the path — "/dev/nvme0n1" is a whole disk yet ends in
//!     a digit, so name alone cannot tell disk from partition. The plan adds a
//!     round-trip check (`part(disk, n) == path`) on top.
//!   * partitions of the live medium are refused outright.
//!   * new partitions are still numbered above the highest number in the scan,
//!     deletions included, so a freed number is never reused.
//!
//! `validate()` stays a pure function so every rule is unit-tested without a
//! disk in sight.

use crate::app::{App, FsOptsTarget, InstallConfig, MANUAL_REST};
use crate::i18n::t;
use crate::screens::nav;
use crate::system::disk::{self, Partition};
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};

/// Filesystems offered for root and /home — the SAME set, in the same order, as
/// the auto disk screen (`disk::FS_LIST`) and the extra-disks screen.
///
/// This used to be just the first three. That was a UI-only limit with nothing
/// behind it: `push_mkfs` is shared with the auto plan and already handles all
/// seven, the sanitiser copies `manual_root_fs` into `root_fs` untouched, and
/// the fs tooling (btrfs-progs / xfsprogs / f2fs-tools / jfsutils) is picked off
/// that same field — so manual installs could always have used any of them.
/// A user who picks f2fs for a whole-disk install had no way to pick it for a
/// dual-boot one, for no reason they could see.
pub(crate) const FS_CHOICES: [&str; 7] = ["ext4", "btrfs", "xfs", "f2fs", "jfs", "ext3", "ext2"];

/// Roles, in display/creation order. The index doubles as the modal cursor.
const ROLE_ESP: usize = 0;
const ROLE_ROOT: usize = 1;
const ROLE_SWAP: usize = 2;
const ROLE_HOME: usize = 3;
/// The 1 MiB type-EF02 partition GRUB needs to boot BIOS from a GPT disk.
/// Offered only in legacy mode, and only on GPT — see `bios_boot_required`.
const ROLE_BIOS: usize = 4;
/// Plain data storage: the partition is mounted somewhere of the user's
/// choosing and otherwise left alone. Unlike the other roles this one is not a
/// single slot — any number of partitions can be data — so it is kept in
/// `extra_disks`, the same list the automatic path's "extra disks" step fills.
const ROLE_DATA: usize = 5;
const ROLES: usize = 6;

/// Ready-made mountpoints offered for a data partition, plus a free-form entry.
const MOUNT_TEMPLATES: &[&str] = &["/mnt/data", "/mnt/storage", "/data", "/srv"];

/// One navigable row of the editor. Built fresh from the current state by
/// `build_rows` so draw and the key handler always agree on what the cursor
/// points at. Every disk is shown at once (grouped under its header), so a
/// second disk can host /home with nothing to "switch" and nothing lost.
#[derive(Clone)]
enum Row {
    /// Firmware choice — UEFI or legacy BIOS. First row, because it decides
    /// which boot partition the rest of the layout needs.
    BootMode,
    DiskHeader {
        disk: String,
    },
    Part {
        path: String,
        is_new: bool,
    },
    Create {
        disk: String,
    },
    Continue,
}

/// The effective root filesystem ("ext4" until the user picks one).
fn root_fs_eff(c: &InstallConfig) -> &str {
    if c.manual_root_fs.is_empty() {
        "ext4"
    } else {
        c.manual_root_fs.as_str()
    }
}

/// A partition the editor is going to create.
#[derive(Clone)]
struct Planned {
    path: String,
    mib: u32,
    /// The disk it is carved from — recorded per slot, so root can be planned
    /// on one drive and a fresh /home on another.
    disk: String,
}

/// Every planned NEW partition, in creation order per disk (rest-of-disk last,
/// matching what the plan does).
fn planned_slots(c: &InstallConfig) -> Vec<Planned> {
    let mut v: Vec<Planned> = [
        (
            &c.manual_esp,
            c.manual_esp_new_mib,
            ROLE_ESP,
            &c.manual_esp_disk,
        ),
        (
            &c.manual_root,
            c.manual_root_new_mib,
            ROLE_ROOT,
            &c.manual_root_disk,
        ),
        (
            &c.manual_swap,
            c.manual_swap_new_mib,
            ROLE_SWAP,
            &c.manual_swap_disk,
        ),
        (
            &c.manual_home,
            c.manual_home_new_mib,
            ROLE_HOME,
            &c.manual_home_disk,
        ),
        (
            &c.manual_bios,
            c.manual_bios_new_mib,
            ROLE_BIOS,
            &c.manual_bios_disk,
        ),
    ]
    .into_iter()
    .filter(|(dev, mib, _, _)| *mib > 0 && !dev.is_empty())
    .map(|(dev, mib, _role, disk)| Planned {
        path: dev.clone(),
        mib,
        disk: disk.clone(),
    })
    .collect();
    // Data-storage partitions join the plan on equal terms: they occupy space,
    // get numbered, and are created by the same sgdisk pass.
    for dp in &c.manual_data_new {
        if dp.mib > 0 && !dp.path.is_empty() {
            v.push(Planned {
                path: dp.path.clone(),
                mib: dp.mib,
                disk: dp.disk.clone(),
            });
        }
    }
    v.sort_by_key(|p| {
        (
            p.disk.clone(),
            p.mib == MANUAL_REST,
            disk::part_number(&p.disk, &p.path).unwrap_or(u32::MAX),
        )
    });
    v
}

/// Which disk a role's planned partition sits on.
fn planned_disk(c: &InstallConfig, role: usize) -> String {
    match role {
        ROLE_ESP => c.manual_esp_disk.clone(),
        ROLE_ROOT => c.manual_root_disk.clone(),
        ROLE_SWAP => c.manual_swap_disk.clone(),
        ROLE_BIOS => c.manual_bios_disk.clone(),
        _ => c.manual_home_disk.clone(),
    }
}

fn set_planned_disk(c: &mut InstallConfig, role: usize, disk: &str) {
    let slot = match role {
        ROLE_ESP => &mut c.manual_esp_disk,
        ROLE_ROOT => &mut c.manual_root_disk,
        ROLE_SWAP => &mut c.manual_swap_disk,
        ROLE_BIOS => &mut c.manual_bios_disk,
        _ => &mut c.manual_home_disk,
    };
    *slot = disk.to_string();
}

/// Which role a device path is currently assigned to.
fn role_of(c: &InstallConfig, dev: &str) -> Option<usize> {
    if c.manual_esp == dev {
        Some(ROLE_ESP)
    } else if c.manual_root == dev {
        Some(ROLE_ROOT)
    } else if c.manual_swap == dev {
        Some(ROLE_SWAP)
    } else if c.manual_home == dev {
        Some(ROLE_HOME)
    } else if c.manual_bios == dev {
        Some(ROLE_BIOS)
    } else if data_mount(c, dev).is_some() {
        Some(ROLE_DATA)
    } else {
        None
    }
}

/// Does this partition live on the disk the installer booted from?
///
/// lsblk marks the medium, and `disks()` carries the flag. Deleting a partition
/// of the live disk would pull the floor out from under a running install, so
/// the editor refuses it outright rather than trusting the user to notice.
fn partition_is_live(app: &App, dev: &str) -> bool {
    let Some(parent) = app
        .parts_list
        .iter()
        .find(|p| p.path == dev)
        .map(|p| p.parent.clone())
    else {
        return false;
    };
    crate::screens::disk::disks()
        .iter()
        .any(|d| d.path == parent && d.is_live)
}

/// Is this existing partition marked for deletion?
fn is_deleted(c: &InstallConfig, dev: &str) -> bool {
    c.manual_deleted.iter().any(|p| p.path == dev)
}

/// Toggle the deletion mark on an EXISTING partition.
///
/// Marking also drops any role it held: the partition is going away, so it
/// cannot also be the ESP or the root. Nothing is destroyed here — the mark
/// becomes an `sgdisk -d` in the plan, which only runs after the final
/// confirmation, so this stays reversible right up to that point.
fn toggle_deleted(app: &mut App, dev: &str) {
    let c = &mut app.config;
    if let Some(i) = c.manual_deleted.iter().position(|p| p.path == dev) {
        c.manual_deleted.remove(i);
        return;
    }
    // Disk AND identity come from the SCAN, never from parsing the path.
    let Some((parent, partuuid)) = app
        .parts_list
        .iter()
        .find(|p| p.path == dev)
        .map(|p| (p.parent.clone(), p.partuuid.clone()))
    else {
        return;
    };
    let c = &mut app.config;
    c.manual_deleted.push(crate::app::DeletedPart {
        path: dev.to_string(),
        disk: parent,
        partuuid,
    });
    for (slot, mib) in [
        (&mut c.manual_esp, &mut c.manual_esp_new_mib),
        (&mut c.manual_root, &mut c.manual_root_new_mib),
        (&mut c.manual_swap, &mut c.manual_swap_new_mib),
        (&mut c.manual_home, &mut c.manual_home_new_mib),
        (&mut c.manual_bios, &mut c.manual_bios_new_mib),
    ] {
        if slot == dev {
            slot.clear();
            *mib = 0;
        }
    }
    if c.manual_esp.is_empty() {
        c.manual_esp_format = false;
    }
    renumber_planned(app);
    refresh_boot_disk(app);
}

/// The mountpoint this partition is used at as plain data storage, if any.
fn data_mount(c: &InstallConfig, dev: &str) -> Option<String> {
    c.extra_disks
        .iter()
        .find(|e| e.disk == dev && !e.mountpoint.is_empty())
        .map(|e| e.mountpoint.clone())
}

/// Point `dev` at `mountpoint` as plain storage.
///
/// Stored in `extra_disks` so the existing plan does the work — mkfs when it is
/// needed, mkdir, mount, fstab. An EXISTING filesystem is kept (mounted as-is):
/// this role is "use what is on it", and silently reformatting somebody's data
/// partition because they gave it a folder would be the worst kind of surprise.
/// A partition with no filesystem — including one the editor is about to
/// create — has to be formatted, so it is.
fn set_data_mount(app: &mut App, dev: &str, mountpoint: &str) {
    let existing_fs = app
        .parts_list
        .iter()
        .find(|p| p.path == dev)
        .map(|p| p.fstype.clone())
        .unwrap_or_default();
    let format = existing_fs.is_empty();
    // Changing the FOLDER must not silently undo the FILESYSTEM: this rebuilds
    // the entry from scratch, so a target being formatted carries over whatever
    // ←/→ and the options modal already chose for it. A preserved partition has
    // nothing to carry — its filesystem is whatever is on the disk.
    let prev = app.config.extra_disks.iter().find(|e| e.disk == dev);
    let (fs, compress) = if format {
        match prev.filter(|e| e.format && !e.fs.is_empty()) {
            Some(e) => (e.fs.clone(), e.compress),
            None => ("ext4".to_string(), false),
        }
    } else {
        (existing_fs, false)
    };
    app.config.extra_disks.retain(|e| e.disk != dev);
    app.config.extra_disks.push(crate::app::ExtraDisk {
        disk: dev.to_string(),
        mountpoint: mountpoint.to_string(),
        fs,
        format,
        compress,
        whole_disk: false,
        ..Default::default()
    });
}

/// Is this data partition set to be LUKS-encrypted?
/// May the manual /home be encrypted on a disk SHARED with another OS?
///
/// Only when /home sits on a disk of its own. In dual boot the root disk is
/// shared and stays untouched by LUKS (that restriction is deliberate and
/// unchanged), but a second drive that holds nothing except the /home about to
/// be created is entirely ours, and refusing to encrypt it protects nobody.
///
/// "Nothing except" is checked against the SCAN, not against intent: a disk
/// carrying somebody else's partitions is not ours to encrypt, even if the
/// only thing we plan to write there is one new /home. Being wrong in that
/// direction destroys data that was never ours.
pub(crate) fn home_disk_is_exclusively_ours(app: &App) -> bool {
    let c = &app.config;
    let home_disk = if !c.manual_home_disk.is_empty() {
        c.manual_home_disk.clone()
    } else {
        return false;
    };
    // The root's disk is the shared one by definition here.
    let root_disk = planned_disk_of(c, ROLE_ROOT, &c.manual_root);
    if home_disk.is_empty() || home_disk == root_disk {
        return false;
    }
    // Any partition already on that disk that we are not deleting means the
    // drive belongs to somebody as well as us.
    !app.parts_list
        .iter()
        .any(|p| p.parent == home_disk && !c.manual_deleted.iter().any(|d| d.path == p.path))
}

fn data_is_encrypted(c: &InstallConfig, dev: &str) -> bool {
    c.extra_disks
        .iter()
        .any(|e| e.disk == dev && e.encrypt && e.format)
}

/// Turn encryption on or off for a data partition.
///
/// Only for one being FORMATTED: encrypting a partition whose contents are being
/// kept means destroying those contents, and keeping them is the reason it is
/// not being formatted. Returns whether anything changed, so the caller can say
/// why when it did not.
///
/// The plan side of this already existed and is exercised by the extra-disks
/// step — `ExtraDisk::encrypt` carries it through mkfs, crypttab and the keyfile
/// that unlocks it at boot. What was missing was any way to ASK for it from the
/// manual editor, and once the extra-disks step stopped appearing in manual mode
/// there was no way to reach it at all.
pub(crate) fn toggle_data_encryption(c: &mut InstallConfig, dev: &str) -> bool {
    if let Some(e) = c.extra_disks.iter_mut().find(|e| e.disk == dev) {
        if e.format {
            e.encrypt = !e.encrypt;
            return true;
        }
    }
    false
}

fn clear_data_mount(c: &mut InstallConfig, dev: &str) {
    c.extra_disks.retain(|e| e.disk != dev);
}

fn role_label(app: &App, role: usize) -> String {
    let key = match role {
        ROLE_ESP => "parts.esp",
        ROLE_ROOT => "parts.root",
        ROLE_SWAP => "parts.swap",
        ROLE_BIOS => "parts.biosboot",
        ROLE_DATA => "parts.data",
        _ => "parts.home",
    };
    t(app.lang, key)
}

fn build_rows(app: &App) -> Vec<Row> {
    let mut rows = vec![Row::BootMode];
    // Every disk, in order, with its existing partitions and (on the create
    // disk) its planned new partitions, then a per-disk "+ Create" row.
    for d in crate::screens::disk::disks() {
        rows.push(Row::DiskHeader {
            disk: d.path.clone(),
        });
        let mut existing: Vec<&Partition> = app
            .parts_list
            .iter()
            .filter(|p| p.parent == d.path)
            .collect();
        existing.sort_by_key(|p| disk::part_number(&d.path, &p.path).unwrap_or(u32::MAX));
        for p in existing {
            // A partition marked for deletion is NOT part of the layout any
            // more: its space is free and a new partition can be planned into
            // it. It reappears below, under "removed", so the deletion stays
            // reversible without pretending the partition is still there.
            if is_deleted(&app.config, &p.path) {
                continue;
            }
            rows.push(Row::Part {
                path: p.path.clone(),
                is_new: false,
            });
        }
        for p in planned_slots(&app.config)
            .into_iter()
            .filter(|p| p.disk == d.path)
        {
            rows.push(Row::Part {
                path: p.path,
                is_new: true,
            });
        }
        rows.push(Row::Create {
            disk: d.path.clone(),
        });
    }
    rows.push(Row::Continue);
    rows
}

/// Best-effort free space on `disk`: total − existing partitions − any planned
/// fixed-size slots on it − a little table overhead. 0 = unknown.
/// Space a "rest of the disk" slot on `disk` would actually receive: the disk
/// minus its surviving partitions, minus the fixed-size slots planned there.
///
/// Separate from `free_bytes` because the two answer different questions. This
/// one is "how big will that partition be" — which the editor prints next to it,
/// since "rest of the disk" tells the user nothing about whether their 50 GiB
/// drive was really used.
pub(crate) fn rest_bytes(app: &App, disk: &str) -> u64 {
    let total = crate::screens::disk::disks()
        .iter()
        .find(|x| x.path == disk)
        .map(|x| x.size_bytes)
        .unwrap_or(0);
    if total == 0 {
        return 0;
    }
    // A partition marked for deletion is space the user can plan into: the
    // plan removes it before it creates anything.
    let used: u64 = app
        .parts_list
        .iter()
        .filter(|p| p.parent == disk && !is_deleted(&app.config, &p.path))
        .map(|p| p.size_bytes)
        .sum();
    let planned: u64 = planned_slots(&app.config)
        .iter()
        .filter(|p| p.disk == disk && p.mib != MANUAL_REST)
        .map(|p| p.mib as u64 * 1024 * 1024)
        .sum();
    total
        .saturating_sub(used)
        .saturating_sub(planned)
        .saturating_sub(2 * 1024 * 1024)
}

/// The planned slot on `disk` that takes whatever is left, if there is one.
///
/// It is the reason a disk can be full while the screen shows plain numbers:
/// such a slot's size is RESOLVED for display, not fixed, so anything else
/// planned on the same disk comes out of it — and used to do so in silence.
/// Reported from QEMU: creating a 4 GiB swap turned a 29 GiB root into 25 GiB
/// with nothing said, and the row next to it already read "space is allocated".
fn rest_slot_on(c: &InstallConfig, disk: &str) -> Option<String> {
    planned_slots(c)
        .into_iter()
        .find(|p| p.disk == disk && p.mib == MANUAL_REST)
        .map(|p| p.path)
}

/// "Root (/) (/dev/nvme0n1p2)" — which slot the user has to go and shrink.
/// A refusal that does not name it just moves the guessing somewhere else.
fn slot_name(app: &App, dev: &str) -> String {
    match role_of(&app.config, dev) {
        Some(role) => format!("{} ({dev})", role_label(app, role)),
        None => dev.to_string(),
    }
}

/// How much the size box may spend: what is free on the disk, PLUS whatever
/// the slot being resized already holds (`own_mib`, zero when creating).
///
/// RESIZING GIVES ITS OWN SPACE BACK FIRST. `free_bytes` counts every planned
/// slot as spent — including the very one being edited — so a 15 GiB root on a
/// 30 GiB disk with 10 GiB left was refused 20 GiB, although 25 GiB were in
/// fact available to it. The refusal even printed the free figure, which the
/// user could see on screen and could see was not the whole story. The old size
/// is occupied by nothing but the number about to be overwritten.
///
/// Zero still means "no limit known" and skips the check, so a rest-of-disk
/// slot (`MANUAL_REST`, a sentinel, not a size) is never added in.
fn size_budget_mib(free_mib: u32, own_mib: u32) -> u32 {
    if own_mib == MANUAL_REST {
        return free_mib;
    }
    free_mib.saturating_add(own_mib)
}

/// Space still available to plan INTO on `disk`.
///
/// Zero once a rest-of-disk slot is planned there: it claims whatever is left,
/// so there is nothing more to give. Reporting the raw remainder made a disk
/// look untouched right after a partition had been spread across all of it.
/// A size in MiB as the table shows it: whole GiB above a gigabyte, MiB below.
/// Used for refusals, where the number has to read the same way it was typed.
fn human_mib(mib: u32) -> String {
    if mib >= 1024 && mib.is_multiple_of(1024) {
        format!("{} GiB", mib / 1024)
    } else if mib >= 1024 {
        format!("{:.1} GiB", mib as f64 / 1024.0)
    } else {
        format!("{mib} MiB")
    }
}

fn free_bytes(app: &App, disk: &str) -> u64 {
    if planned_slots(&app.config)
        .iter()
        .any(|p| p.disk == disk && p.mib == MANUAL_REST)
    {
        return 0;
    }
    rest_bytes(app, disk)
}

// ── drawing ─────────────────────────────────────────────────────────────────

/// A titled rounded section box; accent border + title when its rows hold the
/// cursor, dim otherwise. Shared shape with the alongside and auto disk faces.
fn section_box(title: &str, focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            theme::border()
        } else {
            theme::border_dim()
        })
        .title(format!(" {title} "))
        .title_style(if focused {
            theme::title()
        } else {
            theme::dim()
        })
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    // Lazy scan: first draw after entering (or after 'r') fetches the list.
    // lsblk reads sysfs — milliseconds — so this stays synchronous on purpose.
    if !app.parts_scanned {
        app.parts_list = disk::list_partitions_all().unwrap_or_default();
        app.parts_scanned = true;
        app.parts_modal_cursor = 0;
    }

    let verdict = validate_app(app);
    app.can_advance = verdict.is_ok();

    let rows = build_rows(app);
    let cursor = app.cursor.min(rows.len().saturating_sub(1));
    let mark = |row: usize| if cursor == row { "> " } else { "  " };
    let style = |row: usize| -> Style {
        if cursor == row {
            theme::selected()
        } else {
            theme::normal()
        }
    };

    // Narrow panel (an 80x24 console leaves the content panel ~60 cols wide):
    // trim wide row content so nothing gets clipped off the right edge.
    let compact = area.width < 78;

    // Boxed sections: a one-line instruction, the multi-disk Partitions table,
    // and a footer box (status line + Continue).
    //
    // The footer used to carry a "Root filesystem options" row as well. It was
    // redundant: the root and /home rows already show an "[o] Options" cue the
    // moment btrfs is selected, and that cue opens the very same modal — right
    // where the filesystem was chosen, instead of at the far end of the screen.
    // The firmware choice gets its OWN section. It sat as the first line inside
    // the partitions box, where it read as an odd entry in the disk list —
    // tester's words: "I didn't understand what that row was, or why I wasn't
    // going straight to the disks". It isn't a partition, so it shouldn't live
    // among them. On a short panel it drops its frame to a single labelled line
    // rather than eating three rows the disk table needs more.
    let fw_h = if area.height < 18 { 1 } else { 3 };
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),    // instruction (wraps to ≤2 lines)
            Constraint::Length(fw_h), // firmware
            Constraint::Min(4),       // partitions table (all disks)
            Constraint::Length(4),    // status + continue
        ])
        .split(area);

    // Instruction line.
    f.render_widget(
        Paragraph::new(Span::styled(
            t(app.lang, "parts.hint_external"),
            theme::dim(),
        ))
        .wrap(Wrap { trim: true }),
        sections[0],
    );

    // ── Firmware section (its own box; not a partition) ──
    {
        let i = rows
            .iter()
            .position(|r| matches!(r, Row::BootMode))
            .unwrap_or(0);
        let focused = cursor == i;
        let uefi = app.config.boot_mode.is_uefi();
        let cur = if uefi {
            t(app.lang, "parts.boot_uefi")
        } else {
            t(app.lang, "parts.boot_bios")
        };
        let opts = [
            t(app.lang, "parts.boot_uefi"),
            t(app.lang, "parts.boot_bios"),
        ];
        let refs: Vec<&str> = opts.iter().map(|x| x.as_str()).collect();
        let mut spans = vec![Span::styled(
            if focused { " \u{203a} " } else { "   " }.to_string(),
            theme::accent(),
        )];
        spans.extend(option_strip(&refs, &cur));
        if fw_h == 1 {
            // No room for a frame: label the line instead, so it still reads as
            // a setting rather than a stray list entry.
            let mut line = vec![Span::styled(
                format!(" {}  ", t(app.lang, "parts.boot_label")),
                if focused {
                    theme::title()
                } else {
                    theme::dim()
                },
            )];
            line.extend(spans.into_iter().skip(1));
            f.render_widget(Paragraph::new(Line::from(line)), sections[1]);
        } else {
            let block = section_box(&t(app.lang, "parts.boot_label"), focused);
            let inner = block.inner(sections[1]);
            f.render_widget(block, sections[1]);
            f.render_widget(Paragraph::new(Line::from(spans)), inner);
        }
    }

    // ── Partitions table box (every disk, grouped under its header) ──
    {
        let focused = matches!(
            rows.get(cursor),
            Some(Row::Part { .. } | Row::Create { .. } | Row::DiskHeader { .. })
        );
        let block = section_box(&t(app.lang, "parts.tbl_title"), focused);
        let inner = block.inner(sections[2]);
        f.render_widget(block, sections[2]);
        let mut lines: Vec<Line> = Vec::new();
        // The line index where the focused table row starts, so the view can
        // follow the cursor when there are more disks than fit (20+ disks).
        let mut cursor_line: Option<usize> = None;
        for (i, row) in rows.iter().enumerate() {
            if i == cursor {
                cursor_line = Some(lines.len());
            }
            match row {
                Row::DiskHeader { disk } => {
                    // On a narrow panel drop the model — path + size + the
                    // [system] tag are what matter and the model overflows.
                    let dd = crate::screens::disk::disks()
                        .iter()
                        .find(|x| &x.path == disk)
                        .map(|x| {
                            // Formatted from the byte count, not lsblk's own
                            // "30G" string: the units have to read the same here
                            // as in every size the editor prints beside them.
                            let sz = disk::human_size_fmt(x.size_bytes, !compact);
                            if compact {
                                format!("{}  {}", x.path, sz)
                            } else {
                                format!("{}  {}  {}", x.path, sz, x.model)
                            }
                        })
                        .unwrap_or_else(|| disk.clone());
                    let hd = if cursor == i {
                        theme::selected()
                    } else {
                        theme::heading()
                    };
                    let mut spans = vec![Span::styled(format!("{}# {dd}", mark(i)), hd)];
                    // The whole-disk WIPE selector rides on the header (solo
                    // only): the full reveal strip when the header is focused, a
                    // compact ⚠ badge when it is marked but not. In dual-boot the
                    // header is a plain caption — no wipe there.
                    let focused_here = cursor == i;
                    let idx = current_wipe_index(app, disk);
                    let (rota, _) = disk_hw(disk);
                    let offered = wipe_methods_for(rota);
                    let wipeable = wipe_refusal(disk).is_none();
                    if wipeable {
                        if focused_here {
                            let opts = wipe_strip_labels(app, disk);
                            let refs: Vec<&str> = opts.iter().map(|s| s.as_str()).collect();
                            let cur = refs[idx.min(refs.len() - 1)];
                            spans.push(Span::styled("   ".to_string(), theme::mute()));
                            let used = spans_width(&spans);
                            let budget = (inner.width as usize).saturating_sub(used + 2);
                            spans.extend(option_strip_fit(&refs, cur, budget));
                        } else if idx > 0 {
                            spans.push(Span::styled(
                                format!(
                                    "   [!] \u{2039}{}\u{203a}",
                                    wipe_strip_labels(app, disk)[idx]
                                ),
                                theme::warn(),
                            ));
                        }
                    }
                    lines.push(Line::from(spans));
                    // Under a focused header: what the current selection means,
                    // with the HDD/SSD note, plus the ←/→ · Enter hint.
                    if focused_here && wipeable {
                        let desc = if idx == 0 || idx > offered.len() {
                            t(app.lang, "parts.wipe_none_desc")
                        } else {
                            wipe_method_desc(app, offered[idx - 1])
                        };
                        lines.push(Line::from(Span::styled(
                            format!("      {desc}"),
                            theme::mute(),
                        )));
                        lines.push(Line::from(Span::styled(
                            format!("      {}", t(app.lang, "parts.wipe_hint_strip")),
                            theme::dim(),
                        )));
                    }
                    // A disk that carries ANOTHER system — a bootloader or a
                    // Windows volume — says so the moment a wipe is selected on
                    // it. Losing a neighbouring OS must never be a surprise, and
                    // the badge alone does not say what is about to be lost.
                    if idx > 0 && wipeable {
                        let foreign = foreign_parts(app, disk);
                        if !foreign.is_empty() {
                            lines.push(Line::from(Span::styled(
                                format!("      [!] {}", t(app.lang, "parts.wipe_foreign_warn")),
                                theme::warn(),
                            )));
                            if focused_here {
                                for (path, what) in &foreign {
                                    lines.push(Line::from(Span::styled(
                                        format!("         {path} — {what}"),
                                        theme::warn(),
                                    )));
                                }
                            }
                        }
                    }
                }
                Row::Part { path, is_new } => lines.push(part_line(
                    app,
                    path,
                    *is_new,
                    RowStyle {
                        mark: mark(i),
                        st: style(i),
                        focused: cursor == i,
                        compact,
                        width: inner.width as usize,
                    },
                )),
                Row::Create { disk } => {
                    let free = free_bytes(app, disk);
                    // Say WHY there is nothing to create rather than dropping
                    // the note: a silent row next to a disk the user just
                    // claimed reads as though nothing happened.
                    let free_disp = if free > 0 {
                        format!(
                            "   ({} {})",
                            t(app.lang, "parts.free"),
                            disk::human_size(free)
                        )
                    } else if planned_slots(&app.config).iter().any(|p| p.disk == *disk) {
                        format!("   ({})", t(app.lang, "parts.free_claimed"))
                    } else {
                        String::new()
                    };
                    lines.push(Line::from(Span::styled(
                        format!("{}  + {}{free_disp}", mark(i), t(app.lang, "parts.create")),
                        if cursor == i {
                            theme::selected()
                        } else {
                            theme::dim()
                        },
                    )));
                    lines.push(Line::from(""));
                }
                _ => {}
            }
        }
        // Scroll so the focused row stays visible (roughly centered), clamped to
        // the real bottom. When the cursor is on a footer row (fs-options /
        // Continue), keep the table showing its end. No wrap: one Line per row
        // keeps the scroll math exact, and wide rows truncate at the edge.
        let view_h = inner.height as usize;
        let max_scroll = lines.len().saturating_sub(view_h);
        let scroll = match cursor_line {
            Some(cl) => cl.saturating_sub(view_h / 2).min(max_scroll),
            None => max_scroll,
        } as u16;
        f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
    }

    // ── Footer box: fs-options row, status, Continue ──
    {
        let focused = matches!(rows.get(cursor), Some(Row::Continue));
        let block = section_box(&t(app.lang, "parts.foot_title"), focused);
        let inner = block.inner(sections[3]);
        f.render_widget(block, sections[3]);
        let mut lines: Vec<Line> = Vec::new();
        // Status line: a transient action message (e.g. "new partitions only
        // on one disk") wins; otherwise the validation verdict (ready ✓ / the
        // first broken rule ⚠).
        if !app.pmode_status.is_empty() {
            lines.push(Line::from(Span::styled(
                format!(" [!] {}", app.pmode_status),
                theme::warn(),
            )));
        } else {
            match &verdict {
                Ok(()) => lines.push(Line::from(Span::styled(
                    format!(" x {}", t(app.lang, "parts.st_ready")),
                    theme::ok(),
                ))),
                Err((key, dev)) => lines.push(Line::from(Span::styled(
                    format!(" [!] {}", t(app.lang, key).replace("{dev}", dev)),
                    theme::warn(),
                ))),
            }
        }
        // Continue — styled like every other action button: bright bold accent
        // with the [Enter] tag when focused. It used to take `ok()` (plain
        // Cyan) on focus, the very colour of the ✓ status line above it, so the
        // one control the user must find looked like a dimmed caption.
        if let Some(i) = rows.iter().position(|r| matches!(r, Row::Continue)) {
            let label = if cursor == i {
                format!("{}[Enter] {}", mark(i), t(app.lang, "parts.continue"))
            } else {
                format!("{}{}", mark(i), t(app.lang, "parts.continue"))
            };
            lines.push(Line::from(Span::styled(
                label,
                if !app.can_advance {
                    theme::mute()
                } else if cursor == i {
                    theme::gold()
                } else {
                    theme::normal()
                },
            )));
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
    }

    if app.fs_opts_modal_open {
        let fs = fs_for_target(app, app.fs_opts_target);
        crate::screens::disk::draw_fs_opts_modal(f, app, area, &fs);
    }
    if app.parts_modal_open {
        draw_role_modal(f, app);
    }
    if app.parts_create_open {
        draw_create_modal(f, app);
    }
    if app.parts_mount_open {
        draw_mount_modal(f, app);
    }
    if app.parts_wipe_ack_open {
        draw_wipe_ack_modal(f, app);
    }
    if app.parts_wipe_open {
        draw_wipe_modal(f, app);
    }
    // The "now" wipe overlay sits on top of everything, running or finished.
    if app.wipe_run_rx.is_some() || app.wipe_run_done.is_some() {
        draw_wipe_run_overlay(f, app);
    }
}

/// Columns a span list occupies.
fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// A reveal strip that FITS the space it's given.
///
/// The full filesystem list is seven entries — about 47 columns with the ` · `
/// separators — which overruns the row on anything but a very wide terminal.
/// Rather than let ratatui slice it mid-word at the panel edge (or hide it
/// behind a magic width threshold), drop the entries FARTHEST from the selected
/// one until what's left fits. The selected option is always kept, so the strip
/// degrades from "all seven" through "a few neighbours" down to "just the
/// value" as the terminal narrows, and ←/→ still cycle the whole list.
pub(crate) fn option_strip_fit(
    options: &[&str],
    current: &str,
    budget: usize,
) -> Vec<Span<'static>> {
    let sel = options.iter().position(|o| *o == current).unwrap_or(0);
    let (mut lo, mut hi) = (0usize, options.len());
    loop {
        let strip = option_strip(&options[lo..hi], current);
        if hi - lo <= 1 || spans_width(&strip) <= budget {
            return strip;
        }
        // Trim whichever end is farther from the selection, so the neighbours
        // that survive longest are the ones ←/→ reaches first.
        if sel - lo >= hi - 1 - sel {
            lo += 1;
        } else {
            hi -= 1;
        }
    }
}

/// A "reveal" strip of choices: every option shown, the selected one wrapped in
/// ‹ › and highlighted, the rest dim — so a ←/→ toggle never hides its options.
fn option_strip(options: &[&str], current: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (k, o) in options.iter().enumerate() {
        if k > 0 {
            spans.push(Span::styled(" \u{00b7} ".to_string(), theme::mute()));
        }
        if *o == current {
            spans.push(Span::styled(format!("\u{2039}{o}\u{203a}"), theme::gold()));
        } else {
            spans.push(Span::styled((*o).to_string(), theme::mute()));
        }
    }
    spans
}

/// The filesystem strip for a root/home row: the ext4/btrfs/xfs reveal, plus a
/// bright "[o] Options" flag when btrfs is selected (so its subvolume/snapshot/
/// compression options aren't missed). Focused rows show the whole strip;
/// unfocused ones just the value (still with the btrfs flag). On a NARROW panel
/// (`compact`) even a focused row shows only the value — the strip wouldn't fit
/// — and the flag shrinks to "[o]"; ←/→ (per the footer hint) still cycle it.
fn fs_spans(
    spans: &mut Vec<Span<'static>>,
    app: &App,
    fs: &str,
    focused: bool,
    compact: bool,
    width: usize,
) {
    // Reserve the [o] cue before measuring, so the strip never grows into the
    // space the cue is about to need.
    let cue = if fs == "btrfs" {
        Some(if compact {
            "  [o]".to_string()
        } else {
            format!("   {}", t(app.lang, "parts.opts_cue"))
        })
    } else {
        None
    };
    let reserved = cue.as_deref().map(|c| c.chars().count()).unwrap_or(0);

    if focused && !compact {
        let budget = width
            .saturating_sub(spans_width(spans))
            .saturating_sub(reserved + 1);
        spans.extend(option_strip_fit(&FS_CHOICES, fs, budget));
    } else {
        spans.push(Span::styled(fs.to_string(), theme::accent()));
    }
    if let Some(c) = cue {
        spans.push(Span::styled(c, theme::title()));
    }
}

/// One partition table row: path, size, detected/target fs, role and fate.
/// `compact` (a narrow panel, e.g. an 80x24 console) trims the row: an assigned
/// partition drops its middle "current fs / label" column (the role + fate says
/// what matters), toggles collapse to single values, and the btrfs flag shrinks.
/// How one row is drawn, as opposed to what it says: the cursor mark, its
/// style, and how much room the panel is giving it. Grouped because these five
/// always travel together and always come from the same place — the loop that
/// knows the row's index and the panel's width.
struct RowStyle<'a> {
    mark: &'a str,
    st: Style,
    focused: bool,
    compact: bool,
    width: usize,
}

fn part_line(app: &App, path: &str, is_new: bool, sty: RowStyle) -> Line<'static> {
    let RowStyle {
        mark,
        st,
        focused,
        compact,
        width,
    } = sty;
    let c = &app.config;
    let role = role_of(c, path);
    // Left half: what the partition IS. Existing partitions show their real
    // size + filesystem (+ label in quotes when set — "Windows"/"SYSTEM" is
    // how a human tells a dual-boot volume apart); planned ones show their
    // target size and a "new" tag instead of a scanned fstype.
    //
    // Whether this row is planned comes from the ROW, never from a scan lookup.
    // A planned partition deliberately reuses the number of one being deleted,
    // so its path is still in `parts_list` carrying the OLD partition's size:
    // deciding by lookup printed 20G and 30G for two freshly planned 10 GiB
    // slots, while the free-space note and the install itself used the correct
    // figures. The row knows; ask it.
    let scanned = if is_new {
        None
    } else {
        app.parts_list.iter().find(|p| p.path == path)
    };
    let (size, descr) = match scanned {
        Some(p) => {
            let mut d = if p.fstype.is_empty() {
                t(app.lang, "parts.unformatted")
            } else {
                p.fstype.clone()
            };
            if !p.label.is_empty() {
                d = format!("{d} \u{201c}{}\u{201d}", p.label);
            }
            // From the byte count, not lsblk's own string: a row that reads
            // "512M" beside a planned one reading "512 MiB" makes two units out
            // of one, and the reader has to work out whether they mean the same.
            (disk::human_size_fmt(p.size_bytes, !compact), d)
        }
        None => {
            // Always a concrete size, whatever the scenario. A rest-of-disk
            // slot used to read "rest of the disk", which says nothing about
            // whether the 50 GiB drive was actually used — so it is resolved to
            // the bytes it will really get. Only an unknown disk size (no
            // lsblk figure) falls back to the words.
            let slot = planned_slots(c).into_iter().find(|p| p.path == path);
            let size = match &slot {
                Some(p) if p.mib == MANUAL_REST => {
                    let b = rest_bytes(app, &p.disk);
                    if b > 0 {
                        disk::human_size_fmt(b, !compact)
                    } else {
                        t(app.lang, "parts.size_rest")
                    }
                }
                Some(p) => disk::human_size_fmt(p.mib as u64 * 1024 * 1024, !compact),
                None => t(app.lang, "parts.size_rest"),
            };
            (size, t(app.lang, "parts.new_mark"))
        }
    };
    // Left half: "▸ /dev/sda1      512M  vfat “SYSTEM”". On a narrow panel, an
    // ASSIGNED partition drops the descr column to make room for its fate — an
    // UNASSIGNED one keeps it (its label is how you spot the Windows volume).
    // Spelled-out units need three more columns than "512M" did.
    let szw = if compact { 8 } else { 11 };
    let drop_descr = compact && role.is_some();
    let left = if drop_descr {
        format!("{mark}{path:<15} {size:>szw$}")
    } else {
        format!("{mark}{path:<15} {size:>szw$}  {descr}")
    };
    let mut spans = vec![Span::styled(left, st)];

    // Right half: what the partition BECOMES. Toggle values are shown as a
    // whole "reveal" strip — every choice visible, the selected one in ‹ › — so
    // there's no hidden ←/→ state. FOCUSED rows show the full strip; unfocused
    // ones stay compact. A btrfs root/home also flags "[o] Options" so the extra
    // configuration (subvolumes/snapshots/compression) is never missed.
    match role {
        Some(ROLE_ESP) => {
            let mp = if c.manual_esp_mount == "/boot/efi" {
                "/boot/efi"
            } else {
                "/boot"
            };
            let fate = if c.manual_esp_format {
                "FAT32".to_string()
            } else {
                t(app.lang, "parts.reuse")
            };
            spans.push(Span::styled("   \u{2192} ".to_string(), theme::accent()));
            if focused && !compact {
                spans.extend(option_strip(&["/boot/efi", "/boot"], mp));
            } else {
                spans.push(Span::styled(mp.to_string(), theme::accent()));
            }
            spans.push(Span::styled(format!("  {fate}"), theme::accent()));
        }
        Some(ROLE_ROOT) => {
            let fs = root_fs_eff(c);
            spans.push(Span::styled(
                "   \u{2192} /   ".to_string(),
                theme::accent(),
            ));
            fs_spans(&mut spans, app, fs, focused, compact, width);
        }
        Some(ROLE_HOME) => {
            let fs = if c.manual_home_fs.is_empty() {
                "ext4"
            } else {
                c.manual_home_fs.as_str()
            };
            spans.push(Span::styled(
                "   \u{2192} /home   ".to_string(),
                theme::accent(),
            ));
            fs_spans(&mut spans, app, fs, focused, compact, width);
        }
        Some(ROLE_SWAP) => {
            spans.push(Span::styled(
                "   \u{2192} swap".to_string(),
                theme::accent(),
            ));
        }
        Some(ROLE_DATA) => {
            let mp = data_mount(c, path).unwrap_or_default();
            spans.push(Span::styled(format!("   \u{2192} {mp}"), theme::accent()));
            // Say whether the data on it survives — that is the whole question
            // for a partition someone is mounting rather than installing onto.
            // A partition that IS being made (a fresh one, or an existing one
            // with no filesystem) gets the same filesystem picker as root and
            // /home: it is about to be created, so every choice is open. A
            // preserved one shows only what it already carries.
            // A partition being FORMATTED can be encrypted; one kept as-is
            // cannot — the data on it would have to go first, and keeping it is
            // exactly why it is not being formatted.
            // THE MARK MUST SHOW FOR /home TOO. It was drawn only for data
            // partitions, which keep the flag in `extra_disks`; /home keeps its
            // own in `manual_home_encrypt`, so turning encryption on there
            // produced one status line and then nothing — the table looked
            // identical either way, and pressing `e` again to check would
            // silently turn it back off. A setting you cannot see is a setting
            // you cannot trust.
            let encrypted = data_is_encrypted(c, path)
                || (role_of(c, path) == Some(ROLE_HOME) && c.manual_home_encrypt);
            if encrypted {
                spans.push(Span::styled(
                    format!("  [{}]", t(app.lang, "parts.enc_mark")),
                    theme::warn(),
                ));
            }
            if data_is_formatted(c, path) {
                spans.push(Span::styled("   ".to_string(), theme::accent()));
                fs_spans(
                    &mut spans,
                    app,
                    data_fs_eff(c, path),
                    focused,
                    compact,
                    width,
                );
            } else {
                spans.push(Span::styled(
                    format!("   {}", t(app.lang, "parts.reuse")),
                    theme::accent(),
                ));
            }
        }
        _ => {
            spans.push(Span::styled(
                format!("   {}", t(app.lang, "parts.keep_mark")),
                theme::dim(),
            ));
        }
    }
    Line::from(spans)
}

pub(crate) use crate::screens::widgets::modal_rect_fit;

/// The role-assignment modal for an existing partition: none + the 4 roles.
/// Mountpoint picker for a data partition: ready-made folders, or type a path.
fn draw_mount_modal(f: &mut Frame, app: &App) {
    let custom = app.parts_mount_cursor == MOUNT_TEMPLATES.len();
    let mut lines = vec![
        Line::from(Span::styled(format!(" {}", app.parts_target), theme::dim())),
        Line::from(""),
    ];
    for (i, m) in MOUNT_TEMPLATES.iter().enumerate() {
        let sel = i == app.parts_mount_cursor;
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
            t(app.lang, "parts.mount_custom"),
            app.parts_mount_input
        ),
        if custom {
            theme::selected()
        } else {
            theme::normal()
        },
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(" {}", t(app.lang, "parts.hint_mount")),
        theme::mute(),
    )));
    let rect = modal_rect_fit(
        f,
        60.max(crate::screens::widgets::text_width(&lines)),
        lines.len() as u16 + 2,
        app.modal_zoom,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {} ", t(app.lang, "parts.mount_title")));
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

fn mount_modal_key(app: &mut App, key: KeyEvent) {
    let len = MOUNT_TEMPLATES.len() + 1;
    if nav::move_cursor(key.code, &mut app.parts_mount_cursor, len) {
        return;
    }
    let custom = app.parts_mount_cursor == MOUNT_TEMPLATES.len();
    match key.code {
        KeyCode::Esc => {
            // Abandon cleanly: nothing has been committed yet, and the "this
            // picker is finishing the create wizard" flag must not leak into a
            // later picker opened on an EXISTING partition — Enter there would
            // then create a phantom new partition instead of mounting.
            app.parts_mount_open = false;
            app.parts_mount_for_new = false;
            app.parts_pending_mib = 0;
        }
        // The free-form row takes typing; a path must start at the root.
        KeyCode::Char(ch) if custom && app.parts_mount_input.chars().count() < 60 => {
            if app.parts_mount_input.is_empty() && ch != '/' {
                app.parts_mount_input.push('/');
            }
            app.parts_mount_input.push(ch);
        }
        KeyCode::Backspace if custom => {
            app.parts_mount_input.pop();
        }
        KeyCode::Enter => {
            let mp = if custom {
                app.parts_mount_input.trim().to_string()
            } else {
                MOUNT_TEMPLATES[app.parts_mount_cursor].to_string()
            };
            // Refuse a path that isn't one, rather than storing something the
            // plan would later try to mkdir.
            if !mp.starts_with('/') || mp.len() < 2 {
                app.pmode_status = t(app.lang, "parts.bad_mount");
                return;
            }
            if app.parts_mount_for_new {
                create_data_slot(app, &mp);
            } else {
                let target = app.parts_target.clone();
                set_data_mount(app, &target, &mp);
            }
            app.parts_mount_for_new = false;
            app.parts_mount_open = false;
        }
        _ => {}
    }
}

/// Finish the create wizard's "data" branch: carve a new partition on the
/// wizard's disk and mount it at `mp`.
///
/// The creation lives in `manual_data_new` (so the plan's sgdisk pass makes the
/// partition) and the mount lives in `extra_disks` (so the existing extra-disk
/// machinery formats, mounts and fstab-records it). The path is predicted by
/// `renumber_planned` first, then the mount entry is keyed by that path.
fn create_data_slot(app: &mut App, mp: &str) {
    let disk = app.parts_create_disk.clone();
    let mib = app.parts_pending_mib;
    if disk.is_empty() || mib == 0 {
        return;
    }
    app.config.manual_data_new.push(crate::app::DataPart {
        path: String::new(),
        disk,
        mib,
    });
    renumber_planned(app);
    let Some(path) = app
        .config
        .manual_data_new
        .last()
        .map(|dp| dp.path.clone())
        .filter(|p| !p.is_empty())
    else {
        // No path could be predicted (unknown disk) — take the plan back
        // rather than leaving a partition that can never be created.
        app.config.manual_data_new.pop();
        return;
    };
    app.config.extra_disks.retain(|e| e.disk != path);
    app.config.extra_disks.push(crate::app::ExtraDisk {
        disk: path,
        mountpoint: mp.to_string(),
        fs: "ext4".into(),
        format: true, // brand new — nothing on it to preserve
        whole_disk: false,
        ..Default::default()
    });
    app.parts_pending_mib = 0;
}

fn draw_role_modal(f: &mut Frame, app: &App) {
    let entries: Vec<String> = std::iter::once(t(app.lang, "parts.none"))
        .chain((0..ROLES).map(|r| role_label(app, r)))
        .collect();
    let rect = modal_rect_fit(f, 44, entries.len() as u16 + 4, app.modal_zoom);
    let mut lines = vec![Line::from(Span::styled(
        format!(" {}", app.parts_target),
        theme::dim(),
    ))];
    for (i, e) in entries.iter().enumerate() {
        let sel = i == app.parts_modal_cursor;
        let pre = if sel { "> " } else { "  " };
        lines.push(Line::from(Span::styled(
            format!("{pre}{e}"),
            if sel {
                theme::selected()
            } else {
                theme::normal()
            },
        )));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {} ", t(app.lang, "parts.role_title")));
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

/// The create-partition wizard: stage 0 picks the role, stage 1 the size.
fn draw_create_modal(f: &mut Frame, app: &App) {
    let avail = available_roles(app);
    let (title, mut lines) = if app.parts_create_stage == 0 {
        let mut ls: Vec<Line> = Vec::new();
        if avail.is_empty() {
            ls.push(Line::from(Span::styled(
                format!(" {}", t(app.lang, "parts.create_none")),
                theme::dim(),
            )));
        }
        for (i, r) in avail.iter().enumerate() {
            let sel = i == app.parts_create_role.min(avail.len().saturating_sub(1));
            let pre = if sel { "> " } else { "  " };
            ls.push(Line::from(Span::styled(
                format!("{pre}{}", role_label(app, *r)),
                if sel {
                    theme::selected()
                } else {
                    theme::normal()
                },
            )));
        }
        (t(app.lang, "parts.create_role"), ls)
    } else {
        let role = app.parts_create_role;
        let _ = role;
        let prompt = t(app.lang, "parts.create_size_rest");
        let ls = vec![
            Line::from(Span::styled(format!(" {prompt}"), theme::normal())),
            Line::from(""),
            Line::from(vec![
                Span::styled(" > ".to_string(), theme::accent()),
                Span::styled(app.parts_size_input.clone(), theme::normal()),
                Span::styled(format!("| {}", size_unit_label(app)), theme::accent()),
            ]),
        ];
        let title = if app.parts_resize_dev.is_empty() {
            role_label(app, role)
        } else {
            format!(
                "{} {} — {}",
                t(app.lang, "parts.resize_title"),
                app.parts_resize_dev,
                role_label(app, role)
            )
        };
        (title, ls)
    };
    lines.push(Line::from(""));
    // THE CAPTION MUST DESCRIBE THIS STAGE, not the next one. The footer was
    // taught that and this box was not, so the role list — a plain up-down
    // list — carried "digits — size · ←/→ — MiB/GiB" underneath it: an
    // instruction for the step after the one being looked at.
    lines.push(Line::from(Span::styled(
        format!(
            " {}",
            if app.parts_create_stage == 0 {
                t(app.lang, "parts.hint_role")
            } else {
                t(app.lang, "parts.hint_size")
            }
        ),
        theme::mute(),
    )));
    // Wide enough for the size hint, which now also names the MiB/GiB switch.
    let rect = modal_rect_fit(
        f,
        68.max(crate::screens::widgets::text_width(&lines)),
        lines.len() as u16 + 2,
        app.modal_zoom,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {title} "));
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

/// The i18n label / description key for a wipe method — one slug, from `id()`.
fn wipe_method_label(app: &App, m: crate::app::WipeMethod) -> String {
    t(app.lang, &format!("parts.wipe_{}", m.id()))
}
fn wipe_method_desc(app: &App, m: crate::app::WipeMethod) -> String {
    t(app.lang, &format!("parts.wipe_{}_desc", m.id()))
}

/// "This drive holds another operating system" — shown when a wipe is selected
/// on a drive carrying someone else's bootloader or Windows volumes.
///
/// Deliberately NOT a refusal. Wiping Windows can be exactly the intent: Artix
/// may be the first system going on this machine, with another Linux added by
/// hand afterwards. So the modal's job is understanding, not permission — it
/// names every partition that will be destroyed, says plainly if that kills the
/// other system's ability to boot, and defaults to cancel.
fn draw_wipe_ack_modal(f: &mut Frame, app: &App) {
    let disk = app.parts_wipe_ack_disk.clone();
    let foreign = foreign_parts(app, &disk);
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            format!(" {} {}", t(app.lang, "parts.wipe_ack_intro"), disk),
            theme::warn(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!(" {}", t(app.lang, "parts.wipe_ack_lost")),
            theme::normal(),
        )),
    ];
    for (path, what) in &foreign {
        lines.push(Line::from(Span::styled(
            format!("    {path} — {what}"),
            theme::warn(),
        )));
    }
    // The consequence people miss: this is the drive the other OS boots from.
    if is_neighbour_boot_disk(app, &disk) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(" {}", t(app.lang, "parts.wipe_ack_boot")),
            theme::warn(),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(" {}", t(app.lang, "parts.wipe_ack_ok_note")),
        theme::mute(),
    )));
    lines.push(Line::from(""));
    for (i, lab) in ["parts.wipe_ack_no", "parts.wipe_ack_yes"]
        .iter()
        .enumerate()
    {
        let sel = i == app.parts_wipe_ack_cursor;
        let pre = if sel { "> " } else { "  " };
        let style = if sel {
            if i == 1 {
                theme::warn()
            } else {
                theme::selected()
            }
        } else {
            theme::normal()
        };
        lines.push(Line::from(Span::styled(
            format!("{pre}{}", t(app.lang, lab)),
            style,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(" {}", t(app.lang, "parts.wipe_hint_confirm")),
        theme::dim(),
    )));

    // These sentences are long — and the one that matters most ("the other OS
    // will no longer start") is the longest. A Paragraph without wrapping CUTS
    // the tail off, so the warning would be silently half-shown. Wrap, and size
    // the box for the wrapped height rather than the line count.
    const W: u16 = 76;
    let inner_w = (W as usize).saturating_sub(2).max(1);
    let rows: usize = lines
        .iter()
        .map(|l| {
            let w: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
            w.max(1).div_ceil(inner_w)
        })
        .sum();
    let rect = modal_rect_fit(f, W, rows as u16 + 2, app.modal_zoom);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::warn())
        .title(format!(" {} ", t(app.lang, "parts.wipe_ack_title")));
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

/// The strong "erase this disk NOW" confirmation. The method is already chosen
/// on the header strip; this only asks the irreversible yes/no, defaulting to
/// cancel. Nothing here touches a disk — it drives the state the key handler
/// acts on.
fn draw_wipe_modal(f: &mut Frame, app: &App) {
    let disk = app.parts_wipe_disk.clone();
    let method = app
        .config
        .manual_wipe
        .iter()
        .find(|w| w.disk == disk)
        .map(|w| w.method)
        .unwrap_or(crate::app::WipeMethod::Quick);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(" {} {}", t(app.lang, "parts.wipe_confirm"), disk),
        theme::warn(),
    )));
    lines.push(Line::from(Span::styled(
        format!(
            " {} {}",
            t(app.lang, "parts.wipe_confirm_method"),
            wipe_method_label(app, method)
        ),
        theme::mute(),
    )));
    // Name what belongs to another operating system before it is destroyed —
    // the last chance to notice that this is the Windows drive.
    let foreign = foreign_parts(app, &disk);
    if !foreign.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(" [!] {}", t(app.lang, "parts.wipe_foreign_warn")),
            theme::warn(),
        )));
        for (path, what) in &foreign {
            lines.push(Line::from(Span::styled(
                format!("    {path} — {what}"),
                theme::warn(),
            )));
        }
    }
    lines.push(Line::from(""));
    // Three outcomes, all of them spelled out. There used to be two — cancel and
    // "erase now" — while the third, "erase during the install", existed only as
    // a side effect of having picked a method with ←/→ and then NOT opening this
    // dialog. It was the option most people want, and the only way to choose it
    // was to not press the key that shows the choices.
    for (i, lab) in ["parts.wipe_cancel", "parts.wipe_later", "parts.wipe_do"]
        .iter()
        .enumerate()
    {
        let sel = i == app.parts_wipe_confirm_cursor;
        let pre = if sel { "> " } else { "  " };
        let style = if sel {
            if i == 2 {
                theme::warn()
            } else {
                theme::selected()
            }
        } else {
            theme::normal()
        };
        lines.push(Line::from(Span::styled(
            format!("{pre}{}", t(app.lang, lab)),
            style,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(" {}", t(app.lang, "parts.wipe_hint_confirm")),
        theme::dim(),
    )));

    let title = t(app.lang, "parts.wipe_confirm_title");
    let rect = modal_rect_fit(
        f,
        72.max(crate::screens::widgets::text_width(&lines)),
        lines.len() as u16 + 2,
        app.modal_zoom,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::warn())
        .title(format!(" {title} "));
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

/// The "now" wipe progress overlay: a status line, the tail of the streamed
/// output, and a dismiss hint once finished.
fn draw_wipe_run_overlay(f: &mut Frame, app: &App) {
    let running = app.wipe_run_rx.is_some();
    let title = format!("{} {}", t(app.lang, "parts.wipe_title"), app.wipe_run_disk);
    let rect = modal_rect_fit(f, 78, 18, app.modal_zoom);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::warn())
        .title(format!(" {title} "));
    f.render_widget(Clear, rect);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let status = if running {
        Span::styled(t(app.lang, "parts.wipe_running"), theme::warn())
    } else {
        match &app.wipe_run_done {
            Some(Ok(())) => Span::styled(t(app.lang, "parts.wipe_done_ok"), theme::ok()),
            _ => Span::styled(t(app.lang, "parts.wipe_done_err"), theme::warn()),
        }
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    f.render_widget(Paragraph::new(Line::from(status)), chunks[0]);
    let take = chunks[1].height as usize;
    let tail: Vec<Line> = app
        .wipe_run_log
        .iter()
        .rev()
        .take(take)
        .rev()
        .map(|l| Line::from(Span::styled(l.clone(), theme::dim())))
        .collect();
    f.render_widget(Paragraph::new(tail), chunks[1]);
    let hint = if running {
        String::new()
    } else {
        t(app.lang, "parts.wipe_run_dismiss")
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, theme::dim()))),
        chunks[2],
    );
}

/// Which filesystem the `[o]` cue on the row under the cursor configures.
///
/// The cue is drawn on the root row AND on a dedicated /home row, but `o` used
/// to always open the ROOT's options — so configuring "/home" silently rewrote
/// the root's subvolume layout. The row decides the target now.
fn fs_opts_target_for(app: &App, row: &Row) -> FsOptsTarget {
    match row {
        Row::Part { path, .. }
            if !app.config.manual_home.is_empty() && *path == app.config.manual_home =>
        {
            FsOptsTarget::ManualHome
        }
        Row::Part { path, .. } if role_of(&app.config, path) == Some(ROLE_DATA) => {
            FsOptsTarget::ManualData
        }
        _ => FsOptsTarget::Root,
    }
}

/// The filesystem whose options `target` configures. Takes the whole `App`
/// because a data partition's identity lives outside the config
/// (`fs_opts_data_dev`) — there can be several, unlike root and /home.
fn fs_for_target(app: &App, target: FsOptsTarget) -> String {
    match target {
        FsOptsTarget::Root => root_fs_eff(&app.config).to_string(),
        FsOptsTarget::ManualHome => home_fs_eff(&app.config).to_string(),
        FsOptsTarget::ManualData => data_fs_eff(&app.config, &app.fs_opts_data_dev).to_string(),
    }
}

/// The filesystem a data partition will carry: the one chosen for a partition
/// being formatted, or the one already on it when it is mounted as-is.
/// What the SCAN says is on this device right now — not what the plan intends.
///
/// Used to tell "formatting an empty partition" from "formatting one that holds
/// something", which is the difference between a note and a warning.
fn scan_fs_of(app: &App, dev: &str) -> String {
    app.parts_list
        .iter()
        .find(|p| p.path == dev)
        .map(|p| p.fstype.clone())
        .unwrap_or_default()
}

fn data_fs_eff<'a>(c: &'a InstallConfig, dev: &str) -> &'a str {
    c.extra_disks
        .iter()
        .find(|e| e.disk == dev)
        .map(|e| e.fs.as_str())
        .filter(|f| !f.is_empty())
        .unwrap_or("ext4")
}

/// Whether a data partition is going to be MADE (mkfs) rather than mounted with
/// whatever it already holds. Only a formatted one gets to pick a filesystem —
/// offering the choice on a preserved partition would promise a conversion that
/// does not happen.
fn data_is_formatted(c: &InstallConfig, dev: &str) -> bool {
    c.extra_disks
        .iter()
        .find(|e| e.disk == dev)
        .map(|e| e.format)
        .unwrap_or(false)
}

/// The effective /home filesystem ("ext4" until the user picks one).
fn home_fs_eff(c: &InstallConfig) -> &str {
    if c.manual_home_fs.is_empty() {
        "ext4"
    } else {
        c.manual_home_fs.as_str()
    }
}

// ââ keys â───────────────────────────────────────────────────────────────────

/// How many undo steps the editor keeps. Deep enough that no realistic editing
/// session hits the ceiling, bounded so a held-down arrow key can't grow the
/// history without limit.
const UNDO_LIMIT: usize = 256;

pub fn handle_key(app: &mut App, key: KeyEvent) {
    // `u` undoes ONE change. Esc is left alone: it walks back a page, like
    // everywhere else in the wizard.
    //
    // Esc used to do the undoing, and only left once the history was empty —
    // which meant stepping back to change a desktop or add a package first
    // destroyed the whole layout. Leaving a step must never be a way to lose
    // work. (Modals own both keys first, to close themselves.)
    if matches!(key.code, KeyCode::Char('u') | KeyCode::Char('U'))
        && !app.fs_opts_modal_open
        && !app.parts_modal_open
        && !app.parts_create_open
        && !app.parts_mount_open
        && !app.parts_wipe_open
        && !app.parts_wipe_ack_open
        && app.wipe_run_rx.is_none()
        && app.wipe_run_done.is_none()
    {
        if let Some(prev) = app.parts_undo.pop() {
            app.config = prev;
            app.pmode_status.clear();
        } else {
            app.pmode_status = t(app.lang, "parts.nothing_to_undo");
        }
        return;
    }

    // Everything else: snapshot first, and keep the snapshot only if the key
    // changed the LAYOUT. The snapshot is the whole config, so an undo restores
    // a consistent state — but only a layout change is worth a step.
    let before_layout = layout_fingerprint(&app.config);
    let before = app.config.clone();
    handle_key_inner(app, key);
    if layout_fingerprint(&app.config) != before_layout {
        if app.parts_undo.len() >= UNDO_LIMIT {
            app.parts_undo.remove(0);
        }
        app.parts_undo.push(before);
    }
}

/// What `u` treats as one step: a change to the LAYOUT, not to a setting.
///
/// This used to compare the WHOLE config, on the theory that then no edit could
/// be forgotten by omission. In use that was wrong: cycling a filesystem with
/// ←/→ recorded a step per keypress, so `u` walked back through a dozen
/// invisible ones before reaching the partition the user actually wanted back —
/// and silently reverted their btrfs options on the way, which read as "my
/// options don't get saved".
///
/// The line is drawn at reversibility. A filesystem, a compression flag, a
/// mount option, "format the ESP" — one keypress sets it, the same keypress
/// unsets it, so `u` owes them nothing. What it owes is everything a keypress
/// cannot bring back: which partitions exist, their size, their disk, the role
/// they hold, what is marked for deletion or a wipe, and where things mount.
fn layout_fingerprint(c: &InstallConfig) -> String {
    let mut s = String::with_capacity(256);
    s.push_str(if c.boot_mode.is_uefi() {
        "uefi;"
    } else {
        "bios;"
    });
    for (dev, mib, disk) in [
        (&c.manual_esp, c.manual_esp_new_mib, &c.manual_esp_disk),
        (&c.manual_bios, c.manual_bios_new_mib, &c.manual_bios_disk),
        (&c.manual_root, c.manual_root_new_mib, &c.manual_root_disk),
        (&c.manual_swap, c.manual_swap_new_mib, &c.manual_swap_disk),
        (&c.manual_home, c.manual_home_new_mib, &c.manual_home_disk),
    ] {
        s.push_str(&format!("{dev}:{mib}:{disk}|"));
    }
    for dp in &c.manual_data_new {
        s.push_str(&format!("d{}:{}:{}|", dp.path, dp.mib, dp.disk));
    }
    for p in &c.manual_deleted {
        s.push_str(&format!("x{}|", p.path));
    }
    for w in &c.manual_wipe {
        s.push_str(&format!("w{}:{}|", w.disk, w.method.id()));
    }
    // Membership and mountpoint only — an entry's filesystem and its options are
    // settings, and toggling one must not cost an undo step.
    for e in &c.extra_disks {
        s.push_str(&format!("m{}:{}|", e.disk, e.mountpoint));
    }
    s
}

fn handle_key_inner(app: &mut App, key: KeyEvent) {
    if app.fs_opts_modal_open {
        let fs = fs_for_target(app, app.fs_opts_target);
        crate::screens::disk::fs_opts_modal_key(app, key, &fs);
        return;
    }
    if app.parts_modal_open {
        role_modal_key(app, key);
        return;
    }
    if app.parts_create_open {
        create_modal_key(app, key);
        return;
    }
    if app.parts_mount_open {
        mount_modal_key(app, key);
        return;
    }
    // A "now" wipe overlay owns every key while it runs and until dismissed.
    if app.wipe_run_rx.is_some() || app.wipe_run_done.is_some() {
        wipe_run_key(app, key);
        return;
    }
    if app.parts_wipe_ack_open {
        wipe_ack_key(app, key);
        return;
    }
    if app.parts_wipe_open {
        wipe_modal_key(app, key);
        return;
    }

    let rows = build_rows(app);
    if nav::move_cursor(key.code, &mut app.cursor, rows.len()) {
        // Borrow-safe: decide which headers are landable BEFORE taking the
        // cursor mutably (wipe_refusal reads the whole App).
        let landable: Vec<String> = rows
            .iter()
            .filter_map(|r| match r {
                Row::DiskHeader { disk } if wipe_refusal(disk).is_none() => Some(disk.clone()),
                _ => None,
            })
            .collect();
        skip_labels(&rows, &mut app.cursor, moving_down(key.code), &|d| {
            landable.iter().any(|x| x == d)
        });
        // Moving the cursor dismisses a transient action message.
        app.pmode_status.clear();
        return;
    }
    let Some(row) = rows.get(app.cursor).cloned() else {
        return;
    };

    // Screen-wide keys first.
    match key.code {
        KeyCode::Char('r') | KeyCode::Char('R') => {
            // Rescan — the user just made partitions on another console.
            app.parts_scanned = false;
            return;
        }
        KeyCode::Char('o') | KeyCode::Char('O') => {
            let target = fs_opts_target_for(app, &row);
            // Bind the modal to THIS row's device before asking what filesystem
            // it carries — `fs_for_target` reads `fs_opts_data_dev` to answer.
            if target == FsOptsTarget::ManualData {
                if let Row::Part { path, .. } = &row {
                    app.fs_opts_data_dev = path.clone();
                }
            }
            let fs = fs_for_target(app, target);
            if !crate::screens::disk::fs_features_for(target, &fs).is_empty() {
                app.fs_opts_target = target;
                app.fs_opt_cursor = 0;
                app.fs_opts_modal_open = true;
            }
            return;
        }
        KeyCode::Char('w') | KeyCode::Char('W') => {
            // Jump to this disk's header, where the wipe selector lives.
            if let Some(disk) = row_disk(app, &row) {
                if let Some(reason) = wipe_refusal(&disk) {
                    app.pmode_status = t(app.lang, reason);
                    return;
                }
                let rows2 = build_rows(app);
                if let Some(pos) = rows2
                    .iter()
                    .position(|r| matches!(r, Row::DiskHeader { disk: d } if *d == disk))
                {
                    app.cursor = pos;
                    app.pmode_status.clear();
                }
            }
            return;
        }
        _ => {}
    }

    match row {
        Row::BootMode => {
            // ←/→ (or Space) flips the firmware. Changing it changes which boot
            // partition the layout needs, so drop a stale one rather than leave
            // an ESP hanging around a legacy install.
            if matches!(
                key.code,
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
            ) {
                app.config.boot_mode = app.config.boot_mode.toggled();
                if app.config.boot_mode.is_uefi() {
                    app.config.manual_bios.clear();
                    app.config.manual_bios_new_mib = 0;
                } else {
                    app.config.manual_esp.clear();
                    app.config.manual_esp_new_mib = 0;
                    app.config.manual_esp_format = false;
                }
                renumber_planned(app);
                refresh_boot_disk(app);
            }
        }
        // A disk header carries the whole-disk WIPE selector (solo mode only):
        // ←/→ cycles the erase method (default "nothing"), Enter raises the
        // strong "erase now" confirm. In dual-boot the header is a plain caption
        // the cursor skips (see `skip_labels`), so this only fires in solo.
        Row::DiskHeader { disk } => match key.code {
            KeyCode::Left | KeyCode::Char('h') => cycle_disk_wipe(app, &disk, false),
            KeyCode::Right | KeyCode::Char('l') => cycle_disk_wipe(app, &disk, true),
            KeyCode::Enter => open_wipe_now_confirm(app, &disk),
            _ => {}
        },
        Row::Part { path, is_new } => match key.code {
            KeyCode::Enter => {
                if is_new {
                    // RESIZE the planned partition. Nothing is on the disk yet,
                    // so a wrong size is a number to correct, not a partition to
                    // delete and rebuild.
                    if let Some(role) = role_of(&app.config, &path) {
                        let mib = planned_mib_of(&app.config, role, &path);
                        app.parts_create_role = role;
                        app.parts_create_stage = 1;
                        app.parts_resize_dev = path.clone();
                        // Carry the slot's OWN disk into the wizard: `create_slot`
                        // writes `parts_create_disk` back onto the slot, so a stale
                        // value here would move the partition to another drive.
                        app.parts_create_disk = planned_disk_of(&app.config, role, &path);
                        // Offer the size back in the unit it reads best in, so the
                        // box shows what the row shows.
                        if mib == MANUAL_REST || mib == 0 {
                            app.parts_size_input.clear();
                            app.parts_size_mib = false;
                        } else if mib.is_multiple_of(1024) {
                            app.parts_size_input = (mib / 1024).to_string();
                            app.parts_size_mib = false;
                        } else {
                            app.parts_size_input = mib.to_string();
                            app.parts_size_mib = true;
                        }
                        app.parts_create_open = true;
                    }
                } else {
                    app.parts_target = path.clone();
                    app.parts_modal_cursor =
                        role_of(&app.config, &path).map(|r| r + 1).unwrap_or(0);
                    app.parts_modal_open = true;
                }
            }
            KeyCode::Char('f') | KeyCode::Char('F')
                if role_of(&app.config, &path) == Some(ROLE_ESP) && !is_new =>
            {
                app.config.manual_esp_format = !app.config.manual_esp_format;
            }
            // `f` on a DATA partition: format it, or keep it as it is.
            //
            // Without this, encryption was unreachable for the case it exists
            // for. A data partition that already has a filesystem is mounted
            // as-is (format=false), and both `e` (encrypt) and ←/→ (filesystem)
            // refuse while that is true — so "encrypt the disk I keep junk on"
            // could only be done by deleting the partition and making a new
            // one. The answer is not to forbid it but to say what it costs:
            // encryption rewrites the whole device, so whatever is on it goes.
            KeyCode::Char('f') | KeyCode::Char('F')
                if role_of(&app.config, &path) == Some(ROLE_DATA) && !is_new =>
            {
                let dev = path.clone();
                let had_fs = scan_fs_of(app, &dev);
                if let Some(e) = app.config.extra_disks.iter_mut().find(|e| e.disk == dev) {
                    e.format = !e.format;
                    if !e.format {
                        // Kept as-is cannot be encrypted, so the flag goes with
                        // it rather than lingering as a promise the plan would
                        // not keep.
                        e.encrypt = false;
                    }
                    app.pmode_status = if e.format {
                        if had_fs.is_empty() {
                            t(app.lang, "parts.data_fmt_on")
                        } else {
                            format!(
                                "{} {}",
                                t(app.lang, "parts.data_fmt_on"),
                                t(app.lang, "parts.data_fmt_erases")
                            )
                        }
                    } else {
                        t(app.lang, "parts.data_fmt_off")
                    };
                }
            }
            // ESP row ←/→: toggle where the ESP mounts — /boot (kernels on the
            // ESP) or /boot/efi (kernels on root, only the bootloader on a small
            // reused ESP). The dual-boot control people look for.
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Right | KeyCode::Char('l')
                if role_of(&app.config, &path) == Some(ROLE_ESP) =>
            {
                app.config.manual_esp_mount = if app.config.manual_esp_mount == "/boot/efi" {
                    "/boot".into()
                } else {
                    "/boot/efi".into()
                };
            }
            KeyCode::Left | KeyCode::Char('h')
                if role_of(&app.config, &path) == Some(ROLE_ROOT) =>
            {
                app.config.manual_root_fs = cycle_fs(&app.config.manual_root_fs, -1);
            }
            KeyCode::Right | KeyCode::Char('l')
                if role_of(&app.config, &path) == Some(ROLE_ROOT) =>
            {
                app.config.manual_root_fs = cycle_fs(&app.config.manual_root_fs, 1);
            }
            KeyCode::Left | KeyCode::Char('h')
                if role_of(&app.config, &path) == Some(ROLE_HOME) =>
            {
                app.config.manual_home_fs = cycle_fs(&app.config.manual_home_fs, -1);
            }
            KeyCode::Right | KeyCode::Char('l')
                if role_of(&app.config, &path) == Some(ROLE_HOME) =>
            {
                app.config.manual_home_fs = cycle_fs(&app.config.manual_home_fs, 1);
            }
            // A data partition being formatted picks its filesystem exactly like
            // root and /home. One that is mounted as-is does not: its filesystem
            // is a fact about the disk, not a choice, and pretending otherwise
            // would promise a conversion the plan never performs.
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Right | KeyCode::Char('l')
                if role_of(&app.config, &path) == Some(ROLE_DATA)
                    && data_is_formatted(&app.config, &path) =>
            {
                let dir = if matches!(key.code, KeyCode::Left | KeyCode::Char('h')) {
                    -1
                } else {
                    1
                };
                let next = cycle_fs(data_fs_eff(&app.config, &path), dir);
                let dev = path.clone();
                if let Some(e) = app.config.extra_disks.iter_mut().find(|e| e.disk == dev) {
                    // Only the filesystem changes. The options are KEPT even
                    // while a filesystem that cannot use them is selected, so
                    // cycling past ext4 and back to btrfs returns the user's
                    // choices intact — losing them mid-cycle is what made
                    // "my btrfs options don't get saved" true. Nothing leaks:
                    // the plan applies compression only when the filesystem is
                    // actually btrfs (`d.compress && d.fs == "btrfs"`), and the
                    // row hides options the current filesystem has no use for.
                    e.fs = next;
                }
            }
            KeyCode::Char('d') | KeyCode::Char('D') if is_new => {
                delete_planned(app, &path);
                app.cursor = app.cursor.saturating_sub(1);
            }
            // An EXISTING partition: mark (or unmark) it for deletion, freeing
            // its space so a differently-sized one can be planned there. The
            // live medium is refused — deleting the partition the installer is
            // running from kills the install midway.
            KeyCode::Char('d') | KeyCode::Char('D') => {
                if partition_is_live(app, &path) {
                    app.pmode_status = t(app.lang, "parts.err_delete_live");
                } else {
                    toggle_deleted(app, &path);
                }
            }
            // LUKS on a data partition. Reachable only from here in manual mode:
            // the extra-disks step, where this used to live, no longer appears
            // once the editor is doing that job.
            // No `e` here. Encryption is decided on the "Bootloader and
            // encryption" step, beside the root's own — one word meaning one
            // thing, in one place. A key that only says "this moved" is still a
            // key people press and a line the hints have to carry; the mark in
            // the table already reports what was chosen there, which is what a
            // partition table is for.
            _ => {}
        },
        // "+ Створити розділ" under a disk: opens the two-stage wizard (role,
        // then size) for THAT disk. The disk travels with the wizard so a
        // layout can be built across several drives at once.
        Row::Create { disk } => {
            if key.code == KeyCode::Enter {
                // THE ROW ALREADY SAID "space is allocated" — so the action has
                // to mean it. Creating here used to open the wizard anyway and
                // take the space out of the rest-of-disk slot without a word,
                // which is how a 29 GiB root became 25 GiB the moment a swap
                // was added. Enter never stays silent (rule 2), and a full disk
                // is a decision for the user to make, not one to make for them.
                if free_bytes(app, &disk) == 0 {
                    if let Some(rest) = rest_slot_on(&app.config, &disk) {
                        app.pmode_status = t(app.lang, "parts.err_disk_claimed")
                            .replace("{part}", &slot_name(app, &rest));
                        return;
                    }
                    if planned_slots(&app.config).iter().any(|p| p.disk == disk) {
                        app.pmode_status = t(app.lang, "parts.err_disk_full");
                        return;
                    }
                }
                app.parts_create_disk = disk;
                app.parts_create_role = 0;
                app.parts_create_stage = 0;
                app.parts_resize_dev.clear();
                app.parts_size_input.clear();
                app.parts_size_pristine = false;
                app.parts_create_open = true;
            }
        }
        // The Continue control at the foot of the screen: the same "next step"
        // Enter performs, offered as a landable row so the way forward is
        // visible rather than remembered.
        Row::Continue => {
            if key.code == KeyCode::Enter {
                app.goto_next();
            }
        }
    }
}

/// The role picker for an EXISTING partition: "leave alone" plus every role.
///
/// Choosing "data" does not finish the job — that role needs a mountpoint, so
/// it hands over to the mount picker instead of silently assigning a partition
/// with nowhere to go.
fn role_modal_key(app: &mut App, key: KeyEvent) {
    if nav::move_cursor(key.code, &mut app.parts_modal_cursor, ROLES + 1) {
        return;
    }
    match key.code {
        KeyCode::Esc => app.parts_modal_open = false,
        KeyCode::Enter => {
            let dev = app.parts_target.clone();
            let sel = app.parts_modal_cursor;
            app.parts_modal_open = false;
            if sel == 0 {
                assign_role(app, &dev, None);
                clear_data_mount(&mut app.config, &dev);
                return;
            }
            let role = sel - 1;
            if role == ROLE_DATA {
                app.parts_target = dev;
                app.parts_mount_for_new = false;
                app.parts_mount_cursor = 0;
                app.parts_mount_input.clear();
                app.parts_mount_open = true;
            } else {
                assign_role(app, &dev, Some(role));
            }
        }
        _ => {}
    }
}

/// The two-stage creation wizard: which role, then how big.
///
/// Stage 0 walks `available_roles` — a role already filled is not offered, so
/// the list shrinks as the layout is built and never proposes a second root.
fn create_modal_key(app: &mut App, key: KeyEvent) {
    let avail = available_roles(app);
    if app.parts_create_stage == 0 {
        if nav::move_cursor(key.code, &mut app.parts_create_role, avail.len().max(1)) {
            return;
        }
        match key.code {
            KeyCode::Esc => app.parts_create_open = false,
            KeyCode::Enter => {
                let Some(role) = avail.get(app.parts_create_role).copied() else {
                    app.parts_create_open = false;
                    return;
                };
                app.parts_create_role = role;
                app.parts_create_stage = 1;
                // A sensible starting size per role, in the unit that suits it:
                // an ESP is naturally sized in MiB (512 is the common default),
                // swap in GiB; root/home default to "rest of the disk" (empty).
                app.parts_size_mib = role == ROLE_ESP;
                app.parts_size_input = match role {
                    ROLE_ESP => "512".into(),
                    ROLE_SWAP => "4".into(),
                    _ => String::new(),
                };
                app.parts_size_pristine = !app.parts_size_input.is_empty();
            }
            _ => {}
        }
        return;
    }
    // Stage 1: size entry (whole GiB; empty/0 = rest for root and /home).
    match key.code {
        KeyCode::Esc => {
            // A resize has no role-picking stage behind it — Esc leaves.
            if !app.parts_resize_dev.is_empty() {
                app.parts_resize_dev.clear();
                app.parts_create_open = false;
                return;
            }
            app.parts_create_stage = 0;
            app.parts_create_role = 0;
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
            // Switch the unit WITHOUT converting the number: the box is a size
            // the user is typing, not a measurement being re-expressed. Typing
            // 500 and switching to MiB must mean 500 MiB.
            app.parts_size_mib = !app.parts_size_mib;
        }
        // THE FIRST DIGIT REPLACES THE SUGGESTION. Appending to it turned a
        // typed 4 into 44 — a 44 GiB swap on a 30 GiB disk, accepted without a
        // word — and would turn a typed 40 in the ESP box into 51240. Nobody
        // types a number expecting it to be glued onto one they never chose.
        // A DECIMAL POINT IS ACCEPTED, because half a gibibyte is a size people
        // actually want. Refused before, the only way to ask for 25.5 GiB was to
        // work out 26112 MiB by hand — and the user who tried typed 25500,
        // which is 24.9 GiB, and had no way to see that it was not what they
        // meant. A comma types the same character: on the layouts this installer
        // ships, that is the key beside the full stop.
        KeyCode::Char(ch) if ch.is_ascii_digit() || ch == '.' || ch == ',' => {
            if app.parts_size_pristine {
                app.parts_size_input.clear();
                app.parts_size_pristine = false;
            }
            // One point only, and never as the first character: ".5" parses,
            // but a box that opens with a lone dot reads as a stuck key.
            if (ch == '.' || ch == ',')
                && (app.parts_size_input.is_empty() || app.parts_size_input.contains('.'))
            {
                return;
            }
            if app.parts_size_input.len() < 8 {
                app.parts_size_input.push(if ch == ',' { '.' } else { ch });
            }
        }
        // Backspace on an untouched suggestion clears it whole: deleting one
        // digit from a number you never typed leaves a number you never chose.
        KeyCode::Backspace => {
            if app.parts_size_pristine {
                app.parts_size_input.clear();
                app.parts_size_pristine = false;
            } else {
                app.parts_size_input.pop();
            }
        }
        KeyCode::Enter => {
            let role = app.parts_create_role;
            let typed: f64 = app.parts_size_input.parse().unwrap_or(0.0);
            // An empty box means "all the space that's left", for every role.
            // It used to mean that only for root and /home; ESP and swap simply
            // returned, leaving the wizard open with nothing said — which reads
            // as the installer having made an empty partition rather than
            // having refused. Enter must never be silent.
            //
            // MiB is the smallest unit the plan works in, so a fraction of one
            // is rounded, not dropped. The ceiling stops short of MANUAL_REST,
            // which is `u32::MAX` used as a marker — a size that landed exactly
            // on it would silently mean "the rest of the disk" instead.
            let to_mib = |v: f64| v.round().clamp(1.0, (u32::MAX - 1) as f64) as u32;
            let mib = if typed <= 0.0 {
                MANUAL_REST
            } else if app.parts_size_mib {
                to_mib(typed)
            } else {
                to_mib(typed * 1024.0)
            };
            // A SIZE THAT DOES NOT FIT IS REFUSED, not quietly written down.
            //
            // Nothing checked this: a 44 GiB swap was accepted on a 30 GiB disk
            // and shown in the table as if it were real, with the remaining
            // space reported as already spent — so the root could no longer be
            // created and the reason was nowhere on screen. The plan would have
            // failed much later, at sgdisk, with the disk already being written.
            if mib != MANUAL_REST {
                let disk = if !app.parts_resize_dev.is_empty() {
                    planned_disk_of(&app.config, role, &app.parts_resize_dev)
                } else {
                    app.parts_create_disk.clone()
                };
                if !disk.is_empty() {
                    let free_mib = (free_bytes(app, &disk) / (1024 * 1024)) as u32;
                    let own_mib = if app.parts_resize_dev.is_empty() {
                        0
                    } else {
                        planned_mib_of(&app.config, role, &app.parts_resize_dev)
                    };
                    // GROWING a slot on a disk whose remainder is already
                    // claimed takes that space from the rest-of-disk slot —
                    // the same silent shrink, one keystroke away by another
                    // route. Shrinking stays free: it only gives space back.
                    if let Some(rest) = rest_slot_on(&app.config, &disk) {
                        if rest != app.parts_resize_dev && mib > own_mib {
                            app.pmode_status = t(app.lang, "parts.err_disk_claimed")
                                .replace("{part}", &slot_name(app, &rest));
                            return;
                        }
                    }
                    let free_mib = size_budget_mib(free_mib, own_mib);
                    if free_mib > 0 && mib > free_mib {
                        app.pmode_status = t(app.lang, "parts.err_size_too_big")
                            .replace("{want}", &human_mib(mib))
                            .replace("{free}", &human_mib(free_mib));
                        return;
                    }
                }
            }
            // Resizing an existing plan: change the number in place. A data
            // partition already has its mountpoint, so it must NOT be sent back
            // through the mountpoint picker as if it were new.
            if !app.parts_resize_dev.is_empty() {
                let dev = app.parts_resize_dev.clone();
                if role == ROLE_DATA {
                    if let Some(dp) = app
                        .config
                        .manual_data_new
                        .iter_mut()
                        .find(|dp| dp.path == dev)
                    {
                        dp.mib = mib;
                    }
                    renumber_planned(app);
                } else {
                    create_slot(app, role, mib);
                }
                app.parts_resize_dev.clear();
                app.parts_create_open = false;
                return;
            }
            // A data partition needs one more answer — WHERE it mounts. Hand
            // over to the mountpoint picker; the partition is only created when
            // a mountpoint is confirmed there, so Esc still abandons cleanly.
            if role == ROLE_DATA {
                app.parts_pending_mib = mib;
                app.parts_target = app.parts_create_disk.clone();
                app.parts_mount_for_new = true;
                app.parts_mount_cursor = 0;
                app.parts_mount_input.clear();
                app.parts_create_open = false;
                app.parts_mount_open = true;
                return;
            }
            create_slot(app, role, mib);
            app.parts_create_open = false;
        }
        _ => {}
    }
}

/// Roles not yet assigned to any partition (existing or planned).
/// Does this layout need a BIOS-boot partition?
///
/// Only when booting legacy (non-UEFI) from a GPT disk: that is where GRUB has
/// nowhere else to embed core.img. An MBR table gives it the post-MBR gap, and
/// UEFI uses the ESP instead — offering the role in those cases would just be a
/// confusing extra step.
fn bios_boot_required(app: &App) -> bool {
    if app.config.boot_mode.is_uefi() {
        return false;
    }
    let target = app.config.manual_boot_disk.clone();
    if target.is_empty() {
        return false;
    }
    crate::screens::disk::disks()
        .iter()
        .find(|d| d.path == target)
        .map(|d| d.pttype.eq_ignore_ascii_case("gpt"))
        // No table yet means the plan will create a GPT, so the partition is
        // needed. Unknown disk: assume GPT, the safer of the two (asking for a
        // partition that turns out unnecessary is recoverable; booting without
        // one is not).
        .unwrap_or(true)
}

/// Which way the cursor was travelling, so a skipped label is stepped over in
/// the same direction rather than bouncing the user backwards.
fn moving_down(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Down | KeyCode::PageDown | KeyCode::End | KeyCode::Char('j')
    )
}

/// Step the cursor off any row that is a caption rather than a choice.
///
/// Only disk headers qualify: they name the drive the rows beneath belong to.
/// At the very top there is nothing above to fall back to, so the walk turns
/// around rather than parking on the label.
/// Move the cursor off a row that is only a caption.
///
/// A disk header IS a control when that disk can be wiped (it carries the erase
/// strip), so the cursor lands on it; the header of a disk that may not be wiped
/// — the live medium, or the drive the neighbouring OS boots from — is a plain
/// caption and gets skipped. `landable` answers that per disk.
fn skip_labels(rows: &[Row], cursor: &mut usize, down: bool, landable: &dyn Fn(&str) -> bool) {
    for _ in 0..rows.len() {
        let is_caption = match rows.get(*cursor) {
            Some(Row::DiskHeader { disk }) => !landable(disk),
            _ => false,
        };
        if !is_caption {
            return;
        }
        if down || *cursor == 0 {
            if *cursor + 1 >= rows.len() {
                break;
            }
            *cursor += 1;
        } else {
            *cursor -= 1;
        }
    }
}

/// Keep `manual_boot_disk` in step with whatever now holds the root.
///
/// Resolved from the SCAN — a partition's `parent` — or from `manual_disk` when
/// the root is one the editor is about to create. Never parsed out of a device
/// name: "/dev/nvme0n1" is a whole disk that ends in a digit, so names cannot
/// be told apart reliably, and this value decides where a bootloader is written.
fn refresh_boot_disk(app: &mut App) {
    let root = app.config.manual_root.clone();
    let from_scan = app
        .parts_list
        .iter()
        .find(|p| p.path == root)
        .map(|p| p.parent.clone());
    app.config.manual_boot_disk = match from_scan {
        Some(d) => d,
        // A planned root has no scan entry yet — take the disk the slot itself
        // records. Using `manual_disk` here meant "whichever disk the create
        // wizard was last opened on", which with multi-disk layouts could put
        // the bootloader on the wrong drive entirely.
        None if !root.is_empty() => planned_disk(&app.config, ROLE_ROOT),
        None => String::new(),
    };
}

fn available_roles(app: &App) -> Vec<usize> {
    let c = &app.config;
    let mut v = Vec::new();
    if c.manual_esp.is_empty() {
        v.push(ROLE_ESP);
    }
    if c.manual_root.is_empty() {
        v.push(ROLE_ROOT);
    }
    if c.manual_swap.is_empty() {
        v.push(ROLE_SWAP);
    }
    if c.manual_home.is_empty() {
        v.push(ROLE_HOME);
    }
    if bios_boot_required(app) && c.manual_bios.is_empty() {
        v.push(ROLE_BIOS);
    }
    // Plain data storage is not a single slot — any number of partitions can
    // be carved just to hold files, so the choice is always on offer.
    v.push(ROLE_DATA);
    v
}

fn planned_mib(c: &InstallConfig, role: usize) -> u32 {
    match role {
        ROLE_ESP => c.manual_esp_new_mib,
        ROLE_ROOT => c.manual_root_new_mib,
        ROLE_SWAP => c.manual_swap_new_mib,
        ROLE_BIOS => c.manual_bios_new_mib,
        ROLE_HOME => c.manual_home_new_mib,
        // Data partitions are not a single slot — `planned_mib_of` answers for
        // them, because it knows WHICH one. Falling through to /home here made
        // resizing a data partition change the size of /home instead.
        _ => 0,
    }
}

/// Planned size of the slot a specific DEVICE occupies. The five roles are one
/// slot each, so the role is enough; a data partition needs the device too,
/// since any number of them can be planned.
fn planned_mib_of(c: &InstallConfig, role: usize, dev: &str) -> u32 {
    if role == ROLE_DATA {
        return c
            .manual_data_new
            .iter()
            .find(|dp| dp.path == dev)
            .map(|dp| dp.mib)
            .unwrap_or(0);
    }
    planned_mib(c, role)
}

/// Disk a specific planned device is carved from — same reasoning as above.
fn planned_disk_of(c: &InstallConfig, role: usize, dev: &str) -> String {
    if role == ROLE_DATA {
        return c
            .manual_data_new
            .iter()
            .find(|dp| dp.path == dev)
            .map(|dp| dp.disk.clone())
            .unwrap_or_default();
    }
    planned_disk(c, role)
}

/// Point `role` at an existing partition (or clear it with None). The device
/// is first detached from any role it held; a planned slot that backed the
/// role is dropped — an existing partition replaces it.
fn assign_role(app: &mut App, dev: &str, role: Option<usize>) {
    let c = &mut app.config;
    if c.manual_esp == dev {
        c.manual_esp.clear();
        c.manual_esp_new_mib = 0;
        c.manual_esp_disk.clear();
        c.manual_esp_format = false;
    }
    if c.manual_root == dev {
        c.manual_root.clear();
        c.manual_root_new_mib = 0;
        c.manual_root_disk.clear();
    }
    if c.manual_swap == dev {
        c.manual_swap.clear();
        c.manual_swap_new_mib = 0;
        c.manual_swap_disk.clear();
    }
    if c.manual_home == dev {
        c.manual_home.clear();
        c.manual_home_new_mib = 0;
        c.manual_home_disk.clear();
    }
    if c.manual_bios == dev {
        c.manual_bios.clear();
        c.manual_bios_new_mib = 0;
        c.manual_bios_disk.clear();
    }
    clear_data_mount(c, dev);
    c.manual_data_new.retain(|dp| dp.path != dev);
    match role {
        Some(ROLE_ESP) => {
            c.manual_esp = dev.to_string();
            c.manual_esp_new_mib = 0;
            c.manual_esp_disk.clear();
            c.manual_esp_format = false; // reuse by default — a Windows ESP must survive
                                         // Mount defaults to /boot/efi (set in InstallConfig::default), which
                                         // fits both a small reused Windows ESP and a fresh one; ←/→ toggles.
        }
        Some(ROLE_ROOT) => {
            c.manual_root = dev.to_string();
            c.manual_root_new_mib = 0;
            c.manual_root_disk.clear();
            c.manual_root_disk.clear();
            if c.manual_root_fs.is_empty() {
                c.manual_root_fs = "ext4".into();
            }
        }
        Some(ROLE_SWAP) => {
            c.manual_swap = dev.to_string();
            c.manual_swap_new_mib = 0;
            c.manual_swap_disk.clear();
            c.manual_swap_disk.clear();
        }
        Some(ROLE_BIOS) => {
            c.manual_bios = dev.to_string();
            c.manual_bios_new_mib = 0;
            c.manual_bios_disk.clear();
        }
        Some(ROLE_HOME) => {
            c.manual_home = dev.to_string();
            c.manual_home_new_mib = 0;
            c.manual_home_disk.clear();
            c.manual_home_disk.clear();
            if c.manual_home_fs.is_empty() {
                c.manual_home_fs = "ext4".into();
            }
        }
        _ => {}
    }
    renumber_planned(app);
    refresh_boot_disk(app);
}

// ── disk wipe ──────────────────────────────────────────────────────────────
//
// Erasing a whole disk is the ONE place the manual editor is allowed to touch a
// disk beyond the partitions it was told about — deliberately opt-in, behind a
// dedicated modal. "Later" records a mark that the install plan runs first
// (respecting the plan/execute model); "now" runs the erase from the editor,
// behind a strong confirmation, then rescans so the disk shows up empty.
// Either way a wiped disk numbers its new partitions from 1.

use crate::app::{WipeMark, WipeMethod};

/// The disk a landable row belongs to — a partition's parent, or the disk of a
/// "+ Create" row. Headers are not landable, so the cursor reaches a disk
/// through one of its partition/create rows. `None` for BootMode / Continue.
fn row_disk(app: &App, row: &Row) -> Option<String> {
    match row {
        Row::DiskHeader { disk } | Row::Create { disk } => Some(disk.clone()),
        Row::Part { path, .. } => app
            .parts_list
            .iter()
            .find(|p| p.path == *path)
            .map(|p| p.parent.clone())
            .or_else(|| {
                planned_slots(&app.config)
                    .into_iter()
                    .find(|s| s.path == *path)
                    .map(|s| s.disk)
            }),
        _ => None,
    }
}

/// A disk's rotational flag and bus transport from the scan (for the erase
/// method default and the nvme-vs-blkdiscard choice).
fn disk_hw(disk: &str) -> (bool, String) {
    crate::screens::disk::disks()
        .iter()
        .find(|d| d.path == disk)
        .map(|d| (d.rota, d.tran.clone()))
        .unwrap_or((false, String::new()))
}

fn disk_is_live(disk: &str) -> bool {
    crate::screens::disk::disks()
        .iter()
        .any(|d| d.path == disk && d.is_live)
}

/// The erase methods OFFERED for a disk, given its rotational kind — the
/// safeguard against using the wrong tool on the wrong medium.
///
/// - A spinning HDD never offers the SSD crypto/TRIM erase (`blkdiscard` is a
///   no-op on a disk that cannot discard).
/// - An SSD never offers the zero-fill: writing the whole drive wears the flash
///   for a WORSE erase (wear-levelling relocates cells, so the zeros miss data),
///   which is exactly what the user asked to prevent.
///
/// Quick (table only) and ATA Secure Erase (controller-driven; NVMe falls back
/// to blkdiscard) suit both, so both media offer them.
fn wipe_methods_for(rota: bool) -> Vec<WipeMethod> {
    if rota {
        vec![
            WipeMethod::Quick,
            WipeMethod::ZeroHdd,
            WipeMethod::AtaSecure,
        ]
    } else {
        vec![
            WipeMethod::Quick,
            WipeMethod::CryptoSsd,
            WipeMethod::AtaSecure,
        ]
    }
}

/// The wipe "slot" a disk is in: 0 = nothing, 1..=N index into the disk's
/// OFFERED methods (`wipe_methods_for`). The header ←/→ strip cycles through
/// these; the state IS `manual_wipe`.
fn current_wipe_index(app: &App, disk: &str) -> usize {
    let (rota, _) = disk_hw(disk);
    match app.config.manual_wipe.iter().find(|w| w.disk == disk) {
        None => 0,
        Some(w) => wipe_methods_for(rota)
            .iter()
            .position(|m| *m == w.method)
            .map(|i| i + 1)
            .unwrap_or(0),
    }
}

/// The strip labels for a disk: "nothing" first, then its offered methods —
/// which differ for an HDD vs an SSD, so the inapplicable one is never shown.
fn wipe_strip_labels(app: &App, disk: &str) -> Vec<String> {
    let (rota, _) = disk_hw(disk);
    let mut v = vec![t(app.lang, "parts.wipe_none")];
    for m in wipe_methods_for(rota) {
        v.push(t(app.lang, &format!("parts.wipe_s_{}", m.id())));
    }
    v
}

/// Apply strip index `idx` to `disk`: 0 clears the mark, else marks the offered
/// method at that position and resets the disk to empty for numbering.
fn set_disk_wipe(app: &mut App, disk: &str, idx: usize) {
    let (rota, tran) = disk_hw(disk);
    let methods = wipe_methods_for(rota);
    if idx == 0 || idx > methods.len() {
        unmark_wipe(app, disk);
        return;
    }
    let method = methods[idx - 1];
    app.config.manual_wipe.retain(|w| w.disk != disk);
    app.config.manual_wipe.push(WipeMark {
        disk: disk.to_string(),
        method,
        rota,
        tran,
    });
    clear_disk_after_wipe(app, disk);
    renumber_planned(app);
    app.pmode_status = t(app.lang, "parts.wipe_marked");
}

/// Why this disk may not be wiped, or `None` when it may.
///
/// Exactly ONE disk is off limits, and not as policy: the live medium is the
/// installer we are running from, and erasing it kills the running install.
///
/// Everything else — including the drive another OS boots from, including
/// Windows — is the user's to erase. They may be replacing that system on
/// purpose: Artix can be the FIRST system installed, with another Linux added by
/// hand later. So a drive that holds someone else's OS is not refused, it is
/// EXPLAINED: `foreign_parts` names what is about to be lost and an
/// acknowledgement modal makes the user say they understand (see
/// `needs_wipe_ack`). A warning that can be read beats a rule that cannot be
/// overridden.
fn wipe_refusal(disk: &str) -> Option<&'static str> {
    if disk_is_live(disk) {
        return Some("parts.wipe_refuse_live");
    }
    None
}

/// True when `disk` is what the OTHER operating system currently boots from —
/// it carries the ESP we were going to reuse, or it is the boot disk. Not a
/// veto: it adds a line to the acknowledgement saying that system will no longer
/// start, which is the part a user is most likely to have overlooked.
fn is_neighbour_boot_disk(app: &App, disk: &str) -> bool {
    if app.config.manual_solo {
        return false;
    }
    let esp_disk = app
        .parts_list
        .iter()
        .find(|p| p.path == app.config.manual_esp)
        .map(|p| p.parent.clone())
        .unwrap_or_default();
    (!esp_disk.is_empty() && esp_disk == disk)
        || (!app.config.manual_boot_disk.is_empty() && app.config.manual_boot_disk == disk)
}

/// Whether selecting a wipe on `disk` must first be acknowledged: it holds
/// another operating system's data, and the user has not yet said they
/// understand this run. Asked ONCE per disk — cycling between methods afterwards
/// is not a new decision, and a modal on every keypress would train the user to
/// dismiss it unread.
fn needs_wipe_ack(app: &App, disk: &str) -> bool {
    !app.parts_wipe_acked.iter().any(|d| d == disk) && !foreign_parts(app, disk).is_empty()
}

/// Partitions on `disk` that belong to something we are NOT installing: another
/// system's bootloader, or a Windows volume. Wiping the disk destroys them, so
/// they are named before the erase rather than discovered afterwards.
///
/// Recognised by GPT partition TYPE (an ESP is a type, not a size or a label)
/// and by filesystem for NTFS. Returns (path, localized description).
fn foreign_parts(app: &App, disk: &str) -> Vec<(String, String)> {
    use crate::system::disk as sysdisk;
    app.parts_list
        .iter()
        .filter(|p| p.parent == disk)
        .filter_map(|p| {
            let key = match p.parttype.as_str() {
                x if x == sysdisk::PARTTYPE_ESP => "parts.foreign_esp",
                x if x == sysdisk::PARTTYPE_BIOS_BOOT => "parts.foreign_bios",
                x if x == sysdisk::PARTTYPE_MSR || x == sysdisk::PARTTYPE_WINRE => {
                    "parts.foreign_windows"
                }
                _ if p.fstype == "ntfs" => "parts.foreign_ntfs",
                _ => return None,
            };
            Some((p.path.clone(), t(app.lang, key)))
        })
        .collect()
}

/// ←/→ on a disk header: move the wipe selection one step (wrapping) through the
/// disk's OFFERED methods.
fn cycle_disk_wipe(app: &mut App, disk: &str, forward: bool) {
    if let Some(reason) = wipe_refusal(disk) {
        app.pmode_status = t(app.lang, reason);
        return;
    }
    let (rota, _) = disk_hw(disk);
    let n = wipe_methods_for(rota).len() + 1;
    let cur = current_wipe_index(app, disk);
    let next = if forward {
        (cur + 1) % n
    } else {
        (cur + n - 1) % n
    };
    // Turning a wipe ON for a disk that holds another operating system stops to
    // spell out what will be destroyed. Turning it OFF (back to "nothing") never
    // asks — undoing a destructive choice needs no ceremony.
    if next != 0 && needs_wipe_ack(app, disk) {
        app.parts_wipe_ack_open = true;
        app.parts_wipe_ack_disk = disk.to_string();
        app.parts_wipe_ack_idx = next;
        app.parts_wipe_ack_cursor = 0; // default: cancel
        return;
    }
    set_disk_wipe(app, disk, next);
}

/// The acknowledgement: ←/→ picks, Enter acts, Esc cancels. Confirming records
/// that this disk was acknowledged, so choosing among methods afterwards is free.
fn wipe_ack_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.parts_wipe_ack_open = false,
        KeyCode::Left
        | KeyCode::Right
        | KeyCode::Up
        | KeyCode::Down
        | KeyCode::Char('h')
        | KeyCode::Char('l')
        | KeyCode::Char('j')
        | KeyCode::Char('k') => app.parts_wipe_ack_cursor ^= 1,
        KeyCode::Enter => {
            app.parts_wipe_ack_open = false;
            if app.parts_wipe_ack_cursor == 1 {
                let disk = app.parts_wipe_ack_disk.clone();
                let idx = app.parts_wipe_ack_idx;
                app.parts_wipe_acked.push(disk.clone());
                set_disk_wipe(app, &disk, idx);
            }
        }
        _ => {}
    }
}

/// Enter on a disk header: raise the strong "erase NOW" confirm — only when a
/// method is actually selected (otherwise there is nothing to run).
fn open_wipe_now_confirm(app: &mut App, disk: &str) {
    if current_wipe_index(app, disk) == 0 {
        app.pmode_status = t(app.lang, "parts.wipe_pick_first");
        return;
    }
    app.parts_wipe_disk = disk.to_string();
    app.parts_wipe_open = true;
    app.parts_wipe_confirm_cursor = 0; // default: cancel
}

fn close_wipe_modal(app: &mut App) {
    app.parts_wipe_open = false;
}

/// Everything referencing an EXISTING partition on a wiped disk is stale: the
/// erase removes those partitions, so their roles, data mounts and deletion
/// marks go with them. New partitions planned on the disk are untouched.
fn clear_disk_after_wipe(app: &mut App, disk: &str) {
    let existing: Vec<String> = app
        .parts_list
        .iter()
        .filter(|p| p.parent == disk)
        .map(|p| p.path.clone())
        .collect();
    for path in &existing {
        assign_role(app, path, None);
    }
    app.config.manual_deleted.retain(|p| p.disk != disk);
}

/// Drop a disk's wipe mark.
fn unmark_wipe(app: &mut App, disk: &str) {
    app.config.manual_wipe.retain(|w| w.disk != disk);
    renumber_planned(app);
    app.pmode_status = t(app.lang, "parts.wipe_unmarked");
}

/// Start a "now" wipe: erase the disk immediately, streaming into the overlay.
/// The method is the one already selected on the header (held in `manual_wipe`).
fn start_wipe_now(app: &mut App) {
    let disk = app.parts_wipe_disk.clone();
    let method = app
        .config
        .manual_wipe
        .iter()
        .find(|w| w.disk == disk)
        .map(|w| w.method)
        .unwrap_or(WipeMethod::Quick);
    let (rota, tran) = disk_hw(&disk);
    // A "now" wipe MAY suspend to thaw a frozen ATA drive: no install is running
    // yet, so the brief rtcwake suspend is safe here (never mid-install).
    let actions = crate::system::disk::wipe_actions(&disk, method, rota, &tran, true);
    // The existing partitions really are about to go.
    clear_disk_after_wipe(app, &disk);
    app.config.manual_wipe.retain(|w| w.disk != disk); // a now-wipe supersedes a mark
    renumber_planned(app);

    app.wipe_run_disk = disk;
    app.wipe_run_method = method;
    app.wipe_run_log.clear();
    app.wipe_run_done = None;
    if let Some(a) = actions.into_iter().next() {
        let argref: Vec<&str> = a.args.iter().map(|s| s.as_str()).collect();
        app.wipe_run_rx = Some(crate::system::runner::spawn_with_mode(
            &a.program, &argref, false,
        ));
    } else {
        app.wipe_run_done = Some(Ok(()));
    }
}

/// The erase confirm: ↑/↓ picks cancel / during-install / now, Enter acts, Esc closes.
/// The cursor defaults to cancel, so a careless Enter never erases a disk.
fn wipe_modal_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => close_wipe_modal(app),
        KeyCode::Right | KeyCode::Down | KeyCode::Char('l') | KeyCode::Char('j') => {
            app.parts_wipe_confirm_cursor = (app.parts_wipe_confirm_cursor + 1) % 3
        }
        KeyCode::Up | KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('k') => {
            app.parts_wipe_confirm_cursor = (app.parts_wipe_confirm_cursor + 2) % 3
        }
        KeyCode::Enter => {
            match app.parts_wipe_confirm_cursor {
                // "No, cancel" answers the question on screen — ERASE EVERYTHING
                // on this disk? — so it takes the mark back off. Leaving it set
                // would mean the install still erased the disk after the user
                // said no, which is the one outcome this dialog exists to avoid.
                0 => {
                    let disk = app.parts_wipe_disk.clone();
                    set_disk_wipe(app, &disk, 0);
                }
                // Keep the mark: the plan erases it as its first step.
                1 => {}
                _ => start_wipe_now(app),
            }
            close_wipe_modal(app);
        }
        _ => {}
    }
}

/// The "now" wipe overlay: keys are ignored while the erase runs (it must not be
/// interrupted half-written); once done, Enter/Esc dismisses it and forces a
/// rescan so the now-empty disk reappears.
fn wipe_run_key(app: &mut App, key: KeyEvent) {
    if app.wipe_run_done.is_some()
        && matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' '))
    {
        app.wipe_run_done = None;
        app.wipe_run_log.clear();
        app.parts_scanned = false;
    }
}

/// Drain the "now" wipe channel. Same shape as the installer's — the UI thread
/// never waits, so the spinner keeps moving.
pub fn tick(app: &mut App) {
    let Some(rx) = app.wipe_run_rx.clone() else {
        return;
    };
    use crate::system::runner::LogLine;
    while let Ok(line) = rx.try_recv() {
        match line {
            LogLine::Out(s) | LogLine::Err(s) | LogLine::Prompt(s) => {
                app.wipe_run_log.push(s);
                if app.wipe_run_log.len() > 500 {
                    app.wipe_run_log.remove(0);
                }
            }
            LogLine::Done(res) => {
                app.wipe_run_rx = None;
                app.wipe_run_done = Some(res);
                return;
            }
        }
    }
}

/// Plan a NEW partition for `role` (or update its size) and predict its path.
fn create_slot(app: &mut App, role: usize, mib: u32) {
    // Remember WHICH disk this slot is carved from — that is what lets root and
    // a fresh /home live on different drives.
    let target = app.parts_create_disk.clone();
    set_planned_disk(&mut app.config, role, &target);
    let c = &mut app.config;
    match role {
        ROLE_ESP => {
            c.manual_esp_new_mib = mib;
            c.manual_esp_format = true; // it's new — always formatted
        }
        ROLE_ROOT => {
            c.manual_root_new_mib = mib;
            if c.manual_root_fs.is_empty() {
                c.manual_root_fs = "ext4".into();
            }
        }
        ROLE_SWAP => c.manual_swap_new_mib = mib,
        ROLE_BIOS => c.manual_bios_new_mib = mib,
        _ => {
            c.manual_home_new_mib = mib;
            if c.manual_home_fs.is_empty() {
                c.manual_home_fs = "ext4".into();
            }
        }
    }
    renumber_planned(app);
    refresh_boot_disk(app);
}

fn delete_planned(app: &mut App, path: &str) {
    let c = &mut app.config;
    if c.manual_esp == path && c.manual_esp_new_mib > 0 {
        c.manual_esp.clear();
        c.manual_esp_new_mib = 0;
        c.manual_esp_disk.clear();
        c.manual_esp_format = false;
    } else if c.manual_root == path && c.manual_root_new_mib > 0 {
        c.manual_root.clear();
        c.manual_root_new_mib = 0;
        c.manual_root_disk.clear();
    } else if c.manual_swap == path && c.manual_swap_new_mib > 0 {
        c.manual_swap.clear();
        c.manual_swap_new_mib = 0;
        c.manual_swap_disk.clear();
    } else if c.manual_home == path && c.manual_home_new_mib > 0 {
        c.manual_home.clear();
        c.manual_home_new_mib = 0;
        c.manual_home_disk.clear();
    } else if c.manual_bios == path && c.manual_bios_new_mib > 0 {
        c.manual_bios.clear();
        c.manual_bios_new_mib = 0;
        c.manual_bios_disk.clear();
    } else if c.manual_data_new.iter().any(|dp| dp.path == path) {
        // A planned data partition: drop the creation AND its mount entry —
        // the entry is keyed by a path that will now belong to nothing.
        c.manual_data_new.retain(|dp| dp.path != path);
        c.extra_disks.retain(|e| e.disk != path);
    }
    renumber_planned(app);
    refresh_boot_disk(app);
}

/// Recompute the predicted device paths of every planned partition: fixed-size
/// slots first in role order, the rest-of-disk slot last, numbered after the
/// highest existing partition on the disk. Deterministic — the plan creates
/// them in exactly this order, so path and reality meet.
fn renumber_planned(app: &mut App) {
    // Numbering is done PER DISK, assigning each planned partition the LOWEST
    // number that disk has free — fixed sizes first, the rest-of-disk slot last
    // (the order the plan creates them in).
    //
    // Per disk, because new partitions are no longer confined to one: root can
    // be carved from one drive and a fresh /home from another, which is an
    // ordinary layout and used to be refused outright.
    //
    // "Free" means not held by a SURVIVING partition — a number freed by a
    // deletion is reclaimable, so resizing a partition (delete it, create a new
    // one) keeps its number instead of being pushed to the end. This is safe
    // because the plan never deletes by number: it deletes the marked PARTUUID
    // wherever it sits and skips if it is gone, and a create refuses to clobber a
    // slot it does not own (see build_manual_plan). What gets a number: the five
    // role slots, and every planned data partition (its index in manual_data_new).
    #[derive(Clone, Copy)]
    enum Slot {
        Role(usize),
        Data(usize),
    }

    let mut disks: Vec<String> = Vec::new();
    let mut renames: Vec<(String, String)> = Vec::new();
    for role in [ROLE_ESP, ROLE_BIOS, ROLE_ROOT, ROLE_SWAP, ROLE_HOME] {
        let d = planned_disk(&app.config, role);
        if planned_mib(&app.config, role) > 0 && !d.is_empty() && !disks.contains(&d) {
            disks.push(d);
        }
    }
    for dp in &app.config.manual_data_new {
        if dp.mib > 0 && !dp.disk.is_empty() && !disks.contains(&dp.disk) {
            disks.push(dp.disk.clone());
        }
    }

    for d in disks {
        // Numbers held by partitions that SURVIVE on this disk (existing ones the
        // user did not delete). A disk marked for a full wipe is erased first, so
        // it keeps none — its new partitions number from 1. Everything a deletion
        // freed is available for reuse.
        let wiped = disk::wipe_applies(&app.config, &d);
        let mut used: std::collections::BTreeSet<u32> = if wiped {
            std::collections::BTreeSet::new()
        } else {
            app.parts_list
                .iter()
                .filter(|p| p.parent == d && !is_deleted(&app.config, &p.path))
                .filter_map(|p| disk::part_number(&d, &p.path))
                .collect()
        };
        let mut order: Vec<Slot> = Vec::new();
        for rest in [false, true] {
            for role in [ROLE_ESP, ROLE_BIOS, ROLE_ROOT, ROLE_SWAP, ROLE_HOME] {
                let mib = planned_mib(&app.config, role);
                if mib == 0 || planned_disk(&app.config, role) != d {
                    continue;
                }
                if (mib == MANUAL_REST) == rest {
                    order.push(Slot::Role(role));
                }
            }
            for (i, dp) in app.config.manual_data_new.iter().enumerate() {
                if dp.mib == 0 || dp.disk != d {
                    continue;
                }
                if (dp.mib == MANUAL_REST) == rest {
                    order.push(Slot::Data(i));
                }
            }
        }
        for slot in order {
            // Lowest number this disk has free (not held by a survivor, not yet
            // taken by an earlier planned slot in this pass).
            let mut n = 1;
            while used.contains(&n) {
                n += 1;
            }
            used.insert(n);
            let path = disk::part(&d, n);
            let c = &mut app.config;
            match slot {
                Slot::Role(ROLE_ESP) => c.manual_esp = path,
                Slot::Role(ROLE_BIOS) => c.manual_bios = path,
                Slot::Role(ROLE_ROOT) => c.manual_root = path,
                Slot::Role(ROLE_SWAP) => c.manual_swap = path,
                Slot::Role(_) => c.manual_home = path,
                Slot::Data(i) => {
                    let old = c.manual_data_new[i].path.clone();
                    c.manual_data_new[i].path = path.clone();
                    if !old.is_empty() && old != path {
                        renames.push((old, path));
                    }
                }
            }
        }
    }

    // The mount entry in `extra_disks` is keyed by the device path, so when
    // renumbering moves a data partition its entry must move with it — or the
    // plan would create /dev/vdb3 and then mkfs+mount a /dev/vdb2 that belongs
    // to something else. Applied as ONE sweep against the ORIGINAL paths:
    // renaming entries one at a time could hit a freshly renamed neighbour
    // (vdb2→vdb3 first, then vdb3→vdb4 would grab the wrong entry).
    for e in &mut app.config.extra_disks {
        if let Some((_, new)) = renames.iter().find(|(old, _)| *old == e.disk) {
            e.disk = new.clone();
        }
    }
}

/// The unit label shown next to the size box.
fn size_unit_label(app: &App) -> &'static str {
    if app.parts_size_mib {
        "MiB"
    } else {
        "GiB"
    }
}

fn cycle_fs(cur: &str, dir: i32) -> String {
    let i = FS_CHOICES.iter().position(|f| *f == cur).unwrap_or(0) as i32;
    let n = FS_CHOICES.len() as i32;
    FS_CHOICES[((i + dir).rem_euclid(n)) as usize].to_string()
}

pub fn footer_hint(app: &App) -> String {
    if app.fs_opts_modal_open {
        return t(app.lang, "disk.fsopt_modal_hint");
    }
    if app.parts_modal_open {
        return t(app.lang, "parts.hint_modal");
    }
    if app.parts_create_open {
        // The wizard has two stages and they take different keys. Both used to
        // show the SIZE hint, so the role list — SWAP / /home / Data, a plain
        // up-down list — was captioned "digits — size · ←/→ — MiB/GiB", which
        // describes the step after this one.
        return if app.parts_create_stage == 0 {
            t(app.lang, "parts.hint_role")
        } else {
            t(app.lang, "parts.hint_size")
        };
    }
    let rows = build_rows(app);
    let key = match rows.get(app.cursor.min(rows.len().saturating_sub(1))) {
        Some(Row::BootMode) => "parts.hint_boot",
        // The ESP row's ←/→ toggles the mountpoint (/boot vs /boot/efi), not a
        // filesystem — it earns its own hint so that control is discoverable.
        Some(Row::Part { path, .. }) if role_of(&app.config, path) == Some(ROLE_ESP) => {
            "parts.hint_esp"
        }
        // The Data role is the only one that can be encrypted from here, so it
        // is the only row where saying so is useful.
        Some(Row::Part { path, .. }) if role_of(&app.config, path) == Some(ROLE_DATA) => {
            "parts.hint_data"
        }
        Some(Row::Part { path, .. })
            if matches!(
                role_of(&app.config, path),
                Some(ROLE_ROOT) | Some(ROLE_HOME)
            ) =>
        {
            "parts.hint_fs"
        }
        // A data partition being formatted has the same ←/→ and [o] controls, so
        // it earns the same hint. A preserved one does not — it keeps whatever
        // filesystem it holds, and the generic row hint is the honest one.
        Some(Row::Part { path, .. })
            if role_of(&app.config, path) == Some(ROLE_DATA)
                && data_is_formatted(&app.config, path) =>
        {
            "parts.hint_fs"
        }
        // A PLANNED data partition is the one people create for storage, and
        // encrypting it is the whole reason to make it here rather than later.
        // Its hint said nothing about `e`, so the feature existed and was
        // invisible on exactly the row where it is most wanted.
        Some(Row::Part {
            path, is_new: true, ..
        }) if role_of(&app.config, path) == Some(ROLE_DATA) => "parts.hint_new_data",
        Some(Row::Part { is_new: true, .. }) => "parts.hint_new",
        Some(Row::Part { .. }) => "parts.hint_part",
        Some(Row::Create { .. }) => "parts.hint_create",
        // A landable disk header (solo only) carries the wipe strip.
        Some(Row::DiskHeader { .. }) => "parts.wipe_hint_strip",
        _ => "parts.hint",
    };
    // While edits are pending, say what Esc will do — it steps BACK through
    // them rather than leaving, and that is not guessable.
    if app.parts_undo.is_empty() {
        t(app.lang, key)
    } else {
        format!(
            "{}   |   {}",
            t(app.lang, key),
            t(app.lang, "parts.hint_undo").replace("{n}", &app.parts_undo.len().to_string())
        )
    }
}

// ── validation ──────────────────────────────────────────────────────────────

/// `validate` with the App's own scan, extra-disk list and disk size plugged in.
fn validate_app(app: &App) -> Result<(), (&'static str, String)> {
    let extra: Vec<String> = app
        .config
        .extra_disks
        .iter()
        .map(|d| d.disk.clone())
        .collect();
    let disk_bytes = crate::screens::disk::disks()
        .iter()
        .find(|d| d.path == app.config.manual_disk)
        .map(|d| d.size_bytes)
        .unwrap_or(0);
    let mbr: Vec<String> = crate::screens::disk::disks()
        .iter()
        .filter(|d| d.pttype.eq_ignore_ascii_case("dos"))
        .map(|d| d.path.clone())
        .collect();
    validate(
        &app.config,
        &app.parts_list,
        &extra,
        disk_bytes,
        bios_boot_required(app),
        &mbr,
    )
}

/// The rules of manual mode, as one pure function.
///
/// Returns the i18n key of the FIRST broken rule (plus the offending device
/// for the "{dev}" placeholder), so the status line always names one concrete
/// thing to fix. Pure on purpose: every rule below is unit-tested with
/// fixture partitions, no disk required. `disk_bytes` is the total size of
/// `manual_disk` (0 = unknown → the free-space check is skipped).
pub fn validate(
    c: &InstallConfig,
    parts: &[Partition],
    extra_disks: &[String],
    disk_bytes: u64,
    bios_needs_boot_part: bool,
    // Disks carrying an MBR ("dos") table — new partitions are refused there.
    mbr_disks: &[String],
) -> Result<(), (&'static str, String)> {
    // Legacy (BIOS) manual installs are supported: old machines, and firmware
    // whose UEFI is too broken to trust, still need a way in. What changes is
    // WHICH boot partition is required — an ESP is a UEFI concept.
    if c.boot_mode.is_uefi() {
        if c.manual_esp.is_empty() {
            return Err(("parts.st_need_esp", String::new()));
        }
    } else if bios_needs_boot_part && c.manual_bios.is_empty() {
        // GPT + BIOS: GRUB has nowhere to embed core.img without a 1 MiB EF02
        // partition. (On MBR it uses the post-MBR gap, so nothing is asked.)
        return Err(("parts.st_need_biosboot", String::new()));
    }
    if c.manual_root.is_empty() {
        return Err(("parts.st_need_root", String::new()));
    }
    if !c.boot_mode.is_uefi() && c.manual_boot_disk.is_empty() {
        // BIOS GRUB installs to a whole disk, so we must know which one.
        return Err(("parts.st_need_bootdisk", String::new()));
    }

    // New-slot bookkeeping — over EVERY planned partition, the five role slots
    // and the data-storage ones alike. (`planned_slots` is what the sgdisk pass
    // is built from, so validating anything narrower would leave holes.)
    let slots = planned_slots(c);
    let any_new = !slots.is_empty()
        || c.manual_data_new.iter().any(|dp| dp.mib > 0)
        || [
            c.manual_esp_new_mib,
            c.manual_root_new_mib,
            c.manual_swap_new_mib,
            c.manual_home_new_mib,
            c.manual_bios_new_mib,
        ]
        .iter()
        .any(|m| *m > 0);
    // Every planned partition must know which disk it is carved from.
    for role in [ROLE_ESP, ROLE_BIOS, ROLE_ROOT, ROLE_SWAP, ROLE_HOME] {
        if planned_mib(c, role) > 0 && planned_disk(c, role).is_empty() {
            return Err(("parts.st_need_disk", String::new()));
        }
    }
    // Creating on an MBR ("dos") table is refused. `sgdisk -n` on such a disk
    // CONVERTS the table to GPT — which is exactly how a dual-boot Windows on
    // an old machine gets lost. Assigning roles to its existing partitions is
    // fine; making new ones has to happen in cfdisk first (console 2).
    for p in &slots {
        if mbr_disks.contains(&p.disk) {
            return Err(("parts.st_mbr_create", p.disk.clone()));
        }
    }
    // "The rest of the disk" is per DISK — two drives may each have one, but
    // one drive cannot have two.
    for p in slots.iter().filter(|p| p.mib == MANUAL_REST) {
        let rests = slots
            .iter()
            .filter(|q| q.mib == MANUAL_REST && q.disk == p.disk)
            .count();
        if rests > 1 {
            return Err(("parts.st_two_rest", p.disk.clone()));
        }
    }
    // A new ESP below 64 MiB cannot hold FAT32 + a kernel.
    if c.manual_esp_new_mib > 0 && c.manual_esp_new_mib != MANUAL_REST && c.manual_esp_new_mib < 64
    {
        return Err(("parts.st_esp_small", String::new()));
    }

    // One partition, one role.
    let assigned: Vec<&String> = [
        &c.manual_esp,
        &c.manual_root,
        &c.manual_swap,
        &c.manual_home,
        &c.manual_bios,
    ]
    .into_iter()
    .filter(|d| !d.is_empty())
    .collect();
    for (i, a) in assigned.iter().enumerate() {
        if assigned.iter().skip(i + 1).any(|b| b == a) {
            return Err(("parts.st_dup", (*a).clone()));
        }
    }

    for dev in &assigned {
        let parent = parent_of(parts, dev);
        // The LUKS key stick is sacred — same rule the auto path enforces.
        if !c.usb_key_device.is_empty() && (**dev == c.usb_key_device || parent == c.usb_key_device)
        {
            return Err(("parts.st_key", (*dev).clone()));
        }
        // A disk the "extra disks" step will wipe cannot also carry a manual
        // role: extra_disks_plan would format it AFTER we mounted it.
        if extra_disks.iter().any(|e| e == *dev || *e == parent) {
            return Err(("parts.st_extra", (*dev).clone()));
        }
    }

    // Reusing a non-FAT "ESP" cannot boot; either format it or pick the real
    // one. Only checkable when the scan actually knows the partition (a NEW
    // ESP is not in the scan and is always formatted).
    if !c.manual_esp_format {
        if let Some(p) = parts.iter().find(|p| p.path == c.manual_esp) {
            if p.fstype != "vfat" {
                return Err(("parts.st_esp_not_vfat", c.manual_esp.clone()));
            }
        }
    }

    // The planned partitions must fit in the disk's free space (best-effort;
    // skipped when the disk size is unknown). The rest-of-disk slot needs at
    // least 1 GiB left over to be worth creating.
    // Checked per disk, and only for the one whose size the caller resolved —
    // `disk_bytes` describes `manual_disk`, the disk of the create wizard.
    if any_new && disk_bytes > 0 {
        let d = c.manual_disk.clone();
        let mine: Vec<&Planned> = slots.iter().filter(|p| p.disk == d).collect();
        if !mine.is_empty() {
            // A partition marked for deletion gives its space back.
            let used: u64 = parts
                .iter()
                .filter(|p| p.parent == d && !c.manual_deleted.iter().any(|x| x.path == p.path))
                .map(|p| p.size_bytes)
                .sum();
            let fixed: u64 = mine
                .iter()
                .filter(|p| p.mib != MANUAL_REST)
                .map(|p| p.mib as u64 * 1024 * 1024)
                .sum();
            let rest_min: u64 = if mine.iter().any(|p| p.mib == MANUAL_REST) {
                1024 * 1024 * 1024
            } else {
                0
            };
            let overhead = 2 * 1024 * 1024;
            if used + fixed + rest_min + overhead > disk_bytes {
                return Err(("parts.st_no_space", d));
            }
        }
    }

    Ok(())
}

/// Parent disk of a partition: from the scan when known, otherwise the
/// classic suffix heuristic (sda2 → sda, nvme0n1p2 → nvme0n1).
fn parent_of(parts: &[Partition], dev: &str) -> String {
    if let Some(p) = parts.iter().find(|p| p.path == dev) {
        return p.parent.clone();
    }
    let trimmed = dev.trim_end_matches(|ch: char| ch.is_ascii_digit());
    if trimmed.ends_with('p')
        && trimmed.len() < dev.len()
        && trimmed[..trimmed.len() - 1].ends_with(|ch: char| ch.is_ascii_digit())
    {
        trimmed[..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    /// A data partition that already holds something can still be encrypted —
    /// after saying yes to formatting it.
    ///
    /// This was unreachable. Such a partition is mounted as-is (format=false),
    /// and both `e` and ←/→ refuse while that holds, so "encrypt the disk I keep
    /// junk on" could only be done by deleting the partition and making a new
    /// one. The rule is not to forbid it but to say what it costs: encryption
    /// rewrites the whole device, so what is on it goes.
    /// The creation box must caption the stage it is SHOWING.
    ///
    /// It appended the size hint unconditionally, so the role list — SWAP,
    /// /home, Data, a plain up-down list — was captioned "digits — size ·
    /// ←/→ — MiB/GiB", which describes the step after this one. The footer had
    /// already been fixed for exactly this; the box carried its own copy and
    /// was missed, so the bug looked fixed and was not.
    /// THE FIRST DIGIT REPLACES THE SUGGESTION IN THE SIZE BOX.
    ///
    /// Found by driving the installer: the swap box is pre-filled with "4", a
    /// typed 4 produced 44, and a 44 GiB swap was written down on a 30 GiB disk
    /// without a word. The ESP box holds "512", so a typed 40 would have made
    /// 51240. The same mistake was found and fixed in the VM picker; this field
    /// was a separate copy and kept it.
    /// AN ENCRYPTED /home SAYS SO IN THE TABLE.
    ///
    /// The [ШИФР] mark was drawn only for data partitions, which keep the flag
    /// in `extra_disks`. /home keeps its own in `manual_home_encrypt`, so
    /// turning encryption on there produced a single status line and then
    /// nothing: the row looked identical either way, and pressing `e` again to
    /// find out would silently turn it back off. Found by driving the installer
    /// and walking the cursor away and back.
    #[test]
    fn an_encrypted_home_is_marked_in_the_table() {
        let src = std::fs::read_to_string("src/screens/parts.rs").expect("readable");
        let code = &src[..src.find("#[cfg(test)]").expect("test module")];
        assert!(
            code.contains("c.manual_home_encrypt") && code.contains("parts.enc_mark"),
            "the /home flag reaches the row that draws the mark"
        );
    }

    #[test]
    fn the_first_digit_replaces_the_suggested_size() {
        let mut app = App::new();
        app.parts_create_open = true;
        app.parts_create_stage = 1;
        app.parts_size_input = "4".into();
        app.parts_size_pristine = true;
        handle_key(&mut app, key(KeyCode::Char('8')));
        assert_eq!(
            app.parts_size_input, "8",
            "the suggestion was extended, not replaced"
        );
        // Once typing has started, further digits append normally.
        handle_key(&mut app, key(KeyCode::Char('0')));
        assert_eq!(app.parts_size_input, "80");
    }

    /// Backspace on an untouched suggestion clears it whole. Deleting one digit
    /// from a number nobody typed leaves a number nobody chose — "512" becomes
    /// "51", which is a perfectly valid ESP size and completely wrong.
    #[test]
    fn backspace_on_an_untouched_suggestion_clears_it() {
        let mut app = App::new();
        app.parts_create_open = true;
        app.parts_create_stage = 1;
        app.parts_size_input = "512".into();
        app.parts_size_pristine = true;
        handle_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.parts_size_input, "", "one digit was shaved off instead");
    }

    #[test]
    fn the_creation_box_captions_the_stage_it_is_showing() {
        let src = std::fs::read_to_string("src/screens/parts.rs").expect("readable");
        let code = &src[..src.find("#[cfg(test)]").expect("test module")];
        let f = &code[code.find("fn draw_create_modal").expect("it exists")..];
        let f = &f[..f.find("\nfn ").unwrap_or(f.len())];
        assert!(
            f.contains("parts.hint_role"),
            "the role stage gets the role hint"
        );
        // Assembled from pieces on purpose: spelled out in full this literal
        // looks like a real translation lookup to the i18n key scanner, which
        // then fails on a key the code does not actually ask for.
        let q = '"';
        let unconditional = f.contains(&format!("t(app.lang, {q}parts.hint_size{q}))"));
        assert!(
            !unconditional,
            "the size hint is no longer appended regardless of stage"
        );
    }

    #[test]
    fn a_kept_data_partition_can_be_formatted_and_then_encrypted() {
        let mut c = crate::app::InstallConfig::default();
        c.extra_disks.push(crate::app::ExtraDisk {
            disk: "/dev/vdb1".into(),
            mountpoint: "/mnt/data".into(),
            fs: "ext4".into(),
            format: false, // mounted as it is
            ..Default::default()
        });

        // While it is kept, encryption is refused — nothing silently happens.
        assert!(
            !toggle_data_encryption(&mut c, "/dev/vdb1"),
            "encryption was accepted for a partition that is not formatted"
        );
        assert!(!c.extra_disks[0].encrypt);

        // Turn formatting on (what `f` does), and it becomes available.
        c.extra_disks[0].format = true;
        assert!(toggle_data_encryption(&mut c, "/dev/vdb1"));
        assert!(c.extra_disks[0].encrypt);

        // Turning formatting back off must take the encryption with it, or the
        // plan would carry a promise it cannot keep.
        c.extra_disks[0].format = false;
        c.extra_disks[0].encrypt = false;
        assert!(!toggle_data_encryption(&mut c, "/dev/vdb1"));
    }

    /// The create wizard has two stages and they take different keys. Both used
    /// to be captioned with the SIZE hint, so the role list — a plain up-down
    /// list of SWAP / /home / Data — was labelled "digits — size · ←/→ —
    /// MiB/GiB", describing the step after the one on screen.
    #[test]
    fn the_create_wizard_captions_the_stage_it_is_actually_on() {
        let mut app = App::new();
        app.screen = crate::app::Screen::Disk;
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.parts_create_open = true;

        app.parts_create_stage = 0;
        let role = footer_hint(&app);
        assert_eq!(role, t(app.lang, "parts.hint_role"));

        app.parts_create_stage = 1;
        let size = footer_hint(&app);
        assert_eq!(size, t(app.lang, "parts.hint_size"));
        assert_ne!(role, size, "both stages carry the same caption");
    }

    use super::*;
    use crate::app::{App, BootMode};
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A size can be given in MiB, not just whole GiB.
    ///
    /// Reported use case: a 500 MiB boot partition. The box only ever read whole
    /// GiB, so that size was simply not expressible. Switching the unit must NOT
    /// convert the number — the box holds a size the user is typing, so "500"
    /// then a switch to MiB means 500 MiB, not 512000.
    #[test]
    fn partition_size_can_be_entered_in_mib() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.config.manual_disk = "/dev/sda".into();
        app.parts_create_open = true;
        app.parts_create_stage = 1;
        app.parts_create_role = ROLE_ESP;

        // Type 500 and confirm, in MiB.
        app.parts_size_mib = true;
        app.parts_size_input = "500".into();
        create_modal_key(&mut app, key(KeyCode::Enter));
        assert_eq!(
            app.config.manual_esp_new_mib, 500,
            "500 MiB was not stored as 500 MiB"
        );

        // The same digits in GiB mean something else entirely.
        app.parts_create_open = true;
        app.parts_create_stage = 1;
        app.parts_create_role = ROLE_SWAP;
        app.parts_size_mib = false;
        app.parts_size_input = "2".into();
        create_modal_key(&mut app, key(KeyCode::Enter));
        assert_eq!(
            app.config.manual_swap_new_mib, 2048,
            "2 GiB should be 2048 MiB"
        );

        // ←/→ flips the unit and leaves the typed number alone.
        app.parts_size_input = "500".into();
        app.parts_size_mib = false;
        create_modal_key(&mut app, key(KeyCode::Right));
        assert!(app.parts_size_mib, "→ did not switch the unit");
        assert_eq!(
            app.parts_size_input, "500",
            "switching the unit rewrote the number"
        );
    }

    /// The full edit-and-undo loop the user asked for, driven through the real
    /// key handlers: create a partition, decide the size was wrong, drop it,
    /// create it again at a different size — then delete an EXISTING partition
    /// and put it back.
    ///
    /// None of this touches a disk. Every step is config the plan reads later,
    /// which is the whole point: mistakes are cheap until the final
    /// confirmation, and free afterwards only because there is no afterwards.
    #[test]
    fn the_editor_can_make_changes_and_take_them_all_back() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.config.manual_disk = "/dev/vda".into();
        app.parts_create_disk = "/dev/vda".into();
        app.parts_scanned = true;
        app.parts_list = vec![part("/dev/vda1", "/dev/vda", "vfat")];
        let before = app.config.clone();

        // CREATE a swap at 8 GiB.
        app.parts_create_open = true;
        app.parts_create_stage = 1;
        app.parts_create_role = ROLE_SWAP;
        app.parts_size_mib = false;
        app.parts_size_input = "8".into();
        create_modal_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.config.manual_swap_new_mib, 8192, "swap was not created");
        let planned_path = app.config.manual_swap.clone();
        assert!(!planned_path.is_empty(), "the new partition got no path");

        // Wrong size — DROP it.
        delete_planned(&mut app, &planned_path);
        assert_eq!(
            app.config.manual_swap_new_mib, 0,
            "the planned swap survived"
        );
        assert_eq!(app.config.manual_swap, "", "its path was left behind");

        // CREATE it again, smaller, and in MiB this time.
        app.parts_create_open = true;
        app.parts_create_stage = 1;
        app.parts_create_role = ROLE_SWAP;
        app.parts_size_mib = true;
        app.parts_size_input = "2048".into();
        create_modal_key(&mut app, key(KeyCode::Enter));
        assert_eq!(
            app.config.manual_swap_new_mib, 2048,
            "the retry did not take"
        );

        // DELETE an existing partition, then restore it.
        toggle_deleted(&mut app, "/dev/vda1");
        assert!(is_deleted(&app.config, "/dev/vda1"));
        toggle_deleted(&mut app, "/dev/vda1");
        assert!(!is_deleted(&app.config, "/dev/vda1"));

        // Drop the retry too, and the config is back where it started — proof
        // that editing left no residue anywhere.
        let retry_path = app.config.manual_swap.clone();
        delete_planned(&mut app, &retry_path);
        assert_eq!(
            app.config.manual_swap_new_mib, before.manual_swap_new_mib,
            "undo left a planned size behind"
        );
        assert_eq!(
            app.config.manual_deleted.len(),
            before.manual_deleted.len(),
            "undo left a deletion mark behind"
        );
        assert_eq!(
            app.parts_list.len(),
            1,
            "the editor mutated the scanned table — it must only ever plan"
        );
    }

    /// `u` undoes one edit; Esc leaves the step WITHOUT touching the layout.
    ///
    /// Esc used to do the undoing, and only walked back a page once the history
    /// was empty. That made leaving the step a way to lose work: stepping back
    /// to swap a desktop or add a package first unwound every partition. The
    /// layout must survive a trip to another step and back.
    #[test]
    fn undo_is_its_own_key_and_esc_never_destroys_the_layout() {
        let mut app = App::new();
        app.screen = crate::app::Screen::Disk;
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.parts_scanned = true;
        app.parts_create_disk = "/dev/vda".into();

        create_slot(&mut app, ROLE_ESP, 512);
        app.parts_undo.push(App::new().config); // one step of history
        create_slot(&mut app, ROLE_ROOT, MANUAL_REST);
        app.parts_undo.push(app.config.clone());
        let laid_out = app.config.clone();

        // Esc is NOT the editor's: the global handler takes it and leaves.
        assert!(
            crate::event::handle_global(&mut app, key(KeyCode::Esc)),
            "Esc must walk back a page, not be swallowed by the editor"
        );
        assert_ne!(
            app.screen,
            crate::app::Screen::Disk,
            "Esc did not leave the step"
        );
        assert_eq!(
            app.config.manual_esp_new_mib, laid_out.manual_esp_new_mib,
            "leaving the step destroyed the layout"
        );
        assert_eq!(
            app.config.manual_root_new_mib, laid_out.manual_root_new_mib,
            "leaving the step destroyed the layout"
        );

        // `u` is what takes an edit back.
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.parts_scanned = true;
        app.parts_create_disk = "/dev/vda".into();
        let pristine = app.config.clone();
        handle_key(&mut app, key(KeyCode::Enter)); // firmware row: no-op, no history
        app.cursor = 0;
        handle_key(&mut app, key(KeyCode::Right)); // flip the firmware
        assert_eq!(app.parts_undo.len(), 1, "the edit was not recorded");
        handle_key(&mut app, key(KeyCode::Char('u')));
        assert_eq!(app.config, pristine, "u did not take the edit back");

        // And with nothing left, `u` says so rather than doing nothing.
        handle_key(&mut app, key(KeyCode::Char('u')));
        assert!(
            !app.pmode_status.is_empty(),
            "u with an empty history must explain itself, not stay silent"
        );
    }

    /// A NEW partition can be created as plain data storage, end to end.
    ///
    /// This was the standing gap the user kept asking about: the create wizard
    /// only offered the five install roles, so carving part of a second drive
    /// just to hold files was impossible — only whole blank disks (storage
    /// step) or existing partitions (role modal) could be data. Driven through
    /// the real key handlers, because that is the layer where the last such
    /// gap hid.
    #[test]
    fn a_new_partition_can_be_created_as_data_storage() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.parts_scanned = true;
        app.parts_list = vec![part("/dev/vdb1", "/dev/vdb", "ext4")];

        // Open the create wizard on the second disk.
        let row = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::Create { disk } if disk == "/dev/vdb"))
            .expect("no create row for /dev/vdb");
        app.cursor = row;
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(app.parts_create_open);

        // The role list must offer "data"; walk the cursor onto it.
        let avail = available_roles(&app);
        let data_at = avail
            .iter()
            .position(|r| *r == ROLE_DATA)
            .expect("the create wizard does not offer the data role");
        for _ in 0..data_at {
            handle_key(&mut app, key(KeyCode::Down));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(
            app.parts_create_stage, 1,
            "no size stage after picking data"
        );

        // 20 GiB, then the mountpoint picker appears.
        for ch in ['2', '0'] {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            app.parts_mount_open,
            "picking a size for a data partition must ask where to mount it"
        );

        // Take the /mnt/data template.
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(!app.parts_mount_open);

        // The creation is planned…
        let dp = app
            .config
            .manual_data_new
            .first()
            .expect("no data partition was planned");
        assert_eq!(dp.disk, "/dev/vdb");
        assert_eq!(dp.mib, 20 * 1024);
        assert_eq!(
            dp.path, "/dev/vdb2",
            "numbered after the existing partition"
        );
        // …and its mount entry rides in extra_disks, formatted (it is new).
        let e = app
            .config
            .extra_disks
            .iter()
            .find(|e| e.disk == "/dev/vdb2")
            .expect("the data partition has no mount entry");
        assert_eq!(e.mountpoint, "/mnt/data");
        assert!(e.format);
        assert_eq!(e.fs, "ext4");

        // It shows up in the editor as a new row under its disk.
        assert!(
            build_rows(&app)
                .iter()
                .any(|r| matches!(r, Row::Part { path, is_new: true } if path == "/dev/vdb2")),
            "the planned data partition is not listed"
        );
        assert_eq!(role_of(&app.config, "/dev/vdb2"), Some(ROLE_DATA));

        // Deleting it takes BOTH records away.
        let row = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::Part { path, .. } if path == "/dev/vdb2"))
            .unwrap();
        app.cursor = row;
        handle_key(&mut app, key(KeyCode::Char('d')));
        assert!(
            app.config.manual_data_new.is_empty(),
            "the creation survived d"
        );
        assert!(
            !app.config.extra_disks.iter().any(|e| e.disk == "/dev/vdb2"),
            "the mount entry outlived its partition"
        );
    }

    /// Walk the create wizard by KEYS to a new data partition on /dev/vdb.
    /// Driving it by keys rather than by setting the config is the point: the
    /// only bug this editor ever shipped to a real console got through because
    /// every test set fields directly and none of them pressed anything.
    fn app_with_a_new_data_partition() -> App {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.parts_scanned = true;
        app.parts_list = vec![part("/dev/vdb1", "/dev/vdb", "ext4")];

        let row = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::Create { disk } if disk == "/dev/vdb"))
            .expect("no create row for /dev/vdb");
        app.cursor = row;
        handle_key(&mut app, key(KeyCode::Enter));
        let data_at = available_roles(&app)
            .iter()
            .position(|r| *r == ROLE_DATA)
            .expect("the create wizard does not offer the data role");
        for _ in 0..data_at {
            handle_key(&mut app, key(KeyCode::Down));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        for ch in ['2', '0'] {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        handle_key(&mut app, key(KeyCode::Enter)); // take the /mnt/data template
        app.cursor = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::Part { path, .. } if path == "/dev/vdb2"))
            .expect("the planned data partition is not listed");
        app
    }

    /// A data partition picks from EVERY filesystem, exactly like root and /home.
    ///
    /// It used to be born ext4 with no way to change it — the create wizard hard
    /// coded the filesystem and the row printed a fixed "will be formatted
    /// (ext4)". A partition being made from scratch has nothing to preserve, so
    /// there was never a reason to withhold the choice.
    #[test]
    fn a_new_data_partition_can_pick_any_filesystem() {
        let mut app = app_with_a_new_data_partition();
        let dev = "/dev/vdb2";
        let fs_now = |a: &App| data_fs_eff(&a.config, dev).to_string();
        assert_eq!(
            fs_now(&app),
            "ext4",
            "a fresh data partition starts at ext4"
        );

        // →/← walk the SAME list the other roles use, in both directions.
        for expect in FS_CHOICES.iter().skip(1).chain(FS_CHOICES.iter().take(1)) {
            handle_key(&mut app, key(KeyCode::Right));
            assert_eq!(&fs_now(&app), expect, "→ did not reach {expect}");
        }
        handle_key(&mut app, key(KeyCode::Left));
        assert_eq!(
            fs_now(&app),
            *FS_CHOICES.last().unwrap(),
            "← does not walk the list the other way"
        );

        // On btrfs the options modal opens and writes to THIS partition's entry.
        while fs_now(&app) != "btrfs" {
            handle_key(&mut app, key(KeyCode::Right));
        }
        handle_key(&mut app, key(KeyCode::Char('o')));
        assert!(
            app.fs_opts_modal_open,
            "btrfs offers no options on a data row"
        );
        assert_eq!(
            app.fs_opts_data_dev, dev,
            "the modal is bound to no partition"
        );
        let fs = fs_for_target(&app, app.fs_opts_target);
        crate::screens::disk::fs_opts_modal_key(&mut app, key(KeyCode::Char(' ')), &fs);
        let e = |a: &App| {
            a.config
                .extra_disks
                .iter()
                .find(|e| e.disk == dev)
                .expect("the data partition lost its mount entry")
                .clone()
        };
        assert!(
            e(&app).compress,
            "the btrfs option did not reach the partition"
        );
        assert!(
            !app.config.btrfs_compress,
            "it wrote to the ROOT filesystem's options instead"
        );

        // Re-picking the FOLDER must not quietly undo the filesystem.
        set_data_mount(&mut app, dev, "/srv");
        let e = e(&app);
        assert_eq!(e.mountpoint, "/srv");
        assert_eq!(
            e.fs, "btrfs",
            "changing the mountpoint reset the filesystem"
        );
        assert!(
            e.compress,
            "changing the mountpoint reset the btrfs options"
        );
    }

    /// The choice has to be VISIBLE on the row, and it has to reach the plan.
    #[test]
    fn the_data_row_shows_its_filesystem_strip_and_the_plan_honours_it() {
        let mut app = app_with_a_new_data_partition();
        while data_fs_eff(&app.config, "/dev/vdb2") != "xfs" {
            handle_key(&mut app, key(KeyCode::Right));
        }
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 30)).unwrap();
        term.draw(|f| {
            let a = f.area();
            draw(f, &mut app, a);
        })
        .unwrap();
        let out = term.backend().to_string();
        // The focused row reveals the alternatives, with the pick bracketed —
        // the same idiom as the root row, not a bare word.
        assert!(
            out.contains("\u{2039}xfs\u{203a}"),
            "the data row does not reveal its filesystem choice:\n{out}"
        );
        assert!(
            out.contains("btrfs"),
            "the data row offers no filesystem besides its current one:\n{out}"
        );

        // And the pick is what actually gets made.
        let plan = crate::system::install::build_plan(&app);
        let text = plan
            .iter()
            .map(|a| format!("{} {}", a.program, a.args.join(" ")))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("mkfs.xfs -f /dev/vdb2"),
            "the chosen filesystem never reaches the plan:\n{text}"
        );
    }

    /// Renumbering keeps a data partition's mount entry glued to it.
    ///
    /// The entry is keyed by the predicted device path. When another planned
    /// partition lands BEFORE the data one (fixed sizes come first), the data
    /// partition's number shifts — and the entry has to shift with it, or the
    /// plan would format a path that now belongs to a different partition.
    #[test]
    fn a_renumbered_data_partition_keeps_its_mount_entry() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.parts_scanned = true;
        app.parts_list = vec![];

        // A rest-of-disk data partition on a blank disk → /dev/vdb1.
        app.parts_create_disk = "/dev/vdb".into();
        app.parts_pending_mib = MANUAL_REST;
        create_data_slot(&mut app, "/mnt/data");
        assert_eq!(app.config.manual_data_new[0].path, "/dev/vdb1");

        // A fixed-size /home planned on the same disk sorts BEFORE the rest
        // slot, pushing the data partition to number 2.
        app.parts_create_disk = "/dev/vdb".into();
        create_slot(&mut app, ROLE_HOME, 8 * 1024);
        assert_eq!(app.config.manual_home, "/dev/vdb1");
        assert_eq!(
            app.config.manual_data_new[0].path, "/dev/vdb2",
            "the data partition did not renumber"
        );
        let e = app
            .config
            .extra_disks
            .iter()
            .find(|e| e.disk == "/dev/vdb2")
            .expect("the mount entry did not follow the renumbering");
        assert_eq!(e.mountpoint, "/mnt/data");
        assert!(
            !app.config.extra_disks.iter().any(|e| e.disk == "/dev/vdb1"),
            "a stale mount entry still points at the old path"
        );
    }

    /// A partition can be used as plain storage, mounted where the user says.
    ///
    /// Previously the manual editor only understood the install roles, so a
    /// second drive meant for files had to be handled on a different step. The
    /// rule that matters: a partition that ALREADY has a filesystem is mounted
    /// as-is. Reformatting somebody's media drive because they gave it a folder
    /// would be the worst kind of surprise.
    #[test]
    fn a_partition_can_be_mounted_as_plain_storage() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.parts_scanned = true;
        app.parts_list = vec![
            part("/dev/vdb1", "/dev/vdb", "ext4"), // has data
            part("/dev/vdb2", "/dev/vdb", ""),     // blank
        ];

        // A template path.
        app.parts_target = "/dev/vdb1".into();
        app.parts_mount_open = true;
        app.parts_mount_cursor = 0; // /mnt/data
        mount_modal_key(&mut app, key(KeyCode::Enter));
        assert_eq!(role_of(&app.config, "/dev/vdb1"), Some(ROLE_DATA));
        let e = app
            .config
            .extra_disks
            .iter()
            .find(|e| e.disk == "/dev/vdb1")
            .expect("no mount was recorded");
        assert_eq!(e.mountpoint, "/mnt/data");
        assert!(
            !e.format,
            "a partition with a filesystem must be mounted, not reformatted"
        );
        assert_eq!(e.fs, "ext4", "its existing filesystem was forgotten");

        // A blank partition has nothing to preserve, so it is formatted.
        app.parts_target = "/dev/vdb2".into();
        app.parts_mount_open = true;
        app.parts_mount_cursor = MOUNT_TEMPLATES.len(); // custom
        app.parts_mount_input = "/srv/backup".into();
        mount_modal_key(&mut app, key(KeyCode::Enter));
        let e = app
            .config
            .extra_disks
            .iter()
            .find(|e| e.disk == "/dev/vdb2")
            .expect("no mount for the blank partition");
        assert_eq!(e.mountpoint, "/srv/backup");
        assert!(e.format, "a partition with no filesystem must be formatted");

        // A path that isn't one is refused, with a reason.
        app.parts_target = "/dev/vdb2".into();
        app.parts_mount_open = true;
        app.parts_mount_cursor = MOUNT_TEMPLATES.len();
        app.parts_mount_input = "backup".into();
        mount_modal_key(&mut app, key(KeyCode::Enter));
        assert!(
            app.parts_mount_open,
            "a bad path should keep the picker open"
        );
        assert!(!app.pmode_status.is_empty(), "and say why");

        // Giving the partition a real role instead drops the data mount.
        assign_role(&mut app, "/dev/vdb1", Some(ROLE_HOME));
        assert!(
            !app.config.extra_disks.iter().any(|e| e.disk == "/dev/vdb1"),
            "the old data mount outlived the role change"
        );
    }

    /// Every partition shows a real size, whatever the scenario.
    ///
    /// A rest-of-disk slot used to render as the words "rest of the disk",
    /// which answers the wrong question: the user wants to know their 50 GiB
    /// drive was actually used, not that the installer will work it out later.
    #[test]
    fn every_partition_row_shows_a_concrete_size() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.parts_scanned = true;
        app.parts_list = vec![];

        // A fixed slot and a rest-of-disk slot, on different drives.
        app.parts_create_disk = "/dev/vda".into();
        create_slot(&mut app, ROLE_ESP, 512);
        create_slot(&mut app, ROLE_ROOT, MANUAL_REST);
        app.parts_create_disk = "/dev/vdb".into();
        create_slot(&mut app, ROLE_HOME, MANUAL_REST);

        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 30)).unwrap();
        term.draw(|f| {
            let a = f.area();
            draw(f, &mut app, a);
        })
        .unwrap();
        let out = term.backend().to_string();

        // The words must be gone — every row carries a figure instead.
        assert!(
            !out.contains(&t(app.lang, "parts.size_rest")),
            "a partition still reports its size as \"rest of the disk\":\n{out}"
        );
        // Either spelling of the unit: the panel decides which it can afford.
        let shows = |n: &str, u: &str| out.contains(&format!("{n} {u}iB")) || out.contains(n);
        // The fixed one keeps its own size…
        assert!(shows("512", "M"), "the fixed-size partition lost its size");
        // …and each rest-of-disk one shows what it will really get: the whole
        // of the 500G second disk, and the 240G first disk less the ESP.
        assert!(
            shows("500", "G"),
            "the rest-of-disk /home does not show the space it takes:\n{out}"
        );
        assert!(
            shows("239", "G"),
            "the rest-of-disk root does not account for the ESP beside it:\n{out}"
        );
    }

    /// A "rest of the disk" partition must be seen to claim the disk.
    ///
    /// Free space ignored rest-of-disk slots, so after planning /home across a
    /// whole spare drive the row underneath still advertised the full 50G as
    /// free. Reported as "I left the size empty and it just made an empty
    /// partition that took nothing" — the partition was fine; the disk was
    /// lying about what was left.
    #[test]
    fn a_rest_of_disk_partition_leaves_no_free_space() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.parts_scanned = true;
        app.parts_list = vec![];

        let before = free_bytes(&app, "/dev/vdb");
        assert!(before > 0, "an empty disk should start with free space");

        // Plan /home across the whole of the second disk.
        app.parts_create_disk = "/dev/vdb".into();
        create_slot(&mut app, ROLE_HOME, MANUAL_REST);

        assert_eq!(
            free_bytes(&app, "/dev/vdb"),
            0,
            "the disk still reports free space after being claimed end to end"
        );
        // The other disk is untouched by that claim.
        assert!(
            free_bytes(&app, "/dev/vda") > 0,
            "claiming one disk must not consume another"
        );

        // A FIXED-size partition still leaves the remainder available.
        let mut app2 = App::new();
        app2.config.partition_mode = crate::app::PartitionMode::Manual;
        app2.parts_scanned = true;
        app2.parts_create_disk = "/dev/vdb".into();
        create_slot(&mut app2, ROLE_HOME, 1024);
        let left = free_bytes(&app2, "/dev/vdb");
        assert!(
            left > 0 && left < before,
            "a 1 GiB slot should leave the rest free"
        );
    }

    /// Creating on a SECOND disk must stay on that disk.
    ///
    /// Driven through the real key handlers, because that is where it broke:
    /// the unit tests all set the config directly and so never noticed that the
    /// wizard was pinning every new slot to one device. Reported from QEMU as
    /// "creating /home on the other disk switches back to the first one".
    #[test]
    fn creating_on_a_second_disk_lands_on_that_disk() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.parts_scanned = true;
        app.parts_list = vec![];

        // Walk to the "+ Create" row of the SECOND disk and create /home there.
        let create_rows: Vec<usize> = build_rows(&app)
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match r {
                Row::Create { disk } if disk == "/dev/vdb" => Some(i),
                _ => None,
            })
            .collect();
        let row = *create_rows
            .first()
            .expect("no create row for the second disk");
        app.cursor = row;
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(app.parts_create_open, "the create wizard did not open");

        // Pick /home, then a size.
        app.parts_create_role = ROLE_HOME;
        app.parts_create_stage = 1;
        app.parts_size_mib = false;
        app.parts_size_input = "20".into();
        handle_key(&mut app, key(KeyCode::Enter));

        assert_eq!(
            app.config.manual_home_disk, "/dev/vdb",
            "the new /home was pinned to another disk"
        );
        assert!(
            app.config.manual_home.starts_with("/dev/vdb"),
            "its predicted path is on the wrong disk: {}",
            app.config.manual_home
        );
        // And it is listed under the second disk, not the first.
        let under_vdb = build_rows(&app)
            .iter()
            .any(|r| matches!(r, Row::Part { path, is_new: true } if path.starts_with("/dev/vdb")));
        assert!(
            under_vdb,
            "the planned partition is not shown on the second disk"
        );
    }

    /// A removed partition vanishes outright, and `u` is how it comes back.
    ///
    /// It first stayed in the table tagged "will be deleted", which read as
    /// though it still held its space; then it moved to an undo row below the
    /// layout. Neither was what was wanted: the partition should simply be
    /// gone, with Esc walking the edits back one at a time — and only leaving
    /// the step once there is nothing left to undo.
    #[test]
    fn a_removed_partition_vanishes_and_undo_brings_it_back() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.parts_scanned = true;
        app.config.manual_disk = "/dev/vda".into();
        app.parts_list = vec![
            part("/dev/vda1", "/dev/vda", "vfat"),
            part("/dev/vda2", "/dev/vda", "ext4"),
        ];

        let listed = |app: &App| -> Vec<String> {
            build_rows(app)
                .into_iter()
                .filter_map(|r| match r {
                    Row::Part { path, .. } => Some(path),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(listed(&app).len(), 2);
        let free_before = free_bytes(&app, "/dev/vda");

        // Put the cursor on /dev/vda2 and delete it.
        let idx = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::Part { path, .. } if path == "/dev/vda2"))
            .expect("vda2 should be listed");
        app.cursor = idx;
        handle_key(&mut app, key(KeyCode::Char('d')));

        assert_eq!(
            listed(&app),
            vec!["/dev/vda1".to_string()],
            "the partition should be gone from the layout entirely"
        );
        assert!(
            !build_rows(&app)
                .iter()
                .any(|r| matches!(r, Row::Part { path, .. } if path == "/dev/vda2")),
            "no row for the removed partition may remain"
        );
        assert!(
            free_bytes(&app, "/dev/vda") > free_before,
            "its space must be free to plan into"
        );

        // `u` undoes exactly that one change.
        handle_key(&mut app, key(KeyCode::Char('u')));
        assert_eq!(listed(&app).len(), 2, "u did not restore the partition");
        assert!(
            app.parts_undo.is_empty(),
            "the undo history should be spent"
        );
    }

    /// Deleting is a MARK, and a mark can be taken back.
    ///
    /// The user's requirement, verbatim: someone may reach the summary, change
    /// their mind about a partition they deleted on another disk, and walk back
    /// to restore it. That only holds if the editor never touches the disk —
    /// which it doesn't: `d` toggles a flag, and the erasing lives in the plan,
    /// behind the final confirmation. This pins the round trip, including the
    /// role being released and the space coming back.
    #[test]
    fn deleting_a_partition_is_a_reversible_mark() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.parts_list = vec![
            part("/dev/sdb1", "/dev/sdb", "ext4"),
            part("/dev/sdb2", "/dev/sdb", "ext4"),
        ];
        // The partition starts out carrying a role.
        app.config.manual_home = "/dev/sdb1".into();
        app.config.manual_home_fs = "ext4".into();

        toggle_deleted(&mut app, "/dev/sdb1");
        assert!(is_deleted(&app.config, "/dev/sdb1"), "not marked");
        assert_eq!(
            app.config.manual_home, "",
            "a partition being deleted cannot still be /home"
        );
        assert_eq!(
            app.config.manual_deleted[0].disk, "/dev/sdb",
            "the disk must come from the scan, not from parsing the path"
        );

        // Un-delete: the mark goes away and nothing was destroyed on the way.
        toggle_deleted(&mut app, "/dev/sdb1");
        assert!(!is_deleted(&app.config, "/dev/sdb1"), "still marked");
        assert!(
            app.config.manual_deleted.is_empty(),
            "the delete list kept a ghost entry"
        );
        assert_eq!(
            app.parts_list.len(),
            2,
            "the editor removed a partition from the scan — it must only mark"
        );
    }

    /// The `[o]` cue on the /home row must configure /home, not root.
    ///
    /// It used to open the ROOT's option set from either row, so a user setting
    /// up a second disk as /home saw the root's ticks and, by touching them,
    /// silently rewrote the root: unticking "Subvolumes" there dropped the root's
    /// @/@snapshots layout (snapshots are kept in sync with it) while nothing on
    /// screen said so.
    #[test]
    fn the_home_row_configures_home_not_root() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.config.manual_root = "/dev/sda2".into();
        app.config.manual_root_fs = "btrfs".into();
        app.config.manual_home = "/dev/sdb1".into();
        app.config.manual_home_fs = "btrfs".into();
        // The root is fully configured, as if set up on an earlier visit.
        app.config.btrfs_subvolumes = true;
        app.config.btrfs_snapshots = true;
        app.config.btrfs_compress = true;

        let home_row = Row::Part {
            path: "/dev/sdb1".into(),
            is_new: false,
        };
        let root_row = Row::Part {
            path: "/dev/sda2".into(),
            is_new: false,
        };
        assert_eq!(
            fs_opts_target_for(&app, &home_row),
            FsOptsTarget::ManualHome
        );
        assert_eq!(fs_opts_target_for(&app, &root_row), FsOptsTarget::Root);

        // /home is offered ONLY the options that mean anything for it: the
        // @home subvolume is skipped for an external /home and snapper hangs off
        // the root, so subvolumes/snapshots there would be dead switches.
        let home_opts = crate::screens::disk::fs_features_for(FsOptsTarget::ManualHome, "btrfs");
        let ids: Vec<&str> = home_opts.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec!["compress", "discard"], "wrong options for /home");

        // The root keeps the full set.
        let root_opts = crate::screens::disk::fs_features_for(FsOptsTarget::Root, "btrfs");
        assert_eq!(root_opts.len(), 4, "the root lost options");

        // Toggling from the /home row must leave every root field alone.
        app.fs_opts_target = FsOptsTarget::ManualHome;
        for (id, _) in home_opts {
            crate::screens::disk::fs_opts_modal_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
                "btrfs",
            );
            let _ = id;
            app.fs_opt_cursor += 1;
        }
        assert!(
            app.config.btrfs_subvolumes && app.config.btrfs_snapshots && app.config.btrfs_compress,
            "configuring /home changed the root's filesystem options"
        );
        assert!(
            app.config.manual_home_compress,
            "/home's own compression flag was not set"
        );
    }

    /// The filesystem reveal strip must never be sliced at the panel edge.
    ///
    /// All seven filesystems with separators run ~47 columns, which does not fit
    /// beside a partition row on anything but a very wide terminal. The strip
    /// therefore sheds its outermost entries to fit — but it must ALWAYS keep
    /// the selected one (that is the value the row is reporting) and must never
    /// hand back something wider than the space it was given, because ratatui
    /// would then cut it mid-word.
    #[test]
    fn the_filesystem_strip_shrinks_to_fit_instead_of_being_cut() {
        for fs in FS_CHOICES {
            for budget in 0..60usize {
                let strip = option_strip_fit(&FS_CHOICES, fs, budget);
                let text: String = strip.iter().map(|s| s.content.as_ref()).collect();

                // The selected value is always present, in its ‹ › brackets.
                assert!(
                    text.contains(&format!("\u{2039}{fs}\u{203a}")),
                    "budget {budget} dropped the selected {fs}: {text:?}"
                );
                // Never wider than the budget — unless even the lone selected
                // value doesn't fit, which no smaller strip could fix.
                let lone = spans_width(&option_strip(&[fs], fs));
                assert!(
                    spans_width(&strip) <= budget.max(lone),
                    "budget {budget} overrun by {fs}: {text:?}"
                );
                // Never ends mid-separator.
                assert!(
                    !text.ends_with('\u{00b7}') && !text.ends_with(' '),
                    "budget {budget} left a dangling separator: {text:?}"
                );
            }
        }
        // Given room, the strip is the WHOLE list — the reveal design's point.
        let full = option_strip_fit(&FS_CHOICES, "ext4", 200);
        let text: String = full.iter().map(|s| s.content.as_ref()).collect();
        for fs in FS_CHOICES {
            assert!(text.contains(fs), "a roomy strip must show {fs}: {text:?}");
        }
    }

    const GIB: u64 = 1024 * 1024 * 1024;

    fn part(path: &str, parent: &str, fstype: &str) -> Partition {
        Partition {
            path: path.into(),
            parent: parent.into(),
            size: "10G".into(),
            size_bytes: 10 * GIB,
            fstype: fstype.into(),
            label: String::new(),
            partuuid: format!("uuid-of-{}", path.trim_start_matches("/dev/")),
            parttype: String::new(),
        }
    }

    /// Like `part`, but carrying a GPT type GUID — for the foreign-OS checks,
    /// which recognise an ESP by its type rather than its size or label.
    fn part_typed(path: &str, parent: &str, fstype: &str, parttype: &str) -> Partition {
        Partition {
            parttype: parttype.into(),
            ..part(path, parent, fstype)
        }
    }

    /// A planned partition REUSES the number of one being deleted, so its path
    /// is still in the SCAN, carrying the old partition's size. The row must
    /// print the size the user asked for — never whatever used to live there.
    ///
    /// Reported from QEMU: two freshly planned 10 GiB slots on /dev/vdb rendered
    /// as "20G" and "30G" (the sizes of the partitions they replaced), while the
    /// free-space note and the install confirmation both used the correct
    /// figures. `part_line` was deciding "planned or existing?" by looking the
    /// path up in the scan, which is exactly the question number-reuse makes
    /// unanswerable that way. The row already carries `is_new`.
    #[test]
    fn a_planned_partition_shows_its_own_size_not_the_one_it_replaces() {
        let sized = |path: &str, gib: u64| Partition {
            size: format!("{gib}G"),
            size_bytes: gib * GIB,
            ..part(path, "/dev/vdb", "btrfs")
        };
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.parts_scanned = true;
        app.parts_list = vec![sized("/dev/vdb1", 20), sized("/dev/vdb2", 30)];

        // Delete both, which frees their numbers for the plan to reuse.
        for dev in ["/dev/vdb1", "/dev/vdb2"] {
            app.cursor = build_rows(&app)
                .iter()
                .position(|r| matches!(r, Row::Part { path, .. } if path == dev))
                .expect("partition not listed");
            handle_key(&mut app, key(KeyCode::Char('d')));
        }

        // Plan 10 GiB of /home into the freed space, on the reused number.
        app.config.manual_home = "/dev/vdb1".into();
        app.config.manual_home_new_mib = 10 * 1024;
        app.config.manual_home_disk = "/dev/vdb".into();

        let row = |app: &App, path: &str, is_new: bool| -> String {
            part_line(
                app,
                path,
                is_new,
                RowStyle {
                    mark: "  ",
                    st: theme::normal(),
                    focused: false,
                    compact: false,
                    width: 100,
                },
            )
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect()
        };

        // Either spelling of the unit — the wide "10 GiB" on a roomy panel, the
        // cramped "10G" on a narrow one. What matters is WHOSE number it is.
        let shows = |row: &str, n: &str| row.contains(&format!("{n} GiB")) || row.contains(n);

        let planned = row(&app, "/dev/vdb1", true);
        assert!(
            shows(&planned, "10"),
            "the planned row does not show its own 10 GiB: {planned}"
        );
        assert!(
            !shows(&planned, "20"),
            "the planned row shows the size of the partition it replaces: {planned}"
        );

        // And the opposite direction still works: an EXISTING partition is
        // reported from the scan, at its real size.
        app.parts_list = vec![sized("/dev/vdb1", 20)];
        let existing = row(&app, "/dev/vdb1", false);
        assert!(
            shows(&existing, "20"),
            "an existing partition must show its scanned size: {existing}"
        );
    }

    fn manual_cfg() -> InstallConfig {
        let mut c = App::new().config;
        c.boot_mode = BootMode::Uefi;
        c.manual_esp = "/dev/sda1".into();
        c.manual_root = "/dev/sda5".into();
        c.manual_root_fs = "btrfs".into();
        c
    }

    fn fixture() -> Vec<Partition> {
        vec![
            part("/dev/sda1", "/dev/sda", "vfat"),
            part("/dev/sda5", "/dev/sda", ""),
            part("/dev/sda6", "/dev/sda", "swap"),
            part("/dev/sdb1", "/dev/sdb", "ext4"),
        ]
    }

    /// A disk marked for a full wipe numbers its new partitions from 1 even when
    /// the scan still shows old ones. This is the reused-disk fix: without the
    /// wipe, three leftover partitions push the first "new" one to #4 — exactly
    /// the surprise the user hit.
    #[test]
    fn a_wiped_disk_numbers_new_partitions_from_one() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.config.manual_solo = true; // wipe is a solo-only feature
        app.parts_scanned = true;
        // vdb already carries three partitions from a previous install…
        app.parts_list = vec![
            part("/dev/vdb1", "/dev/vdb", "ext4"),
            part("/dev/vdb2", "/dev/vdb", "ext4"),
            part("/dev/vdb3", "/dev/vdb", "swap"),
        ];
        // …and a fresh /home is planned on it.
        app.config.manual_home = "x".into();
        app.config.manual_home_new_mib = MANUAL_REST;
        app.config.manual_home_disk = "/dev/vdb".into();

        // Without a wipe: the leftovers push the new /home to #4.
        renumber_planned(&mut app);
        assert_eq!(
            app.config.manual_home, "/dev/vdb4",
            "the reused-disk behaviour changed unexpectedly"
        );

        // With vdb marked for wipe: it numbers from 1.
        app.config.manual_wipe = vec![crate::app::WipeMark {
            disk: "/dev/vdb".into(),
            method: crate::app::WipeMethod::Quick,
            rota: true,
            tran: "sata".into(),
        }];
        renumber_planned(&mut app);
        assert_eq!(
            app.config.manual_home, "/dev/vdb1",
            "a wiped disk must number its new partitions from 1"
        );
    }

    /// The strip never offers the wrong erase for the medium: an SSD is not
    /// offered the zero-fill (which wears it for a worse erase) and an HDD is not
    /// offered the SSD crypto/TRIM (a no-op on a disk that cannot discard).
    #[test]
    fn the_wipe_strip_matches_the_medium() {
        use crate::app::WipeMethod;
        let ssd = wipe_methods_for(false);
        assert!(
            !ssd.contains(&WipeMethod::ZeroHdd),
            "an SSD must not offer zero-fill"
        );
        assert!(
            ssd.contains(&WipeMethod::CryptoSsd),
            "an SSD must offer the crypto/TRIM erase"
        );
        let hdd = wipe_methods_for(true);
        assert!(
            !hdd.contains(&WipeMethod::CryptoSsd),
            "an HDD must not offer the SSD crypto/TRIM erase"
        );
        assert!(
            hdd.contains(&WipeMethod::ZeroHdd),
            "an HDD must offer zero-fill"
        );
        for m in [WipeMethod::Quick, WipeMethod::AtaSecure] {
            assert!(
                ssd.contains(&m) && hdd.contains(&m),
                "{m:?} should suit both media"
            );
        }
    }

    /// Filesystem options survive a trip through another filesystem. Cycling
    /// ←/→ past ext4 and back to btrfs must return the user's choices — the data
    /// row used to clear compression on the way past, so the setting was gone by
    /// the time they came back.
    #[test]
    fn btrfs_options_survive_cycling_the_filesystem_away_and_back() {
        // ── a DATA partition: the one that actually lost them ──
        let mut app = app_with_a_new_data_partition();
        let dev = app.config.manual_data_new[0].path.clone();
        for e in app.config.extra_disks.iter_mut().filter(|e| e.disk == dev) {
            e.fs = "btrfs".into();
            e.compress = true;
        }
        app.cursor = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::Part { path, .. } if *path == dev))
            .expect("the data partition is not listed");
        // Away from btrfs…
        handle_key(&mut app, key(KeyCode::Right));
        assert_ne!(
            data_fs_eff(&app.config, &dev),
            "btrfs",
            "the filesystem did not change"
        );
        // …and all the way back round.
        for _ in 0..12 {
            if data_fs_eff(&app.config, &dev) == "btrfs" {
                break;
            }
            handle_key(&mut app, key(KeyCode::Right));
        }
        assert_eq!(
            data_fs_eff(&app.config, &dev),
            "btrfs",
            "never got back to btrfs"
        );
        let e = app
            .config
            .extra_disks
            .iter()
            .find(|e| e.disk == dev)
            .expect("the mount entry vanished");
        assert!(
            e.compress,
            "compression was lost by cycling the filesystem away and back"
        );

        // ── the ROOT row: was already fine, and must stay that way ──
        let mut r = App::new();
        r.config.partition_mode = crate::app::PartitionMode::Manual;
        r.config.manual_solo = true;
        r.parts_scanned = true;
        r.parts_list = vec![part("/dev/vda2", "/dev/vda", "ext4")];
        r.config.manual_root = "/dev/vda2".into();
        r.config.manual_root_fs = "btrfs".into();
        r.config.btrfs_subvolumes = true;
        r.config.btrfs_compress = true;
        r.config.btrfs_snapshots = true;
        r.cursor = build_rows(&r)
            .iter()
            .position(|x| matches!(x, Row::Part { path, .. } if path == "/dev/vda2"))
            .unwrap();
        handle_key(&mut r, key(KeyCode::Right));
        for _ in 0..12 {
            if r.config.manual_root_fs == "btrfs" {
                break;
            }
            handle_key(&mut r, key(KeyCode::Right));
        }
        assert!(
            r.config.btrfs_subvolumes && r.config.btrfs_compress && r.config.btrfs_snapshots,
            "the root's btrfs options were lost by cycling the filesystem"
        );

        // ── a manual /home: its own flags, also kept ──
        let mut h = App::new();
        h.config.partition_mode = crate::app::PartitionMode::Manual;
        h.config.manual_solo = true;
        h.parts_scanned = true;
        h.parts_list = vec![part("/dev/vda3", "/dev/vda", "ext4")];
        h.config.manual_home = "/dev/vda3".into();
        h.config.manual_home_fs = "btrfs".into();
        h.config.manual_home_compress = true;
        h.config.manual_home_discard = true;
        h.cursor = build_rows(&h)
            .iter()
            .position(|x| matches!(x, Row::Part { path, .. } if path == "/dev/vda3"))
            .unwrap();
        handle_key(&mut h, key(KeyCode::Right));
        for _ in 0..12 {
            if h.config.manual_home_fs == "btrfs" {
                break;
            }
            handle_key(&mut h, key(KeyCode::Right));
        }
        assert!(
            h.config.manual_home_compress && h.config.manual_home_discard,
            "the /home btrfs options were lost by cycling the filesystem"
        );
    }

    /// `u` steps back through LAYOUT changes only. Cycling a filesystem is one
    /// keypress to set and one to unset, so it is not a step — recording it made
    /// `u` walk back through invisible ones and silently revert the user's btrfs
    /// options along the way.
    #[test]
    fn undo_ignores_settings_and_steps_back_through_layout_changes() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.config.manual_solo = true;
        app.parts_scanned = true;
        app.parts_list = vec![
            part("/dev/vda1", "/dev/vda", "vfat"),
            part("/dev/vda2", "/dev/vda", "ext4"),
        ];
        app.config.manual_root = "/dev/vda2".into();
        app.config.manual_root_fs = "btrfs".into();
        app.config.btrfs_subvolumes = true;
        app.config.btrfs_compress = true;

        // Cycle the filesystem a few times: settings, not steps.
        app.cursor = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::Part { path, .. } if path == "/dev/vda2"))
            .unwrap();
        for _ in 0..3 {
            handle_key(&mut app, key(KeyCode::Right));
        }
        assert!(
            app.parts_undo.is_empty(),
            "cycling a filesystem recorded {} undo step(s)",
            app.parts_undo.len()
        );

        // A real layout change IS a step: plan a /home.
        app.config.manual_home_disk = "/dev/vda".into();
        app.parts_create_disk = "/dev/vda".into();
        create_slot(&mut app, ROLE_HOME, 10240);
        // (create_slot is called directly here, so push the step the key path
        // would have pushed; what matters below is that undo restores layout
        // while leaving settings alone.)
        let with_home = app.config.clone();
        app.parts_undo.push({
            let mut c = with_home.clone();
            c.manual_home.clear();
            c.manual_home_new_mib = 0;
            c
        });

        // Undo removes the partition but keeps the filesystem options.
        handle_key(&mut app, key(KeyCode::Char('u')));
        assert!(
            app.config.manual_home.is_empty(),
            "undo did not step back over the layout change"
        );
        assert!(
            app.config.btrfs_subvolumes && app.config.btrfs_compress,
            "undo swallowed the filesystem options the user had set"
        );
    }

    /// Reported from QEMU: a 30 GiB NVMe held a 512 MiB ESP, a 15 GiB root and
    /// a 4 GiB swap, leaving 10.5 GiB free. Growing the ROOT to 20 GiB was
    /// refused with "запрошено 20 GiB, а вільно лише 10.5 GiB" — the check
    /// counted the root's own 15 GiB as somebody else's, so the one partition
    /// being edited was the one it would not let you edit.
    #[test]
    fn resizing_a_slot_may_spend_the_space_that_slot_already_holds() {
        let free = 10 * 1024 + 512; // 10.5 GiB left on the disk
        let root = 15 * 1024; // what the root holds right now

        assert_eq!(
            size_budget_mib(free, root),
            25 * 1024 + 512,
            "a resize must be offered its own space back"
        );
        assert!(
            20 * 1024 <= size_budget_mib(free, root),
            "20 GiB fits in 15 + 10.5 and must be accepted"
        );
        assert!(
            26 * 1024 > size_budget_mib(free, root),
            "a size past the real total must still be refused"
        );

        // Creating something new gets no such credit.
        assert_eq!(size_budget_mib(free, 0), free);

        // A rest-of-disk slot carries a sentinel, not a size: adding it would
        // overflow the budget to "anything goes".
        assert_eq!(size_budget_mib(free, MANUAL_REST), free);
    }

    /// Reported from QEMU, and the reason a rest-of-disk slot needs saying out
    /// loud: a disk held a 512 MiB ESP and a root taking whatever was left,
    /// displayed as a plain "29G". Creating a 4 GiB swap turned that root into
    /// 25G with nothing said — while the row beside the button already read
    /// "space is allocated". The button ignored its own label.
    ///
    /// A full disk is the user's decision to unmake, so the refusal has to name
    /// the slot to shrink, and shrinking it has to still work afterwards.
    #[test]
    fn a_claimed_disk_refuses_to_be_shrunk_behind_the_users_back() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.config.manual_solo = true;
        app.parts_scanned = true;
        app.parts_list = vec![];

        // An ESP, then a root that takes everything left of the disk.
        app.parts_create_disk = "/dev/vda".into();
        create_slot(&mut app, ROLE_ESP, 512);
        app.parts_create_disk = "/dev/vda".into();
        create_slot(&mut app, ROLE_ROOT, MANUAL_REST);
        let root_dev = app.config.manual_root.clone();
        let root_had = rest_bytes(&app, "/dev/vda");

        // Enter on "+ Create" — where the swap came from.
        app.cursor = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::Create { disk } if disk == "/dev/vda"))
            .expect("no create row for /dev/vda");
        handle_key(&mut app, key(KeyCode::Enter));

        assert!(
            !app.parts_create_open,
            "the create wizard opened on a disk with nothing left to give"
        );
        assert!(
            app.pmode_status.contains(&root_dev),
            "the refusal must name the slot to shrink, got {:?}",
            app.pmode_status
        );
        assert_eq!(
            rest_bytes(&app, "/dev/vda"),
            root_had,
            "the root was shrunk anyway"
        );

        // And the way out the message points at must work: Enter on the root
        // opens its size box. Blocking that would leave a disk nobody can edit.
        app.cursor = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::Part { path, is_new: true } if *path == root_dev))
            .expect("the planned root is not listed");
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            app.parts_create_open && app.parts_create_stage == 1,
            "the slot the refusal names cannot be resized"
        );
    }

    /// Reported from QEMU: with 25.5 GiB free, the box refused 26 GiB and gave
    /// no way to ask for 25.5 — the field took digits only. So the user switched
    /// to MiB and typed 25500, which is 24.9 GiB, not 25.5, and nothing on
    /// screen could have told them. Half a gibibyte is a size people want.
    #[test]
    fn a_size_can_be_typed_with_a_fraction() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.config.manual_solo = true;
        app.parts_scanned = true;
        app.parts_list = vec![];

        let open_box = |app: &mut App| {
            app.parts_create_open = true;
            app.parts_create_stage = 1;
            app.parts_create_role = ROLE_HOME;
            app.parts_create_disk = "/dev/vda".into();
            app.parts_size_mib = false; // GiB
            app.parts_size_input.clear();
            app.parts_size_pristine = false;
        };

        open_box(&mut app);
        for ch in ['2', '5', '.', '5'] {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        assert_eq!(app.parts_size_input, "25.5", "the point was swallowed");
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(
            app.config.manual_home_new_mib,
            25 * 1024 + 512,
            "25.5 GiB did not become 26112 MiB"
        );

        // A comma is the same key on the layouts this installer ships, and
        // types the same character.
        open_box(&mut app);
        for ch in ['1', ',', '5'] {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        assert_eq!(app.parts_size_input, "1.5", "a comma did not type a point");

        // But not two points, and not one on its own: a box that opens with a
        // lone dot reads as a stuck key rather than as a number.
        open_box(&mut app);
        for ch in ['.', '2', '.', '5', '.'] {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        assert_eq!(
            app.parts_size_input, "2.5",
            "the point guard let a stray in"
        );
    }

    /// A planned partition can be RESIZED in place — nothing is on the disk
    /// until the install runs, so a wrong size is a number to correct, not a
    /// partition to delete and rebuild. The slot must keep its disk.
    #[test]
    fn a_planned_partition_can_be_resized_without_being_rebuilt() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.config.manual_solo = true;
        app.parts_scanned = true;
        app.parts_list = vec![part("/dev/vdb1", "/dev/vdb", "ext4")];

        // Plan a /home of 50 GiB on the second disk — the accident to correct.
        app.parts_create_disk = "/dev/vdb".into();
        create_slot(&mut app, ROLE_HOME, 50 * 1024);
        let planned = app.config.manual_home.clone();
        assert_eq!(app.config.manual_home_new_mib, 50 * 1024);
        assert_eq!(app.config.manual_home_disk, "/dev/vdb");

        // Leaving the step and coming back clears the wizard's scratch state
        // (reset_screen_focus does exactly this). The resize must still know
        // which disk the slot is on — reading the stale scratch value used to
        // blank `manual_home_disk`, which is what made resizing look broken and
        // forced a delete-and-recreate.
        app.parts_create_disk.clear();

        // Enter on its row reopens the size box, prefilled with 50.
        app.cursor = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::Part { path, is_new: true } if *path == planned))
            .expect("the planned /home is not listed");
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            app.parts_create_open && app.parts_create_stage == 1,
            "no size box"
        );
        assert_eq!(
            app.parts_size_input, "50",
            "the box does not show the current size"
        );
        assert_eq!(app.parts_resize_dev, planned);

        // Type 20 and confirm.
        app.parts_size_input.clear();
        for ch in ['2', '0'] {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        handle_key(&mut app, key(KeyCode::Enter));

        assert_eq!(
            app.config.manual_home_new_mib,
            20 * 1024,
            "the size was not changed in place"
        );
        assert_eq!(
            app.config.manual_home_disk, "/dev/vdb",
            "resizing moved the partition to another disk"
        );
        assert!(!app.parts_create_open, "the size box stayed open");
        assert!(
            app.parts_resize_dev.is_empty(),
            "the resize target was not cleared"
        );
    }

    /// Resizing a DATA partition must change THAT partition — it used to fall
    /// through to /home, so correcting a data partition's size silently resized
    /// the home partition instead.
    #[test]
    fn resizing_a_data_partition_does_not_touch_home() {
        let mut app = app_with_a_new_data_partition();
        let dev = app
            .config
            .manual_data_new
            .first()
            .map(|dp| dp.path.clone())
            .expect("no planned data partition");
        app.config.manual_home = "/dev/vdb9".into();
        app.config.manual_home_new_mib = 8 * 1024;
        let home_before = app.config.manual_home_new_mib;

        app.cursor = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::Part { path, is_new: true } if *path == dev))
            .expect("the planned data partition is not listed");
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.parts_resize_dev, dev);
        app.parts_size_input.clear();
        handle_key(&mut app, key(KeyCode::Char('5')));
        handle_key(&mut app, key(KeyCode::Enter));

        assert_eq!(
            app.config.manual_data_new[0].mib,
            5 * 1024,
            "the data partition was not resized"
        );
        assert_eq!(
            app.config.manual_home_new_mib, home_before,
            "resizing a data partition changed /home instead"
        );
        assert!(
            !app.parts_mount_open,
            "a resize sent the user back through the mountpoint picker"
        );
    }

    /// Wiping a drive that holds another OS is ALLOWED — including the drive it
    /// boots from, and including in dual boot. Artix may be the first system on
    /// the machine with another added by hand later, so this is a legitimate
    /// intent. What guards it is consent: the choice stops to name everything
    /// that will be lost, and takes effect only once the user says they
    /// understand. Cancelling leaves the drive untouched.
    #[test]
    fn wiping_another_oss_drive_is_allowed_but_must_be_acknowledged() {
        use crate::system::disk::PARTTYPE_ESP;
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.config.manual_solo = false; // dual boot, sharing with Windows
        app.parts_scanned = true;
        app.parts_list = vec![
            part_typed("/dev/vda1", "/dev/vda", "vfat", PARTTYPE_ESP), // Windows' ESP
            part_typed("/dev/vda2", "/dev/vda", "ntfs", ""),           // Windows itself
            part("/dev/vdb1", "/dev/vdb", "ext4"),                     // a spare drive
        ];
        app.config.manual_esp = "/dev/vda1".into();

        // Nothing is refused outright except the medium we booted from.
        assert!(wipe_refusal("/dev/vda").is_none());
        assert!(wipe_refusal("/dev/vdb").is_none());

        // A drive with nothing foreign on it marks straight away — no ceremony.
        app.cursor = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::DiskHeader { disk } if disk == "/dev/vdb"))
            .expect("the spare drive's header is not reachable");
        handle_key(&mut app, key(KeyCode::Right));
        assert!(!app.parts_wipe_ack_open, "an ordinary drive should not ask");
        assert!(app.config.manual_wipe.iter().any(|w| w.disk == "/dev/vdb"));

        // The Windows drive stops to explain, and marks NOTHING yet.
        app.cursor = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::DiskHeader { disk } if disk == "/dev/vda"))
            .expect("the Windows drive's header must be reachable — no ban any more");
        handle_key(&mut app, key(KeyCode::Right));
        assert!(
            app.parts_wipe_ack_open,
            "wiping another OS's drive went ahead without asking"
        );
        assert!(
            !app.config.manual_wipe.iter().any(|w| w.disk == "/dev/vda"),
            "the drive was marked before the user acknowledged anything"
        );
        assert_eq!(
            app.parts_wipe_ack_cursor, 0,
            "the acknowledgement must default to cancel"
        );

        // It names what is lost, including that Windows will stop booting.
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 30)).unwrap();
        term.draw(|f| {
            let a = f.area();
            draw(f, &mut app, a);
        })
        .unwrap();
        let out = term.backend().to_string();
        assert!(
            out.contains("/dev/vda2"),
            "the Windows volume is not named:\n{out}"
        );
        // Match the opening of the sentence, not all of it: the modal wraps, so
        // the full string never appears on one rendered row.
        let boot_warn = t(app.lang, "parts.wipe_ack_boot");
        let opening: String = boot_warn.chars().take(30).collect();
        assert!(
            out.contains(&opening),
            "it does not warn that the other OS will stop booting:\n{out}"
        );

        // Enter on the default cancels: still unmarked.
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(!app.parts_wipe_ack_open);
        assert!(
            !app.config.manual_wipe.iter().any(|w| w.disk == "/dev/vda"),
            "cancelling the acknowledgement still marked the drive"
        );

        // Acknowledging deliberately does mark it, and the plan honours it.
        handle_key(&mut app, key(KeyCode::Right));
        handle_key(&mut app, key(KeyCode::Right)); // move to "yes, I understand"
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            app.config.manual_wipe.iter().any(|w| w.disk == "/dev/vda"),
            "an acknowledged wipe was not recorded"
        );
        assert!(disk::wipe_applies(&app.config, "/dev/vda"));

        // Asked once per drive: choosing another method does not re-ask.
        handle_key(&mut app, key(KeyCode::Right));
        assert!(
            !app.parts_wipe_ack_open,
            "it asked again for the same drive"
        );
    }

    /// What counts as "another OS" is recognised by GPT partition TYPE (an ESP
    /// is a type, not a size or a label) and by filesystem for NTFS — and once
    /// acknowledged, the header keeps the warning visible.
    #[test]
    fn a_foreign_os_is_recognised_by_partition_type_and_stays_visible() {
        use crate::system::disk::PARTTYPE_ESP;
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.config.manual_solo = true;
        app.parts_scanned = true;
        app.parts_list = vec![
            part_typed("/dev/vda1", "/dev/vda", "vfat", PARTTYPE_ESP),
            part_typed("/dev/vda2", "/dev/vda", "ntfs", ""),
            part("/dev/vda3", "/dev/vda", "ext4"), // ours — not foreign
        ];

        let found = foreign_parts(&app, "/dev/vda");
        assert_eq!(
            found.len(),
            2,
            "expected the ESP and the NTFS volume: {found:?}"
        );
        assert!(found.iter().any(|(p, _)| p == "/dev/vda1"));
        assert!(found.iter().any(|(p, _)| p == "/dev/vda2"));

        // Acknowledge, then the row itself carries the warning.
        app.cursor = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::DiskHeader { disk } if disk == "/dev/vda"))
            .unwrap();
        handle_key(&mut app, key(KeyCode::Right)); // opens the acknowledgement
        handle_key(&mut app, key(KeyCode::Right)); // → "yes, I understand"
        handle_key(&mut app, key(KeyCode::Enter));

        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 30)).unwrap();
        term.draw(|f| {
            let a = f.area();
            draw(f, &mut app, a);
        })
        .unwrap();
        let out = term.backend().to_string();
        assert!(
            out.contains(&t(app.lang, "parts.wipe_foreign_warn")),
            "the header stopped warning once acknowledged:\n{out}"
        );
        assert!(
            out.contains("/dev/vda2"),
            "the warning does not name the Windows volume:\n{out}"
        );
    }

    /// A number freed by a deletion is REUSED: delete a partition and create a
    /// new one, and it reclaims the freed number instead of being pushed to the
    /// end. This is what lets a user resize a partition (delete + recreate) and
    /// keep its number, and it needs no disk wipe.
    #[test]
    fn a_freed_partition_number_is_reused() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.parts_scanned = true;
        app.parts_list = vec![
            part("/dev/vda1", "/dev/vda", "vfat"),
            part("/dev/vda2", "/dev/vda", "ext4"),
            part("/dev/vda3", "/dev/vda", "swap"),
        ];
        // Delete vda2 and vda3, keep vda1.
        for (p, u) in [("/dev/vda2", "u2"), ("/dev/vda3", "u3")] {
            app.config.manual_deleted.push(crate::app::DeletedPart {
                path: p.into(),
                disk: "/dev/vda".into(),
                partuuid: u.into(),
            });
        }
        // A new root reclaims #2 (the lowest freed number), not #4.
        app.config.manual_root = "x".into();
        app.config.manual_root_new_mib = MANUAL_REST;
        app.config.manual_root_disk = "/dev/vda".into();
        renumber_planned(&mut app);
        assert_eq!(
            app.config.manual_root, "/dev/vda2",
            "a freed partition number was not reused"
        );

        // Delete vda1 too → the new partition can take #1.
        app.config.manual_deleted.push(crate::app::DeletedPart {
            path: "/dev/vda1".into(),
            disk: "/dev/vda".into(),
            partuuid: "u1".into(),
        });
        renumber_planned(&mut app);
        assert_eq!(
            app.config.manual_root, "/dev/vda1",
            "with everything deleted, numbering must restart at 1 without a wipe"
        );
    }

    /// The disk-header wipe strip: ←/→ marks the disk (default "nothing"), the
    /// focused header renders the strip, and stepping back to "nothing" unmarks.
    #[test]
    fn the_disk_header_strip_marks_and_unmarks_a_wipe() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.config.manual_solo = true;
        app.parts_scanned = true;
        app.parts_list = vec![part("/dev/vdb1", "/dev/vdb", "ext4")];

        // The header is landable in solo mode.
        app.cursor = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::DiskHeader { disk } if disk == "/dev/vdb"))
            .expect("no header row for /dev/vdb");
        handle_key(&mut app, key(KeyCode::Right)); // Нічого -> Швидке
        assert!(
            app.config.manual_wipe.iter().any(|w| w.disk == "/dev/vdb"),
            "→ on the header did not mark the disk for a wipe"
        );

        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 30)).unwrap();
        term.draw(|f| {
            let a = f.area();
            draw(f, &mut app, a);
        })
        .unwrap();
        let out = term.backend().to_string();
        assert!(
            out.contains(&t(app.lang, "parts.wipe_s_quick")),
            "the focused header does not render the wipe strip:\n{out}"
        );

        // Stepping left, back to "nothing", clears the mark.
        handle_key(&mut app, key(KeyCode::Left));
        assert!(
            app.config.manual_wipe.is_empty(),
            "← back to nothing did not unmark the disk"
        );
    }

    /// Enter on a marked header raises the erase confirm, which defaults to
    /// cancel — one careless Enter never erases a disk.
    #[test]
    fn a_now_wipe_confirm_defaults_to_cancel() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.config.manual_solo = true;
        app.parts_scanned = true;
        app.parts_list = vec![part("/dev/vdb1", "/dev/vdb", "ext4")];
        app.cursor = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::DiskHeader { disk } if disk == "/dev/vdb"))
            .unwrap();
        handle_key(&mut app, key(KeyCode::Right)); // pick a method
        handle_key(&mut app, key(KeyCode::Enter)); // -> the "erase now" confirm
        assert!(
            app.parts_wipe_open,
            "Enter on a marked header did not open the confirm"
        );
        assert_eq!(
            app.parts_wipe_confirm_cursor, 0,
            "the confirm must default to cancel"
        );
        handle_key(&mut app, key(KeyCode::Enter)); // Enter on the default = cancel
        assert!(!app.parts_wipe_open, "the confirm did not close");
        assert!(
            app.wipe_run_rx.is_none() && app.wipe_run_done.is_none(),
            "a wipe was launched from the default confirm"
        );
        // "No, cancel" answers the question in the title — ERASE ALL DATA? — so
        // it takes the mark back off. It used to leave it on, which meant saying
        // no to the dialog and having the install erase the disk anyway; the
        // erase-during-install choice is now its own line instead.
        assert!(
            !app.config.manual_wipe.iter().any(|w| w.disk == "/dev/vdb"),
            "cancelling left the disk marked for erasure anyway"
        );
    }

    /// The middle option keeps the mark and runs nothing now.
    ///
    /// This is the choice most people want — erase the disk as part of the
    /// install — and it had no line of its own. It happened only if you picked a
    /// method with ←/→ and then never pressed Enter, so the way to choose it was
    /// to avoid the key that shows the choices.
    #[test]
    fn the_middle_option_erases_during_the_install() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.config.manual_solo = true;
        app.parts_scanned = true;
        app.parts_list = vec![part("/dev/vdb1", "/dev/vdb", "ext4")];
        app.cursor = build_rows(&app)
            .iter()
            .position(|r| matches!(r, Row::DiskHeader { disk } if disk == "/dev/vdb"))
            .unwrap();
        handle_key(&mut app, key(KeyCode::Right)); // pick a method
        handle_key(&mut app, key(KeyCode::Enter)); // -> the confirm
        handle_key(&mut app, key(KeyCode::Down)); // cancel -> during install
        assert_eq!(app.parts_wipe_confirm_cursor, 1);
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(!app.parts_wipe_open, "the confirm did not close");
        assert!(
            app.wipe_run_rx.is_none() && app.wipe_run_done.is_none(),
            "the during-install option started erasing immediately"
        );
        assert!(
            app.config.manual_wipe.iter().any(|w| w.disk == "/dev/vdb"),
            "the during-install option did not keep the mark, so nothing would \
             be erased at all"
        );
    }

    /// A data partition in the manual editor can be encrypted, and the choice
    /// reaches the plan.
    ///
    /// The plan side always worked — `ExtraDisk::encrypt` carries LUKS through
    /// mkfs, crypttab and the keyfile. It was only ever reachable from the
    /// extra-disks step, and that step stopped appearing in manual mode once the
    /// editor took over its job, so in manual mode there was no way to ask for it
    /// at all.
    #[test]
    fn a_data_partition_can_be_encrypted_from_the_manual_editor() {
        let mut c = manual_cfg();
        c.extra_disks.push(crate::app::ExtraDisk {
            disk: "/dev/vdb2".into(),
            mountpoint: "/mnt/data".into(),
            fs: "ext4".into(),
            format: true,
            ..Default::default()
        });
        assert!(!data_is_encrypted(&c, "/dev/vdb2"));
        assert!(toggle_data_encryption(&mut c, "/dev/vdb2"));
        assert!(data_is_encrypted(&c, "/dev/vdb2"));
        assert!(toggle_data_encryption(&mut c, "/dev/vdb2"));
        assert!(
            !data_is_encrypted(&c, "/dev/vdb2"),
            "it did not turn back off"
        );
    }

    /// A partition kept AS-IS is refused: encrypting it means destroying what is
    /// on it, and keeping that is the only reason it is not being formatted.
    #[test]
    fn a_preserved_partition_is_never_encrypted() {
        let mut c = manual_cfg();
        c.extra_disks.push(crate::app::ExtraDisk {
            disk: "/dev/vdb3".into(),
            mountpoint: "/mnt/photos".into(),
            fs: "ext4".into(),
            format: false,
            ..Default::default()
        });
        assert!(
            !toggle_data_encryption(&mut c, "/dev/vdb3"),
            "a partition whose data is being kept was accepted for encryption"
        );
        assert!(!data_is_encrypted(&c, "/dev/vdb3"));
    }

    /// The happy dual-boot layout passes: reuse the vfat ESP, format root.
    #[test]
    fn a_sane_dual_boot_layout_validates() {
        assert!(validate(&manual_cfg(), &fixture(), &[], 0, false, &[]).is_ok());
    }

    #[test]
    fn legacy_bios_asks_for_the_right_boot_partition() {
        // Manual mode used to refuse BIOS outright. It is supported now — old
        // machines and unreliable UEFI firmware still need a way in — but what
        // counts as "the boot partition" changes with the firmware.
        let mut c = manual_cfg();
        c.boot_mode = BootMode::Bios;
        c.manual_esp.clear(); // an ESP is a UEFI concept
        c.manual_boot_disk = "/dev/sda".into();

        // GPT: GRUB must embed core.img in a 1 MiB EF02 partition.
        assert_eq!(
            validate(&c, &fixture(), &[], 0, true, &[]).unwrap_err().0,
            "parts.st_need_biosboot",
            "a GPT legacy install must ask for a BIOS-boot partition"
        );
        let mut with_bios = c.clone();
        with_bios.manual_bios = "/dev/sda2".into();
        assert!(
            validate(&with_bios, &fixture(), &[], 0, true, &[]).is_ok(),
            "a GPT legacy layout with a BIOS-boot partition is valid"
        );

        // MBR: GRUB uses the post-MBR gap, so nothing extra is required.
        assert!(
            validate(&c, &fixture(), &[], 0, false, &[]).is_ok(),
            "an MBR legacy layout needs no extra partition"
        );

        // But the bootloader still needs a whole disk to be written to.
        let mut no_disk = c.clone();
        no_disk.manual_boot_disk.clear();
        assert_eq!(
            validate(&no_disk, &fixture(), &[], 0, false, &[])
                .unwrap_err()
                .0,
            "parts.st_need_bootdisk"
        );

        // UEFI is unchanged: an ESP is still mandatory.
        let mut uefi = manual_cfg();
        uefi.manual_esp.clear();
        assert_eq!(
            validate(&uefi, &fixture(), &[], 0, false, &[])
                .unwrap_err()
                .0,
            "parts.st_need_esp"
        );
    }

    #[test]
    fn esp_and_root_are_mandatory() {
        let mut c = manual_cfg();
        c.manual_esp.clear();
        assert_eq!(
            validate(&c, &fixture(), &[], 0, false, &[]).unwrap_err().0,
            "parts.st_need_esp"
        );
        let mut c = manual_cfg();
        c.manual_root.clear();
        assert_eq!(
            validate(&c, &fixture(), &[], 0, false, &[]).unwrap_err().0,
            "parts.st_need_root"
        );
    }

    /// One partition cannot play two roles — that's how installs eat homes.
    #[test]
    fn a_partition_cannot_hold_two_roles() {
        let mut c = manual_cfg();
        c.manual_home = c.manual_root.clone();
        let e = validate(&c, &fixture(), &[], 0, false, &[]).unwrap_err();
        assert_eq!(e.0, "parts.st_dup");
        assert_eq!(e.1, "/dev/sda5");
    }

    /// The LUKS key stick must stay untouchable, exactly as in auto mode.
    #[test]
    fn the_key_stick_is_refused() {
        let mut c = manual_cfg();
        c.usb_key_device = "/dev/sdb".into();
        c.manual_home = "/dev/sdb1".into();
        assert_eq!(
            validate(&c, &fixture(), &[], 0, false, &[]).unwrap_err().0,
            "parts.st_key"
        );
    }

    /// A disk the extra-disks step will wipe cannot also carry a manual role.
    #[test]
    fn an_extra_disk_collision_is_refused() {
        let mut c = manual_cfg();
        c.manual_home = "/dev/sdb1".into();
        assert_eq!(
            validate(&c, &fixture(), &["/dev/sdb".into()], 0, false, &[])
                .unwrap_err()
                .0,
            "parts.st_extra"
        );
    }

    /// Reusing an "ESP" that is not FAT cannot boot: refuse until the user
    /// formats it ('f') or picks the real one.
    #[test]
    fn a_non_fat_esp_needs_formatting() {
        let mut c = manual_cfg();
        c.manual_esp = "/dev/sdb1".into(); // ext4 in the fixture
        assert_eq!(
            validate(&c, &fixture(), &[], 0, false, &[]).unwrap_err().0,
            "parts.st_esp_not_vfat"
        );
        c.manual_esp_format = true;
        assert!(validate(&c, &fixture(), &[], 0, false, &[]).is_ok());
    }

    /// An unformatted (fstype-less) root is the NORMAL case: the user just
    /// made the partition and we will format it. Must not be rejected.
    #[test]
    fn a_fresh_unformatted_root_is_fine() {
        // manual_cfg's root /dev/sda5 has fstype "" in the fixture.
        assert!(validate(&manual_cfg(), &fixture(), &[], 0, false, &[]).is_ok());
    }

    /// A layout built entirely from NEW partitions (the blank-disk case that
    /// used to be a dead end) validates: predicted paths satisfy the ESP/root
    /// requirements, and a new ESP needs no vfat check — it will be formatted.
    #[test]
    fn a_layout_of_new_partitions_validates() {
        let mut c = App::new().config;
        c.boot_mode = BootMode::Uefi;
        c.manual_disk = "/dev/vda".into();
        c.manual_esp = "/dev/vda1".into();
        c.manual_esp_new_mib = 1024;
        c.manual_esp_disk = "/dev/vda".into();
        c.manual_esp_format = true;
        c.manual_root = "/dev/vda2".into();
        c.manual_root_new_mib = MANUAL_REST;
        c.manual_root_disk = "/dev/vda".into();
        c.manual_root_fs = "btrfs".into();
        // A blank disk: the scan has nothing on /dev/vda.
        assert!(validate(&c, &[], &[], 40 * GIB, false, &[]).is_ok());
    }

    /// Only one partition can take the rest of the disk.
    #[test]
    fn two_rest_slots_are_refused() {
        let mut c = App::new().config;
        c.boot_mode = BootMode::Uefi;
        c.manual_disk = "/dev/vda".into();
        c.manual_esp = "/dev/vda1".into();
        c.manual_esp_new_mib = 1024;
        c.manual_esp_disk = "/dev/vda".into();
        c.manual_esp_format = true;
        c.manual_root = "/dev/vda2".into();
        c.manual_root_new_mib = MANUAL_REST;
        c.manual_root_disk = "/dev/vda".into();
        c.manual_home = "/dev/vda3".into();
        c.manual_home_new_mib = MANUAL_REST;
        c.manual_home_disk = "/dev/vda".into();
        assert_eq!(
            validate(&c, &[], &[], 40 * GIB, false, &[]).unwrap_err().0,
            "parts.st_two_rest"
        );
    }

    /// Planned partitions that exceed the disk's free space are refused at
    /// validation time — not discovered when sgdisk fails mid-install.
    #[test]
    fn oversized_new_partitions_are_refused() {
        let mut c = App::new().config;
        c.boot_mode = BootMode::Uefi;
        c.manual_disk = "/dev/vda".into();
        c.manual_esp = "/dev/vda1".into();
        c.manual_esp_new_mib = 1024;
        c.manual_esp_disk = "/dev/vda".into();
        c.manual_esp_format = true;
        c.manual_root = "/dev/vda2".into();
        c.manual_root_new_mib = 100 * 1024; // 100 GiB on a 40 GiB disk
        c.manual_root_disk = "/dev/vda".into();
        assert_eq!(
            validate(&c, &[], &[], 40 * GIB, false, &[]).unwrap_err().0,
            "parts.st_no_space"
        );
    }

    /// A new ESP below 64 MiB cannot hold FAT32 + a kernel.
    #[test]
    fn a_tiny_new_esp_is_refused() {
        let mut c = App::new().config;
        c.boot_mode = BootMode::Uefi;
        c.manual_disk = "/dev/vda".into();
        c.manual_esp = "/dev/vda1".into();
        c.manual_esp_new_mib = 32;
        c.manual_esp_disk = "/dev/vda".into();
        c.manual_esp_format = true;
        c.manual_root = "/dev/vda2".into();
        c.manual_root_new_mib = MANUAL_REST;
        c.manual_root_disk = "/dev/vda".into();
        assert_eq!(
            validate(&c, &[], &[], 40 * GIB, false, &[]).unwrap_err().0,
            "parts.st_esp_small"
        );
    }

    #[test]
    fn parent_heuristic_handles_the_common_names() {
        let none: Vec<Partition> = Vec::new();
        assert_eq!(parent_of(&none, "/dev/sda2"), "/dev/sda");
        assert_eq!(parent_of(&none, "/dev/nvme0n1p2"), "/dev/nvme0n1");
        assert_eq!(parent_of(&none, "/dev/mmcblk1p3"), "/dev/mmcblk1");
        // And the scan wins over the heuristic when it knows the device.
        assert_eq!(parent_of(&fixture(), "/dev/sda5"), "/dev/sda");
    }
    // temporary — appended to parts.rs tests
}
