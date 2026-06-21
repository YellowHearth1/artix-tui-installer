//! Step 7 — packages. Two sections, switched with ←/→:
//!   • GPU driver bundle (single-choice, pinned at top)
//!   • Live package search (type to search the repos via `pacman -Ss`, Space to
//!     toggle a result, building a multi-selected set)
//!
//! Search runs on a background thread with a short debounce so typing stays
//! responsive and the UI never blocks on the network. Selected packages persist
//! in `config.extra_packages` even when they scroll out of the current results.

use crate::app::{App, GpuDriver};
use crate::i18n::t;
use crate::screens::widgets;
use crate::system::packages::{self, Pkg};
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, BorderType, List, ListItem, ListState, Paragraph},
    Frame,
};

const GPUS: [GpuDriver; 6] = [
    GpuDriver::None,
    GpuDriver::Nvidia,
    GpuDriver::Nvidia580xx,
    GpuDriver::Nouveau,
    GpuDriver::Amd,
    GpuDriver::Intel,
];

/// Is driver `g` in the comma-separated selection string?
fn gpu_selected(sel: &str, g: GpuDriver) -> bool {
    let name = format!("{:?}", g);
    sel.split(',').any(|s| s.trim() == name)
}

/// Toggle driver `g` in the comma-separated selection. Sane exclusions:
///  • "None" clears everything (and picking any real driver clears "None").
///  • The three NVIDIA stacks (proprietary open-dkms, legacy 580xx, nouveau)
///    target the same hardware with conflicting kernel modules, so picking one
///    removes the other two.
///  • Intel and AMD freely combine with each other and with one NVIDIA stack —
///    that's the hybrid-graphics case (NVIDIA dGPU + Intel/AMD iGPU).
fn toggle_gpu(sel: &mut String, g: GpuDriver) {
    let name = format!("{:?}", g);
    let mut parts: Vec<String> = sel
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if let Some(pos) = parts.iter().position(|p| *p == name) {
        // Already selected → unselect. An empty selection falls back to None.
        parts.remove(pos);
        if parts.is_empty() {
            parts.push("None".into());
        }
    } else {
        if name == "None" {
            // None is exclusive: clears all drivers.
            parts.clear();
            parts.push("None".into());
        } else {
            // A real driver replaces None and conflicts within its family.
            parts.retain(|p| p != "None");
            let nvidia_family = ["Nvidia", "Nvidia580xx", "Nouveau"];
            if nvidia_family.contains(&name.as_str()) {
                parts.retain(|p| !nvidia_family.contains(&p.as_str()));
            }
            parts.push(name);
        }
    }
    *sel = parts.join(",");
}

fn gpu_label(g: GpuDriver) -> &'static str {
    match g {
        GpuDriver::None => "None / generic (mesa default)",
        GpuDriver::Nvidia => "NVIDIA (open-dkms)",
        GpuDriver::Nvidia580xx => "NVIDIA 580xx (legacy)",
        GpuDriver::Nouveau => "NVIDIA nouveau (open-source)",
        GpuDriver::Amd => "AMD (amdgpu / radeon)",
        GpuDriver::Intel => "Intel",
    }
}

pub const FOCUS_GPU: usize = 0;
pub const FOCUS_PKG: usize = 1;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8), // gpu
            Constraint::Length(3), // search box
            Constraint::Min(0),    // results
            Constraint::Length(2), // selected summary
            Constraint::Length(3), // actions
        ])
        .spacing(1)
        .split(area);

    // ── GPU bundles ──
    let gpu_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if app.pkg_focus == FOCUS_GPU { theme::border() } else { theme::border_dim() })
        .title(format!(" {} ", t(app.lang, "pkg.gpu")))
        .title_style(theme::title());
    let gpu_inner = gpu_block.inner(rows[0]);
    f.render_widget(gpu_block, rows[0]);
    let gpu_items: Vec<String> = GPUS
        .iter()
        .map(|g| {
            let sel = gpu_selected(&app.config.gpu, *g);
            format!("{} {}", if sel { "[✓]" } else { "[ ]" }, gpu_label(*g))
        })
        .collect();
    widgets::select_list(f, gpu_inner, &gpu_items, app.gpu_cursor);

    // ── search box ──
    let title = if app.pkg_searching {
        format!(" {} · {} ", t(app.lang, "app.search"), t(app.lang, "pkg.searching"))
    } else {
        format!(" {} ", t(app.lang, "app.search"))
    };
    let search = Paragraph::new(Line::from(vec![
        Span::styled("  ", theme::dim()),
        Span::styled(
            if app.pkg_query.is_empty() { t(app.lang, "pkg.search") } else { app.pkg_query.clone() },
            if app.pkg_query.is_empty() { theme::mute() } else { theme::normal() },
        ),
        Span::styled(if app.pkg_focus == FOCUS_PKG { "▏" } else { "" }, theme::accent()),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if app.pkg_focus == FOCUS_PKG { theme::border() } else { theme::border_dim() })
            .title(title)
            .title_style(theme::dim()),
    );
    f.render_widget(search, rows[1]);

    // ── results / messages ──
    draw_results(f, app, rows[2]);

    // ── selected summary ──
    let sel = &app.config.extra_packages;
    let sel_text = if sel.is_empty() {
        t(app.lang, "pkg.none_selected")
    } else {
        format!("{}: {}", t(app.lang, "pkg.selected"), sel.join(", "))
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!("  {sel_text}"), theme::gold())))
            .wrap(ratatui::widgets::Wrap { trim: true }),
        rows[3],
    );

    app.can_advance = true;
    widgets::action_row(f, rows[4], &t(app.lang, "app.back"), &t(app.lang, "app.next"), true);
}

