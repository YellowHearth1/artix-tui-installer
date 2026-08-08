//! Step 9 — review & install. Left: a tidy summary of every choice. Right: the
//! live installation log with scrollback. The install runs the ordered plan one
//! Action at a time through runner::spawn; a failure halts and lets the user go
//! Back to fix the offending step. Quitting is blocked while installing.

use crate::app::App;
use crate::i18n::t;
use crate::system::disk::Action;
use crate::system::install;
use crate::system::runner::{self, LogLine};
use crate::theme;
use crossbeam_channel::Receiver;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Phase {
    Review,
    Installing,
    Failed,
    Succeeded,
}

/// One-line rendering of the GPU selection for the summary table. The selection
/// is a SET (hybrid graphics needs two stacks), so join the labels rather than
/// showing a single value.
fn gpu_summary(gpus: &[crate::app::GpuDriver]) -> String {
    use crate::app::GpuDriver;
    if gpus.is_empty() || gpus == [GpuDriver::None] {
        return "None".into();
    }
    gpus.iter()
        .map(|g| match g {
            GpuDriver::None => "None",
            GpuDriver::Nvidia => "NVIDIA",
            GpuDriver::Nvidia580xx => "NVIDIA 580xx",
            GpuDriver::Nouveau => "nouveau",
            GpuDriver::Amd => "AMD",
            GpuDriver::Intel => "Intel",
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    match app.install_phase {
        // Before install: full-width review of every choice + confirm prompt.
        Phase::Review => draw_review(f, app, area),
        // During/after install: a large, scrollable log fills the screen, with
        // a compact status line on top.
        _ => draw_install(f, app, area),
    }
    app.can_advance = false;
}

/// Full-screen review: a tidy two-column list of choices and a confirm bar.
fn draw_review(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .spacing(1)
        .split(area);

    // The review list is ~25 lines; the panel on an 80x24 console fits ~11. This
    // is the LAST screen before an irreversible install, so the rest must be
    // reachable rather than silently cut off — ↑/↓ scroll, and the title grows
    // arrows only when there is actually more to see.
    let lines = summary_lines(app);
    let total = lines.len() as u16;
    let inner_h = rows[0].height.saturating_sub(2); // inside the borders
    let max_scroll = total.saturating_sub(inner_h);
    let scroll = app.summary_scroll.min(max_scroll);
    app.summary_scroll = scroll; // clamp the stored offset so ↑/↓ never go dead

    let title = if max_scroll > 0 {
        let arrows = if scroll == 0 {
            "v"
        } else if scroll >= max_scroll {
            "^"
        } else {
            "^v"
        };
        format!(" {}  {arrows} ", t(app.lang, "sum.review"))
    } else {
        format!(" {} ", t(app.lang, "sum.review"))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border())
        .title(title)
        .title_style(theme::title());
    let inner = block.inner(rows[0]);
    f.render_widget(block, rows[0]);

    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .scroll((scroll, 0)),
        inner,
    );

    // Confirm bar.
    let confirm = Paragraph::new(Line::from(Span::styled(
        format!("  > {}", t(app.lang, "sum.confirm")),
        theme::selected(),
    )))
    .block(theme::box_rounded());
    f.render_widget(confirm, rows[1]);

    // Final irreversible-erase confirmation renders on top when open.
    if app.confirm_format_open {
        draw_confirm_format(f, app, area);
    }
}

/// The final erase-confirmation modal: spells out exactly which disk will be
/// wiped (path, model, size) and — when they exist — the existing partitions
/// that will be destroyed, then asks for an explicit Enter to proceed. This is
/// the last stop before any formatting happens; Esc backs out to the review.
fn draw_confirm_format(f: &mut Frame, app: &App, area: Rect) {
    use ratatui::widgets::{Clear, Wrap};

    let manual = app.config.partition_mode.is_manual_family();
    // Two different promises need two different confirmations. Auto: "this
    // whole disk dies" — path, model, and every existing partition on it.
    // Manual: "ONLY these partitions are touched" — each assignment with its
    // fate spelled out, a reused ESP explicitly marked as kept.
    let (disk_line, victims) = if manual {
        let c = &app.config;
        let will = t(app.lang, "sum.fmt_will");
        // A partition the editor will CREATE gets a "(new)" tag, so a dual-boot
        // user sees at a glance which entries are carved from free space versus
        // which existing volumes are formatted or reused.
        let newtag = |mib: u32| -> String {
            if mib > 0 {
                format!(" [{}]", t(app.lang, "parts.new_mark"))
            } else {
                String::new()
            }
        };
        // How big a created partition will be. Existing ones keep their size, so
        // they say nothing here; a new one is the installer's decision and the
        // user is about to authorise it, so it has to be on screen.
        // `disk` is the drive the slot is carved from, so a rest-of-disk slot
        // can be resolved to the bytes it will really receive. "Rest of the
        // disk" in the LAST confirmation before an irreversible write tells the
        // user nothing about how much they are about to commit.
        let size = |mib: u32, on: &str| -> String {
            match mib {
                0 => String::new(), // existing partition — unchanged size
                m if m == crate::app::MANUAL_REST => {
                    let b = crate::screens::parts::rest_bytes(app, on);
                    if b > 0 {
                        format!("  {}", crate::system::disk::human_size(b))
                    } else {
                        format!("  {}", t(app.lang, "parts.size_rest"))
                    }
                }
                m => format!(
                    "  {}",
                    crate::system::disk::human_size(m as u64 * 1024 * 1024)
                ),
            }
        };
        let fs = if c.manual_root_fs.is_empty() {
            "ext4"
        } else {
            c.manual_root_fs.as_str()
        };
        let mut v = vec![format!(
            "  {}{}{}  (/, {})  — {}",
            c.manual_root,
            newtag(c.manual_root_new_mib),
            size(c.manual_root_new_mib, &c.manual_root_disk),
            fs,
            will
        )];
        if !c.manual_home.is_empty() {
            let hfs = if c.manual_home_fs.is_empty() {
                "ext4"
            } else {
                c.manual_home_fs.as_str()
            };
            v.push(format!(
                "  {}{}{}  (/home, {})  — {}",
                c.manual_home,
                newtag(c.manual_home_new_mib),
                size(c.manual_home_new_mib, &c.manual_home_disk),
                hfs,
                will
            ));
        }
        if !c.manual_swap.is_empty() {
            v.push(format!(
                "  {}{}{}  (swap)  — {}",
                c.manual_swap,
                newtag(c.manual_swap_new_mib),
                size(c.manual_swap_new_mib, &c.manual_swap_disk),
                will
            ));
        }
        // The ESP only exists under UEFI. Printing it unconditionally showed a
        // phantom "(ESP)" line with no device on a legacy install.
        if !c.manual_esp.is_empty() {
            let esp_note = if c.manual_esp_format {
                will.clone()
            } else {
                t(app.lang, "sum.fmt_keep")
            };
            v.push(format!(
                "  {}{}{}  (ESP)  — {}",
                c.manual_esp,
                newtag(c.manual_esp_new_mib),
                size(c.manual_esp_new_mib, &c.manual_esp_disk),
                esp_note
            ));
        }
        // The BIOS-boot partition was missing from this list entirely: a legacy
        // install carved a partition the final confirmation never mentioned.
        if !c.manual_bios.is_empty() {
            v.push(format!(
                "  {}{}{}  ({})  — {}",
                c.manual_bios,
                newtag(c.manual_bios_new_mib),
                size(c.manual_bios_new_mib, &c.manual_bios_disk),
                t(app.lang, "parts.biosboot"),
                will
            ));
        }
        // Extra-disk entries that will be FORMATTED — including the data
        // partitions the editor creates (their mount config rides here). Each
        // one is a device losing its contents, so the last confirmation before
        // the irreversible write must name it; the mount-as-is entries preserve
        // data and are left out to keep this list about what gets destroyed.
        for e in c
            .extra_disks
            .iter()
            .filter(|e| e.format && !e.mountpoint.is_empty())
        {
            // A planned data partition carries its size in `manual_data_new`;
            // show it like every other partition being created — the user is
            // authorising exactly this much space to be written.
            let (newtag_e, size_e) = match c.manual_data_new.iter().find(|dp| dp.path == e.disk) {
                Some(dp) => (
                    format!(" [{}]", t(app.lang, "parts.new_mark")),
                    size(dp.mib, &dp.disk),
                ),
                None => (String::new(), String::new()),
            };
            v.push(format!(
                "  {}{}{}  ({}, {})  — {}",
                e.disk, newtag_e, size_e, e.mountpoint, e.fs, will
            ));
        }
        // Deletions go LAST and are named as such. They are the only entries
        // here that destroy a partition outright rather than reformatting one,
        // so the final confirmation must not let them hide among the mounts.
        for dev in &c.manual_deleted {
            v.push(format!(
                "  {}  — {}",
                dev.path,
                t(app.lang, "parts.will_delete")
            ));
        }
        // A full-disk WIPE erases the ENTIRE drive, not one partition — the most
        // destructive act here. List it FIRST so it cannot be missed. (A disk
        // erased "now" is already gone and left no mark, so only pending "later"
        // wipes appear.)
        let wlines: Vec<String> = c
            .manual_wipe
            .iter()
            .filter(|w| crate::system::disk::wipe_applies(c, &w.disk))
            .map(|w| {
                format!(
                    "  {}  — {} ({})",
                    w.disk,
                    t(app.lang, "parts.wipe_badge"),
                    t(app.lang, &format!("parts.wipe_{}", w.method.id()))
                )
            })
            .collect();
        if !wlines.is_empty() {
            v.splice(0..0, wlines);
        }
        (t(app.lang, "sum.pm_manual"), v)
    } else {
        let target = &app.config.disk;
        // Details of the target disk for the human-readable summary.
        let disk_line = crate::screens::disk::disks()
            .iter()
            .find(|d| &d.path == target)
            .map(|d| format!("{}   {}   {}", d.path, d.size, d.model))
            .unwrap_or_else(|| target.clone());

        // Existing partitions on the target that will be erased (best-effort).
        let victims: Vec<String> = crate::system::disk::list_partitions()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.parent == *target)
            .map(|p| {
                let label = if p.label.is_empty() {
                    String::new()
                } else {
                    format!(" \"{}\"", p.label)
                };
                format!("  {}  {}  {}{}", p.path, p.size, p.fstype, label)
            })
            .collect();
        (disk_line, victims)
    };

    // Build the body as (text, style) pairs so we can both size it (from the
    // plain text) and render it, without reaching into Line internals.
    let intro = if manual {
        "sum.fmt_intro_manual"
    } else {
        "sum.fmt_intro"
    };
    let mut items: Vec<(String, Style)> = vec![(t(app.lang, intro), theme::warn())];
    items.push((String::new(), theme::normal()));
    items.push((format!("  {disk_line}"), theme::normal()));
    if !victims.is_empty() {
        items.push((String::new(), theme::normal()));
        if !manual {
            // The manual header line above already introduces the list.
            items.push((t(app.lang, "sum.fmt_partitions"), theme::dim()));
        }
        for v in &victims {
            items.push((v.clone(), theme::warn()));
        }
    }
    items.push((String::new(), theme::normal()));
    items.push((t(app.lang, "sum.fmt_question"), theme::selected()));

    let body: Vec<Line> = items
        .iter()
        .map(|(txt, st)| Line::from(Span::styled(txt.clone(), *st)))
        .collect();

    let wbox = 78u16.min(area.width.saturating_sub(4));
    let inner_w = wbox.saturating_sub(2);
    let mut body_h: u16 = 0;
    for (txt, _) in &items {
        body_h = body_h.saturating_add(wrapped_height_local(txt, inner_w).max(1));
    }
    let chrome = 4u16; // 2 borders + 1 gap + 1 hint
    let max_h = area.height.saturating_sub(2);
    let h = (chrome + body_h).min(max_h).max((chrome + 2).min(max_h));
    let modal = crate::screens::widgets::modal_rect_fit(f, wbox, h, app.modal_zoom);
    f.render_widget(Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::warn())
        .title(format!(" {} ", t(app.lang, "sum.fmt_title")))
        .title_style(theme::warn());
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    f.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), parts[0]);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            t(app.lang, "sum.fmt_hint"),
            theme::mute(),
        ))),
        parts[1],
    );
}

