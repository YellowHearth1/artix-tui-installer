//! Unattended install scenarios, for testing this installer against itself.
//!
//! WHY THIS EXISTS. Recovery is the part that only matters on somebody's worst
//! day, and it shipped with a bootloader repair that had never once put a
//! bootloader back — because the only way to try it was to wait for a real
//! accident, or to click through the whole wizard by hand and then break the
//! result manually. Nobody does that ten times over three filesystems.
//!
//! So each row here is a COMPLETE install: every answer the wizard would ask
//! for is filled in, the install starts immediately, and it ends in a known
//! state — clean, or broken one specific way. What varies between rows is what
//! actually changes the code paths under test: the filesystem, subvolumes,
//! encryption, and which repair the result should need.
//!
//! Developer builds only. The mode-menu row that opens this screen is compiled
//! out of a release, and the screen goes with it.

use crate::app::{App, BootMode, PartitionMode, Screen};
use crate::screens::widgets;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Which desktop a test install puts on, chosen once for whatever scenario is
/// run next.
///
/// A RUN WITH NO DESKTOP CANNOT BE CHECKED. The first version installed none,
/// to save the download — and the result was a machine where SDDM came up with
/// no session behind it, which is EXACTLY the symptom of a real failure seen
/// before. There was then no way to tell a broken repair from a configuration
/// that never had a desktop in it. A test you cannot read is not a test.
///
/// XFCE first, because that is what the machine being tested actually runs —
/// a test on a desktop nobody uses answers a question nobody asked. LXQt is
/// there when the point is turnaround time rather than realism.
pub(crate) const DESKTOPS: [(&str, crate::app::Desktop); 4] = [
    ("XFCE", crate::app::Desktop::Xfce4),
    ("LXQt (fastest)", crate::app::Desktop::Lxqt),
    ("Cinnamon", crate::app::Desktop::Cinnamon),
    ("none (cannot be verified)", crate::app::Desktop::None),
];

/// What a scenario breaks once the install is finished.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Break {
    Nothing,
    Fstab,
    Bootloader,
    Both,
    /// A dinit service that never finishes starting, enabled at boot.
    ///
    /// The failure the author keeps hitting: a service written by hand (or
    /// written to order) that is fine enough to enable and wrong enough to stop
    /// the machine coming up. dinit waits for every entry in boot.d to start or
    /// fail, so one that does neither holds the boot indefinitely — no desktop,
    /// no login prompt, nothing to fix it from. It is repaired from the ISO, by
    /// the service list recovery grew for exactly this.
    Service,
}

pub(crate) struct Scenario {
    pub name: &'static str,
    /// What the run is for, in one line — this is what tells you which row to
    /// pick when something is broken and you are guessing why.
    pub about: &'static str,
    pub fs: &'static str,
    pub subvolumes: bool,
    pub encrypt: bool,
    pub loader: crate::app::Bootloader,
    pub brk: Break,
    /// Anything else this row changes, applied LAST so it can override
    /// anything above. A function rather than five more columns: two rows care
    /// about zram and one about repositories, and a struct field per idea would
    /// describe fourteen rows that do not.
    pub tweak: Option<fn(&mut crate::app::InstallConfig)>,
    /// Struck off after a run that was verified on the disk, not just on
    /// screen. Kept in the list rather than deleted: what has been covered is
    /// worth more than a short list, and a passed scenario is the first thing
    /// to re-run when something near it changes.
    pub passed: bool,
}

