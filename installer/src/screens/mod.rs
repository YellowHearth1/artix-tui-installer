//! Screen dispatch. Every screen module exposes `draw`, `handle_key`, and may
//! optionally provide `tick` (background work) and `footer_hint` (custom keys).

pub mod widgets;

pub mod alongside;
pub mod aur;
mod desktop;
mod disk;
mod finish;
mod kernel;
mod keyboard;
mod language;
mod mode;
pub(crate) mod nav;
pub mod options;
pub mod packages;
pub mod parts;
mod pmode;
mod recovery;
pub(crate) mod storage;
pub mod summary;
pub(crate) mod tbwtest;
pub(crate) mod timezone;
mod user;
pub mod wifi;
mod wifitest;

use crate::app::{App, Screen};
use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

/// One wizard step's wiring: how it draws, how it takes keys, what runs on the
/// background tick, and what it contributes to the footer.
///
/// Dispatch used to be four parallel `match app.screen` blocks — draw, keys,
/// tick, footer hint. Adding a screen meant the same edit in four places, and
/// two of those matches had catch-all arms, so forgetting one didn't even fail
/// to compile: the screen just silently had no tick, or no hint. A step is now
/// ONE row, and the match in `step()` has no catch-all — a new `Screen` variant
/// that isn't wired up is a compile error in exactly one place.
struct Step {
    draw: fn(&mut Frame, &mut App, Rect),
    key: fn(&mut App, KeyEvent),
    tick: fn(&mut App),
    hint: fn(&App) -> Option<String>,
}

/// No background work — the default for screens that don't poll anything.
fn no_tick(_: &mut App) {}

fn step(screen: Screen) -> Step {
    match screen {
        Screen::Language => Step {
            draw: language::draw,
            key: language::handle_key,
            tick: no_tick,
            hint: |_| None,
        },
        Screen::Timezone => Step {
            draw: timezone::draw,
            key: timezone::handle_key,
            tick: no_tick,
            hint: |a| Some(timezone::footer_hint(a)),
        },
        Screen::Wifi => Step {
            draw: wifi::draw,
            key: wifi::handle_key,
            tick: wifi::tick,
            hint: |a| Some(wifi::footer_hint(a)),
        },
        Screen::Keyboard => Step {
            draw: keyboard::draw,
            key: keyboard::handle_key,
            tick: no_tick,
            hint: |a| Some(keyboard::footer_hint(a)),
        },
        Screen::Kernel => Step {
            draw: kernel::draw,
            key: kernel::handle_key,
            tick: no_tick,
            hint: |a| Some(kernel::footer_hint(a)),
        },
        Screen::Desktop => Step {
            draw: desktop::draw,
            key: desktop::handle_key,
            tick: no_tick,
            hint: |a| Some(desktop::footer_hint(a)),
        },
        Screen::Packages => Step {
            draw: packages::draw,
            key: packages::handle_key,
            tick: packages::tick,
            hint: |a| Some(packages::footer_hint(a)),
        },
        Screen::Aur => Step {
            draw: aur::draw,
            key: aur::handle_key,
            tick: aur::tick,
            hint: |a| Some(aur::footer_hint(a)),
        },
        Screen::PartitionMode => Step {
            draw: pmode::draw,
            key: pmode::handle_key,
            tick: no_tick,
            hint: |a| Some(pmode::footer_hint(a)),
        },
        Screen::Disk => Step {
            draw: disk::draw,
            key: disk::handle_key,
            tick: disk::tick,
            hint: |a| Some(disk::footer_hint(a)),
        },
        Screen::Storage => Step {
            draw: storage::draw,
            key: storage::handle_key,
            tick: no_tick,
            hint: |a| Some(storage::footer_hint(a)),
        },
        Screen::User => Step {
            draw: user::draw,
            key: user::handle_key,
            tick: no_tick,
            hint: |a| Some(user::footer_hint(a)),
        },
        // Security and Options are two wizard positions served by one module:
        // the same screen, reached from two places in the flow.
        Screen::Security | Screen::Options => Step {
            draw: options::draw,
            key: options::handle_key,
            tick: no_tick,
            hint: |a| Some(options::footer_hint(a)),
        },
        Screen::Summary => Step {
            draw: summary::draw,
            key: summary::handle_key,
            tick: summary::tick,
            hint: |a| Some(summary::footer_hint(a)),
        },
        Screen::Finish => Step {
            draw: finish::draw,
            key: finish::handle_key,
            tick: no_tick,
            hint: |_| None,
        },
        Screen::Mode => Step {
            draw: mode::draw,
            key: mode::handle_key,
            tick: no_tick,
            hint: |_| None,
        },
        Screen::Recovery => Step {
            draw: recovery::draw,
            key: recovery::handle_key,
            tick: no_tick,
            hint: |a| Some(recovery::footer_hint(a)),
        },
        Screen::WifiTest => Step {
            draw: wifitest::draw,
            key: wifitest::handle_key,
            tick: wifitest::tick,
            hint: |a| Some(wifitest::footer_hint(a)),
        },
        Screen::TbwTest => Step {
            draw: tbwtest::draw,
            key: tbwtest::handle_key,
            tick: tbwtest::tick,
            hint: |a| Some(tbwtest::footer_hint(a)),
        },
    }
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    (step(app.screen).draw)(f, app, area);
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    (step(app.screen).key)(app, key);
}

