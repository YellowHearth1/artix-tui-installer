//! Global application state.
//!
//! `App` holds everything the UI and the installer backend need. The 10 wizard
//! steps each read/write into `InstallConfig`; `Screen` drives which view is
//! rendered and `nav` enforces the forward/back flow described in the spec.

use crate::i18n::Lang;

/// The ten wizard steps, in order. The discriminant order is the flow order,
/// so Next/Back can just bump an index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Screen {
    Language = 0,
    Timezone = 1,
    Wifi = 2,
    Keyboard = 3,
    Kernel = 4,
    Desktop = 5,
    Packages = 6,
    Aur = 7,
    // The auto-vs-manual question comes BEFORE the disk step: by the time the
    // user sees a disk list (auto) or a partition list (manual), the choice of
    // which list that should be has already been made.
    PartitionMode = 8,
    Disk = 9,
    Security = 10,
    Storage = 11,
    User = 12,
    Options = 13,
    Summary = 14,
    Finish = 15,
    // Screens OUTSIDE the linear 16-step install flow (not in ALL, no step
    // number): the post-language mode chooser and the recovery tool. Their
    // navigation is handled explicitly, not via next()/prev().
    Mode = 16,
    Recovery = 17,
    WifiTest = 18,
    TbwTest = 19,
    /// Console font chooser. On a bare TTY — this installer's first target — the
    /// font decides whether the interface is legible and whether it can draw its
    /// own text at all, so it is a first-class choice rather than a setting
    /// buried somewhere.
    FontPick = 20,
}

impl Screen {
    /// How many `Screen` variants exist — the size of every per-screen array.
    /// Counts the off-linear screens too, which `ALL` deliberately does not.
    pub const COUNT: usize = Screen::FontPick as usize + 1;

    pub const ALL: [Screen; 16] = [
        Screen::Language,
        // KEYBOARD SECOND, before anything asks you to type.
        //
        // It used to sit fourth, after the timezone filter and after the Wi-Fi
        // password — two fields you type into on whatever layout the language
        // screen guessed. Guessing right for the interface language is not the
        // same as guessing right for the keyboard in front of you: plenty of
        // Ukrainians use a German board, and people carry keyboards across
        // borders. A Wi-Fi password typed on the wrong layout does not explain
        // itself; it just fails to connect.
        //
        // So the order is now: what language do you read, what keyboard are you
        // typing on, and only then anything that takes text.
        Screen::Keyboard,
        Screen::Timezone,
        Screen::Wifi,
        Screen::Kernel,
        Screen::Desktop,
        Screen::Packages,
        Screen::Aur,
        Screen::PartitionMode,
        Screen::Disk,
        Screen::Security,
        Screen::Storage,
        Screen::User,
        Screen::Options,
        Screen::Summary,
        Screen::Finish,
    ];

    /// The step after this one, IN THE ORDER `ALL` DECLARES.
    ///
    /// Found by searching `ALL`, never by casting the variant to an index. It
    /// used to cast — which quietly assumed the enum was declared in the same
    /// order the wizard runs in. Moving the keyboard step ahead of the timezone
    /// broke that assumption and Esc started walking FORWARD: on the keyboard
    /// step (variant 3) `prev()` handed back `ALL[2]`, the timezone. Reported as
    /// "I press Escape and it throws me onward instead of back".
    ///
    /// Screens outside `ALL` (Mode, Recovery, the font picker …) are their own
    /// neighbours: they navigate explicitly and must never index into `ALL`.
    pub fn next(self) -> Screen {
        match Screen::ALL.iter().position(|s| *s == self) {
            Some(i) if i + 1 < Screen::ALL.len() => Screen::ALL[i + 1],
            _ => self,
        }
    }

    /// The step before this one. See `next` — same rule, same trap.
    pub fn prev(self) -> Screen {
        match Screen::ALL.iter().position(|s| *s == self) {
            Some(i) if i > 0 => Screen::ALL[i - 1],
            _ => self,
        }
    }

    /// Screens outside the linear wizard: they are not in `ALL`, carry no step
    /// number, and own their own back-navigation (Mode → Language, the rest →
    /// Mode). The global "back" keys must defer to them — `prev()` has nowhere
    /// to go from here, so handling it globally either does nothing visible or,
    /// as it did for the font screen, walks off the end of a per-screen array.
    pub fn is_off_linear(self) -> bool {
        (self as usize) >= Screen::ALL.len()
    }
}

/// How the installed system's accounts are set up (user choice on the User
/// screen). Covers the four combinations the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AccountMode {
    /// Create a user; root gets its own separate password.
    UserSeparateRoot,
    /// Create a user; root shares the same password as the user.
    UserSameRoot,
    /// Create a user; root login disabled (sudo-only via wheel).
    UserSudoOnly,
    /// No user at all; log in as root with a root password.
    RootOnly,
}

impl AccountMode {
    pub const ALL: [AccountMode; 4] = [
        AccountMode::UserSameRoot,
        AccountMode::UserSeparateRoot,
        AccountMode::UserSudoOnly,
        AccountMode::RootOnly,
    ];
    pub fn needs_user(self) -> bool {
        !matches!(self, AccountMode::RootOnly)
    }
    pub fn needs_separate_root(self) -> bool {
        matches!(self, AccountMode::UserSeparateRoot | AccountMode::RootOnly)
    }
}

/// Linux kernel choice. Installed during basestrap so the system boots with
/// the selected kernel + matching headers (needed for DKMS modules like the
/// NVIDIA drivers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Kernel {
    Linux,
    Zen,
    Hardened,
    Lts,
}

impl Kernel {
    /// Name for the summary screen.
    pub fn label(self) -> &'static str {
        match self {
            Kernel::Linux => "Linux",
            Kernel::Zen => "Zen",
            Kernel::Hardened => "Hardened",
            Kernel::Lts => "LTS",
        }
    }

    pub fn packages(self) -> &'static [&'static str] {
        match self {
            Kernel::Linux => &["linux", "linux-headers"],
            Kernel::Zen => &["linux-zen", "linux-zen-headers"],
            Kernel::Hardened => &["linux-hardened", "linux-hardened-headers"],
            Kernel::Lts => &["linux-lts", "linux-lts-headers"],
        }
    }
}

/// Which seat manager acquires input/DRM for graphical sessions. Both work; the
/// choice matters because each brings its own per-user session launcher and its
/// own dinit services, and mixing them breaks logins.
///
/// Serialises as "elogind"/"seatd" — the strings this used to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeatProvider {
    Elogind,
    Seatd,
}

impl SeatProvider {
    /// Packages: the daemon plus its Artix dinit service package.
    pub fn packages(self) -> &'static [&'static str] {
        match self {
            SeatProvider::Elogind => &["elogind", "elogind-dinit"],
            SeatProvider::Seatd => &["seatd", "seatd-dinit"],
        }
    }

    /// The dinit service named after the daemon (not after the package).
    pub fn service(self) -> &'static str {
        match self {
            SeatProvider::Elogind => "elogind",
            SeatProvider::Seatd => "seatd",
        }
    }

    /// The per-user launcher that spawns the user's dinit instance on login —
    /// which is what brings up D-Bus, PipeWire and sound. Must match the seat
    /// backend: userspawn belongs to elogind, turnstiled to seatd.
    pub fn user_launcher(self) -> &'static str {
        match self {
            SeatProvider::Elogind => "userspawn",
            SeatProvider::Seatd => "turnstiled",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SeatProvider::Elogind => "elogind",
            SeatProvider::Seatd => "seatd",
        }
    }
}

/// Sentinel size for a manual-mode partition that takes the REST of the free
/// space (root or /home only). Stored in the `manual_*_new_mib` fields, where
/// any other non-zero value is a fixed size in MiB.
pub const MANUAL_REST: u32 = u32::MAX;

/// How the target disk gets its layout.
///
/// `Auto` — the installer takes a whole disk and partitions it itself (the
/// only mode before v243). `Manual` — the partition editor: existing
/// partitions get roles assigned (the dual-boot path, where "the rest of
/// the disk" belongs to another operating system and is never touched), and
/// new partitions can be carved out of the free space with a chosen size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PartitionMode {
    #[default]
    Auto,
    Manual,
    /// Install ALONGSIDE an existing OS with zero manual layout: the installer
    /// reuses the existing ESP and creates a single root partition in the
    /// disk's free space (Bazzite/Ubuntu-style). It shares the manual plan
    /// path (reused ESP, new root, os-prober on) — only the UI differs, filling
    /// the `manual_*` fields automatically instead of by hand.
    Alongside,
}

impl InstallConfig {
    /// Will the installed system have a swap device at all?
    ///
    /// Asked because zswap is a compressed cache IN FRONT OF swap. With no swap
    /// there is nothing behind the cache and nothing for it to do, so offering it
    /// is offering a setting that cannot take effect.
    ///
    /// The answer comes from a different field per mode, and that is the trap:
    /// `swap_gib` defaults to 4 and the manual editor never touches it, so a
    /// manual layout with no swap partition still reads as "4 GiB of swap" if you
    /// ask the auto field. Manual has to be asked about ITS swap — an existing
    /// partition given the swap role, or one planned to be created.
    pub fn has_swap(&self) -> bool {
        if self.partition_mode == PartitionMode::Manual {
            !self.manual_swap.is_empty() || self.manual_swap_new_mib > 0
        } else {
            self.swap_gib > 0
        }
    }
}

impl PartitionMode {
    /// Both non-auto modes drive the manual plan (assign/create partitions
    /// without wiping the table); Alongside just fills the fields for the user.
    pub fn is_manual_family(self) -> bool {
        matches!(self, PartitionMode::Manual | PartitionMode::Alongside)
    }
}

/// Firmware boot mode. Detected from /sys/firmware/efi at startup, overridable
/// on the disk screen (a UEFI machine can still be installed BIOS-style).
///
/// Serialises as "uefi"/"bios" — the strings this used to be, so configs saved
/// by older builds still load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BootMode {
    Uefi,
    Bios,
}

impl BootMode {
    pub fn is_uefi(self) -> bool {
        self == BootMode::Uefi
    }

    /// Label for the summary screen.
    pub fn label(self) -> &'static str {
        match self {
            BootMode::Uefi => "UEFI",
            BootMode::Bios => "BIOS",
        }
    }

    /// The other mode — for the Space/Enter toggle on the disk screen.
    pub fn toggled(self) -> Self {
        match self {
            BootMode::Uefi => BootMode::Bios,
            BootMode::Bios => BootMode::Uefi,
        }
    }
}

/// How the installed system boots. EFISTUB has no bootloader at all — the
/// firmware loads the kernel directly — which is why it's in this enum rather
/// than being modelled as "no bootloader".
///
/// Serialises as "grub"/"refind"/"limine"/"efistub", matching the strings this
/// used to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bootloader {
    Grub,
    Refind,
    Limine,
    Efistub,
}

impl Bootloader {
    /// EFISTUB is the only option that can be Secure Boot-prepared by us: with
    /// no bootloader in the chain, the kernel image itself is what gets signed.
    pub fn supports_secureboot_prep(self) -> bool {
        self == Bootloader::Efistub
    }

    /// Human-readable name, as shown on the options and summary screens. Lives
    /// on the type so the spelling can't drift between the three places that
    /// used to carry their own copy of this mapping.
    pub fn display_name(self) -> &'static str {
        match self {
            Bootloader::Grub => "GRUB",
            Bootloader::Refind => "rEFInd",
            Bootloader::Limine => "Limine",
            Bootloader::Efistub => "EFISTUB",
        }
    }
}

/// One of the fixed GPU driver bundles that head the package list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GpuDriver {
    None,
    Nvidia,
    Nvidia580xx,
    Nouveau,
    Amd,
    Intel,
}

impl GpuDriver {
    /// The three NVIDIA stacks target the same hardware with conflicting kernel
    /// modules, so picking one must drop the others. Intel and AMD combine
    /// freely — with each other and with one NVIDIA stack (hybrid graphics).
    pub fn is_nvidia_family(self) -> bool {
        matches!(
            self,
            GpuDriver::Nvidia | GpuDriver::Nvidia580xx | GpuDriver::Nouveau
        )
    }

    /// True for the proprietary stacks (which need the DKMS/persistence bits);
    /// nouveau is open-source and needs none of that.
    pub fn is_proprietary_nvidia(self) -> bool {
        matches!(self, GpuDriver::Nvidia | GpuDriver::Nvidia580xx)
    }