fn draw_results(f: &mut Frame, app: &App, area: Rect) {
    let typing = !app.pkg_query.trim().is_empty();

    if let Some(err) = &app.pkg_error {
        if typing {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(format!("  {err}"), theme::warn()))),
                area,
            );
            return;
        }
    }

    let rows_source: &Vec<Pkg> = if typing { &app.pkg_results } else { &app.pkg_popular };

    if typing && rows_source.is_empty() {
        let msg = if app.pkg_searching { t(app.lang, "pkg.searching") } else { t(app.lang, "pkg.no_results") };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!("  {msg}"), theme::mute()))),
            area,
        );
        return;
    }

    let header = if typing { t(app.lang, "pkg.results") } else { t(app.lang, "pkg.popular") };

    let inner = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!("  {header}"), theme::title()))),
        inner[0],
    );

    let rows: Vec<ListItem> = rows_source
        .iter()
        .map(|p: &Pkg| {
            let checked = app.config.extra_packages.contains(&p.name);
            let (mark, mark_style, name_style) = if checked {
                ("[✓] ", theme::ok(), theme::gold())
            } else {
                ("[ ] ", theme::mute(), theme::normal())
            };
            ListItem::new(Line::from(vec![
                Span::styled(mark, mark_style),
                Span::styled(p.name.clone(), name_style),
                Span::styled(
                    if p.desc.is_empty() { String::new() } else { format!("  — {}", p.desc) },
                    theme::dim(),
                ),
            ]))
        })
        .collect();
    let list = List::new(rows)
        .highlight_style(theme::selected())
        .highlight_symbol("▎ ");
    let mut st = ListState::default();
    if !rows_source.is_empty() {
        let sel = app.cursor.min(rows_source.len() - 1);
        // Keep the highlighted row vertically centered in the viewport (clamped
        // at the list ends), so the eye stays at mid-screen instead of having to
        // track the cursor down toward the bottom edge.
        let view_h = inner[1].height as usize;
        let half = view_h / 2;
        let max_off = rows_source.len().saturating_sub(view_h);
        let off = sel.saturating_sub(half).min(max_off);
        *st.offset_mut() = off;
        st.select(Some(sel));
    }
    f.render_stateful_widget(list, inner[1], &mut st);
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    // Esc steps backward WITHIN the screen, mirroring the forward flow:
    //   • package search with a query  → clear the query (back to popular list)
    //   • package search, empty query  → move focus up to the GPU section
    //   • GPU section                  → leave to the previous screen (this
    //     last case is routed by the global handler; see screen_can_step_back).
    if key.code == KeyCode::Esc {
        if app.pkg_focus == FOCUS_PKG {
            if !app.pkg_query.trim().is_empty() {
                app.pkg_query.clear();
                app.cursor = 0;
                app.pkg_results.clear();
            } else {
                app.pkg_focus = FOCUS_GPU;
            }
        }
        return;
    }
    // ←/→ switch between GPU section and package search.
    match key.code {
        KeyCode::Left if app.pkg_focus == FOCUS_PKG => {
            app.pkg_focus = FOCUS_GPU;
            return;
        }
        // On the leftmost (GPU) section, Left has nowhere to go → ignore it
        // (back is Esc only).
        KeyCode::Left if app.pkg_focus == FOCUS_GPU => {
            return;
        }
        KeyCode::Right if app.pkg_focus == FOCUS_GPU => {
            app.pkg_focus = FOCUS_PKG;
            return;
        }
        _ => {}
    }

    if app.pkg_focus == FOCUS_GPU {
        match key.code {
            KeyCode::Up => app.gpu_cursor = app.gpu_cursor.saturating_sub(1),
            KeyCode::Down => app.gpu_cursor = (app.gpu_cursor + 1).min(GPUS.len() - 1),
            // Space toggles the highlighted driver. Multiple drivers can be
            // combined (hybrid graphics: NVIDIA dGPU + Intel/AMD iGPU), with
            // sane exclusions handled in toggle_gpu.
            KeyCode::Char(' ') => {
                let g = GPUS[app.gpu_cursor.min(GPUS.len() - 1)];
                toggle_gpu(&mut app.config.gpu, g);
            }
            // Enter does NOT toggle (that's Space's job — pressing Enter on an
            // already-checked driver must not silently uncheck it). It just
            // moves focus down to the package search, so the user flows toward
            // the next step; Enter there advances to the next screen.
            KeyCode::Enter => {
                app.pkg_focus = FOCUS_PKG;
            }
            _ => {}
        }
        return;
    }

    // Package search focus. The active list is the search results while typing,
    // otherwise the curated popular list.
    let typing = !app.pkg_query.trim().is_empty();
    let active_len = if typing { app.pkg_results.len() } else { app.pkg_popular.len() };
    match key.code {
        KeyCode::Up => app.cursor = app.cursor.saturating_sub(1),
        KeyCode::Down => app.cursor = (app.cursor + 1).min(active_len.saturating_sub(1)),
        KeyCode::Char(' ') => {
            let picked = if typing {
                app.pkg_results.get(app.cursor).map(|p| p.name.clone())
            } else {
                app.pkg_popular.get(app.cursor).map(|p| p.name.clone())
            };
            if let Some(name) = picked {
                if let Some(pos) = app.config.extra_packages.iter().position(|x| *x == name) {
                    app.config.extra_packages.remove(pos);
                } else {
                    // zsh and fish are mutually exclusive: only one of them can
                    // become the login shell, and installing both makes a mess
                    // of the shell config. Picking one silently unpicks the
                    // other.
                    if name == "zsh" {
                        app.config.extra_packages.retain(|x| x != "fish");
                    } else if name == "fish" {
                        app.config.extra_packages.retain(|x| x != "zsh");
                    }
                    app.config.extra_packages.push(name);
                }
            }
        }
        KeyCode::Enter => app.goto_next(),
        KeyCode::Char(c) => {
            app.pkg_query.push(c);
            app.cursor = 0;
            app.pkg_debounce = 4; // ~400ms at 100ms tick
        }
        KeyCode::Backspace => {
            app.pkg_query.pop();
            app.cursor = 0;
            app.pkg_debounce = 4;
        }
        _ => {}
    }
}