pub fn tick(app: &mut App) {
    (step(app.screen).tick)(app);
}

/// Optional per-screen footer hint override; the shell falls back to the
/// generic navigation hint when a screen has nothing specific to say.
pub fn footer_hint(app: &App) -> Option<String> {
    (step(app.screen).hint)(app)
}

// ═══════════════════════════════════════════════════════════════════════════
/// Plausible state for a screenshot: the choices a person would have made by
/// the time they reach a given step. Without it every screen renders empty and
/// the pictures show an installer nobody has touched.
///
/// Kept next to the screens rather than in the dumper so it stays honest — it
/// only sets fields the UI itself sets, never anything the user could not have
/// produced by pressing keys.
#[cfg(test)]
pub(crate) fn demo_state(app: &mut crate::app::App) {
    let c = &mut app.config;
    c.timezone = "Europe/Kyiv".into();
    c.keymap = "es".into();
    c.xkb_layouts = vec!["es".into()];
    c.kernel = crate::app::Kernel::Zen;
    c.desktops = vec!["KdePlasma".into()];
    c.display_manager = "sddm".into();
    c.hostname = "artix".into();
    c.username = "usuario".into();
    c.disk = "/dev/vda".into();
    c.root_fs = "btrfs".into();
    c.bootloader = crate::app::Bootloader::Grub;
    c.swap_gib = 4;
    c.optimize_mirrors = true;
    app.cursor = 0;
}

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
    use crate::i18n::{t, Lang};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    /// Render one screen into an off-screen buffer and hand back what it drew.
    ///
    /// TestBackend implements Display, so the whole rendered screen comes back
    /// as text — no cell-by-cell walk. Styles aren't compared: ratatui can't
    /// assert on colour yet (ratatui#1402), and content and layout are what
    /// these tests are about.
    pub(super) fn render_at(app: &mut App, w: u16, h: u16) -> String {
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

    /// The reported flow, driven end to end through the real key handlers.
    ///
    /// "Going back to the desktop step throws me to the DE choice instead of the
    /// login screen." Walk it exactly as a user does — pick a desktop, Enter
    /// through the seat picker onto the login-screen row, Enter to advance, then
    /// Esc back — and check where we land. Driving the global handler as well as
    /// the screen's matters: Esc is routed globally, and it was the interaction
    /// between the two that produced the complaint.
    #[test]
    fn the_desktop_step_hands_back_the_login_row_the_user_left_on() {
        let mut app = App::new();
        app.screen = Screen::Desktop;

        // Down to a desktop further in the list, then Enter → seat picker.
        handle_key(&mut app, key(KeyCode::Down));
        let chosen = app.cursor;
        assert!(chosen > 0, "the desktop list did not move");
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(app.seat_modal_open, "Enter should open the seat picker");

        // Confirm the seat → focus drops to the login-screen list.
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(
            app.de_focus, 1,
            "confirming the seat should land on the DM row"
        );

        // Enter there advances to the next step.
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(
            app.screen,
            Screen::Packages,
            "Enter on the DM row should advance"
        );

        // Esc walks back — routed by the GLOBAL handler, as in the real app.
        crate::event::handle_global(&mut app, key(KeyCode::Esc));
        assert_eq!(
            app.screen,
            Screen::Desktop,
            "Esc should return to the desktop step"
        );
        assert_eq!(
            app.de_focus, 1,
            "landed on the desktop list instead of the login-screen row we left from"
        );
        assert_eq!(
            app.cursor, chosen,
            "the desktop list forgot which desktop was highlighted"
        );
    }

    /// Returning to the desktop step must not strand the desktop/seat pickers.
    ///
    /// This is the hazard that made navigation clear the focus in the first
    /// place. Now that the step is handed back on the login-screen row, that row
    /// must have a way UP: Esc is the global "back a page" and Enter advances,
    /// so if ↑ also refused to move, the desktop list and the seat picker behind
    /// it would be unreachable on every return visit.
    #[test]
    fn the_login_screen_row_is_not_a_one_way_door() {
        let mut app = App::new();
        app.screen = Screen::Desktop;
        app.de_focus = 1; // as if returning from a later step

        // At the top of the login-screen list, Up steps back to the desktops.
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(
            app.de_focus, 0,
            "the login-screen row trapped the user: ↑ did not reach the desktop list"
        );

        // And from there Enter still opens the seat picker, so the middle step
        // of the flow is reachable again too.
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            app.seat_modal_open,
            "Enter on the desktop list must reopen the seat picker"
        );
    }

    /// A remembered row must never outlive the list it points into.
    ///
    /// Now that navigation hands a step back its old cursor, that row index can
    /// be STALE: lists here shrink under the user (the options screen drops the
    /// encryption rows when encryption is switched off, the package results
    /// change with the query, a rescan can return fewer partitions). A screen
    /// that indexes its list with an unclamped remembered row panics — and a
    /// panic here is a dead machine on a live console, not a stack trace.
    #[test]
    fn a_stale_remembered_row_cannot_blow_up_a_screen() {
        for screen in Screen::ALL {
            for lang in [Lang::Uk, Lang::En, Lang::Es] {
                let mut app = App::new();
                app.lang = lang;
                app.screen = screen;
                // Every per-screen row/focus field, parked far past the end of
                // any list this screen could have.
                app.cursor = 999;
                app.de_focus = 999;
                app.disk_focus = 999;
                app.pkg_focus = 999;
                app.user_focus = 999;
                app.storage_cursor = 999;
                app.gpu_cursor = 999;
                app.disk_cursor = 999;
                app.kernel_cursor = 999;
                app.summary_scroll = u16::MAX;
                let out = render_at(&mut app, 100, 30);
                assert!(
                    !out.trim().is_empty(),
                    "{screen:?} ({lang:?}) drew nothing with a stale cursor"
                );
                // And it must still take keys without panicking.
                for code in [KeyCode::Up, KeyCode::Down, KeyCode::Left, KeyCode::Right] {
                    handle_key(&mut app, key(code));
                }
            }
        }
    }

    /// The content panel an 80x24 console actually gives a screen.
    ///
    /// main::draw carves the terminal down before any screen sees it: a 1-cell
    /// outer margin, a 12-wide sidebar + 1 spacing, the panel's own border, and
    /// a 2/1 inner margin — plus a 3-row footer. So an 80x24 console leaves
    /// 59x15, not 80x24. Tests that render at the raw terminal size give every
    /// screen 9 rows it will never have, which is exactly how the Kernel, Disk
    /// and User screens shipped with layouts that clipped their own controls.
    const MIN_PANEL: (u16, u16) = (59, 15);

    /// On the smallest supported console, every screen that has a "Next" button
    /// must actually SHOW it.
    ///
    /// This is the regression guard for a whole class of bug: a screen whose
    /// fixed-height layout adds up to more rows than the panel has. ratatui does
    /// not complain — it silently clips the last constraints, so the button (and
    /// often the controls above it) just vanish. The installer runs on a live
    /// ISO on a physical console, where 80x24 is the NORMAL size, not an edge
    /// case; a user who cannot see the Next button is simply stuck.
    #[test]
    fn action_button_survives_the_smallest_console() {
        // Every screen that renders widgets::action_row.
        let with_button = [
            Screen::Language,
            Screen::Timezone,
            Screen::Wifi,
            Screen::Keyboard,
            Screen::Kernel,
            Screen::Packages,
            Screen::Aur,
            Screen::Disk,
            Screen::Storage,
            Screen::User,
            Screen::Options,
            Screen::Security,
        ];
        let (w, h) = MIN_PANEL;
        for lang in [Lang::Uk, Lang::En, Lang::Es] {
            for screen in with_button {
                let mut app = App::new();
                app.lang = lang;
                app.screen = screen;
                let out = render_at(&mut app, w, h);
                assert!(
                    out.contains("[Enter]"),
                    "{screen:?} ({lang:?}) hides its Next button at {w}x{h} — the \
                     panel an 80x24 console gives. Its layout asks for more rows \
                     than exist, so ratatui clipped the button away."
                );
            }
        }

        // Worst case for the Security screen: encryption armed grows the row
        // list (scope, USB key, passphrase) to roughly twice its resting size.
        // The account modes do the same to the User screen — a separate root
        // password adds two more fields.
        let mut app = App::new();
        app.screen = Screen::Security;
        app.config.encrypt_disk = true;
        let out = render_at(&mut app, w, h);
        assert!(
            out.contains("[Enter]"),
            "Security hides its Next button at {w}x{h} once encryption is armed"
        );

        for mode in crate::app::AccountMode::ALL {
            let mut app = App::new();
            app.screen = Screen::User;
            app.config.account_mode = mode;
            let out = render_at(&mut app, w, h);
            assert!(
                out.contains("[Enter]"),
                "User hides its Next button at {w}x{h} in account mode {mode:?}"
            );
        }
    }

    /// A step with nothing to decide is walked past, in both directions.
    ///
    /// In manual mode every drive can end up with a role, leaving the
    /// extra-disks step with an empty list. An empty screen reads as a broken
    /// step rather than an absent one, and it costs the user two keypresses to
    /// get through in each direction.
    #[test]
    fn an_empty_extra_disks_step_is_skipped_both_ways() {
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.can_advance = true;

        // Claim both fixture disks, so nothing is left over.
        app.config.manual_root_disk = "/dev/vda".into();
        app.config.manual_root = "/dev/vda1".into();
        app.config.manual_root_new_mib = crate::app::MANUAL_REST;
        app.config.manual_home_disk = "/dev/vdb".into();
        app.config.manual_home = "/dev/vdb1".into();
        app.config.manual_home_new_mib = crate::app::MANUAL_REST;

        // Forward: Security → (Storage is empty) → User.
        app.screen = Screen::Security;
        app.goto_next();
        assert_eq!(
            app.screen,
            Screen::User,
            "an empty extra-disks step was shown on the way forward"
        );

        // Backward: the same hole must be skipped, or Esc lands on it.
        app.can_advance = true;
        app.goto_prev();
        assert_eq!(
            app.screen,
            Screen::Security,
            "an empty extra-disks step was shown on the way back"
        );

        // With something left over, the step is shown as usual.
        let mut app = App::new();
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.can_advance = true;
        app.screen = Screen::Security;
        app.goto_next();
        assert_eq!(
            app.screen,
            Screen::Storage,
            "the extra-disks step vanished while it still had disks to offer"
        );
    }

    /// A disk the manual editor is carving is not spare storage, and /home
    /// cannot be handed out twice.
    ///
    /// Both are conflicts the plan cannot resolve: the extra-disks step would
    /// wipe a whole drive the editor is partitioning, and a second "the whole
    /// /home" mount would quietly shadow the /home the editor already assigned.
    #[test]
    fn the_extra_disks_step_avoids_conflicts_with_the_editor() {
        let mut app = App::new();
        app.screen = Screen::Storage;
        app.config.partition_mode = crate::app::PartitionMode::Manual;

        // /dev/vda has no partitions in the fixture, so it is normally offered
        // as a whole disk to format and mount.
        let before = render_at(&mut app, 100, 30);
        assert!(
            before.contains("/dev/vda"),
            "a blank disk should be offered as spare storage:\n{before}"
        );

        // Once the editor plans a partition there, it must disappear.
        app.config.manual_root_disk = "/dev/vda".into();
        app.config.manual_root = "/dev/vda1".into();
        app.config.manual_root_new_mib = crate::app::MANUAL_REST;
        let after = render_at(&mut app, 100, 30);
        assert!(
            !after.contains("/dev/vda"),
            "a disk the editor is carving was still offered as spare storage:\n{after}"
        );

        // With /home assigned in the editor, "the whole /home" is off the menu.
        app.config.manual_home = "/dev/vdb1".into();
        let opts = render_at(&mut app, 100, 30);
        assert!(
            !opts.contains(&t(app.lang, "storage.base_homedisk")),
            "a second /home was still on offer:\n{opts}"
        );

        // Without it, the option is back — the rule is about the conflict, not
        // about manual mode as such.
        app.config.manual_home.clear();
        let opts = render_at(&mut app, 100, 30);
        assert!(
            opts.contains(&t(app.lang, "storage.base_homedisk")),
            "the whole-/home option vanished with nothing claiming /home:\n{opts}"
        );
    }

    /// A partition the manual editor claimed must not also be offered as spare
    /// storage.
    ///
    /// The "Extra disks" step is where a drive becomes a plain folder for files
    /// — that part works, and the manual editor points at it. What did NOT work:
    /// it listed every partition it could see, including the root the editor had
    /// just been given. Accepting both would tell the plan to format one device
    /// two different ways.
    #[test]
    fn the_extra_disks_step_skips_partitions_the_editor_claimed() {
        use crate::app::DeletedPart;
        let mut app = App::new();
        app.screen = Screen::Storage;
        app.config.partition_mode = crate::app::PartitionMode::Manual;

        // Nothing claimed yet: the fixture partition is on offer.
        let free = render_at(&mut app, 100, 30);
        assert!(
            free.contains("/dev/vdb1"),
            "the extra-disks step should offer an unclaimed partition:\n{free}"
        );

        // Claimed as root — it must disappear from the offer.
        app.config.manual_root = "/dev/vdb1".into();
        let claimed = render_at(&mut app, 100, 30);
        assert!(
            !claimed.contains("/dev/vdb1"),
            "the root partition was offered as spare storage too:\n{claimed}"
        );

        // Same for one queued for deletion: deleting AND mounting it is
        // incoherent.
        app.config.manual_root.clear();
        app.config.manual_deleted = vec![DeletedPart {
            path: "/dev/vdb1".into(),
            disk: "/dev/vdb".into(),
            partuuid: "u".into(),
        }];
        let deleted = render_at(&mut app, 100, 30);
        assert!(
            !deleted.contains("/dev/vdb1"),
            "a partition queued for deletion was offered as storage:\n{deleted}"
        );
    }

    /// Every partition the installer will create appears in the final
    /// confirmation, with its size.
    ///
    /// That modal is the last thing between the user and an irreversible write,
    /// so anything it omits is something they authorise without seeing. The
    /// BIOS-boot partition was omitted outright, and sizes were shown nowhere —
    /// a "rest of the disk" root looked identical to a 1 GiB one.
    #[test]
    fn the_final_confirmation_lists_every_created_partition() {
        use crate::app::MANUAL_REST;
        let mut app = App::new();
        app.screen = Screen::Summary;
        app.config.partition_mode = crate::app::PartitionMode::Manual;
        app.confirm_format_open = true;
        app.config.boot_mode = crate::app::BootMode::Bios;
        let c = &mut app.config;
        c.manual_bios = "/dev/vda1".into();
        c.manual_bios_new_mib = 1;
        c.manual_root = "/dev/vda2".into();
        c.manual_root_new_mib = MANUAL_REST;
        c.manual_root_fs = "ext4".into();
        c.manual_swap = "/dev/vda3".into();
        c.manual_swap_new_mib = 2048;
        // A data partition carved on the second drive: it gets FORMATTED, so
        // the confirmation must name it like everything else being written to.
        c.manual_data_new.push(crate::app::DataPart {
            path: "/dev/vdb2".into(),
            disk: "/dev/vdb".into(),
            mib: 4096,
        });
        c.extra_disks.push(crate::app::ExtraDisk {
            disk: "/dev/vdb2".into(),
            mountpoint: "/mnt/data".into(),
            fs: "ext4".into(),
            format: true,
            whole_disk: false,
            ..Default::default()
        });

        let out = render_at(&mut app, 100, 34);
        for dev in ["/dev/vda1", "/dev/vda2", "/dev/vda3", "/dev/vdb2"] {
            assert!(
                out.contains(dev),
                "{dev} is created but never shown in the final confirmation:\n{out}"
            );
        }
        assert!(
            out.contains(&t(app.lang, "parts.biosboot")),
            "the BIOS-boot partition is unlabelled in the confirmation"
        );
        // Sizes: the fixed one by value, the rest-of-disk one by name.
        assert!(
            out.contains("2.0G"),
            "a created partition's size is missing:\n{out}"
        );
        assert!(
            out.contains(&t(app.lang, "parts.size_rest")),
            "a rest-of-disk partition does not say so"
        );
        // The data partition says where it mounts.
        assert!(
            out.contains("/mnt/data"),
            "the formatted data partition's mountpoint is not shown:\n{out}"
        );
        // A legacy install has no ESP — it must not be invented.
        let esp_lines = out.lines().filter(|l| l.contains("(ESP)")).count();
        assert_eq!(
            esp_lines, 0,
            "a phantom ESP was listed for a legacy install"
        );
    }

    /// The firmware choice must read as its OWN control, not a disk-list entry.
    ///
    /// It shipped as the first line inside the "Partitions" box, where a tester
    /// hit it and asked what the row even was and why they weren't going
    /// straight to the disks. It now has its own section — and that section
    /// must not cost the disk table so much room that the disks fall off the
    /// smallest supported panel.
    #[test]
    fn the_firmware_choice_is_separate_from_the_disk_list() {
        for (w, h) in [(100, 30), MIN_PANEL] {
            let mut app = App::new();
            app.screen = Screen::Disk;
            app.config.partition_mode = crate::app::PartitionMode::Manual;
            app.parts_scanned = true;
            let out = render_at(&mut app, w, h);

            let fw = t(app.lang, "parts.boot_label");
            let table = t(app.lang, "parts.tbl_title");
            assert!(out.contains(&fw), "no firmware control at {w}x{h}");
            assert!(out.contains(&table), "no partitions box at {w}x{h}");

            // The two must be on different lines: sharing one is exactly the
            // "it blends into the list" complaint.
            let fw_line = out.lines().position(|l| l.contains(&fw)).unwrap();
            let table_line = out.lines().position(|l| l.contains(&table)).unwrap();
            assert_ne!(
                fw_line, table_line,
                "the firmware control is inside the partitions box header at {w}x{h}"
            );
            assert!(
                fw_line < table_line,
                "the firmware control should come before the disk list at {w}x{h}"
            );
            // And the disks still fit underneath.
            assert!(
                out.contains("/dev/vda"),
                "the new section squeezed the disk list off the panel at {w}x{h}"
            );
        }
    }

    /// `slayfetch` wears its flag: the package name is painted letter by letter
    /// across the rainbow, so the entry says what it is before you read the
    /// description. The cursor highlight must NOT set a foreground colour, or it
    /// would repaint the whole name accent-cyan on whichever row it sits on —
    /// which is exactly what a plain `highlight_style(selected())` does.
    #[test]
    fn slayfetch_package_name_is_a_rainbow() {
        use ratatui::style::Color;
        let mut app = App::new();
        app.screen = Screen::Packages;
        let mut term = Terminal::new(TestBackend::new(100, 40)).expect("test backend");
        let name: Vec<&str> = vec!["s", "l", "a", "y", "f", "e", "t", "c", "h"];

        // Walk the cursor down the list. The colours are checked on every frame
        // the row is visible, and the run only counts as complete once it has
        // ALSO been seen with the cursor parked on it — that is the case a
        // foreground-setting highlight would break.
        let mut checked_highlighted = false;
        for _ in 0..60 {
            term.draw(|f| {
                let area = f.area();
                draw(f, &mut app, area);
            })
            .expect("draw must not panic");
            let buf = term.backend().buffer().clone();
            for y in 0..buf.area.height {
                // Index by CELL, not byte offset — the panel is full of
                // multi-byte box-drawing glyphs and Cyrillic text.
                let cells: Vec<&str> = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                let Some(x0) = cells.windows(9).position(|w| w == name.as_slice()) else {
                    continue;
                };
                let colours: Vec<Color> = (0..9).map(|i| buf[(x0 as u16 + i, y)].fg).collect();
                let distinct = {
                    let mut v = colours.clone();
                    v.dedup();
                    v.len()
                };
                let highlighted = cells[..x0].contains(&"\u{258e}");
                assert!(
                    distinct >= 5,
                    "slayfetch should run through the flag, but its letters used \
                     {distinct} colour run(s) (highlighted={highlighted}): {colours:?}"
                );
                assert!(
                    colours.iter().all(|c| matches!(c, Color::Rgb(..))),
                    "a letter lost its flag colour (highlighted={highlighted}) — the \
                     cursor highlight is setting a foreground again: {colours:?}"
                );
                if highlighted {
                    checked_highlighted = true;
                }
            }
            if checked_highlighted {
                return;
            }
            app.cursor += 1;
        }
        panic!("never saw the slayfetch row under the cursor in the package list");
    }

    /// The manual face of the Disk step renders in both languages, with the
    /// picker modal closed and open, at 100x30 and at the 80x24 minimum. New
    /// screen, same guarantee as the rest: a missing key or an index into an
    /// empty partition list dies HERE, not on a live console mid-install.
    #[test]
    fn manual_partitioning_screen_renders() {
        for lang in [Lang::Uk, Lang::En, Lang::Es] {
            let mut app = App::new();
            app.lang = lang;
            app.screen = Screen::Disk;
            app.config.partition_mode = crate::app::PartitionMode::Manual;
            // No lsblk in a test: pretend the scan already ran and found
            // nothing — the empty-list rendering is exactly what to prove.
            app.parts_scanned = true;
            for (w, h) in [(100, 30), (80, 24)] {
                let out = render_at(&mut app, w, h);
                assert!(
                    !out.trim().is_empty(),
                    "manual disk face drew nothing at {w}x{h}"
                );
            }
            app.parts_target = "/dev/sda1".into();
            app.parts_modal_open = true;
            let out = render_at(&mut app, 100, 30);
            assert!(!out.trim().is_empty(), "role modal drew nothing");
            app.parts_modal_open = false;

            // The create-partition wizard renders in both stages.
            app.parts_create_open = true;
            app.parts_create_stage = 0;
            let out = render_at(&mut app, 100, 30);
            assert!(!out.trim().is_empty(), "create-role stage drew nothing");
            app.parts_create_stage = 1;
            app.parts_size_input = "20".into();
            let out = render_at(&mut app, 80, 24);
            assert!(!out.trim().is_empty(), "create-size stage drew nothing");
            app.parts_create_open = false;

            // A transient status message must render in the footer, not panic.
            app.pmode_status = "test".into();
            let out = render_at(&mut app, 100, 30);
            assert!(!out.trim().is_empty(), "status message drew nothing");
            app.pmode_status.clear();
        }
    }

    /// The slayfetch logo picker renders in both languages, at the 80x24 floor,
    /// with a variant chosen — a missing key or an empty embed must die here.
    #[test]
    fn slayfetch_logo_picker_renders() {
        for lang in [Lang::Uk, Lang::En, Lang::Es] {
            let mut app = App::new();
            app.lang = lang;
            app.screen = Screen::Packages;
            app.config.extra_packages.push("slayfetch".into());
            app.config.fastfetch_logo = "Rainbow".into();
            app.logo_modal_open = true;
            for (w, h) in [(100, 30), (80, 24)] {
                let out = render_at(&mut app, w, h);
                assert!(
                    !out.trim().is_empty(),
                    "logo picker drew nothing at {w}x{h}"
                );
                assert!(
                    !out.contains("pkg.logo"),
                    "logo picker printed a raw i18n key in {lang:?}"
                );
            }
        }
    }

    /// The install-alongside face (a third face of the Disk step) renders in
    /// both languages and at the 80x24 floor, blocked (BIOS → needs UEFI) and
    /// not, with its disk picker open — a missing key or an empty scan must die
    /// here, not on a live console mid-install.
    #[test]
    fn alongside_screen_renders() {
        for lang in [Lang::Uk, Lang::En, Lang::Es] {
            let mut app = App::new();
            app.lang = lang;
            app.screen = Screen::Disk;
            app.config.partition_mode = crate::app::PartitionMode::Alongside;
            app.parts_scanned = true; // pretend the scan ran
            for (w, h) in [(100, 30), (80, 24)] {
                // BIOS: the "needs UEFI" blocked state.
                app.config.boot_mode = crate::app::BootMode::Bios;
                let out = render_at(&mut app, w, h);
                assert!(
                    !out.trim().is_empty(),
                    "alongside (blocked) drew nothing at {w}x{h}"
                );
                // UEFI: the normal plan state.
                app.config.boot_mode = crate::app::BootMode::Uefi;
                let out = render_at(&mut app, w, h);
                assert!(!out.trim().is_empty(), "alongside drew nothing at {w}x{h}");
                assert!(
                    !out.contains("along.") && !out.contains(".plan_title"),
                    "alongside printed a raw i18n key in {lang:?}"
                );
            }
            app.parts_disk_modal_open = true;
            let out = render_at(&mut app, 100, 30);
            assert!(!out.trim().is_empty(), "alongside disk picker drew nothing");

            // The create-partition wizard: both stages must render (an empty
            // partition list means "every role free", the widest role menu).
            app.parts_create_open = true;
            app.parts_create_stage = 0;
            let out = render_at(&mut app, 100, 30);
            assert!(!out.trim().is_empty(), "create-role stage drew nothing");
            app.parts_create_stage = 1;
            app.parts_create_role = 1; // root
            app.parts_size_input = "40".into();
            let out = render_at(&mut app, 80, 24);
            assert!(!out.trim().is_empty(), "create-size stage drew nothing");
        }
    }

    /// The same, in English. Half of this project is a second translation, and
    /// a key present in one TOML and missing from the other renders as a raw
    /// identifier — or panics, depending on the lookup.
    #[test]
    fn every_screen_renders_in_both_languages() {
        for screen in Screen::ALL {
            for lang in [Lang::Uk, Lang::En, Lang::Es] {
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
    fn navigating_closes_modals_in_both_directions() {
        // A modal left flagged open renders on top of whatever is drawn next,
        // so this must hold whichever way the user moves.
        let mut app = App::new();
        app.screen = Screen::Desktop;
        app.seat_modal_open = true;
        app.goto_prev();
        assert!(!app.seat_modal_open, "modal survived a step backwards");

        let mut app = App::new();
        app.screen = Screen::Storage;
        app.can_advance = true;
        app.storage_opts_modal_open = true;
        app.goto_next();
        assert!(
            !app.storage_opts_modal_open,
            "modal survived a step forward"
        );
    }

    /// A modal left open would render over whatever screen we land on.
    #[test]
    fn walking_back_into_a_step_lands_where_you_left_it() {
        // Reported from a live run: "going back to the desktop step throws me
        // to the DE choice instead of the login screen I was on". You LEAVE the
        // desktop step from the login-screen row (de_focus 1, where Enter
        // advances), so that is the row it has to hand back.
        let mut app = App::new();
        app.screen = Screen::Desktop;
        app.cursor = 3; // some desktop down the list
        app.de_focus = 1; // the login-screen row
        app.can_advance = true;

        app.goto_next(); // → Packages
        assert_eq!(app.cursor, 0, "a step not visited yet starts at its top");

        app.goto_prev(); // ← Desktop
        assert_eq!(app.de_focus, 1, "the desktop step forgot its focused row");
        assert_eq!(app.cursor, 3, "the desktop step forgot its cursor");
    }

    /// The other half of the contract: `cursor` is ONE field shared by a dozen
    /// screens, so remembering it must be per-screen. A row index from the
    /// desktop list must never show up as the highlighted row of another step.
    #[test]
    fn the_shared_cursor_does_not_leak_between_screens() {
        let mut app = App::new();
        app.screen = Screen::Desktop;
        app.cursor = 5;
        app.can_advance = true;

        app.goto_next(); // → Packages, never visited
        assert_eq!(app.cursor, 0, "desktop's row leaked into the package list");

        app.cursor = 2; // pick a row here
        app.goto_prev(); // ← Desktop
        assert_eq!(app.cursor, 5, "desktop got the package list's row");
        app.goto_next(); // → Packages again
        assert_eq!(app.cursor, 2, "the package list forgot its own row");
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

#[cfg(test)]
mod button_fit {
    use super::tests::render_at;
    use crate::app::{App, Screen};
    use crate::i18n::{t, Lang};

    /// The "next" button shows its whole label, in every language.
    ///
    /// It used to be a fixed 18 columns — enough for Ukrainian "Далі" and
    /// English "Next", one short of Spanish "Siguiente", which rendered as
    /// "[Enter] Siguient". Nothing failed; the border simply cut the word. The
    /// next language to arrive would have hit the same wall, so the button is
    /// sized to its text now and this checks that it stays that way.
    #[test]
    fn the_next_button_never_cuts_its_own_label() {
        for lang in [Lang::Uk, Lang::En, Lang::Es] {
            let label = t(lang, "app.next");
            for screen in [Screen::Language, Screen::Timezone, Screen::Kernel] {
                let mut app = App::new();
                app.lang = lang;
                app.screen = screen;
                let out = render_at(&mut app, 120, 34);
                assert!(
                    out.contains(&label),
                    "{lang:?}/{screen:?}: the button label {label:?} is clipped"
                );
            }
        }
    }
}

#[cfg(test)]
mod shots {
    use crate::app::{App, Screen};
    use crate::i18n::Lang;
    use ratatui::{backend::TestBackend, Terminal};
    use std::io::Write;

    /// Dump every wizard screen, in one language, as a cell grid a renderer can
    /// turn into an image: `x<TAB>y<TAB>char<TAB>fg<TAB>bg<TAB>bold`.
    ///
    /// Screenshots for the README have to exist in every language the installer
    /// speaks, and driving a VM through fifteen steps to photograph each one is
    /// both slow and impossible for the final screen, which only appears after a
    /// real install. The screens are pure functions of App state, so they can be
    /// rendered directly — every language, every step, including the last.
    ///
    /// Ignored by default: it writes files and is a tool, not a test. Run with
    ///   SHOT_LANG=es SHOT_DIR=/tmp/es cargo test dump_screens -- --ignored
    #[test]
    #[ignore]
    fn dump_screens() {
        let lang = match std::env::var("SHOT_LANG").unwrap_or_default().as_str() {
            "es" => Lang::Es,
            "en" => Lang::En,
            _ => Lang::Uk,
        };
        let dir = std::env::var("SHOT_DIR").unwrap_or_else(|_| "/tmp/shots".into());
        std::fs::create_dir_all(&dir).expect("mkdir");
        let (w, h) = (120u16, 34u16);

        for (i, screen) in Screen::ALL.iter().enumerate() {
            let mut app = App::new();
            app.lang = lang;
            app.screen = *screen;
            crate::screens::demo_state(&mut app);
            let mut term = Terminal::new(TestBackend::new(w, h)).expect("backend");
            // The whole UI, not just the step's panel: the sidebar with the
            // step numbers, the header and the footer are what make a
            // screenshot recognisable as this installer.
            term.draw(|f| crate::draw(f, &mut app)).expect("draw");
            let buf = term.backend().buffer().clone();
            let mut out = String::new();
            for y in 0..h {
                for x in 0..w {
                    let c = &buf[(x, y)];
                    out.push_str(&format!(
                        "{x}\t{y}\t{}\t{:?}\t{:?}\t{}\n",
                        c.symbol(),
                        c.fg,
                        c.bg,
                        c.modifier.contains(ratatui::style::Modifier::BOLD)
                    ));
                }
            }
            let path = format!("{dir}/{:02}.tsv", i + 1);
            let mut f = std::fs::File::create(&path).expect("create");
            f.write_all(out.as_bytes()).expect("write");
        }
        eprintln!("dumped {} screens to {dir}", Screen::ALL.len());
    }
}