    /// The exact package set for each bundle (from the spec).
    pub fn packages(self) -> &'static [&'static str] {
        match self {
            GpuDriver::None => &[],
            GpuDriver::Nvidia => &[
                "nvidia-utils-dinit",
                "lib32-nvidia-utils",
                "lib32-opencl-nvidia",
                "nvidia-open-dkms",
                "nvidia-settings",
                "nvidia-utils",
                "opencl-nvidia",
            ],
            GpuDriver::Nvidia580xx => &[
                "opencl-nvidia-580xx",
                "nvidia-utils-dinit",
                "nvidia-580xx-utils",
                "nvidia-580xx-settings",
                "nvidia-580xx-dkms",
                "lib32-opencl-nvidia-580xx",
                "lib32-nvidia-580xx-utils",
                "libva-nvidia-driver",
            ],
            GpuDriver::Nouveau => &[
                // Open-source NVIDIA driver: mesa + the Xorg nouveau DDX, plus
                // NVK (vulkan-nouveau) for Vulkan, and 32-bit counterparts.
                "mesa",
                "xf86-video-nouveau",
                "vulkan-nouveau",
                "lib32-mesa",
                "lib32-vulkan-nouveau",
            ],
            GpuDriver::Amd => &[
                "mesa",
                "xf86-video-amdgpu",
                "vulkan-radeon",
                "lib32-mesa",
                "lib32-vulkan-radeon",
            ],
            GpuDriver::Intel => &[
                "mesa",
                "xf86-video-intel",
                "vulkan-intel",
                "lib32-mesa",
                "lib32-vulkan-intel",
            ],
        }
    }
}

/// Desktop environment choice (spec step 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desktop {
    None,
    KdePlasma,
    Gnome,
    Xfce4,
    Cinnamon,
    Mate,
    Lxqt,
    Lxde,
    Pinnacle,
}

impl Desktop {
    /// All selectable desktops, in display order. `None` first so the default
    /// is a minimal install. GNOME is intentionally excluded: on
    /// Artix (no systemd) both are broken/unstable, so we don't offer them. The
    /// enum variants are kept for compatibility but never listed here.
    pub const ALL: &'static [Desktop] = &[
        Desktop::None,
        // Wayland-capable desktops FIRST (Plasma and LXQt both ship native
        // Wayland sessions), then the X11-only ones.
        Desktop::KdePlasma,
        Desktop::Lxqt,
        Desktop::Pinnacle,
        Desktop::Xfce4,
        Desktop::Cinnamon,
        Desktop::Mate,
        Desktop::Lxde,
    ];

    /// Packages installed for this desktop. Every name/group here is verified
    /// against the stable Artix repositories (system/world/galaxy/lib32).
    /// `plasma`, `gnome`, `mate`, `xfce4`, `lxqt`, `lxde`, `budgie` are package
    /// *groups* (valid as install targets); the rest are concrete packages.
    pub fn packages(self) -> &'static [&'static str] {
        match self {
            Desktop::None => &[],
            // KDE: just the Plasma desktop. We deliberately do NOT pull the
            // huge `kde-applications-meta` group — it depends on kde-office-meta
            // → ghostwriter, which is absent from the stable Artix repos and
            // makes the whole transaction fail. Users add the KDE apps they want
            // via the package search step. konsole+dolphin give a usable base.
            // Exactly as the official Artix install guide specifies:
            //   pacman -S plasma kde-applications
            // These are package *groups* (not meta packages). pacman prompts to
            // pick group members during install; the installer runs with
            // --noconfirm so defaults are taken. Using the groups (rather than
            // kde-applications-meta) avoids the meta package's broken
            // ghostwriter/kde-office-meta dependency.
            Desktop::KdePlasma => &["plasma", "kde-applications"],
            // GNOME: the `gnome` group is the standard full install.
            Desktop::Gnome => &["gnome"],
            // XFCE: group + goodies, as the Artix guide recommends. XFCE 4.20
            // has experimental Wayland support but ships no compositor of its
            // own, so labwc (the upstream-recommended wlroots compositor) plus
            // xorg-xwayland enable the optional Wayland session (startxfce4
            // --wayland); the default X11 session is unaffected.
            // polkit-gnome: the AUTHENTICATION AGENT.
            //
            // `polkit` itself is pulled in by the desktop and runs — but polkit
            // is only the daemon that decides. Something has to ASK, and this
            // desktop ships nothing that does. The result is not an error
            // message: graphical actions needing authorisation simply do
            // nothing, while anything run through doas or sudo in a terminal
            // works fine, because there the terminal does the asking. Reported
            // exactly that way — "some programs from sudo don't work and don't
            // bring up the polkit manager, and some are fine".
            //
            // Plasma has polkit-kde-agent, GNOME has one inside gnome-shell and
            // LXQt has lxqt-policykit in its group; these four have nothing.
            // The package ships no autostart entry either — see the
            // /etc/xdg/autostart file written in install/mod.rs.
            Desktop::Xfce4 => &[
                "xfce4",
                "xfce4-goodies",
                "labwc",
                "xorg-xwayland",
                "polkit-gnome",
            ],
            // Cinnamon is shipped as a single package in galaxy (not a group);
            // a file manager (nemo) makes it usable out of the box. The cinnamon
            // package already ships the experimental Wayland session (Muffin in
            // Wayland mode); xorg-xwayland lets X11 apps run inside it.
            // Cinnamon on Arch is bare: the `cinnamon` group brings the
            // shell, nemo and settings — and nothing you'd call a desktop.
            // No text editor, no archiver, no PDF or image viewer. Linux Mint
            // (the DE's home) fills that with the X-Apps plus a few GNOME
            // tools it keeps at GTK3, so this list mirrors Mint's
            // out-of-the-box selection using packages that exist in the
            // official repos (each verified in extra): xed (editor), xreader
            // (PDF), xviewer (images), celluloid (video), file-roller with
            // its zip/unzip/p7zip backends and the nemo right-click
            // integration, and the calculator / system-monitor / disks /
            // screenshot / scanner / disk-usage set. Deliberately NOT here:
            // pix (AUR-only) and blueberry (drags in the whole bluetooth
            // stack — that's a hardware decision, not a desktop default).
            Desktop::Cinnamon => &[
                "cinnamon",
                "nemo",
                "metacity",
                "lightdm",
                "lightdm-gtk-greeter",
                "xorg-xwayland",
                "xed",
                "xreader",
                "xviewer",
                "celluloid",
                "file-roller",
                "nemo-fileroller",
                "unzip",
                "zip",
                "p7zip",
                "gnome-calculator",
                "gnome-system-monitor",
                "gnome-disk-utility",
                "gnome-screenshot",
                "simple-scan",
                "baobab",
                // See the note above the other desktops: polkit runs, but
                // nothing here asks for the password, so graphical
                // authorisation silently does nothing.
                "polkit-gnome",
            ],
            // MATE: group + extra, per the guide.
            // polkit-gnome: the AUTHENTICATION AGENT.
            //
            // `polkit` itself is pulled in by the desktop and runs — but polkit
            // is only the daemon that decides. Something has to ASK, and this
            // desktop ships nothing that does. The result is not an error
            // message: graphical actions needing authorisation simply do
            // nothing, while anything run through doas or sudo in a terminal
            // works fine, because there the terminal does the asking. Reported
            // exactly that way — "some programs from sudo don't work and don't
            // bring up the polkit manager, and some are fine".
            //
            // Plasma has polkit-kde-agent, GNOME has one inside gnome-shell and
            // LXQt has lxqt-policykit in its group; these four have nothing.
            // The package ships no autostart entry either — see the
            // /etc/xdg/autostart file written in install/mod.rs.
            // MATE HAS ITS OWN AGENT, and it autostarts itself: mate-polkit
            // ships etc/xdg/autostart/polkit-mate-authentication-agent-1.desktop
            // inside the package. Handing MATE polkit-gnome instead was wrong on
            // both counts — a foreign agent, plus an autostart file written by
            // us that the desktop never needed. Checked against the repos with
            // `pacman -F`, not from memory.
            Desktop::Mate => &["mate", "mate-extra", "mate-polkit"],
            // LXQt group + a panel-friendly file manager.
            Desktop::Lxqt => &["lxqt", "pcmanfm-qt"],
            // LXDE group.
            // polkit-gnome: the AUTHENTICATION AGENT.
            //
            // `polkit` itself is pulled in by the desktop and runs — but polkit
            // is only the daemon that decides. Something has to ASK, and this
            // desktop ships nothing that does. The result is not an error
            // message: graphical actions needing authorisation simply do
            // nothing, while anything run through doas or sudo in a terminal
            // works fine, because there the terminal does the asking. Reported
            // exactly that way — "some programs from sudo don't work and don't
            // bring up the polkit manager, and some are fine".
            //
            // Plasma has polkit-kde-agent, GNOME has one inside gnome-shell and
            // LXQt has lxqt-policykit in its group; these four have nothing.
            // The package ships no autostart entry either — see the
            // /etc/xdg/autostart file written in install/mod.rs.
            Desktop::Lxde => &["lxde", "polkit-gnome"],
            // Pinnacle is an AUR package (pinnacle-comp), not in the repos,
            // so it has NO repo packages here — selecting it injects
            // pinnacle-comp into the AUR list and unpacks the bundled config.
            // The repo side — companion tools AND the compositor's own runtime
            // dependencies — is added at plan time in the Pinnacle block of
            // system/install/packages.rs, keeping this catalogue entry empty.
            Desktop::Pinnacle => &[],
        }
    }

    /// Whether this desktop runs as a Wayland compositor (so it needs a seat
    /// manager — seatd or elogind — rather than relying on an X server).
    /// and GNOME's default session, are Wayland; the rest here are X11.
    /// Whether this desktop can run a *stable* Wayland session. As of 2026:
    /// KDE Plasma, GNOME and LXQt (2.3+) have solid Wayland; the others are
    /// Wayland-only. Cinnamon (limited/mixed), MATE (experimental, Wayfire) and
    /// XFCE (only preliminary labwc support, xfwm4 isn't a compositor yet) are
    /// X11-only here — offering Wayland for them risks black screens / broken
    /// configs, so we don't.
    pub fn supports_wayland(self) -> bool {
        matches!(
            self,
            Desktop::Gnome | Desktop::KdePlasma | Desktop::Lxqt | Desktop::Pinnacle
        )
    }

    /// Whether this desktop can run an X11 session.
    pub fn supports_x11(self) -> bool {
        // GNOME and Pinnacle are Wayland-only here. GNOME dropped its
        // X11 session upstream; Pinnacle is a Wayland compositor with
        // no X11 session of their own (they host X11 apps via Xwayland, but the
        // session itself is Wayland). `None` has no session at all.
        !matches!(self, Desktop::None | Desktop::Gnome | Desktop::Pinnacle)
    }

    /// The default session type for this desktop: prefer Wayland where the
    /// desktop's upstream default is Wayland (GNOME, modern Plasma),
    /// otherwise X11.
    pub fn default_session(self) -> &'static str {
        // Wayland is the FIRST choice wherever the desktop ships a native
        // Wayland session (Plasma; LXQt via lxqt-wayland-session + labwc);
        // X11 stays one ←/→ press away.
        if matches!(
            self,
            Desktop::Gnome | Desktop::KdePlasma | Desktop::Lxqt | Desktop::Pinnacle
        ) {
            "wayland"
        } else {
            "x11"
        }
    }

    /// A short tag describing which session(s) this desktop supports, shown in
    /// the list, e.g. "X11", "Wayland", or "X11/Wayland".
    pub fn session_tag(self) -> &'static str {
        match (self.supports_wayland(), self.supports_x11()) {
            (true, true) => "X11/Wayland",
            (true, false) => "Wayland",
            (false, true) => "X11",
            (false, false) => "",
        }
    }

    /// Short label shown in the picker.
    pub fn label(self) -> &'static str {
        match self {
            Desktop::None => "None / minimal (no desktop)",
            Desktop::KdePlasma => "KDE Plasma",
            Desktop::Gnome => "GNOME 50 ([!] may be unstable)",
            Desktop::Xfce4 => "XFCE4",
            Desktop::Cinnamon => "Cinnamon",
            Desktop::Mate => "MATE",
            Desktop::Lxqt => "LXQt",
            Desktop::Lxde => "LXDE",
            Desktop::Pinnacle => "Pinnacle (Wayland, AUR · AwesomeWM-like)",
        }
    }

    /// An optional caveat shown under the highlighted desktop. Empty when there
    /// is nothing to warn about.
    pub fn note(self) -> &'static str {
        match self {
            // GNOME upstream leans heavily on systemd; on a systemd-free distro
            // like Artix it's community-maintained, lags upstream, can be flaky.
            Desktop::Gnome => {
                "GNOME 50. GNOME upstream depends heavily on systemd; on Artix \
                 (no systemd) it is community-maintained and may be unstable. \
                 Consider KDE Plasma or XFCE4 for a smoother experience."
            }
            _ => "",
        }
    }
}