/// Background work: debounce → launch search thread → drain results.
pub fn tick(app: &mut App) {
    // Drain a finished repo search first.
    if let Some(rx) = &app.pkg_rx {
        if let Ok(res) = rx.try_recv() {
            app.pkg_searching = false;
            app.pkg_rx = None;
            match res {
                Ok(list) => {
                    app.pkg_results = list;
                    app.pkg_error = None;
                    app.cursor = 0;
                }
                Err(e) => {
                    app.pkg_results.clear();
                    app.pkg_error = Some(e);
                }
            }
        }
    }

    // Debounce countdown for the repo search.
    if app.pkg_debounce > 0 {
        app.pkg_debounce -= 1;
        if app.pkg_debounce == 0 {
            let q = app.pkg_query.trim().to_string();
            if q.is_empty() {
                app.pkg_results.clear();
                app.pkg_error = None;
            } else if q != app.pkg_inflight_query || app.pkg_rx.is_none() {
                launch_search(app, q);
            }
        }
    }

}


fn launch_search(app: &mut App, query: String) {
    let (tx, rx) = crossbeam_channel::bounded(1);
    app.pkg_inflight_query = query.clone();
    app.pkg_searching = true;
    app.pkg_rx = Some(rx);
    std::thread::spawn(move || {
        let result = packages::search(&query);
        let _ = tx.send(result);
    });
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "pkg.footer")
}