/// Rows a wrapped line needs (local copy; mirrors the disk screen helper).
fn wrapped_height_local(text: &str, width: u16) -> u16 {
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
        lines = lines.saturating_add(1);
    }
    lines.max(1)
}

fn kv(k: &str, v: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {k:<12}"), theme::dim()),
        Span::styled(v.to_string(), theme::normal()),
    ])
}

/// The set of summary lines, shared by review and the install-side panel.
fn summary_lines(app: &App) -> Vec<Line<'static>> {
    let c = &app.config;
    let swap = if c.partition_mode.is_manual_family() {
        // Manual swap is a partition, not a size.
        if c.manual_swap.is_empty() {
            t(app.lang, "parts.none")
        } else {
            c.manual_swap.clone()
        }
    } else if c.swap_gib > 0 {
        format!("{} GiB", c.swap_gib)
    } else {
        "—".into()
    };
    let account = account_summary(app);
    let pkgs = if c.extra_packages.is_empty() {
        t(app.lang, "pkg.none_selected")
    } else {
        c.extra_packages.join(", ")
    };
    let aur = c.aur_packages.join(", ");
    let extra = if !c.extra_disks.iter().any(|d| !d.mountpoint.is_empty()) {
        "—".to_string()
    } else {
        c.extra_disks
            .iter()
            .filter(|d| !d.mountpoint.is_empty())
            .map(|d| {
                let act = if d.format { "fmt" } else { "mount" };
                format!("{} → {} ({}, {})", d.disk, d.mountpoint, d.fs, act)
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    // Desktop(s): the multi-select set joined, or a headless note when empty.
    let de_disp = {
        let labels = crate::system::install::desktops_label(c);
        if labels.is_empty() {
            "None / minimal (headless)".to_string()
        } else {
            labels
        }
    };
    // Session row: with the desktops installed side by side, the session is
    // chosen at the login screen, so show the available kinds, not one value.
    let session_disp = {
        let tag = crate::system::install::desktops_session_tag(c);
        if tag.is_empty() {
            "—".to_string()
        } else {
            format!("{tag} (picked at login)")
        }
    };
    vec![
        kv("Locale", &c.locale),
        kv("Timezone", &c.timezone),
        kv("Keymap", &c.keymap),
        kv("Layouts", &c.xkb_layouts.join(", ")),
        kv("Kernel", c.kernel.label()),
        kv("Desktop", &de_disp),
        kv("Session", &session_disp),
        kv(
            // The seat/login backend is installed and enabled for EVERY
            // configuration (X11, Wayland, even no DE), so always show what the
            // user picked in the modal — never "—".
            "Seat",
            c.seat_provider.label(),
        ),
        kv(
            "Login",
            crate::screens::options::dm_label(&c.display_manager),
        ),
        kv(
            "USB key",
            if c.usb_key_device.is_empty() {
                "—"
            } else if c.usb_key_only {
                "key-only!"
            } else {
                "key + passphrase"
            },
        ),
        kv("GPU", &gpu_summary(&c.gpu)),
        kv("Disk", &disk_review_value(app)),
        kv("Boot", c.boot_mode.label()),
        kv("Swap", &swap),
        kv("Filesystem", {
            // Manual keeps its fs choice in manual_root_fs; root_fs would show
            // the stale default here.
            if c.partition_mode.is_manual_family() {
                if c.manual_root_fs.is_empty() {
                    "ext4"
                } else {
                    c.manual_root_fs.as_str()
                }
            } else {
                c.root_fs.as_str()
            }
        }),
        kv(
            "Encryption",
            &if c.encrypt_disk {
                if c.encrypt_scope == "full" {
                    "LUKS (full, /boot too)".to_string()
                } else {
                    "LUKS (root only)".to_string()
                }
            } else {
                "—".to_string()
            },
        ),
        kv("Chaotic-AUR", if c.chaotic_aur { "Enabled" } else { "—" }),
        kv("Add. disks", &extra),
        kv(
            "Mirrors",
            if c.optimize_mirrors {
                "Ranked by speed"
            } else {
                "Default"
            },
        ),
        kv(
            "Bootloader",
            &format!(
                "{}{}",
                c.bootloader.display_name(),
                if c.boot_mode.is_uefi() {
                    format!(" ({})", c.bootloader_id)
                } else {
                    String::new()
                },
            ),
        ),
        // Dual boot: whether the installer will add the neighbouring OS to the
        // boot menu. os-prober does it for GRUB; rEFInd detects it natively;
        // Limine/EFISTUB can't, so say so plainly instead of implying it works.
        kv("Dual boot", {
            use crate::app::Bootloader;
            if c.os_prober {
                match c.bootloader {
                    Bootloader::Grub => "detect other OS (os-prober)",
                    Bootloader::Refind => "detect other OS (rEFInd auto)",
                    _ => "other OS NOT auto-added (use GRUB/rEFInd)",
                }
            } else {
                "—"
            }
        }),
        kv("Hostname", &c.hostname),
        kv("Accounts", &account),
        kv("Packages", &pkgs),
        kv("AUR", if aur.is_empty() { "—" } else { &aur }),
    ]
}

/// Human description of the chosen account mode for the review screen.
fn account_summary(app: &App) -> String {
    use crate::app::AccountMode;
    let m = app.config.account_mode;
    match m {
        AccountMode::UserSeparateRoot => format!("{} + root", app.config.username),
        AccountMode::UserSameRoot => format!("{} (+root same pw)", app.config.username),
        AccountMode::UserSudoOnly => format!("{} (sudo, root off)", app.config.username),
        AccountMode::RootOnly => "root only".to_string(),
    }
}

/// During/after install: status line + big scrollable log.
fn draw_install(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let total = app.install_plan.len().max(1);
    let step = (app.install_step + 1).min(total);
    let status = match app.install_phase {
        Phase::Installing => {
            // A shimmering rainbow progress bar. The bar width adapts to the
            // status row; the filled portion reflects step/total, and each cell's
            // hue is offset by the frame counter so the gradient flows.
            let label = format!("  {} ", t(app.lang, "sum.installing"));
            let pct = step as f32 / total as f32;
            // Reserve space for the label and a trailing percentage.
            let bar_w = (rows[0].width as usize)
                .saturating_sub(label.chars().count() + 7)
                .clamp(8, 48);
            let filled = ((bar_w as f32) * pct).round() as usize;
            let mut spans: Vec<Span> = vec![Span::styled(label, theme::gold())];
            // A thin (1-cell) white stripe that travels start → leading edge,
            // then disappears and restarts from the beginning (a looping
            // sawtooth, not a ping-pong). Nothing is left brightened behind it.
            let stripe_pos: isize = if filled > 1 {
                ((app.frame as f32 * 0.8) % (filled as f32)).floor() as isize
            } else {
                -1
            };
            for i in 0..bar_w {
                if i < filled {
                    // Filled cells: ONE flat indexed cyan — the Artix color —
                    // no RGB gradient. (The old 24-bit gradient looked fine in
                    // a terminal emulator, but the VT's 24-bit→16 palette
                    // approximation mapped parts of it to GREEN.) The thin
                    // white stripe still travels exactly as before: a single
                    // bright cell at stripe_pos, nothing trailing behind it.
                    if i as isize == stripe_pos {
                        spans.push(Span::styled("█", Style::default().fg(Color::White)));
                    } else {
                        spans.push(Span::styled("█", Style::default().fg(theme::ACCENT_SOFT)));
                    }
                } else {
                    spans.push(Span::styled("░", theme::mute()));
                }
            }
            spans.push(Span::styled(
                format!(" {:>3.0}%", pct * 100.0),
                theme::dim(),
            ));
            Line::from(spans)
        }
        Phase::Failed => Line::from(Span::styled(
            format!("  [X] {}", t(app.lang, "sum.failed")),
            theme::warn(),
        )),
        Phase::Succeeded => Line::from(Span::styled(
            format!("  [OK] {}", t(app.lang, "sum.done")),
            theme::ok(),
        )),
        Phase::Review => Line::from(""),
    };
    f.render_widget(Paragraph::new(status), rows[0]);

    // When pacman is waiting for a provider number, put the prompt bar BELOW
    // the log — that's where the eye is already tracking the scrolling output,
    // so the question and input field appear right where the user is looking.
    if app.prompt_active {
        let inner = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(6)])
            .split(rows[1]);
        draw_log(f, app, inner[0]);
        let prompt = Paragraph::new(vec![
            Line::from(Span::styled(
                format!("  {}", app.prompt_text),
                theme::gold(),
            )),
            Line::from(vec![
                Span::styled("  > ", theme::accent()),
                Span::styled(app.prompt_input.clone(), theme::normal()),
                Span::styled("|", theme::accent()),
            ]),
            Line::from(Span::styled(
                format!("  {}", t(app.lang, "sum.prompt_hint")),
                theme::dim(),
            )),
            Line::from(Span::styled(
                {
                    // Live countdown to the automatic default pick.
                    let left = app
                        .prompt_opened_at
                        .map(|t0| 60u64.saturating_sub(t0.elapsed().as_secs()))
                        .unwrap_or(60);
                    format!(
                        "  {} · {} {}s",
                        t(app.lang, "sum.prompt_hint2"),
                        t(app.lang, "sum.prompt_auto"),
                        left
                    )
                },
                theme::dim(),
            )),
        ])
        .block(
            ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(theme::accent())
                .title(format!(" {} ", t(app.lang, "sum.prompt_title")))
                .title_style(theme::accent()),
        );
        f.render_widget(prompt, inner[1]);
    } else {
        draw_log(f, app, rows[1]);
    }
}

