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
/// One row of the mode menu, as a THING rather than a number.
///
/// It was an array of keys matched by index in `handle_key` — `if cursor == 4`.
/// That already cost one bug (a hardcoded bound made everything past the fifth
/// row unreachable), and it makes an optional row impossible: hiding one would
/// silently renumber every row after it. Now the list is built for the build,
/// and each row says what it is.
#[derive(Clone, Copy, PartialEq, Eq)]
// The two developer rows are not built into a release, so their variants are
// unconstructed there. Kept in the enum regardless: one list of what a mode row
// can be, whatever this build offers, is easier to read than two.
#[cfg_attr(not(feature = "devtools"), allow(dead_code))]
pub(crate) enum Item {
    Install,
    Recovery,
    WifiTest,
    Tbw,
    Font,
    Reboot,
    Firmware,
    Test,
}

impl Item {
    fn key(self) -> &'static str {
        match self {
            Item::Install => "mode.install",
            Item::Recovery => "mode.recovery",
            Item::WifiTest => "mode.wifitest",
            Item::Tbw => "mode.tbw",
            Item::Font => "mode.font",
            Item::Reboot => "mode.reboot",
            Item::Firmware => "mode.firmware",
            Item::Test => "mode.test",
        }
    }
}

/// The rows this build actually offers.
///
/// The Wi-Fi simulator and the deliberate-breakage "Test" row exist to test
/// THIS INSTALLER and are compiled in only under the `devtools` feature. A
/// published build has neither: one is noise to somebody installing Artix, and
/// the other ends a real install by wrecking it.
pub(crate) fn items() -> Vec<Item> {
    let mut v = vec![Item::Install, Item::Recovery];
    #[cfg(feature = "devtools")]
    v.push(Item::WifiTest);
    // Drive wear stays in every build: reading a disk's SMART write total is
    // something a person choosing where to install genuinely wants.
    v.push(Item::Tbw);
    v.extend([Item::Font, Item::Reboot, Item::Firmware]);
    #[cfg(feature = "devtools")]
    v.push(Item::Test);
    v
}

/// What "Test" will break, in the order the strip cycles.
#[cfg(feature = "devtools")]
pub(crate) const TEST_SCENARIOS: [&str; 3] =
    ["mode.test_fstab", "mode.test_boot", "mode.test_both"];

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

    let rows_list = items();
    let labels: Vec<String> = rows_list
        .iter()
        .map(|it| {
            #[cfg(feature = "devtools")]
            if *it == Item::Test {
                // The chosen sabotage is spelled out on the row itself, with the
                // alternatives beside it — the same reveal strip the partition
                // editor uses, and for the same reason: a switch must not hide
                // what it switches to. Doubly so here, where the choice decides
                // which way the machine will be broken.
                let strip: Vec<String> = TEST_SCENARIOS
                    .iter()
                    .enumerate()
                    .map(|(j, sk)| {
                        let name = t(app.lang, sk);
                        if j == app.test_scenario {
                            format!("<{name}>")
                        } else {
                            name
                        }
                    })
                    .collect();
                return format!("  {}  {}", t(app.lang, it.key()), strip.join(" \u{b7} "));
            }
            format!("  {}", t(app.lang, it.key()))
        })
        .collect();
    let items = labels;
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
    let rows_list = items();
    let cur = rows_list
        .get(app.mode_cursor)
        .copied()
        .unwrap_or(Item::Install);
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.mode_cursor = app.mode_cursor.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            app.mode_cursor = (app.mode_cursor + 1).min(rows_list.len() - 1)
        }
        // The sabotage picker rides on its own row, like every other reveal
        // strip in this installer: the choice is visible instead of hidden
        // behind a key nobody thinks to press.
        #[cfg(feature = "devtools")]
        KeyCode::Left | KeyCode::Right if cur == Item::Test => {
            let n = TEST_SCENARIOS.len();
            app.test_scenario = if matches!(key.code, KeyCode::Right) {
                (app.test_scenario + 1) % n
            } else {
                (app.test_scenario + n - 1) % n
            };
        }
        KeyCode::Enter => match cur {
            // Install: enter the normal flow at its first post-language step —
            // whichever that IS.
            //
            // This named Timezone outright, and naming it was a trap: moving the
            // keyboard step ahead of the timezone would have made this jump
            // straight over it, so choosing "Install" from the menu silently
            // skipped the layout while stepping back from Timezone still reached
            // it. A wizard whose first step depends on how you entered it is a
            // wizard with two different orders.
            Item::Install => {
                app.test_mode = false;
                app.goto(Screen::ALL[1]);
            }
            // A NORMAL INSTALL that ends by breaking one specific thing, so what
            // comes out is a genuine system in a known-bad state — which is what
            // recovery has to be tested against.
            Item::Test => {
                app.cursor = 0;
                app.goto(Screen::TestMenu);
            }
            Item::Recovery => app.goto(Screen::Recovery),
            // Wi-Fi test: bring up a simulated radio + access point so the
            // network screen can be exercised inside a VM with no wireless
            // hardware. Harmless elsewhere (the module just won't load).
            Item::WifiTest => {
                app.wifitest_log.clear();
                app.wifitest_running = false;
                app.goto(Screen::WifiTest);
            }
            // Drive wear (TBW): read each disk's SMART write total. The scan is
            // kicked off lazily the first time the screen draws.
            Item::Tbw => {
                app.tbwtest_scanned = false;
                app.tbwtest_running = false;
                app.tbwtest_rows.clear();
                app.goto(Screen::TbwTest);
            }
            // Console font: a bare TTY is this installer's first target, and
            // there the font decides whether the interface is legible at all.
            // Lands on the font currently in use.
            Item::Font => {
                let (fam, size) = crate::screens::fontpick::position_of(&app.config.console_font);
                app.font_family = fam;
                app.font_size_idx = size;
                app.font_focus = 0;
                app.goto(Screen::FontPick);
            }
            // Reboot. The confirmation is the menu itself: this entry does
            // nothing a power button would not.
            Item::Reboot => {
                app.pending_reboot = true;
                app.should_quit = true;
            }
            // Into the firmware. `efibootmgr` cannot ask for this and the kernel
            // can: a reboot flagged with the EFI "boot to firmware setup" bit
            // lands in the BIOS/UEFI menu. On a BIOS machine there is no such
            // bit, so it degrades to an ordinary reboot and says so.
            Item::Firmware => {
                app.pending_firmware = true;
                app.should_quit = true;
            }
        },
        // Esc leaves the menu for the language screen — the one place in this
        // installer where quitting is offered. The mode menu is off the linear
        // wizard, so the global handler deliberately does not move it; this
        // screen owns its own way back, and a guard test checks that it has one.
        KeyCode::Esc => app.goto(Screen::Language),
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
        for _ in 0..items().len() * 2 {
            handle_key(&mut app, KeyEvent::from(KeyCode::Down));
        }
        assert_eq!(
            app.mode_cursor,
            items().len() - 1,
            "the cursor cannot reach the bottom of the menu"
        );
        // And back up again, all the way.
        for _ in 0..items().len() * 2 {
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
