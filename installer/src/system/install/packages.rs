//! From user choices to concrete pacman package lists.
//!
//! Desktop-environment sets (incl. the Pinnacle applet stack), GPU driver
//! matrices, kernel name/image tables, display-manager packages+services,
//! and the big `base_packages` / `system_packages` builders that merge it
//! all. Package names here MUST exist in Artix repos (world/galaxy/extra)
//! or, where marked, in the AUR via `effective_aur_packages`.

use super::*;

/// The genuine X.Org server and the common X utility apps. Installed for X11
/// desktops so the system uses real Xorg rather than Artix's new default
/// XLibre (which can be flaky with some software). XLibre only replaces
/// `xorg-server` + xf86 drivers, so pulling `xorg-server` explicitly pins the
/// real server; the rest are standard X tools every X session expects.
pub(crate) const XORG_PACKAGES: &[&str] = &[
    "xorg-server",
    "xorg-server-common",
    "xorg-xinit",
    "xorg-xauth",
    "xorg-xrandr",
    "xorg-xset",
    "xorg-xsetroot",
    "xorg-xprop",
    "xorg-xkill",
    "xorg-xev",
    "xorg-xinput",
    "xorg-xkbcomp",
    "xorg-xrdb",
    "xorg-xmodmap",
    "xorg-xdpyinfo",
    "xorg-setxkbmap",
    "xorg-fonts-misc",
    "xorg-fonts-encodings",
];

/// The real (non-None) desktops the user picked, decoded from their serialized
/// names. Empty means a headless / minimal install.
pub fn chosen_desktops(c: &InstallConfig) -> Vec<Desktop> {
    c.desktops
        .iter()
        .map(|s| desktop_from(s))
        .filter(|d| !matches!(d, Desktop::None))
        .collect()
}

/// Public helper for the summary screen: a human-readable, comma-separated list
/// of the chosen desktops' labels (empty string when none are selected).
pub fn desktops_label(c: &InstallConfig) -> String {
    chosen_desktops(c)
        .iter()
        .map(|d| d.label())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Which session types are available across ALL chosen desktops, as a short tag
/// (e.g. "X11", "Wayland", "X11 / Wayland"). Empty when headless.
pub fn desktops_session_tag(c: &InstallConfig) -> &'static str {
    let des = chosen_desktops(c);
    let any_wl = des.iter().any(|d| d.supports_wayland());
    let any_x = des.iter().any(|d| d.supports_x11());
    match (any_wl, any_x) {
        (true, true) => "X11 / Wayland",
        (true, false) => "Wayland",
        (false, true) => "X11",
        (false, false) => "",
    }
}

pub(crate) fn desktop_from(s: &str) -> Desktop {
    match s {
        "KdePlasma" => Desktop::KdePlasma,
        "Gnome" => Desktop::Gnome,
        "Xfce4" => Desktop::Xfce4,
        "Cinnamon" => Desktop::Cinnamon,
        "Mate" => Desktop::Mate,
        "Lxqt" => Desktop::Lxqt,
        "Lxde" => Desktop::Lxde,
        "Pinnacle" => Desktop::Pinnacle,
        _ => Desktop::None,
    }
}

/// The effective AUR package list: the user's explicit AUR picks plus, when
/// Pinnacle is the chosen desktop, pinnacle-comp itself and its AUR-only
/// companion tools (waypaper wallpaper picker, flameshot-git screenshot tool).
/// Deduplicated and order-preserving. Repo-side Pinnacle companions are added
/// separately in system_packages(); this covers only the AUR side. Used by
/// both the AUR install phase and the config-unpack guard so they agree on
/// whether pinnacle-comp is being installed.
pub(crate) fn effective_aur_packages(c: &InstallConfig) -> Vec<String> {
    let mut out: Vec<String> = c.aur_packages.clone();
    if c.desktops.iter().any(|s| s == "Pinnacle") {
        for pkg in ["pinnacle-comp", "waypaper", "flameshot-git"] {
            if !out.iter().any(|x| x == pkg) {
                out.push(pkg.to_string());
            }
        }
    }
    out
}

/// True if any selected GPU driver is a proprietary NVIDIA stack (which needs
/// nvidia-persistenced and a nouveau blacklist).
pub(crate) fn any_proprietary_nvidia(gpus: &[GpuDriver]) -> bool {
    gpus.iter().any(|g| g.is_proprietary_nvidia())
}