fn draw_log(f: &mut Frame, app: &mut App, area: Rect) {
    let scroll_hint = if app.log_follow {
        t(app.lang, "sum.following")
    } else {
        t(app.lang, "sum.scrolled")
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border_dim())
        .title(format!(" {} · {} ", t(app.lang, "sum.logs"), scroll_hint))
        .title_style(theme::dim());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let height = inner.height as usize;
    let total = app.log.len();
    // Auto-follow tail unless the user scrolled up.
    let start = if app.log_follow {
        let s = total.saturating_sub(height);
        // Keep log_scroll synced to the tail position while following, so when
        // the user first presses Up we scroll up from where the view actually
        // is — not from line 0 (which would jump to the very start of the log).
        app.log_scroll = s as u16;
        s
    } else {
        (app.log_scroll as usize).min(total.saturating_sub(height.min(total)))
    };
    let visible: Vec<Line> = app
        .log
        .iter()
        .skip(start)
        .take(height)
        .map(|l| {
            if l.starts_with("$ ") {
                Line::from(Span::styled(l.clone(), theme::accent()))
            } else if l.starts_with("!! ") {
                Line::from(Span::styled(l.clone(), theme::warn()))
            } else if l.starts_with("x") {
                Line::from(Span::styled(l.clone(), theme::ok()))
            } else {
                Line::from(Span::styled(l.clone(), theme::normal()))
            }
        })
        .collect();
    f.render_widget(Paragraph::new(visible), inner);
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match app.install_phase {
        Phase::Review => {
            // While the final erase-confirmation modal is open it captures keys:
            // Enter actually starts the install, Esc backs out to the review.
            if app.confirm_format_open {
                match key.code {
                    KeyCode::Enter => {
                        app.confirm_format_open = false;
                        start_install(app);
                    }
                    KeyCode::Esc => app.confirm_format_open = false,
                    _ => {}
                }
            } else {
                match key.code {
                    // Don't format yet — open the explicit "this erases disk X"
                    // confirmation first. Formatting only begins after Enter there.
                    KeyCode::Enter => app.confirm_format_open = true,
                    KeyCode::Esc => app.goto_prev(),
                    // Scroll the review list: on a short console it is taller
                    // than the panel, and this is the last chance to read it.
                    // (Over-scrolling is clamped to the real bottom at render.)
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.summary_scroll = app.summary_scroll.saturating_sub(1)
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.summary_scroll = app.summary_scroll.saturating_add(1)
                    }
                    KeyCode::PageUp => app.summary_scroll = app.summary_scroll.saturating_sub(10),
                    KeyCode::PageDown => app.summary_scroll = app.summary_scroll.saturating_add(10),
                    _ => {}
                }
            }
        }
        // While installing the user can scroll the log but cannot leave.
        Phase::Installing => {
            if app.prompt_active {
                handle_prompt_key(app, key);
            } else {
                scroll_log(app, key);
            }
        }
        Phase::Failed => match key.code {
            // 'b' resets and goes back one screen (same as Esc, which the
            // global handler routes through reset_for_retry). Enter restarts
            // the install from scratch right here, without leaving the screen.
            KeyCode::Char('b') => {
                reset_for_retry(app);
                app.goto_prev();
            }
            KeyCode::Enter => start_install(app),
            _ => scroll_log(app, key),
        },
        Phase::Succeeded => match key.code {
            // Install finished: advance to the Finish screen directly. We bypass
            // goto_next()'s can_advance gate because draw() resets can_advance to
            // false every frame, which would otherwise swallow this Enter.
            KeyCode::Enter => {
                app.can_advance = true;
                app.screen = app.screen.next();
                app.cursor = 0;
            }
            _ => scroll_log(app, key),
        },
    }
}

