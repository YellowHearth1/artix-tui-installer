//! Entry point + the "graphical installer" chrome.
//!
//! Layout (EndeavourOS/Bazzite vibe):
//!   ┌─────────────┬────────────────────────────────────────────┐
//!   │  SIDEBAR    │  CONTENT PANEL (titled, rounded)            │
//!   │  ● step 1   │                                             │
//!   │  ● step 2   │   <active screen body>                      │
//!   │  ▸ step 3   │                                             │
//!   │  ○ step 4   │                                             │
//!   │   …         ├────────────────────────────────────────────┤
//!   │             │  FOOTER: contextual key hints               │
//!   └─────────────┴────────────────────────────────────────────┘

mod app;
mod event;
mod i18n;
mod rollback;
mod screens;
mod system;
mod theme;

use anyhow::Result;
use app::{App, Screen};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use i18n::{t, Lang};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame, Terminal,
};
use std::io::{self, Stdout};

fn main() -> Result<()> {
    // The installed copy of this binary is dropped in as `artix-rollback`; when
    // invoked under that name (or with --rollback) it runs the snapshot rollback
    // TUI instead of the install wizard. Same binary, same look — no second
    // build, no embedding.
    let arg0 = std::env::args().next().unwrap_or_default();
    let as_rollback = std::path::Path::new(&arg0)
        .file_name()
        .map(|n| n.to_string_lossy().contains("rollback"))
        .unwrap_or(false);
    if as_rollback || std::env::args().any(|a| a == "--rollback" || a == "--rollback-initramfs") {
        return rollback::run();
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        default_hook(info);
    }));

    let mut terminal = setup_terminal()?;
    let mut app = App::new();
    let res = run(&mut terminal, &mut app);
    restore_terminal()?;
    res
}

fn setup_terminal() -> Result<Terminal<ratatui::backend::CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal() -> Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

fn run(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        // If an interactive step is queued, suspend the TUI, run it on the real
        // terminal so the user can answer prompts (e.g. pacman provider
        // selection), then restore the TUI and advance the install step.
        if let Some((program, args)) = app.pending_interactive.take() {
            run_interactive_step(terminal, app, &program, &args)?;
            if app.should_quit {
                return Ok(());
            }
            continue;
        }
        app.frame = app.frame.wrapping_add(1);
        terminal.draw(|f| draw(f, app))?;
        event::handle(app)?;
        if app.should_quit {
            return Ok(());
        }
    }
}

/// Guidance shown on the bare terminal before the recovery chroot shell opens,
/// and after it exits. Bilingual, hardcoded (like the rollback tool) so it needs
/// no i18n keys. Plain ASCII so it renders cleanly on any console.
const REC_GUIDE_UK: &str = "
============================================================
  ВІДНОВЛЕННЯ — ви root у вашій встановленій системі
============================================================
  Корінь встановленої системи примонтовано як /, ви — root.
  Лагодьте що треба: правте конфіги, перевстановлюйте пакунки,
  запускайте grub-mkconfig, виправляйте fstab тощо.

  Коли закінчите — наберіть  exit  (або Ctrl-D).
  Тоді інструмент САМ відмонтує встановлену систему й
  перезавантажить комп'ютер (буде кілька секунд, щоб скасувати).

  НЕ запускайте тут  reboot  чи  shutdown  — це chroot, воно
  не спрацює. Просто вийдіть через  exit.
============================================================

";
const REC_GUIDE_EN: &str = "
============================================================
  RECOVERY — you are root in your installed system
============================================================
  Your installed system's root is mounted as /, you are root.
  Repair what you need: edit configs, reinstall packages, run
  grub-mkconfig, fix fstab, and so on.

  When you're done, type  exit  (or Ctrl-D).
  The tool will THEN unmount the installed system and reboot
  the computer automatically (a few seconds to cancel first).

  Do NOT run  reboot  or  shutdown  here — this is a chroot,
  it won't work. Just leave with  exit.
============================================================

";

