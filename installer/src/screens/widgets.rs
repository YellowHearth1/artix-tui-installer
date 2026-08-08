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
/// A label that does not fit, scrolled horizontally so all of it can be read.
///
/// ratatui clips silently: the tail of a long timezone name or package
/// description is simply not there, and nothing says so. This slides the text
/// instead, holding still at each end so the beginning and the end are both
/// readable rather than flashing past.
///
/// Character-based, not byte-based — the text this exists for is Ukrainian,
/// Spanish and, in translations to come, Arabic or Japanese, where a byte index
/// lands in the middle of a character.
pub fn marquee_fit(text: &str, width: usize, offset: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if width == 0 || chars.len() <= width {
        return text.to_string();
    }
    let span = chars.len() - width;
    // Hold at both ends: `span` steps of travel, plus a pause of a quarter of
    // the trip at each end so neither edge is a blur.
    let hold = (span / 4).max(3);
    let cycle = span + 2 * hold;
    let pos = offset % cycle;
    let start = if pos < hold {
        0
    } else if pos < hold + span {
        pos - hold
    } else {
        span
    };
    chars[start..start + width].iter().collect()
}

pub fn select_list(f: &mut Frame, area: Rect, items: &[String], selected: usize) {
    select_list_scrolled(f, area, items, selected, 0)
}

/// The same list, with the selected row scrolled by `offset` characters when it
/// is too wide to fit. Only that row moves — a list where everything slides at
/// once cannot be read at all.
pub fn select_list_scrolled(
    f: &mut Frame,
    area: Rect,
    items: &[String],
    selected: usize,
    offset: usize,
) {
    // Two columns go to the highlight symbol, so that is what a row really has.
    let room = area.width.saturating_sub(2) as usize;
    let rows: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let text = if i == selected {
                marquee_fit(s, room, offset)
            } else {
                s.clone()
            };
            ListItem::new(Line::from(Span::raw(text)))
        })
        .collect();
    let list = List::new(rows)
        .highlight_style(theme::selected())
        .highlight_symbol("> ")
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
                ("[x] ", theme::ok(), theme::gold())
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
        .highlight_symbol("> ");
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
    let cursor = if focused { "|" } else { "" };
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