/// Handle keys while pacman is waiting for a provider number. Digits build the
/// answer, Backspace edits, Enter submits it to the child via the PTY writer.
/// Empty Enter submits an empty line (pacman then takes its own default).
fn handle_prompt_key(app: &mut App, key: KeyEvent) {
    match key.code {
        // Digits build the answer; Backspace edits; Enter submits.
        KeyCode::Char(c) if c.is_ascii_digit() => app.prompt_input.push(c),
        KeyCode::Backspace => {
            app.prompt_input.pop();
        }
        KeyCode::Enter => {
            let answer = app.prompt_input.clone();
            if let Some(w) = &app.pty_writer {
                w.send_line(&answer);
            }
            app.push_log(format!("> {answer}"));
            app.prompt_active = false;
            app.prompt_input.clear();
            app.prompt_text.clear();
            app.prompt_opened_at = None;
            // Resume following the log tail: the install continues now, so snap
            // back to live output instead of leaving the user stuck wherever
            // they scrolled to while reviewing the choices.
            app.log_follow = true;
        }
        // Arrow / page / home / end keys scroll the log above, so the user can
        // review a long provider list (e.g. 16 vulkan-driver options) that
        // doesn't fit on screen, without disturbing the number being typed.
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Home
        | KeyCode::End => scroll_log(app, key),
        _ => {}
    }
}

