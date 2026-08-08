//! "Install alongside" — the zero-layout dual-boot path (Bazzite/Ubuntu-style).
//!
//! The user picks nothing to partition: the installer takes the disk's EXISTING
//! free space, reuses the existing (Windows) ESP, and creates a single root
//! partition in the gap. Windows and every other partition are left exactly as
//! they are; os-prober then adds the neighbour to the GRUB menu.
//!
//! It is a thin, auto-filling front end over the MANUAL plan: on "Continue" it
//! writes the `manual_*` config (disk, reused ESP at /boot/efi, a rest-of-free-
//! space root) and the shared `build_manual_plan` does the rest — additive
//! `sgdisk -n` into the free block, never a wipe. The one thing it adds over
//! manual is judgement: it finds the ESP and the free space for you.
//!
//! It deliberately does NOT shrink Windows: resizing NTFS from the installer is
//! how data dies. The user makes room first (Windows Disk Management → Shrink),
//! reboots into the live medium, and this screen uses what's now unallocated.

use crate::app::{App, InstallConfig, MANUAL_REST};
use crate::i18n::t;
use crate::system::disk::{self, Partition};
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};

/// The minimum free space worth offering: below this a desktop install has no
/// breathing room. 15 GiB — enough for the base + a desktop, with headroom.
const MIN_FREE_BYTES: u64 = 15 * 1024 * 1024 * 1024;

/// Filesystems offered (same two the manual editor leads with).
// The SAME seven filesystems the manual editor and the auto disk face offer —
// "alongside" is not a reason to take choices away. (It shipped with only
// btrfs/ext4, which read as an arbitrary restriction.)
use crate::screens::parts::FS_CHOICES;

/// Root must keep at least this much after a swap partition is carved out —
/// otherwise the swap size is refused rather than leaving a cramped root.
const MIN_ROOT_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Navigable rows.
const ROW_DISK: usize = 0;
const ROW_FS: usize = 1;
const ROW_SWAP: usize = 2;
const ROW_CONTINUE: usize = 3;
const ROWS: usize = 4;

/// The computed plan for one disk — what alongside WOULD do, and whether it can.
struct Plan {
    disk: String,
    esp: Option<String>,
    esp_size: String,
    free: u64,
    other_os: String, // localized "Windows" / "another OS" / ""
    /// The next free partition NUMBER on the disk (after the highest existing
    /// one). Swap, when enabled, takes this number and root takes the next; with
    /// no swap, root takes it. Kept as a number so both paths derive from it.
    next: u32,
    /// None = viable; Some(i18n key) = why not.
    blocked: Option<&'static str>,
}

impl Plan {
    /// Swap partition path + size in bytes when swap is enabled (config
    /// `swap_gib > 0`), else None. Swap takes the first free number.
    fn swap(&self, swap_gib: u32) -> Option<(String, u64)> {
        (swap_gib > 0).then(|| {
            (
                disk::part(&self.disk, self.next),
                swap_gib as u64 * 1024 * 1024 * 1024,
            )
        })
    }
    /// The root partition path — after swap if there is one.
    fn root_path(&self, swap_gib: u32) -> String {
        let n = if swap_gib > 0 {
            self.next + 1
        } else {
            self.next
        };
        disk::part(&self.disk, n)
    }
    /// Bytes left for root after the ESP (existing) and any swap are accounted
    /// for. Root fills the rest of the free block.
    fn root_bytes(&self, swap_gib: u32) -> u64 {
        self.free
            .saturating_sub(swap_gib as u64 * 1024 * 1024 * 1024)
    }
}

fn root_fs_eff(c: &InstallConfig) -> &str {
    if c.manual_root_fs.is_empty() {
        "btrfs"
    } else {
        c.manual_root_fs.as_str()
    }
}

