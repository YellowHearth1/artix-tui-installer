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
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph},
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
fn gpu_selected(sel: &[GpuDriver], g: GpuDriver) -> bool {
    sel.contains(&g)
}

/// Toggle driver `g` in the comma-separated selection. Sane exclusions:
///  • "None" clears everything (and picking any real driver clears "None").
///  • The three NVIDIA stacks (proprietary open-dkms, legacy 580xx, nouveau)
///    target the same hardware with conflicting kernel modules, so picking one
///    removes the other two.
///  • Intel and AMD freely combine with each other and with one NVIDIA stack —
///    that's the hybrid-graphics case (NVIDIA dGPU + Intel/AMD iGPU).
fn toggle_gpu(sel: &mut Vec<GpuDriver>, g: GpuDriver) {
    if let Some(pos) = sel.iter().position(|x| *x == g) {
        // Already selected → unselect. An empty selection falls back to None.
        sel.remove(pos);
        if sel.is_empty() {
            sel.push(GpuDriver::None);
        }
        return;
    }
    if g == GpuDriver::None {
        // None is exclusive: clears all drivers.
        sel.clear();
        sel.push(GpuDriver::None);
        return;
    }
    // A real driver replaces None and conflicts within its own family.
    sel.retain(|x| *x != GpuDriver::None);
    if g.is_nvidia_family() {
        sel.retain(|x| !x.is_nvidia_family());
    }
    sel.push(g);
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
        .border_style(if app.pkg_focus == FOCUS_GPU {
            theme::border()
        } else {
            theme::border_dim()
        })
        .title(format!(" {} ", t(app.lang, "pkg.gpu")))
        .title_style(theme::title());
    let gpu_inner = gpu_block.inner(rows[0]);
    f.render_widget(gpu_block, rows[0]);
    let gpu_items: Vec<String> = GPUS
        .iter()
        .map(|g| {
            let sel = gpu_selected(&app.config.gpu, *g);
            format!("{} {}", if sel { "[x]" } else { "[ ]" }, gpu_label(*g))
        })
        .collect();
    widgets::select_list_scrolled(f, gpu_inner, &gpu_items, app.gpu_cursor, app.marquee);

    // ── search box ──
    let title = if app.pkg_searching {
        format!(
            " {} · {} ",
            t(app.lang, "app.search"),
            t(app.lang, "pkg.searching")
        )
    } else {
        format!(" {} ", t(app.lang, "app.search"))
    };
    let search = Paragraph::new(Line::from(vec![
        Span::styled("  ", theme::dim()),
        Span::styled(
            if app.pkg_query.is_empty() {
                t(app.lang, "pkg.search")
            } else {
                app.pkg_query.clone()
            },
            if app.pkg_query.is_empty() {
                theme::mute()
            } else {
                theme::normal()
            },
        ),
        Span::styled(
            if app.pkg_focus == FOCUS_PKG { "|" } else { "" },
            theme::accent(),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if app.pkg_focus == FOCUS_PKG {
                theme::border()
            } else {
                theme::border_dim()
            })
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
        Paragraph::new(Line::from(Span::styled(
            format!("  {sel_text}"),
            theme::gold(),
        )))
        .wrap(ratatui::widgets::Wrap { trim: true }),
        rows[3],
    );

    app.can_advance = true;
    widgets::action_row(
        f,
        rows[4],
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        true,
    );

    // THE DIALOG GOES LAST, over the WHOLE screen — not last inside one of the
    // rows above it. It used to be drawn at the end of `draw_results`, which is
    // only the middle band, so the two paragraphs rendered afterwards painted
    // straight over it: the selected-package list appeared INSIDE the logo
    // picker, eating its left border and its choices. `Clear` cannot help with
    // that, because the overwriting happens after `Clear` has already run.
    if app.logo_modal_open {
        draw_logo_modal(f, app);
    }
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

    let rows_source: &Vec<Pkg> = if typing {
        &app.pkg_results
    } else {
        &app.pkg_popular
    };

    if typing && rows_source.is_empty() {
        let msg = if app.pkg_searching {
            t(app.lang, "pkg.searching")
        } else {
            t(app.lang, "pkg.no_results")
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!("  {msg}"), theme::mute()))),
            area,
        );
        return;
    }

    let header = if typing {
        t(app.lang, "pkg.results")
    } else {
        t(app.lang, "pkg.popular")
    };

    let inner = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("  {header}"),
            theme::title(),
        ))),
        inner[0],
    );

    let rows: Vec<ListItem> = rows_source
        .iter()
        .map(|p: &Pkg| {
            let checked = app.config.extra_packages.contains(&p.name);
            let (mark, mark_style, name_style) = if checked {
                ("[x] ", theme::ok(), theme::gold())
            } else {
                ("[ ] ", theme::mute(), theme::normal())
            };
            let mut spans = vec![Span::styled(mark, mark_style)];
            // slayfetch wears its own flag: the name is painted red→violet so the
            // entry says what it is before you read the description. Every other
            // package keeps the plain checked/unchecked styling.
            if p.name == "slayfetch" {
                spans.extend(rainbow_text(&p.name));
            } else {
                spans.push(Span::styled(p.name.clone(), name_style));
            }
            spans.push(Span::styled(
                if p.desc.is_empty() {
                    String::new()
                } else {
                    format!("  — {}", p.desc)
                },
                theme::dim(),
            ));
            // Show the chosen pride logo right on the selected slayfetch row, and
            // flag that Space (re)opens the picker.
            if p.name == "slayfetch" && checked && !app.config.fastfetch_logo.is_empty() {
                spans.push(Span::styled(
                    format!("  → {}", app.config.fastfetch_logo),
                    theme::title(),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    // Like the logo picker, the highlight sets NO foreground: a fg here would
    // repaint slayfetch's rainbow (and the gold "checked" names) accent-cyan on
    // whichever row the cursor sits on. bg + bold + the ▎ bar mark it instead,
    // and all three degrade gracefully on a bare VT.
    let list = List::new(rows)
        .highlight_style(
            ratatui::style::Style::default()
                .bg(theme::SEL_BG)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )
        .highlight_symbol("> ");
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

/// The slayfetch logo picker: a scrollable list of the embedded Artix + pride
/// logo variants, the chosen one highlighted. Names only — the actual images
/// ship in the README collage, so the user knows what each looks like.
fn draw_logo_modal(f: &mut Frame, app: &App) {
    use ratatui::widgets::Clear;
    let variants = crate::system::logos::variants();
    // SIZED FROM ITS OWN CONTENT, because at the largest console font this box
    // had four rows to work with and spent two of them on chrome: the user saw
    // one flag name and the word "Esc", and could not tell it was a list.
    //
    // What it asks for is the whole list plus its title and hint. What it gets
    // is clamped to the panel — and when the clamp bites, the ROWS are what
    // survives: the hint drops first (Esc closes every dialog in this
    // installer), so the shrinking box keeps showing choices for as long as it
    // has any room at all.
    let longest = variants
        .iter()
        .map(|v| v.chars().count())
        .max()
        .unwrap_or(0) as u16;
    // THE TITLE AND THE HINT ARE CONTENT TOO. Sized from the flag names alone,
    // the box was narrower than its own footer: the hint came out as
    // "…· Esc –" and the key that closes the dialog was the part cut off.
    let title_w = t(app.lang, "pkg.logo_title").chars().count() as u16 + 6;
    let hint_w = t(app.lang, "pkg.logo_hint").chars().count() as u16 + 4;
    let w = (longest + 8).max(title_w).max(hint_w).max(24);
    // Tall enough for the whole list, but never touching the panel's edge: at
    // full stretch the last row sat against the border with nothing under it.
    // Four rows held back leave a clear margin above and below, and the list
    // scrolls for whatever does not fit — which on a large console is nothing.
    // Measured against the DISPLAY, because that is what the box is centred in
    // now — the panel it was raised from is narrower and starts lower.
    let room = f.area().height.saturating_sub(4).max(6);
    let h = (variants.len() as u16 + 3).min(room);
    let rect = crate::screens::widgets::modal_rect_fit(f, w, h, app.modal_zoom);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border())
        .title(format!(" {} ", t(app.lang, "pkg.logo_title")))
        .title_style(theme::title());
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);

    // Each variant's NAME is painted in ITS flag's colours, distributed across
    // the letters top→bottom-stripe style. The currently-chosen one is marked
    // (✓, accent) so it's findable while browsing from the top.
    let chosen = &app.config.fastfetch_logo;
    let rows: Vec<ListItem> = variants
        .iter()
        .map(|v| {
            let mut spans = vec![Span::styled(
                if v == chosen { "x " } else { "  " }.to_string(),
                theme::accent(),
            )];
            spans.extend(flag_colored(v));
            ListItem::new(Line::from(spans))
        })
        .collect();
    // The highlight must NOT set a foreground, or it would repaint the flag
    // colours cyan on the selected row. A subtle bg + bold + the ▶ symbol mark
    // the cursor instead (the bg degrades to the ▶/bold on a bare VT).
    let list = List::new(rows)
        .highlight_style(
            ratatui::style::Style::default()
                .bg(theme::SEL_BG)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut st = ListState::default();
    if !variants.is_empty() {
        let sel = app.logo_cursor.min(variants.len() - 1);
        // The rows the list will really get — one fewer when the hint is drawn.
        // Counting a row that is not there scrolled the cursor off the top on a
        // short box, which is how a list of nine flags managed to show none.
        let view_h = if inner.height >= 3 {
            inner.height - 1
        } else {
            inner.height.max(1)
        } as usize;
        let max_off = variants.len().saturating_sub(view_h);
        *st.offset_mut() = sel.saturating_sub(view_h / 2).min(max_off);
        st.select(Some(sel));
    }
    // The hint gets the last row only while there is one to spare. On a console
    // squeezed down to a handful of cells the choices matter and the reminder
    // that Esc closes things does not — it is the same on every dialog here.
    if inner.height >= 3 {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);
        f.render_stateful_widget(list, split[0], &mut st);
        f.render_widget(
            Paragraph::new(Span::styled(t(app.lang, "pkg.logo_hint"), theme::mute())),
            split[1],
        );
    } else {
        f.render_stateful_widget(list, inner, &mut st);
    }
}

/// Paint a variant's name. Only the RAINBOW flag is coloured — its bright
/// primaries (red→violet across the letters) map cleanly onto the 16-colour VT
/// palette. Every other flag leans on pastels or dark tones that a truecolor-
/// less console approximates poorly, so those names stay plain white for
/// guaranteed legibility everywhere. Rainbow's one darker stripe is lifted by
/// `readable()` so it reads on the dark background too.
fn flag_colored(variant: &str) -> Vec<Span<'static>> {
    if variant != "Rainbow" {
        return vec![Span::styled(variant.to_string(), theme::normal())];
    }
    rainbow_text(variant)
}

/// Spread the rainbow palette across a string, one colour per letter (red→violet
/// left to right). Used for the "Rainbow" logo name and for the `slayfetch`
/// package name itself, so the entry announces what it is at a glance.
fn rainbow_text(text: &str) -> Vec<Span<'static>> {
    use ratatui::style::Color;
    let palette = crate::system::logos::flag_palette("Rainbow");
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len().max(1);
    let m = palette.len().max(1);
    chars
        .into_iter()
        .enumerate()
        .map(|(i, ch)| {
            let (r, g, b) = readable(palette[(i * m / n).min(m - 1)]);
            Span::styled(
                ch.to_string(),
                ratatui::style::Style::default().fg(Color::Rgb(r, g, b)),
            )
        })
        .collect()
}

/// Lift a colour so it reads on the dark panel background: pure black becomes a
/// light grey, and any near-black colour is scaled up (hue preserved) to a
/// visible brightness. Bright colours pass through unchanged.
fn readable((r, g, b): (u8, u8, u8)) -> (u8, u8, u8) {
    let maxc = r.max(g).max(b);
    if maxc == 0 {
        return (0xAE, 0xAE, 0xBE); // black stripe → light grey
    }
    if maxc < 130 {
        let factor = 175.0 / maxc as f32;
        let s = |v: u8| (v as f32 * factor).round().min(255.0) as u8;
        return (s(r), s(g), s(b));
    }
    (r, g, b)
}

/// Set up and open the logo picker. The list opens at the top (cursor 0), which
/// is Rainbow — pinned first by `logos::variants()`, with the rest alphabetical
/// after it. A leftover-empty choice resolves to DEFAULT_VARIANT in the plan, so
/// slayfetch always installs a real logo.
fn open_logo_modal(app: &mut App) {
    let variants = crate::system::logos::variants();
    if variants.is_empty() {
        return; // no logos embedded — nothing to pick
    }
    if app.config.fastfetch_logo.is_empty() {
        app.config.fastfetch_logo = crate::system::logos::DEFAULT_VARIANT.to_string();
    }
    app.logo_cursor = 0;
    app.logo_modal_open = true;
}

/// Keys while the logo picker is open: move, commit (Enter/Space), or cancel.
fn logo_modal_key(app: &mut App, key: KeyEvent) {
    let variants = crate::system::logos::variants();
    if super::nav::move_cursor(key.code, &mut app.logo_cursor, variants.len().max(1)) {
        return;
    }
    match key.code {
        KeyCode::Enter | KeyCode::Char(' ') => {
            if let Some(v) = variants.get(app.logo_cursor) {
                app.config.fastfetch_logo = v.clone();
            }
            app.logo_modal_open = false;
        }
        KeyCode::Esc => app.logo_modal_open = false,
        _ => {}
    }
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    // The slayfetch logo picker owns all keys while it's open.
    if app.logo_modal_open {
        logo_modal_key(app, key);
        return;
    }
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
        if super::nav::move_cursor(key.code, &mut app.gpu_cursor, GPUS.len()) {
            return;
        }
        match key.code {
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
    let active_len = if typing {
        app.pkg_results.len()
    } else {
        app.pkg_popular.len()
    };
    if super::nav::move_cursor(key.code, &mut app.cursor, active_len) {
        return;
    }
    match key.code {
        KeyCode::Char(' ') => {
            let picked = if typing {
                app.pkg_results.get(app.cursor).map(|p| p.name.clone())
            } else {
                app.pkg_popular.get(app.cursor).map(|p| p.name.clone())
            };
            if let Some(name) = picked {
                if let Some(pos) = app.config.extra_packages.iter().position(|x| *x == name) {
                    app.config.extra_packages.remove(pos);
                    // Unticking slayfetch drops its logo choice.
                    if name == "slayfetch" {
                        app.config.fastfetch_logo.clear();
                    }
                } else {
                    // Mutually exclusive pairs: only one may be picked.
                    //  • zsh / fish — only one login shell; both makes a mess.
                    //  • fastfetch / slayfetch — slayfetch IS fastfetch with a
                    //    chosen logo, so having both would install the package
                    //    twice and fight over the config.
                    match name.as_str() {
                        "zsh" => app.config.extra_packages.retain(|x| x != "fish"),
                        "fish" => app.config.extra_packages.retain(|x| x != "zsh"),
                        "fastfetch" => {
                            app.config.extra_packages.retain(|x| x != "slayfetch");
                            app.config.fastfetch_logo.clear();
                        }
                        "slayfetch" => app.config.extra_packages.retain(|x| x != "fastfetch"),
                        _ => {}
                    }
                    let is_slay = name == "slayfetch";
                    app.config.extra_packages.push(name);
                    // Picking slayfetch opens the logo picker straight away.
                    if is_slay {
                        open_logo_modal(app);
                    }
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
        // ALL three channel states must be handled. Matching only `Ok` left a
        // dead worker (panic mid-search) spinning "searching…" forever with no
        // way to retry — the same silent-limbo class as the old Wi-Fi bug.
        match rx.try_recv() {
            Ok(res) => {
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
            Err(crossbeam_channel::TryRecvError::Empty) => {}
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                app.pkg_searching = false;
                app.pkg_rx = None;
                app.pkg_results.clear();
                app.pkg_error = Some("search worker crashed - press Enter to retry".into());
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
