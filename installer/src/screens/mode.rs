//! Mode chooser, shown right after the language screen and OUTSIDE the linear
//! 13-step install flow. Two choices: install the system (the normal flow) or
//! enter the recovery tool (mount an existing install + drop into a chroot).

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

/// The menu, in order. ONE list: the drawing and the cursor bound both read it.
///
/// They used to be separate — the items were built inline and the cursor was
/// clamped to a literal 4 — so adding an entry drew it and made it unreachable.
/// The same shape once hid half the recovery actions behind a `^= 1`.
///
/// LEAVING IS PART OF THE JOB: after an install the machine has to start the
/// system just written, and the only ways out were `q` on the first screen and
/// the power button. In a VM it matters more, because the ISO is still the boot
/// device and the firmware has to be reached to change that.
const ITEMS: [&str; 7] = [
    "mode.install",
    "mode.recovery",
    "mode.wifitest",
    "mode.tbw",
    "mode.font",
    "mode.reboot",
    "mode.firmware",
];

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // hint
            Constraint::Min(0),    // list
            Constraint::Length(3), // actions
        ])
        .spacing(1)
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            t(app.lang, "mode.hint"),
            theme::dim(),
        ))),
        rows[0],
    );

    let items: Vec<String> = ITEMS
        .iter()
        .map(|k| format!("  {}", t(app.lang, k)))
        .collect();
    widgets::select_list_scrolled(f, rows[1], &items, app.mode_cursor, app.marquee);

    widgets::action_row(
        f,
        rows[2],
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        true,
    );
    app.can_advance = true;
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.mode_cursor = app.mode_cursor.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            app.mode_cursor = (app.mode_cursor + 1).min(ITEMS.len() - 1)
        }
        KeyCode::Enter => {
            if app.mode_cursor == 0 {
                // Install: enter the normal flow at its first post-language
                // step — whichever that IS.
                //
                // This named Timezone outright, and naming it was a trap: moving
                // the keyboard step ahead of the timezone would have made this
                // jump straight over it, so choosing "Install" from the menu
                // silently skipped the layout while stepping back from Timezone
                // still reached it. A wizard whose first step depends on how you
                // entered it is a wizard with two different orders.
                app.goto(Screen::ALL[1]);
            } else if app.mode_cursor == 2 {
                // Wi-Fi test: bring up a simulated radio + access point so the
                // network screen can be exercised inside a VM with no wireless
                // hardware. Harmless elsewhere (the module just won't load).
                app.wifitest_log.clear();
                app.wifitest_running = false;
                app.goto(Screen::WifiTest);
            } else if app.mode_cursor == 4 {
                // Console font: a bare TTY is this installer's first target, and
                // there the font decides whether the interface is legible at
                // all. Lands on the font currently in use.
                let (fam, size) = crate::screens::fontpick::position_of(&app.config.console_font);
                app.font_family = fam;
                app.font_size_idx = size;
                app.font_focus = 0;
                app.goto(Screen::FontPick);
            } else if app.mode_cursor == 3 {
                // Drive wear (TBW): read each disk's SMART write total. The scan
                // is kicked off lazily the first time the screen draws.
                app.tbwtest_scanned = false;
                app.tbwtest_running = false;
                app.tbwtest_rows.clear();
                app.goto(Screen::TbwTest);
            } else if app.mode_cursor == 5 {
                // Reboot. The confirmation is the menu itself: this entry does
                // nothing that a power button would not, and it is reached only
                // by choosing it deliberately from a five-item list.
                app.pending_reboot = true;
                app.should_quit = true;
            } else if app.mode_cursor == 6 {
                // Into the firmware. `efibootmgr` cannot ask for this, and the
                // kernel can: a reboot flagged with the EFI "boot to firmware
                // setup" bit lands in the BIOS/UEFI menu instead of booting.
                // On a BIOS machine there is no such bit, so it degrades to an
                // ordinary reboot and says so.
                app.pending_firmware = true;
                app.should_quit = true;
            } else {
                // Recovery: jump to the recovery tool and start a fresh scan.
                app.recovery_focus = 0;
                app.recovery_unlock = 0;
                app.recovery_passphrase.clear();
                app.recovery_status.clear();
                app.recovery_mounted = false;
                app.goto(Screen::Recovery);
            }
        }
        KeyCode::Esc => {
            // Back to the language screen.
            app.goto(Screen::Language);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    /// "Install" enters at whatever the first post-language step IS.
    ///
    /// It used to name Timezone outright. Moving the keyboard step ahead of it
    /// would then have made this jump straight over the layout — while stepping
    /// back from Timezone still reached it, so the wizard would have had two
    /// different orders depending on how you got in.
    /// EVERY ITEM IN THE MENU MUST BE REACHABLE.
    ///
    /// The list was built inline while the cursor was clamped to a literal 4,
    /// so the two entries added below it drew fine and could never be selected:
    /// the cursor simply stopped, with nothing on screen explaining why. Both
    /// now read the same array, and this walks the whole way down to prove it.
    #[test]
    fn the_cursor_reaches_the_last_menu_item() {
        let mut app = App::new();
        app.mode_cursor = 0;
        for _ in 0..ITEMS.len() * 2 {
            handle_key(&mut app, KeyEvent::from(KeyCode::Down));
        }
        assert_eq!(
            app.mode_cursor,
            ITEMS.len() - 1,
            "the cursor cannot reach the bottom of the menu"
        );
        // And back up again, all the way.
        for _ in 0..ITEMS.len() * 2 {
            handle_key(&mut app, KeyEvent::from(KeyCode::Up));
        }
        assert_eq!(app.mode_cursor, 0);
    }

    #[test]
    fn install_enters_at_the_first_step_whatever_it_is() {
        let mut app = App::new();
        app.goto(Screen::Mode);
        app.mode_cursor = 0;
        handle_key(&mut app, KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.screen, Screen::ALL[1]);
        assert_ne!(
            app.screen,
            Screen::Language,
            "it re-entered the language step"
        );
    }

    /// The layout is settled BEFORE anything asks for text. The timezone filter
    /// and the Wi-Fi password are both typed into, and both used to come first.
    #[test]
    fn the_keyboard_step_comes_before_every_field_you_type_into() {
        let pos = |s: Screen| Screen::ALL.iter().position(|x| *x == s).unwrap();
        assert!(
            pos(Screen::Keyboard) < pos(Screen::Timezone),
            "the timezone filter is typed into before the layout is chosen"
        );
        assert!(
            pos(Screen::Keyboard) < pos(Screen::Wifi),
            "the Wi-Fi password is typed on a layout nobody has confirmed"
        );
        assert!(
            pos(Screen::Language) < pos(Screen::Keyboard),
            "the layout is offered before the language it is labelled in"
        );
    }
}