/// The scenarios, chosen so that each one exercises something the others do
/// not. Order runs cheap-and-simple first: ext4 without encryption is the
/// layout where the fewest things can go wrong, so a failure there is a failure
/// in the repair rather than in the layout around it.
pub(crate) const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "ext4 · no fstab",
        about: "no fstab on a simple layout: it still BOOTS, and that is the lesson",
        fs: "ext4",
        subvolumes: false,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Fstab,
        // Verified 2026-08-13 on the disk: fstab rebuilt with /, /boot and
        // swap; one firmware entry, not two; home intact.
        tweak: None,
        passed: true,
    },
    Scenario {
        name: "ext4 · no bootloader",
        about: "the EFI files and grub.cfg are removed; tests the loader repair",
        fs: "ext4",
        subvolumes: false,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Bootloader,
        // Verified 2026-08-14 on the machine: the diagnosis NAMED the broken
        // loader instead of passing it, and the repair put GRUB back under the
        // id that was already there. Worth its own note, because it took two
        // runs: the first said "nothing obviously broken" about a machine that
        // would not boot — the .efi query was matching grub's own module files
        // in /boot/grub/x86_64-efi, which are on every GRUB system and which no
        // firmware loads.
        tweak: None,
        passed: true,
    },
    Scenario {
        name: "ext4 · both broken",
        about: "no fstab AND no loader — the two repairs must not fight",
        fs: "ext4",
        subvolumes: false,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Both,
        // Verified 2026-08-14 on the machine, and it is the row that justifies
        // the whole screen: two faults at once is where a repair that cannot
        // report on itself stops being usable. Both were named, fixed one after
        // the other, and the re-check after each one is what showed the first
        // had landed before the second was started.
        tweak: None,
        passed: true,
    },
    Scenario {
        name: "xfs · no fstab",
        about: "the same silent damage on xfs — swap off, ESP unmounted, boots fine",
        fs: "xfs",
        subvolumes: false,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Fstab,
        tweak: None,
        passed: false,
    },
    Scenario {
        name: "btrfs (subvolumes) · no fstab",
        about: "the layout where a missing fstab DOES stop the boot: @log is /var/log",
        fs: "btrfs",
        subvolumes: true,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Fstab,
        tweak: None,
        passed: false,
    },
    Scenario {
        name: "btrfs (subvolumes) · no bootloader",
        about: "loader repair with the ESP mounted at /boot/efi",
        fs: "btrfs",
        subvolumes: true,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Bootloader,
        tweak: None,
        passed: false,
    },
    Scenario {
        name: "btrfs + LUKS · no fstab",
        about: "the repair must work inside an unlocked container",
        fs: "btrfs",
        subvolumes: true,
        encrypt: true,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Fstab,
        tweak: None,
        passed: false,
    },
    Scenario {
        name: "btrfs + LUKS · no bootloader",
        about: "encrypted root, so /boot is the ESP and the kernels live there",
        fs: "btrfs",
        subvolumes: true,
        encrypt: true,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Bootloader,
        tweak: None,
        passed: false,
    },
    Scenario {
        name: "btrfs + LUKS · both broken",
        about: "the worst realistic case: nothing boots and nothing is described",
        fs: "btrfs",
        subvolumes: true,
        encrypt: true,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Both,
        tweak: None,
        passed: false,
    },
    Scenario {
        name: "btrfs (subvolumes) · nothing broken",
        about: "a clean baseline — if this will not boot, the fault is upstream",
        fs: "btrfs",
        subvolumes: true,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Nothing,
        tweak: None,
        passed: false,
    },
    Scenario {
        name: "xfs · no bootloader",
        about: "loader repair on a filesystem with no subvolume tricks at all",
        fs: "xfs",
        subvolumes: false,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Bootloader,
        tweak: None,
        passed: false,
    },
    Scenario {
        name: "xfs · both broken",
        about: "xfs, nothing described and nothing bootable",
        fs: "xfs",
        subvolumes: false,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Both,
        tweak: None,
        passed: false,
    },
    Scenario {
        name: "xfs + LUKS · no fstab",
        about: "xfs inside a LUKS container — no btrfs anywhere in the path",
        fs: "xfs",
        subvolumes: false,
        encrypt: true,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Fstab,
        tweak: None,
        passed: false,
    },
    Scenario {
        name: "rEFInd · no bootloader",
        about: "recovery must recognise rEFInd on the ESP, not assume GRUB",
        fs: "ext4",
        subvolumes: false,
        encrypt: false,
        loader: crate::app::Bootloader::Refind,
        brk: Break::Bootloader,
        tweak: None,
        passed: false,
    },
    Scenario {
        name: "Limine · no bootloader",
        about: "the same, for Limine",
        fs: "ext4",
        subvolumes: false,
        encrypt: false,
        loader: crate::app::Bootloader::Limine,
        brk: Break::Bootloader,
        tweak: None,
        passed: false,
    },
    Scenario {
        name: "EFISTUB · no bootloader",
        about: "no loader at all — the kernel IS the EFI binary",
        fs: "ext4",
        subvolumes: false,
        encrypt: false,
        loader: crate::app::Bootloader::Efistub,
        brk: Break::Bootloader,
        tweak: None,
        passed: false,
    },
    // ── Paths this installer has never actually been run down ──────────────
    //
    // Everything above breaks a system on purpose to test a repair. These do
    // not: they install configurations that are written, plan-tested and never
    // once booted. A plan test proves the words are right; only a run proves
    // the packages exist, the service starts, and the machine comes up.
    Scenario {
        name: "btrfs · zram (zramen)",
        about: "swap in RAM instead of a partition — check `swapon` shows zram0",
        fs: "btrfs",
        subvolumes: true,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Nothing,
        tweak: Some(|c| {
            c.zram = true;
            c.zswap = false;
        }),
        passed: false,
    },
    Scenario {
        name: "btrfs · zram, no swap partition at all",
        about: "the layout zram is FOR: nothing on disk to swap to",
        fs: "btrfs",
        subvolumes: true,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Nothing,
        tweak: Some(|c| {
            c.zram = true;
            c.zswap = false;
            c.swap_gib = 0;
        }),
        passed: false,
    },
    Scenario {
        name: "ext4 · zswap + earlyoom",
        about: "the other memory path — kernel cmdline plus a hand-written service",
        fs: "ext4",
        subvolumes: false,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Nothing,
        tweak: Some(|c| {
            c.zswap = true;
            c.zswap_compressor = "lzo-rle".into();
            c.earlyoom = true;
        }),
        passed: false,
    },
    Scenario {
        name: "ext4 · a dinit service that hangs the boot",
        about: "for the service list in recovery: nothing else can reach this machine",
        fs: "ext4",
        subvolumes: false,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Service,
        tweak: None,
        passed: false,
    },
    Scenario {
        name: "btrfs + LUKS · a dinit service that hangs the boot",
        about: "the same, behind encryption — recovery must unlock before it can list",
        fs: "btrfs",
        subvolumes: true,
        encrypt: true,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Service,
        tweak: None,
        // Verified 2026-09-06 by the author, and it is THE row this release
        // exists for: the service was at the top of recovery's list (newest
        // first), disabling it brought the machine back. Confirmed here on the
        // disk as well — /etc/dinit.d/test-hang with its boot.d symlink, an
        // hour newer than every other entry.
        //
        // What is proven is the RECOVERY half. The break half was still the
        // racy `before = local.target` version at that point, which stops a
        // boot only when it wins a race with local.target; it has since been
        // replaced by one that cannot lose. So this row's repair is confirmed
        // and its trigger is now more reliable than the run that confirmed it.
        passed: true,
    },
    Scenario {
        name: "ext4 · AURIS + Chaotic-AUR",
        about: "third-party repos: key trust first, then the stanza — never run live",
        fs: "ext4",
        subvolumes: false,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Nothing,
        tweak: Some(|c| {
            c.auris = true;
            c.chaotic_aur = true;
        }),
        passed: false,
    },
    Scenario {
        name: "console only · no xorg, doas",
        about: "what this installer is actually for: a TTY, no X named at all",
        fs: "ext4",
        subvolumes: false,
        encrypt: false,
        loader: crate::app::Bootloader::Grub,
        brk: Break::Nothing,
        tweak: Some(|c| {
            // The desktop and X both go. `xorg` is a MARKER in extra_packages,
            // not a package: dropping it makes the installer name no X package
            // at all, which is the case a desktop install can never exercise.
            c.desktops.clear();
            c.extra_packages.retain(|p| p != "xorg");
            c.use_doas = true;
        }),
        passed: false,
    },
];

