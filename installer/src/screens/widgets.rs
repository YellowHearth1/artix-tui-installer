//! Shared UI building blocks so every screen looks like one cohesive product:
//! selectable lists with a left accent bar, a "Next ▸ / ◂ Back" button row,
//! labeled text inputs.

use crate::theme;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};

/// A selectable list with accent highlight. `selected` is the cursor index.
pub fn select_list(f: &mut Frame, area: Rect, items: &[String], selected: usize) {
    let rows: Vec<ListItem> = items
        .iter()
        .map(|s| ListItem::new(Line::from(Span::raw(s.clone()))))
        .collect();
    let list = List::new(rows)
        .highlight_style(theme::selected())
        .highlight_symbol("▎ ")
        .style(theme::normal());
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(selected.min(items.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// A multi-select list: `checked` marks chosen rows, `cursor` is focus.
pub fn multi_list(
    f: &mut Frame,
    area: Rect,
    items: &[String],
    checked: &dyn Fn(usize) -> bool,
    cursor: usize,
) {
    let rows: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let on = checked(i);
            let (mark, mark_style, text_style) = if on {
                ("[✓] ", theme::ok(), theme::gold())
            } else {
                ("[ ] ", theme::mute(), theme::normal())
            };
            ListItem::new(Line::from(vec![
                Span::styled(mark, mark_style),
                Span::styled(s.clone(), text_style),
            ]))
        })
        .collect();
    let list = List::new(rows)
        .highlight_style(theme::selected())
        .highlight_symbol("▎ ");
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(cursor.min(items.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// A labeled text field. `focused` draws an accent border; `mask` hides input.
pub fn input(f: &mut Frame, area: Rect, label: &str, value: &str, focused: bool, mask: bool) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(3)])
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(label, theme::dim()))),
        rows[0],
    );

    let shown = if mask {
        "•".repeat(value.chars().count())
    } else {
        value.to_string()
    };
    let cursor = if focused { "▏" } else { "" };
    let border = if focused {
        theme::border()
    } else {
        theme::border_dim()
    };
    let b = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(border);
    // ONE span, ONE style for text + caret. With multiple styles on the line,
    // incremental redraws while typing leave the first character with a stale
    // attribute on the Linux VT (its handling of intensity-reset SGR codes is
    // unreliable), producing the "first • bright, the rest darker" artifact.
    // A single uniform span makes that structurally impossible.
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            format!(" {shown}{cursor}"),
            if focused {
                theme::normal()
            } else {
                theme::mute()
            },
        )]))
        .block(b),
        rows[1],
    );
}

/// A two-button action row: Back (left) and Next/Confirm (right, accented when
/// enabled). Returns nothing; purely visual — screens own the key handling.
pub fn action_row(f: &mut Frame, area: Rect, _back: &str, next: &str, next_enabled: bool) {
    // Only the "Next" button is shown. Going back is done with Esc (noted in the
    // footer hint), so a decorative "Back" button would just duplicate it and,
    // since the TUI is key-driven (buttons aren't clickable), mislead the user.
    let cells = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(18)])
        .split(area);

    let (style, bstyle) = if next_enabled {
        (theme::selected(), theme::border())
    } else {
        (theme::mute(), theme::border_dim())
    };
    // Label the button so it's obvious it activates on Enter (not the → arrow).
    // Plain ASCII "[Enter]" renders on any console font, unlike a return glyph.
    let next_p = Paragraph::new(Line::from(Span::styled(format!("[Enter] {next}"), style)))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(bstyle),
        );
    f.render_widget(next_p, cells[1]);
}

/// A short helper-text line under a heading.
pub fn hint_line(f: &mut Frame, area: Rect, text: &str) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(text, theme::dim()))),
        area,
    );
}
