//! Step 8 — AUR packages. A full screen dedicated to installing packages from
//! the Arch User Repository via paru. Mirrors the repo package screen: a search
//! box at the top, a live results list (AUR RPC API) in the middle, and a
//! curated "recommended from AUR" list when the search box is empty. Space
//! toggles a package in/out of the selection; the chosen packages are built by
//! paru at the end of installation.

use crate::app::App;
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

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // intro line
            Constraint::Length(3), // search box
            Constraint::Min(0),    // results
            Constraint::Length(2), // selected summary
            Constraint::Length(3), // actions
        ])
        .spacing(1)
        .split(area);

    // ── intro ──
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("  {}", t(app.lang, "aur.intro")),
            theme::dim(),
        ))),
        rows[0],
    );

    // ── search box ──
    let title = if app.aur_searching {
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
            if app.aur_query.is_empty() {
                t(app.lang, "aur.search")
            } else {
                app.aur_query.clone()
            },
            if app.aur_query.is_empty() {
                theme::mute()
            } else {
                theme::normal()
            },
        ),
        Span::styled("▏", theme::accent()),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme::border())
            .title(title)
            .title_style(theme::accent()),
    );
    f.render_widget(search, rows[1]);

    // ── results / messages ──
    draw_results(f, app, rows[2]);

    // ── selected summary ──
    let sel = &app.config.aur_packages;
    let sel_text = if sel.is_empty() {
        t(app.lang, "aur.none_selected")
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
}

fn draw_results(f: &mut Frame, app: &App, area: Rect) {
    let typing = !app.aur_query.trim().is_empty();

    if let Some(err) = &app.aur_error {
        if typing {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(format!("  {err}"), theme::warn()))),
                area,
            );
            return;
        }
    }

    let rows_source: &Vec<Pkg> = if typing {
        &app.aur_results
    } else {
        &app.aur_popular
    };

    if typing && rows_source.is_empty() {
        let msg = if app.aur_searching {
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
        t(app.lang, "aur.results")
    } else {
        t(app.lang, "aur.popular")
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
            let checked = app.config.aur_packages.contains(&p.name);
            let (mark, mark_style, name_style) = if checked {
                ("[✓] ", theme::ok(), theme::gold())
            } else {
                ("[ ] ", theme::mute(), theme::normal())
            };
            ListItem::new(Line::from(vec![
                Span::styled(mark, mark_style),
                Span::styled(p.name.clone(), name_style),
                Span::styled(
                    if p.desc.is_empty() {
                        String::new()
                    } else {
                        format!("  — {}", p.desc)
                    },
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
        let sel = app.aur_cursor.min(rows_source.len() - 1);
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
    let typing = !app.aur_query.trim().is_empty();
    let active_len = if typing {
        app.aur_results.len()
    } else {
        app.aur_popular.len()
    };
    if super::nav::move_cursor(key.code, &mut app.aur_cursor, active_len) {
        return;
    }
    match key.code {
        // Space toggles the highlighted AUR package in/out of the selection.
        KeyCode::Char(' ') => {
            let picked = if typing {
                app.aur_results.get(app.aur_cursor).map(|p| p.name.clone())
            } else {
                app.aur_popular.get(app.aur_cursor).map(|p| p.name.clone())
            };
            if let Some(name) = picked {
                if let Some(pos) = app.config.aur_packages.iter().position(|x| *x == name) {
                    app.config.aur_packages.remove(pos);
                } else {
                    // The two MangoWM packages provide the same compositor and
                    // conflict at install time, so selecting one removes the
                    // other — the user can only have one.
                    const MANGO: [&str; 2] = ["mangowm", "mangowm-wlonly-git"];
                    if MANGO.contains(&name.as_str()) {
                        app.config
                            .aur_packages
                            .retain(|x| !MANGO.contains(&x.as_str()));
                    }
                    app.config.aur_packages.push(name);
                }
            }
        }
        KeyCode::Enter => app.goto_next(),
        KeyCode::Esc => {
            // Esc clears the live search first (returning to the recommended
            // list); only an Esc with an already-empty query leaves the screen
            // — and that case is routed by the global handler, so here we only
            // ever handle "clear the query".
            app.aur_query.clear();
            app.aur_cursor = 0;
            app.aur_results.clear();
        }
        // Typing edits the AUR query and (debounced) triggers a live search.
        KeyCode::Char(c) => {
            app.aur_query.push(c);
            app.aur_cursor = 0;
            app.aur_debounce = 4; // ~400ms at the 100ms tick
        }
        KeyCode::Backspace => {
            app.aur_query.pop();
            app.aur_cursor = 0;
            app.aur_debounce = 4;
        }
        _ => {}
    }
}

pub fn tick(app: &mut App) {
    // Drain a finished AUR search.
    if let Some(rx) = &app.aur_rx {
        // ALL three channel states must be handled — see packages.rs::tick for
        // the story (a dead worker must surface as an error, not a forever-
        // spinner).
        match rx.try_recv() {
            Ok(res) => {
                app.aur_searching = false;
                app.aur_rx = None;
                match res {
                    Ok(list) => {
                        app.aur_results = list;
                        app.aur_error = None;
                        app.aur_cursor = 0;
                    }
                    Err(e) => {
                        app.aur_results.clear();
                        app.aur_error = Some(e);
                    }
                }
            }
            Err(crossbeam_channel::TryRecvError::Empty) => {}
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                app.aur_searching = false;
                app.aur_rx = None;
                app.aur_results.clear();
                app.aur_error = Some("search worker crashed - press Enter to retry".into());
            }
        }
    }

    // Debounce countdown; when it hits zero and the query changed, search.
    if app.aur_debounce > 0 {
        app.aur_debounce -= 1;
        if app.aur_debounce == 0 {
            let q = app.aur_query.trim().to_string();
            if q.is_empty() {
                app.aur_results.clear();
                app.aur_error = None;
            } else if q != app.aur_inflight_query || app.aur_rx.is_none() {
                launch_aur_search(app, q);
            }
        }
    }
}

fn launch_aur_search(app: &mut App, query: String) {
    let (tx, rx) = crossbeam_channel::bounded(1);
    app.aur_inflight_query = query.clone();
    app.aur_searching = true;
    app.aur_rx = Some(rx);
    std::thread::spawn(move || {
        let result = packages::aur_search(&query);
        let _ = tx.send(result);
    });
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "aur.footer")
}
