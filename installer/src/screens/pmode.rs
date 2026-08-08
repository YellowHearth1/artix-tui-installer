//! Step 9 — the dual-boot survey + disk-layout chooser. It asks, up front,
//! whether Windows (or another OS) is in the picture and where, then maps the
//! answer to a partition mode AND the right dual-boot defaults (os-prober on,
//! encryption availability), so the later screens are already tuned:
//!
//!   0 Whole disk   — Auto, single OS: wipe the whole disk. Encryption OK.
//!   1 Alongside    — same disk as Windows, into its free space (no wiping).
//!   2 Separate disk— Windows stays on ITS disk; Artix takes another disk
//!                    whole (Auto on that disk). Encryption OK; os-prober on.
//!   3 Manual       — the partition editor (assign roles / create partitions).
//!
//! Shared-disk paths (1, 3) are GRUB-only and, in v1, cannot encrypt the system
//! disk (LUKS on a disk shared with Windows isn't wired up yet). The separate-
//! disk path (2) has none of those limits — that disk is entirely Artix's.
//!
//! Firmware: Manual (3) works on UEFI *and* legacy BIOS — its editor carries a
//! firmware toggle and asks for a BIOS-boot partition instead of an ESP, which
//! is what old hardware needs. Alongside (1) and Separate (2) remain UEFI-only:
//! both are built around sharing or sitting beside an existing ESP.

use crate::app::{App, PartitionMode};
use crate::i18n::t;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

const OPT_WHOLE: usize = 0;
const OPT_ALONGSIDE: usize = 1;
const OPT_SEPARATE: usize = 2;
const OPT_MANUAL: usize = 3;
const OPTS: usize = 4;

/// Every dual-boot option needs a UEFI ESP to share or sit beside Windows (v1).
/// The plain whole-disk install is the only one that also works on BIOS.
fn needs_uefi(cursor: usize) -> bool {
    // Manual is no longer UEFI-only: its editor carries a firmware toggle and
    // knows to ask for a BIOS-boot partition instead of an ESP, which is what
    // an old machine (or firmware whose UEFI can't be trusted) needs.
    // "Alongside" still is — it shares an existing ESP by construction.
    matches!(cursor, OPT_ALONGSIDE | OPT_SEPARATE)
}