/// Free bytes on a disk: total − everything already partitioned − a little GPT
/// and alignment overhead. An estimate of the unallocated space (sgdisk fills
/// the largest single free block from it).
fn free_on(disk: &str, parts: &[Partition]) -> u64 {
    let total = disk::disks_list()
        .iter()
        .find(|d| d.path == disk)
        .map(|d| d.size_bytes)
        .unwrap_or(0);
    if total == 0 {
        return 0;
    }
    let used: u64 = parts
        .iter()
        .filter(|p| p.parent == disk)
        .map(|p| p.size_bytes)
        .sum();
    total.saturating_sub(used).saturating_sub(4 * 1024 * 1024)
}

/// Analyse one disk: find its ESP, its free space and any neighbour OS.
fn analyse(app: &App, disk: &str) -> Plan {
    let parts: Vec<&Partition> = app.parts_list.iter().filter(|p| p.parent == disk).collect();
    // The ESP is the FAT partition (a dual-boot disk's Windows/other ESP).
    let esp = parts.iter().find(|p| p.fstype == "vfat");
    let has_ntfs = parts.iter().any(|p| p.fstype == "ntfs");
    let other_os = if has_ntfs {
        t(app.lang, "along.windows")
    } else if !parts.is_empty() {
        t(app.lang, "along.other_os")
    } else {
        String::new()
    };
    let free = free_on(disk, &app.parts_list);
    // Predicted next free partition number: after the highest existing one.
    let next = parts
        .iter()
        .filter_map(|p| disk::part_number(disk, &p.path))
        .max()
        .unwrap_or(0)
        + 1;

    let swap_gib = app.config.swap_gib;
    let swap_bytes = swap_gib as u64 * 1024 * 1024 * 1024;
    let blocked = if !app.config.boot_mode.is_uefi() {
        Some("along.need_uefi")
    } else if esp.is_none() {
        Some("along.no_esp")
    } else if free < MIN_FREE_BYTES {
        Some("along.no_space")
    } else if swap_gib > 0 && free.saturating_sub(swap_bytes) < MIN_ROOT_BYTES {
        // A swap so large the root would be cramped: refuse the size, don't
        // silently shrink root to nothing.
        Some("along.swap_too_big")
    } else {
        None
    };

    Plan {
        disk: disk.to_string(),
        esp: esp.map(|p| p.path.clone()),
        esp_size: esp.map(|p| p.size.clone()).unwrap_or_default(),
        free,
        other_os,
        next,
        blocked,
    }
}

/// The disk alongside should default to: the viable candidate with the most
/// free space, else the one with the most free space regardless, else the
/// first disk. Auto-picked on entry so the common single-disk case is one
/// keypress (Continue).
fn best_disk(app: &App) -> String {
    let disks = disk::disks_list();
    let mut best: Option<(bool, u64, String)> = None;
    for d in disks {
        let p = analyse(app, &d.path);
        let key = (p.blocked.is_none(), p.free, d.path.clone());
        if best.as_ref().is_none_or(|b| (key.0, key.1) > (b.0, b.1)) {
            best = Some(key);
        }
    }
    best.map(|b| b.2).unwrap_or_default()
}

fn current_disk(app: &App) -> String {
    if !app.config.manual_disk.is_empty() {
        app.config.manual_disk.clone()
    } else {
        best_disk(app)
    }
}

// ── drawing ─────────────────────────────────────────────────────────────────

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    if !app.parts_scanned {
        app.parts_list = disk::list_partitions_all().unwrap_or_default();
        app.parts_scanned = true;
    }
    // The disk is COMPUTED here (best candidate until the user picks one),
    // never stored — drawing must not decide config. `apply()` on Continue is
    // what commits `manual_disk`; the disk modal commits an explicit choice.
    let disk = current_disk(app);
    let plan = analyse(app, &disk);
    app.can_advance = plan.blocked.is_none();

    let cursor = app.cursor.min(ROWS - 1);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // disk box
            Constraint::Min(5),    // the plan box
            Constraint::Length(4), // options box (filesystem + swap)
            Constraint::Length(3), // continue / status box
        ])
        .spacing(0)
        .split(area);

    draw_disk_box(f, app, rows[0], &plan, cursor == ROW_DISK);
    draw_plan_box(f, app, rows[1], &plan);
    draw_options_box(f, app, rows[2], cursor);
    draw_action_box(f, app, rows[3], &plan, cursor == ROW_CONTINUE);

    if app.parts_disk_modal_open {
        draw_disk_modal(f, app, area);
    }
    if app.fs_opts_modal_open {
        let fs = root_fs_eff(&app.config).to_string();
        crate::screens::disk::draw_fs_opts_modal(f, app, area, &fs);
    }
}