/// One-line labelled input for short consoles: `› label: value▏`, no border box.
/// The boxed `input` above needs 4 rows per field, so a form with several fields
/// overflows the panel on an 80x24 console; this fits one field per row while
/// keeping the same focus caret and password masking.
pub fn input_inline(
    f: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    focused: bool,
    mask: bool,
) {
    let shown = if mask {
        "•".repeat(value.chars().count())
    } else {
        value.to_string()
    };
    let caret = if focused { "|" } else { "" };
    let line = Line::from(vec![
        Span::styled(if focused { "\u{203a} " } else { "  " }, theme::accent()),
        Span::styled(format!("{label}: "), theme::dim()),
        Span::styled(
            format!("{shown}{caret}"),
            if focused {
                theme::normal()
            } else {
                theme::mute()
            },
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// A two-button action row: Back (left) and Next/Confirm (right, accented when
/// enabled). Returns nothing; purely visual — screens own the key handling.
pub fn action_row(f: &mut Frame, area: Rect, _back: &str, next: &str, next_enabled: bool) {
    // Only the "Next" button is shown. Going back is done with Esc (noted in the
    // footer hint), so a decorative "Back" button would just duplicate it and,
    // since the TUI is key-driven (buttons aren't clickable), mislead the user.
    // The button is sized to ITS LABEL, not to a number that happened to fit the
    // first two languages. At a fixed 18 columns "[Enter] Siguiente" came out as
    // "[Enter] Siguient" — Ukrainian "Далі" and English "Next" fit, Spanish did
    // not, and nothing failed: the text was simply cut off inside the border.
    // Two columns for the borders, two for breathing room, never wider than the
    // row it sits in.
    let label = format!("[Enter] {next}");
    let want = label.chars().count() as u16 + 4;
    let btn_w = want.max(18).min(area.width);
    let cells = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(btn_w)])
        .split(area);

    let (style, bstyle) = if next_enabled {
        (theme::selected(), theme::border())
    } else {
        (theme::mute(), theme::border_dim())
    };
    // Label the button so it's obvious it activates on Enter (not the → arrow).
    // Plain ASCII "[Enter]" renders on any console font, unlike a return glyph.
    let next_p = Paragraph::new(Line::from(Span::styled(label, style)))
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

/// Smallest box still worth drawing a border around.
const MODAL_MIN_W: u16 = 20;
const MODAL_MIN_H: u16 = 4;

/// The width this text wants: its longest line, plus the borders and a column
/// of breathing room each side.
///
/// A DIALOG THAT FITS ITS TEXT DOES NOT MOVE; one whose text would be mangled
/// into a narrow column grows until it is not. The designed width is a floor,
/// not a verdict — before this, every box was whatever number the author of that
/// screen picked, so a translated line one word longer than the original wrapped
/// into mush and the frame around it stayed exactly as it was.
///
/// Counted in CHARACTERS, not bytes: these strings are Ukrainian more often than
/// not, and `len()` would ask for twice the columns they need.
pub(crate) fn text_width(lines: &[Line]) -> u16 {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0)
        .saturating_add(4)
        .min(u16::MAX as usize) as u16
}

/// Centre a dialog in `area`: the box its CONTENT asked for, plus the user's
/// own steps, clamped to the panel.
///
/// `w`/`h` are what the content needs, so the frame is derived from the text
/// rather than picked in advance — a dialog of wrapped prose asks to be wide, a
/// list asks to be tall, and `zoom` extends each in the proportion that dialog
/// already has. That is the "frame follows the text" half.
///
/// THE STEPS BELONG TO THE DIALOG ALONE. While one is open, `+`/`-` move `zoom`
/// and NOTHING else — not the console font, not the installer behind it. That
/// separation is the whole request: the box was resizing the entire screen along
/// with itself, and it should not.
///
/// Which leaves one thing this cannot do, and it is worth writing down so it is
/// not attempted again: a Linux VT has exactly ONE font for the whole screen, so
/// the letters inside a dialog cannot be made bigger while those outside it stay
/// put. Glyph size is the console font — global by construction — and it is
/// still on these keys whenever no dialog is open, and on the font screen.
///
/// Clamped by the panel, so a dialog can never spill off a console that has run
/// out of cells, and floored so `-` cannot shrink it to a line. Content that no
/// longer fits must WRAP rather than be cut, which is why every overlay sets
/// `Wrap { trim: true }`.
/// CENTRED ON THE WHOLE DISPLAY, not on the panel the screen happens to draw
/// into. Every dialog used to centre inside the content panel, which starts
/// after the step rail — so the middle of the box sat right of the middle of the
/// screen, and reading one meant turning your head. A dialog belongs to the
/// installer, not to the panel that raised it.
pub(crate) fn modal_rect_fit(f: &Frame, w: u16, h: u16, zoom: i16) -> Rect {
    modal_rect_in(f.area(), w, h, zoom)
}

/// The sizing itself, against an explicit rect — what the tests exercise.
pub(crate) fn modal_rect_in(area: Rect, w: u16, h: u16, zoom: i16) -> Rect {
    // A step is a tenth of the dialog's own width and a sixth of its height —
    // so a wide box gets wider and a tall one taller, instead of every dialog
    // creeping towards the same square. The floors keep the step visible on a
    // small box.
    let step_w = (w / 10).max(3) as i32;
    let step_h = (h / 6).max(1) as i32;
    let want_w = (w as i32 + step_w * zoom as i32).max(MODAL_MIN_W as i32);
    let want_h = (h as i32 + step_h * zoom as i32).max(MODAL_MIN_H as i32);

    let max_w = area.width.saturating_sub(4).max(MODAL_MIN_W);
    let max_h = area.height.saturating_sub(2).max(MODAL_MIN_H);
    let w = (want_w.min(max_w as i32) as u16).max(MODAL_MIN_W);
    let h = (want_h.min(max_h as i32) as u16).max(MODAL_MIN_H);
    Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    )
}

#[cfg(test)]
mod zoom_tests {
    use super::*;

    /// EVERY overlay must be sized through `modal_rect_fit`.
    ///
    /// A dialog that centres itself by hand does not get clamped to the panel,
    /// and on a console with few cells — which is exactly what a large console
    /// font produces — it draws outside its own screen. This shipped once
    /// already, with only the six partition-editor dialogs going through the
    /// helper because it lived in that file.
    ///
    /// `render_widget(Clear, ...)` is what makes something an overlay, so every
    /// one of those in a screen has to be matched by a sizing call in the same
    /// file. Counting is crude but it fails loudly the moment a dialog is added
    /// with its own hand-rolled centering.
    #[test]
    fn every_overlay_is_sized_through_the_zoom_helper() {
        let mut checked = 0;
        let entries = std::fs::read_dir("src/screens").expect("src/screens is readable");
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap_or_default();
            let overlays = src.matches("render_widget(Clear").count();
            if overlays == 0 {
                continue;
            }
            let sized = src.matches("modal_rect_fit").count();
            assert!(
                sized >= overlays,
                "{}: {overlays} overlay(s) but only {sized} sized through \
                 modal_rect_fit — a dialog that centres itself by hand is not \
                 clamped to the panel and will draw off a small console",
                path.display()
            );
            checked += 1;
        }
        assert!(
            checked >= 6,
            "only {checked} screens scanned — did the path move?"
        );
    }

    /// At rest the box is exactly what its content asked for; the steps move it
    /// from there, and it never leaves the panel.
    #[test]
    fn a_dialog_is_the_size_of_its_own_content() {
        let area = Rect::new(0, 0, 100, 40);
        let r = modal_rect_in(area, 60, 12, 0);
        assert_eq!(
            (r.width, r.height),
            (60, 12),
            "at rest the box is not the size its content asked for"
        );
        assert!(
            r.x + r.width <= area.width && r.y + r.height <= area.height,
            "the dialog is not inside its panel"
        );

        // A step has to be visible, and proportional: a wide dialog gets wider
        // rather than creeping towards a square.
        let bigger = modal_rect_in(area, 60, 12, 2);
        assert!(
            bigger.width > r.width && bigger.height > r.height,
            "the step did not grow the dialog: {bigger:?}"
        );
        assert!(
            bigger.width - r.width > bigger.height - r.height,
            "a wide dialog grew squarer instead of wider: {bigger:?}"
        );

        // A CONSOLE WITH FEW CELLS is what a large console font produces, and
        // the box has to come back inside it rather than draw over the edge —
        // held-down `+` included.
        let cramped = Rect::new(0, 0, 40, 10);
        for zoom in [0i16, 12] {
            let r = modal_rect_in(cramped, 78, 18, zoom);
            assert!(
                r.width <= cramped.width && r.height <= cramped.height,
                "zoom {zoom} drew outside a 40x10 console: {r:?}"
            );
        }

        // And `-` held down never collapses it to something with no room.
        let r = modal_rect_in(area, 60, 12, -100);
        assert!(r.width >= MODAL_MIN_W && r.height >= MODAL_MIN_H);
    }

    /// The frame is derived from the text, so a longer translation gets a wider
    /// box instead of being wrapped into a narrow column.
    #[test]
    fn the_frame_widens_for_text_that_would_not_fit() {
        let short = vec![Line::from("corto")];
        let long = vec![Line::from(
            "una linea mucho mas larga que la anterior y necesita mas sitio",
        )];
        assert!(
            text_width(&long) > text_width(&short),
            "the longer line did not ask for a wider box"
        );
        // Counted in CHARACTERS. The interface is Ukrainian and Spanish as often
        // as English, and those letters are two bytes each — a byte count would
        // ask for twice the columns the text actually occupies.
        let accented = vec![Line::from("áéíóú")];
        assert_eq!(
            text_width(&accented),
            9,
            "the width was measured in bytes, not characters"
        );
    }
}

