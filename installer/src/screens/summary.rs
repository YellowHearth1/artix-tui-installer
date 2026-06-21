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
use crossterm::event::{KeyCode, KeyEvent};
use crossbeam_channel::Receiver;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, BorderType, Paragraph, Wrap},
    Frame,
};

#[derive(PartialEq, Clone, Copy)]
pub enum Phase {
    Review,
    Installing,
    Failed,
    Succeeded,
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
fn draw_review(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .spacing(1)
        .split(area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border())
        .title(format!(" {} ", t(app.lang, "sum.review")))
        .title_style(theme::title());
    let inner = block.inner(rows[0]);
    f.render_widget(block, rows[0]);

    f.render_widget(Paragraph::new(summary_lines(app)).wrap(Wrap { trim: true }), inner);

    // Confirm bar.
    let confirm = Paragraph::new(Line::from(Span::styled(
        format!("  > {}", t(app.lang, "sum.confirm")),
        theme::selected(),
    )))
    .block(theme::box_rounded());
    f.render_widget(confirm, rows[1]);
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
    let swap = if c.swap_gib > 0 { format!("{} GiB", c.swap_gib) } else { "—".into() };
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
    vec![
        kv("Locale", &c.locale),
        kv("Timezone", &c.timezone),
        kv("Keymap", &c.keymap),
        kv("Layouts", &c.xkb_layouts.join(", ")),
        kv("Kernel", &c.kernel),
        kv("Desktop", &c.desktop),
        kv(
            "Session",
            if crate::system::install::desktop_has_session(&c.desktop) {
                if crate::system::install::desktop_is_wayland(&c.desktop, &c.session) {
                    "Wayland"
                } else {
                    "X11"
                }
            } else {
                "—"
            },
        ),
        kv(
            // The seat/login backend is installed and enabled for EVERY
            // configuration (X11, Wayland, even no DE), so always show what the
            // user picked in the modal — never "—".
            "Seat",
            c.seat_provider.as_str(),
        ),
        kv("Login", crate::screens::options::dm_label(&c.display_manager)),
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
        kv("GPU", &c.gpu),
        kv("Disk", &c.disk),
        kv("Boot", &c.boot_mode.to_uppercase()),
        kv("Swap", &swap),
        kv("Filesystem", &c.root_fs),
        kv(
            "Encryption",
            &if c.encrypt_disk {
                if c.encrypt_scope == "full" { "LUKS (full, /boot too)".to_string() } else { "LUKS (root only)".to_string() }
            } else {
                "—".to_string()
            },
        ),
        kv("Chaotic-AUR", if c.chaotic_aur { "Enabled" } else { "—" }),
        kv("Add. disks", &extra),
        kv("Mirrors", if c.optimize_mirrors { "Ranked by speed" } else { "Default" }),
        kv(
            "Bootloader",
            &format!(
                "{}{}",
                match c.bootloader.as_str() { "refind" => "rEFInd", "limine" => "Limine", _ => "GRUB" },
                if c.boot_mode == "uefi" { format!(" ({})", c.bootloader_id) } else { String::new() },
            ),
        ),
        kv("Hostname", &c.hostname),
        kv("Accounts", &account),
        kv("Packages", &pkgs),
        kv("AUR", if aur.is_empty() { "—" } else { &aur }),
    ]
}

/// Human description of the chosen account mode for the review screen.
fn account_summary(app: &App) -> String {
    use crate::app::AccountMode;
    let m = match app.config.account_mode.as_str() {
        "UserSameRoot" => AccountMode::UserSameRoot,
        "UserSudoOnly" => AccountMode::UserSudoOnly,
        "RootOnly" => AccountMode::RootOnly,
        _ => AccountMode::UserSeparateRoot,
    };
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
            spans.push(Span::styled(format!(" {:>3.0}%", pct * 100.0), theme::dim()));
            Line::from(spans)
        }
        Phase::Failed => Line::from(Span::styled(format!("  [X] {}", t(app.lang, "sum.failed")), theme::warn())),
        Phase::Succeeded => Line::from(Span::styled(format!("  [OK] {}", t(app.lang, "sum.done")), theme::ok())),
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
                Span::styled("▏", theme::accent()),
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
                        .map(|t0| 300u64.saturating_sub(t0.elapsed().as_secs()))
                        .unwrap_or(300);
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
            } else if l.starts_with("✓") {
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
        Phase::Review => match key.code {
            KeyCode::Enter => start_install(app),
            KeyCode::Esc => app.goto_prev(),
            _ => {}
        },
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
        KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        | KeyCode::Home | KeyCode::End => scroll_log(app, key),
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
    app.log.clear();
    app.log_follow = true;
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
    // Drop progress-bar lines: those dominated by hashmarks / box-drawing fills
    // or ending in a percentage like "100%".
    let hashish = s.chars().filter(|&c| c == '#' || c == '-' || c == '=').count();
    if hashish > 8 {
        return;
    }
    if s.contains("[#") || s.contains("[-") || s.contains("KiB/s") || s.contains("MiB/s") {
        return;
    }
    // Mirror to the on-screen log AND the install-log file, so the saved
    // ~/installer.log carries command OUTPUT (installing/upgrading lines,
    // warnings, hook messages, errors) and not just the command announcements.
    // Progress-bar spam is already filtered out above, so the file stays useful.
    app.push_log(s.to_string());
}

/// Called by the main loop after a successful interactive step (the step index
/// has already been advanced). Spawns the next step in the plan.
pub fn resume_after_interactive(app: &mut App) {
    app.push_log("✓ interactive step completed".to_string());
    spawn_current(app);
}

/// Called by the main loop when an interactive step failed. Halts the install.
pub fn fail_after_interactive(app: &mut App, msg: String) {
    app.push_log(format!("!! {msg}"));
    app.install_phase = Phase::Failed;
    app.install_rx = None;
}

fn spawn_current(app: &mut App) {
    if let Some(Action { program, args, interactive }) = app.install_plan.get(app.install_step).cloned() {
        // Echo the command into the log so the user sees what's running.
        let shown = if args.len() > 4 && program == "artix-chroot" {
            // artix-chroot /mnt sh -c '<script>' — show the script for clarity.
            format!("$ chroot: {}", args.last().cloned().unwrap_or_default())
        } else {
            format!("$ {} {}", program, args.join(" "))
        };
        app.push_log(shown);
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
        app.push_log("✓ all steps completed".to_string());
    }
}

/// Drain the active command's channel; advance to the next step on success,
/// halt on failure.
pub fn tick(app: &mut App) {
    if app.install_phase != Phase::Installing {
        return;
    }
    // 5-minute auto-default: an unattended provider question must not stall
    // the whole install forever (the person may have walked away). After 300s
    // with no answer we send an empty line — pacman/paru read that as "take
    // the default" — and close the panel. Logged so the choice is auditable.
    if app.prompt_active {
        if let Some(t0) = app.prompt_opened_at {
            if t0.elapsed() >= std::time::Duration::from_secs(300) {
                if let Some(w) = &app.pty_writer {
                    w.send_line("");
                }
                app.push_log("(auto) 5 min, no answer → default".to_string());
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
                app.install_rx = None;
                app.pty_writer = None;
                app.prompt_active = false;
                spawn_current(app);
                return;
            }
            LogLine::Done(Err(e)) => {
                app.push_log(format!("!! {e}"));
                app.install_phase = Phase::Failed;
                app.install_rx = None;
                app.pty_writer = None;
                app.prompt_active = false;
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
