//! Screen dispatch. Every screen module exposes `draw`, `handle_key`, and may
//! optionally provide `tick` (background work) and `footer_hint` (custom keys).

pub mod widgets;

pub mod aur;
mod desktop;
mod disk;
mod finish;
mod kernel;
mod keyboard;
mod language;
mod mode;
pub mod options;
pub mod packages;
mod recovery;
pub(crate) mod storage;
pub mod summary;
pub(crate) mod timezone;
mod user;
pub mod wifi;
mod wifitest;

use crate::app::{App, Screen};
use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    match app.screen {
        Screen::Language => language::draw(f, app, area),
        Screen::Timezone => timezone::draw(f, app, area),
        Screen::Wifi => wifi::draw(f, app, area),
        Screen::Keyboard => keyboard::draw(f, app, area),
        Screen::Kernel => kernel::draw(f, app, area),
        Screen::Desktop => desktop::draw(f, app, area),
        Screen::Packages => packages::draw(f, app, area),
        Screen::Aur => aur::draw(f, app, area),
        Screen::Disk => disk::draw(f, app, area),
        Screen::Storage => storage::draw(f, app, area),
        Screen::User => user::draw(f, app, area),
        Screen::Security => options::draw(f, app, area),
        Screen::Options => options::draw(f, app, area),
        Screen::Summary => summary::draw(f, app, area),
        Screen::Finish => finish::draw(f, app, area),
        Screen::Mode => mode::draw(f, app, area),
        Screen::Recovery => recovery::draw(f, app, area),
        Screen::WifiTest => wifitest::draw(f, app, area),
    }
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match app.screen {
        Screen::Language => language::handle_key(app, key),
        Screen::Timezone => timezone::handle_key(app, key),
        Screen::Wifi => wifi::handle_key(app, key),
        Screen::Keyboard => keyboard::handle_key(app, key),
        Screen::Kernel => kernel::handle_key(app, key),
        Screen::Desktop => desktop::handle_key(app, key),
        Screen::Packages => packages::handle_key(app, key),
        Screen::Aur => aur::handle_key(app, key),
        Screen::Disk => disk::handle_key(app, key),
        Screen::Storage => storage::handle_key(app, key),
        Screen::User => user::handle_key(app, key),
        Screen::Security => options::handle_key(app, key),
        Screen::Options => options::handle_key(app, key),
        Screen::Summary => summary::handle_key(app, key),
        Screen::Finish => finish::handle_key(app, key),
        Screen::Mode => mode::handle_key(app, key),
        Screen::Recovery => recovery::handle_key(app, key),
        Screen::WifiTest => wifitest::handle_key(app, key),
    }
}

pub fn tick(app: &mut App) {
    match app.screen {
        Screen::Summary => summary::tick(app),
        Screen::Wifi => wifi::tick(app),
        Screen::Packages => packages::tick(app),
        Screen::Aur => aur::tick(app),
        _ => {}
    }
}

