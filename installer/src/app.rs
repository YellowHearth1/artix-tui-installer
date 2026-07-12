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
    Disk = 8,
    Security = 9,
    Storage = 10,
    User = 11,
    Options = 12,
    Summary = 13,
    Finish = 14,
    // Screens OUTSIDE the linear 15-step install flow (not in ALL, no step
    // number): the post-language mode chooser and the recovery tool. Their
    // navigation is handled explicitly, not via next()/prev().
    Mode = 15,
    Recovery = 16,
}

impl Screen {
    pub const ALL: [Screen; 15] = [
        Screen::Language,
        Screen::Timezone,
        Screen::Wifi,
        Screen::Keyboard,
        Screen::Kernel,
        Screen::Desktop,
        Screen::Packages,
        Screen::Aur,
        Screen::Disk,
        Screen::Security,
        Screen::Storage,
        Screen::User,
        Screen::Options,
        Screen::Summary,
        Screen::Finish,
    ];

    pub fn next(self) -> Screen {
        // Mode/Recovery are outside ALL; navigating them is done explicitly by
        // their screens, so next()/prev() just return self for them (never
        // index ALL out of range).
        if (self as usize) >= Screen::ALL.len() {
            return self;
        }
        let i = (self as usize + 1).min(Screen::ALL.len() - 1);
        Screen::ALL[i]
    }

    pub fn prev(self) -> Screen {
        if (self as usize) >= Screen::ALL.len() {
            return self;
        }
        let i = (self as usize).saturating_sub(1);
        Screen::ALL[i]
    }

    /// Step number shown in the header, 1-based.
    pub fn step_number(self) -> usize {
        self as usize + 1
    }
}

/// How the installed system's accounts are set up (user choice on the User
/// screen). Covers the four combinations the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kernel {
    Linux,
    Zen,
    Hardened,
    Lts,
}

impl Kernel {
    pub fn packages(self) -> &'static [&'static str] {
        match self {
            Kernel::Linux => &["linux", "linux-headers"],
            Kernel::Zen => &["linux-zen", "linux-zen-headers"],
            Kernel::Hardened => &["linux-hardened", "linux-hardened-headers"],
            Kernel::Lts => &["linux-lts", "linux-lts-headers"],
        }
    }
}

/// One of the fixed GPU driver bundles that head the package list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuDriver {
    None,
    Nvidia,
    Nvidia580xx,
    Nouveau,
    Amd,
    Intel,
}

impl GpuDriver {
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
            Desktop::Xfce4 => &["xfce4", "xfce4-goodies", "labwc", "xorg-xwayland"],
            // Cinnamon is shipped as a single package in galaxy (not a group);
            // a file manager (nemo) makes it usable out of the box. The cinnamon
            // package already ships the experimental Wayland session (Muffin in
            // Wayland mode); xorg-xwayland lets X11 apps run inside it.
            Desktop::Cinnamon => &[
                "cinnamon",
                "nemo",
                "metacity",
                "lightdm",
                "lightdm-gtk-greeter",
                "xorg-xwayland",
            ],
            // MATE: group + extra, per the guide.
            Desktop::Mate => &["mate", "mate-extra"],
            // LXQt group + a panel-friendly file manager.
            Desktop::Lxqt => &["lxqt", "pcmanfm-qt"],
            // LXDE group.
            Desktop::Lxde => &["lxde"],
            // Pinnacle is an AUR package (pinnacle-comp), not in the repos, so
            // it has NO repo packages here — selecting it instead injects
            // pinnacle-comp (plus its companion tools) into the AUR package
            // list, and unpacks the bundled config. See the desktop→AUR wiring
            // in system/install.rs. The repo side adds only Xwayland so X11
            // apps run under it; that's handled there too, keeping this empty.
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
            Desktop::Gnome => "GNOME 50 (⚠ may be unstable)",
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
    "pipewire-libcamera",
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InstallConfig {
    pub lang: String,     // "en" | "uk" — UI language
    pub locale: String,   // e.g. "uk_UA.UTF-8" — system locale
    pub timezone: String, // e.g. "Europe/Kyiv"
    pub keymap: String,   // console keymap, e.g. "ua"
    pub xkb_layouts: Vec<String>,
    pub kernel: String,        // serialized Kernel
    pub desktops: Vec<String>, // serialized Desktop names (multi-select; empty = headless)
    /// Chosen session type for desktops that support both: "wayland" or "x11".
    /// Ignored for single-session desktops. Default follows Desktop::default_session.
    pub session: String,
    /// Seat/session manager: "elogind" (universal — works with X11 and Wayland,
    /// integrates with PAM/polkit; the safe default) or "seatd" (minimal, seat
    /// management only). Default "elogind".
    pub seat_provider: String,
    pub gpu: String, // serialized GpuDriver
    pub extra_packages: Vec<String>,
    /// Additional whole disks assigned to mountpoints (e.g. /home on a separate
    /// HDD, or a storage disk at /data). Empty on single-disk systems. The
    /// system disk (`disk` above) is never included here.
    #[serde(default)]
    pub extra_disks: Vec<ExtraDisk>,
    /// AUR packages the user typed (space-separated), built via paru at the end
    /// of install as the new user. Empty = none.
    pub aur_packages: Vec<String>,
    pub disk: String,      // e.g. "/dev/sda"
    pub boot_mode: String, // "bios" | "uefi"
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
    /// Bootloader to install: "grub" (default), "refind", or "limine".
    /// Only GRUB can decrypt an encrypted /boot, so full-disk encryption forces
    /// GRUB; the others are offered for plaintext and root-only LUKS setups.
    pub bootloader: String,
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
    pub account_mode: String,
}

impl Default for InstallConfig {
    fn default() -> Self {
        InstallConfig {
            lang: "uk".into(),
            locale: "uk_UA.UTF-8".into(),
            timezone: "Europe/Kyiv".into(),
            keymap: "ua".into(),
            xkb_layouts: vec!["ua".into(), "gb".into()],
            kernel: "Linux".into(),
            desktops: Vec::new(),
            session: "wayland".into(),
            seat_provider: "elogind".into(),
            gpu: "None".into(),
            // Default-checked package set: pre-selected on a fresh run, all
            // ordinary entries the user can untick in the packages screen.
            // The distro's own configs ride on these: zsh → shipped .zshrc +
            // starship (and becomes the login shell), kitty → Catppuccin
            // kitty.conf, fastfetch → the syrnyk-logo config, nftables → the
            // embedded default-deny firewall; octopi is the friendly pacman
            // GUI. Untick zsh (and fish) to stay on bash.
            extra_packages: vec![
                "zsh".into(),
                "kitty".into(),
                "fastfetch".into(),
                "nftables".into(),
                "octopi".into(),
            ],
            extra_disks: Vec::new(),
            aur_packages: Vec::new(),
            disk: String::new(),
            boot_mode: "uefi".into(),
            passwordless_sudo: false,
            use_doas: false,
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
            bootloader: "grub".into(),
            display_manager: "sddm".into(),
            usb_key_device: String::new(),
            usb_key_only: false,
            bootloader_id: "Artix".into(),
            username: String::new(),
            hostname: "artix".into(),
            user_password: String::new(),
            root_password: String::new(),
            root_same_as_user: true,
            account_mode: "UserSameRoot".into(),
        }
    }
}

/// Top-level app state.
pub struct App {
    pub screen: Screen,
    pub lang: Lang,
    pub config: InstallConfig,
    pub should_quit: bool,
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

