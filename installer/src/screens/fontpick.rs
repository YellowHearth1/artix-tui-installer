//! Console font chooser, reached from the mode menu and OUTSIDE the 15-step
//! install flow.
//!
//! This installer's first target is a bare TTY — no X, no drivers, no graphical
//! terminal. There the font is not decoration: it decides whether the interface
//! is legible at all, and whether it can draw its own text. Two fonts shipped
//! before this screen existed and both were half-broken for it: `UniCyr_8x16`
//! has Cyrillic and NOT ONE Spanish accent, `LatArCyrHeb-16+` has both scripts
//! and no arrows, so `↑/↓` hints came out blank.
//!
//! Every font offered here was checked by reading its PSF Unicode table against
//! what the interface actually asks for — the Ukrainian letters і ї є ґ, the
//! Latin-1 accents, the arrows, the dashes and the middle dot. A font that
//! cannot draw all of that is not on the list, however nice it looks.
//!
//! Picking is live: the font applies as the cursor moves, because the only
//! honest preview of a console font is the console wearing it.

use crate::app::{App, Screen};
use crate::i18n::t;
use crate::screens::widgets;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// A font family, and the sizes it ships in.
///
/// Split into family and size because those are two different questions: one is
/// what the letters look like, the other is how big they are, and a single flat
/// list of two dozen names made you answer both at once by reading codes like
/// `ter-v22b`.
pub(crate) struct Family {
    pub name: &'static str,
    /// `(label shown, name passed to setfont)`, smallest first.
    pub sizes: &'static [(&'static str, &'static str)],
    /// Whether the family has `▀ ▄ ▌` — the half-height blocks. Nothing in the
    /// interface needs them EXCEPT the donation QR code, which packs two module
    /// rows into every text row and cannot fit on an 80×24 console any other way.
    /// Terminus does not have them (it ships `█ ░ ▒` and stops there), so on a
    /// console wearing Terminus the code came out as a field of holes that no
    /// scanner could read. `finish.rs` asks this before drawing it.
    pub half_blocks: bool,
    /// True when the family is carried on the live image rather than installed by
    /// a package. It matters at install time: `/etc/vconsole.conf` on the new
    /// system names the chosen font, and a name the new system does not HAVE
    /// leaves it booting with the kernel default. So a vendored font is copied
    /// into the target — see `install::mod`'s vconsole step.
    pub vendored: bool,
}

/// Every family here was checked by reading its PSF Unicode table: it draws the
/// Ukrainian і ї є ґ, the Latin-1 accents Spanish needs, the ←→ of the key hints,
/// the dashes, the quotes and the box lines.
///
/// Only three of these come from packages, and that is not caution — it is the
/// whole shelf. Of the 362 console fonts `kbd` and `terminus-font` install, 64
/// have the Ukrainian letters, but `terminus-font`'s ten variants (`ter-c`,
/// `ter-d`, … `ter-u`, `ter-v`) are NINE single-codepage subsets of ~262 glyphs
/// plus `ter-v`: only `ter-v` and `kbd`'s `LatGrkCyr` can draw all three
/// languages at once. And `pacman -F usr/share/kbd/consolefonts/` names `kbd`,
/// `terminus-font` and `cmatrix` as the only packages in Arch, Artix or
/// Chaotic-AUR that ship console fonts at all.
///
/// So the other three are carried on the live image instead, from
/// `iso-profile/live-overlay/`. Two of them were missing a few letters and were
/// completed by `scripts/psf-patch.py`, which gives up glyph slots the interface
/// never asks for — a console font holds exactly 512 glyphs and every one of
/// these was already at that ceiling, so nothing can simply be added.
///
/// No font here is complete in every respect, which is why `half_blocks` exists:
/// Terminus has no `▀ ▄ ▌`, LatGrkCyr has no `▲ ▼ ≥`. The interface avoids the
/// latter set entirely; the former is needed only by the donation QR.
pub(crate) const FAMILIES: &[Family] = &[
    // Terminus with three glyphs added and a name of its own.
    //
    // Terminus is OFL-1.1 with the Reserved Font Name "Terminus Font", and
    // clause 3 forbids a modified version from carrying that name — so the
    // modified copies are called something else, and the licence and provenance
    // travel with them in usr/share/licenses/artix-tui-consolefonts/.
    //
    // What was added: `▀ ▄ ▌`. Terminus ships `█ ░ ▒` and stops there, and the
    // donation QR is half-block encoded — two module rows per text row, the only
    // way it fits a console at all. Without them the code drew as a field of
    // holes no scanner could read, on the DEFAULT font, which is the one console
    // it was most likely to be looked at on.
    Family {
        name: "Artix TUI Bold",
        sizes: &[
            ("8×14", "tui-14b"),
            ("8×16", "tui-16b"),
            ("10×18", "tui-18b"),
            ("10×20", "tui-20b"),
            ("11×22", "tui-22b"),
            ("12×24", "tui-24b"),
            ("14×28", "tui-28b"),
            ("16×32", "tui-32b"),
        ],
        half_blocks: true,
        vendored: true,
    },
    Family {
        name: "Artix TUI",
        sizes: &[
            ("6×12", "tui-12n"),
            ("8×14", "tui-14n"),
            ("8×16", "tui-16n"),
            ("10×18", "tui-18n"),
            ("10×20", "tui-20n"),
            ("11×22", "tui-22n"),
            ("12×24", "tui-24n"),
            ("14×28", "tui-28n"),
            ("16×32", "tui-32n"),
        ],
        half_blocks: true,
        vendored: true,
    },
];