/// Shared log scrollback controls: ↑/↓ line, PgUp/PgDn page, Home top, End tail.
fn scroll_log(app: &mut App, key: KeyEvent) {
    let total = app.log.len() as u16;
    match key.code {
        KeyCode::Up => {
            app.log_follow = false;
            app.log_scroll = app.log_scroll.saturating_sub(1);
        }
        KeyCode::Down => {
            // Quick double-tap of Down = "snap to the bottom and follow live".
            // A second Down within 400ms jumps straight to the tail and turns
            // follow back on, so the user doesn't have to scroll all the way
            // down or hold the key — the log then auto-scrolls in real time.
            let now = std::time::Instant::now();
            let double = app
                .log_last_down
                .map(|t| now.duration_since(t) <= std::time::Duration::from_millis(400))
                .unwrap_or(false);
            app.log_last_down = Some(now);
            if double {
                app.log_follow = true;
                app.log_scroll = total.saturating_sub(1);
            } else {
                // Single step down; reaching the very bottom also re-enables
                // follow (existing behaviour).
                app.log_scroll = app.log_scroll.saturating_add(1);
                if app.log_scroll >= total.saturating_sub(1) {
                    app.log_follow = true;
                }
            }
        }
        KeyCode::PageUp => {
            app.log_follow = false;
            app.log_scroll = app.log_scroll.saturating_sub(10);
        }
        KeyCode::PageDown => {
            app.log_scroll = app.log_scroll.saturating_add(10);
            if app.log_scroll >= total.saturating_sub(1) {
                app.log_follow = true;
            }
        }
        KeyCode::Home => {
            app.log_follow = false;
            app.log_scroll = 0;
        }
        KeyCode::End => {
            app.log_follow = true;
        }
        _ => {}
    }
}

