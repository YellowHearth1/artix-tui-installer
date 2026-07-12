//! Visual theme — the soul of the "graphical installer" feel.
//!
//! Direction: refined dark, deep slate background with a single luminous teal
//! accent and warm coral for warnings. Generous spacing, rounded borders,
//! clear done/active/pending state colors for the step sidebar. The palette is
//! tuned to read well on real TTYs and VM consoles alike.

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders};

// ── Palette ────────────────────────────────────────────────────────────────
// IMPORTANT: the installer's primary target is the live VT console (the kernel
// terminal), which has NO truecolor — the kernel approximates 24-bit SGR onto
// its 16-color palette, and the mapping is lossy: mid-tone RGB values (our old
// gold, soft grays) collapse into near-identical grays, killing the focus
// highlight entirely. So every color that carries MEANING (accent, gold, warn,
// ok, dim/mute steps) is an indexed ANSI color: deterministic on the VT and
// respectable in any terminal emulator. Only the cosmetic backgrounds stay RGB
// — they degrade gracefully to black on the VT.
pub const BG: Color = Color::Rgb(16, 20, 26); // deep slate (VT: black)
pub const PANEL: Color = Color::Rgb(22, 28, 36); // (VT: black)
pub const ACCENT: Color = Color::LightCyan;
pub const ACCENT_SOFT: Color = Color::Cyan;
pub const FG: Color = Color::White;
pub const FG_DIM: Color = Color::Gray;
pub const FG_MUTE: Color = Color::DarkGray;
pub const SEL_BG: Color = Color::Rgb(30, 52, 54); // (VT: black; fg carries it)
pub const WARN: Color = Color::LightRed;
// "Success/done" color. Deliberately NOT green: the whole UI is keyed to the
// Artix cyan, and green clashes with it (and looks alien next to the logo).
// Done/ok states read as the calmer indexed cyan; the active accent stays the
// bright LightCyan, warnings stay red. No green anywhere in the palette.
pub const OK: Color = Color::Cyan;

// ── Text styles ──────────────────────────────────────────────────────────────
pub fn title() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}
pub fn heading() -> Style {
    Style::default().fg(FG).add_modifier(Modifier::BOLD)
}
pub fn normal() -> Style {
    Style::default().fg(FG)
}
pub fn dim() -> Style {
    Style::default().fg(FG_DIM)
}
pub fn mute() -> Style {
    Style::default().fg(FG_MUTE)
}
pub fn selected() -> Style {
    Style::default()
        .fg(ACCENT)
        .bg(SEL_BG)
        .add_modifier(Modifier::BOLD)
}
pub fn accent() -> Style {
    Style::default().fg(ACCENT)
}
pub fn warn() -> Style {
    Style::default().fg(WARN).add_modifier(Modifier::BOLD)
}
pub fn ok() -> Style {
    Style::default().fg(OK)
}
pub fn gold() -> Style {
    // Historically LightYellow — but on real fbcon/VT consoles bright yellow
    // (sent by crossterm as 38;5;11) washes out to an indistinct grey on some
    // framebuffer palettes, silently killing every focus highlight that used
    // it. Bold bright-cyan is the one accent proven to render everywhere this
    // installer runs, so the "gold" role is now BOLD ACCENT. (Name kept to
    // avoid a rename across every screen.)
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}
pub fn border() -> Style {
    Style::default().fg(ACCENT_SOFT)
}
pub fn border_dim() -> Style {
    Style::default().fg(FG_MUTE)
}

// ── Sidebar step states ──────────────────────────────────────────────────────
pub fn step_done() -> Style {
    Style::default().fg(OK)
}
pub fn step_active() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}
pub fn step_pending() -> Style {
    Style::default().fg(FG_MUTE)
}

// ── Reusable blocks ──────────────────────────────────────────────────────────
/// A rounded content panel with a soft accent border and a padded title.
pub fn panel(title_str: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border())
        .title(format!(" {title_str} "))
        .title_style(title())
        .style(Style::default().bg(PANEL))
}

/// Like `panel`, but dimmed — for a panel that is not currently focused.
pub fn panel_dim(title_str: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_dim())
        .title(format!(" {title_str} "))
        .title_style(dim())
        .style(Style::default().bg(PANEL))
}

/// A plain rounded box, no title.
pub fn box_rounded() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_dim())
}