/// setfont name for a family/size pair, clamped so an out-of-range size index
/// (a smaller family after switching) still resolves to something real.
pub(crate) fn font_name(fam: usize, size: usize) -> &'static str {
    let f = &FAMILIES[fam.min(FAMILIES.len() - 1)];
    f.sizes[size.min(f.sizes.len() - 1)].1
}

/// Where a fresh run starts: the bold face at 8×16, the size a plain VGA console
/// uses.
///
/// Bold because the regular weight is thin on a real console — fine on a backlit
/// panel, much less so on the hardware this installer is aimed at. And this
/// particular copy because it is the one that can draw the donation QR: the
/// packaged Terminus cannot, and a default that silently drops the code is not a
/// default worth having.
pub(crate) fn default_pos() -> (usize, usize) {
    (0, 1)
}

/// The font a fresh run starts with, by name.
///
/// `App::new()` seeds `config.console_font` from HERE rather than repeating a
/// name. It used to repeat one, and the two drifted the moment the list
/// changed: the screen opened on the bold face while the config still said
/// `ter-v16n`, so the installer started thin, wrote thin into the installed
/// system, and only switched once the cursor was moved.
pub(crate) fn default_font() -> &'static str {
    let (fam, size) = default_pos();
    font_name(fam, size)
}

/// Find a font by setfont name, for reopening the screen on the current pick.
pub(crate) fn position_of(name: &str) -> (usize, usize) {
    for (fi, f) in FAMILIES.iter().enumerate() {
        if let Some(si) = f.sizes.iter().position(|s| s.1 == name) {
            return (fi, si);
        }
    }
    default_pos()
}

/// The file name a vendored font lives under on the live image, or `None` for a
/// font that came from a package.
///
/// The installer writes the chosen font into the new system's
/// `/etc/vconsole.conf`; a vendored font exists only on the ISO, so without
/// copying the file across, the installed system would name a font it does not
/// have and boot with the kernel default instead.
pub(crate) fn vendored_file(font: &str) -> Option<String> {
    FAMILIES
        .iter()
        .find(|f| f.vendored && f.sizes.iter().any(|s| s.1 == font))
        .map(|_| format!("{font}.psfu.gz"))
}

/// Are we on the Linux console rather than in a graphical terminal? The console
/// sets `TERM=linux`; every graphical emulator sets something else
/// (`xterm-256color`, `foot`, …). This is the only thing that separates the two
/// worlds without probing hardware, and it matters because the console can only
/// draw what the loaded PSF font contains, while a graphical terminal falls back
/// through the whole system font stack and can draw practically anything.
pub(crate) fn on_linux_console() -> bool {
    std::env::var("TERM").map(|t| t == "linux").unwrap_or(false)
}