fn start_install(app: &mut App) {
    app.install_plan = install::build_plan(app);
    app.install_step = 0;
    app.install_phase = Phase::Installing;
    app.install_auto_retries = 0;
    app.install_retry_at = None;
    app.log.clear();
    app.log_follow = true;
    // Time the run so every line in the LOG FILE carries how far into the
    // install it happened. Set on a retry too — the clock should measure the
    // attempt the log is recording, not the one that already failed.
    app.install_started = Some(std::time::Instant::now());
    spawn_current(app);
}

/// Reset all install state after a FAILED run so the user can go back, fix the
/// offending choice, and start the install over from a clean slate. Clears the
/// half-executed plan, the step counter, any pending prompt, and the log, and
/// flips the phase back to Review. Called by the global Esc handler when the
/// install screen is in the Failed phase.
pub fn reset_for_retry(app: &mut App) {
    app.install_phase = Phase::Review;
    app.install_plan.clear();
    app.install_step = 0;
    app.install_rx = None;
    app.pty_writer = None;
    app.prompt_active = false;
    app.prompt_input.clear();
    app.prompt_text.clear();
    app.prompt_opened_at = None;
    app.install_auto_retries = 0;
    app.install_retry_at = None;
    app.log.clear();
    app.log_follow = true;
}

/// Close the provider-choice prompt panel if it's open, clearing its text,
/// input and countdown. Called when normal tool output resumes after a prompt
/// — that means the question was answered and the tool moved on, so the panel
/// must not linger with stale text. No-op if no prompt is open.
fn close_prompt_if_open(app: &mut App) {
    if app.prompt_active {
        app.prompt_active = false;
        app.prompt_input.clear();
        app.prompt_text.clear();
        app.prompt_opened_at = None;
    }
}

/// progress-bar redraws so the in-TUI log stays clean and readable. pacman
/// emits download/progress lines containing carriage returns, long runs of '#'
/// or '-' hashmarks, and percentage bars; those look garbled in the log pane.
/// We keep meaningful lines (installing/upgrading X, warnings, errors, hooks).
fn push_clean_log(app: &mut App, line: String) {
    // Take only the part after the last carriage return (the final redraw state)
    // and trim trailing whitespace.
    let s = line.rsplit('\r').next().unwrap_or(&line).trim_end();
    if s.is_empty() {
        return;
    }
    // Download / progress-bar lines collapse into ONE live-updating line in the
    // on-screen log (never written to the file) so a long download shows movement
    // without spamming the scrollback. Detection counts ONLY '#' runs plus the
    // transfer-rate / bar-bracket markers — NOT '-' or '=', which appear all over
    // ordinary output: package-version lines like "jbig2dec-0.20-2  jasper-4.2.9-1"
    // carry many hyphens and must NOT be mistaken for a progress bar.
    let hashes = s.chars().filter(|&c| c == '#').count();
    let is_progress = hashes > 4
        || s.contains("[#")
        || s.contains("[-")
        || s.contains("KiB/s")
        || s.contains("MiB/s")
        || s.contains("GiB/s");
    if is_progress {
        // Show the REAL pacman progress line (already ANSI-stripped) — package,
        // size, transfer RATE and percent — updating IN PLACE so the user sees
        // live movement and the actual download speed, without spamming the
        // scrollback. In-memory only; the noisy per-redraw lines are never
        // written to the install-log file.
        let live = s.to_string();
        // Overwrite the previous live line when the last entry is still ours
        // (tracked by log_live); otherwise begin a fresh one.
        if app.log_live {
            if let Some(last) = app.log.last_mut() {
                *last = live;
            } else {
                app.log.push(live);
            }
        } else {
            app.log.push(live);
            app.log_live = true;
        }
        return;
    }
    // A real line: push_log records it to the on-screen log AND the file, and
    // clears log_live so the next progress update starts a fresh live line.
    app.push_log(s.to_string());
}

/// Called by the main loop after a successful interactive step (the step index
/// has already been advanced). Spawns the next step in the plan.
pub fn resume_after_interactive(app: &mut App) {
    app.push_log("x interactive step completed".to_string());
    spawn_current(app);
}

/// Called by the main loop when an interactive step failed. Routed through the
/// same handler as a streamed failure so it, too, auto-retries a mirror storm.
pub fn fail_after_interactive(app: &mut App, msg: String) {
    handle_step_failure(app, msg);
}

/// How many times a single step may auto-retry a mirror/network failure before
/// the install gives up and hands control back to the user. Per-step (reset on
/// every success), so a long install that hits several separate blips keeps
/// going, while a genuinely dead mirror set still stops instead of looping.
const MAX_AUTO_RETRIES: u32 = 5;

/// Whether a step's output looks like a transient DOWNLOAD failure — the only
/// kind worth retrying unattended. A real error (a package conflict, a full
/// disk, a bad checksum) must NOT match: retrying it just loops, and the user
/// needs to see it. So this matches pacman/curl retrieval signatures only, and
/// deliberately NOT "conflicting files" or "no space left".
fn is_mirror_failure(lines: &[String]) -> bool {
    lines.iter().any(|l| {
        l.contains("failed to retrieve")
            || l.contains("failed retrieving file")
            || l.contains("too many errors from")
            || l.contains("The requested URL returned error")
            || l.contains("Could not resolve host")
            || l.contains("Failed to connect")
            || l.contains("Connection timed out")
            || l.contains("Temporary failure in name resolution")
    })
}

