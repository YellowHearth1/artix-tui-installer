//! Keyboard handling. Global keys (quit, Tab/Shift-Tab navigation) are handled
//! here; everything else is delegated to the active screen so each screen owns
//! its own input semantics.

use crate::app::{App, Screen};
use crate::screens;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;

pub fn handle(app: &mut App) -> Result<()> {
    // Poll so the UI can also react to background work (e.g. install log
    // streaming) on the Summary screen without blocking forever.
    if !event::poll(Duration::from_millis(100))? {
        // Give the active screen a chance to pump background state.
        screens::tick(app);
        // And move any label that is too wide to fit. This is the only clock the
        // interface has: the poll returns every 100 ms whether or not anything
        // happened, which is exactly the tick a marquee needs.
        app.tick_marquee();
        return Ok(());
    }

    if let Event::Key(key) = event::read()? {
        if key.kind != event::KeyEventKind::Press {
            return Ok(());
        }
        let key = normalize(key);
        if handle_global(app, key) {
            return Ok(());
        }
        screens::handle_key(app, key);
    }
    Ok(())
}

/// Straighten out what the console actually sent before anyone acts on it.
///
/// **Ctrl+H is Backspace.** Keycode 14 carries the BackSpace keysym (`^H`,
/// 0x08) in several stock console maps — `ua-utf` among them — where `us.map`
/// carries Delete (`^?`, 0x7f). crossterm reports 0x08 as `Char('h')` with
/// CONTROL, and every text field in this installer matches `Char(c)` without
/// looking at modifiers. So on those consoles Backspace did not erase: it
/// TYPED. Reported from the timezone filter as "I press Backspace and it writes
/// hhhhhhh" — and the same key was going into password fields.
///
/// The installed system gets a keymap wrapper that pins keycode 14 back to
/// Delete, but the live ISO has no such thing, and neither does anyone else's
/// console. Fixing it here fixes it for every screen at once, on any keymap.
///
/// **A control chord is never text.** Ctrl+anything else is stripped of its
/// character so it cannot be typed into a field either; the chords the global
/// layer wants (Ctrl+C) match on the code, which is untouched.
fn normalize(key: KeyEvent) -> KeyEvent {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return key;
    }
    match key.code {
        KeyCode::Char('h') | KeyCode::Char('H') => {
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)
        }
        _ => key,
    }
}