    // Disk
    pub disk_cursor: usize,
    pub disk_focus: usize, // 0 boot, 1 disk, 2 swap, 3 filesystem, 4 fs-options
    pub fs_opt_cursor: usize, // cursor within the per-filesystem options list
    pub fs_opt_desc_scroll: u16, // vertical scroll offset of the option's description
    pub fs_opts_modal_open: bool, // the filesystem-options modal (with descriptions)
    pub storage_opts_modal_open: bool, // per-disk options modal on the storage screen
    pub storage_opt_cursor: usize,
    pub storage_cursor: usize, // selected row on the Additional-disks screen
    /// Removable devices detected for the USB-keyfile row (refreshed on each
    /// cycle press so a just-plugged stick appears).
    pub usb_devices: Vec<crate::system::disk::Disk>,
    /// When the current provider prompt opened (for the 5-minute auto-default).
    pub prompt_opened_at: Option<std::time::Instant>,

    // Install
    pub install_phase: crate::screens::summary::Phase,
    pub install_plan: Vec<crate::system::disk::Action>,
    pub install_step: usize,
    pub install_rx: Option<crossbeam_channel::Receiver<crate::system::runner::LogLine>>,
    /// When set, the main loop suspends the TUI (leaves raw mode / alt screen),
    /// runs this (program, args) on the real terminal so the user can answer
    /// prompts, then restores the TUI and advances the install step. Used for
    /// interactive-mode basestrap.
    pub pending_interactive: Option<(String, Vec<String>)>,
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
    pub fn new() -> Self {
        let mut config = InstallConfig::default();
        // Auto-detect the firmware mode the live system actually booted in:
        // /sys/firmware/efi exists only under UEFI. This makes the Disk step
        // default to the correct mode (so grub installs as x86_64-efi under
        // UEFI, i386-pc under BIOS) instead of relying on the user to toggle it.
        config.boot_mode = if std::path::Path::new("/sys/firmware/efi").exists() {
            "uefi".into()
        } else {
            "bios".into()
        };
        App {
            screen: Screen::Language,
            lang: Lang::Uk,
            config,
            should_quit: false,
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
            disk_cursor: 0,
            disk_focus: 0,
            fs_opt_cursor: 0,
            fs_opt_desc_scroll: 0,
            fs_opts_modal_open: false,
            storage_opts_modal_open: false,
            storage_opt_cursor: 0,
            storage_cursor: 0,
            usb_devices: Vec::new(),
            prompt_opened_at: None,
            install_phase: crate::screens::summary::Phase::Review,
            install_plan: Vec::new(),
            install_step: 0,
            install_rx: None,
            pending_interactive: None,
            pty_writer: None,
            prompt_active: false,
            prompt_text: String::new(),
            prompt_input: String::new(),
            mode_cursor: 0,
            recovery_focus: 0,
            recovery_disk_cursor: 0,
            recovery_unlock: 0,
            recovery_passphrase: String::new(),
            recovery_status: String::new(),
            recovery_mounted: false,
        }
    }

    pub fn goto_next(&mut self) {
        if self.can_advance && self.screen != Screen::Finish {
            self.screen = self.screen.next();
            self.cursor = 0;
        }
    }

    pub fn goto_prev(&mut self) {
        // Never allow stepping back out of Finish into a re-install.
        if self.screen != Screen::Finish {
            self.screen = self.screen.prev();
            self.cursor = 0;
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
        {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/installer.log")
            {
                let _ = writeln!(f, "{}", line);
            }
        }
        self.log.push(line);
    }
}