/// Escalating backoff before re-running a failed step: 15s, 30s, 45s, … The
/// classic 404 storm is a sync DB gone stale against a moved-on pool; the retry
/// re-runs the step, which re-runs its own `pacman -Syu` refresh, so a short
/// wait is usually enough — but the wait grows so a slower mirror recovery
/// still gets time without a fixed long stall.
fn retry_backoff(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs(15 * attempt as u64)
}

/// The single failure handler for both streamed and interactive steps. Either
/// schedules an unattended retry of THIS step (never the whole install — that
/// would re-wipe the disk) or halts for the user.
fn handle_step_failure(app: &mut App, msg: String) {
    app.push_log(format!("!! {msg}"));
    app.install_rx = None;
    app.pty_writer = None;
    app.prompt_active = false;

    let this_step = app.log.get(app.install_step_log_start..).unwrap_or(&[]);
    if is_mirror_failure(this_step) && app.install_auto_retries < MAX_AUTO_RETRIES {
        app.install_auto_retries += 1;
        let wait = retry_backoff(app.install_auto_retries);
        app.install_retry_at = Some(std::time::Instant::now() + wait);
        app.push_log(format!(
            ">>> mirror/network failure — auto-retrying this step in {}s ({}/{}); no action needed",
            wait.as_secs(),
            app.install_auto_retries,
            MAX_AUTO_RETRIES
        ));
        // Stay in Installing so `tick` keeps running and the backoff timer fires.
        app.log_follow = true;
        return;
    }
    if app.install_auto_retries >= MAX_AUTO_RETRIES {
        app.push_log(format!(
            "!! still failing after {MAX_AUTO_RETRIES} automatic retries — stopping. \
             The mirrors may be down; check the network and press Enter to try again."
        ));
    }
    app.install_phase = Phase::Failed;
}

fn spawn_current(app: &mut App) {
    if let Some(action) = app.install_plan.get(app.install_step).cloned() {
        let Action {
            program,
            args,
            interactive,
            ..
        } = action.clone();
        // Mark where this step's output begins, so a later failure is classified
        // from THIS step's lines only — not an earlier step's 404 that was
        // already retried past.
        app.install_step_log_start = app.log.len();
        // Echo the command into the log so the user sees what's running — via
        // `log_line`, which is the ONLY place a plan step is turned into log
        // text and therefore the only place a secret could escape into it.
        app.push_log(action.log_line());
        if interactive {
            // Interactive: run under a PTY so pacman shows provider prompts. We
            // stay inside the TUI; output streams to the log, and when pacman
            // waits for a number we show an input field and send the answer
            // back through the writer. "Proceed? [Y/n]" is auto-answered Y.
            let argref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let (rx, writer) = runner::spawn_pty(&program, &argref);
            app.install_rx = Some(rx);
            app.pty_writer = Some(writer);
        } else {
            let argref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            app.install_rx = Some(runner::spawn_with_mode(&program, &argref, false));
            app.pty_writer = None;
        }
    } else {
        app.install_phase = Phase::Succeeded;
        app.push_log("x all steps completed".to_string());
    }
}

/// Drain the active command's channel; advance to the next step on success,
/// halt on failure.
pub fn tick(app: &mut App) {
    if app.install_phase != Phase::Installing {
        return;
    }
    // Backoff between auto-retries of a failed step. While waiting there is no
    // running child (install_rx is None), so this MUST come before the channel
    // drain below — otherwise the empty-channel guard would return first and the
    // timer would never fire.
    if let Some(at) = app.install_retry_at {
        if std::time::Instant::now() >= at {
            app.install_retry_at = None;
            spawn_current(app);
        }
        return;
    }
    // 60-second auto-default: an unattended provider question must not stall
    // the whole install for long (the person may have walked away). After 60s
    // with no answer we send an empty line — pacman/paru read that as "take
    // the default" — and close the panel. Logged so the choice is auditable.
    if app.prompt_active {
        if let Some(t0) = app.prompt_opened_at {
            if t0.elapsed() >= std::time::Duration::from_secs(60) {
                if let Some(w) = &app.pty_writer {
                    w.send_line("");
                }
                app.push_log("(auto) 60s, no answer → default".to_string());
                app.prompt_active = false;
                app.prompt_input.clear();
                app.prompt_text.clear();
                app.prompt_opened_at = None;
                app.log_follow = true;
            }
        }
    }
    let rx: Option<Receiver<LogLine>> = app.install_rx.clone();
    let Some(rx) = rx else { return };
    while let Ok(line) = rx.try_recv() {
        match line {
            LogLine::Out(s) => {
                // Fresh normal output means any provider question that was open
                // has been answered (by the user or the watchdog's auto-pick)
                // and the tool has moved on — so close a lingering prompt panel
                // instead of leaving it stuck with stale text and a countdown.
                close_prompt_if_open(app);
                push_clean_log(app, s);
            }
            LogLine::Err(s) => {
                close_prompt_if_open(app);
                push_clean_log(app, s);
            }
            LogLine::Prompt(p) => {
                // pacman is waiting for a provider number. Show the prompt and
                // open the input field; the user types a number and presses
                // Enter (handled in handle_key → submit_prompt).
                app.prompt_active = true;
                app.prompt_text = p.clone();
                app.prompt_input.clear();
                app.prompt_opened_at = Some(std::time::Instant::now());
                // Snap to the log tail so the provider list and the question
                // are in view when the prompt opens.
                app.log_follow = true;
                app.push_log(format!("? {p}"));
            }
            LogLine::Done(Ok(())) => {
                app.install_step += 1;
                // This step succeeded, so the next one starts with a full retry
                // budget — a blip earlier must not use up a later step's tries.
                app.install_auto_retries = 0;
                app.install_rx = None;
                app.pty_writer = None;
                app.prompt_active = false;
                spawn_current(app);
                return;
            }
            LogLine::Done(Err(e)) => {
                handle_step_failure(app, e);
                return;
            }
        }
    }
}