/// Fill in every answer the wizard would have asked for.
///
/// A SCENARIO IS THE WHOLE CONFIGURATION, not a few overrides on top of
/// whatever was left from browsing the menus. Anything not set here would carry
/// over from a previous run, and a test whose inputs depend on what you did
/// before it is not a test.
pub(crate) fn apply(app: &mut App, idx: usize) {
    let Some(s) = SCENARIOS.get(idx) else { return };
    let lang = app.config.lang.clone();
    let font = app.config.console_font.clone();
    app.config = crate::app::InstallConfig::default();
    app.config.lang = lang;
    app.config.console_font = font;

    app.config.locale = "en_US.UTF-8".into();
    app.config.timezone = "Europe/Kyiv".into();
    app.config.rtc_utc = true;
    app.config.xkb_layouts = vec!["us".into()];
    app.config.keymap = "us".into();
    app.config.hostname = "artix-test".into();
    app.config.username = "tester".into();
    app.config.user_password = "1323".into();
    app.config.root_password = "1323".into();
    app.config.kernel = crate::app::Kernel::Linux;
    // A REAL DESKTOP, so the result can actually be judged. See DESKTOPS.
    app.config.desktops.clear();
    let (_, de) = DESKTOPS[app.test_desktop.min(DESKTOPS.len() - 1)];
    if de != crate::app::Desktop::None {
        app.config.desktops.push(format!("{de:?}"));
    }
    app.config.extra_packages.clear();
    app.config.aur_packages.clear();

    app.config.boot_mode = BootMode::Uefi;
    app.config.partition_mode = PartitionMode::Auto;
    // THE TARGET DISK, which the wizard would have asked for on the disk step.
    //
    // Leaving it empty produced `wipefs -a` with no argument — "probing
    // initialization failed: No such file or directory" — twenty minutes into a
    // run, after every package had already been installed. A scenario that
    // fills in every answer but the one that says WHERE is not unattended.
    //
    // The biggest disk that is not the live medium: a test stand has the boot
    // stick plus one or more targets, and the stick is the one thing that must
    // never be chosen.
    app.config.disk = crate::screens::disk::disks()
        .iter()
        .filter(|d| !d.is_live)
        .max_by_key(|d| d.size_bytes)
        .map(|d| d.path.clone())
        .unwrap_or_default();
    app.config.root_fs = s.fs.into();
    app.config.btrfs_subvolumes = s.subvolumes;
    app.config.btrfs_snapshots = s.subvolumes;
    app.config.encrypt_disk = s.encrypt;
    app.config.bootloader = s.loader;
    // A RANDOM NAME, DELIBERATELY. The repair is supposed to reuse whatever
    // bootloader-id is already on the ESP, and with the default "artix" every
    // run would pass whether it reused it or hardcoded it — the exact bug this
    // is here to catch. A word nobody could have written into the source makes
    // the difference visible: after the repair the entry must still be called
    // this, and there must not be a second one beside it.
    app.config.bootloader_id = random_id();
    app.config.encrypt_scope = "root".into();
    app.config.luks_passphrase = "1323".into();
    // The encryption block is also fed from the "separate /home" idea in manual
    // mode; auto mode has no such option, so scenarios stay whole-disk.
    app.config.swap_gib = 2;

    // The plan reads a number, so the shape is translated once, here, rather
    // than teaching the always-compiled install code about a devtools-only enum.
    app.test_mode = s.brk != Break::Nothing;
    app.test_scenario = match s.brk {
        Break::Fstab => 0,
        Break::Bootloader => 1,
        Break::Both => 2,
        Break::Service => 3,
        Break::Nothing => 0,
    };

    // LAST, so a row can override anything above it — including the desktop and
    // the swap size, which the console-only and zram rows both depend on.
    if let Some(tweak) = s.tweak {
        tweak(&mut app.config);
    }
}