/// Returns true if the key was consumed globally.
/// The global key layer, applied before the active screen sees a key.
///
/// `pub(crate)` rather than private so it can be tested directly: `handle()`
/// above reads from the terminal, which a unit test has no way to feed. Keeping
/// the DECISION separate from the READING is what makes the 'q' rule testable
/// at all.
pub(crate) fn handle_global(app: &mut App, key: KeyEvent) -> bool {
    // Any keypress starts the label under the cursor from its beginning again:
    // joining a scrolling line halfway through is worse than not scrolling.
    app.reset_marquee();
    match (key.code, key.modifiers) {
        // q QUITS ONLY ON THE FIRST SCREEN, and Esc never quits at all.
        //
        // This used to be the other way round: q quit everywhere EXCEPT the
        // screens that capture text. So a stray q in the manual partition
        // editor — thirty keystrokes into a layout, on a screen whose other
        // letters are all commands — ended the session and took every choice
        // with it. There is no undo for that and no confirmation before it.
        //
        // Leaving is a decision, and the place to make it is the screen where
        // nothing has been decided yet. Everywhere else Esc walks back, one
        // step at a time, until it arrives there. Ctrl+C remains the universal
        // escape hatch, because a terminal program that ignores it is worse.
        (KeyCode::Char('q'), KeyModifiers::NONE) if matches!(app.screen, Screen::Language) => {
            app.should_quit = true;
            true
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
            true
        }
        // `=`/`+` and `-` step the CONSOLE FONT, and that is all they do.
        //
        // Everything on screen grows together, dialog included, because a Linux
        // VT has one font and no API gives a region of it a different glyph
        // size. Three attempts were made to split the two apart — a box that
        // grew on its own, a font that snapped back on close, a dimmed backdrop
        // so the reflow behind would not draw the eye. The user tried all three
        // and settled it: everything scales together, and the one thing that
        // actually matters is that a DIALOG STAYS READABLE at the largest size,
        // where the console has the fewest cells to give it. That is a sizing
        // problem, and it is solved in `widgets::modal_rect_fit` and in each
        // dialog's own layout, not here.
        //
        // These two keys sit together right of 0, and `=` already carries `+`
        // on every layout, so it works with or without Shift.
        (KeyCode::Char('=') | KeyCode::Char('+'), _) => {
            crate::screens::apply_console_font(app, 1);
            true
        }
        (KeyCode::Char('-') | KeyCode::Char('_'), _) => {
            crate::screens::apply_console_font(app, -1);
            true
        }
        // F2 reveals the password fields, on the two screens that have them.
        //
        // A function key because anything typeable would land IN the field. It
        // is global so both screens behave the same, and it is a no-op anywhere
        // else rather than a key that silently means different things.
        //
        // This exists because the keyboard layout is now chosen DURING the
        // install: typing a passphrase blind is a guess about which key makes
        // which character, and a wrong guess on the LUKS passphrase is only
        // discovered at the next boot, when the disk will not open.
        (KeyCode::F(2), _) => {
            if matches!(
                app.screen,
                Screen::User | Screen::Options | Screen::Security
            ) {
                app.show_secrets = !app.show_secrets;
                true
            } else {
                false
            }
        }
        // Universal "back": Esc and Shift+Tab go to the previous page from
        // anywhere. A screen-level modal owns Esc first (to close itself); Wi-Fi
        // steps back through its own sub-stages; everything else just leaves to
        // the previous page. Forward navigation is driven by each screen (Enter /
        // its action button). Stepping focus *up* through a screen's rows is done
        // with the Up arrow (see below) — Esc no longer does that.
        (KeyCode::Esc, _) => {
            // A screen-level modal (filesystem options, seat/login picker, …)
            // owns Esc so it can close itself. Without this, the global "back"
            // fires first and leaves the screen with the modal still flagged
            // open — so returning to the screen shows the stuck modal.
            if any_modal_open(app) {
                return false;
            }
            if app.screen.is_off_linear() {
                // Outside the linear flow: these screens own their Esc (Mode →
                // Language, the rest → Mode). goto_prev() has no previous step to
                // find here. This used to be a hand-written list of screens, and
                // the font screen was left off it — so Esc there fell through to
                // goto_prev(), which indexed past nav_cursor and killed the
                // process instead of going back to the menu.
                false
            } else if app.screen == Screen::Wifi
                && app.wifi_stage != crate::screens::wifi::Stage::Choose
            {
                false // let wifi step back through its own sub-stages
            } else if app.screen == Screen::Summary
                && app.install_phase == crate::screens::summary::Phase::Installing
            {
                // Never navigate away mid-install: the runner is busy and the
                // config must not change under it.
                true
            } else if app.screen == Screen::Summary
                && app.install_phase == crate::screens::summary::Phase::Failed
            {
                // After a FAILED install, reset the install state back to Review
                // (clearing the half-run plan/step) so the user can fix the
                // offending choice and run again — not be stuck on a dead screen.
                crate::screens::summary::reset_for_retry(app);
                app.goto_prev();
                true
            } else {
                app.goto_prev();
                true
            }
        }
        // Up moves focus within a screen; pressing it again while already on the
        // top item leaves to the previous page (so you can walk up out of a
        // screen). While a modal is open, Up moves the modal's selection instead.
        (KeyCode::Up, KeyModifiers::NONE) => {
            if any_modal_open(app) {
                return false;
            }
            if at_top(app) {
                app.goto_prev();
                true
            } else {
                false
            }
        }
        (KeyCode::BackTab, _) => {
            if any_modal_open(app) {
                return false;
            }
            if app.screen.is_off_linear() {
                return false; // same as Esc: the screen walks itself back
            }
            if app.screen == Screen::Summary
                && app.install_phase == crate::screens::summary::Phase::Installing
            {
                return true; // locked during install
            }
            app.goto_prev();
            true
        }
        _ => false,
    }
}