/// Suspend the TUI, run a command with inherited stdio (real terminal), then
/// re-enter the TUI and tell the installer the step finished. Keeps the screen
/// clean — no ratatui drawing happens while the child owns the terminal.
fn run_interactive_step(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    app: &mut App,
    program: &str,
    args: &[String],
) -> Result<()> {
    use std::io::Write;
    use std::process::Command;

    // Leave raw mode / alternate screen so the child draws to a normal terminal.
    restore_terminal()?;
    let recovery = matches!(
        app.screen,
        crate::app::Screen::Recovery | crate::app::Screen::WifiTest
    );
    let finish = matches!(app.screen, crate::app::Screen::Finish);
    // Both the recovery tool and the end-of-install "enter the system" option
    // drop into a chroot on the installed system; on exit, both unmount it
    // cleanly and reboot (with a cancel window).
    let post_install = recovery || finish;
    let uk = matches!(app.lang, crate::i18n::Lang::Uk);

    {
        use crossterm::terminal::{Clear, ClearType};
        let mut out = io::stdout();
        // Clear the screen and move to the top-left so the child's output starts
        // on a clean terminal instead of overlapping leftover TUI / scrollback.
        let _ = execute!(out, Clear(ClearType::All), crossterm::cursor::MoveTo(0, 0));
        if recovery {
            // Tell the user exactly what they're in, how to leave, and that
            // rebooting from inside a chroot won't work.
            let _ = write!(out, "{}", if uk { REC_GUIDE_UK } else { REC_GUIDE_EN });
        } else if finish {
            // Same idea, but they're the freshly-created user doing final setup.
            let who = if app.config.username.trim().is_empty() {
                "root".to_string()
            } else {
                app.config.username.clone()
            };
            let g = if uk {
                format!(
                    "
============================================================
  ВСТАНОВЛЕНО — ви {who} у вашій новій системі
============================================================
  Ви всередині встановленої системи (chroot) від імені свого
  користувача. Зробіть фінальні кроки: застосуйте свої
  налаштування, запускайте скрипти, ставте пакунки
  (sudo pacman -S ...). Мережа вже працює.

  Коли закінчите — наберіть  exit  (або Ctrl-D). Тоді інсталятор
  САМ безпечно відмонтує розділи (umount -R /mnt, закриє LUKS)
  й перезавантажить комп'ютер — буде кілька секунд, щоб скасувати.

  НЕ запускайте тут  reboot  чи  shutdown  — це chroot, не
  спрацює. Просто вийдіть через  exit.
============================================================

"
                )
            } else {
                format!(
                    "
============================================================
  INSTALLED — you are {who} in your new system
============================================================
  You are inside the installed system (chroot) as your user.
  Do any final steps: apply your configs, run scripts, install
  packages (sudo pacman -S ...). Networking already works.

  When you're done, type  exit  (or Ctrl-D). The tool will THEN
  unmount the partitions safely (umount -R /mnt, close LUKS) and
  reboot the computer — a few seconds to cancel first.

  Do NOT run  reboot  or  shutdown  here — this is a chroot, it
  won't work. Just leave with  exit.
============================================================

"
                )
            };
            let _ = write!(out, "{g}");
        } else {
            let _ = writeln!(out, ">> {} {}\n", program, args.join(" "));
        }
        let _ = out.flush();
    }

    let status = Command::new(program).args(args).status();

    if post_install {
        // The user left the chroot. Unmount the installed system thoroughly —
        // cleanup() does `umount -R /mnt` (every nested + chroot bind mount) and
        // closes the LUKS mappers: the correct, scoped equivalent of `umount -a`
        // (a literal `umount -a` here would also tear down the LIVE system's own
        // mounts). Then reboot automatically, with a short window to cancel in
        // case the shell was left by accident (e.g. a stray Ctrl-D).
        {
            let mut out = io::stdout();
            let _ = writeln!(
                out,
                "\n>> {}",
                t(app.lang, "ui.leaving_unmounting_the_installed_system")
            );
            let _ = out.flush();
        }
        crate::system::recovery::cleanup();
        app.recovery_mounted = false;
        app.recovery_status.clear();
        // Where to land if the reboot is cancelled below.
        app.screen = if recovery {
            crate::app::Screen::Mode
        } else {
            crate::app::Screen::Finish
        };

        // Countdown with an abort key (raw mode so a single keypress is enough).
        let _ = enable_raw_mode();
        let mut aborted = false;
        for n in (1..=5).rev() {
            {
                let mut out = io::stdout();
                let _ = write!(
                    out,
                    "\r{}",
                    t(app.lang, "ui.reboot_countdown").replace("{n}", &n.to_string())
                );
                let _ = out.flush();
            }
            if crossterm::event::poll(std::time::Duration::from_secs(1)).unwrap_or(false) {
                if let Ok(crossterm::event::Event::Key(_)) = crossterm::event::read() {
                    aborted = true;
                    break;
                }
            }
        }
        let _ = disable_raw_mode();

        let mut out = io::stdout();
        let _ = writeln!(out);
        if aborted {
            let _ = writeln!(
                out,
                ">> {}",
                t(app.lang, "ui.cancelled_returning_to_the_menu")
            );
            let _ = out.flush();
            // Back to the TUI (mode chooser).
            enable_raw_mode()?;
            execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
            terminal.clear()?;
            return Ok(());
        }
        let _ = writeln!(out, ">> {}", t(app.lang, "ui.rebooting"));
        let _ = out.flush();
        let _ = Command::new("reboot").status();
        app.should_quit = true;
        return Ok(());
    }

    // Non-recovery interactive step (an install step that needs the real
    // terminal): pause so the user can read the result, then resume the TUI.
    {
        let mut out = io::stdout();
        let _ = writeln!(out, "\n>> done — press Enter to continue");
        let _ = out.flush();
        let mut buf = String::new();
        let _ = std::io::stdin().read_line(&mut buf);
    }

    // Re-enter the TUI.
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    terminal.clear()?;

    // Report result into the installer state, mirroring the streamed path.
    let ok = matches!(status, Ok(ref s) if s.success());
    if ok {
        app.install_step += 1;
        app.install_rx = None;
        crate::screens::summary::resume_after_interactive(app);
    } else {
        let msg = match status {
            Ok(s) => format!(
                "{} exited with {}",
                program,
                s.code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into())
            ),
            Err(e) => format!("{program}: {e}"),
        };
        crate::screens::summary::fail_after_interactive(app, msg);
    }
    Ok(())
}