#[cfg(test)]
mod marquee_tests {
    use super::*;

    /// A label that fits is never touched.
    #[test]
    fn short_labels_are_left_alone() {
        assert_eq!(marquee_fit("Europe/Kyiv", 20, 0), "Europe/Kyiv");
        assert_eq!(marquee_fit("Europe/Kyiv", 20, 99), "Europe/Kyiv");
    }

    /// A long one starts at the beginning, travels, and ends at the end — the
    /// whole text is reachable, which is the point. Clipping showed the head
    /// forever and the tail never.
    #[test]
    fn a_long_label_eventually_shows_all_of_itself() {
        // Accented Latin, not Cyrillic: the guard against hardcoded UI
        // strings scans for Cyrillic words, and these are multibyte either way.
        let text = "Español — traducción muy larga de ich0x";
        let width = 12;
        let mut seen = String::new();
        for offset in 0..200 {
            let piece = marquee_fit(text, width, offset);
            assert_eq!(piece.chars().count(), width, "a step changed the width");
            if !seen.contains(&piece) {
                seen.push_str(&piece);
            }
        }
        let chars: Vec<char> = text.chars().collect();
        let head: String = chars[..width].iter().collect();
        let tail: String = chars[chars.len() - width..].iter().collect();
        assert!(seen.contains(&head), "the start of the label never showed");
        assert!(seen.contains(&tail), "the end of the label never showed");
    }

    /// It holds still at both ends rather than bouncing off them, so the first
    /// and last words can actually be read.
    #[test]
    fn it_pauses_at_each_end() {
        let text = "a-very-long-label-that-will-not-fit-at-all";
        let w = 10;
        let first = marquee_fit(text, w, 0);
        assert_eq!(first, marquee_fit(text, w, 1), "it moved on the first tick");
        assert_eq!(first, marquee_fit(text, w, 2));
    }

    /// Counted in CHARACTERS. A byte slice would cut a Cyrillic letter in half
    /// and panic, which is a crash on the timezone list for half the languages
    /// this installer speaks.
    #[test]
    fn multibyte_text_is_cut_between_characters() {
        for offset in 0..60 {
            let piece = marquee_fit("Zona horaria Europe/Kyiv año español", 8, offset);
            assert_eq!(piece.chars().count(), 8);
        }
    }
}