/// A throwaway name for the firmware entry, different on every run.
///
/// No dependency for this: the clock is already a source of numbers nobody
/// planned, and the point is only that the name is not one the source code
/// could have known. Two words plus a number, so it is still readable when it
/// turns up in `efibootmgr` output or on the ESP.
fn random_id() -> String {
    const WORDS: [&str; 16] = [
        "amber", "birch", "cedar", "delta", "ember", "fjord", "grove", "heron", "ivory", "jetty",
        "larch", "maple", "nomad", "onyx", "quartz", "raven",
    ];
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    format!(
        "{}-{}{}",
        WORDS[n % WORDS.len()],
        WORDS[(n / WORDS.len()) % WORDS.len()],
        n % 100
    )
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6), // warning + break/desktop strips
            Constraint::Min(0),    // list
            Constraint::Length(3), // description of the highlighted row
        ])
        .spacing(1)
        .split(area);

    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Each row INSTALLS A COMPLETE SYSTEM without asking anything,",
                theme::warn(),
            )),
            Line::from(Span::styled(
                "erasing the target disk, and then breaks it as described.",
                theme::warn(),
            )),
            Line::from(Span::styled(
                "passwords: 1323 · host: artix-test · the UEFI entry gets a RANDOM name",
                theme::dim(),
            )),
            Line::from(vec![
                Span::styled("  break (b): ", theme::dim()),
                if app.test_break_now {
                    Span::styled(
                        "<at the end of the install>  ·  after I run ~/break-*.sh",
                        theme::gold(),
                    )
                } else {
                    Span::styled(
                        "at the end of the install  ·  <after I run ~/break-*.sh>",
                        theme::gold(),
                    )
                },
            ]),
            Line::from({
                let mut v = vec![Span::styled("  desktop (←/→): ", theme::dim())];
                for (i, (name, _)) in DESKTOPS.iter().enumerate() {
                    if i > 0 {
                        v.push(Span::styled(" · ", theme::mute()));
                    }
                    v.push(if i == app.test_desktop {
                        Span::styled(format!("<{name}>"), theme::gold())
                    } else {
                        Span::styled((*name).to_string(), theme::mute())
                    });
                }
                v
            }),
        ]),
        rows[0],
    );

    // Struck off, not removed: what has already been verified stays visible, so
    // the list doubles as the coverage record.
    let items: Vec<String> = SCENARIOS
        .iter()
        .map(|s| format!("  [{}] {}", if s.passed { "x" } else { " " }, s.name))
        .collect();
    let cur = app.cursor.min(items.len().saturating_sub(1));
    widgets::select_list_scrolled(f, rows[1], &items, cur, app.marquee);

    if let Some(s) = SCENARIOS.get(cur) {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {}", s.about),
                theme::dim(),
            )))
            .wrap(ratatui::widgets::Wrap { trim: true }),
            rows[2],
        );
    }
    app.can_advance = false;
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.cursor = app.cursor.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            app.cursor = (app.cursor + 1).min(SCENARIOS.len() - 1)
        }
        KeyCode::Char('b') => app.test_break_now = !app.test_break_now,
        KeyCode::Left => {
            app.test_desktop = (app.test_desktop + DESKTOPS.len() - 1) % DESKTOPS.len();
        }
        KeyCode::Right => {
            app.test_desktop = (app.test_desktop + 1) % DESKTOPS.len();
        }
        KeyCode::Enter => {
            apply(app, app.cursor);
            // Straight to the summary, which is where the install is reviewed
            // and started. Deliberately NOT auto-started: the summary lists the
            // disk that is about to be erased, and even a test run should show
            // that to a person before doing it.
            app.goto(Screen::Summary);
        }
        KeyCode::Esc => app.goto(Screen::Mode),
        _ => {}
    }
}