/// The per-option caveat shown under the list. Manual has two, because the
/// row's own toggle decides whether a neighbour OS is in the picture.
fn note_key(app: &App) -> &'static str {
    if app.cursor == OPT_MANUAL {
        return if app.config.manual_solo {
            "pmode.note_manual_solo"
        } else {
            "pmode.note_shared"
        };
    }
    match app.cursor {
        OPT_WHOLE => "pmode.note_whole",
        OPT_SEPARATE => "pmode.note_separate",
        // Alongside lands on the disk Windows lives on.
        _ => "pmode.note_shared",
    }
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let uefi = app.config.boot_mode.is_uefi();
    app.can_advance = !needs_uefi(app.cursor) || uefi;

    let mark = |sel: bool| if sel { "(\u{2022}) " } else { "( ) " };
    let style = |sel: bool| {
        if sel {
            theme::selected()
        } else {
            theme::normal()
        }
    };

    // Four options each carrying a two-line description + blank spacer, plus the
    // heading and a caveat, need ~20 rows. The real content panel on an 80x24
    // console is only ~15, so the 4th option and the note fall off the bottom —
    // with no scroll, they become invisible AND unreachable-looking. On a short
    // panel collapse to: every option's TITLE (so all choices stay visible) but
    // only the SELECTED option's description and note.
    let compact = area.height < 20;

    let mut lines = vec![
        Line::from(Span::styled(t(app.lang, "pmode.q"), theme::heading())),
        Line::from(""),
    ];
    for (i, (title, desc)) in [
        ("pmode.whole", "pmode.whole_desc"),
        ("pmode.alongside", "pmode.alongside_desc"),
        ("pmode.separate", "pmode.separate_desc"),
        ("pmode.manual", "pmode.manual_desc"),
    ]
    .into_iter()
    .enumerate()
    {
        let sel = app.cursor == i;
        let mut title_spans = vec![
            Span::styled(mark(sel), style(sel)),
            Span::styled(t(app.lang, title), style(sel)),
        ];
        // Manual is ONE entry with a ←/→ switch rather than two near-identical
        // rows: the editor is the same either way, only the promises differ.
        if i == OPT_MANUAL {
            let solo = app.config.manual_solo;
            let pick = |on: bool, key: &str| {
                if on {
                    Span::styled(
                        format!("\u{2039}{}\u{203a}", t(app.lang, key)),
                        if sel { theme::gold() } else { theme::accent() },
                    )
                } else {
                    Span::styled(format!(" {} ", t(app.lang, key)), theme::mute())
                }
            };
            title_spans.push(Span::styled("   ".to_string(), theme::dim()));
            title_spans.push(pick(solo, "pmode.manual_only"));
            title_spans.push(Span::styled(" \u{00b7} ".to_string(), theme::mute()));
            title_spans.push(pick(!solo, "pmode.manual_dual"));
        }
        lines.push(Line::from(title_spans));
        // Full: every description. Compact: only the selected option's, so the
        // whole survey fits without a scrollbar.
        let desc = if i == OPT_MANUAL && app.config.manual_solo {
            "pmode.manual_solo_desc"
        } else {
            desc
        };
        if !compact || sel {
            lines.push(Line::from(Span::styled(
                format!("      {}", t(app.lang, desc)),
                theme::dim(),
            )));
        }
        if !compact {
            lines.push(Line::from(""));
        }
    }
    // The caveat follows the highlighted option.
    if compact {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        t(app.lang, note_key(app)),
        theme::warn(),
    )));
    if !app.pmode_status.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("[!] {}", app.pmode_status),
            theme::warn(),
        )));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {} ", t(app.lang, "pmode.title")));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.cursor = app.cursor.saturating_sub(1);
            app.pmode_status.clear();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.cursor < OPTS - 1 {
                app.cursor += 1;
            }
            app.pmode_status.clear();
        }
        // On the manual row, ←/→ switches between "Artix only" and "dual
        // boot". Nowhere else does the survey use them, so they are free.
        KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if app.cursor == OPT_MANUAL => {
            app.config.manual_solo = !app.config.manual_solo;
            app.pmode_status.clear();
        }
        KeyCode::Enter => {
            if needs_uefi(app.cursor) && !app.config.boot_mode.is_uefi() {
                app.pmode_status = t(app.lang, "pmode.need_uefi");
                return;
            }
            // Map the scenario to a partition mode AND the dual-boot defaults.
            // os_prober = "add the neighbour OS to the boot menu"; it's on for
            // every dual-boot scenario and off for a lone whole-disk install.
            let (mode, os_prober) = match app.cursor {
                OPT_ALONGSIDE => (PartitionMode::Alongside, true),
                OPT_SEPARATE => (PartitionMode::Auto, true),
                OPT_MANUAL => (PartitionMode::Manual, true),
                _ => (PartitionMode::Auto, false),
            };
            app.config.partition_mode = mode;
            // `manual_solo` is already set by the row's own toggle. It has no
            // neighbour OS to find, so os-prober is pointless there — and it is
            // what tells the rest of the plan that encryption and the other
            // bootloaders are back on the table.
            let solo = mode.is_manual_family() && app.config.manual_solo;
            app.config.os_prober = os_prober && !solo;
            // A SHARED-disk install (alongside, or manual alongside Windows)
            // can't encrypt the system disk in v1 — drop a leftover choice so it
            // doesn't linger unused. A solo manual install keeps it.
            if mode.is_manual_family() && !solo {
                app.config.encrypt_disk = false;
            }
            // Switching disk-prep mode invalidates a disk chosen for another
            // mode, so the next screen re-picks cleanly.
            app.config.manual_disk.clear();
            app.pmode_status.clear();
            app.can_advance = true;
            app.goto_next();
        }
        _ => {}
    }
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "pmode.hint")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Screen;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Manual partitioning is ONE entry with a ←/→ switch, not two rows.
    ///
    /// The editor is identical either way; what differs is what the disk is
    /// shared with, and therefore what the plan may promise. Folding that into
    /// a switch keeps the survey to four genuinely different answers, and the
    /// switch has to carry the consequences with it — encryption and os-prober
    /// both hang off it.
    #[test]
    fn the_manual_row_switches_between_solo_and_dual_boot() {
        let mut app = App::new();
        app.screen = Screen::PartitionMode;
        app.cursor = OPT_MANUAL;
        app.config.boot_mode = crate::app::BootMode::Uefi;
        app.can_advance = true;

        // The survey is back to four answers.
        assert_eq!(OPTS, 4, "manual should not have a twin row any more");

        // ←/→ flips the mode without leaving the row.
        let before = app.config.manual_solo;
        handle_key(&mut app, key(KeyCode::Right));
        assert_ne!(app.config.manual_solo, before, "the switch did nothing");
        assert_eq!(app.cursor, OPT_MANUAL, "the switch moved the cursor");

        // Solo: encryption survives and no neighbour is searched for.
        app.config.manual_solo = true;
        app.config.encrypt_disk = true;
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.config.partition_mode, PartitionMode::Manual);
        assert!(app.config.manual_solo);
        assert!(
            app.config.encrypt_disk,
            "a solo manual install must keep the encryption choice"
        );
        assert!(!app.config.os_prober, "there is no second OS to look for");

        // Dual boot: the shared-disk limits come back.
        let mut app = App::new();
        app.screen = Screen::PartitionMode;
        app.cursor = OPT_MANUAL;
        app.config.boot_mode = crate::app::BootMode::Uefi;
        app.can_advance = true;
        app.config.manual_solo = false;
        app.config.encrypt_disk = true;
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            !app.config.encrypt_disk,
            "a shared disk cannot be encrypted in v1 — the stale choice must be dropped"
        );
        assert!(
            app.config.os_prober,
            "dual boot needs the neighbour detected"
        );
    }
}