/// Optional per-screen footer hint override.
pub fn footer_hint(app: &App) -> Option<String> {
    match app.screen {
        Screen::Wifi => Some(wifi::footer_hint(app)),
        Screen::Keyboard => Some(keyboard::footer_hint(app)),
        Screen::Kernel => Some(kernel::footer_hint(app)),
        Screen::Desktop => Some(desktop::footer_hint(app)),
        Screen::Disk => Some(disk::footer_hint(app)),
        Screen::Storage => Some(storage::footer_hint(app)),
        Screen::Packages => Some(packages::footer_hint(app)),
        Screen::Aur => Some(aur::footer_hint(app)),
        Screen::Timezone => Some(timezone::footer_hint(app)),
        Screen::Summary => Some(summary::footer_hint(app)),
        Screen::User => Some(user::footer_hint(app)),
        Screen::Security => Some(options::footer_hint(app)),
        Screen::Options => Some(options::footer_hint(app)),
        Screen::Recovery => Some(recovery::footer_hint(app)),
        Screen::WifiTest => Some(wifitest::footer_hint(app)),
        _ => None,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Rendering and event tests, on ratatui's TestBackend.
//
// TestBackend draws into an in-memory buffer instead of a terminal, so a screen
// can be rendered and inspected in a unit test — no TTY, no PTY, no ISO. This
// is the standard way to test a ratatui app, and it catches a class of bug the
// install-plan tests structurally cannot: a panic inside draw(), a layout that
// underflows on a small console, a key that silently does nothing.
//
// Why it matters here specifically: this installer runs on a PHYSICAL CONSOLE
// from a live ISO. A panic in draw() is not a stack trace in a scrollback
// buffer — it's a dead machine mid-install, with no way back.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Screen};
    use crate::i18n::Lang;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    /// Render one screen into an off-screen buffer and hand back what it drew.
    ///
    /// TestBackend implements Display, so the whole rendered screen comes back
    /// as text — no cell-by-cell walk. Styles aren't compared: ratatui can't
    /// assert on colour yet (ratatui#1402), and content and layout are what
    /// these tests are about.
    fn render_at(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).expect("test backend");
        term.draw(|f| {
            let area = f.area();
            draw(f, app, area);
        })
        .expect("draw must not panic");
        term.backend().to_string()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // ── Rendering ─────────────────────────────────────────────────────────

    /// Every screen must render without panicking. An unwrap on a missing
    /// translation key, an index into an empty list, an arithmetic overflow in
    /// a Constraint — all of these are compile-clean and kill the installer at
    /// runtime, on the user's console, mid-install.
    #[test]
    fn every_screen_renders_without_panicking() {
        for screen in Screen::ALL {
            let mut app = App::new();
            app.screen = screen;
            let out = render_at(&mut app, 100, 30);
            assert!(
                !out.trim().is_empty(),
                "{screen:?} drew nothing at all — an empty screen is a bug, \
                 not a layout"
            );
        }
    }

    /// The same, in English. Half of this project is a second translation, and
    /// a key present in one TOML and missing from the other renders as a raw
    /// identifier — or panics, depending on the lookup.
    #[test]
    fn every_screen_renders_in_both_languages() {
        for screen in Screen::ALL {
            for lang in [Lang::Uk, Lang::En] {
                let mut app = App::new();
                app.lang = lang;
                app.screen = screen;
                let out = render_at(&mut app, 100, 30);
                assert!(
                    out.chars().any(|c| c.is_alphabetic()),
                    "{screen:?} rendered no text in {lang:?}"
                );
                // i18n::t() falls back to returning the KEY ITSELF when it
                // can't find a translation. So a missing key doesn't blow up —
                // it quietly prints "disk.title" on the screen, in a language
                // the author probably doesn't read. Catch it here.
                assert!(
                    !out.contains(".title") && !out.contains(".hint"),
                    "{screen:?} printed a raw i18n key in {lang:?} — \
                     that key is missing from the TOML"
                );
            }
        }
    }

    /// Console sizes are not a free choice: a live ISO on real hardware lands
    /// on whatever the firmware gives us. 80x24 is the floor we promise to
    /// support (MIN_COLS/MIN_ROWS), and the layout must survive it — Ratatui
    /// panics on a Constraint that doesn't fit, so this is a real failure mode,
    /// not a cosmetic one.
    #[test]
    fn screens_survive_the_smallest_supported_console() {
        for screen in Screen::ALL {
            let mut app = App::new();
            app.screen = screen;
            // 80x24 is the documented minimum; 200x60 is a maximised terminal.
            for (w, h) in [(80, 24), (120, 40), (200, 60)] {
                let out = render_at(&mut app, w, h);
                assert!(!out.trim().is_empty(), "{screen:?} drew nothing at {w}x{h}");
            }
        }
    }

    /// Rendering must not mutate the config.
    ///
    /// draw() takes &mut App because screens need it for scroll state — which
    /// makes it easy to *decide* something while merely painting. This test
    /// found two live instances of that on its first run:
    ///
    ///   * the timezone screen assigned config.timezone from the cursor on
    ///     EVERY FRAME. On a fresh screen the cursor is 0, so the very first
    ///     repaint overwrote the default (Europe/Kyiv) with whatever sorts
    ///     first alphabetically (Africa/Abidjan). The user's zone was gone
    ///     before they pressed a key.
    ///
    ///   * the disk screen did the same with config.disk — the disk that gets
    ///     WIPED, re-derived from a cursor on every repaint.
    ///
    /// Both now commit in the key handler, where a decision belongs. Rendering
    /// shows state; it never decides it.
    #[test]
    fn drawing_a_screen_does_not_change_the_install_config() {
        for screen in Screen::ALL {
            let mut app = App::new();
            app.screen = screen;
            let before = app.config.clone();
            let _ = render_at(&mut app, 100, 30);
            assert_eq!(
                app.config, before,
                "{screen:?} mutated InstallConfig while merely drawing"
            );
        }
    }

    /// The build number must be on the first screen, and must be the version
    /// that actually produced the binary.
    ///
    /// Without it, "the bug is still there" and "you're running an old build"
    /// are indistinguishable from either side of a bug report — and we burned a
    /// round trip on exactly that. env!() reads Cargo.toml at compile time, so
    /// the number on screen cannot lie about which source built the binary.
    #[test]
    fn the_language_screen_shows_the_build_number() {
        let mut app = App::new();
        app.screen = Screen::Language;
        let out = render_at(&mut app, 100, 30);

        let version = env!("CARGO_PKG_VERSION");
        assert!(
            out.contains(version),
            "the first screen must show build {version} — it's how a bug report \
             and a binary get matched up"
        );

        // It sits at the bottom, out of the way of the actual choice.
        let bottom = out.lines().rev().take(3).collect::<String>();
        assert!(
            bottom.contains(version),
            "the build number belongs at the bottom, not in the middle of the UI"
        );
    }

    /// It must not crowd out the wizard on a small console.
    #[test]
    fn the_build_number_does_not_break_the_smallest_console() {
        let mut app = App::new();
        app.screen = Screen::Language;
        // 80x24 is the documented floor.
        let out = render_at(&mut app, 80, 24);
        assert!(
            out.contains(env!("CARGO_PKG_VERSION")),
            "the build number must still fit at 80x24"
        );
        // And the language choice itself is still there.
        assert!(
            out.chars().any(|c| c.is_alphabetic()),
            "the wizard must still render"
        );
    }

    // ── Events ────────────────────────────────────────────────────────────

    /// Arrow keys must not walk the cursor off either end of a list. An index
    /// that runs past the end panics on the next draw; one that wraps to
    /// usize::MAX panics immediately. Both are one keypress away on a physical
    /// console, and both kill the installer.
    #[test]
    fn arrow_keys_cannot_push_the_cursor_out_of_bounds() {
        for screen in Screen::ALL {
            let mut app = App::new();
            app.screen = screen;

            // Hammer both directions well past any plausible list length.
            for _ in 0..50 {
                handle_key(&mut app, key(KeyCode::Up));
            }
            let _ = render_at(&mut app, 100, 30);

            for _ in 0..50 {
                handle_key(&mut app, key(KeyCode::Down));
            }
            // The assertion is the render: an out-of-range cursor panics inside
            // draw(), not inside the key handler.
            let _ = render_at(&mut app, 100, 30);
        }
    }

    /// Screens whose lists are populated in the background (Wi-Fi networks,
    /// package search, disks in a VM with none attached) get drawn BEFORE the
    /// data arrives. Indexing an empty list is the classic TUI panic, and here
    /// it would strand someone mid-install.
    #[test]
    fn screens_render_before_their_background_data_arrives() {
        for screen in [
            Screen::Wifi,
            Screen::Packages,
            Screen::Aur,
            Screen::Disk,
            Screen::Storage,
            Screen::Timezone,
            Screen::Keyboard,
        ] {
            let mut app = App::new();
            app.screen = screen;
            // Fresh App: every async list is still empty.
            let _ = render_at(&mut app, 100, 30);
            // And a cursor that's been moved before anything loaded.
            handle_key(&mut app, key(KeyCode::Down));
            handle_key(&mut app, key(KeyCode::Enter));
            let _ = render_at(&mut app, 100, 30);
        }
    }

    /// Enter, on any screen, must leave the app in a state that still renders.
    /// It's the one key pressed on every screen, and the one most likely to
    /// advance a cursor into a list that isn't there yet.
    #[test]
    fn enter_never_leaves_a_screen_unable_to_redraw() {
        for screen in Screen::ALL {
            let mut app = App::new();
            app.screen = screen;
            handle_key(&mut app, key(KeyCode::Enter));
            // Redraw after the keypress: the panic, if any, lands here.
            let _ = render_at(&mut app, 100, 30);
        }
    }

    /// Regression, found on a live binary: ticking zstd compression on a btrfs
    /// disk and then picking "all of /home" silently un-ticked it.
    ///
    /// The real cause was subtle, and my first fix missed it. `format` was
    /// DERIVED from the mountpoint:
    ///
    ///     e.format = !e.mountpoint.is_empty();
    ///
    /// The mountpoint picker cycles "" → home → homedisk. On the middle step
    /// `mount_base` is "home" but `mount_name` is still blank — the user hasn't
    /// typed a folder name — so sync_mountpoint() yields an EMPTY mountpoint.
    /// `format` flipped to false on the way past, taking `compress` with it. By
    /// the time "homedisk" restored the mountpoint, the flag was already gone.
    ///
    /// The lesson generalises: cycling forward through one control must not
    /// destroy state that belongs to a DIFFERENT control. The filesystem strip
    /// decides `format` — explicitly, in one place — and nothing else recomputes
    /// it behind the user's back.
    #[test]
    fn cycling_the_mountpoint_does_not_eat_the_compression_choice() {
        use crate::app::ExtraDisk;
        use crate::screens::storage::{step_base, sync_compress};

        // An empty disk, formatted btrfs, compression ticked.
        let mut e = ExtraDisk {
            disk: "/dev/sdb".into(),
            fs: "btrfs".into(),
            format: true,
            whole_disk: true,
            compress: true,
            ..Default::default()
        };

        // Cycle the mountpoint forward: "" → "home".
        // This is the step that used to kill it: no folder name typed yet, so
        // the mountpoint is empty here.
        step_base(&mut e, 1);
        assert!(
            e.compress,
            "compression must survive an intermediate step of the mountpoint \
             cycle — the mountpoint being blank mid-cycle is not a decision"
        );

        // → "homedisk" — the choice the user was actually reaching for.
        step_base(&mut e, 1);
        assert_eq!(e.mountpoint, "/home");
        assert!(e.compress, "compression must still be ticked at /home");

        // The cases where dropping it IS correct:
        e.fs = "ext4".into();
        sync_compress(&mut e);
        assert!(!e.compress, "zstd is a btrfs option — meaningless on ext4");

        e.fs = "btrfs".into();
        e.compress = true;
        e.format = false;
        sync_compress(&mut e);
        assert!(!e.compress, "a disk we don't format has no mount options");
    }

    /// Esc walks back one screen, and never off the front of the wizard.
    #[test]
    fn esc_never_walks_off_the_first_screen() {
        let mut app = App::new();
        app.screen = Screen::Language;
        app.goto_prev();
        assert_eq!(
            app.screen,
            Screen::Language,
            "there is nothing before the first screen"
        );
    }

    /// Regression: walking back into the desktop screen must hand it back in a
    /// first-visit state. goto_prev() used to reset only `cursor`, leaving
    /// de_focus at 1 — so Enter took the "already past the DE list" branch and
    /// jumped forward, making the desktop and seat choices unreachable once
    /// made. Navigation resets every per-screen focus for exactly this reason.
    #[test]
    fn navigating_back_clears_per_screen_focus() {
        let mut app = App::new();
        app.screen = Screen::Desktop;
        app.de_focus = 1; // as if the seat modal had already been confirmed
        app.seat_modal_open = true;

        app.goto_prev();

        assert_eq!(app.de_focus, 0, "de_focus must be reset when leaving");
        assert!(
            !app.seat_modal_open,
            "a modal must not stay flagged open across a screen change"
        );
    }

    /// A modal left open would render over whatever screen we land on.
    #[test]
    fn navigating_forward_also_clears_focus_and_modals() {
        let mut app = App::new();
        app.screen = Screen::Storage;
        app.can_advance = true;
        app.storage_opts_modal_open = true;
        app.disk_focus = 3;

        app.goto_next();

        assert!(!app.storage_opts_modal_open, "modal must close on advance");
        assert_eq!(
            app.disk_focus, 0,
            "focus must not leak into the next screen"
        );
    }

    /// The wizard is a straight line: next() from the last screen stays put
    /// rather than wrapping around to the first (and re-running the install).
    #[test]
    fn the_wizard_does_not_wrap_around_at_the_end() {
        let mut app = App::new();
        app.screen = Screen::Finish;
        app.can_advance = true;
        app.goto_next();
        assert_eq!(
            app.screen,
            Screen::Finish,
            "Finish must not advance into a second install"
        );
    }

    /// goto_next() honours can_advance: screens gate it on their own validation
    /// (an empty username, mismatched passwords), and a screen that says "not
    /// yet" must not be walked past.
    #[test]
    fn advancing_is_refused_when_the_screen_says_it_is_not_ready() {
        let mut app = App::new();
        app.screen = Screen::User;
        app.can_advance = false;
        app.goto_next();
        assert_eq!(
            app.screen,
            Screen::User,
            "a screen that isn't ready must not be left"
        );
    }
}
