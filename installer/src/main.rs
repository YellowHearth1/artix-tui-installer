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

#![allow(dead_code)]

mod app;
mod event;
mod i18n;
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
use i18n::t;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame, Terminal,
};
use std::io::{self, Stdout};

fn main() -> Result<()> {
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
    {
        use crossterm::terminal::{Clear, ClearType};
        let mut out = io::stdout();
        // Clear the screen and move to the top-left so pacman's output starts on
        // a clean terminal instead of overlapping leftover TUI / scrollback.
        let _ = execute!(out, Clear(ClearType::All), crossterm::cursor::MoveTo(0, 0));
        let _ = writeln!(out, ">> {} {}\n", program, args.join(" "));
        let _ = out.flush();
    }

    let status = Command::new(program).args(args).status();

    // Pause so the user can read the final result before the TUI snaps back.
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
    if app.screen == crate::app::Screen::Recovery {
        // The interactive step was the recovery chroot shell. The user has
        // exited it; unmount everything we mounted and go back to the mode
        // chooser. (We don't touch install state here.)
        crate::system::recovery::cleanup();
        app.recovery_mounted = false;
        app.recovery_status.clear();
        app.screen = crate::app::Screen::Mode;
    } else if ok {
        app.install_step += 1;
        app.install_rx = None;
        crate::screens::summary::resume_after_interactive(app);
    } else {
        let msg = match status {
            Ok(s) => format!(
                "{} exited with {}",
                program,
                s.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into())
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
    let root = f.area().inner(Margin { horizontal: 1, vertical: 1 });

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
fn draw_too_small(f: &mut Frame, area: Rect) {
    let pad = area.height.saturating_sub(6) / 2;
    let mut lines: Vec<Line> = (0..pad).map(|_| Line::from("")).collect();
    lines.push(Line::from(Span::styled(
        "⚠  Замале вікно · Terminal too small",
        theme::warn(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "{}×{}  →  потрібно ≥ {}×{}",
            area.width, area.height, MIN_COLS, MIN_ROWS
        ),
        theme::dim(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from("Збільшіть вікно або зменшіть шрифт"));
    lines.push(Line::from("Enlarge the window or reduce the font size"));
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
}

/// Left rail: branding + the wizard steps with done/active/pending glyphs.
fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border())
        .style(Style::default().bg(theme::PANEL));
    let inner = block.inner(area).inner(Margin { horizontal: 1, vertical: 1 });
    f.render_widget(block, area);

    // Mode/Recovery live outside the numbered flow. Use an out-of-range
    // "current" so NONE of the install steps render as done/active — they all
    // show as pending, which reads correctly (no install step is in progress).
    let current = match app.screen {
        Screen::Mode | Screen::Recovery => usize::MAX,
        s => s as usize,
    };
    let mut lines: Vec<Line> = Vec::new();

    // Brand. The rail is narrow (12 cols), so keep this short. "ARTIX" centered,
    // with a small mark above it. Plain ASCII/geometric chars only, so it
    // renders on a bare console font too.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  ARTIX", theme::title())));
    lines.push(Line::from(Span::styled("  ─────", theme::dim())));
    lines.push(Line::from(""));

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
    let inner = block.inner(area).inner(Margin { horizontal: 2, vertical: 1 });
    f.render_widget(block, area);
    screens::draw(f, app, inner);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border_dim());
    let inner = block.inner(area).inner(Margin { horizontal: 2, vertical: 0 });
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
        Screen::Mode | Screen::Recovery => unreachable!(),
    };
    format!(" {} / 15  ·  {} ", app.screen.step_number(), t(app.lang, key))
}