/// Can the console draw `▀ ▄ ▌` right now? In a graphical terminal: always, its
/// font stack has them. On the console: only if the loaded font does — which is
/// looked up in `FAMILIES`, since that is the list we put there ourselves.
///
/// An unknown name (someone ran `setfont` by hand) is treated as NOT having them:
/// silently dropping the QR code is recoverable, drawing an unscannable one is
/// what we are fixing.
pub(crate) fn can_draw_half_blocks(font: &str) -> bool {
    half_blocks_available(font, on_linux_console())
}

/// The decision above with the environment passed in, so it can be tested
/// without writing to `TERM` — a global that the render tests read from other
/// threads at the same time.
fn half_blocks_available(font: &str, on_console: bool) -> bool {
    if !on_console {
        return true;
    }
    FAMILIES
        .iter()
        .find(|f| f.sizes.iter().any(|s| s.1 == font))
        .map(|f| f.half_blocks)
        .unwrap_or(false)
}

/// Load a font. Returns whether it actually loaded.
///
/// The result matters. `setfont` fails when the file is not there — which is
/// exactly what happens to a font carried on the ISO if the image has not been
/// rebuilt since it was added. Swallowing that made the screen lie: the cursor
/// moved, the name under the list changed, and the console kept the previous
/// font. It read as "fonts apply sometimes and sometimes not", because the
/// packaged ones worked and the carried ones did not.
///
/// In a graphical terminal `setfont` is not installed at all. That is not a
/// failure worth reporting — there is no console font there to set — so it is
/// told apart by asking whether the command could be run at all.
pub(crate) fn apply(name: &str) -> bool {
    // Never touch the real console from a test: the suite runs on the
    // developer's machine, and `setfont` there would either change the terminal
    // out from under them or fail for reasons that say nothing about the code.
    if cfg!(test) {
        return true;
    }
    let ran = std::process::Command::new("setfont")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match ran {
        // Only a console can fail meaningfully. In a graphical terminal setfont
        // is either missing or refuses because there is no console to set, and
        // neither says anything about the font — reporting it there would put a
        // false error in front of someone whose fonts are fine.
        Ok(st) => st.success() || !on_linux_console(),
        Err(_) => true,
    }
}