/// Services that must be enabled in the installed system (spec).
// Full PipeWire audio stack — always installed so the system has complete,
// working sound (ALSA/JACK/PulseAudio compatibility, camera, video). The lib32-*
// packages need multilib (we enable [lib32] before basestrap). The user dinit
// services (pipewire/wireplumber/pipewire-pulse) are enabled per-user at
// install time; these are the daemon + plugin packages they rely on.
pub const AUDIO_PACKAGES: &[&str] = &[
    "pipewire",
    "libpipewire",
    "pipewire-audio",
    "pipewire-pulse",
    "pipewire-alsa",
    "pipewire-jack",
    "pipewire-v4l2",
    // pipewire-libcamera is DELIBERATELY absent. As of July 2026 it file- and
    // dependency-conflicts with current wireplumber after a system update, and
    // the visible symptom is the worst kind: sound simply doesn't work on the
    // freshly installed system (field report from live machines, 2026-07-16).
    // Camera-portal support can be added back once the packaging settles;
    // audio must not be hostage to a webcam plugin.
    "pipewire-ffado",
    "pipewire-roc",
    "gst-plugin-pipewire",
    "wireplumber",
    "lib32-libpipewire",
    "lib32-pipewire",
    "lib32-pipewire-jack",
    "lib32-pipewire-v4l2",
];

// dinit service *packages* to INSTALL (these provide the service files). These
// are real package names with the -dinit suffix.
pub const DINIT_PACKAGES: &[&str] = &[
    "dbus-dinit",
    "dbus-dinit-user",
    // NB: the per-user dinit launcher is turnstile (turnstile-dinit), added in
    // base_packages. We deliberately do NOT install userspawn-dinit here — it
    // CONFLICTS with turnstile-dinit (both are user-dinit backends and can't
    // coexist), and userspawn only works with elogind anyway, so turnstile is
    // used for every seat backend instead.
    "networkmanager-dinit",
    "bluez-dinit",
    "avahi-dinit",
    "ntp-dinit",
    // Skipped at install time when the user unticks the default-checked
    // nftables entry in the packages screen.
    "nftables-dinit",
    "pipewire-dinit",
    "pipewire-pulse-dinit",
    "wireplumber-dinit",
    "power-profiles-daemon-dinit",
];

// dinit SYSTEM service names to ENABLE in the installed system. CRITICAL: these
// are the names of the service FILES in /etc/dinit.d, which are named after the
// daemon — NOT the package (package "networkmanager-dinit" → service
// "NetworkManager"). Enabling a package name is a silent no-op, so the service
// never starts. pipewire/wireplumber/dbus session bits are USER services
// (/etc/dinit.d/user) started per-login, so they are NOT enabled here.
pub const DINIT_SERVICES: &[&str] = &[
    "dbus",           // system D-Bus
    "NetworkManager", // networking (capital N — that's the real service name)
    "bluetoothd",     // bluetooth daemon
    "avahi-daemon",   // mDNS/zeroconf
    "ntpd",           // time sync
    "nftables",       // firewall (loads our /etc/nftables.conf); skipped if unticked
    "power-profiles-daemon", // CPU/power profile switching (used by KDE/GNOME)
                      // NB: the per-user dinit launcher (turnstiled) is enabled separately in
                      // install.rs, not here — it pairs with seat/PAM setup. We do NOT enable
                      // userspawn: it depends on elogind's logind D-Bus signals and so never
                      // fires on a seatd/no-elogind system (turnstile is used instead).
];

/// Everything the user picks across the wizard. Persisted to JSON before the
/// install phase so the actual installer step can run from a single struct.
/// A storage assignment beyond the system disk. Two kinds, distinguished by
/// `format`:
///   • format == true  — `disk` is a WHOLE empty disk: it gets wiped, given one
///     GPT partition, formatted with `fs`, and mounted at `mountpoint`. Used for
///     a separate /home or a storage/"dump" disk.
///   • format == false — `disk` is an EXISTING partition (e.g. an NTFS Windows
///     volume): it is mounted at `mountpoint` AS-IS, preserving all data, never
///     formatted. `fs` is the detected filesystem (e.g. "ntfs").
/// In both cases fstab records it so it mounts on every boot. The system disk
/// itself is never represented here.
/// An existing partition the user marked for deletion in the manual editor.
///
/// Carries its DISK as well as its path, taken straight from the lsblk scan.
/// Deriving the disk from the path by string surgery is not safe enough for
/// something that feeds `sgdisk -d`: "/dev/nvme0n1" is a whole disk but ends in
/// a digit, so it is indistinguishable from a partition by name alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeletedPart {
    pub path: String,
    pub disk: String,
    /// The partition's GPT UUID at the moment it was marked.
    ///
    /// The plan checks this before deleting anything. A partition NUMBER is not
    /// an identity — GPT hands freed slots to new partitions, and this installer
    /// invites the user to repartition externally (cfdisk on console 2) while
    /// the editor is open. Without the check, "delete partition 3" could reach
    /// a different partition 3 than the one that was marked. Empty when lsblk
    /// reported none, in which case the deletion falls back to the number.
    #[serde(default)]
    pub partuuid: String,
}

/// A NEW partition the manual editor will create as plain data storage.
///
/// The five install roles are single slots; data partitions are not — any
/// number of them can be carved, across any disks. Only the creation lives
/// here (path is predicted, like the role slots); the mountpoint, filesystem
/// and formatting ride in `extra_disks`, keyed by `path`, so the existing
/// extra-disk machinery does the mkfs, mount and fstab work unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DataPart {
    pub path: String,
    pub disk: String,
    pub mib: u32,
}

/// How a whole disk is erased before it is repartitioned. The manual editor can
/// mark a disk for one of these; the erase runs either immediately ("now",
/// straight from the editor behind its own confirmation) or as the first steps
/// of the install plan ("later"). Either way a wiped disk is treated as EMPTY
/// for numbering, so partitions created on it start at 1 — which is the whole
/// reason the feature exists: a reused disk keeps its old partition numbers, so
/// the first "new" partition would otherwise be #4, not #1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum WipeMethod {
    /// `wipefs -a` + `sgdisk --zap-all`: the table and every filesystem
    /// signature go, the data blocks stay (recoverable). Instant — this is the
    /// numbering-reset default and the only one that does not destroy data.
    #[default]
    Quick,
    /// `shred -z`: one zeroing pass over the whole device. Slow (an HDD is
    /// hours) but the data is genuinely gone. The spinning-disk choice.
    ZeroHdd,
    /// `nvme format` (NVMe) or `blkdiscard` TRIM (SATA SSD): fast, wipes all
    /// data by resetting the drive's contents. The solid-state choice.
    CryptoSsd,
    /// ATA Secure Erase through the drive controller (hdparm). A frozen drive is
    /// best-effort thawed (a brief suspend, only on the "now" path); if that
    /// fails the erase FALLS BACK to blkdiscard/shred so the install is never
    /// aborted on a stubborn controller.
    AtaSecure,
}

impl WipeMethod {
    /// Stable slug — the i18n key suffix (`parts.wipe_<id>*`) and the token the
    /// plan-text tests match on.
    pub fn id(self) -> &'static str {
        match self {
            WipeMethod::Quick => "quick",
            WipeMethod::ZeroHdd => "zero",
            WipeMethod::CryptoSsd => "ssd",
            WipeMethod::AtaSecure => "ata",
        }
    }
}

/// A disk the manual editor marked to be fully erased as part of the install
/// (the "later" path). Self-contained: it carries the rotational flag and bus
/// transport captured from the scan, so `build_manual_plan` can emit the right
/// erase command without re-reading hardware at plan time.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WipeMark {
    pub disk: String,
    pub method: WipeMethod,
    /// lsblk ROTA at mark time: true = HDD. Chooses the fallback erase.
    #[serde(default)]
    pub rota: bool,
    /// lsblk TRAN at mark time: "nvme", "sata", … Picks nvme-format vs blkdiscard.
    #[serde(default)]
    pub tran: String,
}

