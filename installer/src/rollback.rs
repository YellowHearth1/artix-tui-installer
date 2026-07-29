//! Snapshot rollback — a small, self-contained tool with two contexts.
//!
//! The installer binary doubles as the rollback tool. It is reached from
//! `main` when invoked as `artix-rollback` (the copy dropped into the target),
//! with `--rollback`, or with `--rollback-initramfs <top>`:
//!
//!   * Running system  — `artix-rollback` from the desktop/terminal. Finds the
//!     root device, mounts the pool top-level, swaps `@` for the chosen
//!     snapshot and offers to reboot.
//!   * Early boot       — `--rollback-initramfs <top>`, launched by an initramfs
//!     hook when the kernel is booted with `artix.rollback` (the bootloader's
//!     Rollback entry). The pool top-level is already mounted at `<top>`; we
//!     pick a snapshot, swap `@`, and let the boot continue into the restored,
//!     read-write root. This works even when the normal system won't boot and
//!     is bootloader-agnostic — no overlay, no kernel-version dependency.
//!
//! Both contexts share `core_swap`. In early boot, if no usable terminal is
//! available for the ratatui UI, we fall back to a plain line-based picker so a
//! recovery is always possible. When a rollback can't help (no snapshots, or it
//! fails), the user is pointed at a live-USB.

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
    Frame, Terminal,
};
use std::io::{self, Stdout};
use std::path::Path;
use std::process::Command;

use crate::i18n::{t, Lang};
use crate::theme;

/// Where the snapper "root" config keeps its snapshots on a running system.
const SNAP_DIR: &str = "/.snapshots";

/// One snapshot, parsed from `<dir>/<num>/info.xml`.
struct Snapshot {
    num: u32,
    date: String,
    kind: String, // "single" | "pre" | "post"
    desc: String,
}

/// UI state machine. `Clone` so the key handler can `match mode.clone()` and
/// freely reassign `mode` inside its own arms.
#[derive(Clone)]
enum Mode {
    List,
    Confirm,
    Working,
    Done,
    Error(String),
}

// ── entry point ──────────────────────────────────────────────────────────────

/// Run the rollback tool. Returns once the user quits, a reboot is issued, or
/// (in early boot) the boot should continue.
pub fn run() -> Result<()> {
    let lang = Lang::from_env();

    // Self-contained panic hook so a panic never leaves the terminal in raw mode.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        default_hook(info);
    }));

    // Early-boot context: `--rollback-initramfs <top>` where <top> is the
    // already-mounted pool top-level (subvolid=5).
    let args: Vec<String> = std::env::args().collect();
    let initramfs_top = args
        .iter()
        .position(|a| a == "--rollback-initramfs")
        .and_then(|i| args.get(i + 1).cloned());

    if let Some(top) = initramfs_top {
        let snaps = read_snapshots(&format!("{top}/@snapshots"));
        // Prefer the ratatui UI; if the early-boot console can't do raw mode,
        // fall back to a plain line-based picker so recovery still works.
        return match setup() {
            Ok(mut term) => {
                let res = ui_loop(&mut term, lang, &snaps, true, Some(top.as_str()));
                let _ = restore();
                res
            }
            Err(_) => run_linebased(lang, &top, &snaps),
        };
    }

    // Running-system context. Swapping subvolumes / set-default / reboot need root.
    if !is_root() {
        eprintln!("{}", t(lang, "rb.rollback_needs_root_privileges_run"));
        std::process::exit(1);
    }

    let snaps = read_snapshots(SNAP_DIR);
    let mut term = setup()?;
    let res = ui_loop(&mut term, lang, &snaps, false, None);
    restore()?;
    res
}

// ── data ─────────────────────────────────────────────────────────────────────

/// Pull the text of the first `<tag>…</tag>` out of an info.xml. info.xml is
/// flat and predictable, so a substring scan is enough (no XML dependency).
fn extract(xml: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    match (xml.find(&open), xml.find(&close)) {
        (Some(a), Some(b)) if b > a + open.len() => xml[a + open.len()..b].trim().to_string(),
        _ => String::new(),
    }
}