pub fn footer_hint(app: &App) -> String {
    match app.install_phase {
        Phase::Review => t(app.lang, "sum.footer_review"),
        Phase::Installing => t(app.lang, "sum.footer_installing"),
        Phase::Failed => t(app.lang, "sum.footer_failed"),
        Phase::Succeeded => t(app.lang, "sum.footer_done"),
    }
}

/// The "Disk" value in the review: the auto target, or the manual layout in
/// one line — the review must never show an empty disk path just because the
/// user brought their own partitions.
fn disk_review_value(app: &App) -> String {
    let c = &app.config;
    if !c.partition_mode.is_manual_family() {
        return c.disk.clone();
    }
    let fs = if c.manual_root_fs.is_empty() {
        "ext4"
    } else {
        c.manual_root_fs.as_str()
    };
    let esp_note = if c.manual_esp_format {
        t(app.lang, "sum.fmt_will")
    } else {
        t(app.lang, "sum.fmt_keep")
    };
    let mut v = format!(
        "{}: / {} ({}) · ESP {} ({})",
        t(app.lang, "sum.pm_manual_short"),
        c.manual_root,
        fs,
        c.manual_esp,
        esp_note
    );
    if !c.manual_swap.is_empty() {
        v.push_str(&format!(" · swap {}", c.manual_swap));
    }
    if !c.manual_home.is_empty() {
        v.push_str(&format!(" · /home {}", c.manual_home));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;

    /// A step is failed with `msg`, its output already in the log window.
    fn fail_with(app: &mut App, step_lines: &[&str], msg: &str) {
        app.install_phase = Phase::Installing;
        app.install_step_log_start = app.log.len();
        for l in step_lines {
            app.push_log((*l).to_string());
        }
        handle_step_failure(app, msg.to_string());
    }

    #[test]
    fn a_download_failure_is_told_apart_from_a_real_error() {
        // The 404 storm and its kin must match…
        for l in [
            "error: failed retrieving file 'xorg-server-21.1.21-2-x86_64.pkg.tar.zst' from mirror.x : The requested URL returned error: 404",
            "warning: too many errors from mirror.nju.edu.cn, skipping for the remainder of this transaction",
            "error: failed to commit transaction (failed to retrieve some files)",
            "curl: (6) Could not resolve host: mirror.example.org",
        ] {
            assert!(is_mirror_failure(&[l.to_string()]), "missed a mirror failure:\n{l}");
        }
        // …and a genuine error must NOT, or auto-retry would loop on it forever.
        for l in [
            "error: failed to commit transaction (conflicting files)",
            "lua54: /usr/bin/lua5.4 exists in filesystem (owned by lua)",
            "mkfs.ext4: No space left on device",
            "error: could not open file: invalid or corrupted package (PGP signature)",
        ] {
            assert!(
                !is_mirror_failure(&[l.to_string()]),
                "wrongly retryable:\n{l}"
            );
        }
    }

    #[test]
    fn a_mirror_storm_auto_retries_this_step_without_the_user() {
        let mut app = App::new();
        fail_with(
            &mut app,
            &["error: failed to commit transaction (failed to retrieve some files)"],
            "artix-chroot exited with 1",
        );
        // Scheduled a retry of THIS step, not a halt.
        assert_eq!(app.install_auto_retries, 1);
        assert!(app.install_retry_at.is_some(), "no retry was scheduled");
        assert_eq!(
            app.install_phase,
            Phase::Installing,
            "it halted instead of retrying"
        );
    }

    #[test]
    fn a_real_error_halts_for_the_user_instead_of_looping() {
        let mut app = App::new();
        fail_with(
            &mut app,
            &["error: failed to commit transaction (conflicting files)"],
            "artix-chroot exited with 1",
        );
        assert_eq!(
            app.install_auto_retries, 0,
            "a non-network error must not consume a retry"
        );
        assert!(app.install_retry_at.is_none());
        assert_eq!(app.install_phase, Phase::Failed);
    }

    #[test]
    fn auto_retry_gives_up_after_the_cap_so_a_dead_mirror_set_still_stops() {
        let mut app = App::new();
        // Each attempt re-fails the same way. The cap must eventually halt.
        for _ in 0..MAX_AUTO_RETRIES {
            fail_with(
                &mut app,
                &["error: failed to retrieve some files"],
                "exited with 1",
            );
            assert_eq!(app.install_phase, Phase::Installing, "gave up too early");
            // The retry fired: clear the timer as tick would before re-running.
            app.install_retry_at = None;
        }
        // One more failure past the budget: now it stops for the user.
        fail_with(
            &mut app,
            &["error: failed to retrieve some files"],
            "exited with 1",
        );
        assert_eq!(
            app.install_phase,
            Phase::Failed,
            "it never stops — an infinite loop"
        );
        assert_eq!(app.install_auto_retries, MAX_AUTO_RETRIES);
    }

    #[test]
    fn a_success_refills_the_retry_budget() {
        // A blip on an early step must not starve a later step of its retries.
        let mut app = App::new();
        fail_with(
            &mut app,
            &["failed to retrieve some files"],
            "exited with 1",
        );
        assert_eq!(app.install_auto_retries, 1);
        // Simulate the step finally succeeding on retry (as tick's Done(Ok) does).
        app.install_auto_retries = 0;
        // A later step hits its own blip and still has the full budget.
        fail_with(
            &mut app,
            &["failed to retrieve some files"],
            "exited with 1",
        );
        assert_eq!(
            app.install_auto_retries, 1,
            "the budget did not refill after success"
        );
    }

    #[test]
    fn the_backoff_grows_with_each_attempt() {
        assert!(retry_backoff(1) < retry_backoff(2));
        assert!(retry_backoff(2) < retry_backoff(3));
    }
}