/// Which filesystem the options modal (btrfs subvolumes/snapshots/compression)
/// is configuring: the root filesystem, a manual /home on its own partition, or
/// a plain data partition. `ManualData` names no device so this stays `Copy` —
/// which partition it means is `App::fs_opts_data_dev`, set when the modal opens.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FsOptsTarget {
    #[default]
    Root,
    ManualHome,
    ManualData,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExtraDisk {
    pub disk: String,       // "/dev/sdb" (whole disk) or "/dev/sdb1" (partition)
    pub mountpoint: String, // e.g. "/home", "/data", "/mnt/windows"
    pub fs: String,         // target fs (format) or detected fs (existing mount)
    #[serde(default)]
    pub format: bool, // true: wipe+partition+mkfs; false: mount existing as-is
    #[serde(default)]
    pub whole_disk: bool, // format target is a whole disk (repartition) vs an
    // existing partition reformatted in place (mkfs only)
    #[serde(default)]
    pub noatime: bool, // mount with noatime (format disks)
    #[serde(default)]
    pub compress: bool, // btrfs zstd compression (format disks, btrfs only)
    #[serde(default)]
    pub encrypt: bool, // LUKS-encrypt this format disk (keyfile auto-unlock)
    #[serde(default)]
    pub bookmark: bool, // mounted custom folder: add a GTK bookmark so it's
    // visible in the file manager sidebar
    #[serde(default)]
    pub mount_base: String, // "", "home" (/home/<user>/name), "mnt" (/mnt/name),
    // or "custom" (the user types a full path)
    #[serde(default)]
    pub mount_name: String, // folder name (home/mnt) or full path (custom)
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InstallConfig {
    pub lang: String,     // "en" | "uk" — UI language
    pub locale: String,   // e.g. "uk_UA.UTF-8" — system locale
    pub timezone: String, // e.g. "Europe/Kyiv"
    /// Is the machine's hardware clock kept in UTC?
    ///
    /// Linux alone: yes, and that is the default. Beside Windows: no — Windows
    /// keeps the RTC in LOCAL time, and two systems disagreeing about it makes
    /// the clock jump by the timezone offset every time you switch.
    ///
    /// The hidden GRUB menu offered this as `utc=`; it is here because that menu
    /// is no longer shown, and the choice still has to exist somewhere.
    pub rtc_utc: bool,
    /// Compressed swap cache in RAM. Off by default: it is a real win on a
    /// machine short of memory and a needless complication on one that is not.
    pub zswap: bool,
    pub zswap_compressor: String,
    /// Share of RAM zswap may fill, as a PERCENTAGE — the same number then means
    /// the same thing on a 2 GB netbook and a 32 GB desktop.
    pub zswap_percent: u8,
    /// Kill the biggest memory hog before the kernel's OOM killer would, while
    /// the machine can still respond.
    pub earlyoom: bool,
    /// Free-memory percentage at which earlyoom acts.
    pub earlyoom_percent: u8,

    pub keymap: String, // console keymap, e.g. "ua"
    /// Console font for the INSTALLED system's TTY, chosen on the font screen.
    /// Defaults to the one the installer itself starts with; every option is
    /// verified to draw all three interface languages.
    pub console_font: String,
    pub xkb_layouts: Vec<String>,
    pub kernel: Kernel,
    pub desktops: Vec<String>, // serialized Desktop names (multi-select; empty = headless)
    /// Chosen session type for desktops that support both: "wayland" or "x11".
    /// Ignored for single-session desktops. Default follows Desktop::default_session.
    pub session: String,
    /// Seat/session manager: "elogind" (universal — works with X11 and Wayland,
    /// integrates with PAM/polkit; the safe default) or "seatd" (minimal, seat
    /// management only). Default "elogind".
    pub seat_provider: SeatProvider,
    /// GPU driver bundles to install. A SET, not one choice: hybrid graphics
    /// (an NVIDIA dGPU next to an Intel/AMD iGPU) needs both stacks. Empty is
    /// impossible — the screen falls back to `[GpuDriver::None]`.
    pub gpu: Vec<GpuDriver>,
    pub extra_packages: Vec<String>,
    /// The "slayfetch" logo variant chosen in the packages screen (e.g.
    /// "Rainbow", "Transgender"). Empty unless "slayfetch" is selected; then it
    /// names which Artix+pride logo replaces fastfetch's default. slayfetch is
    /// the ordinary fastfetch package with this cosmetic swap.
    #[serde(default)]
    pub fastfetch_logo: String,
    /// Additional whole disks assigned to mountpoints (e.g. /home on a separate
    /// HDD, or a storage disk at /data). Empty on single-disk systems. The
    /// system disk (`disk` above) is never included here.
    #[serde(default)]
    pub extra_disks: Vec<ExtraDisk>,
    /// AUR packages the user typed (space-separated), built via paru at the end
    /// of install as the new user. Empty = none.
    pub aur_packages: Vec<String>,
    pub disk: String, // e.g. "/dev/sda"
    pub boot_mode: BootMode,
    /// If true, the user gets passwordless sudo (NOPASSWD); else password each time.
    pub passwordless_sudo: bool,
    /// Privilege-escalation tool: false = sudo (default), true = doas (an
    // OpenBSD-derived, minimal-config alternative). The passwordless choice
    // applies to whichever is selected.
    pub use_doas: bool,
    /// If true, enable the Chaotic-AUR binary repository (prebuilt AUR packages,
    /// maintained by the Garuda Linux team) by installing its key/keyring/
    /// mirrorlist and appending [chaotic-aur] to pacman.conf.
    pub chaotic_aur: bool,
    /// AURIS — the Artix User Repository of Init Scripts.
    ///
    /// Init scripts for s6, OpenRC, runit and dinit that upstream does not ship,
    /// contributed by users. On a distro whose whole point is the init system,
    /// a missing service file is what stops a package from being usable at all,
    /// so this is a different kind of repository from Chaotic-AUR: not "more
    /// software", but "the piece that makes software work here".
    pub auris: bool,

    /// If true, rank pacman mirrors by speed before installing (and copy the
    /// optimized lists to the target). Region is seeded from the chosen
    /// timezone, then mirrors are ranked by real connection speed.
    pub optimize_mirrors: bool,
    /// If true, encrypt the root partition with LUKS.
    pub encrypt_disk: bool,
    /// Run os-prober at grub-mkconfig so other OSes (e.g. Windows) get a boot
    /// menu entry. GRUB-only; off by default (GRUB 2.06+ disables it itself).
    #[serde(default)]
    pub os_prober: bool,
    /// EFISTUB only: prepare (but do NOT enable) Secure Boot — install sbctl and
    /// generate signing keys in the chroot, write bilingual instructions to the
    /// user's home, and leave enrollment + signing + the mandatory UEFI Setup-Mode
    /// step to the user on first boot. Never enrolls keys or enables Secure Boot
    /// automatically (that can't be done safely from the installer and risks
    /// bricking some firmware).
    pub prepare_secureboot: bool,
    /// LUKS passphrase (when encrypt_disk). Held only in memory during install.
    pub luks_passphrase: String,
    /// Encryption scope: "root" (encrypt root only, /boot stays plaintext) or
    /// "full" (encrypt /boot too; GRUB unlocks it at boot via cryptodisk).
    pub encrypt_scope: String,
    pub swap_gib: u32, // 0 = no swap; default 4
    /// Filesystem for the root partition: ext4, btrfs, xfs, f2fs, etc.
    pub root_fs: String,
    /// Auto: installer partitions `disk` itself. Manual: the `manual_*` fields
    /// below name pre-made partitions to use. See `PartitionMode`.
    #[serde(default)]
    pub partition_mode: PartitionMode,
    /// Manual: the EFI System Partition to use (e.g. "/dev/nvme0n1p1").
    #[serde(default)]
    pub manual_esp: String,
    /// Manual: reformat the ESP (true) or keep it as-is (false — the safe
    /// default: a Windows ESP must survive a dual-boot install).
    #[serde(default)]
    pub manual_esp_format: bool,
    /// Manual: where the ESP is mounted — "/boot" (default: kernels live on the
    /// ESP, needs a roomy ESP) or "/boot/efi" (kernels stay on the root
    /// filesystem, only the bootloader goes on the ESP). The dual-boot default
    /// people expect: a small reused Windows ESP (100–300 MiB) can't hold Linux
    /// kernels, so "/boot/efi" is the layout that actually fits.
    #[serde(default)]
    pub manual_esp_mount: String,
    /// Manual: the root partition. Always formatted.
    #[serde(default)]
    pub manual_root: String,
    /// Manual: filesystem for root ("ext4"/"btrfs"/"xfs").
    #[serde(default)]
    pub manual_root_fs: String,
    /// Manual: swap partition; "" = none.
    #[serde(default)]
    pub manual_swap: String,
    /// Manual: separate /home partition (any disk); "" = none. Formatted.
    #[serde(default)]
    pub manual_home: String,
    /// Manual: filesystem for the /home partition.
    #[serde(default)]
    pub manual_home_fs: String,
    /// Manual: the disk NEW partitions are created on ("" = none chosen yet).
    #[serde(default)]
    pub manual_disk: String,
    /// Manual: sizes in MiB for roles whose partition the installer CREATES in
    /// the free space of `manual_disk`. 0 = the role uses an existing
    /// partition (or is absent); `MANUAL_REST` = take the rest of the disk
    /// (root/home only). The `manual_esp`/`manual_root`/… fields still hold
    /// the device path — predicted at assignment time from the partition
    /// numbering — so everything downstream stays path-based and cannot tell
    /// a created partition from a pre-made one.
    #[serde(default)]
    pub manual_esp_new_mib: u32,
    #[serde(default)]
    pub manual_root_new_mib: u32,
    #[serde(default)]
    pub manual_swap_new_mib: u32,
    #[serde(default)]
    pub manual_home_new_mib: u32,
    /// Optional per-filesystem features chosen on the disk screen, all default
    /// off ("standard" filesystem). noatime applies to any filesystem (less
    /// write wear, SSD-friendly). btrfs: subvolumes (@/@home/@snapshots/@log/
    /// @cache, snapshot-ready), transparent zstd compression, and async discard
    /// (SSD TRIM). f2fs: zstd compression. Applied at format/mount time;
    /// fstabgen then captures the mount options into fstab.
    #[serde(default)]
    pub mount_noatime: bool,
    #[serde(default)]
    pub btrfs_subvolumes: bool,
    /// Auto-snapshots via snapper + snap-pac (pre/post pacman snapshots). Only
    /// valid together with btrfs_subvolumes (the @/@snapshots layout); the UI
    /// keeps the two in sync.
    #[serde(default)]
    pub btrfs_snapshots: bool,
    #[serde(default)]
    pub btrfs_compress: bool,
    #[serde(default)]
    pub btrfs_discard: bool,
    /// btrfs options for a MANUAL /home on its own partition. Separate from the
    /// `btrfs_*` fields above, which describe the ROOT filesystem: a dedicated
    /// /home is plain mkfs + mount (the @home subvolume is skipped for it, and
    /// snapper is wired to the root's @/@snapshots), so only the two mount-time
    /// options mean anything there — and they must not be shared with root, or
    /// configuring one silently reconfigures the other.
    /// Existing partitions the user marked for DELETION in the manual editor.
    ///
    /// Marks only — nothing is destroyed while editing. The paths become
    /// `sgdisk -d` actions in the install plan, so they run at the same moment
    /// as every other destructive step: after the final "this will erase…"
    /// confirmation. Until then the mark is a toggle, so a user who reviews the
    /// summary and changes their mind can walk back and un-delete.
    /// BIOS-boot partition for a legacy (non-UEFI) manual install on GPT.
    ///
    /// GRUB embeds core.img in a 1 MiB type-EF02 partition when booting BIOS
    /// from a GPT disk; there is no filesystem on it and it is never mounted.
    /// Not needed on an MBR table (GRUB uses the post-MBR gap) and irrelevant
    /// under UEFI, where the ESP does this job.
    /// The disk the bootloader is installed onto in manual mode.
    ///
    /// Resolved from the SCAN by the editor (never by parsing a device name)
    /// and kept in step with whichever partition holds the root. BIOS GRUB
    /// needs a whole-disk target — `grub-install --target=i386-pc /dev/sdX` —
    /// and `manual_disk` only exists when the user created something.
    /// Manual partitioning for a SINGLE OS (no dual boot).
    ///
    /// Same editor, different promises. The dual-boot manual path is pinned to
    /// GRUB with no LUKS because it shares a disk with somebody else's OS; a
    /// solo manual install has no such neighbour, so encryption and the other
    /// bootloaders are available exactly as they are for a whole-disk install.
    #[serde(default)]
    pub manual_solo: bool,
    #[serde(default)]
    pub manual_boot_disk: String,
    /// The DISK each planned new partition is carved from, per role.
    ///
    /// New partitions used to be restricted to a single disk (`manual_disk`),
    /// because their device paths are predicted from one disk's numbering. That
    /// blocked an ordinary layout — root on one drive, a fresh /home on
    /// another — so each slot now records its own disk and the numbering is
    /// done per disk.
    #[serde(default)]
    pub manual_esp_disk: String,
    #[serde(default)]
    pub manual_root_disk: String,
    #[serde(default)]
    pub manual_swap_disk: String,
    #[serde(default)]
    pub manual_home_disk: String,
    #[serde(default)]
    pub manual_bios_disk: String,
    #[serde(default)]
    pub manual_bios: String,
    #[serde(default)]
    pub manual_bios_new_mib: u32,
    #[serde(default)]
    pub manual_deleted: Vec<DeletedPart>,
    /// Disks the manual editor marked to be fully erased as part of the install
    /// (the "later" path) — see [`WipeMark`]. A disk listed here is treated as
    /// empty when numbering new partitions, and the plan erases it before it
    /// deletes or creates anything. A "now" wipe is not stored here: it runs
    /// immediately and the rescan simply shows an empty disk.
    #[serde(default)]
    pub manual_wipe: Vec<WipeMark>,
    /// New data-storage partitions the manual editor will create — see
    /// [`DataPart`]. Their mount configuration lives in `extra_disks`.
    #[serde(default)]
    pub manual_data_new: Vec<DataPart>,
    #[serde(default)]
    pub manual_home_compress: bool,
    /// LUKS-encrypt the manual /home slot.
    ///
    /// Expressed by handing the slot to the extra-disk machinery rather than by
    /// growing a second crypto path here: that chain (LUKS format, keyfile,
    /// crypttab entry, mount) is already written and already tested, and a
    /// parallel implementation of it would be one more thing to get subtly
    /// wrong on a disk that holds somebody's files.
    pub manual_home_encrypt: bool,
    #[serde(default)]
    pub manual_home_discard: bool,
    /// Bootloader to install: "grub" (default), "refind", or "limine".
    /// Only GRUB can decrypt an encrypted /boot, so full-disk encryption forces
    /// GRUB; the others are offered for plaintext and root-only LUKS setups.
    pub bootloader: Bootloader,
    /// Display manager: "sddm" (default for graphical desktops), "none", or a
    /// greetd greeter: "agreety", "tuigreet", "gtkgreet", "regreet", "wlgreet"
    /// (the greetd variants from the Arch Wiki that exist as repo packages).
    pub display_manager: String,
    /// USB stick to hold an auto-unlock LUKS keyfile ("" = off). The stick is
    /// REFORMATTED (FAT32, label ARTIXKEY), a random 4096-byte key is added to
    /// the root container as an extra slot, and the initramfs `encrypt` hook
    /// reads it via cryptkey=LABEL=ARTIXKEY. Stick present at boot → unlocks
    /// silently; absent → falls back to the passphrase prompt.
    pub usb_key_device: String,
    /// USB-keyfile mode: false (default) keeps the passphrase as a backup LUKS
    /// slot; true REMOVES the passphrase slot after the key is written, so the
    /// stick becomes the ONLY way to unlock the disk. Losing it = losing the
    /// data — guarded by a permanent red warning in the UI.
    pub usb_key_only: bool,
    /// EFI bootloader id — the name shown in the UEFI boot menu (grub-install
    /// --bootloader-id). Default "Artix".
    pub bootloader_id: String,
    pub username: String,
    /// The machine hostname (written to /etc/hostname and /etc/hosts).
    pub hostname: String,
    // Passwords are kept only in memory in App, never serialized to disk.
    #[serde(skip)]
    pub user_password: String,
    #[serde(skip)]
    pub root_password: String,
    pub root_same_as_user: bool,
    /// Serialized AccountMode (e.g. "UserSeparateRoot").
    pub account_mode: AccountMode,
}

impl Default for InstallConfig {
    fn default() -> Self {
        InstallConfig {
            lang: "uk".into(),
            locale: "uk_UA.UTF-8".into(),
            timezone: crate::screens::timezone::first_zone().into(),
            rtc_utc: true,
            zswap: false,
            zswap_compressor: "zstd".into(),
            zswap_percent: 20,
            earlyoom: false,
            earlyoom_percent: 10,
            keymap: "ua".into(),
            console_font: crate::screens::fontpick::default_font().into(),
            xkb_layouts: vec!["ua".into(), "gb".into()],
            kernel: Kernel::Linux,
            desktops: Vec::new(),
            session: "wayland".into(),
            seat_provider: SeatProvider::Elogind,
            gpu: vec![GpuDriver::None],
            // Default-checked package set: pre-selected on a fresh run, all
            // ordinary entries the user can untick in the packages screen.
            // The distro's own configs ride on these: zsh → shipped .zshrc +
            // starship (and becomes the login shell), kitty → Catppuccin
            // kitty.conf, fastfetch → the syrnyk-logo config, nftables → the
            // embedded default-deny firewall; octopi is the friendly pacman
            // GUI. Untick zsh (and fish) to stay on bash.
            fastfetch_logo: String::new(),
            extra_packages: vec![
                // Genuine X.Org, on by default. It is a MARKER, not a package
                // name — see effective_packages. Untick it and the installer
                // names no X package at all, leaving the choice to the user.
                "xorg".into(),
                "zsh".into(),
                "kitty".into(),
                "fastfetch".into(),
                "nftables".into(),
                "octopi".into(),
            ],
            extra_disks: Vec::new(),
            aur_packages: Vec::new(),
            disk: String::new(),
            boot_mode: BootMode::Uefi,
            partition_mode: PartitionMode::Auto,
            manual_esp: String::new(),
            manual_esp_format: false,
            // /boot/efi is the dual-boot-friendly default: kernels stay on the
            // root filesystem, only the bootloader lands on the ESP — so a small
            // reused Windows ESP still fits.
            manual_esp_mount: "/boot/efi".into(),
            manual_root: String::new(),
            // ext4 first/default: the simplest, most familiar filesystem.
            manual_root_fs: "ext4".into(),
            manual_swap: String::new(),
            manual_home: String::new(),
            manual_home_fs: "ext4".into(),
            manual_disk: String::new(),
            manual_esp_new_mib: 0,
            manual_root_new_mib: 0,
            manual_swap_new_mib: 0,
            manual_home_new_mib: 0,
            passwordless_sudo: false,
            use_doas: false,
            auris: false,
            chaotic_aur: false,
            optimize_mirrors: true,
            encrypt_disk: false,
            os_prober: false,
            prepare_secureboot: false,
            luks_passphrase: String::new(),
            encrypt_scope: "root".into(),
            swap_gib: 4,
            root_fs: "ext4".into(),
            mount_noatime: false,
            btrfs_subvolumes: false,
            btrfs_snapshots: false,
            btrfs_compress: false,
            btrfs_discard: false,
            manual_solo: false,
            manual_boot_disk: String::new(),
            manual_esp_disk: String::new(),
            manual_root_disk: String::new(),
            manual_swap_disk: String::new(),
            manual_home_disk: String::new(),
            manual_bios_disk: String::new(),
            manual_bios: String::new(),
            manual_bios_new_mib: 0,
            manual_deleted: Vec::new(),
            manual_wipe: Vec::new(),
            manual_data_new: Vec::new(),
            manual_home_compress: false,
            manual_home_encrypt: false,
            manual_home_discard: false,
            bootloader: Bootloader::Grub,
            display_manager: "sddm".into(),
            usb_key_device: String::new(),
            usb_key_only: false,
            bootloader_id: "Artix".into(),
            username: String::new(),
            hostname: "artix".into(),
            user_password: String::new(),
            root_password: String::new(),
            root_same_as_user: true,
            account_mode: AccountMode::UserSameRoot,
        }
    }
}

/// Top-level app state.
pub struct App {
    pub screen: Screen,
    pub lang: Lang,
    pub config: InstallConfig,
    pub should_quit: bool,
    /// Leave the installer and reboot, rather than dropping to a shell.
    ///
    /// After an install the machine has to start the system that was just
    /// written, and nothing offered a way there: the only exits were `q` on the
    /// first screen and the power button.
    pub pending_reboot: bool,
    /// Reboot INTO the firmware setup. In a VM the ISO is still the boot
    /// device, so reaching the boot order is the difference between testing the
    /// installed system and reinstalling it again.
    pub pending_firmware: bool,
    /// Per-screen UI state (list cursors etc.) lives in the screen modules,
    /// but a shared scratch index is handy for simple list screens.
    pub cursor: usize,
    /// When true, the seat/login-manager choice modal is open on the Desktop
    /// step, forcing an explicit pick before continuing. Cursor: 0 = elogind
    /// (default, recommended), 1 = seatd.
    pub seat_modal_open: bool,
    pub seat_modal_cursor: usize,
    /// True once the user has confirmed a seat backend in the modal (Enter).
    /// Until then, apply_desktop_defaults() is free to set seat_provider from
    /// the chosen desktop set (Wayland → seatd, X11 → elogind); after, the
    /// explicit pick is locked and changing the desktop set never reverts it.
    /// This distinguishes "user picked elogind" from "still on the default
    /// elogind", which a bare seat_provider string cannot.
    pub seat_chosen: bool,
    /// Disk screen: the pre-flight warnings modal (live medium / too small /
    /// UEFI-BIOS mismatch) is open. Advisory only — closing it doesn't change
    /// any choice.
    pub disk_warn_modal_open: bool,
    /// Scroll offset (in rows) for the pre-flight warnings modal body.
    pub disk_warn_scroll: u16,
    /// Summary screen: the final "this will irreversibly erase disk X" modal is
    /// open. Opened by Enter on the review; confirming in it actually starts the
    /// install, so formatting never begins without this explicit second step.
    pub confirm_format_open: bool,
    /// Focus on the Desktop screen: 0 = the DE list, 1 = the session row,
    /// 2 = the login-screen (DM) row. ↑/↓ moves between them, ←/→ changes the
    /// focused row's value. Keeps the whole screen on arrows/space/enter/esc.
    pub de_focus: usize,
    /// Whether the currently shown screen considers its input valid enough to
    /// advance. Screens set this in their update logic.
    pub can_advance: bool,
    /// Frame counter, bumped once per render. Drives subtle animations like the
    /// shimmering install progress bar. Wraps harmlessly.
    pub frame: u64,
    /// Rolling install log lines for the Summary screen's scrollback.
    pub log: Vec<String>,
    pub log_scroll: u16,
    pub log_follow: bool,
    /// Remembered cursor row per screen, indexed by `Screen as usize`.
    ///
    /// `cursor` is ONE field shared by a dozen screens, so navigation has to
    /// clear it or a row index would leak from one list into another. Clearing
    /// it alone, though, means walking back into a step always lands at the top
    /// — you leave the desktop step on the login-screen row and come back to the
    /// desktop list. This remembers each screen's row so a step hands itself
    /// back the way you left it.
    /// One slot per `Screen`, indexed by `Screen as usize` — so it MUST cover
    /// every variant, including the ones outside `ALL`. It was sized 19 by hand
    /// while the enum grew to 21, and `Esc` on the font screen then indexed past
    /// the end: that panic killed the process and dropped the user back to the
    /// console, which reads as "the installer threw me out". `every_screen_has_a_nav_slot`
    /// is what keeps the two in step now.
    pub nav_cursor: [usize; Screen::COUNT],
    /// Scroll offset for the Summary review list. The list of choices is ~25
    /// lines and the panel on an 80x24 console shows ~11, so without this the
    /// user cannot read half of what they are about to commit to.
    pub summary_scroll: u16,
    /// When Down was last pressed in the log view, for detecting a quick
    /// double-tap (which snaps to the tail and re-enables live follow).
    pub log_last_down: Option<std::time::Instant>,
    pub log_live: bool, // last on-screen log line is the live download indicator

    // ── Per-screen UI state ──
    pub tz_query: String,
    pub kb_query: String,
    pub user_confirm: String,
    pub root_confirm: String,
    pub user_focus: usize,

    // Wi-Fi
    pub wifi_stage: crate::screens::wifi::Stage,
    pub wifi_adapters: Vec<String>,
    pub wifi_adapter: String,
    pub wifi_networks: Vec<String>,
    pub wifi_ssid: String,
    pub wifi_password: String,
    /// One-line live status under the Wi-Fi lists: retry hints, daemon and
    /// connection errors. Empty = nothing to say. Rendered by wifi::draw.
    pub wifi_status: String,
    /// Whether `wifi_status` is an error (warn colour) or plain info (dim).
    pub wifi_status_is_error: bool,
    /// True when the status line announces a VERIFIED successful connection.
    /// Doubles as the "Enter now advances" flag for the Password stage — the
    /// user confirms they've seen the success instead of being silently
    /// teleported to the next screen.
    pub wifi_status_success: bool,
    /// Output of the embedded Wi-Fi test harness (mode screen → "Wi-Fi test").
    pub wifitest_log: Vec<String>,
    /// True while the harness is running, so the screen can say "working…".
    pub wifitest_running: bool,
    /// The Wi-Fi self-test runs in a worker thread (modprobe + hostapd + two
    /// sleeps ≈ 5 s); this channel delivers its collected log to `tick()`.
    pub wifitest_rx: Option<crossbeam_channel::Receiver<Vec<String>>>,
    /// Drive-wear (TBW) inspector (mode screen → "Drive wear"): the parsed
    /// per-disk rows, whether a scan is running, whether one has been kicked off
    /// for this visit, and the worker channel. smartctl runs off the UI thread.
    pub tbwtest_rows: Vec<crate::screens::tbwtest::DriveWear>,
    pub tbwtest_running: bool,
    pub tbwtest_scanned: bool,
    pub tbwtest_rx: Option<crossbeam_channel::Receiver<Vec<crate::screens::tbwtest::DriveWear>>>,
    /// Manual-partitioning editor: the role-assignment modal (open flag +
    /// cursor), and the device path of the partition row it is editing.
    pub parts_modal_open: bool,
    pub parts_modal_cursor: usize,
    pub parts_target: String,
    /// Manual-partitioning editor: cursor into the disk list on the disk row,
    /// and whether the full-disk-list picker modal is open (a clear list of
    /// every disk, so multiple disks aren't hidden behind a one-at-a-time
    /// cycler).
    pub parts_disk_cursor: usize,
    pub parts_disk_modal_open: bool,
    /// Create-partition modal: open flag, wizard stage (0 = role, 1 = size),
    /// the chosen role, and the size being typed (whole GiB, digits only).
    pub parts_create_open: bool,
    pub parts_create_stage: u8,
    pub parts_create_role: usize,
    pub parts_size_input: String,
    /// The size box still holds the SUGGESTION and has not been typed into.
    ///
    /// The first digit must REPLACE that suggestion, not extend it: a swap box
    /// pre-filled with "4" turned a typed 4 into 44, and an ESP box holding
    /// "512" would turn a typed 40 into 51240. The same mistake was found and
    /// fixed in the VM picker; the partition editor kept its own copy of the
    /// field and was never corrected.
    pub parts_size_pristine: bool,
    /// Unit the create-partition size box is currently in: MiB when true, GiB
    /// when false. Sizes are stored in MiB throughout, so this is purely how the
    /// number is read from the user — but without it a 512 MiB ESP was simply
    /// not expressible, the box only ever accepted whole GiB.
    /// Disk the create-partition wizard is carving from.
    pub parts_create_disk: String,
    /// The planned partition whose SIZE is being changed, or empty when the
    /// wizard is creating a new one. Resizing matters because nothing is written
    /// until the install runs: a partition planned at the wrong size should be
    /// corrected in place, not deleted and rebuilt. It also names WHICH data
    /// partition is meant — there can be any number of them.
    pub parts_resize_dev: String,
    /// Mountpoint picker for a partition used as plain data storage.
    pub parts_mount_open: bool,
    pub parts_mount_cursor: usize,
    pub parts_mount_input: String,
    /// The mount picker is finishing the CREATE wizard: role "data" was chosen
    /// and a size entered, and confirming a mountpoint creates the partition.
    /// Without this flag the picker acts on `parts_target`, an existing device.
    pub parts_mount_for_new: bool,
    /// Size (MiB) carried from the create wizard into the mount picker.
    pub parts_pending_mib: u32,
    pub parts_size_mib: bool,
    /// "Erase this disk NOW" confirmation. The method itself is chosen inline on
    /// the disk-header row (a ←/→ strip, default "nothing"); this modal is only
    /// the strong confirm that Enter raises before an immediate erase. The
    /// cursor starts on "cancel", so a "now" wipe is never one careless keypress.
    pub parts_wipe_open: bool,
    pub parts_wipe_disk: String,
    pub parts_wipe_confirm_cursor: usize,
    /// "This drive holds another operating system" acknowledgement. Selecting a
    /// wipe on such a drive is NOT refused — the user may be replacing that
    /// system on purpose — but it stops to name what will be destroyed and asks
    /// them to say they understand. `idx` is the strip position waiting to be
    /// applied once they do.
    pub parts_wipe_ack_open: bool,
    pub parts_wipe_ack_disk: String,
    pub parts_wipe_ack_idx: usize,
    pub parts_wipe_ack_cursor: usize,
    /// Drives whose foreign-OS warning has already been acknowledged this run,
    /// so switching between erase methods afterwards does not re-ask. A modal on
    /// every keypress teaches people to dismiss it unread.
    pub parts_wipe_acked: Vec<String>,
    /// A "now" wipe running from the editor: its target/method (for the overlay
    /// title), the streamed log, the live channel, and the outcome once it ends.
    /// Same thread+channel shape as the installer — the UI never blocks. When
    /// `wipe_run_rx` is `Some`, the parts screen shows the progress overlay and
    /// its tick drains the channel; on completion the scan is invalidated so the
    /// now-empty disk reappears clean.
    pub wipe_run_disk: String,
    pub wipe_run_method: WipeMethod,
    pub wipe_run_log: Vec<String>,
    pub wipe_run_rx: Option<crossbeam_channel::Receiver<crate::system::runner::LogLine>>,
    pub wipe_run_done: Option<Result<(), String>>,
    /// Undo history for the manual partition editor: the config as it was
    /// BEFORE each change, newest last.
    ///
    /// Esc pops one entry instead of leaving the screen, and only walks back a
    /// page once there is nothing left to undo. That is what lets a deleted
    /// partition vanish outright rather than lingering as a marked row — the
    /// way back is the undo stack, not a leftover line in the layout.
    pub parts_undo: Vec<InstallConfig>,
    /// Cached partition scan for the manual screen ('r' refreshes).
    pub parts_list: Vec<crate::system::disk::Partition>,
    pub parts_scanned: bool,
    /// PartitionMode screen: why a selection was refused (e.g. BIOS firmware).
    pub pmode_status: String,
    /// Receiver for the in-flight `nmcli connect` attempt. Some(..) means a
    /// connection is being tried right now — the UI stays responsive and
    /// `wifi::tick` polls this each frame. Ok(()) = connected, Err(msg) =
    /// failed (msg may be empty when nmcli "succeeded" but the device never
    /// reached the connected state).
    pub wifi_connect_rx: Option<crossbeam_channel::Receiver<Result<(), String>>>,
    /// Background network scan in flight: `tick()` polls this. The scan MUST
    /// run off the UI thread — `nmcli --rescan yes` blocks for 5–15 s, and an
    /// event loop stuck that long reads as a crash (the animation freezes and
    /// keys go dead).
    pub wifi_scan_rx: Option<crossbeam_channel::Receiver<Vec<String>>>,
    /// When the current attempt started, so we can give up instead of hanging
    /// forever — nmcli can block indefinitely on a flaky or simulated radio.
    pub wifi_connect_started: Option<std::time::Instant>,
    /// Set once the post-connect background install of live-environment
    /// prerequisites (git + install tools) has been kicked off, so it runs at
    /// most once even if the user revisits the network step.
    pub prereq_started: bool,
    /// Cursor for the end-of-install menu on the Finish screen (reboot /
    /// poweroff / enter the installed system).
    pub finish_cursor: usize,

    // Packages
    pub pkg_query: String,
    /// Text buffer for the AUR package input field (space-separated names).
    pub aur_query: String,
    pub pkg_focus: usize, // 0 = gpu, 1 = search/list, 2 = AUR input
    /// Live search results (name+desc) for the current query.
    pub pkg_results: Vec<crate::system::packages::Pkg>,
    /// Curated popular packages, shown when the search box is empty.
    pub pkg_popular: Vec<crate::system::packages::Pkg>,
    /// Receiver for the in-flight background search, if any.
    pub pkg_rx:
        Option<crossbeam_channel::Receiver<Result<Vec<crate::system::packages::Pkg>, String>>>,
    /// The query string the in-flight search was launched for (debounce guard).
    pub pkg_inflight_query: String,
    /// Ticks remaining before we launch a search (simple debounce).
    pub pkg_debounce: u8,
    /// Last search error message, if any (e.g. no network).
    pub pkg_error: Option<String>,
    pub pkg_searching: bool,
    // AUR search state (mirrors the repo search above, but for the AUR section).
    pub aur_results: Vec<crate::system::packages::Pkg>,
    pub aur_popular: Vec<crate::system::packages::Pkg>,
    pub aur_rx:
        Option<crossbeam_channel::Receiver<Result<Vec<crate::system::packages::Pkg>, String>>>,
    pub aur_inflight_query: String,
    pub aur_debounce: u8,
    pub aur_error: Option<String>,
    pub aur_searching: bool,
    /// Cursor within the AUR results/popular list.
    pub aur_cursor: usize,
    pub gpu_cursor: usize,
    pub kernel_cursor: usize,
    /// Packages screen: the "slayfetch" logo-picker modal (open flag + cursor
    /// into the list of embedded Artix+pride logo variants).
    pub logo_modal_open: bool,
    pub logo_cursor: usize,

    // Disk
    pub disk_cursor: usize,
    pub disk_focus: usize, // 0 boot, 1 disk, 2 swap, 3 filesystem, 4 fs-options
    pub fs_opt_cursor: usize, // cursor within the per-filesystem options list
    pub fs_opt_desc_scroll: u16, // vertical scroll offset of the option's description
    pub fs_opts_modal_open: bool, // the filesystem-options modal (with descriptions)
    /// WHICH filesystem the options modal is editing. The manual editor shows an
    /// "[o] Options" cue on the root row AND on a dedicated /home row; without
    /// this the modal always edited root, so opening it from the /home row
    /// silently rewrote the root's layout.
    pub fs_opts_target: FsOptsTarget,
    /// The data partition `FsOptsTarget::ManualData` refers to. Unlike root and
    /// /home there can be many of them, so the row's device has to be carried.
    pub fs_opts_data_dev: String,
    pub storage_opts_modal_open: bool, // per-disk options modal on the storage screen
    pub storage_opt_cursor: usize,
    pub storage_cursor: usize, // selected row on the Additional-disks screen
    /// Removable devices detected for the USB-keyfile row (refreshed on each
    /// cycle press so a just-plugged stick appears).
    pub usb_devices: Vec<crate::system::disk::Disk>,
    /// When the current provider prompt opened (for the 5-minute auto-default).
    pub prompt_opened_at: Option<std::time::Instant>,
    /// When the current install attempt began. Stamps the log FILE with how far
    /// into the run each line landed, which is what turns a wall of pacman
    /// output into something you can find the slow or stalled step in.
    pub install_started: Option<std::time::Instant>,

    // Install
    pub install_phase: crate::screens::summary::Phase,
    pub install_plan: Vec<crate::system::disk::Action>,
    pub install_step: usize,
    pub install_rx: Option<crossbeam_channel::Receiver<crate::system::runner::LogLine>>,
    /// Auto-retry state for a step that failed on a MIRROR/network error. The
    /// current step is re-run (not the whole install — that would re-wipe the
    /// disk) up to a cap, so a 404 storm mid-download does not need the user to
    /// sit and press Enter. Reset to 0 whenever a step succeeds, so each step
    /// gets its own budget and a genuinely dead mirror set still halts.
    pub install_auto_retries: u32,
    /// When the log started for the CURRENT step, so the failure classifier
    /// looks only at THIS step's output, not an earlier step's 404 that was
    /// already retried past.
    pub install_step_log_start: usize,
    /// Backoff deadline: when set, `tick` re-spawns the failed step once this
    /// passes. Keeps the phase at Installing so the timer keeps ticking.
    pub install_retry_at: Option<std::time::Instant>,
    /// When set, the main loop suspends the TUI (leaves raw mode / alt screen),
    /// runs this (program, args) on the real terminal so the user can answer
    /// prompts, then restores the TUI and advances the install step. Used for
    /// interactive-mode basestrap.
    pub pending_interactive: Option<(String, Vec<String>)>,
    /// Which action the RECOVERY screen offers once the system is mounted:
    /// 0 = open a root chroot shell, 1 = repair ownership and permissions.
    /// The repair is a second action rather than something typed into the shell
    /// because the way back from `chmod 777 /` should not require knowing the
    /// incantation — that is exactly the state in which the user has no working
    /// sudo to look it up with.
    pub recovery_action: usize,
    /// How much bigger (or smaller) the OPEN DIALOG is drawn, in steps of a few
    /// cells. Each `+` adds one while a dialog is up, each `-` takes one away.
    ///
    /// THIS IS THE ONLY THING THOSE KEYS TOUCH WHILE A DIALOG IS OPEN. A Linux
    /// VT has one font for the whole screen, so the letters inside a box cannot
    /// be enlarged on their own — but the box can, and that is what was asked
    /// for: the dialog resizing SEPARATELY, instead of the entire installer
    /// around it growing and shrinking with it.
    ///
    /// Kept as a signed count of steps rather than absolute cells, so every
    /// dialog keeps its own proportions and simply gets roomier; the clamping
    /// to the panel lives in `widgets::modal_rect_fit`. It persists after the
    /// dialog closes — it is a preference the user set, not a property of one
    /// box.
    pub modal_zoom: i16,
    /// Font chooser: which family, which of its sizes, and which of the two
    /// questions has focus.
    ///
    /// The size is ALSO what `+` and `-` move when no dialog is open — there is
    /// no second notion of console font size any more. There used to be one, a
    /// step count into a fixed ladder of another typeface entirely, and it meant
    /// pressing `+` replaced the chosen font instead of enlarging it.
    /// Show the password fields in clear text while the user asks for it.
    ///
    /// Typing a password blind is a guess about the keyboard layout, and the
    /// layout is now something the user changes DURING the install — so a
    /// mistyped LUKS passphrase would only surface at the next boot, when the
    /// disk refuses to open and there is nothing left to fix it with.
    ///
    /// Off by default, and cleared on every screen change (`reset_screen_focus`)
    /// so it can never be left on for the next person to walk past the screen.
    /// Has the user actually chosen a keyboard layout, as opposed to being given
    /// one by the language they picked?
    ///
    /// The language screen sets a sensible layout for each language, and must be
    /// free to REPLACE that when the language changes — but never to overwrite a
    /// deliberate choice. It used to tell the two apart by comparing the layout
    /// list against every known default (`["gb"]`, `["gb","ua"]`, …). That list
    /// was always one short: pick Spanish, then Ukrainian, and the Spanish
    /// layout stayed, because `["es"]` was not on it. A flag cannot be one short.
    pub keyboard_touched: bool,
    /// Whether the timezone list was used during THIS visit to the screen.
    ///
    /// The list opens at the top rather than on the configured zone, so without
    /// this a person who stepped through the screen without touching it would
    /// have their earlier choice quietly replaced by whatever sorts first.
    pub tz_touched: bool,
    /// Horizontal scroll offset, in CHARACTERS, for a label too wide to fit.
    ///
    /// A list row that does not fit is clipped by ratatui without a word, so the
    /// end of a long package description or timezone name simply does not exist
    /// as far as the reader is concerned. Only the SELECTED row scrolls: a whole
    /// list in motion is unreadable, and the row under the cursor is the one
    /// being read.
    pub marquee: usize,
    /// Ticks since the last step. The tick is ~10 Hz (the input poll), and a
    /// step every fifth one gives about two characters per half second — slow
    /// enough to read, which is the whole point.
    pub marquee_tick: u16,
    pub show_secrets: bool,
    pub font_family: usize,
    pub font_size_idx: usize,
    pub font_focus: usize,
    /// Writer to the interactive (PTY) child, so the user's typed answer can be
    /// sent back to pacman. Present only during an interactive step.
    pub pty_writer: Option<crate::system::runner::PtyWriter>,
    /// When pacman (under PTY) is waiting for a provider number, this holds the
    /// prompt text and the input the user is typing.
    pub prompt_active: bool,
    pub prompt_text: String,
    pub prompt_input: String,

    // ── Mode chooser + recovery tool ─────────────────────────────────────────
    /// Which top-level action the user picked after the language screen:
    /// 0 = install (the normal flow), 1 = recovery. Drives the Mode screen
    /// cursor and where Enter/Esc go from there.
    pub mode_cursor: usize,
    /// Recovery screen: the field/row the cursor is on (target disk, unlock
    /// method, passphrase entry, mount+chroot action).
    pub recovery_focus: usize,
    /// Recovery: index of the selected block device (into the scanned list).
    pub recovery_disk_cursor: usize,
    /// What each partition IS, parallel to the scanned partition list. Filled
    /// once from `recovery::suggest_all` and the user's from then on — see the
    /// role constants in `system/recovery.rs` for why recovery stopped trusting
    /// the target's own /etc/fstab to answer this.
    pub recovery_roles: Vec<usize>,
    /// Whether the roles above were READ from the install's own record on the
    /// ESP, or guessed from partition sizes. The two look identical on screen
    /// and only one deserves to be trusted without checking.
    pub recovery_has_record: bool,
    /// The bootloader that record names, if any.
    pub recovery_record_loader: String,
    /// Where each partition given the "data" role gets mounted. Parallel to the
    /// list, like the roles. Empty means "not chosen yet"; the mount step falls
    /// back to the first preset. A free-text path lives here too — the presets
    /// cover the common cases, but somebody who mounted a disk at
    /// `/mnt/фотки-2019` needs to be able to say so, and a ring of four fixed
    /// choices cannot express that.
    pub recovery_data_path: Vec<String>,
    /// The "where should this be mounted" picker: open, which row, and the
    /// free-text field under the presets.
    pub recovery_path_open: bool,
    pub recovery_path_cursor: usize,
    pub recovery_path_input: String,
    /// Recovery: unlock method cursor — 0 = none (unencrypted), 1 = passphrase,
    /// 2 = USB key file.
    pub recovery_unlock: usize,
    /// Recovery: the passphrase the user types to unlock a LUKS root.
    pub recovery_passphrase: String,
    /// Recovery: status/log text shown after attempting mount (what was
    /// mounted, which bootloader was detected, or the error).
    pub recovery_status: String,
    /// Recovery: set once partitions are mounted, so Enter launches the chroot
    /// shell instead of re-running the mount.
    pub recovery_mounted: bool,
}

impl App {
    /// Whether ANY modal window is currently open.
    ///
    /// One place, listing every flag, because the alternative is what the code
    /// had: each screen knowing about its own dialogs and nothing knowing about
    /// all of them. The size keys need exactly this answer — grow the dialog the
    /// user is looking at, never the installer underneath it — and a flag left
    /// out here would silently resize the wrong thing.
    pub fn modal_open(&self) -> bool {
        self.confirm_format_open
            || self.disk_warn_modal_open
            || self.fs_opts_modal_open
            || self.logo_modal_open
            || self.parts_create_open
            || self.parts_disk_modal_open
            || self.parts_modal_open
            || self.parts_mount_open
            || self.recovery_path_open
            || self.parts_wipe_ack_open
            || self.parts_wipe_open
            || self.seat_modal_open
            || self.storage_opts_modal_open
    }

    pub fn new() -> Self {
        // Auto-detect the firmware mode the live system actually booted in:
        // /sys/firmware/efi exists only under UEFI. This makes the Disk step
        // default to the correct mode (so grub installs as x86_64-efi under
        // UEFI, i386-pc under BIOS) instead of relying on the user to toggle it.
        let config = InstallConfig {
            boot_mode: if std::path::Path::new("/sys/firmware/efi").exists() {
                BootMode::Uefi
            } else {
                BootMode::Bios
            },
            ..Default::default()
        };
        App {
            screen: Screen::Language,
            lang: Lang::Uk,
            config,
            should_quit: false,
            pending_reboot: false,
            pending_firmware: false,
            cursor: 0,
            seat_modal_open: false,
            seat_modal_cursor: 0,
            seat_chosen: false,
            disk_warn_modal_open: false,
            disk_warn_scroll: 0,
            confirm_format_open: false,
            de_focus: 0,
            can_advance: true,
            frame: 0,
            log: Vec::new(),
            log_scroll: 0,
            log_follow: true,
            summary_scroll: 0,
            nav_cursor: [0; Screen::COUNT],
            log_last_down: None,
            log_live: false,
            tz_query: String::new(),
            kb_query: String::new(),
            user_confirm: String::new(),
            root_confirm: String::new(),
            user_focus: 0,
            wifi_stage: crate::screens::wifi::Stage::Choose,
            wifi_adapters: Vec::new(),
            wifi_adapter: String::new(),
            wifi_networks: Vec::new(),
            wifi_ssid: String::new(),
            wifi_password: String::new(),
            wifi_status: String::new(),
            wifi_status_is_error: false,
            wifi_status_success: false,
            wifitest_log: Vec::new(),
            wifitest_running: false,
            wifitest_rx: None,
            tbwtest_rows: Vec::new(),
            tbwtest_running: false,
            tbwtest_scanned: false,
            tbwtest_rx: None,
            parts_modal_open: false,
            parts_modal_cursor: 0,
            parts_target: String::new(),
            parts_disk_cursor: 0,
            parts_disk_modal_open: false,
            parts_create_open: false,
            parts_create_stage: 0,
            parts_create_role: 0,
            parts_size_input: String::new(),
            parts_size_pristine: false,
            parts_create_disk: String::new(),
            parts_resize_dev: String::new(),
            parts_mount_open: false,
            parts_mount_cursor: 0,
            parts_mount_input: String::new(),
            parts_mount_for_new: false,
            parts_pending_mib: 0,
            parts_size_mib: false,
            parts_wipe_open: false,
            parts_wipe_disk: String::new(),
            parts_wipe_confirm_cursor: 0,
            parts_wipe_ack_open: false,
            parts_wipe_ack_disk: String::new(),
            parts_wipe_ack_idx: 0,
            parts_wipe_ack_cursor: 0,
            parts_wipe_acked: Vec::new(),
            wipe_run_disk: String::new(),
            wipe_run_method: WipeMethod::Quick,
            wipe_run_log: Vec::new(),
            wipe_run_rx: None,
            wipe_run_done: None,
            parts_undo: Vec::new(),
            parts_list: Vec::new(),
            parts_scanned: false,
            pmode_status: String::new(),
            wifi_connect_rx: None,
            wifi_scan_rx: None,
            wifi_connect_started: None,
            prereq_started: false,
            finish_cursor: 0,
            pkg_query: String::new(),
            aur_query: String::new(),
            pkg_focus: 0,
            pkg_results: Vec::new(),
            pkg_popular: crate::system::packages::popular(),
            pkg_rx: None,
            pkg_inflight_query: String::new(),
            pkg_debounce: 0,
            pkg_error: None,
            pkg_searching: false,
            aur_results: Vec::new(),
            aur_popular: crate::system::packages::aur_popular(),
            aur_rx: None,
            aur_inflight_query: String::new(),
            aur_debounce: 0,
            aur_error: None,
            aur_searching: false,
            aur_cursor: 0,
            gpu_cursor: 0,
            kernel_cursor: 0,
            logo_modal_open: false,
            logo_cursor: 0,
            disk_cursor: 0,
            disk_focus: 0,
            fs_opt_cursor: 0,
            fs_opts_target: FsOptsTarget::Root,
            fs_opts_data_dev: String::new(),
            fs_opt_desc_scroll: 0,
            fs_opts_modal_open: false,
            storage_opts_modal_open: false,
            storage_opt_cursor: 0,
            storage_cursor: 0,
            usb_devices: Vec::new(),
            prompt_opened_at: None,
            install_started: None,
            install_phase: crate::screens::summary::Phase::Review,
            install_plan: Vec::new(),
            install_step: 0,
            install_rx: None,
            install_auto_retries: 0,
            install_step_log_start: 0,
            install_retry_at: None,
            pending_interactive: None,
            recovery_action: 0,
            modal_zoom: 0,
            keyboard_touched: false,
            tz_touched: false,
            marquee: 0,
            marquee_tick: 0,
            show_secrets: false,
            font_family: crate::screens::fontpick::default_pos().0,
            font_size_idx: crate::screens::fontpick::default_pos().1,
            font_focus: 0,
            pty_writer: None,
            prompt_active: false,
            prompt_text: String::new(),
            prompt_input: String::new(),
            mode_cursor: 0,
            recovery_focus: 0,
            recovery_disk_cursor: 0,
            recovery_roles: Vec::new(),
            recovery_has_record: false,
            recovery_record_loader: String::new(),
            recovery_data_path: Vec::new(),
            recovery_path_open: false,
            recovery_path_cursor: 0,
            recovery_path_input: String::new(),
            recovery_unlock: 0,
            recovery_passphrase: String::new(),
            recovery_status: String::new(),
            recovery_mounted: false,
        }
    }

    pub fn goto_next(&mut self) {
        if self.can_advance && self.screen != Screen::Finish {
            let mut to = self.screen.next();
            while !self.step_has_content(to) && to != Screen::Finish {
                to = to.next();
            }
            self.switch_screen(to);
        }
    }

    /// Move to `to`, filing the row we're standing on under the screen we're
    /// leaving and picking up the row `to` was left on.
    ///
    /// Every step therefore keeps its own place: the shared `cursor` can't leak
    /// between lists (each screen reads its own slot), and walking back doesn't
    /// dump you at the top of a step you had already worked through.
    fn switch_screen(&mut self, to: Screen) {
        self.nav_cursor[self.screen as usize] = self.cursor;
        self.screen = to;
        self.cursor = self.nav_cursor[to as usize];
        self.reset_screen_focus();
    }

    /// Go to a screen that is not the next or previous step — the mode menu, the
    /// font chooser, recovery, and back again.
    ///
    /// It exists because assigning `app.screen` directly LOOKS like navigation
    /// and is not: it skips the cursor bookkeeping in `switch_screen`, so the
    /// arriving screen inherits whatever row the departing one was on and the
    /// leaving screen forgets where it was.
    ///
    /// That is not theoretical. Language and the mode menu both jumped by
    /// assignment, so choosing a language a second time walked
    /// Timezone → Language → Mode → Timezone carrying the LANGUAGE list's cursor
    /// into a 400-entry timezone list. `reset_screen_focus` never ran, so the
    /// cursor was never parked back on the chosen zone: the timezone the user
    /// picked was still in the config, but the highlight sat somewhere else and
    /// the next arrow key committed a different zone over it. It read as "it
    /// forgot my timezone", because in every way that shows on screen, it had.
    pub fn goto(&mut self, to: Screen) {
        self.switch_screen(to);
    }

    /// Does this step have anything to ask? A step with nothing to decide is
    /// walked past in both directions — an empty screen reads as broken rather
    /// than absent.
    pub(crate) fn step_has_content(&self, s: Screen) -> bool {
        match s {
            // Extra disks. In MANUAL partitioning this step has nothing left to
            // offer: the partition editor already mounts existing partitions and
            // creates new ones on any drive, through the "Data" role, and it
            // writes them to the very same `extra_disks` list this screen edits.
            // Showing both meant two screens editing one list — a second chance
            // to give the same partition a different mountpoint, and a step whose
            // only honest answer was "I already did this".
            //
            // In AUTO mode it still earns its place: there the disk step claims
            // one drive and says nothing about the others.
            // ONLY the mode that actually HAS the editor may skip this step.
            //
            // It was skipped for the whole "manual family", which includes
            // Alongside — and Alongside never shows the partition editor (see
            // screens/disk.rs, which forwards only for Manual). So that mode
            // lost both screens at once and a second drive could not be mounted
            // anywhere at all. The reasoning was sound for manual partitioning
            // and was applied to a mode it does not describe.
            Screen::Storage => {
                self.config.partition_mode != PartitionMode::Manual
                    && crate::screens::storage::has_candidates(self)
            }
            _ => true,
        }
    }

    /// The steps this run will actually show, in order.
    ///
    /// Not every screen in `ALL` is a step for everyone: extra disks is skipped
    /// in manual partitioning, and in auto mode when there is nothing left over.
    /// The rail and the header both count from THIS, so "9 / 15" means the ninth
    /// of the fifteen you will really see — and moves to "9 / 16" for someone who
    /// does get the extra-disks step.
    ///
    /// Both used to be written out by hand: a rail of literal "01".."15" and a
    /// header of `/ 15`, while `ALL` had sixteen entries. So the count was wrong
    /// for everybody, and wrong in a way no test could notice — three separate
    /// places each holding their own idea of how long the wizard is.
    pub fn visible_steps(&self) -> Vec<Screen> {
        Screen::ALL
            .iter()
            .copied()
            .filter(|s| self.step_has_content(*s))
            .collect()
    }

    /// Where the current screen sits among the visible steps: (1-based, total).
    /// Position 0 means "not a numbered step" — the mode menu and its siblings.
    pub fn step_position(&self) -> (usize, usize) {
        let steps = self.visible_steps();
        let pos = steps
            .iter()
            .position(|s| *s == self.screen)
            .map_or(0, |i| i + 1);
        (pos, steps.len())
    }

    /// Advance the marquee, and reset it when the cursor moves.
    ///
    /// Steps by TWO characters rather than one. Asked for, and right: with
    /// Arabic, Japanese or Chinese a single character is often a whole word, so
    /// one-at-a-time crawls while two keeps context arriving at a readable rate.
    pub fn tick_marquee(&mut self) {
        const TICKS_PER_STEP: u16 = 5;
        const STEP: usize = 2;
        self.marquee_tick = self.marquee_tick.wrapping_add(1);
        if self.marquee_tick.is_multiple_of(TICKS_PER_STEP) {
            self.marquee += STEP;
        }
    }

    /// Put a label back to its start. Called whenever the cursor moves, so a row
    /// is always read from the beginning rather than joined halfway.
    pub fn reset_marquee(&mut self) {
        self.marquee = 0;
        self.marquee_tick = 0;
    }

    pub fn goto_prev(&mut self) {
        // Never allow stepping back out of Finish into a re-install.
        if self.screen != Screen::Finish {
            let mut to = self.screen.prev();
            while !self.step_has_content(to) && to != Screen::ALL[0] {
                to = to.prev();
            }
            self.switch_screen(to);
        }
    }

    /// Close anything transient before a screen is handed over.
    ///
    /// The per-screen focus fields (the desktop step's DE-vs-login-manager row,
    /// the disk step's boot/disk/swap/fs row, and so on) are deliberately NOT
    /// cleared here. Each lives in its own field, so nothing leaks between
    /// screens, and keeping them is what lets a step hand itself back the way
    /// you left it — see `switch_screen`.
    ///
    /// This used to zero them, on the grounds that returning mid-way through the
    /// desktop step left Enter on the login-screen row jumping straight forward
    /// again, stranding the desktop and seat pickers. Esc now steps focus back
    /// up a row on those screens (desktop, disk, user, options), so the earlier
    /// rows are reachable again and the reset only cost the user their place.
    ///
    /// What must still be cleared: MODALS. A modal left flagged open would
    /// render over the screen the moment it is drawn.
    fn reset_screen_focus(&mut self) {
        // A revealed password must never survive the screen it belongs to.
        self.show_secrets = false;
        self.fs_opts_target = FsOptsTarget::Root;
        self.fs_opts_data_dev.clear();
        self.seat_modal_open = false;
        self.storage_opts_modal_open = false;
        self.recovery_path_open = false;
        self.recovery_path_input.clear();
        self.parts_modal_open = false;
        self.parts_modal_cursor = 0;
        self.parts_target.clear();
        self.logo_modal_open = false;
        self.parts_disk_modal_open = false;
        self.parts_create_open = false;
        self.parts_create_stage = 0;
        self.parts_size_input.clear();
        self.parts_size_mib = false;
        self.parts_create_disk.clear();
        self.parts_resize_dev.clear();
        self.parts_mount_open = false;
        self.parts_mount_cursor = 0;
        self.parts_mount_input.clear();
        self.parts_mount_for_new = false;
        self.parts_pending_mib = 0;
        self.parts_wipe_open = false;
        self.parts_wipe_disk.clear();
        self.parts_wipe_confirm_cursor = 0;
        self.parts_wipe_ack_open = false;
        self.parts_wipe_ack_disk.clear();
        self.parts_wipe_ack_idx = 0;
        self.parts_wipe_ack_cursor = 0;
        // `parts_wipe_acked` is NOT cleared: it records that the user was shown
        // what a drive holds and said they understood. Stepping away to change a
        // package and coming back does not un-understand it.
        // A "now" wipe in flight is NOT cancelled here: reset_screen_focus fires
        // on step changes, and abandoning a running erase mid-write would be far
        // worse than letting it finish. Its tick clears the channel on its own.
        self.parts_undo.clear();
        self.pmode_status.clear();

        // The timezone list opens AT THE TOP, with an empty filter, every time.
        //
        // It used to park the cursor on the zone the config already held. That
        // sounds helpful and was not: the config changes as you move through the
        // list, so every re-entry landed somewhere else, and switching language
        // between visits made it look arbitrary — "щоразу в іншому місці".
        // A list of 567 entries is not browsed anyway; it is searched.
        //
        // The choice already made is NOT lost by this. `tz_touched` says whether
        // the list was actually used this visit, and Enter only re-commits when
        // it was — so walking through the step keeps whatever was chosen before,
        // and the screen prints it above the list so it is never invisible.
        if self.screen == Screen::Timezone {
            self.tz_query.clear();
            self.cursor = 0;
            self.tz_touched = false;
        }
    }

    pub fn push_log<S: Into<String>>(&mut self, line: S) {
        let line = line.into();
        // A real log line ends any in-progress live download indicator, so the
        // next progress update starts a fresh line rather than overwriting this.
        self.log_live = false;
        // Mirror every log line to a file on the live system. A late install
        // step copies it to /var/log/installer.log on the target, so the
        // finished system carries a full record of its own installation for
        // post-mortem diagnosis. Best-effort: a failed write never disrupts the
        // install or the on-screen log.
        //
        // The FILE gets an elapsed-time stamp; the SCREEN does not. The panel
        // is 59 columns on an 80x24 console, where a stamp would cost a tenth
        // of every line for something nobody reads while watching it scroll —
        // but afterwards, in the file, "which step ate twenty minutes" is
        // exactly the question being asked. Elapsed rather than wall-clock:
        // it needs no timezone and answers that question directly.
        {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/installer.log")
            {
                match self.install_started {
                    Some(t0) => {
                        let s = t0.elapsed().as_secs();
                        let _ = writeln!(
                            f,
                            "[+{:02}:{:02}:{:02}] {}",
                            s / 3600,
                            (s % 3600) / 60,
                            s % 60,
                            line
                        );
                    }
                    None => {
                        let _ = writeln!(f, "{}", line);
                    }
                }
            }
        }
        self.log.push(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `nav_cursor` is indexed by `Screen as usize`, so a variant living past its
    /// end panics the moment you navigate away from that screen. That is not a
    /// cosmetic failure: the panic kills the process, the terminal is left as-is
    /// and the user lands back at the console login, which looks exactly like
    /// "the installer threw me out". It shipped that way for the font screen.
    ///
    /// The match below is exhaustive on purpose. Adding a `Screen` stops this
    /// file compiling until the new variant is written in, and writing it in is
    /// what makes you confirm `COUNT` still covers it.
    /// A MODE WITHOUT THE EDITOR MUST KEEP THE ADDITIONAL-DISKS STEP.
    ///
    /// The step was skipped for the whole "manual family", which includes
    /// Alongside — and Alongside never shows the partition editor, so that
    /// mode lost both screens at once and a second drive could not be mounted
    /// anywhere. The justification ("the editor already does this") was true
    /// of manual partitioning only.
    #[test]
    fn only_the_mode_that_has_the_editor_skips_the_extra_disks_step() {
        let mut a = App::new();
        a.config.extra_disks = vec![ExtraDisk {
            disk: "/dev/vdb1".into(),
            mountpoint: "/mnt/data".into(),
            fs: "ext4".into(),
            format: true,
            ..Default::default()
        }];
        a.config.partition_mode = PartitionMode::Manual;
        assert!(
            !a.step_has_content(Screen::Storage),
            "manual partitioning has the editor, so the step is redundant there"
        );
        a.config.partition_mode = PartitionMode::Alongside;
        assert!(
            a.step_has_content(Screen::Storage) || !crate::screens::storage::has_candidates(&a),
            "Alongside has no editor — taking the step away leaves nowhere to mount a disk"
        );
    }

    #[test]
    fn every_screen_has_a_nav_slot() {
        let every = [
            Screen::Language,
            Screen::Timezone,
            Screen::Wifi,
            Screen::Keyboard,
            Screen::Kernel,
            Screen::Desktop,
            Screen::Packages,
            Screen::Aur,
            Screen::PartitionMode,
            Screen::Disk,
            Screen::Security,
            Screen::Storage,
            Screen::User,
            Screen::Options,
            Screen::Summary,
            Screen::Finish,
            Screen::Mode,
            Screen::Recovery,
            Screen::WifiTest,
            Screen::TbwTest,
            Screen::FontPick,
        ];
        for s in every {
            match s {
                Screen::Language
                | Screen::Timezone
                | Screen::Wifi
                | Screen::Keyboard
                | Screen::Kernel
                | Screen::Desktop
                | Screen::Packages
                | Screen::Aur
                | Screen::PartitionMode
                | Screen::Disk
                | Screen::Security
                | Screen::Storage
                | Screen::User
                | Screen::Options
                | Screen::Summary
                | Screen::Finish
                | Screen::Mode
                | Screen::Recovery
                | Screen::WifiTest
                | Screen::TbwTest
                | Screen::FontPick => {}
            }
            assert!(
                (s as usize) < Screen::COUNT,
                "{s:?} = {} is past nav_cursor (COUNT = {})",
                s as usize,
                Screen::COUNT
            );
        }
        // The indexing itself, from and to every screen: this is the operation
        // that panicked, so exercise it rather than only the arithmetic.
        for s in every {
            let mut app = App::new();
            app.screen = s;
            app.switch_screen(Screen::Mode);
            app.switch_screen(s);
        }
    }

    /// The step counter tells the truth, and it is computed in ONE place.
    ///
    /// It used to be three: `ALL` with sixteen entries, a rail of hand-written
    /// "01".."15", and a header that said `/ 15`. So every user was told the
    /// wizard was fifteen steps long while walking through sixteen, and the
    /// number under the cursor did not match the number in the header. No test
    /// could catch it, because none of the three knew about the others.
    #[test]
    fn the_step_counter_matches_the_steps_actually_shown() {
        // Auto mode with spare disks: every step in ALL is shown.
        let mut app = App::new();
        app.config.partition_mode = PartitionMode::Auto;
        let steps = app.visible_steps();
        assert_eq!(
            steps.len(),
            Screen::ALL.len(),
            "auto mode should show the whole wizard"
        );

        // Walking it: the position must climb by exactly one each time and end
        // on the last step, never past the total.
        for (i, s) in steps.iter().enumerate() {
            app.screen = *s;
            let (pos, total) = app.step_position();
            assert_eq!(pos, i + 1, "{s:?} reported the wrong position");
            assert_eq!(total, steps.len());
            assert!(pos <= total, "{s:?}: {pos} of {total} is off the end");
        }

        // Manual mode drops the extra-disks step, and the total drops with it.
        let mut app = App::new();
        app.config.partition_mode = PartitionMode::Manual;
        let manual = app.visible_steps();
        assert!(
            !manual.contains(&Screen::Storage),
            "manual mode still counts a step it skips"
        );
        assert_eq!(manual.len(), Screen::ALL.len() - 1);
        app.screen = Screen::Finish;
        let (pos, total) = app.step_position();
        assert_eq!(pos, total, "the last step must read as the last");

        // Off the wizard entirely: no number at all rather than a wrong one.
        app.screen = Screen::FontPick;
        assert_eq!(app.step_position().0, 0);
    }

    /// Walking the wizard with next()/prev() must reproduce `ALL`, forwards and
    /// backwards.
    ///
    /// They used to cast the variant to an index, which assumed the enum was
    /// DECLARED in the order the wizard RUNS in. Moving the keyboard step ahead
    /// of the timezone broke that silently: on the keyboard step Esc handed back
    /// the timezone — walking forward instead of back — and every test still
    /// passed, because they all went through `visible_steps`, which iterates
    /// `ALL` honestly.
    #[test]
    fn stepping_forward_and_back_follows_the_declared_order() {
        // Forward from the first step reproduces ALL.
        let mut walked = vec![Screen::ALL[0]];
        let mut s = Screen::ALL[0];
        for _ in 1..Screen::ALL.len() {
            s = s.next();
            walked.push(s);
        }
        assert_eq!(walked, Screen::ALL.to_vec(), "next() does not follow ALL");
        assert_eq!(s.next(), s, "next() ran off the end");

        // And backwards is exactly the reverse.
        let mut back = vec![s];
        for _ in 1..Screen::ALL.len() {
            s = s.prev();
            back.push(s);
        }
        back.reverse();
        assert_eq!(back, Screen::ALL.to_vec(), "prev() does not follow ALL");
        assert_eq!(s.prev(), s, "prev() ran off the start");

        // Every step's neighbours agree with each other.
        for w in Screen::ALL.windows(2) {
            assert_eq!(w[0].next(), w[1]);
            assert_eq!(w[1].prev(), w[0]);
        }

        // Screens outside ALL are their own neighbours: they navigate
        // explicitly, and indexing ALL from them is how the font picker once
        // walked off the end of an array.
        for off in [
            Screen::Mode,
            Screen::Recovery,
            Screen::FontPick,
            Screen::WifiTest,
        ] {
            assert_eq!(off.next(), off, "{off:?} indexed into ALL");
            assert_eq!(off.prev(), off, "{off:?} indexed into ALL");
        }
    }

    /// No screen may navigate by assigning `app.screen`.
    ///
    /// Assignment looks like navigation and is not: it skips `switch_screen`, so
    /// the arriving screen keeps the departing screen's cursor and the departing
    /// screen forgets its own. That is how choosing a language twice appeared to
    /// erase the timezone — see `App::goto`.
    ///
    /// Tests assign freely; they are placing a screen, not travelling to one.
    #[test]
    fn no_screen_navigates_by_assigning_the_field() {
        let mut checked = 0;
        let mut offenders = Vec::new();
        let entries = std::fs::read_dir("src/screens").expect("src/screens is readable");
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap_or_default();
            // Everything past `#[cfg(test)]` is test code and exempt.
            let live = match src.find("#[cfg(test)]") {
                Some(i) => &src[..i],
                None => &src[..],
            };
            for (n, line) in live.lines().enumerate() {
                if line.contains(".screen = ") && line.contains("Screen::") {
                    offenders.push(format!("{}:{}", path.display(), n + 1));
                }
            }
            checked += 1;
        }
        assert!(
            offenders.is_empty(),
            "navigate with app.goto(...), not by assigning app.screen — \
             assignment skips the cursor bookkeeping: {}",
            offenders.join(", ")
        );
        assert!(
            checked >= 10,
            "only {checked} screens scanned — did the path move?"
        );
    }
}
