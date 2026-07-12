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
        return Ok(());
    }

    if let Event::Key(key) = event::read()? {
        if key.kind != event::KeyEventKind::Press {
            return Ok(());
        }
        if handle_global(app, key) {
            return Ok(());
        }
        screens::handle_key(app, key);
    }
    Ok(())
}

/// Returns true if the key was consumed globally.
fn handle_global(app: &mut App, key: KeyEvent) -> bool {
    match (key.code, key.modifiers) {
        // q quits, but not while installing on the Summary screen — there the
        // screen itself guards against quitting mid-install.
        (KeyCode::Char('q'), KeyModifiers::NONE) if app.screen != Screen::Summary => {
            app.should_quit = true;
            true
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
            true
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
            if app.screen == Screen::Mode || app.screen == Screen::Recovery {
                // Outside the linear flow: these screens own their Esc (Mode →
                // Language, Recovery → Mode). goto_prev() would index past ALL.
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
        Screen::Disk => app.disk_focus == 0,
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
}