/// Read every numeric snapshot directory under `dir` and parse its metadata,
/// newest first. `dir` is `/.snapshots` on a running system, or
/// `<top>/@snapshots` in early boot.
fn read_snapshots(dir: &str) -> Vec<Snapshot> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            let num: u32 = match name.parse() {
                Ok(n) => n,
                Err(_) => continue,
            };
            let xml = std::fs::read_to_string(format!("{dir}/{num}/info.xml")).unwrap_or_default();
            out.push(Snapshot {
                num,
                date: extract(&xml, "date"),
                kind: extract(&xml, "type"),
                desc: extract(&xml, "description"),
            });
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.num)); // newest at the top
    out
}

// ── shelling out ─────────────────────────────────────────────────────────────

fn run_cmd(prog: &str, args: &[&str]) -> std::result::Result<(), String> {
    let out = Command::new(prog)
        .args(args)
        .output()
        .map_err(|e| format!("{prog}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("{prog} {}: {}", args.join(" "), err.trim()))
    }
}

fn capture(prog: &str, args: &[&str]) -> std::result::Result<String, String> {
    let out = Command::new(prog)
        .args(args)
        .output()
        .map_err(|e| format!("{prog}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{prog} failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn is_root() -> bool {
    capture("id", &["-u"])
        .map(|s| s.trim() == "0")
        .unwrap_or(false)
}

/// Find a free `@.rollback-…` name to move the live root aside to. This must
/// not depend on the clock: in early boot the system time isn't set yet, so
/// `date` returns the same value on every boot — using it as the only suffix
/// makes the name collide on the second rollback and the rename fails with
/// "Directory not empty". Fall back to a numeric suffix when the base is taken.
fn unique_aside(top: &str) -> String {
    let stamp = capture("date", &["+%Y%m%d-%H%M%S"])
        .unwrap_or_default()
        .trim()
        .to_string();
    let base = format!(
        "{top}/@.rollback-{}",
        if stamp.is_empty() {
            "old".to_string()
        } else {
            stamp
        }
    );
    if !Path::new(&base).exists() {
        return base;
    }
    for i in 1..100000 {
        let cand = format!("{base}.{i}");
        if !Path::new(&cand).exists() {
            return cand;
        }
    }
    format!("{base}.{}", std::process::id())
}

/// The shared core: swap the live `@` (inside the already-mounted pool top-level
/// `top`) for a fresh writable copy of snapshot `num`, and make it the btrfs
/// default so the next boot actually lands on it. Best-effort restore on failure
/// so a partial run never leaves the pool without a working `@`.
fn core_swap(top: &str, num: u32) -> std::result::Result<(), String> {
    let snap = format!("{top}/@snapshots/{num}/snapshot");
    if !Path::new(&snap).is_dir() {
        return Err(format!("snapshot {num} not found"));
    }
    let at = format!("{top}/@");
    let aside = unique_aside(top);

    // Move the live root aside (a subvolume dir rename), then materialise the
    // chosen snapshot as the new @.
    std::fs::rename(&at, &aside).map_err(|e| format!("rename @: {e}"))?;
    if let Err(e) = run_cmd(
        "btrfs",
        &["subvolume", "snapshot", snap.as_str(), at.as_str()],
    ) {
        let _ = std::fs::rename(&aside, &at); // best-effort restore
        return Err(e);
    }

    // snap-pac's PRE snapshot can carry a stale pacman lock — drop it so the
    // restored system doesn't report "unable to lock database".
    let _ = std::fs::remove_file(format!("{at}/var/lib/pacman/db.lck"));

    // Drop a marker into the restored root so the active snapshot is verifiable
    // after boot: `cat /etc/artix-rollback-active`. Best-effort; purely
    // informational (also handy for confirming a rollback actually took).
    let stamp = capture("date", &["+%Y-%m-%d %H:%M:%S"]).unwrap_or_default();
    let _ = std::fs::write(
        format!("{at}/etc/artix-rollback-active"),
        format!("snapshot={num}\nrolled-back-at={}\n", stamp.trim()),
    );

    // Repoint the btrfs *default* subvolume at the new @ — by path, so there is
    // no fragile "Subvolume ID:" parsing. This is what makes a no-subvol-pin boot
    // (the snapshots layout) mount the new @ rather than the old root. If it
    // fails, undo the swap so the system stays exactly as it was.
    if let Err(e) = run_cmd("btrfs", &["subvolume", "set-default", at.as_str()]) {
        let _ = run_cmd("btrfs", &["subvolume", "delete", at.as_str()]);
        let _ = std::fs::rename(&aside, &at);
        return Err(format!("set-default failed: {e}"));
    }

    // Ask the restored root's first boot to reconcile /boot with the snapshot
    // (kernel image, initramfs, boot menu, rescue pair) — consumed by the
    // artix-rollback-fixup one-shot service. Best-effort: a missing flag only
    // means the fixup is skipped, never a failed rollback.
    let _ = std::fs::create_dir_all(format!("{at}/var/lib/artix-rollback"));
    let _ = std::fs::write(format!("{at}/var/lib/artix-rollback/fixup-pending"), b"");

    // Prune older pre-rollback backups (keep only the one from this rollback) so
    // repeated rollbacks don't pile up @.rollback-… subvolumes. Best-effort.
    if let Ok(rd) = std::fs::read_dir(top) {
        for e in rd.flatten() {
            let n = e.file_name();
            let n = n.to_string_lossy();
            if n.starts_with("@.rollback-") {
                let p = format!("{top}/{n}");
                if p != aside {
                    let _ = run_cmd("btrfs", &["subvolume", "delete", p.as_str()]);
                }
            }
        }
    }
    // Force a synchronous btrfs transaction commit WHILE the pool is still
    // mounted, so the renamed/created subvolumes and the new default reach disk
    // before the latehook unmounts. Without it the unmount can release the
    // filesystem before the changes commit; the init then mounts the real root
    // from the SAME device, re-reads a stale on-disk subvolume tree, and
    // resolves subvol=@ to the PRE-swap subvolume — the rollback appears not to
    // take and only "works on the second try".
    let _ = run_cmd("btrfs", &["filesystem", "sync", top]);
    Ok(())
}

/// Running-system rollback: find the root device, mount the pool top-level,
/// run `core_swap`, then release the temp mount.
fn perform_rollback(num: u32) -> std::result::Result<(), String> {
    let dev_raw = capture("findmnt", &["-no", "SOURCE", "/"])?;
    let dev = dev_raw.split('[').next().unwrap_or("").trim().to_string();
    if dev.is_empty() {
        return Err("could not determine the root device".into());
    }
    let top = capture("mktemp", &["-d"])?.trim().to_string();
    if top.is_empty() {
        return Err("mktemp failed".into());
    }
    run_cmd("mount", &["-o", "subvolid=5", dev.as_str(), top.as_str()])?;
    let r = core_swap(&top, num);
    // core_swap already forces a btrfs commit; add a full page-cache flush too
    // so everything (incl. the marker) is on disk before the imminent reboot.
    let _ = run_cmd("sync", &[]);
    let _ = run_cmd("umount", &[top.as_str()]);
    r
}

/// Force an immediate reboot from the initramfs so the freshly-swapped @ is
/// mounted by the NEXT (fresh) boot. Continuing the current boot can remount
/// the PRE-swap subvolume — the running kernel already scanned the old
/// subvolume tree before the swap — which is exactly why a rollback used to
/// appear to need a second attempt.
///
/// Uses the kernel sysrq trigger so no `reboot` binary needs to live in the
/// initramfs; falls back to a `reboot` command if one happens to be present.
/// Best-effort: if every path is refused the caller lets the boot continue
/// (the previous behaviour — no worse than before).
fn reboot_now() {
    // core_swap already forced a btrfs commit; 's' flushes any remaining page
    // cache and 'b' resets the machine immediately.
    let _ = std::fs::write("/proc/sys/kernel/sysrq", "1\n");
    let _ = std::fs::write("/proc/sysrq-trigger", "s\n");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let _ = std::fs::write("/proc/sysrq-trigger", "b\n");
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _ = Command::new("reboot").arg("-f").status();
    let _ = Command::new("busybox").args(["reboot", "-f"]).status();
}

// ── line-based fallback (early boot without a raw terminal) ────────────────────

/// A dependency-light picker used in early boot when the console can't provide
/// a raw terminal for the ratatui UI. Reads a number from stdin; no raw mode.
fn run_linebased(lang: Lang, top: &str, snaps: &[Snapshot]) -> Result<()> {
    use std::io::Write;
    let mut out = io::stdout();

    if snaps.is_empty() {
        let _ = writeln!(out, "\n{}", t(lang, "rb.no_snapshots_to_roll_back"));
        return Ok(());
    }

    let _ = writeln!(out, "\n{}", t(lang, "rb.snapshots_to_roll_back_to"));
    for s in snaps {
        let date = if s.date.len() >= 16 {
            &s.date[..16]
        } else {
            s.date.as_str()
        };
        let desc = if s.desc.is_empty() {
            "-"
        } else {
            s.desc.as_str()
        };
        let _ = writeln!(out, "  {:>4}  {:<16}  {}", s.num, date, desc);
    }
    let _ = write!(out, "\n{}", t(lang, "rb.enter_a_snapshot_number_enter"));
    let _ = out.flush();

    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }
    let num: u32 = match line.parse() {
        Ok(n) => n,
        Err(_) => {
            let _ = writeln!(out, "{}", t(lang, "rb.not_a_number"));
            return Ok(());
        }
    };
    if !snaps.iter().any(|s| s.num == num) {
        let _ = writeln!(out, "{}", t(lang, "rb.no_such_snapshot"));
        return Ok(());
    }

    match core_swap(top, num) {
        Ok(()) => {
            let _ = writeln!(out, "{}", t(lang, "rb.rolled_back_rebooting_into_the"));
            let _ = out.flush();
            std::thread::sleep(std::time::Duration::from_millis(1000));
            // Reboot so the NEXT fresh boot mounts the new @ (continuing this
            // boot can remount the pre-swap subvolume).
            reboot_now();
        }
        Err(e) => {
            let _ = writeln!(
                out,
                "{} {}\n{}",
                t(lang, "rb.rollback_failed"),
                e,
                t(lang, "rb.boot_from_a_live_usb")
            );
        }
    }
    Ok(())
}

// ── terminal ─────────────────────────────────────────────────────────────────

fn setup() -> Result<Terminal<ratatui::backend::CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore() -> Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

// ── loop ─────────────────────────────────────────────────────────────────────

fn ui_loop(
    term: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    lang: Lang,
    snaps: &[Snapshot],
    initramfs: bool,
    top: Option<&str>,
) -> Result<()> {
    let mut state = ListState::default();
    if !snaps.is_empty() {
        state.select(Some(0));
    }
    let mut mode = Mode::List;

    loop {
        term.draw(|f| draw(f, lang, snaps, &mut state, &mode, initramfs))?;

        let ev = event::read()?;
        let key = match ev {
            Event::Key(k) if k.kind == KeyEventKind::Press => k.code,
            Event::Mouse(m) => {
                if matches!(mode, Mode::List) {
                    match m.kind {
                        MouseEventKind::ScrollDown => move_sel(&mut state, snaps, 1),
                        MouseEventKind::ScrollUp => move_sel(&mut state, snaps, -1),
                        _ => {}
                    }
                }
                continue;
            }
            _ => continue,
        };

        match mode.clone() {
            Mode::List => match key {
                KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => move_sel(&mut state, snaps, -1),
                KeyCode::Down | KeyCode::Char('j') => move_sel(&mut state, snaps, 1),
                KeyCode::Enter if !snaps.is_empty() => mode = Mode::Confirm,
                _ => {}
            },
            Mode::Confirm => match key {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    // Paint "Working…" before we block on the swap.
                    mode = Mode::Working;
                    term.draw(|f| draw(f, lang, snaps, &mut state, &mode, initramfs))?;
                    let num = state.selected().and_then(|i| snaps.get(i)).map(|s| s.num);
                    mode = match num {
                        Some(n) => {
                            let r = if initramfs {
                                core_swap(top.unwrap_or(""), n)
                            } else {
                                perform_rollback(n)
                            };
                            match r {
                                Ok(()) => {
                                    if initramfs {
                                        // Swap committed. Show it briefly, then reboot so
                                        // the NEXT fresh boot mounts the new @ — continuing
                                        // THIS boot can remount the pre-swap subvolume (the
                                        // "needs a second attempt" bug). If the reboot is
                                        // somehow refused we fall through to Done and let
                                        // the boot continue (old behaviour).
                                        let done = Mode::Done;
                                        let _ = term.draw(|f| {
                                            draw(f, lang, snaps, &mut state, &done, initramfs)
                                        });
                                        std::thread::sleep(std::time::Duration::from_millis(1300));
                                        reboot_now();
                                    }
                                    Mode::Done
                                }
                                Err(e) => Mode::Error(e),
                            }
                        }
                        None => Mode::List,
                    };
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => mode = Mode::List,
                _ => {}
            },
            Mode::Done => {
                if initramfs {
                    // The swap is done; any key lets the boot continue into the
                    // restored, read-write root.
                    return Ok(());
                }
                match key {
                    KeyCode::Char('r') | KeyCode::Char('R') => {
                        let _ = restore();
                        let _ = Command::new("reboot").status();
                        return Ok(());
                    }
                    KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc | KeyCode::Enter => {
                        return Ok(())
                    }
                    _ => {}
                }
            }
            Mode::Error(_) => match key {
                KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc | KeyCode::Enter => {
                    mode = Mode::List
                }
                _ => {}
            },
            Mode::Working => {}
        }
    }
}

fn move_sel(state: &mut ListState, snaps: &[Snapshot], delta: i32) {
    if snaps.is_empty() {
        return;
    }
    let n = snaps.len() as i32;
    let cur = state.selected().unwrap_or(0) as i32;
    state.select(Some((cur + delta).rem_euclid(n) as usize));
}

// ── drawing ──────────────────────────────────────────────────────────────────

fn draw(
    f: &mut Frame,
    lang: Lang,
    snaps: &[Snapshot],
    state: &mut ListState,
    mode: &Mode,
    initramfs: bool,
) {
    f.render_widget(
        Block::default().style(Style::default().bg(theme::BG)),
        f.area(),
    );

    let area = f.area();
    if area.width < 56 || area.height < 14 {
        let p = Paragraph::new(t(lang, "rb.window_too_small"))
            .style(theme::warn())
            .alignment(Alignment::Center);
        f.render_widget(
            p,
            area.inner(Margin {
                horizontal: 1,
                vertical: 1,
            }),
        );
        return;
    }

    let root = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(root);

    draw_panel(f, lang, snaps, state, initramfs, rows[0]);
    draw_footer(f, lang, mode, snaps.is_empty(), initramfs, rows[1]);

    match mode {
        Mode::Confirm => draw_confirm(f, lang, snaps, state, area),
        Mode::Working => draw_working(f, lang, area),
        Mode::Done => draw_done(f, lang, initramfs, area),
        Mode::Error(e) => draw_error(f, lang, e, initramfs, area),
        Mode::List => {}
    }
}

fn draw_panel(
    f: &mut Frame,
    lang: Lang,
    snaps: &[Snapshot],
    state: &mut ListState,
    initramfs: bool,
    area: Rect,
) {
    let title = t(lang, "rb.system_rollback");
    let block = theme::panel(&title);
    let inner = block.inner(area).inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    f.render_widget(block, area);

    if snaps.is_empty() {
        let msg = if initramfs {
            t(lang, "rb.no_snapshots_to_roll_back_2")
        } else {
            t(lang, "rb.no_snapshots_yet_n_nsnapshots")
        };
        f.render_widget(
            Paragraph::new(msg)
                .style(theme::dim())
                .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(inner);

    let intro = if initramfs {
        t(lang, "rb.pick_a_snapshot_the_system")
    } else {
        t(lang, "rb.pick_a_snapshot_to_roll")
    };
    f.render_widget(
        Paragraph::new(intro)
            .style(theme::dim())
            .wrap(Wrap { trim: true }),
        chunks[0],
    );

    let items: Vec<ListItem> = snaps
        .iter()
        .map(|s| {
            let kind = match s.kind.as_str() {
                "pre" => "PRE ",
                "post" => "POST",
                _ => "    ",
            };
            let date = if s.date.is_empty() {
                "—"
            } else {
                s.date.get(0..16).unwrap_or(s.date.as_str())
            };
            let desc = if s.desc.is_empty() {
                t(lang, "rb.no_description")
            } else {
                s.desc.clone()
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("#{:<4}", s.num), theme::accent()),
                Span::styled(format!("{kind}  "), theme::mute()),
                Span::styled(format!("{date}  "), theme::dim()),
                Span::styled(desc.to_string(), theme::normal()),
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(theme::selected())
        .highlight_symbol("▌ ");

    let overflow = snaps.len() > chunks[1].height as usize;
    let list_area = if overflow {
        Rect {
            width: chunks[1].width.saturating_sub(2),
            ..chunks[1]
        }
    } else {
        chunks[1]
    };
    f.render_stateful_widget(list, list_area, state);

    if overflow {
        let sb_area = Rect {
            x: chunks[1].x + chunks[1].width.saturating_sub(1),
            width: 1,
            ..chunks[1]
        };
        let mut sbs = ScrollbarState::new(snaps.len()).position(state.selected().unwrap_or(0));
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None);
        f.render_stateful_widget(sb, sb_area, &mut sbs);
    }
}

fn draw_footer(f: &mut Frame, lang: Lang, mode: &Mode, empty: bool, initramfs: bool, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border_dim());
    let inner = block.inner(area).inner(Margin {
        horizontal: 2,
        vertical: 0,
    });
    f.render_widget(block, area);

    let hint = match mode {
        Mode::List => {
            if empty {
                t(lang, "rb.q_quit")
            } else {
                t(lang, "rb.select_enter_roll_back_q")
            }
        }
        Mode::Confirm => t(lang, "rb.y_confirm_n_cancel"),
        Mode::Done => {
            if initramfs {
                t(lang, "rb.rebooting")
            } else {
                t(lang, "rb.r_reboot_q_quit")
            }
        }
        Mode::Error(_) => t(lang, "rb.enter_back"),
        Mode::Working => t(lang, "rb.working"),
    };

    let mut spans: Vec<Span> = Vec::new();
    for (i, seg) in hint.split('·').map(|s| s.trim()).enumerate() {
        if seg.is_empty() {
            continue;
        }
        if i > 0 {
            spans.push(Span::styled("   |   ", theme::border_dim()));
        }
        match seg.split_once(char::is_whitespace) {
            Some((key, rest)) => {
                spans.push(Span::styled(key.to_string(), theme::accent()));
                spans.push(Span::styled(format!(" {}", rest.trim()), theme::dim()));
            }
            None => spans.push(Span::styled(seg.to_string(), theme::accent())),
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}

/// A centered rectangle of a fixed size, clamped to the available area.
fn centered(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

/// Clear + draw a titled rounded modal, returning its padded inner area.
fn modal(
    f: &mut Frame,
    area: Rect,
    width: u16,
    height: u16,
    title: &str,
    border: Style,
    ts: Style,
) -> Rect {
    let rect = centered(width, height, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(format!(" {title} "))
        .title_style(ts)
        .style(Style::default().bg(theme::PANEL));
    let inner = block.inner(rect).inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    f.render_widget(block, rect);
    inner
}

fn draw_confirm(f: &mut Frame, lang: Lang, snaps: &[Snapshot], state: &mut ListState, area: Rect) {
    let s = match state.selected().and_then(|i| snaps.get(i)) {
        Some(s) => s,
        None => return,
    };

    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::styled(t(lang, "rb.roll_back_to_snapshot"), theme::normal()),
        Span::styled(format!("#{}", s.num), theme::accent()),
        Span::styled(" ?", theme::normal()),
    ])];
    if !s.desc.is_empty() {
        // Truncate the (sometimes very long) package-list description to one line
        // so it can't wrap and push the [y]/[n] prompt out of the modal.
        let desc = if s.desc.chars().count() > 52 {
            let t: String = s.desc.chars().take(51).collect();
            format!("{t}…")
        } else {
            s.desc.clone()
        };
        lines.push(Line::from(Span::styled(desc, theme::dim())));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        t(lang, "rb.your_current_root_is_kept"),
        theme::dim(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("[y] ", theme::accent()),
        Span::styled(t(lang, "rb.yes_roll_back"), theme::normal()),
        Span::styled("[n] ", theme::accent()),
        Span::styled(t(lang, "rb.cancel"), theme::normal()),
    ]));

    let h = lines.len() as u16 + 5;
    let inner = modal(
        f,
        area,
        64,
        h,
        &t(lang, "rb.confirm"),
        theme::border(),
        theme::title(),
    );
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_working(f: &mut Frame, lang: Lang, area: Rect) {
    let inner = modal(
        f,
        area,
        34,
        5,
        &t(lang, "rb.working_2"),
        theme::border(),
        theme::title(),
    );
    f.render_widget(
        Paragraph::new(t(lang, "rb.rolling_back"))
            .style(theme::normal())
            .alignment(Alignment::Center),
        inner,
    );
}

fn draw_done(f: &mut Frame, lang: Lang, initramfs: bool, area: Rect) {
    let lines = if initramfs {
        vec![
            Line::from(Span::styled(t(lang, "rb.rollback_complete"), theme::ok())),
            Line::from(""),
            Line::from(Span::styled(
                t(lang, "rb.rebooting_into_the_restored_system"),
                theme::dim(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                t(lang, "rb.if_it_doesn_t_reboot"),
                theme::mute(),
            )),
        ]
    } else {
        vec![
            Line::from(Span::styled(t(lang, "rb.rollback_complete_2"), theme::ok())),
            Line::from(""),
            Line::from(Span::styled(
                t(lang, "rb.reboot_to_enter_the_restored"),
                theme::dim(),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("[r] ", theme::accent()),
                Span::styled(t(lang, "rb.reboot_now"), theme::normal()),
                Span::styled("[q] ", theme::accent()),
                Span::styled(t(lang, "rb.quit"), theme::normal()),
            ]),
        ]
    };
    let h = lines.len() as u16 + 4;
    let inner = modal(
        f,
        area,
        56,
        h,
        &t(lang, "rb.done"),
        theme::border(),
        theme::title(),
    );
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_error(f: &mut Frame, lang: Lang, e: &str, initramfs: bool, area: Rect) {
    let mut lines = vec![
        Line::from(Span::styled(t(lang, "rb.rollback_failed_2"), theme::warn())),
        Line::from(""),
        Line::from(Span::styled(e.to_string(), theme::dim())),
        Line::from(""),
    ];
    if initramfs {
        lines.push(Line::from(Span::styled(
            t(lang, "rb.boot_from_a_live_usb_2"),
            theme::dim(),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            t(lang, "rb.the_system_was_not_changed"),
            theme::dim(),
        )));
    }
    let h = lines.len() as u16 + 6;
    let inner = modal(
        f,
        area,
        66,
        h,
        &t(lang, "rb.error"),
        theme::warn(),
        theme::warn(),
    );
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