/// True when any screen-level modal overlay is open. While one is, the global
/// back/navigation keys defer to the active screen so the modal can handle them
/// (typically closing itself on Esc) instead of being bypassed.
fn any_modal_open(app: &App) -> bool {
    app.seat_modal_open
        || app.fs_opts_modal_open
        || app.storage_opts_modal_open
        || app.disk_warn_modal_open
        || app.confirm_format_open
        // Manual partition editor: the role picker and the create-partition
        // wizard own Esc (close / step back). Without them here the global
        // "back" fires first and ejects from the whole disk screen instead.
        || app.parts_modal_open
        || app.parts_create_open || app.parts_mount_open
        || app.parts_disk_modal_open
        // The disk-wipe modal owns Esc (step back / close); the "now" wipe
        // overlay owns it too so Esc cannot eject mid-erase.
        || app.parts_wipe_open
        || app.parts_wipe_ack_open
        || app.wipe_run_rx.is_some()
        || app.wipe_run_done.is_some()
        // The slayfetch logo picker on the packages screen owns Esc (close).
        || app.logo_modal_open
}

/// True when the current screen's focus is on its TOP item, so a further Up
/// should leave to the previous page rather than move within the screen. Each
/// screen tracks its own focus field; this reads the relevant one. Screens
/// where "top" is ambiguous or Up means something else (Summary scrolling,
/// Wi-Fi mid-stage) return false so their Up is left untouched.
fn at_top(app: &App) -> bool {
    match app.screen {
        Screen::Language | Screen::Timezone | Screen::Keyboard => app.cursor == 0,
        Screen::Kernel => app.kernel_cursor == 0,
        // Desktop has two stacked lists (environments, then login manager);
        // de_focus picks the list, cursor the row in it. The true top is the
        // first row of the FIRST list — otherwise Up within the list would
        // wrongly leave the page.
        Screen::Desktop => app.de_focus == 0 && app.cursor == 0,
        Screen::Aur => app.aur_cursor == 0,
        Screen::User => app.user_focus == 0,
        Screen::Options | Screen::Security => app.cursor == 0,
        Screen::Storage => app.storage_cursor == 0,
        Screen::Disk => {
            // Two faces of one step: the manual role list navigates app.cursor,
            // the auto picker its own disk_focus.
            if app.config.partition_mode.is_manual_family() {
                app.cursor == 0
            } else {
                app.disk_focus == 0
            }
        }
        // Packages has two sections; the very top is the GPU list's first row.
        Screen::Packages => {
            app.pkg_focus == crate::screens::packages::FOCUS_GPU && app.gpu_cursor == 0
        }
        // Wi-Fi: only the initial "use Wi-Fi?" choice counts as the top.
        Screen::Wifi => app.wifi_stage == crate::screens::wifi::Stage::Choose && app.cursor == 0,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> App {
        App::new()
    }

    // Regression guard for the desktop navigation bug: pressing Up in the
    // MIDDLE of the environments list must NOT leave the page. at_top is true
    // only at the first row of the first list (de_focus == 0 && cursor == 0).
    /// Esc on a screen outside the wizard must hand back to that screen, which
    /// walks itself to its own parent — never to the global `goto_prev`, which
    /// has no previous step here. Getting this wrong is not a mis-navigation:
    /// the font screen's Esc indexed past `nav_cursor` and the panic dropped the
    /// user out of the installer at the console login.
    /// LEAVING THE INSTALLER IS POSSIBLE ONLY WHERE NOTHING HAS BEEN DECIDED.
    ///
    /// `q` used to quit everywhere except the screens that capture text, which
    /// meant a stray q in the manual partition editor — thirty keystrokes into
    /// a layout, on a screen whose other letters are all commands — ended the
    /// session and took every choice with it. No confirmation, no undo.
    ///
    /// Esc must never quit anywhere: it walks back, one step at a time, until
    /// it reaches the first screen, and leaving is a decision made there.
    #[test]
    fn only_the_first_screen_can_be_quit_and_never_with_esc() {
        for s in Screen::ALL.iter().copied().chain([
            Screen::Mode,
            Screen::Recovery,
            Screen::WifiTest,
            Screen::TbwTest,
        ]) {
            let mut a = fresh();
            a.screen = s;
            handle_global(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert!(!a.should_quit, "Esc quit the installer on {s:?}");

            let mut b = fresh();
            b.screen = s;
            handle_global(
                &mut b,
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            );
            if s == Screen::Language {
                assert!(b.should_quit, "q must still leave from the language screen");
            } else {
                assert!(
                    !b.should_quit,
                    "q quit the installer on {s:?}, discarding everything chosen so far"
                );
            }
        }
    }

    #[test]
    fn esc_off_the_wizard_returns_to_the_menu_not_out_of_the_installer() {
        for s in [
            Screen::Mode,
            Screen::Recovery,
            Screen::WifiTest,
            Screen::TbwTest,
            Screen::FontPick,
        ] {
            let mut a = fresh();
            a.screen = s;
            let handled = handle_global(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert!(!handled, "{s:?} must own its own Esc");
            assert_eq!(a.screen, s, "the global handler moved off {s:?}");
            assert!(!a.should_quit, "Esc on {s:?} quit the installer");

            // And the screen's own handler is what takes it back.
            screens::handle_key(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert_ne!(a.screen, s, "{s:?} did not step back on Esc");
        }
    }

    #[test]
    fn at_top_desktop_only_first_row_first_list() {
        let mut a = fresh();
        a.screen = Screen::Desktop;
        a.de_focus = 0;
        a.cursor = 0;
        assert!(at_top(&a));
        a.cursor = 3;
        assert!(!at_top(&a));
        a.de_focus = 1;
        a.cursor = 0;
        assert!(!at_top(&a));
    }

    // Regression guard for the disk screen: only the boot-mode row (area 0) is
    // the page top; navigating the disk list (area 1) is not.
    #[test]
    fn at_top_disk_only_boot_row() {
        let mut a = fresh();
        a.screen = Screen::Disk;
        a.disk_focus = 0;
        assert!(at_top(&a));
        a.disk_focus = 1;
        assert!(!at_top(&a));
    }

    #[test]
    fn at_top_plain_list_uses_cursor() {
        let mut a = fresh();
        a.screen = Screen::Language;
        a.cursor = 0;
        assert!(at_top(&a));
        a.cursor = 1;
        assert!(!at_top(&a));
    }

    #[test]
    fn at_top_packages_two_sections() {
        let mut a = fresh();
        a.screen = Screen::Packages;
        a.pkg_focus = crate::screens::packages::FOCUS_GPU;
        a.gpu_cursor = 0;
        assert!(at_top(&a));
        a.gpu_cursor = 2;
        assert!(!at_top(&a));
    }

    fn plain(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// Regression: 'q' used to quit from ANY screen, including text fields.
    /// Typing a LUKS passphrase with a "q" in it — or a username like
    /// "quentin" — destroyed every choice made so far, with no confirmation.
    /// Screens that take text input must treat 'q' as input.
    #[test]
    fn q_does_not_quit_from_a_screen_with_a_text_field() {
        for screen in [
            Screen::Summary,
            Screen::Packages,
            Screen::Aur,
            Screen::User,
            Screen::Timezone,
            Screen::Keyboard,
            Screen::Wifi,
            Screen::Options,
        ] {
            let mut app = App::new();
            app.screen = screen;
            handle_global(&mut app, plain('q'));
            assert!(
                !app.should_quit,
                "{screen:?} has a text field — 'q' there is a letter, not a command"
            );
        }
    }

    /// On screens with no text input, 'q' is still the quick way out.
    #[test]
    fn q_still_quits_from_a_screen_without_text_input() {
        let mut app = App::new();
        app.screen = Screen::Language;
        handle_global(&mut app, plain('q'));
        assert!(
            app.should_quit,
            "'q' must still work where nothing is typed"
        );
    }

    /// Ctrl+C is the universal escape hatch and works everywhere — including
    /// the screens that swallow 'q'. Without it, a text screen would have no
    /// way out at all.
    #[test]
    fn ctrl_c_quits_from_everywhere() {
        for screen in Screen::ALL {
            let mut app = App::new();
            app.screen = screen;
            handle_global(
                &mut app,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            );
            assert!(
                app.should_quit,
                "{screen:?}: Ctrl+C must always quit — it's the last resort"
            );
        }
    }

    /// Esc closes an open modal instead of leaving the screen. Otherwise a
    /// modal becomes a trap: the only way to dismiss it also throws away the
    /// screen behind it.
    #[test]
    fn esc_closes_a_modal_rather_than_leaving_the_screen() {
        let mut app = App::new();
        app.screen = Screen::Desktop;
        app.seat_modal_open = true;

        handle_global(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(
            app.screen,
            Screen::Desktop,
            "Esc with a modal open must not walk back a screen"
        );
    }

    /// The manual partition editor's own modals (role picker, create-partition
    /// wizard) must own Esc too — otherwise Esc ejects from the whole disk
    /// screen instead of closing the modal, discarding the layout in progress.
    #[test]
    fn esc_defers_to_the_manual_partition_modals() {
        for open in [
            |a: &mut App| a.parts_modal_open = true,
            |a: &mut App| a.parts_create_open = true,
        ] {
            let mut app = App::new();
            app.screen = Screen::Disk;
            app.config.partition_mode = crate::app::PartitionMode::Manual;
            open(&mut app);

            let consumed = handle_global(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

            assert!(
                !consumed,
                "Esc must defer to the open manual-editor modal, not be consumed globally"
            );
            assert_eq!(
                app.screen,
                Screen::Disk,
                "Esc with a manual-editor modal open must not walk back a screen"
            );
        }
    }
}
#[cfg(test)]
mod resize_keys {
    use super::*;
    use crate::app::App;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(app: &mut App, ch: char) -> bool {
        handle_global(app, KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
    }

    /// F2 reveals a password, and leaving the screen hides it again.
    ///
    /// Revealing exists because the keyboard layout is now chosen during the
    /// install: a passphrase typed blind is a guess about which key makes which
    /// character, and a wrong guess on the LUKS one is discovered at the next
    /// boot, when the disk will not open and nothing can fix it.
    ///
    /// It must not survive the screen. A password left in clear text on a
    /// console someone else walks past is a different failure, and a worse one
    /// to discover.
    #[test]
    fn f2_reveals_a_password_and_leaving_hides_it_again() {
        let mut app = App::new();
        app.screen = Screen::User;
        assert!(!app.show_secrets, "passwords start hidden");

        let f2 = KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE);
        assert!(
            handle_global(&mut app, f2),
            "F2 was not handled on the user screen"
        );
        assert!(app.show_secrets, "F2 did not reveal the password");
        handle_global(&mut app, f2);
        assert!(!app.show_secrets, "F2 did not hide it again");

        // Walking away re-hides, whatever state it was left in.
        app.show_secrets = true;
        app.can_advance = true;
        app.goto_prev();
        assert!(
            !app.show_secrets,
            "a revealed password survived the screen it belongs to"
        );

        // And on a screen with no password it is not swallowed, so it stays
        // available for whatever else might want it.
        let mut app = App::new();
        app.screen = Screen::Desktop;
        assert!(
            !handle_global(&mut app, f2),
            "F2 was consumed on a screen with no password"
        );
        assert!(!app.show_secrets);
    }

    /// The size keys grow the DIALOG when one is open, and never the installer
    /// at the same time. That distinction is the whole feature: a key that
    /// enlarged both would only shuffle the dialog's borders around.
    ///
    /// With nothing open they step the console font a size UP — inside the
    /// family the user chose. They used to jump to a fixed ladder of a different
    /// typeface entirely, so `+` swapped the face instead of enlarging it.
    ///
    /// With a dialog open the step is ALSO remembered, so every context menu
    /// afterwards opens at the size the user picked for menus. A box that grew
    /// while the letters inside it stayed small was rejected twice — the keys
    /// have to move the glyphs.
    #[test]
    fn plus_and_minus_resize_whichever_thing_is_in_front() {
        use crate::screens::fontpick;

        // NOTHING OPEN: the font moves, and nothing is remembered for menus.
        let mut app = App::new();
        assert!(!app.modal_open());
        let (fam_before, size_before) = fontpick::position_of(&app.config.console_font);
        press(&mut app, '=');
        let (fam, size) = fontpick::position_of(&app.config.console_font);
        assert_eq!(fam, fam_before, "+ changed the typeface, not the size");
        assert_eq!(size, size_before + 1, "the console font did not grow");
        press(&mut app, '-');
        assert_eq!(
            fontpick::position_of(&app.config.console_font).1,
            size_before,
            "minus did not step the font back down"
        );

        // A DIALOG OPEN: the letters grow AND the amount is kept, so the next
        // context menu opens at the same size without being asked again.
        let mut app = App::new();
        app.parts_modal_open = true;
        assert!(app.modal_open());
        let (_, before) = fontpick::position_of(&app.config.console_font);
        press(&mut app, '=');
        press(&mut app, '=');
        assert_eq!(
            fontpick::position_of(&app.config.console_font).1,
            before + 2,
            "the letters in the dialog did not get bigger"
        );
        press(&mut app, '-');
        assert_eq!(
            fontpick::position_of(&app.config.console_font).1,
            before + 1,
            "minus did not step the font back"
        );
    }

    /// Backspace erases even when the console sends Ctrl+H for it.
    ///
    /// Keycode 14 carries BackSpace (`^H`) in several stock console maps —
    /// `ua-utf` among them — and crossterm reports that as Char('h') with
    /// CONTROL. Every text field here matches Char(c) without looking at
    /// modifiers, so Backspace did not erase: it TYPED. Reported from the
    /// timezone filter as "I press Backspace and it writes hhhhhhh", and the
    /// same key was going into password fields.
    #[test]
    fn ctrl_h_is_backspace_because_some_consoles_send_it() {
        let mut app = App::new();
        app.goto(Screen::Timezone);
        for c in ['k', 'y', 'i'] {
            screens::handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            );
        }
        assert_eq!(app.tz_query, "kyi");

        // What the console actually sends for Backspace on such a keymap.
        let ctrl_h = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
        let fixed = normalize(ctrl_h);
        assert_eq!(
            fixed.code,
            KeyCode::Backspace,
            "Ctrl+H was not straightened"
        );
        screens::handle_key(&mut app, fixed);
        assert_eq!(app.tz_query, "ky", "Backspace typed instead of erasing");

        // And a plain Backspace still works, on consoles that send ^?.
        screens::handle_key(&mut app, KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.tz_query, "k");
    }

    /// Every dialog flag is reachable through `modal_open`. A flag left out
    /// would silently resize the installer while a dialog sat on top of it.
    #[test]
    fn every_dialog_flag_counts_as_a_dialog() {
        type Set = fn(&mut App);
        let flags: Vec<(&str, Set)> = vec![
            ("confirm_format_open", |a| a.confirm_format_open = true),
            ("disk_warn_modal_open", |a| a.disk_warn_modal_open = true),
            ("fs_opts_modal_open", |a| a.fs_opts_modal_open = true),
            ("logo_modal_open", |a| a.logo_modal_open = true),
            ("parts_create_open", |a| a.parts_create_open = true),
            ("parts_disk_modal_open", |a| a.parts_disk_modal_open = true),
            ("parts_modal_open", |a| a.parts_modal_open = true),
            ("parts_mount_open", |a| a.parts_mount_open = true),
            ("parts_wipe_ack_open", |a| a.parts_wipe_ack_open = true),
            ("parts_wipe_open", |a| a.parts_wipe_open = true),
            ("seat_modal_open", |a| a.seat_modal_open = true),
            ("storage_opts_modal_open", |a| {
                a.storage_opts_modal_open = true
            }),
        ];
        for (name, set) in flags {
            let mut app = App::new();
            set(&mut app);
            assert!(app.modal_open(), "{name} does not count as an open dialog");
        }
    }
}