pub fn footer_hint(_app: &App) -> String {
    "↑/↓ scenario · ←/→ desktop · b when to break · Enter install · Esc back".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// EVERY SCENARIO MUST PRODUCE A PLAN THAT CAN RUN.
    ///
    /// The bootloader scenario died at the very last step with "grub-install:
    /// /boot doesn't look like an EFI partition" — after every package was
    /// installed — because the ESP had been moved to /boot/efi in one place and
    /// `--efi-directory` still said /boot in another. Twenty minutes to learn
    /// that two constants disagreed.
    ///
    /// So the plans are built here, for all of them, in a fraction of a second:
    /// the mount and the loader must name the SAME directory, and nothing may
    /// be handed a device path that is empty.
    #[test]
    fn every_scenario_builds_a_plan_that_agrees_with_itself() {
        for (i, sc) in SCENARIOS.iter().enumerate() {
            let mut app = App::new();
            apply(&mut app, i);
            // `apply` picks the biggest non-live disk; under test that is the
            // stubbed list, so a target is always chosen.
            assert!(
                !app.config.disk.is_empty(),
                "{}: no target disk was chosen",
                sc.name
            );

            let plan = crate::system::install::build_plan(&app);
            let text: String = plan
                .iter()
                .map(|a| format!("{} {}\n", a.program, a.args.join(" ")))
                .collect();

            let esp = crate::system::disk::esp_mountpoint(&app.config);
            assert!(
                text.contains(&format!("mount {} /mnt{esp}", app.config.disk))
                    || text.contains(&format!("/mnt{esp}")),
                "{}: nothing mounts the ESP at {esp}",
                sc.name
            );
            if app.config.bootloader == crate::app::Bootloader::Grub {
                assert!(
                    text.contains(&format!("--efi-directory={esp}")),
                    "{}: the ESP is mounted at {esp} but grub-install points \
                     somewhere else — this is the failure that cost a whole run",
                    sc.name
                );
            }

            // A COMMAND WITH A MISSING DEVICE. `wipefs -a` with nothing after it
            // reports "probing initialization failed: No such file or
            // directory" — twenty minutes in, naming neither the disk nor the
            // step. Cheap to rule out here.
            for a in &plan {
                assert!(
                    !a.args.iter().any(|x| x.is_empty()),
                    "{}: `{} {}` has an empty argument",
                    sc.name,
                    a.program,
                    a.args.join(" ")
                );
            }
        }
    }
}