/// Minimum usable terminal size. The 12-col sidebar + content + footer layout
/// becomes an unreadable, overlapping mess below this (and some framebuffer
/// consoles show artifacts), so we show an "enlarge me" notice instead.
const MIN_COLS: u16 = 80;
const MIN_ROWS: u16 = 24;

fn draw(f: &mut Frame, app: &mut App) {
    // Paint the whole background.
    f.render_widget(
        Block::default().style(Style::default().bg(theme::BG)),
        f.area(),
    );

    // Below a usable size, don't even try to lay out the wizard — show a clear
    // notice until the window is enlarged (or the console font shrinks).
    let area = f.area();
    if area.width < MIN_COLS || area.height < MIN_ROWS {
        draw_too_small(f, area);
        return;
    }

    // Outer margin for breathing room.
    let root = f.area().inner(Margin {
        horizontal: 1,
        vertical: 1,
    });

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(12), Constraint::Min(0)])
        .spacing(1)
        .split(root);

    draw_sidebar(f, app, cols[0]);

    // Right side: content panel + footer.
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(cols[1]);

    draw_content(f, app, right[0]);
    draw_footer(f, app, right[1]);
}

/// Notice shown when the terminal is below the usable minimum. Bilingual and
/// dependency-free (the language may not be chosen yet, and on a tiny screen
/// showing both is friendlier). Vertically centered; the background is already
/// painted by the caller.
/// Deliberately shows BOTH languages at once.
///
/// This is the one screen that can appear before the user has chosen a
/// language — and it appears precisely when the terminal is too small to render
/// the language picker. Guessing wrong here would leave someone staring at a
/// message they can't read, with no way to reach the screen that would let them
/// change it. So it doesn't guess: both lines are drawn, every time. The strings
/// still live in the TOMLs, so a third language means editing translations, not
/// this function.
fn draw_too_small(f: &mut Frame, area: Rect) {
    let pad = area.height.saturating_sub(6) / 2;
    let mut lines: Vec<Line> = (0..pad).map(|_| Line::from("")).collect();
    lines.push(Line::from(Span::styled(
        format!(
            "⚠  {} · {}",
            t(Lang::Uk, "ui.too_small"),
            t(Lang::En, "ui.too_small")
        ),
        theme::warn(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "{}×{}  →  {} {}×{}",
            area.width,
            area.height,
            t(Lang::Uk, "ui.need_at_least"),
            MIN_COLS,
            MIN_ROWS
        ),
        theme::dim(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(t(Lang::Uk, "ui.enlarge_window")));
    lines.push(Line::from(t(Lang::En, "ui.enlarge_window")));
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
}

/// Left rail: branding + the wizard steps with done/active/pending glyphs.
fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border())
        .style(Style::default().bg(theme::PANEL));
    let inner = block.inner(area).inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    f.render_widget(block, area);

    // Mode/Recovery live outside the numbered flow. Use an out-of-range
    // "current" so NONE of the install steps render as done/active — they all
    // show as pending, which reads correctly (no install step is in progress).
    let current = match app.screen {
        Screen::Mode | Screen::Recovery | Screen::WifiTest => usize::MAX,
        s => s as usize,
    };
    // Brand. The rail is narrow (12 cols), so keep this short. "ARTIX" centered,
    // with a small mark above it. Plain ASCII/geometric chars only, so it
    // renders on a bare console font too.
    let mut lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(Span::styled("  ARTIX", theme::title())),
        Line::from(Span::styled("  ─────", theme::dim())),
        Line::from(""),
    ];

    let nums = [
        "01", "02", "03", "04", "05", "06", "07", "08", "09", "10", "11", "12", "13", "14", "15",
    ];

    // Spinner for the ACTIVE step. The bare Linux console font often lacks
    // fancy glyphs (diamonds, braille), which then show up as "*" or "?", so we
    // use the classic ASCII spinner |/-\ — it renders correctly in every
    // terminal and console font and reads clearly as "in progress".
    let spin = ['|', '/', '-', '\\'];
    let spin_ch = spin[(app.frame / 2) as usize % spin.len()];

    for (i, num) in nums.iter().enumerate() {
        let (glyph, style) = if i < current {
            ("●".to_string(), theme::step_done())
        } else if i == current {
            (spin_ch.to_string(), theme::step_active())
        } else {
            ("○".to_string(), theme::step_pending())
        };
        // Numbers only — no text labels. Keeps the rail narrow so the content
        // panel gets the space; the full step title lives in the panel header.
        lines.push(Line::from(vec![
            Span::styled(format!("  {glyph} "), style),
            Span::styled((*num).to_string(), style),
        ]));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

/// Right content: a titled rounded panel; the active screen draws inside.
fn draw_content(f: &mut Frame, app: &mut App, area: Rect) {
    let title = screen_title(app);
    let block = theme::panel(&title);
    let inner = block.inner(area).inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    f.render_widget(block, area);
    screens::draw(f, app, inner);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border_dim());
    let inner = block.inner(area).inner(Margin {
        horizontal: 2,
        vertical: 0,
    });
    f.render_widget(block, area);

    let hint = screens::footer_hint(app).unwrap_or_else(|| t(app.lang, "nav.hint"));

    // Each hint is a list of "KEY action" pairs joined by " · ". We render the
    // key part (everything up to the first run of spaces in a segment) in the
    // accent color and the action in dim, with a clearer separator between
    // pairs, so the eye can tell keys from their descriptions at a glance.
    let mut spans: Vec<Span> = Vec::new();
    for (i, seg) in hint.split('·').map(|s| s.trim()).enumerate() {
        if seg.is_empty() {
            continue;
        }
        if i > 0 {
            spans.push(Span::styled("   |   ", theme::border_dim()));
        }
        // Split a segment into key (first token group) and the rest (action).
        match seg.split_once(char::is_whitespace) {
            Some((key, rest)) => {
                spans.push(Span::styled(key.to_string(), theme::accent()));
                spans.push(Span::styled(format!(" {}", rest.trim()), theme::dim()));
            }
            None => spans.push(Span::styled(seg.to_string(), theme::accent())),
        }
    }

    f.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Left),
        inner,
    );
}