/// The /boot image file names for the chosen kernel: (vmlinuz, initramfs).
/// Bootloaders with a static config (Limine) must point at the REAL files —
/// hardcoding vmlinuz-linux breaks every non-default kernel choice.
pub(crate) fn kernel_images(k: Kernel) -> (&'static str, &'static str) {
    match k {
        Kernel::Zen => ("vmlinuz-linux-zen", "initramfs-linux-zen.img"),
        Kernel::Hardened => ("vmlinuz-linux-hardened", "initramfs-linux-hardened.img"),
        Kernel::Lts => ("vmlinuz-linux-lts", "initramfs-linux-lts.img"),
        Kernel::Linux => ("vmlinuz-linux", "initramfs-linux.img"),
    }
}

/// Packages for the chosen display manager. All names verified to exist in the
/// OFFICIAL Artix repos (no AUR): greetd-tuigreet (world), greetd-regreet
/// (galaxy), sddm (world). The *-dinit service packages are Artix's.
/// xorg-xinit ships `startx`, used by tuigreet's --xsession-wrapper for X11
/// sessions (tiny, so always bundled with greetd). cage is the kiosk
/// compositor that hosts ReGreet (a GTK Wayland greeter).
pub(crate) fn dm_packages(dm: &str) -> Vec<&'static str> {
    match dm {
        "sddm" => vec!["sddm", "sddm-dinit"],
        "tuigreet" => vec!["greetd", "greetd-dinit", "greetd-tuigreet", "xorg-xinit"],
        "regreet" => vec![
            "greetd",
            "greetd-dinit",
            "greetd-regreet",
            "cage",
            "xorg-xinit",
        ],
        _ => vec![],
    }
}

/// The dinit SERVICE to enable for the chosen display manager (service files
/// are named after the daemon, not the package).
pub(crate) fn dm_service(dm: &str) -> Option<&'static str> {
    match dm {
        "sddm" => Some("sddm"),
        "none" => None,
        _ => Some("greetd"),
    }
}

/// Assemble the full base package set for basestrap.
/// Expand a pacman group name into its member packages. Runs `pacman -Sgq
/// <name>`, which prints one member per line for a group, or nothing for a
/// plain package. If it's not a group (no output / error), the original name is
/// returned unchanged so it's installed as a normal package. Requires synced
/// databases; we sync once lazily (the live image starts with none).
pub(crate) fn expand_group(name: &str) -> Vec<String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SYNCED: AtomicBool = AtomicBool::new(false);
    if !SYNCED.swap(true, Ordering::Relaxed) {
        // -Sy once so -Sgq has data to read. Ignore failure (offline → we just
        // fall back to passing the group name through).
        let _ = crate::system::runner::capture("pacman", &["-Sy", "--noconfirm"]);
    }
    match crate::system::runner::capture("pacman", &["-Sgq", name]) {
        Ok(out) => {
            let members: Vec<String> = out
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            if members.is_empty() {
                vec![name.to_string()] // not a group → install as-is
            } else {
                members
            }
        }
        Err(_) => vec![name.to_string()],
    }
}