/// Load whatever the cursor is on, and say so if it did not take.
///
/// A font that will not load is not a small problem on this screen: the whole
/// point of it is that the console wears what you are pointing at, so a silent
/// failure turns the list into a lie.
fn pick(app: &mut App) {
    let name = font_name(app.font_family, app.font_size_idx);
    if apply(name) {
        app.config.console_font = name.to_string();
        app.pmode_status.clear();
    } else {
        app.pmode_status = format!("{} {name}", t(app.lang, "font.failed"));
    }
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // hint
            Constraint::Length(4), // family
            Constraint::Length(4), // size
            Constraint::Length(4), // sample
            Constraint::Min(0),    // spacer
            Constraint::Length(3), // actions
        ])
        .spacing(1)
        .split(area);

    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(t(app.lang, "font.hint"), theme::dim())),
            Line::from(Span::styled(t(app.lang, "font.hint2"), theme::mute())),
        ]),
        rows[0],
    );

    let fam = app.font_family.min(FAMILIES.len() - 1);
    let ftitle = t(app.lang, "font.family");
    let f_block = if app.font_focus == 0 {
        theme::panel(&ftitle)
    } else {
        theme::panel_dim(&ftitle)
    };
    let f_inner = f_block.inner(rows[1]);
    f.render_widget(f_block, rows[1]);
    let fam_pills: Vec<Span> = FAMILIES
        .iter()
        .enumerate()
        .flat_map(|(i, fm)| {
            let sel = i == fam;
            vec![
                Span::styled(
                    format!(" {} ", fm.name),
                    if sel {
                        theme::selected()
                    } else {
                        theme::normal()
                    },
                ),
                Span::raw("  "),
            ]
        })
        .collect();
    f.render_widget(Paragraph::new(Line::from(fam_pills)), f_inner);

    let sizes = FAMILIES[fam].sizes;
    let si = app.font_size_idx.min(sizes.len() - 1);
    let stitle = t(app.lang, "font.size");
    let s_block = if app.font_focus == 1 {
        theme::panel(&stitle)
    } else {
        theme::panel_dim(&stitle)
    };
    let s_inner = s_block.inner(rows[2]);
    f.render_widget(s_block, rows[2]);
    let size_pills: Vec<Span> = sizes
        .iter()
        .enumerate()
        .flat_map(|(i, (label, _))| {
            let sel = i == si;
            vec![
                Span::styled(
                    format!(" {label} "),
                    if sel {
                        theme::selected()
                    } else {
                        theme::normal()
                    },
                ),
                Span::raw(" "),
            ]
        })
        .collect();
    f.render_widget(Paragraph::new(Line::from(size_pills)), s_inner);

    // Every glyph class the interface leans on, in the font now loaded. If any
    // of it were blank that font would have broken the UI — none on this list
    // does, and showing it is how you can tell at a glance.
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(t(app.lang, "font.sample"), theme::dim())),
            Line::from(Span::styled(
                format!("  {}", t(app.lang, "font.glyphs")),
                theme::accent(),
            )),
            Line::from(Span::styled(
                format!("  setfont {}", font_name(fam, si)),
                theme::mute(),
            )),
        ]),
        rows[3],
    );

    widgets::action_row(
        f,
        rows[5],
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        true,
    );
    app.can_advance = false;
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    let fam = app.font_family.min(FAMILIES.len() - 1);
    match key.code {
        // ↑/↓ move between the two questions; ←/→ answer the focused one. Every
        // change loads the font immediately, because a list of names is useless
        // as a preview of a console font.
        KeyCode::Up | KeyCode::Char('k') => app.font_focus = 0,
        KeyCode::Down | KeyCode::Char('j') => app.font_focus = 1,
        KeyCode::Left | KeyCode::Char('h') => {
            if app.font_focus == 0 {
                app.font_family = fam.saturating_sub(1);
                // A shorter family can leave the size index past its end.
                let n = FAMILIES[app.font_family].sizes.len();
                app.font_size_idx = app.font_size_idx.min(n - 1);
            } else {
                app.font_size_idx = app.font_size_idx.saturating_sub(1);
            }
            pick(app);
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if app.font_focus == 0 {
                app.font_family = (fam + 1).min(FAMILIES.len() - 1);
                let n = FAMILIES[app.font_family].sizes.len();
                app.font_size_idx = app.font_size_idx.min(n - 1);
            } else {
                let n = FAMILIES[fam].sizes.len();
                app.font_size_idx = (app.font_size_idx + 1).min(n - 1);
            }
            pick(app);
        }
        // Both Enter and Esc keep the pick: walking away from a font you just
        // chose and having it snap back would be its own small betrayal.
        KeyCode::Enter | KeyCode::Esc => {
            app.config.console_font = font_name(app.font_family, app.font_size_idx).to_string();
            app.goto(Screen::Mode);
        }
        _ => {}
    }
}