fn screen_title(app: &App) -> String {
    // Mode and Recovery are outside the numbered install flow — they get a
    // plain title with no "N / 13" step counter.
    match app.screen {
        Screen::Mode => return format!(" {} ", t(app.lang, "mode.title")),
        Screen::Recovery => return format!(" {} ", t(app.lang, "rec.title")),
        Screen::WifiTest => return format!(" {} ", t(app.lang, "wt.title")),
        _ => {}
    }
    let key = match app.screen {
        Screen::Language => "lang.title",
        Screen::Timezone => "tz.title",
        Screen::Wifi => "wifi.title",
        Screen::Keyboard => "kb.title",
        Screen::Kernel => "kern.title",
        Screen::Desktop => "de.title",
        Screen::Packages => "pkg.title",
        Screen::Aur => "aur.title",
        Screen::Disk => "disk.title",
        Screen::Security => "sec.title",
        Screen::Storage => "storage.title",
        Screen::User => "user.title",
        Screen::Options => "opt.title",
        Screen::Summary => "sum.title",
        Screen::Finish => "fin.title",
        // Handled above with an early return.
        Screen::Mode | Screen::Recovery | Screen::WifiTest => unreachable!(),
    };
    format!(
        " {} / 15  ·  {} ",
        app.screen.step_number(),
        t(app.lang, key)
    )
}