/// A titled rounded box, accent border when the row is focused.
fn boxed(title: &str, focused: bool) -> Block<'static> {
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

fn draw_disk_box(f: &mut Frame, app: &App, area: Rect, plan: &Plan, focused: bool) {
    let block = boxed(&t(app.lang, "parts.disk"), focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let disks = disk::disks_list();
    // Drop the model on a narrow panel so the "[N disks — Enter]" chip isn't
    // clipped off the right edge.
    let compact = area.width < 78;
    let info = disks
        .iter()
        .find(|d| d.path == plan.disk)
        .map(|d| {
            if compact {
                format!("{}  {}", d.path, d.size)
            } else {
                format!("{}  {}  {}", d.path, d.size, d.model)
            }
        })
        .unwrap_or_else(|| t(app.lang, "parts.not_set"));
    let mut spans = vec![Span::styled(format!(" {info}"), theme::normal())];
    if disks.len() > 1 {
        spans.push(Span::styled(
            format!(
                "   [{}]",
                t(app.lang, "parts.disks_count").replace("{n}", &disks.len().to_string())
            ),
            theme::accent(),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn draw_plan_box(f: &mut Frame, app: &App, area: Rect, plan: &Plan) {
    let block = boxed(&t(app.lang, "along.plan_title"), false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    if let Some(key) = plan.blocked {
        // Not viable — say why, plainly, and what to do instead.
        lines.push(Line::from(Span::styled(
            format!(" [!] {}", t(app.lang, key)),
            theme::warn(),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("   {}", t(app.lang, "along.suggest")),
            theme::dim(),
        )));
    } else {
        let fs = root_fs_eff(&app.config);
        let fs_note = if fs == "btrfs" {
            t(app.lang, "along.btrfs_note")
        } else {
            String::new()
        };
        // "Alongside Windows on /dev/sda":
        let head = if plan.other_os.is_empty() {
            t(app.lang, "along.head_generic").replace("{disk}", &plan.disk)
        } else {
            t(app.lang, "along.head")
                .replace("{os}", &plan.other_os)
                .replace("{disk}", &plan.disk)
        };
        lines.push(Line::from(Span::styled(
            format!(" {head}"),
            theme::heading(),
        )));
        lines.push(Line::from(""));
        let swap_gib = app.config.swap_gib;
        // • root: NN GiB (btrfs, snapshots) — size is what's left after swap.
        lines.push(bullet(
            app,
            &format!(
                "{}: {} ({}{})",
                t(app.lang, "along.b_root"),
                disk::human_size(plan.root_bytes(swap_gib)),
                fs,
                if fs_note.is_empty() {
                    String::new()
                } else {
                    format!(", {fs_note}")
                }
            ),
        ));
        // • swap: NN GiB (only when the user turned it on)
        if let Some((_, bytes)) = plan.swap(swap_gib) {
            lines.push(bullet(
                app,
                &format!(
                    "{}: {}",
                    t(app.lang, "along.b_swap"),
                    disk::human_size(bytes)
                ),
            ));
        }
        // • ESP: /dev/sda1 shared, mounted at /boot/efi
        if let Some(esp) = &plan.esp {
            lines.push(bullet(
                app,
                &format!(
                    "{}: {} {} \u{2192} /boot/efi",
                    t(app.lang, "along.b_esp"),
                    esp,
                    plan.esp_size
                ),
            ));
        }
        // • the neighbour + everything else is left untouched
        lines.push(bullet(app, &t(app.lang, "along.b_keep")));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn bullet(app: &App, text: &str) -> Line<'static> {
    let _ = app;
    Line::from(vec![
        Span::styled("  \u{2022} ".to_string(), theme::accent()),
        Span::styled(text.to_string(), theme::normal()),
    ])
}

/// The two user choices, in one box: root FILESYSTEM (btrfs / ext4) and SWAP
/// (off, or a size in GiB). Each is a row; the focused one is highlighted and
/// driven by ←/→ (and Space toggles swap).
fn draw_options_box(f: &mut Frame, app: &App, area: Rect, cursor: usize) {
    let focused = cursor == ROW_FS || cursor == ROW_SWAP;
    let block = boxed(&t(app.lang, "along.opts_title"), focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Filesystem row.
    let fs_focus = cursor == ROW_FS;
    let cur = root_fs_eff(&app.config);
    let mut fs_spans: Vec<Span> = vec![Span::styled(
        format!(" {:<10} ", format!("{}:", t(app.lang, "disk.fs"))),
        if fs_focus {
            theme::title()
        } else {
            theme::dim()
        },
    )];
    // The [o] cue is reserved BEFORE the strip is measured, so the seven-entry
    // reveal can never grow into the space the cue needs (same discipline as
    // the manual editor's rows).
    let cue = if cur == "btrfs" {
        Some(format!("   {}", t(app.lang, "parts.opts_cue")))
    } else {
        None
    };
    let reserved = cue.as_deref().map(|c| c.chars().count()).unwrap_or(0);
    if fs_focus {
        let used: usize = fs_spans.iter().map(|s| s.content.chars().count()).sum();
        let budget = (inner.width as usize)
            .saturating_sub(used)
            .saturating_sub(reserved + 1);
        fs_spans.extend(crate::screens::parts::option_strip_fit(
            &FS_CHOICES,
            cur,
            budget,
        ));
    } else {
        fs_spans.push(Span::styled(cur.to_string(), theme::accent()));
    }
    if let Some(c) = cue {
        fs_spans.push(Span::styled(c, theme::title()));
    }

    // Swap row.
    let sw_focus = cursor == ROW_SWAP;
    let mut sw_spans: Vec<Span> = vec![Span::styled(
        format!(" {:<10} ", "SWAP:"),
        if sw_focus {
            theme::title()
        } else {
            theme::dim()
        },
    )];
    if app.config.swap_gib == 0 {
        sw_spans.push(Span::styled(
            t(app.lang, "along.swap_off"),
            if sw_focus {
                theme::gold()
            } else {
                theme::mute()
            },
        ));
        sw_spans.push(Span::styled(
            format!("   {}", t(app.lang, "along.swap_toggle")),
            theme::dim(),
        ));
    } else {
        let arrow = if sw_focus {
            theme::gold()
        } else {
            theme::mute()
        };
        sw_spans.push(Span::styled("\u{2039} ", arrow));
        sw_spans.push(Span::styled(
            format!("{} GiB", app.config.swap_gib),
            if sw_focus {
                theme::normal()
            } else {
                theme::dim()
            },
        ));
        sw_spans.push(Span::styled(" \u{203a}", arrow));
        sw_spans.push(Span::styled(
            format!("   {}", t(app.lang, "along.swap_toggle")),
            theme::dim(),
        ));
    }

    f.render_widget(
        Paragraph::new(vec![Line::from(fs_spans), Line::from(sw_spans)]),
        inner,
    );
}

fn draw_action_box(f: &mut Frame, app: &App, area: Rect, plan: &Plan, focused: bool) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            theme::border()
        } else {
            theme::border_dim()
        });
    let inner = block.inner(area);
    f.render_widget(block, area);
    // Focused → the standard bright [Enter] button, like every action row in
    // the wizard. `ok()` (plain Cyan) made the one control that fires the
    // install look dimmer than the labels around it.
    let line = match plan.blocked {
        None => Line::from(Span::styled(
            if focused {
                format!("  > [Enter] {}", t(app.lang, "parts.continue"))
            } else {
                format!("  > {}", t(app.lang, "parts.continue"))
            },
            if focused {
                theme::gold()
            } else {
                theme::normal()
            },
        )),
        Some(_) => Line::from(Span::styled(
            format!("  {}", t(app.lang, "along.blocked_hint")),
            theme::mute(),
        )),
    };
    f.render_widget(Paragraph::new(line), inner);
}

/// The disk picker (shared shape with the manual editor's).
fn draw_disk_modal(f: &mut Frame, app: &App, area: Rect) {
    let disks = disk::disks_list();
    let mut lines: Vec<Line> = Vec::new();
    for (i, dk) in disks.iter().enumerate() {
        let sel = i == app.parts_disk_cursor.min(disks.len().saturating_sub(1));
        let pre = if sel { "> " } else { "  " };
        let p = analyse(app, &dk.path);
        let mut spans = vec![Span::styled(
            format!("{pre}{}  {}  {}", dk.path, dk.size, dk.model),
            if sel {
                theme::selected()
            } else {
                theme::normal()
            },
        )];
        // Show each disk's free space so the choice is informed.
        if p.blocked.is_none() {
            spans.push(Span::styled(
                format!(
                    "   {}",
                    t(app.lang, "along.free_space").replace("{n}", &disk::human_size(p.free))
                ),
                theme::ok(),
            ));
        } else {
            spans.push(Span::styled(
                format!("   \u{2014} {}", t(app.lang, "along.na")),
                theme::dim(),
            ));
        }
        lines.push(Line::from(spans));
    }
    let w = 70u16.min(area.width.saturating_sub(4)).max(24);
    let h = (disks.len() as u16 + 4)
        .min(area.height.saturating_sub(2))
        .max(5);
    let rect = crate::screens::widgets::modal_rect_fit(f, w, h, app.modal_zoom);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {} ", t(app.lang, "parts.disk_title")));
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

// ── keys ────────────────────────────────────────────────────────────────────

pub fn handle_key(app: &mut App, key: KeyEvent) {
    if app.fs_opts_modal_open {
        let fs = root_fs_eff(&app.config).to_string();
        crate::screens::disk::fs_opts_modal_key(app, key, &fs);
        return;
    }
    if app.parts_disk_modal_open {
        disk_modal_key(app, key);
        return;
    }
    // Screen-wide keys: row movement and rescan.
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.cursor = app.cursor.saturating_sub(1);
            return;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.cursor < ROWS - 1 {
                app.cursor += 1;
            }
            return;
        }
        KeyCode::Char('r' | 'R') => {
            app.parts_scanned = false;
            return;
        }
        _ => {}
    }
    // Per-row actions, flattened so the match reads as (row, key) pairs.
    match (app.cursor.min(ROWS - 1), key.code) {
        (ROW_DISK, KeyCode::Enter) if disk::disks_list().len() > 1 => {
            // Preselect the current disk in the picker.
            if let Some(i) = disk::disks_list()
                .iter()
                .position(|d| d.path == current_disk(app))
            {
                app.parts_disk_cursor = i;
            }
            app.parts_disk_modal_open = true;
        }
        (ROW_FS, KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l')) => {
            let cur = root_fs_eff(&app.config).to_string();
            let i = FS_CHOICES.iter().position(|f| *f == cur).unwrap_or(0) as i32;
            let dir: i32 = if matches!(key.code, KeyCode::Left | KeyCode::Char('h')) {
                -1
            } else {
                1
            };
            let n = (i + dir).rem_euclid(FS_CHOICES.len() as i32) as usize;
            app.config.manual_root_fs = FS_CHOICES[n].into();
            // btrfs defaults belong to the SWITCH, not to Enter: subvolumes +
            // snapshots come on when btrfs is picked, so the [o] modal opens
            // showing the real state — and anything the user changes there
            // survives, because apply() no longer overwrites it.
            let btrfs = FS_CHOICES[n] == "btrfs";
            app.config.btrfs_subvolumes = btrfs;
            app.config.btrfs_snapshots = btrfs;
        }
        // `o` opens the same filesystem-options modal the other faces have —
        // subvolumes, snapshots, compression, TRIM — once btrfs is selected.
        (ROW_FS, KeyCode::Char('o' | 'O')) => {
            if root_fs_eff(&app.config) == "btrfs" {
                app.fs_opts_target = crate::app::FsOptsTarget::Root;
                app.fs_opt_cursor = 0;
                app.fs_opts_modal_open = true;
            }
        }
        // Swap: Space toggles it on/off (default 4 GiB), ←/→ adjusts the size.
        (ROW_SWAP, KeyCode::Char(' ')) => {
            app.config.swap_gib = if app.config.swap_gib > 0 { 0 } else { 4 };
        }
        (ROW_SWAP, KeyCode::Right | KeyCode::Char('l' | '+')) => {
            if app.config.swap_gib > 0 && app.config.swap_gib < 64 {
                app.config.swap_gib += 1;
            }
        }
        (ROW_SWAP, KeyCode::Left | KeyCode::Char('h' | '-')) => {
            if app.config.swap_gib > 1 {
                app.config.swap_gib -= 1;
            }
        }
        (ROW_CONTINUE, KeyCode::Enter) => {
            let disk = current_disk(app);
            if analyse(app, &disk).blocked.is_none() {
                apply(app, &disk);
                app.can_advance = true;
                app.goto_next();
            }
        }
        _ => {}
    }
}

fn disk_modal_key(app: &mut App, key: KeyEvent) {
    let disks = disk::disks_list();
    let n = disks.len().max(1);
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.parts_disk_cursor = (app.parts_disk_cursor + n - 1) % n;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.parts_disk_cursor = (app.parts_disk_cursor + 1) % n;
        }
        KeyCode::Esc => app.parts_disk_modal_open = false,
        KeyCode::Enter => {
            if let Some(dk) = disks.get(app.parts_disk_cursor) {
                app.config.manual_disk = dk.path.clone();
            }
            app.parts_disk_modal_open = false;
        }
        _ => {}
    }
}

/// Write the manual-plan config from the analysed plan, so `build_manual_plan`
/// creates the root in free space and reuses the ESP. Called on Continue.
fn apply(app: &mut App, disk: &str) {
    let plan = analyse(app, disk);
    let fs = root_fs_eff(&app.config).to_string();
    let swap_gib = app.config.swap_gib;
    let swap = plan.swap(swap_gib);
    let root_path = plan.root_path(swap_gib);
    let c = &mut app.config;
    c.manual_disk = disk.to_string();
    // Reuse the existing ESP at /boot/efi (a small Windows ESP can't hold
    // kernels), never reformatted.
    if let Some(esp) = plan.esp {
        c.manual_esp = esp;
    }
    c.manual_esp_format = false;
    c.manual_esp_mount = "/boot/efi".into();
    c.manual_esp_new_mib = 0;
    // Swap: a NEW fixed-size partition carved BEFORE root (so root fills what's
    // left). Off when the user left it off.
    if let Some((path, bytes)) = swap {
        c.manual_swap = path;
        c.manual_swap_new_mib = (bytes / (1024 * 1024)) as u32;
    } else {
        c.manual_swap.clear();
        c.manual_swap_new_mib = 0;
    }
    // Root: a NEW partition filling the rest of the free block.
    c.manual_root = root_path;
    c.manual_root_new_mib = MANUAL_REST;
    c.manual_root_fs = fs.clone();
    // No separate /home — keep it "nothing to decide" beyond fs + swap.
    c.manual_home.clear();
    c.manual_home_new_mib = 0;
    // btrfs defaults (subvolumes + snapshots) are applied when the filesystem
    // is SWITCHED, in the ←/→ handler — not here. Enter used to re-enable them,
    // silently undoing whatever the user had just set in the [o] modal.
}

pub fn footer_hint(app: &App) -> String {
    if app.fs_opts_modal_open {
        return t(app.lang, "disk.fsopt_modal_hint");
    }
    if app.parts_disk_modal_open {
        return t(app.lang, "parts.hint_modal");
    }
    match app.cursor.min(ROWS - 1) {
        ROW_DISK => t(app.lang, "along.hint_disk"),
        ROW_FS => t(app.lang, "along.hint_fs"),
        ROW_SWAP => t(app.lang, "along.hint_swap"),
        _ => t(app.lang, "along.hint_go"),
    }
}