pub(crate) fn base_packages(c: &InstallConfig) -> Vec<String> {
    let mut p: Vec<String> = vec![
        "base",
        "base-devel",
        "dinit",
        "linux-firmware",
        "networkmanager",
        "networkmanager-dinit",
        "nano",
        "geany",
        "geany-plugins",
        "grub",
        "efibootmgr",
        "os-prober",
        "dbus-dinit",
        "dbus-dinit-user",
        "mkinitcpio",
        // iptables-nft chosen explicitly: nftables/networkmanager pull in a
        // libxtables provider, and without naming one basestrap (which forces
        // --noconfirm) would silently take provider #1. We standardise on the
        // modern nft backend. Naming it as an explicit target makes pacman use
        // it instead of plain iptables, with no interactive prompt.
        "iptables-nft",
        // Fonts: Ubuntu family for desktops, JetBrainsMono Nerd Font for the
        // terminal/icons, Terminus as a Unicode-capable console (TTY) font, and
        // the Nerd Fonts symbol packages so glyphs (e.g. the Artix logo U+F31F)
        // render in terminal emulators on the installed system.
        "ttf-ubuntu-font-family",
        "ttf-jetbrains-mono-nerd",
        "terminus-font",
        "ttf-nerd-fonts-symbols",
        "ttf-nerd-fonts-symbols-common",
        "ttf-nerd-fonts-symbols-mono",
        // Broad Unicode coverage so the installed system never shows tofu
        // (□ boxes) for emoji or CJK/other scripts: Noto base + extra scripts,
        // colour emoji, and the CJK (Chinese/Japanese/Korean) families.
        "noto-fonts",
        "noto-fonts-extra",
        "noto-fonts-emoji",
        "noto-fonts-cjk",
        // Arch compatibility: lets the installed system enable Arch's own repos
        // (extra/multilib) and trust their signing keys, so packages not in
        // Artix can be installed. archlinux-keyring + mirrorlist are required
        // for that to work; the support packages wire up the repos.
        "artix-archlinux-support",
        "lib32-artix-archlinux-support",
        "archlinux-keyring",
        "archlinux-mirrorlist",
        // System logging stack (collect all system logs + auto-expire weekly):
        //   • syslog-ng + syslog-ng-dinit — the syslog daemon that captures the
        //     whole system's logs (kernel, daemons, auth, …) into /var/log/*.
        //     This is what gives a complete system log; dinit's own `catlog` is
        //     only a per-service in-memory buffer, not a system-wide log.
        //   • logrotate — rotates and DELETES old logs. Configured (below) to
        //     keep one week and drop anything older.
        //   • cronie + cronie-dinit — cron daemon that runs logrotate on a
        //     schedule (the /etc/cron.daily/logrotate job). Without a cron
        //     daemon, logrotate would never run and logs would grow forever.
        // The matching dinit services (syslog-ng, cronie) are enabled later.
        "syslog-ng",
        "syslog-ng-dinit",
        "logrotate",
        "cronie",
        "cronie-dinit",
        // rtkit (RealtimeKit): grants PipeWire/WirePlumber real-time scheduling
        // priority so audio threads aren't preempted (prevents xruns/crackle
        // under load). The daemon is started ON DEMAND by the D-Bus *system* bus
        // the first time a client asks (it ships an org.freedesktop.RealtimeKit1
        // D-Bus service file with an Exec= line, so it activates fine under dinit
        // — no systemd, no separate rtkit-dinit service needed). Without this
        // package the system bus has no RealtimeKit1 provider and PipeWire logs
        // "RTKit error: ServiceUnknown" and falls back to non-RT scheduling. Its
        // deps (dbus, polkit, libelogind) are already present.
        "rtkit",
        // The per-user dinit launcher (turnstile vs userspawn) is chosen by the
        // seat backend and pushed conditionally AFTER this vec — see below.
        // The base/network stack pulls libxtables, which has THREE providers
        // (iptables / iptables-legacy / iptables-nft). basestrap forces
        // --noconfirm so it can't ask — it would silently take provider #1.
        // We name iptables-nft explicitly so the modern nft backend is chosen
        // deterministically (it matches our nftables-based firewall) instead of
        // a blind default. As a named target in the same transaction, pacman
        // resolves the provider to this without prompting.
        "iptables-nft",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    // Privilege-escalation tool follows the options-screen choice: sudo
    // (default) or doas. Exactly ONE is installed — both give the same
    // capability and co-installing only invites confusion. doas keeps a
    // tiny /etc/doas.conf (written later in the run); sudo uses sudoers.
    p.push(if c.use_doas {
        "opendoas".to_string()
    } else {
        "sudo".to_string()
    });

    // The kernel chosen by the user (linux / zen / hardened / lts) + headers.
    p.extend(c.kernel.packages().iter().map(|s| s.to_string()));
    // CPU microcode. Both packages are installed (each is tiny); the kernel
    // only ever loads the one matching the actual CPU, and installing both
    // keeps the disk portable between AMD/Intel machines. grub-mkconfig picks
    // the images up automatically; the Limine config adds them explicitly.
    p.push("amd-ucode".into());
    p.push("intel-ucode".into());

    // Userspace tools for the chosen root filesystem, so the installed system
    // can fsck/mount/maintain it. ext2/3/4 use e2fsprogs (pulled by base).
    match c.root_fs.as_str() {
        "btrfs" => p.push("btrfs-progs".into()),
        "xfs" => p.push("xfsprogs".into()),
        "f2fs" => p.push("f2fs-tools".into()),
        "jfs" => p.push("jfsutils".into()),
        _ => {} // ext*/default: e2fsprogs is already part of base
    }
    // Auto-snapshots: snapper manages the btrfs snapshots and snap-pac hooks
    // pacman to take a pre/post snapshot around every transaction. Only added
    // for btrfs with the subvolume layout (the UI guarantees subvolumes is on
    // whenever snapshots is). cronie (already in base) runs the cleanup job.
    if c.root_fs == "btrfs" && c.btrfs_subvolumes && c.btrfs_snapshots {
        p.push("snapper".into());
        p.push("snap-pac".into());
        // NB: we deliberately do NOT install grub-btrfs. Its GRUB snapshot
        // submenu boots snapshots read-only via an overlayfs hook that is broken
        // on kernels >=6.8 (Antynea/grub-btrfs issue #328) — every entry would
        // just fail to boot, which is a trap for the user. Rollback is handled by
        // the built-in `artix-rollback` tool instead (it swaps @ and boots the
        // restored root read-write, no overlay, kernel-agnostic, works for every
        // bootloader). snapper + snap-pac stay: they create the snapshots the
        // tool rolls back to.
    }
    // Userspace tools for additional disks: the maintenance tools for any extra
    // filesystem the user formats, plus ntfs-3g whenever an existing NTFS volume
    // (e.g. a Windows partition) is mounted, so it actually mounts at boot.
    for d in &c.extra_disks {
        if d.mountpoint.is_empty() {
            continue; // a filesystem was pre-picked but no mountpoint set → ignore
        }
        if d.format {
            match d.fs.as_str() {
                "btrfs" if !p.iter().any(|x| x == "btrfs-progs") => p.push("btrfs-progs".into()),
                "xfs" if !p.iter().any(|x| x == "xfsprogs") => p.push("xfsprogs".into()),
                "f2fs" if !p.iter().any(|x| x == "f2fs-tools") => p.push("f2fs-tools".into()),
                "jfs" if !p.iter().any(|x| x == "jfsutils") => p.push("jfsutils".into()),
                _ => {}
            }
        } else if d.fs.eq_ignore_ascii_case("ntfs") && !p.iter().any(|x| x == "ntfs-3g") {
            p.push("ntfs-3g".into());
        }
    }
    // Bootloader package for the chosen bootloader (grub is already in the base
    // list above as the default/fallback). rEFInd and Limine are added on top.
    match c.bootloader {
        Bootloader::Refind => p.push("refind".into()),
        Bootloader::Limine => p.push("limine".into()),
        // GRUB is already in the base list; EFISTUB has no bootloader package
        // (efibootmgr, which writes the firmware entry, comes with the base).
        Bootloader::Grub | Bootloader::Efistub => {}
    }
    // Secure Boot prep (EFISTUB only): sbctl provides key generation, signing,
    // and a pacman hook that re-signs the kernel on every update. Installed here
    // so it's present for the user to finish enrollment on first boot.
    if c.bootloader.supports_secureboot_prep() && c.prepare_secureboot {
        p.push("sbctl".into());
    }

    // LUKS: the installed system needs cryptsetup present so the initramfs
    // `encrypt` hook (and any later maintenance) works — and so the dinit
    // auto-unlock service for an encrypted extra disk can run at boot.
    let any_extra_encrypt = c
        .extra_disks
        .iter()
        .any(|d| d.format && d.encrypt && !d.mountpoint.is_empty());
    if c.encrypt_disk || any_extra_encrypt {
        p.push("cryptsetup".into());
        // Full-boot encryption uses /etc/crypttab to reopen the encrypted /boot
        // in the running system. cryptsetup-dinit ships the dinit service that
        // processes crypttab at boot; it's auto-enabled by the *-dinit scanner.
        if c.encrypt_disk && c.encrypt_scope == "full" {
            p.push("cryptsetup-dinit".into());
            p.push("device-mapper-dinit".into());
        }
    }

    // All required dinit service packages (these ship the service files). No
    // provider conflicts, so they install fine via basestrap's --noconfirm.
    // nftables-dinit follows the (default-checked) nftables entry in the
    // packages screen: unticking nftables drops the service package too.
    p.extend(
        DINIT_PACKAGES
            .iter()
            .filter(|s| **s != "nftables-dinit" || c.extra_packages.iter().any(|x| x == "nftables"))
            .map(|s| s.to_string()),
    );

    // Per-user dinit launcher — the piece that spawns the user's dinit instance
    // at login (and thus D-Bus + PipeWire → sound). The two options CONFLICT
    // (both are user-dinit backends), so EXACTLY ONE is installed, chosen by
    // the seat backend:
    //   • elogind → userspawn (+ userspawn-dinit): the stable, Artix-native
    //     launcher. It reacts to logind's UserNew D-Bus signal, which elogind
    //     provides — so it works here. Wired with a userspawnrc + the userspawn
    //     service later.
    //   • seatd / none → turnstile (+ turnstile-dinit): the newer/more
    //     experimental option, but the only one that works WITHOUT elogind (it
    //     has its own pam_turnstile.so and can manage the runtime dir itself).
    //     Wired with pam_turnstile + turnstiled.conf + the turnstiled service.
    if c.seat_provider == SeatProvider::Elogind {
        p.push("userspawn".into());
        p.push("userspawn-dinit".into());
    } else {
        p.push("turnstile".into());
        p.push("turnstile-dinit".into());
    }

    // Full PipeWire audio stack (daemons + plugins + lib32), always installed.
    // No provider conflicts.
    p.extend(AUDIO_PACKAGES.iter().map(|s| s.to_string()));

    p.sort();
    p.dedup();
    p
}

/// Packages installed in the SECOND phase: `pacman -S` in the chroot, run
/// interactively under a PTY (no --noconfirm) so the user picks providers for
/// the packages that actually have provider choices (vulkan-driver,
/// qt6-multimedia-backend, jack, etc.). This is everything beyond the minimal
/// bootable base: the desktop environment, GPU/driver stack, the user's chosen
/// extra packages, the display manager, seat backend, and shell extras.
pub(crate) fn system_packages(c: &InstallConfig) -> Vec<String> {
    let mut p: Vec<String> = Vec::new();
    // Every selected desktop (multi-select): union their packages so the user
    // can log into ANY of them at the display manager. chosen_desktops drops None.
    let des = chosen_desktops(c);
    for de in &des {
        // The desktop may declare package *groups* (e.g. "plasma"). We expand
        // groups into explicit members so pacman doesn't show the group prompt.
        for item in de.packages() {
            p.extend(expand_group(item));
        }
        // LXQt's native Wayland session lives in a separate package
        // (lxqt-wayland-session ships /usr/bin/startlxqtwayland + the
        // wayland-sessions .desktop entry) and needs a compositor — labwc is its
        // documented default/fallback. We install BOTH sessions whenever LXQt is
        // picked, so either X11 or Wayland can be chosen at the login screen.
        if matches!(de, Desktop::Lxqt) {
            for pkg in ["lxqt-wayland-session", "labwc"] {
                if !p.iter().any(|x| x == pkg) {
                    p.push(pkg.into());
                }
            }
        }
        // Pinnacle (AUR compositor) selected as the desktop: the pinnacle-comp
        // package itself is installed in the AUR phase (see desktop→AUR wiring in
        // build_plan), but its companion TOOLS that live in the official repos are
        // added here so they're in the fast basestrap transaction. These back the
        // shipped config + scripts: wl-clipboard (wl-copy, used by clipboard.sh and
        // for copy/paste), cliphist (clipboard history), wofi (the launcher the
        // scripts call), pasystray (tray audio applet), and xorg-xwayland so X11
        // apps run under the Wayland session. The AUR-only companions (waypaper,
        // flameshot-git) are added to the AUR list instead.
        if matches!(de, Desktop::Pinnacle) {
            // Everything the shipped pinnacle config spawns from the repos — without
            // these, autostart entries and keybinds silently fail (e.g. mod+e did
            // nothing because the file manager caja wasn't installed). Split by role:
            //   • caja            — file manager (mod+e keybind)
            //   • swaync          — notification daemon + swaync-client (autostart + keybind)
            //   • brightnessctl   — backlight keys
            //   • playerctl       — media keys (play/pause/next/prev)
            //   • network-manager-applet — provides nm-applet (tray, autostart)
            //   • pasystray       — PulseAudio/PipeWire tray (autostart)  [already added below historically]
            //   • lxsession       — provides /usr/lib/lxsession/lxpolkit (polkit agent, autostart)
            //   • xdg-desktop-portal + -wlr — portals for screenshots/file pickers (autostart)
            //   • kdeconnect      — kdeconnectd + kdeconnect-indicator (autostart)
            //   • wl-clipboard    — wl-paste/wl-copy (clipboard watch via cliphist)
            //   • kitty           — the terminal the config binds to mod+Return;
            //                       listed here so Pinnacle keeps a terminal even
            //                       if the user unticks the default-checked entry
            //   • cliphist, wofi  — clipboard history + launcher (mod+space)
            //   • xorg-xwayland   — X11 apps under the Wayland session
            // wpctl is part of wireplumber (already pulled in by audio); waypaper +
            // flameshot-git are AUR-only and handled in effective_aur_packages.
            // The config's web-browser keybind launches `firefox` (install it if you
            // want that key to work). The shipped Pinnacle config has no personal
            // apps in autostart, so nothing else is force-installed here.
            for pkg in [
                "wl-clipboard",
                "cliphist",
                "wofi",
                "waybar",
                "pasystray",
                "xorg-xwayland",
                "kitty",
                "caja",
                "swaync",
                "brightnessctl",
                "playerctl",
                "network-manager-applet",
                "lxsession",
                "xdg-desktop-portal",
                "xdg-desktop-portal-wlr",
                "kdeconnect",
            ] {
                if !p.iter().any(|x| x == pkg) {
                    p.push(pkg.to_string());
                }
            }
        }
    } // end: for de in &des — multi-desktop package union
      // Union the packages of ALL selected GPU drivers (hybrid graphics),
      // deduplicated (mesa/lib32-mesa overlap between Intel/AMD/nouveau).
    for g in &c.gpu {
        for pkg in g.packages() {
            if !p.iter().any(|x| x == pkg) {
                p.push(pkg.to_string());
            }
        }
    }
    p.extend(c.extra_packages.iter().cloned());
    // Companion packages: some apps are split into a thin main package plus
    // separate plugin/codec packages, and are nearly useless without them. We
    // pull those in automatically so the app works out of the box.
    if c.extra_packages.iter().any(|x| x == "vlc") {
        // Modern Artix splits VLC's codecs/outputs into vlc-plugins-*; the
        // `vlc-plugins-all` metapackage depends on base/extra/video-output/
        // visualization, so this single name covers them all. Without it VLC
        // can't play most formats.
        p.push("vlc-plugins-all".into());
    }
    // waybar's shipped config calls out to external programs and fonts that are
    // NOT dependencies of the waybar package itself, so the bar would look
    // broken (missing icons) or its click actions would silently fail without
    // them. Pull them in so the bar works out of the box:
    //   • pavucontrol     — pulseaudio module's on-click volume GUI
    //   • libpulse        — provides `pactl`, used by the module's mute/scroll
    //                       actions (works against the pipewire-pulse server)
    //   • otf-font-awesome — the style.css explicitly requires it for icons
    //   • ttf-roboto      — the style.css font-family (Roboto) for label text
    if p.iter().any(|x| x == "waybar") {
        for pkg in ["pavucontrol", "libpulse", "otf-font-awesome", "ttf-roboto"] {
            if !p.iter().any(|x| x == pkg) {
                p.push(pkg.into());
            }
        }
    }
    // Shell enhancements for zsh/fish.
    let want_zsh = c.extra_packages.iter().any(|x| x == "zsh");
    let want_fish = c.extra_packages.iter().any(|x| x == "fish");
    if want_zsh || want_fish {
        p.push("starship".into());
    }
    if want_zsh {
        for pkg in [
            "zsh-completions",
            "zsh-autosuggestions",
            "zsh-syntax-highlighting",
            "zsh-history-substring-search",
        ] {
            p.push(pkg.into());
        }
    }
    // Display manager — the user's explicit choice from the Options screen.
    p.extend(
        dm_packages(&c.display_manager)
            .iter()
            .map(|s| s.to_string()),
    );
    // elogind library/package: always present for a graphical desktop (Plasma,
    // polkit, portals link against libelogind). For "no DE" it's only added if
    // elogind is the chosen seat backend (below).
    if !des.is_empty() {
        p.push("elogind".into());
    }
    // Seat/login backend — installed ALWAYS, even with no desktop. A user who
    // skips the DE will install their own compositor/WM later, and it still
    // needs a seat manager (seatd or elogind) to acquire input/DRM; without one
    // there'd be no working graphical session. We honour the user's choice.
    p.extend(c.seat_provider.packages().iter().map(|x| x.to_string()));
    p.sort();
    p.dedup();
    p
}