#[cfg(test)]
mod tests {
    /// No user-facing string may be hardcoded in Rust.
    ///
    /// The project has a full i18n layer — and it used to have a SECOND,
    /// parallel one living in the source: `if uk { "…" } else { "…" }`, 45 of
    /// them. Two mechanisms for one job. A third language would have meant
    /// editing the TOMLs *and* hunting every `if uk` in the code; the CI parity
    /// check only ever saw the TOMLs, so the two could drift apart silently.
    ///
    /// This test is the guard that keeps the debt from growing back. It scans
    /// the source for Cyrillic text outside the translation files — a
    /// hardcoded Ukrainian string is the fingerprint of a translation that
    /// skipped the i18n layer.
    ///
    /// The exceptions are deliberate and few:
    ///   * `system/install/scripts.rs` — shell scripts that run on the INSTALLED
    ///     system, outside this binary. They carry their own `msg "en" "uk"`
    ///     helper; Rust's i18n isn't reachable from a POSIX shell.
    ///   * comments and doc comments — those are for whoever reads the code.
    #[test]
    fn no_ui_string_is_hardcoded_outside_the_translation_files() {
        fn cyrillic_run(line: &str) -> bool {
            // Four Cyrillic letters in a row is a WORD. Fewer than that catches
            // box-drawing characters and arrows, which are not translations.
            let mut run = 0;
            for c in line.chars() {
                if ('\u{0400}'..='\u{04FF}').contains(&c) {
                    run += 1;
                    if run >= 4 {
                        return true;
                    }
                } else {
                    run = 0;
                }
            }
            false
        }

        let mut offenders = Vec::new();
        let mut stack = vec![std::path::PathBuf::from("src")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let p = path.to_string_lossy().replace('\\', "/");
                // The shell scripts translate themselves — see above.
                if p.ends_with("system/install/scripts.rs") {
                    continue;
                }
                let Ok(src) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for (n, line) in src.lines().enumerate() {
                    let trimmed = line.trim_start();
                    // Comments are for the reader of the code, not the user.
                    if trimmed.starts_with("//") {
                        continue;
                    }
                    // Only string literals matter.
                    if !line.contains('"') {
                        continue;
                    }
                    if cyrillic_run(line) {
                        offenders.push(format!("{p}:{}: {}", n + 1, trimmed.trim()));
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "hardcoded UI strings — these belong in i18n/*.toml, reached with \
             t(lang, \"key\"):\n  {}",
            offenders.join("\n  ")
        );
    }
}