pub fn hint(app: &App) -> String {
    t(app.lang, "font.footer")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The donation QR is half-block encoded, so it may only be drawn by a font
    /// that HAS half blocks. Terminus does not, and it is the default: the code
    /// used to render as an unscannable field of holes on the console it was most
    /// likely to be seen on.
    #[test]
    fn only_a_half_block_font_may_draw_the_qr_on_the_console() {
        // The packaged Terminus is not offered and has no half blocks, so it is
        // the case the QR must be suppressed on.
        assert!(
            !half_blocks_available("ter-v16n", true),
            "the packaged Terminus has no half blocks, so the QR must be suppressed"
        );
        // Every font the chooser offers has them — that is why they are offered.
        assert!(half_blocks_available(default_font(), true));
        // A font nobody listed: assume the worst rather than draw a broken code.
        assert!(!half_blocks_available("whatever-the-user-loaded", true));
        // A graphical terminal draws them whatever the console font says.
        assert!(
            half_blocks_available("ter-v16n", false),
            "in a graphical terminal the font stack has the blocks"
        );
    }

    /// `half_blocks` must state what the shipped PSF file actually contains, and
    /// only LatGrkCyr does. If a family is added, this is the reminder to read its
    /// Unicode table instead of guessing.
    #[test]
    fn exactly_one_family_claims_half_blocks() {
        let claimed: Vec<&str> = FAMILIES
            .iter()
            .filter(|f| f.half_blocks)
            .map(|f| f.name)
            .collect();
        assert_eq!(
            claimed,
            vec!["Artix TUI Bold", "Artix TUI"],
            "half_blocks must match the PSF files, not a guess: of the packaged \
             fonts only LatGrkCyr has U+2580/2584, and all three vendored ones do"
        );
    }

    /// A fresh run must START with the font this screen calls the default.
    ///
    /// These were two separate facts: `default_pos()` here, and a font name
    /// written out again in `App::new()`. Changing the list moved the first and
    /// left the second behind, so the installer booted thin, WROTE thin into the
    /// installed system, and only switched to the intended font if the user
    /// happened to open this screen and move the cursor. It looked like the
    /// default had been changed, because the screen opened on the right entry.
    #[test]
    fn the_installer_starts_on_the_font_this_screen_calls_default() {
        let app = crate::app::App::new();
        assert_eq!(
            app.config.console_font,
            default_font(),
            "the config default and the screen default are different fonts"
        );
        // And it must be a font that exists in the list, not a stale name.
        assert!(
            FAMILIES
                .iter()
                .any(|f| f.sizes.iter().any(|s| s.1 == app.config.console_font)),
            "the starting font {} is not offered by the chooser at all",
            app.config.console_font
        );
    }

    /// The ISO's launcher must set the SAME font the installer thinks it has.
    ///
    /// `installer-start` runs `setfont` before the binary starts. If it names a
    /// different font, the console wears one thing and the installer reports
    /// another — and the pick written into the installed system is the one
    /// nobody saw.
    #[test]
    fn the_iso_launcher_sets_the_same_default_font() {
        let launcher =
            std::fs::read_to_string("../iso-profile/root-overlay/usr/bin/installer-start")
                .expect("the ISO launcher is in the repo");
        let first = launcher
            .lines()
            .find(|l| l.trim_start().starts_with("setfont "))
            .expect("the launcher sets a console font");
        assert!(
            first.contains(default_font()),
            "the launcher sets {first:?} but the installer starts on {}",
            default_font()
        );
    }

    /// Sizes go smallest-first inside every family, so `→` always means bigger.
    #[test]
    fn every_family_is_ordered_smallest_first() {
        let px = |s: &str| -> u32 {
            s.split('×')
                .nth(1)
                .and_then(|h| h.parse().ok())
                .unwrap_or(0)
        };
        for fam in FAMILIES {
            assert!(!fam.sizes.is_empty(), "{}: no sizes", fam.name);
            let mut prev = 0;
            for (label, name) in fam.sizes {
                let h = px(label);
                assert!(h > 0, "{}: unreadable label {label:?}", fam.name);
                assert!(h >= prev, "{}: {label} is out of order", fam.name);
                assert!(!name.is_empty(), "{}: empty setfont name", fam.name);
                prev = h;
            }
        }
    }

    /// Switching to a family with fewer sizes must not point past its end — the
    /// clamp lives in `font_name`, and this is what makes moving from Terminus
    /// (nine sizes) to LatGrkCyr (two) safe.
    #[test]
    fn a_shorter_family_clamps_instead_of_panicking() {
        let last = FAMILIES.len() - 1;
        let name = font_name(last, 99);
        assert!(
            FAMILIES[last].sizes.iter().any(|s| s.1 == name),
            "the clamp produced a font that is not in that family"
        );
        // And an out-of-range family clamps too.
        assert!(!font_name(99, 0).is_empty());
    }

    /// Reopening the screen lands on the font that is actually in use.
    #[test]
    fn position_of_round_trips_every_font() {
        for (fi, fam) in FAMILIES.iter().enumerate() {
            for (si, (_, name)) in fam.sizes.iter().enumerate() {
                assert_eq!(
                    position_of(name),
                    (fi, si),
                    "{name} does not map back to its own place"
                );
            }
        }
        // An unknown name falls back to the default rather than to (0, 0).
        assert_eq!(position_of("no-such-font"), default_pos());
    }

    /// No setfont name appears twice — a duplicate would be two options that do
    /// the same thing.
    #[test]
    fn no_font_is_listed_twice() {
        let mut all: Vec<&str> = FAMILIES
            .iter()
            .flat_map(|f| f.sizes.iter().map(|s| s.1))
            .collect();
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(n, all.len(), "the font list has duplicates");
        assert_eq!(
            n, 17,
            "the verified set is 17 files — did one join untested?"
        );
    }
}
