//! Install orchestration. Turns an InstallConfig into an ordered list of
//! Actions. The Summary screen runs them one-by-one through runner::spawn so
//! every step streams into the log and a failure halts the queue.
//!
//! NOTE: chroot steps are wrapped as `artix-chroot /mnt sh -c '...'`. We keep
//! each as a discrete Action for clear per-step logging and failure points.

use crate::app::{App, Desktop, GpuDriver, Kernel, InstallConfig, AUDIO_PACKAGES, DINIT_PACKAGES, DINIT_SERVICES};
use crate::system::disk::{self, Action};

fn act(program: &str, args: &[&str]) -> Action {
    Action { program: program.to_string(), args: args.iter().map(|s| s.to_string()).collect(), interactive: false }
}

/// Like `act`, but the step runs on the foreground terminal so the user can
/// answer interactive prompts (used for interactive-mode basestrap).
#[allow(dead_code)]
fn chroot(script: &str) -> Action {
    Action {
        program: "artix-chroot".to_string(),
        args: vec!["/mnt".into(), "sh".into(), "-c".into(), script.to_string()],
        interactive: false,
    }
}

/// Like `chroot`, but runs under a PTY so the user can answer interactive
/// prompts (e.g. paru's provider-number selection for AUR dependencies).
fn chroot_interactive(script: &str) -> Action {
    Action {
        program: "artix-chroot".to_string(),
        args: vec!["/mnt".into(), "sh".into(), "-c".into(), script.to_string()],
        interactive: true,
    }
}

/// Escape a string for safe inclusion inside a double-quoted shell context.
/// Backslash, double-quote, dollar and backtick are the characters that retain
/// special meaning inside double quotes.
fn shell_escape_dq(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Write a text file into the installed system using a quoted heredoc, so the
/// content is written verbatim (no shell expansion of $, backticks, etc.). The
/// path is relative to `home`. We run it through artix-chroot.
fn write_home_file(home: &str, rel_path: &str, content: &str) -> Action {
    // 'EOF' is single-quoted so the body isn't expanded by the shell. We pick a
    // marker unlikely to appear in configs.
    let script = format!(
        "cat > {home}/{rel_path} <<'ARTIX_INSTALLER_EOF'\n{content}\nARTIX_INSTALLER_EOF"
    );
    chroot(&script)
}

/// Standard base64 encoder (RFC 4648). Small enough to inline so we don't pull
/// in a crate just to ship one embedded image.
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Write a BINARY file into the user's home, returning the SEQUENCE of steps
/// that do it. The bytes are base64-encoded, then written in CHUNKS via
/// repeated `printf >> file.b64`, and finally decoded with `base64 -d`. This
/// avoids the kernel's MAX_ARG_STRLEN limit (~128 KB per single argument): a
/// large asset (e.g. the ~136 KB base64 of the fastfetch PNG) can't be passed
/// as one `sh -c` argument, so we split it. Each chunk is well under the limit.
/// `rel_path` is relative to `home`. base64 -d ignores newlines between chunks.
fn write_home_binary(home: &str, rel_path: &str, bytes: &[u8]) -> Vec<Action> {
    let b64 = base64_encode(bytes);
    let tmp = format!("{home}/{rel_path}.b64");
    let mut steps = Vec::new();
    // Start the temp file empty (truncate any leftover from a retried run).
    steps.push(chroot(&format!(": > {tmp}")));
    // Append in 60 KB slices — comfortably below MAX_ARG_STRLEN even with the
    // surrounding `printf '%s' '…'` wrapper. base64 has no quotes/backslashes,
    // so single-quoting each slice is safe and needs no escaping.
    const CHUNK: usize = 60 * 1024;
    let b = b64.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let end = (i + CHUNK).min(b.len());
        // base64 is ASCII, so slicing on byte boundaries never splits a char.
        let slice = &b64[i..end];
        steps.push(chroot(&format!("printf '%s' '{slice}' >> {tmp}")));
        i = end;
    }
    // Decode to the real file and drop the temp. If there were zero chunks
    // (empty input — shouldn't happen for our assets) the file is still created.
    steps.push(chroot(&format!(
        "base64 -d {tmp} > {home}/{rel_path} && rm -f {tmp}"
    )));
    steps
}

/// Write `content` to an absolute path inside the target. `mnt_path` is the
/// path as seen from OUTSIDE the chroot (e.g. "/mnt/etc/nftables.conf"); we
/// strip the leading "/mnt" and write it from inside the chroot so ownership
/// and context are the target's. Uses a single-quoted heredoc so the body is
/// preserved verbatim (no shell expansion of $, backticks, etc.).
/// The parent directory is created first: not every config dir exists in a
/// fresh Artix base (e.g. /etc/sysctl.d), and a bare `cat >` into a missing
/// directory fails the whole install step.
fn write_target_file(mnt_path: &str, content: &str) -> Action {
    let in_chroot = mnt_path.strip_prefix("/mnt").unwrap_or(mnt_path);
    let dir = std::path::Path::new(in_chroot)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".into());
    let script = format!(
        "mkdir -p {dir} && cat > {in_chroot} <<'ARTIX_INSTALLER_EOF'\n{content}\nARTIX_INSTALLER_EOF"
    );
    chroot(&script)
}

/// .zshrc shipped to the user when they pick zsh. Mirrors a typical oh-my-zsh
/// feature set using the distro's plugin packages, then starts starship.
const ZSHRC_TEMPLATE: &str = r#"# ~/.zshrc — generated by the Artix installer

# History
HISTFILE=~/.zsh_history
HISTSIZE=50000
SAVEHIST=50000
setopt SHARE_HISTORY HIST_IGNORE_ALL_DUPS HIST_IGNORE_SPACE INC_APPEND_HISTORY

# Completion system
autoload -Uz compinit && compinit
zstyle ':completion:*' menu select
zstyle ':completion:*' matcher-list 'm:{a-zA-Z}={A-Za-z}'
setopt AUTO_CD

# Plugins (installed from the repos)
[ -f /usr/share/zsh/plugins/zsh-autosuggestions/zsh-autosuggestions.zsh ] && \
  source /usr/share/zsh/plugins/zsh-autosuggestions/zsh-autosuggestions.zsh
[ -f /usr/share/zsh/plugins/zsh-history-substring-search/zsh-history-substring-search.zsh ] && \
  source /usr/share/zsh/plugins/zsh-history-substring-search/zsh-history-substring-search.zsh
# Syntax highlighting must be sourced last.
[ -f /usr/share/zsh/plugins/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh ] && \
  source /usr/share/zsh/plugins/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh

# History substring search: bind up/down arrows
bindkey '^[[A' history-substring-search-up
bindkey '^[[B' history-substring-search-down

# Handy aliases
alias ls='ls --color=auto'
alias ll='ls -lah'
alias grep='grep --color=auto'
alias ..='cd ..'

# Starship prompt
command -v starship >/dev/null && eval "$(starship init zsh)"
"#;

/// config.fish shipped to the user when they pick fish.
const FISH_CONFIG_TEMPLATE: &str = r#"# ~/.config/fish/config.fish — generated by the Artix installer

if status is-interactive
    # Cleaner greeting
    set -g fish_greeting

    # Handy abbreviations (expand as you type)
    abbr -a ll 'ls -lah'
    abbr -a .. 'cd ..'
    abbr -a ... 'cd ../..'
    abbr -a gs 'git status'
    abbr -a gc 'git commit'
    abbr -a gp 'git push'

    # Color ls/grep
    alias ls='ls --color=auto'
    alias grep='grep --color=auto'

    # Starship prompt
    command -v starship >/dev/null && starship init fish | source
end
"#;

const KITTY_CONFIG_TEMPLATE: &str = r#"# vim:ft=kitty

## name:     Catppuccin Kitty Mocha
## author:   Catppuccin Org
## license:  MIT
## upstream: https://github.com/catppuccin/kitty/blob/main/themes/mocha.conf
## blurb:    Soothing pastel theme for the high-spirited!

# The basic colors
foreground              #cdd6f4
background              #1e1e2e
selection_foreground    #1e1e2e
selection_background    #f5e0dc

# Cursor colors
cursor                  #f5e0dc
cursor_text_color       #1e1e2e

# URL underline color when hovering with mouse
url_color               #f5e0dc

# Kitty window border colors
active_border_color     #b4befe
inactive_border_color   #6c7086
bell_border_color       #f9e2af

# OS Window titlebar colors
wayland_titlebar_color system
macos_titlebar_color system

# Tab bar colors
active_tab_foreground   #11111b
active_tab_background   #cba6f7
inactive_tab_foreground #cdd6f4
inactive_tab_background #181825
tab_bar_background      #11111b

# Colors for marks (marked text in the terminal)
mark1_foreground #1e1e2e
mark1_background #b4befe
mark2_foreground #1e1e2e
mark2_background #cba6f7
mark3_foreground #1e1e2e
mark3_background #74c7ec

# The 16 terminal colors

# black
color0 #45475a
color8 #585b70

# red
color1 #f38ba8
color9 #f38ba8

# green
color2  #a6e3a1
color10 #a6e3a1

# yellow
color3  #f9e2af
color11 #f9e2af

# blue
color4  #89b4fa
color12 #89b4fa

# magenta
color5  #f5c2e7
color13 #f5c2e7

# cyan
color6  #94e2d5
color14 #94e2d5

# white
color7  #bac2de
color15 #a6adc8

tab_bar_min_tabs            1
tab_bar_edge                bottom
tab_bar_style               powerline
tab_powerline_style         slanted
tab_title_template          {title}{' :{}:'.format(num_windows) if num_windows > 1 else ''}

background_opacity 0.8

"#;

/// fastfetch config (the user's JSONC) and its logo image, embedded at compile
/// time so they ship inside the installer binary — no ISO asset dependency.
/// Both are written into ~/.config/fastfetch on the installed system.
/// Paths are relative to THIS source file (src/system/ → ../assets/).
const FASTFETCH_CONFIG: &str = include_str!("../assets/fastfetch.jsonc");
const FASTFETCH_LOGO_PNG: &[u8] = include_bytes!("../assets/fastfetch.png");

/// wofi (Wayland launcher) config + stylesheet, embedded at compile time.
/// Written into ~/.config/wofi only when wofi is among the selected packages
/// (it's Wayland-only, so it's offered as an optional pick, not a default).
const WOFI_CONFIG: &str = include_str!("../assets/wofi.config");
const WOFI_STYLE_CSS: &str = include_str!("../assets/wofi.style.css");

/// waybar (Wayland status bar) config + stylesheet, embedded at compile time.
/// Written into ~/.config/waybar only when waybar is among the selected
/// packages (Wayland-only, so an optional pick rather than a default).
const WAYBAR_CONFIG: &str = include_str!("../assets/waybar.config.jsonc");
const WAYBAR_STYLE_CSS: &str = include_str!("../assets/waybar.style.css");

/// pinnacle (Wayland compositor) config tree, embedded at compile time as a
/// gzip tarball (Cargo.toml/lock, pinnacle.toml, src/main.rs, scripts/). It's
/// extracted into ~/.config/pinnacle only when pinnacle-comp is among the
/// selected AUR packages. Shipped as one archive because it's a multi-file,
/// multi-directory tree — far cleaner than a dozen include_str! constants.
const PINNACLE_CONFIG_TARBALL: &[u8] = include_bytes!("../assets/pinnacle.tar.gz");

const NFTABLES_CONFIG_TEMPLATE: &str = r#"#!/usr/sbin/nft -f
#
# Default-deny stateful firewall for an Artix/dinit desktop.
# Inbound is dropped except for established/related, loopback, ICMP, mDNS, and
# the explicit application ports below. Outbound is allowed.
#
# Installed to /etc/nftables.conf and started via nftables-dinit.

flush ruleset

table inet filter {

    # ── Application port sets ────────────────────────────────────────────────
    # KDE Connect: TCP+UDP 1714-1764 (device pairing, sharing, clipboard, etc.)
    set kdeconnect_ports {
        type inet_service
        flags interval
        elements = { 1714-1764 }
    }

    # LocalSend: TCP+UDP 53317 (HTTP file transfer + multicast discovery).
    set localsend_ports {
        type inet_service
        elements = { 53317 }
    }

    # Sunshine (game streaming, Moonlight host):
    #   TCP 47984, 47989, 47990, 48010
    #   UDP 47998, 47999, 48000, 48002, 48010
    set sunshine_tcp {
        type inet_service
        elements = { 47984, 47989, 47990, 48010 }
    }
    set sunshine_udp {
        type inet_service
        elements = { 47998, 47999, 48000, 48002, 48010 }
    }

    # RustDesk (direct / peer-to-peer connections):
    #   TCP 21115-21119, UDP 21116
    set rustdesk_tcp {
        type inet_service
        flags interval
        elements = { 21115-21119 }
    }
    set rustdesk_udp {
        type inet_service
        elements = { 21116 }
    }

    # Steam Remote Play / In-Home Streaming:
    #   TCP 27036-27037, UDP 27031-27036
    set steam_tcp {
        type inet_service
        flags interval
        elements = { 27036-27037 }
    }
    set steam_udp {
        type inet_service
        flags interval
        elements = { 27031-27036 }
    }

    # Syncthing (file synchronisation):
    #   TCP 22000 (sync), UDP 22000 (QUIC sync), UDP 21027 (local discovery)
    set syncthing_tcp {
        type inet_service
        elements = { 22000 }
    }
    set syncthing_udp {
        type inet_service
        elements = { 22000, 21027 }
    }

    chain input {
        type filter hook input priority filter; policy drop;

        # Stateful: allow what we initiated.
        ct state established,related accept
        ct state invalid drop

        # Loopback.
        iif "lo" accept

        # ICMP / ICMPv6 (ping, path MTU, neighbor discovery).
        ip protocol icmp accept
        ip6 nexthdr ipv6-icmp accept

        # mDNS / Avahi discovery (LocalSend, KDE Connect, printers).
        udp dport 5353 accept

        # SSH (remote administration).
        tcp dport 22 accept

        # ── Applications ──
        tcp dport @kdeconnect_ports accept
        udp dport @kdeconnect_ports accept

        tcp dport @localsend_ports accept
        udp dport @localsend_ports accept

        tcp dport @sunshine_tcp accept
        udp dport @sunshine_udp accept

        tcp dport @rustdesk_tcp accept
        udp dport @rustdesk_udp accept

        tcp dport @steam_tcp accept
        udp dport @steam_udp accept

        tcp dport @syncthing_tcp accept
        udp dport @syncthing_udp accept

        # Everything else: drop (policy).
    }

    chain forward {
        type filter hook forward priority filter; policy drop;
    }

    chain output {
        type filter hook output priority filter; policy accept;
    }
}
"#;

/// The genuine X.Org server and the common X utility apps. Installed for X11
/// desktops so the system uses real Xorg rather than Artix's new default
/// XLibre (which can be flaky with some software). XLibre only replaces
/// `xorg-server` + xf86 drivers, so pulling `xorg-server` explicitly pins the
/// real server; the rest are standard X tools every X session expects.
const XORG_PACKAGES: &[&str] = &[
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

/// Public helper for other modules (e.g. the summary screen): is the desktop
/// named by this serialized string a Wayland desktop?
pub fn desktop_is_wayland(s: &str, session: &str) -> bool {
    desktop_from(s).session_is_wayland(session)
}

/// Whether the named desktop has any graphical session at all (i.e. not None).
pub fn desktop_has_session(s: &str) -> bool {
    !matches!(desktop_from(s), Desktop::None)
}

fn desktop_from(s: &str) -> Desktop {
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
fn effective_aur_packages(c: &InstallConfig) -> Vec<String> {
    let mut out: Vec<String> = c.aur_packages.clone();
    if desktop_from(&c.desktop) == Desktop::Pinnacle {
        for pkg in ["pinnacle-comp", "waypaper", "flameshot-git"] {
            if !out.iter().any(|x| x == pkg) {
                out.push(pkg.to_string());
            }
        }
    }
    out
}

/// The LUKS portion of the kernel command line for non-GRUB bootloaders.
/// Empty when not encrypting. Root itself is added at install time in the
/// chroot (resolved to a UUID dynamically), so this only carries the
/// cryptdevice mapping for root-only LUKS.
fn luks_cmdline_part(c: &InstallConfig) -> String {
    if c.encrypt_disk {
        let mut s = "cryptdevice=PARTLABEL=ROOT:cryptroot ".to_string();
        // USB auto-unlock: the encrypt hook reads the keyfile from the stick
        // (resolved by filesystem LABEL, so no runtime UUID capture needed).
        // If the stick is absent at boot the hook simply falls back to the
        // passphrase prompt — the passphrase stays in LUKS slot 0.
        if !c.usb_key_device.is_empty() && c.encrypt_scope != "full" {
            s.push_str("cryptkey=LABEL=ARTIXKEY:vfat:/artix-luks.key ");
        }
        s
    } else {
        String::new()
    }
}

/// Kernel cmdline fragment for btrfs subvolume layouts. The bootloader must be
/// told the root subvolume, otherwise the kernel mounts the top-level subvolume
/// (id 5) — which holds @, @home, … as directories but no /sbin/init — and the
/// boot fails. GRUB derives this itself via grub-mkconfig (it reads the live
/// mount), but rEFInd and Limine build their cmdline by hand, so we add it
/// explicitly there. Empty in every other case. Trailing space matches the
/// luks_cmdline_part convention so fragments concatenate cleanly.
fn rootflags_part(c: &InstallConfig) -> String {
    if c.root_fs == "btrfs" && c.btrfs_subvolumes {
        "rootflags=subvol=@ ".to_string()
    } else {
        String::new()
    }
}

/// Partition, format, and mount the additional WHOLE disks the user chose to
/// format (format == true). Each gets a single GPT partition spanning the disk,
/// the chosen filesystem, and is mounted under /mnt at its target so basestrap
/// and fstabgen see it. Existing partitions to mount as-is (format == false) are
/// NOT touched here — they're added to fstab after fstabgen, data preserved.
fn extra_disks_plan(c: &InstallConfig) -> Vec<Action> {
    let mut plan = Vec::new();
    for d in &c.extra_disks {
        if !d.format || d.mountpoint.is_empty() {
            continue;
        }
        // The LUKS / mkfs target: a whole disk gets a fresh GPT + one partition;
        // an existing partition being reformatted is mkfs'd IN PLACE (no wipefs,
        // no repartition — the partition table is left intact, only this
        // partition's contents are replaced).
        let base_dev = if d.whole_disk {
            let p1 = disk::part(&d.disk, 1);
            plan.push(act("wipefs", &["-a", &d.disk]));
            plan.push(act("sgdisk", &["--zap-all", &d.disk]));
            // One Linux-filesystem partition (type 8300) spanning the whole disk.
            plan.push(act("sgdisk", &["-n", "1:0:0", "-t", "1:8300", &d.disk]));
            plan.push(act(
                "sh",
                &["-c", &format!("partprobe {} 2>/dev/null; udevadm settle 2>/dev/null; sleep 1", d.disk)],
            ));
            p1
        } else {
            // Reformatting an existing partition: clear any old filesystem
            // signature so mkfs starts clean, but keep the partition itself.
            plan.push(act("wipefs", &["-a", &d.disk]));
            d.disk.clone()
        };

        // For an encrypted disk, LUKS-format the target with a random keyfile
        // (kept in the live /tmp for now; copied to the encrypted root and wired
        // to a dinit auto-unlock service after basestrap). mkfs then runs on the
        // opened mapper, not the raw device.
        // Resolve a home-relative ("~/name") mountpoint to the real path now that
        // the username is known; presets and /mnt paths pass through unchanged.
        let mp = resolve_mp(c, &d.mountpoint);
        // to a dinit auto-unlock service after basestrap). mkfs then runs on the
        // opened mapper, not the raw device.
        let dev = if d.encrypt {
            let mapper = crypt_mapper(&mp);
            let keyfile = format!("/tmp/{mapper}.key");
            plan.push(act(
                "sh",
                &["-c", &format!("dd if=/dev/urandom of={keyfile} bs=512 count=8 2>/dev/null; chmod 600 {keyfile}")],
            ));
            plan.push(act(
                "sh",
                &["-c", &format!("cryptsetup -q luksFormat --key-file {keyfile} {base_dev}")],
            ));
            plan.push(act(
                "sh",
                &["-c", &format!("cryptsetup open --key-file {keyfile} {base_dev} {mapper}")],
            ));
            format!("/dev/mapper/{mapper}")
        } else {
            base_dev.clone()
        };

        match d.fs.as_str() {
            "btrfs" => plan.push(act("mkfs.btrfs", &["-f", &dev])),
            "xfs" => plan.push(act("mkfs.xfs", &["-f", &dev])),
            "f2fs" => plan.push(act("mkfs.f2fs", &["-f", &dev])),
            "jfs" => plan.push(act("mkfs.jfs", &["-q", &dev])),
            "ext3" => plan.push(act("mkfs.ext3", &["-F", &dev])),
            "ext2" => plan.push(act("mkfs.ext2", &["-F", &dev])),
            _ => plan.push(act("mkfs.ext4", &["-F", &dev])),
        }
        let target = format!("/mnt{}", mp);
        plan.push(act("mkdir", &["-p", &target]));
        // Per-disk mount options chosen in the storage screen's options modal.
        let mut opts: Vec<&str> = Vec::new();
        if d.noatime {
            opts.push("noatime");
        }
        if d.compress && d.fs == "btrfs" {
            opts.push("compress=zstd");
        }
        if opts.is_empty() {
            plan.push(act("mount", &[&dev, &target]));
        } else {
            let o = opts.join(",");
            plan.push(act("mount", &["-o", &o, &dev, &target]));
        }
        // A freshly formatted custom folder belongs to the user (uid 1000 — the
        // single account this installer creates), so they can write to it. Kept
        // (non-formatted) mounts keep their existing ownership / mount options.
        if d.bookmark && d.format {
            plan.push(act("chown", &["1000:1000", &target]));
        }
    }
    plan
}

/// Resolve a home-relative mountpoint ("~/name") to "/home/<user>/name". Other
/// paths (presets, /mnt/...) are returned unchanged.
fn resolve_mp(c: &InstallConfig, mp: &str) -> String {
    if let Some(rest) = mp.strip_prefix("~/") {
        format!("/home/{}/{}", c.username, rest)
    } else {
        mp.to_string()
    }
}

/// Stable device-mapper name for an encrypted extra disk, derived from its
/// mountpoint (unique on the storage screen): "/mnt/storage" -> "crypt_mnt_storage".
fn crypt_mapper(mp: &str) -> String {
    let s: String = mp
        .trim_start_matches('/')
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect();
    format!("crypt_{}", if s.is_empty() { "data".to_string() } else { s })
}

fn gpu_from(s: &str) -> GpuDriver {
    match s {
        "Nvidia" => GpuDriver::Nvidia,
        "Nvidia580xx" => GpuDriver::Nvidia580xx,
        "Nouveau" => GpuDriver::Nouveau,
        "Amd" => GpuDriver::Amd,
        "Intel" => GpuDriver::Intel,
        _ => GpuDriver::None,
    }
}

/// Parse the (comma-separated) GPU selection into the list of drivers. The UI
/// allows combining drivers for hybrid graphics (e.g. "Nvidia,Amd" on a laptop
/// with an NVIDIA dGPU and AMD iGPU).
fn gpus_from(s: &str) -> Vec<GpuDriver> {
    let v: Vec<GpuDriver> = s
        .split(',')
        .map(|p| gpu_from(p.trim()))
        .filter(|g| !matches!(g, GpuDriver::None))
        .collect();
    if v.is_empty() { vec![GpuDriver::None] } else { v }
}

/// True if any selected GPU driver is a proprietary NVIDIA stack (which needs
/// nvidia-persistenced and a nouveau blacklist).
fn any_proprietary_nvidia(s: &str) -> bool {
    gpus_from(s)
        .iter()
        .any(|g| matches!(g, GpuDriver::Nvidia | GpuDriver::Nvidia580xx))
}

fn kernel_from(s: &str) -> Kernel {
    match s {
        "Zen" => Kernel::Zen,
        "Hardened" => Kernel::Hardened,
        "Lts" => Kernel::Lts,
        _ => Kernel::Linux,
    }
}

/// The /boot image file names for the chosen kernel: (vmlinuz, initramfs).
/// Bootloaders with a static config (Limine) must point at the REAL files —
/// hardcoding vmlinuz-linux breaks every non-default kernel choice.
fn kernel_images(s: &str) -> (&'static str, &'static str) {
    match kernel_from(s) {
        Kernel::Linux => ("vmlinuz-linux", "initramfs-linux.img"),
        Kernel::Zen => ("vmlinuz-linux-zen", "initramfs-linux-zen.img"),
        Kernel::Hardened => ("vmlinuz-linux-hardened", "initramfs-linux-hardened.img"),
        Kernel::Lts => ("vmlinuz-linux-lts", "initramfs-linux-lts.img"),
    }
}

/// A 64-hex-char throwaway passphrase from the kernel CSPRNG, for key-only
/// USB encryption (the user never sees or needs it). /dev/urandom can't
/// realistically fail on the live system; the fallback only exists so a
/// broken environment degrades to a weaker secret instead of a mid-install
/// panic — and that secret is still removed from the container at the end.
fn random_passphrase() -> String {
    use std::io::Read;
    let mut buf = [0u8; 32];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok()
    {
        buf.iter().map(|b| format!("{b:02x}")).collect()
    } else {
        format!(
            "{:x}{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        )
    }
}

/// Packages for the chosen display manager. All names verified to exist in the
/// OFFICIAL Artix repos (no AUR): greetd-tuigreet (world), greetd-regreet
/// (galaxy), sddm (world). The *-dinit service packages are Artix's.
/// xorg-xinit ships `startx`, used by tuigreet's --xsession-wrapper for X11
/// sessions (tiny, so always bundled with greetd). cage is the kiosk
/// compositor that hosts ReGreet (a GTK Wayland greeter).
fn dm_packages(dm: &str) -> Vec<&'static str> {
    match dm {
        "sddm" => vec!["sddm", "sddm-dinit"],
        "tuigreet" => vec!["greetd", "greetd-dinit", "greetd-tuigreet", "xorg-xinit"],
        "regreet" => vec!["greetd", "greetd-dinit", "greetd-regreet", "cage", "xorg-xinit"],
        _ => vec![],
    }
}

/// The dinit SERVICE to enable for the chosen display manager (service files
/// are named after the daemon, not the package).
fn dm_service(dm: &str) -> Option<&'static str> {
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
fn expand_group(name: &str) -> Vec<String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SYNCED: AtomicBool = AtomicBool::new(false);
    if !SYNCED.swap(true, Ordering::Relaxed) {
        // -Sy once so -Sgq has data to read. Ignore failure (offline → we just
        // fall back to passing the group name through).
        let _ = super::runner::capture("pacman", &["-Sy", "--noconfirm"]);
    }
    match super::runner::capture("pacman", &["-Sgq", name]) {
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

fn base_packages(c: &InstallConfig) -> Vec<String> {
    let mut p: Vec<String> = vec![
        "base", "base-devel", "dinit", "elogind-dinit", "linux-firmware",
        "networkmanager", "networkmanager-dinit", "nano", "geany", "geany-plugins", "sudo", "grub",
        "efibootmgr", "os-prober", "dbus-dinit", "dbus-dinit-user", "mkinitcpio",
        // fastfetch ships by default with every install (any DE/WM or none):
        // the distro drops a themed fastfetch config + logo into the user's
        // ~/.config/fastfetch, so the tool must always be present to read it.
        "fastfetch",
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

    // The kernel chosen by the user (linux / zen / hardened / lts) + headers.
    p.extend(kernel_from(&c.kernel).packages().iter().map(|s| s.to_string()));
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
    match c.bootloader.as_str() {
        "refind" => p.push("refind".into()),
        "limine" => p.push("limine".into()),
        _ => {}
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
    p.extend(DINIT_PACKAGES.iter().map(|s| s.to_string()));

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
    if c.seat_provider == "elogind" {
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
fn system_packages(c: &InstallConfig) -> Vec<String> {
    let mut p: Vec<String> = Vec::new();
    let de = desktop_from(&c.desktop);
    // The desktop may declare package *groups* (e.g. "plasma"). We expand groups
    // into explicit members so pacman doesn't show the group-selection prompt.
    for item in de.packages() {
        p.extend(expand_group(item));
    }
    // LXQt's native Wayland session lives in a separate package
    // (lxqt-wayland-session ships /usr/bin/startlxqtwayland + the
    // wayland-sessions .desktop entry) and needs a compositor — labwc is its
    // documented default/fallback. Without these two, a "Wayland" pick for
    // LXQt would silently install an X11-only desktop.
    if matches!(de, Desktop::Lxqt) && de.session_is_wayland(&c.session) {
        p.push("lxqt-wayland-session".into());
        p.push("labwc".into());
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
        //   • cliphist, wofi  — clipboard history + launcher (mod+space)
        //   • xorg-xwayland   — X11 apps under the Wayland session
        // wpctl is part of wireplumber (already pulled in by audio); waypaper +
        // flameshot-git are AUR-only and handled in effective_aur_packages.
        // The config's web-browser keybind launches `firefox` (install it if you
        // want that key to work). The shipped Pinnacle config has no personal
        // apps in autostart, so nothing else is force-installed here.
        for pkg in [
            "wl-clipboard", "cliphist", "wofi", "waybar", "pasystray", "xorg-xwayland",
            "caja", "swaync", "brightnessctl", "playerctl",
            "network-manager-applet", "lxsession",
            "xdg-desktop-portal", "xdg-desktop-portal-wlr", "kdeconnect",
        ] {
            if !p.iter().any(|x| x == pkg) {
                p.push(pkg.to_string());
            }
        }
    }
    // Union the packages of ALL selected GPU drivers (hybrid graphics),
    // deduplicated (mesa/lib32-mesa overlap between Intel/AMD/nouveau).
    for g in gpus_from(&c.gpu) {
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
    p.extend(dm_packages(&c.display_manager).iter().map(|s| s.to_string()));
    // elogind library/package: always present for a graphical desktop (Plasma,
    // polkit, portals link against libelogind). For "no DE" it's only added if
    // elogind is the chosen seat backend (below).
    if !matches!(de, Desktop::None) {
        p.push("elogind".into());
    }
    // Seat/login backend — installed ALWAYS, even with no desktop. A user who
    // skips the DE will install their own compositor/WM later, and it still
    // needs a seat manager (seatd or elogind) to acquire input/DRM; without one
    // there'd be no working graphical session. We honour the user's choice.
    match c.seat_provider.as_str() {
        "elogind" => {
            p.push("elogind".into());
            p.push("elogind-dinit".into());
        }
        _ => {
            p.push("seatd".into());
            p.push("seatd-dinit".into());
        }
    }
    p.sort();
    p.dedup();
    p
}

/// Build the entire install plan in order.
/// Maps the chosen timezone to mirror-list country NAMES (as they appear in the
/// `## Country` headers of the Artix/Arch mirrorlists), used to find and
/// uncomment the regional mirrors. The user's own country comes first, then
/// nearby neighbors. rankmirrors then sorts these by real speed, so the set
/// only needs to be "the right part of the world". Falls back to a continent
/// set, then a safe default. Names must match the generators' headers exactly.
/// Shell script that rewrites a pacman mirrorlist so the regional mirrors are
/// active and ranked by speed at the TOP, the full list is kept (commented)
/// below for easy relocation, and unwanted mirrors are filtered out. It's a script
/// (not inline) because the logic is substantial and reused for Artix and Arch.
/// Positional args ($@) are the regional country names to surface. Everything
/// is best-effort: missing rankmirrors, no network, or an unreachable generator
/// all fall back gracefully, never aborting the install.
const MIRROR_OPTIMIZE_SCRIPT: &str = r###"#!/bin/sh
set -u
log() { echo ">>> $*"; }

MODE=region
case "${1:-}" in
  --arch) MODE=arch; shift;;
  --chaotic) MODE=chaotic; shift;;
esac

HAVE_RANK=0
command -v rankmirrors >/dev/null 2>&1 && HAVE_RANK=1

# country names -> /tmp/mo_args (region/arch modes)
: > /tmp/mo_args
for a in "$@"; do printf '%s\n' "$a" >> /tmp/mo_args; done

# Silently filter out excluded mirrors: their section headers, their
# server lines, and the matching CDN country entries. Nothing about them
# is left anywhere in the generated list.
strip_ru() {
  awk '
    /[Rr]ussia/ { next }
    /[Ss]erver[[:space:]]*=.*\.ru\//                       { next }
    /[Ss]erver[[:space:]]*=.*\/\/ru-?[0-9]*-?mirror\.chaotic/ { next }
    { print }
  '
}

# Active block for Artix/Arch: per target country, a "## Country" header then
# its uncommented Server lines (ranked within the country when possible).
build_regional() {
  full="$1"; : > /tmp/mo_active
  while IFS= read -r country; do
    [ -n "$country" ] || continue
    servers=$(awk -v C="$country" '$0==("## " C){f=1;next} /^## /{f=0} f' "$full" \
              | sed -E 's/^#*[[:space:]]*Server/Server/' | grep '^Server' || true)
    [ -n "$servers" ] || continue
    # Bound the candidate set so rankmirrors stays fast even for countries with
    # 100+ mirrors (e.g. Germany), then keep only the few fastest — "nearest",
    # not "every mirror in the country".
    servers=$(printf '%s\n' "$servers" | head -n 8)
    if [ "$HAVE_RANK" = 1 ]; then
      ranked=$(printf '%s\n' "$servers" | rankmirrors - 2>/dev/null | grep '^Server' || true)
      [ -n "$ranked" ] && servers="$ranked"
    fi
    servers=$(printf '%s\n' "$servers" | head -n 4)
    printf '## %s\n%s\n\n' "$country" "$servers" >> /tmp/mo_active
  done < /tmp/mo_args
}

# Map a country NAME to its Chaotic-AUR 2-letter code (only those Chaotic
# actually hosts a country mirror for); empty when there is none.
chaotic_code() {
  case "$1" in
    Poland) echo pl;; Germany) echo de;; France) echo fr;; Italy) echo it;;
    Spain) echo es;; Sweden) echo se;; Greece) echo gr;; Switzerland) echo ch;;
    "United Kingdom") echo gb;; Netherlands) echo nl;; "United States") echo us;;
    Canada) echo ca;; Brazil) echo br;; Japan) echo jp;; "South Korea") echo kr;;
    Taiwan) echo tw;; Singapore) echo sg;; "Hong Kong") echo hk;; India) echo in;;
    Australia) echo au;; "New Zealand") echo nz;; Indonesia) echo id;;
    Israel) echo il;; Mexico) echo mx;; Chile) echo cl;; Colombia) echo co;;
    Peru) echo pe;; "Saudi Arabia") echo sa;; "South Africa") echo za;;
    Thailand) echo th;; "United Arab Emirates") echo ae;; Argentina) echo ar;;
    Vietnam) echo vn;; Nigeria) echo ng;; *) echo "";;
  esac
}

# Active block for chaotic: geo-mirror (auto-routes to closest) + cdn-mirror
# (reliable fallback), then the per-country virtual mirror for each nearby
# region Chaotic hosts (e.g. for Ukraine's neighbours -> pl, de). Each of those
# auto-routes within its country, making solid close fallbacks behind geo.
build_chaotic_active() {
  full="$1"; : > /tmp/mo_active
  geo=$(grep -E '^#?[[:space:]]*Server[[:space:]]*=.*geo-mirror\.chaotic' "$full" | head -1 | sed -E 's/^#*[[:space:]]*//')
  cdn=$(grep -E '^#?[[:space:]]*Server[[:space:]]*=.*cdn-mirror\.chaotic' "$full" | head -1 | sed -E 's/^#*[[:space:]]*//')
  { [ -n "$geo" ] && printf '## Geo mirror (auto-routes to the closest up-to-date mirror)\n%s\n\n' "$geo"
    [ -n "$cdn" ] && printf '## CDN mirror (reliable fallback)\n%s\n\n' "$cdn"; } >> /tmp/mo_active
  while IFS= read -r country; do
    [ -n "$country" ] || continue
    code=$(chaotic_code "$country")
    [ -n "$code" ] || continue
    line=$(grep -E "^#?[[:space:]]*Server[[:space:]]*=.*//${code}-mirror\.chaotic" "$full" | head -1 | sed -E 's/^#*[[:space:]]*//')
    [ -n "$line" ] && printf '## %s (closest country mirror)\n%s\n\n' "$country" "$line" >> /tmp/mo_active
  done < /tmp/mo_args
}

assemble() {
  ml="$1"; label="$2"; full="$3"; out=/tmp/mo_out
  {
    echo "## $label mirrors"
    echo "## Closest mirrors for your region are active below; the full list"
    echo "## (commented) follows. Uncomment any Server line to use it."
    echo "##"
    echo ""
    if [ -s /tmp/mo_active ]; then cat /tmp/mo_active
    else echo "## (no regional match - uncomment one from the full list below)"; echo ""; fi
    echo "## ------------------------------------------------------------------"
    echo "## Full mirror list (commented). Uncomment any Server to use it."
    echo "## If you relocate: comment distant mirrors and uncomment closer ones."
    echo "## ------------------------------------------------------------------"
    sed -E 's/^[[:space:]]*Server/#Server/' "$full"
  } > "$out"
  mv "$out" "$ml"
}

process() {
  ml="$1"; gen="$2"; label="$3"; mode="$4"
  [ -e "$ml" ] || { log "$label: $ml absent, skipping."; return 0; }
  cp "$ml" "$ml.bak" 2>/dev/null || true
  full=/tmp/mo_full; : > "$full"
  if [ "$gen" != "-" ]; then
    curl -fsS --connect-timeout 10 --max-time 45 "$gen" 2>/dev/null > "$full" || : > "$full"
  fi
  [ -s "$full" ] || cp "$ml.bak" "$full" 2>/dev/null || : > "$full"
  [ -s "$full" ] || { log "$label: no mirror data, skipping."; return 0; }
  strip_ru < "$full" > "$full.x" && mv "$full.x" "$full"
  if [ "$mode" = chaotic ]; then build_chaotic_active "$full"; else build_regional "$full"; fi
  assemble "$ml" "$label" "$full"
  na=$(grep -c '^Server' "$ml" 2>/dev/null || echo 0)
  log "$label: $na active mirror(s) on top; full list below."
}

case "$MODE" in
  chaotic) process /etc/pacman.d/chaotic-mirrorlist "-" "Chaotic-AUR" chaotic;;
  arch)    process /etc/pacman.d/mirrorlist-arch "https://archlinux.org/mirrorlist/all/" "Arch" region;;
  *)       process /etc/pacman.d/mirrorlist "https://packages.artixlinux.org/mirrorlist/?country=all&protocol=https" "Artix" region;;
esac
log "Mirror optimization complete.""###;

fn mirror_region_countries(timezone: &str) -> &'static [&'static str] {
    match timezone {
        "Europe/Kyiv" | "Europe/Kiev" => &["Ukraine", "Poland", "Slovakia", "Hungary", "Moldova", "Romania", "Germany"],
        "Europe/Warsaw" => &["Poland", "Germany", "Czechia", "Ukraine", "Slovakia", "Lithuania"],
        "Europe/Berlin" | "Europe/Vienna" | "Europe/Zurich" => &["Germany", "Netherlands", "Austria", "Czechia", "Poland", "France", "Switzerland"],
        "Europe/Paris" | "Europe/Brussels" | "Europe/Amsterdam" => &["France", "Netherlands", "Germany", "Belgium", "United Kingdom"],
        "Europe/London" | "Europe/Dublin" => &["United Kingdom", "Ireland", "Netherlands", "France", "Germany"],
        "Europe/Moscow" => &["Finland", "Ukraine", "Germany", "Kazakhstan"],
        "Europe/Madrid" | "Europe/Lisbon" => &["Spain", "Portugal", "France", "Germany"],
        "Europe/Rome" => &["Italy", "France", "Germany", "Austria", "Switzerland"],
        "Europe/Stockholm" | "Europe/Helsinki" | "Europe/Oslo" | "Europe/Copenhagen" => &["Sweden", "Finland", "Norway", "Denmark", "Germany"],
        "America/New_York" | "America/Toronto" | "America/Chicago" => &["United States", "Canada"],
        "America/Los_Angeles" | "America/Vancouver" | "America/Denver" => &["United States", "Canada"],
        "America/Sao_Paulo" | "America/Argentina/Buenos_Aires" => &["Brazil", "Chile", "United States"],
        "Asia/Tokyo" => &["Japan", "South Korea", "Taiwan", "Singapore", "Hong Kong"],
        "Asia/Singapore" | "Asia/Kuala_Lumpur" | "Asia/Jakarta" => &["Singapore", "Hong Kong", "Japan", "India"],
        "Asia/Kolkata" | "Asia/Calcutta" => &["India", "Singapore", "Hong Kong"],
        "Asia/Shanghai" | "Asia/Hong_Kong" | "Asia/Taipei" => &["Hong Kong", "Taiwan", "Singapore", "Japan"],
        "Australia/Sydney" | "Australia/Melbourne" | "Australia/Perth" => &["Australia", "New Zealand", "Singapore"],
        _ => match timezone.split('/').next().unwrap_or("") {
            "Europe" => &["Germany", "France", "Netherlands", "Poland", "United Kingdom", "Sweden", "Czechia", "Austria", "Finland"],
            "America" => &["United States", "Canada", "Brazil"],
            "Asia" => &["Japan", "Singapore", "India", "South Korea", "Hong Kong", "Taiwan"],
            "Africa" => &["South Africa", "Germany", "France"],
            "Australia" | "Pacific" => &["Australia", "New Zealand", "Singapore"],
            "Indian" => &["India", "Singapore", "South Africa"],
            "Atlantic" => &["United Kingdom", "United States", "Germany"],
            _ => &["Germany", "United States", "Netherlands"],
        },
    }
}

/// Logging help document, written into the user's home in their chosen
/// language. Explains the install log plus how to use the system's logging
/// (syslog-ng text logs) and service manager (dinit): per-program output,
/// following in real time, and the rest.
const LOG_HELP_UK: &str = r##"# Як читати логи системи

Цей дистрибутив використовує **syslog-ng** для системних логів і **dinit** для
керування службами. Нижче — як знаходити й читати логи.

## Лог встановлення

Повний журнал встановлення цієї системи лежить поряд із цим файлом:

    less ~/installer.log

## Системні логи (syslog-ng)

Логи — це звичайні текстові файли в `/var/log/`:

- `/var/log/everything.log` — усе разом (найзручніше для пошуку)
- `/var/log/messages.log`   — загальні системні повідомлення
- `/var/log/auth.log`       — вхід, sudo, автентифікація
- `/var/log/kernel.log`     — повідомлення ядра
- `/var/log/daemon.log`     — служби та демони

### У реальному часі

Стежити за логом наживо (вийти — Ctrl+C):

    sudo tail -f /var/log/everything.log

### Лише конкретна програма

Відфільтрувати рядки певної програми чи служби:

    sudo grep 'NetworkManager' /var/log/everything.log

Те саме, але наживо:

    sudo tail -f /var/log/everything.log | grep --line-buffered 'sshd'

### Інше корисне

    sudo tail -n 100 /var/log/messages.log          # останні 100 рядків
    sudo grep -i 'error' /var/log/everything.log     # усі помилки
    sudo less +G /var/log/everything.log             # відкрити в кінці
    du -sh /var/log/*.log                            # розмір логів

## Служби (dinit)

    sudo dinitctl list                  # стан усіх служб
    sudo dinitctl status <служба>       # стан однієї
    sudo dinitctl restart <служба>      # перезапуск
    sudo dinitctl start|stop <служба>   # запуск/зупинка

### Користувацькі служби (вашого сеансу, без sudo)

    dinitctl list
    dinitctl catlog <служба>            # переглянути буфер логів служби

---
Згенеровано інсталятором Artix.
"##;

const LOG_HELP_EN: &str = r##"# How to read the system logs

This distribution uses **syslog-ng** for system logs and **dinit** to manage
services. Here is how to find and read logs.

## Install log

The full log of this system's installation sits next to this file:

    less ~/installer.log

## System logs (syslog-ng)

Logs are plain text files under `/var/log/`:

- `/var/log/everything.log` — everything together (best for searching)
- `/var/log/messages.log`   — general system messages
- `/var/log/auth.log`       — logins, sudo, authentication
- `/var/log/kernel.log`     — kernel messages
- `/var/log/daemon.log`     — services and daemons

### In real time

Follow a log live (Ctrl+C to quit):

    sudo tail -f /var/log/everything.log

### One program only

Filter the lines of a given program or service:

    sudo grep 'NetworkManager' /var/log/everything.log

The same, live:

    sudo tail -f /var/log/everything.log | grep --line-buffered 'sshd'

### Other handy commands

    sudo tail -n 100 /var/log/messages.log          # last 100 lines
    sudo grep -i 'error' /var/log/everything.log     # all errors
    sudo less +G /var/log/everything.log             # open at the end
    du -sh /var/log/*.log                            # log sizes

## Services (dinit)

    sudo dinitctl list                  # status of all services
    sudo dinitctl status <service>      # status of one
    sudo dinitctl restart <service>     # restart
    sudo dinitctl start|stop <service>  # start/stop

### User services (your session, no sudo)

    dinitctl list
    dinitctl catlog <service>           # view a service's log buffer

---
Generated by the Artix installer.
"##;

pub fn build_plan(app: &App) -> Vec<Action> {
    let c = &app.config;
    let uefi = c.boot_mode == "uefi";
    let mut plan: Vec<Action> = Vec::new();

    // Key-only USB mode: the user types NO passphrase at all. LUKS still
    // needs an initial secret to format and open the container, so we mint a
    // strong throwaway one from the kernel CSPRNG. It exists only inside this
    // plan: it authorizes setup (luksFormat/open, luksAddKey) and is removed
    // as the FINAL link of the USB chain, leaving the 4096-byte keyfile on
    // the stick as the container's only slot.
    let minted: Option<String> = if c.encrypt_disk
        && !c.usb_key_device.is_empty()
        && c.usb_key_only
        && c.luks_passphrase.is_empty()
    {
        Some(random_passphrase())
    } else {
        None
    };
    let luks_pass: &str = minted.as_deref().unwrap_or(&c.luks_passphrase);

    // ─── Pre-flight checks (run before ANY download or write) ───────────────
    // Catch problems before a single destructive step. No internet is fatal —
    // the install must fetch packages — and aborts cleanly here, before
    // partitioning. Low RAM or a small target disk are warnings only. Firmware
    // mode (UEFI/BIOS) is reported for the record.
    {
        // The no-internet message is the one the user actually acts on from the
        // failed screen, so it's localized and says exactly what to do: go back
        // to the Wi-Fi step and connect. Multiple echoes keep the lines separate
        // (echo doesn't expand \n in plain sh).
        let no_net = if c.lang == "uk" {
            "echo '!!! Немає підключення до інтернету — для встановлення потрібно завантажувати пакунки.'; \
             echo '!!! Натисніть Esc, щоб повернутися назад, відкрийте крок Wi-Fi (підключення до інтернету), під’єднайтеся, і запустіть встановлення знову.'; \
             echo '!!! Нічого не змінено — ваш диск недоторканий.'"
        } else {
            "echo '!!! No internet connection — an install must download packages.'; \
             echo '!!! Press Esc to go back, open the Wi-Fi (internet) step, connect, then start the install again.'; \
             echo '!!! Nothing has been changed — your disk is untouched.'"
        };
        let preflight = format!(
            "echo '>>> Pre-flight checks...'; \
             FAIL=0; \
             printf '>>> Internet connectivity: '; \
             if curl -fsS --connect-timeout 10 --max-time 20 -I https://artixlinux.org >/dev/null 2>&1 || curl -fsS --connect-timeout 10 --max-time 20 -I https://archlinux.org >/dev/null 2>&1; then echo OK; else echo FAILED; FAIL=1; fi; \
             if [ -d /sys/firmware/efi ]; then echo '>>> Firmware mode: UEFI'; else echo '>>> Firmware mode: BIOS/Legacy'; fi; \
             mem_kb=$(awk '/MemTotal/{{print $2}}' /proc/meminfo 2>/dev/null || echo 0); \
             mem_mb=$((mem_kb / 1024)); \
             echo \">>> RAM: ${{mem_mb}} MiB\"; \
             if [ \"$mem_mb\" -lt 1024 ]; then echo '!!! Warning: under 1 GiB RAM - the install may be slow or struggle.'; fi; \
             if [ -b '{disk}' ]; then \
               db=$(lsblk -bdno SIZE '{disk}' 2>/dev/null | head -1 || echo 0); \
               dg=$((db / 1024 / 1024 / 1024)); \
               echo \">>> Target disk {disk}: ${{dg}} GiB\"; \
               if [ \"$dg\" -lt 8 ]; then echo '!!! Warning: target disk under 8 GiB - may be too small for a full install.'; fi; \
             fi; \
             if [ \"$FAIL\" = 1 ]; then {no_net}; exit 1; fi; \
             echo '>>> Pre-flight checks passed.'; \
             true",
            disk = c.disk,
            no_net = no_net
        );
        plan.push(act("sh", &["-c", &preflight]));
    }

    // 0) Host tooling. The installer shells out to partitioning/install tools
    //    that may NOT be present when it's run from a STOCK Artix ISO (or a
    //    minimal environment) instead of this distro's own live image, where
    //    they're baked in. Without them the later steps fail with "command not
    //    found" (e.g. sgdisk) or "command not found: basestrap". Install them
    //    up front so every later step has what it needs:
    //      • artools     — basestrap, fstabgen, artix-chroot (the install core)
    //      • gptfdisk    — sgdisk (GPT partitioning)
    //      • parted      — partprobe (re-read the partition table)
    //      • dosfstools  — mkfs.fat (the EFI system partition)
    //      • e2fsprogs   — mkfs.ext4 (root/boot when ext4; usually present)
    //      • btrfs-progs — mkfs.btrfs (only when the root fs is btrfs)
    //      • cryptsetup  — LUKS open/format/addkey (only when encrypting)
    //    --needed makes this a no-op on this distro's own ISO (already there);
    //    -Sy refreshes first so a stale live db doesn't cause "target not found".
    // 0a) Optionally rebuild the pacman mirrorlists BEFORE anything downloads:
    //     regional mirrors (seeded from the timezone) active and speed-ranked on
    //     top, the full list kept commented below for relocation, unwanted
    //     mirrors filtered out. We write MIRROR_OPTIMIZE_SCRIPT to the live system
    //     and run it with the region's country names as positional args. Doing
    //     it up front means host-tools and every later pacman call use the fast
    //     mirrors. Best-effort throughout (see the script).
    if c.optimize_mirrors {
        let countries = mirror_region_countries(&c.timezone);
        // Quoted heredoc keeps the script's $@/$1/`cmd`/$() literal on write.
        let write_cmd = format!(
            "cat > /tmp/optmirrors.sh <<'MIRROPT_EOF'\n{}\nMIRROPT_EOF",
            MIRROR_OPTIMIZE_SCRIPT
        );
        plan.push(act("sh", &["-c", &write_cmd]));
        // Country names may contain spaces ("United States"), so pass each as a
        // separate argv entry rather than one space-split string.
        let mut argv: Vec<&str> = vec!["/tmp/optmirrors.sh"];
        argv.extend(countries.iter().copied());
        plan.push(act("sh", &argv));
    }

    let mut host_tools: Vec<&str> =
        vec!["artools", "gptfdisk", "parted", "dosfstools", "e2fsprogs"];
    if c.root_fs == "btrfs" {
        host_tools.push("btrfs-progs");
    }
    // The live environment needs the mkfs tool for every additional disk the
    // user formats too (e.g. a btrfs/xfs/f2fs /home on a second disk).
    for d in &c.extra_disks {
        if !d.format || d.mountpoint.is_empty() {
            continue;
        }
        let t = match d.fs.as_str() {
            "btrfs" => "btrfs-progs",
            "xfs" => "xfsprogs",
            "f2fs" => "f2fs-tools",
            "jfs" => "jfsutils",
            _ => continue,
        };
        if !host_tools.contains(&t) {
            host_tools.push(t);
        }
    }
    if c.encrypt_disk || c.extra_disks.iter().any(|d| d.format && d.encrypt && !d.mountpoint.is_empty()) {
        host_tools.push("cryptsetup");
    }
    let host_tools_cmd = format!(
        "pacman -Sy --needed --noconfirm {}",
        host_tools.join(" ")
    );
    plan.push(act("sh", &["-c", &host_tools_cmd]));

    // 1) Disk: partition, format, mount.
    plan.extend(disk::build_plan(
        &c.disk,
        uefi,
        c.swap_gib,
        &c.root_fs,
        c.encrypt_disk,
        luks_pass,
        &c.encrypt_scope,
        c.btrfs_subvolumes,
        c.btrfs_compress,
        c.btrfs_discard,
        c.mount_noatime,
        c.extra_disks.iter().any(|d| d.mountpoint == "/home"),
    ));
    // Additional whole disks the user chose to format (e.g. a separate /home or
    // a storage disk): partition, format, and mount them now so they're part of
    // /mnt before basestrap and fstabgen records them. Existing partitions to
    // mount as-is (NTFS Windows, etc.) are handled later, after fstabgen.
    plan.extend(extra_disks_plan(c));

    // 1b) Prepare the LIVE pacman before basestrap. Some chosen packages live in
    //     the [lib32] (multilib) repo — steam, lib32 GPU libraries — which is
    //     disabled by default in the live image, so basestrap can't find them
    //     ("target not found"). Uncomment exactly the [lib32] block (NOT the
    //     [lib32-gremlins] testing repo) in the LIVE /etc/pacman.conf, then
    //     refresh all databases so basestrap downloads complete, current lists.
    //     The awk matches the literal "#[lib32]" header and the single "#Include"
    //     line that follows it, leaving every other repo untouched. Idempotent:
    //     if [lib32] is already enabled, nothing changes.
    plan.push(act(
        "sh",
        &[
            "-c",
            "awk '/^#\\[lib32\\]$/{print \"[lib32]\"; getline; sub(/^#/,\"\"); print; next} {print}' \
             /etc/pacman.conf > /tmp/pacman.conf.new && cp /tmp/pacman.conf.new /etc/pacman.conf",
        ],
    ));
    plan.push(act("sh", &["-c", "pacman -Sy --noconfirm"]));

    // 1c) Ban XLibre in the LIVE pacman.conf. Artix ships an `xlibre` package
    //     group (the XLibre X-server fork + its xf86 driver replacements);
    //     xlibre-xserver *provides* xorg-server, so without this pacman could
    //     offer XLibre as a provider. We block it two ways for full coverage:
    //       IgnorePkg = xlibre-*   (glob — every xlibre-* package, incl. the
    //                               -beta/-devel variants that aren't in the group)
    //       IgnoreGroup = xlibre   (the whole group, incl. any member that might
    //                               not match the xlibre-* prefix)
    //     This keeps the live/basestrap pass on genuine X.Org. Idempotent: only
    //     inserted if xlibre isn't already mentioned.
    plan.push(act(
        "sh",
        &[
            "-c",
            "grep -q 'xlibre' /etc/pacman.conf || \
             { awk '/^\\[options\\]/{print; print \"IgnorePkg = xlibre-*\"; print \"IgnoreGroup = xlibre\"; next} {print}' \
             /etc/pacman.conf > /tmp/pacman.conf.xl && cp /tmp/pacman.conf.xl /etc/pacman.conf && rm -f /tmp/pacman.conf.xl; }",
        ],
    ));

    // 2) basestrap base + chosen packages.
    //    basestrap's own option parsing (see artools source) is the key detail:
    //      - flags must come BEFORE the root argument; anything after /mnt is
    //        treated as a package target.
    //      - it does NOT accept --noconfirm directly; instead the `-i` flag makes
    //        basestrap append --noconfirm to its pacman call (the flag's help
    //        text reads "avoid auto-confirmation", but the code does the
    //        opposite: `${interactive} && pacman_args+=(--noconfirm)`).
    //      - `-C <config>` selects the pacman.conf to use.
    //    So: `yes | basestrap -i -C /etc/pacman.conf /mnt <pkgs>` runs
    //    non-interactively. `-i` makes basestrap pass --noconfirm to pacman
    //    (auto-taking provider/group-member defaults), and piping `yes` answers
    //    pacman's final "Proceed with installation? [Y/n]" prompt — which we
    let pkgs = base_packages(c);
    let de = desktop_from(&c.desktop);
    // Phase 1: basestrap installs the minimal bootable BASE only (kernel,
    // firmware, dinit + service packages, audio stack, grub, fonts, arch
    // support). basestrap always forces --noconfirm internally, but that's fine
    // here: none of the base packages have provider choices. The desktop, GPU
    // driver / vulkan stack, and the user's extra packages — which DO have
    // provider choices — are installed in phase 2 below, interactively.
    let pkg_args = pkgs.join(" ");
    let basestrap_cmd = format!("basestrap -C /etc/pacman.conf /mnt {pkg_args}");
    plan.push(act("sh", &["-c", &basestrap_cmd]));

    // 2b) Ban XLibre on the TARGET. basestrap has just created
    //     /mnt/etc/pacman.conf; block the `xlibre` group + every xlibre-*
    //     package there. This does double duty: during phase 2 (interactive
    //     pacman runs inside the chroot and reads THIS file) it stops XLibre
    //     from being offered as a provider for xorg-server / the xf86 driver ABI
    //     — so there's no xorg-vs-xlibre prompt — and on the finished system it
    //     keeps XLibre out of every future upgrade. Two directives for full
    //     coverage (group + xlibre-* glob, catching -beta/-devel/AUR builds).
    //     Idempotent.
    plan.push(chroot(
        "grep -q 'xlibre' /etc/pacman.conf || \
         { awk '/^\\[options\\]/{print; print \"IgnorePkg = xlibre-*\"; print \"IgnoreGroup = xlibre\"; next} {print}' \
         /etc/pacman.conf > /tmp/pc.xl && cp /tmp/pc.xl /etc/pacman.conf && rm -f /tmp/pc.xl; }",
    ));

    // 3) fstab.
    plan.push(act("sh", &["-c", "fstabgen -U /mnt >> /mnt/etc/fstab"]));

    // Existing partitions the user chose to mount as-is (e.g. an NTFS Windows
    // volume to dual-boot with): add an fstab entry by UUID and create the
    // mountpoint. The partition is NOT mounted or touched during install — its
    // data is preserved; it mounts on first boot. NTFS uses ntfs-3g with the
    // first user (uid 1000) as owner; `nofail` keeps boot going if it's absent.
    for d in &c.extra_disks {
        if d.format || d.mountpoint.is_empty() {
            continue;
        }
        let (fstype, opts) = if d.fs.eq_ignore_ascii_case("ntfs") {
            ("ntfs-3g", "rw,uid=1000,gid=1000,umask=022,windows_names,big_writes,nofail")
        } else {
            (d.fs.as_str(), "defaults,nofail")
        };
        let mp = resolve_mp(c, &d.mountpoint);
        plan.push(act("mkdir", &["-p", &format!("/mnt{}", mp)]));
        plan.push(act(
            "sh",
            &[
                "-c",
                &format!(
                    "uuid=$(blkid -s UUID -o value {dev}); \
                     [ -n \"$uuid\" ] && printf 'UUID=%s  %s  %s  %s  0 0\\n' \
                     \"$uuid\" '{mp}' '{fstype}' '{opts}' >> /mnt/etc/fstab",
                    dev = d.disk, mp = mp, fstype = fstype, opts = opts
                ),
            ],
        ));
    }

    // Encrypted extra disks (format + encrypt): the partition was LUKS-formatted
    // and opened in extra_disks_plan, and the mapper is currently mounted (so
    // fstabgen captured the *filesystem* UUID at the target mountpoint). At boot
    // the mapper won't exist until it's unlocked, so we (1) drop that fstab line,
    // (2) install the keyfile onto the (encrypted) root, and (3) add a dinit
    // service that unlocks the LUKS device with the keyfile and mounts it. The
    // keyfile lives on root, so it's only safe when root itself is encrypted —
    // the storage screen warns otherwise.
    for d in &c.extra_disks {
        if !d.format || !d.encrypt || d.mountpoint.is_empty() {
            continue;
        }
        let mp = resolve_mp(c, &d.mountpoint);
        let mapper = crypt_mapper(&mp);
        // The LUKS container lives on the partition we created (whole disk) or on
        // the existing partition we reformatted in place.
        let p1 = if d.whole_disk {
            disk::part(&d.disk, 1)
        } else {
            d.disk.clone()
        };
        let svc = format!("crypt-{}", mapper.trim_start_matches("crypt_"));
        // (1) remove the auto-captured fstab line for this mountpoint.
        plan.push(act(
            "sh",
            &["-c", &format!("sed -i '\\|[[:space:]]{mp}[[:space:]]|d' /mnt/etc/fstab", mp = mp)],
        ));
        // (2) copy the keyfile onto the target root, locked down.
        plan.push(act(
            "sh",
            &["-c", &format!(
                "mkdir -p /mnt/etc/cryptsetup-keys.d && chmod 700 /mnt/etc/cryptsetup-keys.d && \
                 cp /tmp/{mapper}.key /mnt/etc/cryptsetup-keys.d/{mapper}.key && \
                 chmod 600 /mnt/etc/cryptsetup-keys.d/{mapper}.key"
            )],
        ));
        // (3) write a scripted dinit service that unlocks + mounts at boot, and
        //     enable it by linking into boot.d. The LUKS UUID is resolved now.
        let mut mopts: Vec<&str> = Vec::new();
        if d.noatime {
            mopts.push("noatime");
        }
        if d.compress && d.fs == "btrfs" {
            mopts.push("compress=zstd");
        }
        let mount_flag = if mopts.is_empty() {
            String::new()
        } else {
            format!("-o {} ", mopts.join(","))
        };
        // (3) Write the unlock+mount logic as a small SCRIPT FILE, and point a
        //     scripted service at it. This avoids dinit's command-line parser,
        //     which only understands double quotes — a single-quoted `/bin/sh
        //     -c '…'` was being split on spaces, so the auto-unlock silently
        //     failed. A bare path needs no quoting. The script bakes in the LUKS
        //     UUID (resolved now) and waits for the device to appear, since udev
        //     may still be settling when boot.d services run.
        plan.push(act(
            "sh",
            &["-c", &format!(
                "mkdir -p /mnt/etc/dinit.d/scripts && \
                 luuid=$(cryptsetup luksUUID {p1}) && \
                 printf '%s\\n' \
                   '#!/bin/sh' \
                   \"luuid=$luuid\" \
                   'for i in 1 2 3 4 5 6 7 8 9 10; do [ -e /dev/disk/by-uuid/$luuid ] && break; udevadm settle 2>/dev/null || sleep 1; done' \
                   'cryptsetup open --key-file /etc/cryptsetup-keys.d/{mapper}.key UUID=$luuid {mapper}' \
                   'mkdir -p {mp}' \
                   'mount {mount_flag}/dev/mapper/{mapper} {mp}' \
                   > /mnt/etc/dinit.d/scripts/{svc} && \
                 chmod +x /mnt/etc/dinit.d/scripts/{svc} && \
                 printf '%s\\n' \
                   '#!/bin/sh' \
                   'umount {mp} 2>/dev/null' \
                   'cryptsetup close {mapper} 2>/dev/null' \
                   > /mnt/etc/dinit.d/scripts/{svc}-stop && \
                 chmod +x /mnt/etc/dinit.d/scripts/{svc}-stop && \
                 printf '%s\\n' \
                   'type = scripted' \
                   'command = /etc/dinit.d/scripts/{svc}' \
                   'stop-command = /etc/dinit.d/scripts/{svc}-stop' \
                   'restart = false' \
                   > /mnt/etc/dinit.d/{svc}",
                p1 = p1, svc = svc, mapper = mapper, mp = mp,
                mount_flag = mount_flag
            )],
        ));
        plan.push(chroot(&format!(
            "mkdir -p /etc/dinit.d/boot.d && ln -sf ../{svc} /etc/dinit.d/boot.d/{svc}"
        )));
    }

    // 4) Locale.
    plan.push(chroot(&format!(
        "echo '{} UTF-8' >> /etc/locale.gen && locale-gen && echo 'LANG={}' > /etc/locale.conf",
        c.locale, c.locale
    )));

    // 5) Timezone.
    plan.push(chroot(&format!(
        "ln -sf /usr/share/zoneinfo/{} /etc/localtime && hwclock --systohc",
        c.timezone
    )));

    // 6) Console keymap + a Unicode-capable console font. We ship terminus-font
    //    (provides ter-*) in the base set; ter-116n covers Cyrillic/Latin and
    //    exists in that package, unlike the previously-used (nonexistent)
    //    ter-118n. Falls back gracefully if the font is missing.
    //    For Ukrainian-interface installs the console keymap is `ua-utf` from
    //    kbd: verified against its source, the PLAIN layer is Latin (so the
    //    initramfs LUKS prompt and commands type ASCII as usual) and Cyrillic
    //    sits on a locked group toggled by Right Ctrl / Right Alt
    //    (CtrlL_Lock/CtrlR_Lock on keycodes 97/100) — actual Ukrainian typing
    //    on the TTY instead of Latin-only, with zero passphrase risk.
    let console_keymap = if c.lang == "uk" { "ua-utf" } else { c.keymap.as_str() };
    plan.push(chroot(&format!(
        "printf 'KEYMAP={}\\nFONT=ter-116n\\n' > /etc/vconsole.conf",
        console_keymap
    )));

    // 7) Hostname + hosts file. The Artix install guide requires /etc/hosts to
    //     carry the loopback entries AND a 127.0.1.1 line for the machine's own
    //     name, so programs that resolve the local hostname don't stall or fail.
    //     We keep the hostname and the hosts file in sync.
    let host = if c.hostname.trim().is_empty() { "artix" } else { c.hostname.trim() };
    plan.push(chroot(&format!("echo '{host}' > /etc/hostname")));

    // Repair /etc/os-release if PRETTY_NAME is missing or still a literal
    // template. The live image's getty showed "Artix Linux{PRETTY_NAME}",
    // which means agetty's \\S escape in /etc/issue expanded an os-release
    // whose PRETTY_NAME line was malformed (a packaging template that never
    // got substituted). agetty reads PRETTY_NAME for \\S, so a broken value
    // shows up at every text login. We rewrite the line to a sane value
    // (idempotent: only touches PRETTY_NAME, leaving NAME/ID/etc. intact; if
    // the file is absent we create a minimal valid one).
    plan.push(chroot(
        "f=/etc/os-release; \
         if [ -f \"$f\" ]; then \
           if ! grep -qE '^PRETTY_NAME=\"[^\"{}]+\"$' \"$f\"; then \
             sed -i '/^PRETTY_NAME=/d' \"$f\"; \
             printf 'PRETTY_NAME=\"Artix Linux\"\\n' >> \"$f\"; \
           fi; \
         else \
           printf 'NAME=\"Artix Linux\"\\nPRETTY_NAME=\"Artix Linux\"\\nID=artix\\n' > \"$f\"; \
         fi",
    ));
    plan.push(write_target_file(
        "/mnt/etc/hosts",
        &format!(
            "# Static table lookup for hostnames.\n# See hosts(5) for details.\n\n127.0.0.1\tlocalhost\n::1\t\tlocalhost\n127.0.1.1\t{host}.localdomain\t{host}\n"
        ),
    ));

    // 8) Enable the [lib32] repo in the installed system's pacman.conf (same
    //     robust awk as the live prep at step 1b; leaves [lib32-gremlins] alone).
    plan.push(chroot(
        "awk '/^#\\[lib32\\]$/{print \"[lib32]\"; getline; sub(/^#/,\"\"); print; next} {print}' \
         /etc/pacman.conf > /tmp/pacman.conf.new && cp /tmp/pacman.conf.new /etc/pacman.conf",
    ));

    // 8a) If we ranked mirrors on the live system, copy the optimized lists to
    //     the target so the INSTALLED system also benefits (faster updates
    //     later), not just the install itself. /mnt/etc/pacman.d already exists
    //     from basestrap. Best-effort.
    if c.optimize_mirrors {
        plan.push(act(
            "sh",
            &[
                "-c",
                "echo '>>> Copying ranked mirrors to the target system...'; \
                 cp /etc/pacman.d/mirrorlist /mnt/etc/pacman.d/mirrorlist 2>/dev/null || true; \
                 [ -f /etc/pacman.d/mirrorlist-arch ] && cp /etc/pacman.d/mirrorlist-arch /mnt/etc/pacman.d/mirrorlist-arch 2>/dev/null; \
                 true",
            ],
        ));
    }

    // 8b) Enable the Arch repositories so packages not in Artix can be installed
    //     later. Appended to the END of pacman.conf so they sit BELOW all Artix
    //     repos. This ordering is pacman's ONLY priority mechanism: for a
    //     package that exists in both, pacman picks the one from the repo listed
    //     FIRST — i.e. Artix. That matters because Artix often ships its own
    //     builds with fixes for init-systems without systemd, and we want those
    //     to win over the Arch version. The base system itself is installed
    //     (step 2) before this runs, so it comes entirely from Artix repos.
    //     [community] no longer exists in Arch (merged into extra). The header
    //     comment is written into the file so it's self-explanatory on inspect.
    //     Guarded so we don't append twice.
    plan.push(chroot(
        "grep -q '^\\[extra\\]' /etc/pacman.conf || printf '\\n# --- Arch repositories (kept BELOW Artix so Artix builds take priority) ---\\n[extra]\\nInclude = /etc/pacman.d/mirrorlist-arch\\n[multilib]\\nInclude = /etc/pacman.d/mirrorlist-arch\\n' >> /etc/pacman.conf",
    ));
    plan.push(chroot("pacman-key --init"));
    plan.push(chroot("pacman-key --populate archlinux artix"));
    // Refresh databases so the newly-enabled Arch repos are immediately usable
    // in the installed system (|| true: don't fail the install if a mirror is
    // momentarily unreachable — the user can re-sync later).
    plan.push(chroot("pacman -Sy --noconfirm || true"));

    // Chaotic-AUR (optional, user-toggled on the Options screen): a binary
    // repository of prebuilt AUR packages maintained by the Garuda Linux team.
    // Enabling it BEFORE the package phases means pacman/paru can pull popular
    // AUR software as ready-made binaries instead of compiling from source.
    // Setup follows the official procedure: import + locally-sign the primary
    // key (so the keyring/mirrorlist packages verify), install chaotic-keyring
    // and chaotic-mirrorlist from the CDN, append the [chaotic-aur] section to
    // pacman.conf (idempotent — only if absent), then refresh the databases.
    // The whole block is best-effort: a trailing `true` keeps a transient
    // network/keyserver failure from aborting the install (the user can redo it
    // later), and the append is guarded so a re-run never duplicates the stanza.
    if c.chaotic_aur {
        plan.push(chroot(
            // GUARD 1 — reachability gate. A quick HEAD with a hard
            // connect-timeout: if the Chaotic CDN doesn't answer within ~15s,
            // SKIP the entire setup with a clear log line and continue the
            // install (paru just builds from the AUR as usual). Chaotic's
            // mirrors are frequently overloaded, and pacman's built-in
            // downloader can otherwise block silently for many minutes.
            //
            // GUARD 2 — download timeout. Before adding the repo we set an
            // XferCommand (curl with --connect-timeout + --retry) in
            // pacman.conf. This makes EVERY subsequent download — the
            // chaotic-aur DB and any package pulled from it during the later AUR
            // phase — time out fast on a dead/slow mirror so pacman moves on or
            // fails loudly, instead of hanging with no output. It stays on the
            // installed system too, where Chaotic mirrors remain flaky.
            //
            // Every step echoes progress so the log never goes silent, and the
            // chain is best-effort (trailing `true`) so nothing here aborts the
            // install; a partial failure just leaves Chaotic disabled.
            "echo '>>> Chaotic-AUR: checking server reachability (15s timeout)...'; \
             if ! curl -fsS --connect-timeout 15 --max-time 40 -I \
                 'https://cdn-mirror.chaotic.cx/chaotic-aur/chaotic-mirrorlist.pkg.tar.zst' \
                 >/dev/null 2>&1; then \
               echo '!!! Chaotic-AUR server UNREACHABLE — skipping it. Install continues; AUR packages build from source as normal.'; \
             else \
               echo '>>> Reachable. Setting a pacman download timeout so a slow mirror cannot hang the install...'; \
               { grep -q '^XferCommand' /etc/pacman.conf || \
                 sed -i '/^\\[options\\]/a XferCommand = /usr/bin/curl --connect-timeout 15 --retry 2 --retry-delay 2 -L -C - -f --no-progress-meter -o %o %u' /etc/pacman.conf; }; \
               echo '>>> Importing Chaotic-AUR signing key...'; \
               if pacman-key --recv-key 3056513887B78AEB --keyserver keyserver.ubuntu.com && \
                  pacman-key --lsign-key 3056513887B78AEB && \
                  echo '>>> Installing chaotic-keyring + chaotic-mirrorlist...' && \
                  pacman -U --noconfirm \
                    'https://cdn-mirror.chaotic.cx/chaotic-aur/chaotic-keyring.pkg.tar.zst' \
                    'https://cdn-mirror.chaotic.cx/chaotic-aur/chaotic-mirrorlist.pkg.tar.zst' && \
                  { grep -q '^\\[chaotic-aur\\]' /etc/pacman.conf || \
                    printf '\\n[chaotic-aur]\\nInclude = /etc/pacman.d/chaotic-mirrorlist\\n' \
                    >> /etc/pacman.conf; } && \
                  echo '>>> Syncing package databases...' && \
                  pacman -Sy --noconfirm; then \
                 echo '>>> Chaotic-AUR enabled successfully.'; \
               else \
                 echo '!!! Chaotic-AUR setup failed partway — install continues without it.'; \
               fi; \
             fi; \
             true",
        ));
        // Apply the same mirror treatment to the chaotic-mirrorlist (region on
        // top, unwanted mirrors filtered out), inside the chroot where it lives.
        //
        // IMPORTANT: artix-chroot (like arch-chroot) runs each invocation in its
        // own unshare namespace with a FRESH tmpfs on /tmp, so a file written by
        // one chroot call is GONE in the next. We therefore WRITE the script and
        // RUN it in a SINGLE chroot call, so both see the same /tmp. Best-effort.
        if c.optimize_mirrors {
            let countries = mirror_region_countries(&c.timezone);
            // Country names may contain spaces; single-quote each for the shell.
            let mut args = String::new();
            for c in countries {
                args.push_str(" '");
                args.push_str(c);
                args.push('\'');
            }
            let combined = format!(
                "cat > /tmp/optmirrors.sh <<'MIRROPT_EOF'\n{}\nMIRROPT_EOF\n\
                 sh /tmp/optmirrors.sh --chaotic{}",
                MIRROR_OPTIMIZE_SCRIPT, args
            );
            plan.push(chroot(&combined));
        }
    }

    // Optimize the Arch mirrorlist (extra/multilib) the same way — region on
    // top with per-country headers, unwanted mirrors filtered out — in the chroot, where
    // /etc/pacman.d/mirrorlist-arch exists (it's absent on the live ISO, so it
    // can't be ranked earlier). Single chroot call (each gets a fresh /tmp).
    // Runs before Phase 2 so those package downloads use the better mirrors.
    if c.optimize_mirrors {
        let countries = mirror_region_countries(&c.timezone);
        let mut args = String::new();
        for c in countries {
            args.push_str(" '");
            args.push_str(c);
            args.push('\'');
        }
        let combined = format!(
            "cat > /tmp/optmirrors.sh <<'MIRROPT_EOF'\n{}\nMIRROPT_EOF\n\
             sh /tmp/optmirrors.sh --arch{}",
            MIRROR_OPTIMIZE_SCRIPT, args
        );
        plan.push(chroot(&combined));
    }

    // ─── Phase 2: interactive system packages ───────────────────────────────
    // Everything beyond the minimal base — desktop, GPU/vulkan stack, the
    // user's extra packages, display manager, seat backend — is installed here
    // with `pacman -S` INSIDE the chroot, run under a PTY (chroot_interactive)
    // WITHOUT --noconfirm. That's the whole point: pacman shows provider choices
    // (vulkan-driver, qt6-multimedia-backend, jack, …) and the user picks the
    // right one for their hardware in the TUI, instead of basestrap silently
    // taking provider #1 (which previously chose an incompatible nvidia driver
    // and aborted). "Proceed? [Y/n]" prompts are auto-answered Y by the runner.
    //
    // The X server is installed FIRST, as explicitly-named targets, so the
    // x11win-server virtual dep (pulled by SDDM and the desktops) resolves to
    // genuine X.Org and never to XLibre (whose ABI conflicts with xf86 drivers).
    if de.needs_xorg(&c.session) {
        let mut xpkgs: Vec<&str> = Vec::new();
        if de.wayland_session(&c.session) {
            xpkgs.push("xorg-server");
            xpkgs.push("xorg-server-common");
            xpkgs.push("xorg-xwayland");
        } else {
            xpkgs.extend(XORG_PACKAGES.iter().copied());
            xpkgs.extend([
                "xf86-input-libinput",
                "xf86-input-evdev",
                "xf86-input-wacom",
                "xf86-video-vesa",
                "xf86-video-fbdev",
                "xf86-video-amdgpu",
                "xf86-video-ati",
                "xf86-video-nouveau",
                "xf86-video-qxl",
            ]);
        }
        let xargs = xpkgs.join(" ");
        // --needed so already-present packages aren't reinstalled. No
        // --noconfirm: if the X stack ever has a provider choice the user
        // decides; in practice these named targets resolve cleanly.
        plan.push(chroot_interactive(&format!("pacman -S --needed {xargs}")));
    }
    // The rest of the system: desktop, GPU, extras, DM, seat. Interactive.
    let sys_pkgs = system_packages(c);
    if !sys_pkgs.is_empty() {
        let sys_args = sys_pkgs.join(" ");
        plan.push(chroot_interactive(&format!("pacman -S --needed {sys_args}")));
    }
    // ─────────────────────────────────────────────────────────────────────────

    // 9) Accounts. Four modes (see AccountMode). Passwords are piped to
    //    chpasswd via printf so we avoid interactive prompts; the values come
    //    from the wizard. We keep each step discrete for clear logging.
    let m = match c.account_mode.as_str() {
        "UserSameRoot" => crate::app::AccountMode::UserSameRoot,
        "UserSudoOnly" => crate::app::AccountMode::UserSudoOnly,
        "RootOnly" => crate::app::AccountMode::RootOnly,
        _ => crate::app::AccountMode::UserSeparateRoot,
    };

    if m.needs_user() {
        // Pick the login shell from the chosen extras: zsh or fish if selected,
        // otherwise bash. The shell package itself is in extra_packages already.
        let login_shell = if c.extra_packages.iter().any(|x| x == "zsh") {
            "/bin/zsh"
        } else if c.extra_packages.iter().any(|x| x == "fish") {
            "/usr/bin/fish"
        } else {
            "/bin/bash"
        };
        plan.push(chroot(&format!(
            "useradd -m -G wheel,audio,video,storage,network,input -s {} {}",
            login_shell, c.username
        )));
        // A home-based custom mount ("~/name") makes /home/<user> pre-exist before
        // useradd runs, so `useradd -m` skips the skel copy. Restore it (no-clobber)
        // and fix ownership so the user still gets the default dotfiles.
        if c.extra_disks.iter().any(|d| d.mountpoint.starts_with("~/")) {
            plan.push(chroot(&format!(
                "cp -rn /etc/skel/. /home/{user}/ 2>/dev/null || true; chown -R {user}:{user} /home/{user}",
                user = c.username
            )));
        }
        // chpasswd reads "user:password" from stdin — no shell quoting of the
        // password is needed, which avoids breakage on special characters.
        plan.push(chroot(&format!(
            "printf '%s:%s\\n' {} \"{}\" | chpasswd",
            c.username,
            shell_escape_dq(&c.user_password)
        )));
        // wheel → sudo. With or without a password per the user's choice on the
        // options step. Passwordless uses a NOPASSWD drop-in; otherwise the
        // standard password-required wheel rule is enabled.
        if c.passwordless_sudo {
            plan.push(chroot(
                "echo '%wheel ALL=(ALL:ALL) NOPASSWD: ALL' > /etc/sudoers.d/10-wheel-nopasswd; chmod 440 /etc/sudoers.d/10-wheel-nopasswd",
            ));
        } else {
            plan.push(chroot(
                "sed -i 's/^# *%wheel ALL=(ALL:ALL) ALL/%wheel ALL=(ALL:ALL) ALL/' /etc/sudoers",
            ));
        }
        // With seatd the user must be in the `seat` group so a compositor (or
        // anything using libseat) can acquire the seat. Done for ANY session —
        // the user may pick an X11 DE now and run a Wayland compositor later,
        // and the group costs nothing. seatd's package creates the group; we
        // add the user with gpasswd (|| true so it's harmless if absent).
        if c.seat_provider == "seatd" {
            plan.push(chroot(&format!(
                "gpasswd -a {} seat 2>/dev/null || true",
                c.username
            )));
        }

        // X11 + seatd: vanilla Xorg has NO libseat/seatd backend — it can only
        // run rootless via (e)logind, and the seatd path deliberately doesn't
        // wire up a logind session (it uses pam_turnstile, not pam_elogind). So
        // for an X11 session under seatd we make Xorg run ROOTFUL through its
        // setuid wrapper by writing /etc/X11/Xwrapper.config with
        // needs_root_rights=yes. As root, Xorg opens KMS + input devices
        // directly with no logind session needed, while seatd still manages the
        // seat for the display manager — the classic pre-logind way X is run on
        // elogind-free systems. Only written for an actual X11 session under
        // seatd; Wayland, or the elogind path, go rootless via libseat/logind
        // and must NOT get this file.
        let de_x = desktop_from(&c.desktop);
        if c.seat_provider == "seatd"
            && de_x.needs_xorg(&c.session)
            && !de_x.wayland_session(&c.session)
        {
            plan.push(write_target_file(
                "/mnt/etc/X11/Xwrapper.config",
                "# Managed by the Artix installer: X11 + seatd (no elogind\n\
                 # session), so Xorg runs rootful and reaches KMS/input without\n\
                 # logind. seatd still manages the seat for the display manager.\n\
                 needs_root_rights = yes\n\
                 allowed_users = anybody\n",
            ));
        }

        // Raise the open-file-descriptor limit for the user. Wine/Proton's
        // fsync (and esync) open thousands of eventfd/file descriptors, and
        // running several game launchers in the background multiplies that —
        // hitting the default 1024 soft limit causes crashes and "too many open
        // files" errors. On systemd this is handled by DefaultLimitNOFILE, but
        // Artix+dinit has no such mechanism, so the limit MUST be set via PAM's
        // limits.conf. Both hard and soft go to 1048576. Idempotent: only
        // appended if a line for this user isn't already present (so a retried
        // install doesn't duplicate it). The stock file ends with a
        // "# End of file" marker; appending after it is fine.
        plan.push(chroot(&format!(
            "f=/etc/security/limits.conf; \
             if ! grep -qE '^{user}[[:space:]]+(hard|soft)[[:space:]]+nofile' \"$f\" 2>/dev/null; then \
             printf '%s hard nofile 1048576\\n%s soft nofile 1048576\\n' {user} {user} >> \"$f\"; \
             echo 'raised nofile limit for {user} (Wine/Proton fsync)'; fi",
            user = c.username
        )));

        // Shell configuration. We write the config into the new user's home and
        // chown it to them afterwards. The starship prompt config is generated
        // directly from starship's built-in `pastel-powerline` preset (run as
        // the user, so paths and ownership are correct) — no ISO asset needed,
        // and it always matches the installed starship version.
        let want_zsh = c.extra_packages.iter().any(|x| x == "zsh");
        let want_fish = c.extra_packages.iter().any(|x| x == "fish");
        let want_kitty = c.extra_packages.iter().any(|x| x == "kitty");
        let home = format!("/home/{}", c.username);

        if want_zsh || want_fish {
            // Generate ~/.config/starship.toml from the built-in preset, as the
            // user. `mkdir -p ~/.config` first so the output path exists.
            plan.push(chroot(&format!(
                "su - {user} -c 'mkdir -p ~/.config && starship preset pastel-powerline -o ~/.config/starship.toml' || true",
                user = c.username
            )));
        }

        if want_zsh {
            // A clean .zshrc that wires up the repo plugins (same features as a
            // typical oh-my-zsh setup: completions, autosuggestions, syntax
            // highlighting, history substring search, sensible history + keys),
            // then starts starship. Paths are the Arch/Artix package locations.
            let zshrc = ZSHRC_TEMPLATE;
            plan.push(write_home_file(&home, ".zshrc", zshrc));
            // Pre-create an empty history file so zsh doesn't warn about a
            // missing/corrupt history on first launch.
            plan.push(chroot(&format!("touch {home}/.zsh_history")));
        }

        if want_fish {
            // fish config: sane defaults, abbreviations, and starship init.
            let fishcfg = FISH_CONFIG_TEMPLATE;
            plan.push(chroot(&format!("mkdir -p {home}/.config/fish")));
            plan.push(write_home_file(
                &home,
                ".config/fish/config.fish",
                fishcfg,
            ));
        }

        if want_kitty {
            // Write the embedded kitty config (Catppuccin Mocha theme) into
            // ~/.config/kitty/kitty.conf. Embedded in the binary so it never
            // depends on an ISO asset being present.
            plan.push(chroot(&format!("mkdir -p {home}/.config/kitty")));
            plan.push(write_home_file(
                &home,
                ".config/kitty/kitty.conf",
                KITTY_CONFIG_TEMPLATE,
            ));
        }

        // fastfetch config + logo → ~/.config/fastfetch. fastfetch is now a
        // base package (installed with any DE/WM or none), so the config is
        // always written. The logo is a PNG (binary), written via base64; the
        // config is text whose logo `source` we rewrite to the user's ABSOLUTE
        // home path here. fastfetch only expands ~ on v2.41.0+ and $HOME via
        // wordexp, so a literal absolute path is the one form that always
        // resolves regardless of fastfetch version or launch context (e.g. from
        // .zshrc). Both files land in ~/.config/fastfetch; embedded in the
        // installer binary, no ISO dependency.
        plan.push(chroot(&format!("mkdir -p {home}/.config/fastfetch")));
        let fastfetch_config = FASTFETCH_CONFIG
            .replace("$HOME/.config/fastfetch/fastfetch.png", &format!("{home}/.config/fastfetch/fastfetch.png"));
        plan.push(write_home_file(
            &home,
            ".config/fastfetch/config.jsonc",
            &fastfetch_config,
        ));
        plan.extend(write_home_binary(
            &home,
            ".config/fastfetch/fastfetch.png",
            FASTFETCH_LOGO_PNG,
        ));

        // wofi config + stylesheet → ~/.config/wofi. Written when wofi is a
        // selected package OR when Pinnacle is the desktop (its config binds the
        // launcher to wofi, so it must be configured for the desktop to work).
        let pinnacle_desktop = matches!(de, Desktop::Pinnacle);
        let want_wofi = pinnacle_desktop || c.extra_packages.iter().any(|x| x == "wofi");
        if want_wofi {
            plan.push(chroot(&format!("mkdir -p {home}/.config/wofi")));
            plan.push(write_home_file(&home, ".config/wofi/config", WOFI_CONFIG));
            plan.push(write_home_file(&home, ".config/wofi/style.css", WOFI_STYLE_CSS));
        }

        // waybar config + stylesheet → ~/.config/waybar. Written when waybar is a
        // selected package OR when Pinnacle is the desktop (Pinnacle ships with
        // waybar as its bar, so the config + theme come along automatically).
        let want_waybar = pinnacle_desktop || c.extra_packages.iter().any(|x| x == "waybar");
        if want_waybar {
            plan.push(chroot(&format!("mkdir -p {home}/.config/waybar")));
            plan.push(write_home_file(&home, ".config/waybar/config.jsonc", WAYBAR_CONFIG));
            plan.push(write_home_file(&home, ".config/waybar/style.css", WAYBAR_STYLE_CSS));
        }

        // pinnacle compositor config → ~/.config/pinnacle, only if pinnacle-comp
        // is among the selected AUR packages. The config tree is shipped as a
        // gzip tarball embedded in the binary; we drop it into the home dir via
        // base64 (it's binary), unpack it, remove the tarball, and mark every
        // script under scripts/ executable. The pinnacle.toml runs `cargo run`,
        // so the Rust config (src/main.rs) is compiled by pinnacle on first
        // start — we ship only the source, never the multi-GB target/ tree.
        // pinnacle config tree → ~/.config/pinnacle when pinnacle-comp will be
        // installed — either because Pinnacle was chosen as the DESKTOP (which
        // injects pinnacle-comp into the AUR list) or picked explicitly in the
        // AUR package screen. effective_aur_packages() covers both.
        let want_pinnacle = effective_aur_packages(c).iter().any(|x| x == "pinnacle-comp");
        if want_pinnacle {
            plan.push(chroot(&format!("mkdir -p {home}/.config/pinnacle")));
            plan.extend(write_home_binary(
                &home,
                ".config/pinnacle/.pinnacle-config.tar.gz",
                PINNACLE_CONFIG_TARBALL,
            ));
            // Unpack into the pinnacle config dir, then delete the tarball.
            plan.push(chroot(&format!(
                "tar -xzf {home}/.config/pinnacle/.pinnacle-config.tar.gz \
                 -C {home}/.config/pinnacle && \
                 rm -f {home}/.config/pinnacle/.pinnacle-config.tar.gz"
            )));
            // Make every script executable (the archive preserves the bit for
            // some, but normalise so launcher.sh / mango-tags.sh are runnable
            // too). Guarded by a dir test so it never fails if scripts/ is
            // absent. find … -exec chmod marks each *.sh file.
            plan.push(chroot(&format!(
                "if [ -d {home}/.config/pinnacle/scripts ]; then \
                 find {home}/.config/pinnacle/scripts -type f -name '*.sh' \
                 -exec chmod +x {{}} +; fi"
            )));
            // A systemd-free Wayland session entry for Pinnacle. The stock
            // pinnacle.desktop (shipped by pinnacle-comp) starts `pinnacle
            // --session` directly; on a system without systemd that comes up
            // with NO D-Bus session bus, so portals/clipboard/etc. misbehave.
            // This extra entry wraps the launch in `dbus-run-session`, which
            // spins up a private session bus for the whole compositor — the
            // reliable way to get one without `systemd --user`. The greeter
            // (SDDM) lists it as "PinnacleFree"; pick it instead of the stock
            // "Pinnacle". Written as a system file (mkdir -p makes the dir if
            // pinnacle-comp hasn't created /usr/share/wayland-sessions yet).
            plan.push(write_target_file(
                "/mnt/usr/share/wayland-sessions/pinnacleFree.desktop",
                "[Desktop Entry]\n\
                 Name=PinnacleFree\n\
                 Comment=A Wayland compositor inspired by AwesomeWM\n\
                 Exec=dbus-run-session pinnacle --session\n\
                 Type=Application\n\
                 DesktopNames=pinnacle\n",
            ));
        }

        // Audio: pipewire / wireplumber / pipewire-pulse are USER services
        // (/etc/dinit.d/user/*), started by the per-user dinit instance at
        // login — NOT system services. They're enabled by symlinking them into
        // the user's ~/.config/dinit.d/boot.d/. The dependency chain is
        // pipewire → wireplumber → pipewire-pulse. We always set this up so the
        // installed system has working sound out of the box.
        plan.push(chroot(&format!("mkdir -p {home}/.config/dinit.d/boot.d")));
        // user-dinit always looks for a service named "boot" on startup and
        // exits if it's missing ("could not find service description") — which
        // is exactly why the per-user instance was dying immediately and audio
        // never started. Create the boot service: type=internal with
        // waits-for.d=boot.d, so it pulls in everything symlinked under boot.d/
        // (pipewire → wireplumber → pipewire-pulse). Without this file userspawn
        // spawns dinit, dinit can't find "boot", and quits.
        plan.push(write_home_file(
            &home,
            ".config/dinit.d/boot",
            "type = internal\nwaits-for.d: ./boot.d/\n",
        ));
        // Symlink the user services into boot.d so they start with the user
        // dinit — AND make them log to an in-memory buffer so `dinitctl catlog
        // <svc>` works (handy for watching e.g. pipewire/wireplumber output in
        // real time). dinit has no global default for this; log-type = buffer
        // must be set per service, which is why catlog works for nothing out of
        // the box. We do it WITHOUT editing the packaged files (those live in
        // /etc/dinit.d/user and get overwritten on upgrade): instead we drop an
        // override copy in ~/.config/dinit.d/<svc> (which takes priority and is
        // never touched by pacman), append log-type = buffer + a 1 MiB buffer
        // if not already present, and point boot.d at the copy. If the packaged
        // service file isn't present (unexpected), fall back to a plain symlink.
        // We only do this for USER services — system services keep logging to
        // syslog-ng so /var/log stays complete; buffer would divert them.
        for svc in ["dbus", "pipewire", "wireplumber", "pipewire-pulse"] {
            plan.push(chroot(&format!(
                "src=/etc/dinit.d/user/{svc}; dst={home}/.config/dinit.d/{svc}; \
                 if [ -f \"$src\" ]; then \
                   [ -f \"$dst\" ] || cp \"$src\" \"$dst\"; \
                   grep -q '^log-type' \"$dst\" || printf 'log-type = buffer\\nlog-buffer-size = 1048576\\n' >> \"$dst\"; \
                   ln -sf \"$dst\" {home}/.config/dinit.d/boot.d/{svc}; \
                 else \
                   ln -sf \"$src\" {home}/.config/dinit.d/boot.d/{svc} 2>/dev/null || true; \
                 fi"
            )));
        }
        // That's all that's needed on the user side: turnstile's Dinit backend
        // runs `dinit --user` for us on login (see the turnstiled.conf + PAM
        // wiring below), and dinit then starts the `boot` service, which pulls
        // in everything symlinked under boot.d/. No userspawnrc is involved —
        // that was for userspawn, which we no longer use (it needs elogind).

        // Make sure everything we just wrote is owned by the user, not root.
        plan.push(chroot(&format!("chown -R {0}:{0} {home}", c.username)));
    }

    // Custom-named mount folders get a GTK bookmark so they show up in the file
    // manager sidebar (Nautilus, Nemo, Thunar, PCManFM, Caja). Run as the user so
    // the bookmarks file is theirs; idempotent (no duplicate lines). Home-based
    // mounts are additionally visible as a folder in the home directory.
    for d in &c.extra_disks {
        if !d.bookmark || d.mountpoint.is_empty() {
            continue;
        }
        let resolved = resolve_mp(c, &d.mountpoint);
        let label = resolved.rsplit('/').next().unwrap_or("disk").to_string();
        plan.push(chroot(&format!(
            "su - {user} -c 'mkdir -p ~/.config/gtk-3.0 && touch ~/.config/gtk-3.0/bookmarks && \
             grep -qxF \"file://{path} {label}\" ~/.config/gtk-3.0/bookmarks || \
             echo \"file://{path} {label}\" >> ~/.config/gtk-3.0/bookmarks' || true",
            user = c.username,
            path = resolved,
            label = label
        )));
    }

    // D-Bus SESSION bus for graphical apps. On a systemd-free system nothing
    // starts a per-user session bus automatically the way `systemd --user`
    // would, so GUI apps launched from a display manager (Spotify, Electron
    // apps, many GTK/Qt programs) come up with no DBUS_SESSION_BUS_ADDRESS and
    // either refuse to start or lose features — exactly the "dbus session"
    // complaint, even though the SYSTEM bus is running. The fix (per the Void
    // Linux handbook, which faces the same no-systemd situation): once
    // XDG_RUNTIME_DIR exists (pam_rundir gives us /run/user/<uid>), point a
    // session bus at $XDG_RUNTIME_DIR/bus and start dbus-daemon there if it's
    // not already up, then export the address into the login shell. A
    // profile.d snippet runs for every login shell (X11, Wayland, or bare
    // TTY) and is idempotent: it only acts when no session bus is set yet.
    // dbus-update-activation-environment then shares the env (DISPLAY,
    // WAYLAND_DISPLAY, XDG_*) with the bus so activated services inherit it.
    plan.push(chroot("mkdir -p /etc/profile.d"));
    plan.push(write_target_file(
        "/mnt/etc/profile.d/dbus-session.sh",
        "# Start a per-user D-Bus *session* bus on systemd-free systems if the\n\
         # login process didn't set one up. Safe to source from any shell.\n\
         if [ -z \"$DBUS_SESSION_BUS_ADDRESS\" ] && [ -n \"$XDG_RUNTIME_DIR\" ]; then\n\
         \tbus=\"$XDG_RUNTIME_DIR/bus\"\n\
         \tif [ ! -S \"$bus\" ]; then\n\
         \t\t# --fork makes dbus-daemon daemonise itself cleanly (no stray\n\
         \t\t# foreground job holding up the login shell).\n\
         \t\tdbus-daemon --session --address=\"unix:path=$bus\" --fork --nopidfile >/dev/null 2>&1 || true\n\
         \tfi\n\
         \texport DBUS_SESSION_BUS_ADDRESS=\"unix:path=$bus\"\n\
         \tcommand -v dbus-update-activation-environment >/dev/null 2>&1 && \\\n\
         \t\tdbus-update-activation-environment DISPLAY WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE >/dev/null 2>&1 || true\n\
         fi\n",
    ));
    // The profile.d snippet covers login shells: a TTY login (the "no display
    // manager" case) and anything that sources /etc/profile. But an X11
    // session started with `startx` runs xinitrc, and the xinitrc.d hook
    // directory is the standard place to inject session-bus setup for that
    // path, so we add the SAME logic there. Together these cover all three DM
    // choices: SDDM and greetd start the session through PAM+their own dbus
    // handling, while "no DM" → TTY login (profile.d) → optional startx
    // (xinitrc.d). Hook is sourced by /etc/X11/xinit/xinitrc, executable.
    plan.push(chroot("mkdir -p /etc/X11/xinit/xinitrc.d"));
    plan.push(write_target_file(
        "/mnt/etc/X11/xinit/xinitrc.d/15-dbus-session.sh",
        "#!/bin/sh\n\
         # Ensure a D-Bus session bus for X11 sessions launched via startx on\n\
         # systemd-free systems (mirrors /etc/profile.d/dbus-session.sh).\n\
         if [ -z \"$DBUS_SESSION_BUS_ADDRESS\" ] && [ -n \"$XDG_RUNTIME_DIR\" ]; then\n\
         \tbus=\"$XDG_RUNTIME_DIR/bus\"\n\
         \t[ -S \"$bus\" ] || dbus-daemon --session --address=\"unix:path=$bus\" --fork --nopidfile >/dev/null 2>&1 || true\n\
         \texport DBUS_SESSION_BUS_ADDRESS=\"unix:path=$bus\"\n\
         \tcommand -v dbus-update-activation-environment >/dev/null 2>&1 && \\\n\
         \t\tdbus-update-activation-environment DISPLAY WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE >/dev/null 2>&1 || true\n\
         fi\n",
    ));
    plan.push(chroot("chmod +x /etc/X11/xinit/xinitrc.d/15-dbus-session.sh 2>/dev/null || true"));
    // fish does NOT read /etc/profile (different syntax), so the profile.d
    // snippet above never runs for a fish login — which matters in the "no DM"
    // case where a fish user logs into a TTY. fish sources /etc/fish/conf.d/*
    // for every shell, so put the equivalent there in fish syntax. Harmless if
    // fish isn't installed (the file just sits unused).
    plan.push(chroot("mkdir -p /etc/fish/conf.d"));
    plan.push(write_target_file(
        "/mnt/etc/fish/conf.d/dbus-session.fish",
        "# Ensure a D-Bus session bus for fish login sessions on systemd-free\n\
         # systems (fish doesn't read /etc/profile, so profile.d won't fire).\n\
         if test -z \"$DBUS_SESSION_BUS_ADDRESS\"; and test -n \"$XDG_RUNTIME_DIR\"\n\
         \tset -l bus \"$XDG_RUNTIME_DIR/bus\"\n\
         \ttest -S \"$bus\"; or dbus-daemon --session --address=\"unix:path=$bus\" --fork --nopidfile >/dev/null 2>&1\n\
         \tset -gx DBUS_SESSION_BUS_ADDRESS \"unix:path=$bus\"\n\
         \tif command -v dbus-update-activation-environment >/dev/null 2>&1\n\
         \t\tdbus-update-activation-environment DISPLAY WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE >/dev/null 2>&1\n\
         \tend\n\
         end\n",
    ));
    match m {
        crate::app::AccountMode::UserSameRoot => {
            plan.push(chroot(&format!(
                "printf 'root:%s\\n' \"{}\" | chpasswd",
                shell_escape_dq(&c.user_password)
            )));
        }
        crate::app::AccountMode::UserSeparateRoot | crate::app::AccountMode::RootOnly => {
            plan.push(chroot(&format!(
                "printf 'root:%s\\n' \"{}\" | chpasswd",
                shell_escape_dq(&c.root_password)
            )));
        }
        crate::app::AccountMode::UserSudoOnly => {
            // Disable root login entirely; access is via sudo (wheel).
            plan.push(chroot("passwd -l root"));
        }
    }

    // 9a-luks) When the root is encrypted, wire up the initramfs and GRUB so
    //     the system can unlock the LUKS volume(s) at boot.
    if c.encrypt_disk {
        let full = c.encrypt_scope == "full" && uefi;
        // USB auto-unlock keyfile applies only to root-scope encryption: with
        // an encrypted /boot, GRUB prompts before the initramfs ever runs.
        let usb = !c.usb_key_device.is_empty() && !full;

        // (1) initramfs HOOKS: add `encrypt` before `filesystems` so root is
        //     unlocked before mount.
        plan.push(chroot(
            "grep -q '^HOOKS=.*encrypt' /etc/mkinitcpio.conf || sed -i 's/\\(^HOOKS=.*\\)\\(filesystems\\)/\\1encrypt \\2/' /etc/mkinitcpio.conf",
        ));

        if full {
            // Full-disk (encrypted /boot). Two keyfiles avoid extra passphrase
            // prompts AND keep /boot mounted in the running system:
            //
            //  • /boot/luks/root.key — random key added to the ROOT container,
            //    baked into the initramfs (cryptkey=). At boot GRUB unlocks
            //    /boot (one passphrase), the kernel loads, and the initramfs
            //    uses this key to open root with no second prompt.
            //
            //  • /etc/luks/boot.key — random key added to the BOOT container,
            //    living inside the (encrypted) root. A crypttab entry uses it to
            //    reopen `cryptboot` once root is mounted, so /boot is available
            //    in the running system (needed for kernel updates) and the
            //    early-fs-fstab mount of /boot no longer fails. The key can't
            //    live in /boot itself (that would be circular), so it sits in
            //    root, which is safe (root is encrypted).
            let pass_esc = luks_pass.replace('\'', "'\\''");
            // root.key (for root, in /boot, into initramfs)
            plan.push(chroot(
                "install -d -m 700 /boot/luks && dd if=/dev/urandom of=/boot/luks/root.key bs=512 count=4 && chmod 600 /boot/luks/root.key",
            ));
            plan.push(chroot(&format!(
                "rootpart=$(blkid -t PARTLABEL=ROOT -o device | head -n1); printf '%s' '{pass}' | cryptsetup luksAddKey \"$rootpart\" /boot/luks/root.key",
                pass = pass_esc
            )));
            plan.push(chroot(
                "grep -q '^FILES=.*root.key' /etc/mkinitcpio.conf || sed -i 's#^FILES=(#FILES=(/boot/luks/root.key #' /etc/mkinitcpio.conf",
            ));
            // boot.key (for boot, in /etc on root, used by crypttab)
            plan.push(chroot(
                "install -d -m 700 /etc/luks && dd if=/dev/urandom of=/etc/luks/boot.key bs=512 count=4 && chmod 600 /etc/luks/boot.key",
            ));
            plan.push(chroot(&format!(
                "bootpart=$(blkid -t PARTLABEL=BOOT -o device | head -n1); printf '%s' '{pass}' | cryptsetup luksAddKey \"$bootpart\" /etc/luks/boot.key",
                pass = pass_esc
            )));
            // crypttab: open cryptboot from the BOOT partition's LUKS UUID using
            // the keyfile, automatically, when the system comes up.
            plan.push(chroot(
                "bootpart=$(blkid -t PARTLABEL=BOOT -o device | head -n1); \
                 buuid=$(blkid -s UUID -o value \"$bootpart\"); \
                 grep -q '^cryptboot ' /etc/crypttab || \
                 echo \"cryptboot UUID=${buuid} /etc/luks/boot.key luks\" >> /etc/crypttab",
            ));
        }

        // (1b) USB unlock keyfile. The chosen stick is WIPED and reformatted
        //     FAT32 with the label ARTIXKEY (a label needs no runtime UUID
        //     plumbing — the encrypt hook resolves LABEL= by itself). A fresh
        //     4096-byte RANDOM key (from /dev/urandom, unique per install) is
        //     generated, added as its OWN LUKS slot, and written to the stick;
        //     the temp copy is shredded from the live tmpfs. Runs on the HOST
        //     (live env), not the chroot: the stick is a live-system device.
        //
        //     TWO INDEPENDENT KEYS unlock the disk in BACKUP mode:
        //       • the stick's random key (this slot) — for automatic unlock,
        //       • the user's memorable passphrase (slot 0, used to luksFormat
        //         the container back in step 1) — the fallback if the stick is
        //         lost. They are different secrets in different slots.
        //     In KEY-ONLY mode there is no passphrase: a throwaway random
        //     secret formatted the container, and it is removed at the end of
        //     this chain (kill_pass), leaving the stick's key as the ONLY slot.
        if usb {
            let dev = c.usb_key_device.replace('\'', "");
            let pass_esc = luks_pass.replace('\'', "'\\''");
            plan.push(act("sh", &["-c", &format!(
                "umount '{dev}'* 2>/dev/null; \
                 wipefs -a '{dev}' && mkfs.fat -I -F 32 -n ARTIXKEY '{dev}' && \
                 mkdir -p /run/artix-usbkey && mount -t vfat '{dev}' /run/artix-usbkey && \
                 dd if=/dev/urandom of=/run/artix-usb.key bs=512 count=8 && chmod 600 /run/artix-usb.key && \
                 rootpart=$(blkid -t PARTLABEL=ROOT -o device | head -n1) && \
                 printf '%s' '{pass}' | cryptsetup luksAddKey \"$rootpart\" /run/artix-usb.key && \
                 cp /run/artix-usb.key /run/artix-usbkey/artix-luks.key && sync && \
                 umount /run/artix-usbkey && shred -u /run/artix-usb.key{kill_pass}",
                dev = dev, pass = pass_esc,
                // Key-only mode: remove the passphrase slot — but ONLY as the
                // LAST link of the && chain, after the key is verifiably on
                // the stick. Any earlier failure leaves the passphrase intact,
                // so a botched run can never lock the user out. (In backup mode
                // this is empty, so the passphrase slot is kept as the
                // fallback key.)
                kill_pass = if c.usb_key_only {
                    format!(
                        " && printf '%s' '{pass}' | cryptsetup luksRemoveKey \"$rootpart\"",
                        pass = pass_esc
                    )
                } else {
                    String::new()
                }
            )]));
            // The initramfs must be able to see the stick BEFORE asking for
            // the key: USB storage + vfat + the FAT codepage/NLS modules
            // (autodetect won't reliably include them for a hot-pluggable
            // device, so they're pinned explicitly).
            plan.push(chroot(
                "grep -q '^MODULES=.*vfat' /etc/mkinitcpio.conf || sed -i 's/^MODULES=(/MODULES=(usb_storage vfat nls_cp437 nls_iso8859-1 /' /etc/mkinitcpio.conf",
            ));
        }

        // (2) GRUB cmdline: name the LUKS root device + mapper. For full-boot
        //     also point the encrypt hook at the embedded keyfile.
        let cryptkey = if full {
            " cryptkey=rootfs:/boot/luks/root.key"
        } else if usb {
            " cryptkey=LABEL=ARTIXKEY:vfat:/artix-luks.key"
        } else {
            ""
        };
        plan.push(chroot(&format!(
            "rootpart=$(blkid -t PARTLABEL=ROOT -o device | head -n1); ruuid=$(blkid -s UUID -o value \"$rootpart\"); sed -i \"s#^GRUB_CMDLINE_LINUX_DEFAULT=\\\"#&cryptdevice=UUID=${{ruuid}}:cryptroot root=/dev/mapper/cryptroot{cryptkey} #\" /etc/default/grub",
            cryptkey = cryptkey
        )));

        // (3) Full-boot: GRUB must read the LUKS container itself.
        if full {
            plan.push(chroot(
                "grep -q '^GRUB_ENABLE_CRYPTODISK=y' /etc/default/grub || echo 'GRUB_ENABLE_CRYPTODISK=y' >> /etc/default/grub",
            ));
        }
    }

    // 9a-nvidia) Proprietary NVIDIA (open-dkms or 580xx legacy): blacklist the
    //     open-source nouveau driver so it doesn't grab the card before the
    //     proprietary module loads. NVIDIA's tooling expects this and warns if
    //     it's missing. We do it two ways for robustness: a kernel cmdline arg
    //     in GRUB (nouveau.modeset=0 modprobe.blacklist=nouveau) and a
    //     modprobe.d blacklist (covers the initramfs/modules path too).
    if any_proprietary_nvidia(&c.gpu) {
        plan.push(chroot(
            "sed -i 's#^GRUB_CMDLINE_LINUX_DEFAULT=\"#&nouveau.modeset=0 modprobe.blacklist=nouveau #' /etc/default/grub",
        ));
        plan.push(write_target_file(
            "/mnt/etc/modprobe.d/blacklist-nouveau.conf",
            "# Disable the open-source nouveau driver in favour of the proprietary\n# NVIDIA driver installed by the Artix installer.\nblacklist nouveau\noptions nouveau modeset=0\n",
        ));
    }

    // 9b) Generate the initramfs for the installed kernel(s). mkinitcpio -P
    //     builds every preset present, covering whichever kernel was chosen.
    plan.push(chroot("mkinitcpio -P"));

    // 10) Bootloader. GRUB is the default and the only one that can decrypt an
    //     encrypted /boot; rEFInd and Limine are offered for plaintext and
    //     root-only LUKS (the UI blocks full-disk encryption with them).
    match c.bootloader.as_str() {
        "refind" => {
            if uefi {
                // refind-install copies rEFInd to the ESP and registers it.
                plan.push(chroot("refind-install || true"));
                // Build refind_linux.conf with the root (and LUKS) cmdline,
                // resolving the root device to its mapper/UUID at install time.
                let luks = luks_cmdline_part(c);
                let rootflags = rootflags_part(c);
                let root_dev = if c.encrypt_disk { "/dev/mapper/cryptroot" } else { "UUID=$ruuid" };
                plan.push(chroot(&format!(
                    "rootpart=$(blkid -t PARTLABEL=ROOT -o device | head -n1); \
                     ruuid=$(blkid -s UUID -o value \"$rootpart\"); \
                     printf '\\\"Boot with standard options\\\"  \\\"{luks}{rootflags}root={root} rw\\\"\\n' > /boot/refind_linux.conf",
                    luks = luks, rootflags = rootflags, root = root_dev
                )));
            } else {
                // rEFInd is UEFI-only; fall back to GRUB on BIOS.
                plan.push(chroot(&format!("grub-install --target=i386-pc {}", c.disk)));
                plan.push(chroot("grub-mkconfig -o /boot/grub/grub.cfg"));
            }
        }
        "limine" => {
            if uefi {
                plan.push(chroot(
                    "mkdir -p /boot/EFI/limine && cp /usr/share/limine/BOOTX64.EFI /boot/EFI/limine/ || true",
                ));
                let id = c.bootloader_id.replace('\'', "");
                // NOTE: single backslashes inside single quotes — efibootmgr
                // takes the EFI device path literally, and doubled backslashes
                // produce a path the firmware can't resolve.
                plan.push(chroot(&format!(
                    "efibootmgr --create --disk {disk} --part 1 --loader '\\EFI\\limine\\BOOTX64.EFI' --label '{id}' || true",
                    disk = c.disk, id = id
                )));
                // limine.conf with the kernel/initramfs of the CHOSEN kernel
                // (root resolved at install time). Microcode images are added
                // as modules BEFORE the initramfs when present (Limine has no
                // grub-mkconfig-style autodetection). The config is written
                // both next to the EFI executable (the first place Limine
                // looks) and at the partition root (a documented fallback).
                let luks = luks_cmdline_part(c);
                let rootflags = rootflags_part(c);
                let root_dev = if c.encrypt_disk { "/dev/mapper/cryptroot" } else { "UUID=$ruuid" };
                let (vmlinuz, initramfs) = kernel_images(&c.kernel);
                plan.push(chroot(&format!(
                    "rootpart=$(blkid -t PARTLABEL=ROOT -o device | head -n1); \
                     ruuid=$(blkid -s UUID -o value \"$rootpart\"); \
                     {{ printf 'timeout: 3\\n\\n/Artix Linux\\n    protocol: linux\\n    kernel_path: boot():/{vmlinuz}\\n'; \
                        [ -f /boot/amd-ucode.img ] && printf '    module_path: boot():/amd-ucode.img\\n'; \
                        [ -f /boot/intel-ucode.img ] && printf '    module_path: boot():/intel-ucode.img\\n'; \
                        printf '    module_path: boot():/{initramfs}\\n    cmdline: {luks}{rootflags}root=%s rw\\n' \"{root}\"; \
                     }} > /boot/limine.conf; \
                     cp /boot/limine.conf /boot/EFI/limine/limine.conf",
                    vmlinuz = vmlinuz, initramfs = initramfs,
                    luks = luks, rootflags = rootflags, root = root_dev
                )));
            } else {
                plan.push(chroot(&format!("grub-install --target=i386-pc {}", c.disk)));
                plan.push(chroot("grub-mkconfig -o /boot/grub/grub.cfg"));
            }
        }
        _ => {
            // GRUB (default).
            if uefi {
                let efi_dir = if c.encrypt_disk && c.encrypt_scope == "full" {
                    "/boot/efi"
                } else {
                    "/boot"
                };
                plan.push(chroot(&format!(
                    "grub-install --target=x86_64-efi --efi-directory={efi} --bootloader-id='{id}'",
                    efi = efi_dir,
                    id = c.bootloader_id.replace('\'', "")
                )));
            } else {
                plan.push(chroot(&format!("grub-install --target=i386-pc {}", c.disk)));
            }
            // Optionally let GRUB detect other installed OSes (e.g. Windows) and
            // add boot entries. GRUB 2.06+ disables os-prober by default, so we
            // flip GRUB_DISABLE_OS_PROBER=false before generating the config.
            if c.os_prober {
                plan.push(chroot(
                    "if grep -q '^GRUB_DISABLE_OS_PROBER=' /etc/default/grub; then \
                     sed -i 's/^GRUB_DISABLE_OS_PROBER=.*/GRUB_DISABLE_OS_PROBER=false/' /etc/default/grub; \
                     else echo 'GRUB_DISABLE_OS_PROBER=false' >> /etc/default/grub; fi",
                ));
            }
            plan.push(chroot("grub-mkconfig -o /boot/grub/grub.cfg"));
        }
    }

    // 11) nftables firewall config — embedded in the binary (no ISO asset
    //      dependency). Written to /etc/nftables.conf in the target via a
    //      single-quoted heredoc so all the rule syntax ($, {}, @sets, #) is
    //      preserved verbatim. Started later via nftables-dinit.
    plan.push(write_target_file(
        "/mnt/etc/nftables.conf",
        NFTABLES_CONFIG_TEMPLATE,
    ));

    // 11.5) Display-manager configuration + console-hygiene for systemd-free
    //       systems. Two classes of fixes, learned from real Artix installs:
    //
    //       a) Console spam: with dinit, a supervised service running in
    //          no-daemon mode (NetworkManager -n) writes its log to stdout; a
    //          service file without `logfile` inherits dinit's console — i.e.
    //          the ACTIVE VT. Combined with kernel printk going to the active
    //          console, this paints NM's "couldn't find modem / ModemManager"
    //          probing all over a TUI greeter. Fixed at three levels: route
    //          the NM dinit service to a logfile, lower NM's own log level
    //          (the modem-probe lines are warnings), and quiet kernel printk
    //          on the console (the sysctl tuigreet's own README recommends).
    //          Applied to EVERY install — a bare TTY benefits just as much.
    //
    //       b) greetd runs on vt = 7 (proven on Artix dinit). A full greeter
    //          (tuigreet / graphical) switches the console to that VT itself;
    //          bare agreety does not, so tuigreet is the recommended text
    //          greeter. tty1 and its getty are NEVER touched — dinit needs them.
    plan.push(write_target_file(
        "/mnt/etc/sysctl.d/20-quiet-printk.conf",
        "# Keep kernel messages off the console (they still go to dmesg/logs).\n\
         # Without this, printk paints over TUI greeters and bare TTYs.\n\
         kernel.printk = 3 3 3 3",
    ));
    plan.push(chroot(
        // Route the NetworkManager dinit service's stdout to a logfile instead
        // of the console. /etc/dinit.d files are pacman backup files, so this
        // edit survives updates. Guarded: only appended if no logfile is set.
        "mkdir -p /var/log/dinit; \
         if [ -f /etc/dinit.d/networkmanager ] && ! grep -q '^[[:space:]]*logfile' /etc/dinit.d/networkmanager; then \
           printf '\\nlogfile = /var/log/dinit/networkmanager.log\\n' >> /etc/dinit.d/networkmanager; \
         fi",
    ));
    plan.push(write_target_file(
        "/mnt/etc/NetworkManager/conf.d/20-quiet-logging.conf",
        "# Only log errors. NM's default INFO/WARN level includes endless\n\
         # modem/ModemManager probing chatter on systems with no modem, which\n\
         # (via dinit's console) bleeds onto TUI greeters. Raise back to WARN\n\
         # or INFO temporarily when debugging network issues.\n\
         [logging]\n\
         level=ERR",
    ));
    if dm_service(&c.display_manager) == Some("greetd") {
        let greeter_cmd: String = match c.display_manager.as_str() {
            // tuigreet builds its session list from the installed .desktop
            // files. CRITICAL flag split: --sessions is for WAYLAND session
            // dirs (commands exec'd directly), --xsessions is for X11 session
            // dirs — only those get wrapped in --xsession-wrapper (default
            // "startx /usr/bin/env", hence xorg-xinit), which actually starts
            // the X server. Feeding xsessions through --sessions launches the
            // DE with no display and it dies instantly. --remember* needs a
            // writable cache dir (created below).
            "tuigreet" => "tuigreet --time --remember --remember-user-session --asterisks \
                           --sessions /usr/share/wayland-sessions \
                           --xsessions /usr/share/xsessions"
                .into(),
            // ReGreet is a GTK Wayland greeter; it needs a compositor to host
            // it — cage in kiosk mode (-s allows VT switching).
            "regreet" => "cage -s -- regreet".into(),
            // Fallback (shouldn't hit — every shipped greeter is handled above).
            _ => "tuigreet --time".into(),
        };
        plan.push(write_target_file(
            "/mnt/etc/greetd/config.toml",
            &format!(
                "# /etc/greetd/config.toml — generated by the installer.\n\
                 [terminal]\n\
                 # vt = 7: the standard greetd display VT, proven on Artix\n\
                 # dinit. tty1 and its getty are LEFT ALONE (dinit needs the\n\
                 # tty1 getty). NOTE: a full greeter (tuigreet / the graphical\n\
                 # ones) switches the active console to this VT and clears it\n\
                 # itself. Bare `agreety` does NOT — it's a getty replacement\n\
                 # that expects to be activated on a VT — so with agreety you\n\
                 # may need Ctrl+Alt+F7 to see it. tuigreet is the recommended\n\
                 # text greeter for exactly this reason.\n\
                 vt = 7\n\
                 \n\
                 [default_session]\n\
                 command = \"{greeter_cmd}\"\n\
                 user = \"greeter\""
            ),
        ));
        // The greeter user + writable dirs. The greetd package normally
        // creates the user, but Artix packaging has no systemd-sysusers — be
        // defensive. video group: DRM/backlight access for cage/sway; seat
        // group: REQUIRED with seatd or the greeter's compositor can't
        // acquire the seat (the classic "ReGreet just doesn't start" on
        // Artix). All guarded with || true where absence is legitimate.
        plan.push(chroot(
            "id greeter >/dev/null 2>&1 || useradd -r -M -d /var/lib/greetd -s /usr/bin/nologin greeter; \
             mkdir -p /var/lib/greetd /var/cache/tuigreet; \
             chown greeter: /var/lib/greetd /var/cache/tuigreet; \
             gpasswd -a greeter video 2>/dev/null || true",
        ));
        if c.seat_provider == "seatd" {
            plan.push(chroot("gpasswd -a greeter seat 2>/dev/null || true"));
        }
        // ReGreet needs no extra session file: it reads the installed
        // wayland-sessions/xsessions .desktop entries directly. (tuigreet does
        // the same.) The AUR-only gtkgreet/wlgreet — which needed an
        // environments/sway-config file — are not offered, so there's nothing
        // more to write here.
    }

    // 12) Enable all dinit services in the installed system. Add the chosen
    //      display manager's service, and the chosen seat manager service.
    let mut services: Vec<String> = DINIT_SERVICES.iter().map(|s| s.to_string()).collect();
    if let Some(dm_svc) = dm_service(&c.display_manager) {
        services.push(dm_svc.to_string());
    }
    // Enable the seat manager's dinit SERVICE (named after the daemon, not the
    // *-dinit package): "elogind" or "seatd". Done ALWAYS — even with no
    // desktop — since the package is always installed and a later-installed WM
    // needs the seat service running.
    services.push(match c.seat_provider.as_str() {
        "elogind" => "elogind".into(),
        _ => "seatd".into(),
    });
    // Enable the per-user dinit launcher service that matches the backend
    // installed above: userspawn (elogind) or turnstiled (seatd/none). This is
    // what spawns the user's dinit instance on login → D-Bus, PipeWire, sound.
    services.push(match c.seat_provider.as_str() {
        "elogind" => "userspawn".into(),
        _ => "turnstiled".into(),
    });
    // System logging: syslog-ng captures all system logs to /var/log; cronie
    // runs the scheduled logrotate job that expires them weekly (see the
    // logrotate config written below). Both are always enabled.
    services.push("syslog-ng".into());
    services.push("cronie".into());
    // NVIDIA: enable nvidia-persistenced (shipped by nvidia-utils-dinit) so the
    // driver keeps device state initialized — recommended for NVIDIA GPUs.
    if any_proprietary_nvidia(&c.gpu) {
        services.push("nvidia-persistenced".into());
    }
    // Enable each system service by symlinking it into /etc/dinit.d/boot.d/.
    // We do NOT use `dinitctl --offline enable`: in an installed-but-not-booted
    // root it looks for the `boot` service/state and fails. The symlink method
    // is what the Artix dinit docs use for offline/chroot enabling and always
    // works. We ensure boot.d exists and only link a service whose file exists
    // (so a missing optional service is skipped, not fatal).
    let svc_cmd = format!(
        "mkdir -p /etc/dinit.d/boot.d; for s in {}; do if [ -e /etc/dinit.d/$s ]; then ln -sf /etc/dinit.d/$s /etc/dinit.d/boot.d/$s; echo \"enabled $s\"; else echo \"  (no service file for $s, skipped)\"; fi; done",
        services.join(" ")
    );
    plan.push(chroot(&svc_cmd));

    // Auto-enable ANY dinit service shipped by an installed *-dinit package —
    // including ones the user picked from the repos or AUR that we don't list
    // explicitly above (e.g. asusctl-nosystemd-dinit, cups-dinit, …). For every
    // installed package whose name ends in "-dinit", we read its file list with
    // `pacman -Ql`, pick the service descriptions it drops directly in
    // /etc/dinit.d/ (top level, not the boot.d/ or user/ subdirs), and symlink
    // each into boot.d/ so it starts at boot. Already-enabled ones are simply
    // re-linked (idempotent). This is best-effort: || true so a quirky package
    // never aborts the install.
    plan.push(chroot(
        "mkdir -p /etc/dinit.d/boot.d; \
         for pkg in $(pacman -Qq | grep -- '-dinit$'); do \
           pacman -Ql \"$pkg\" 2>/dev/null | awk '{print $2}' | \
           grep -E '^/etc/dinit.d/[^/]+$' | while read -r f; do \
             svc=$(basename \"$f\"); \
             [ -f \"$f\" ] && ln -sf \"/etc/dinit.d/$svc\" \"/etc/dinit.d/boot.d/$svc\" && \
             echo \"auto-enabled $svc (from $pkg)\"; \
           done; \
         done || true",
    ));

    // Wire up the per-user dinit launcher chosen above. The two backends need
    // different setup, so branch on the seat provider.
    if c.seat_provider == "elogind" {
        // ELOGIND → userspawn. userspawn reacts to logind's UserNew D-Bus
        // signal (provided by elogind) and then runs a userspawnrc script —
        // it does NOT exec dinit directly, so without that script it fails with
        // "Failed to exec user" and no user dinit starts. We write the
        // system-wide /etc/xdg/userspawn/userspawnrc (works for any user) plus
        // the two per-user locations userspawn also checks, each just execing
        // the user dinit. elogind itself provides /run/user/<uid> and tracks
        // the session, so we make sure pam_elogind is in the login stack and
        // strip any pam_rundir/pam_turnstile that might fight it.
        let home = format!("/home/{}", c.username);
        const USERSPAWNRC: &str = "#!/bin/sh\nexec dinit --user\n";
        plan.push(write_target_file(
            "/mnt/etc/xdg/userspawn/userspawnrc",
            USERSPAWNRC,
        ));
        plan.push(chroot("chmod +x /etc/xdg/userspawn/userspawnrc"));
        plan.push(write_home_file(&home, ".userspawnrc", USERSPAWNRC));
        plan.push(chroot(&format!("mkdir -p {home}/.config/userspawn")));
        plan.push(write_home_file(
            &home,
            ".config/userspawn/userspawnrc",
            USERSPAWNRC,
        ));
        plan.push(chroot(&format!(
            "chmod +x {home}/.userspawnrc {home}/.config/userspawn/userspawnrc; \
             chown -R {0}:{0} {home}/.userspawnrc {home}/.config/userspawn",
            c.username
        )));
        plan.push(chroot(
            "f=/etc/pam.d/system-login; if [ -f \"$f\" ]; then \
             sed -i '/pam_rundir.so/d; /pam_turnstile.so/d' \"$f\"; \
             if ! grep -q 'pam_elogind.so' \"$f\"; then \
             if grep -q 'pam_env.so' \"$f\"; then \
             sed -i '/pam_env.so/a -session   optional   pam_elogind.so' \"$f\"; \
             else echo '-session   optional   pam_elogind.so' >> \"$f\"; fi; \
             echo 'wired pam_elogind.so + userspawnrc (elogind -> userspawn -> dinit --user)'; \
             fi; fi",
        ));
    } else {
        // SEATD / NONE → turnstile. Three pieces (per turnstile docs + Artix
        // wiki): (1) pam_turnstile.so in the login stack — on login it tells
        // turnstiled to spawn `dinit --user` via the Dinit backend, with NO
        // elogind needed; (2) /etc/turnstile/turnstiled.conf: backend = dinit
        // and manage_rundir = yes so turnstile creates /run/user/<uid> itself
        // (no elogind to do it); (3) the turnstiled service (enabled above). We
        // strip pam_rundir/pam_elogind so nothing else fights over the runtime
        // dir or session.
        plan.push(chroot(
            "f=/etc/pam.d/system-login; if [ -f \"$f\" ]; then \
             sed -i '/pam_rundir.so/d; /pam_elogind.so/d' \"$f\"; \
             if ! grep -q 'pam_turnstile.so' \"$f\"; then \
             if grep -q 'pam_env.so' \"$f\"; then \
             sed -i '/pam_env.so/a session    optional   pam_turnstile.so' \"$f\"; \
             else echo 'session    optional   pam_turnstile.so' >> \"$f\"; fi; \
             echo 'wired pam_turnstile.so (login -> turnstiled -> dinit --user)'; \
             fi; fi",
        ));
        plan.push(chroot(
            "f=/etc/turnstile/turnstiled.conf; mkdir -p /etc/turnstile; touch \"$f\"; \
             if grep -qE '^[#[:space:]]*backend' \"$f\"; then \
             sed -i 's|^[#[:space:]]*backend.*|backend = dinit|' \"$f\"; \
             else echo 'backend = dinit' >> \"$f\"; fi; \
             if grep -qE '^[#[:space:]]*manage_rundir' \"$f\"; then \
             sed -i 's|^[#[:space:]]*manage_rundir.*|manage_rundir = yes|' \"$f\"; \
             else echo 'manage_rundir = yes' >> \"$f\"; fi; \
             echo 'turnstiled.conf: backend=dinit manage_rundir=yes'",
        ));
    }

    // syslog-ng config. The stock Arch/Artix syslog-ng package does NOT write
    // /var/log out of the box on a current install (its shipped config is
    // minimal), so without this nothing lands in /var/log and the rotation rule
    // below has nothing to act on. We OVERWRITE /etc/syslog-ng/syslog-ng.conf
    // with a straightforward config that reads the local system + kernel +
    // internal sources and writes predictable *.log files: messages.log
    // (everything), auth.log, kernel.log, daemon.log, and a single everything.log
    // catch-all. create_dirs(yes) makes /var/log if missing; owner/group/perm
    // keep them root:log 0640. @version is set a touch low on purpose — syslog-ng
    // accepts an older declaration with a harmless notice, which is more robust
    // across package bumps than pinning the exact installed version.
    plan.push(write_target_file(
        "/mnt/etc/syslog-ng/syslog-ng.conf",
        "@version: 4.0\n\
         @include \"scl.conf\"\n\
         # Managed by the Artix installer.\n\
         options {\n\
         \x20   chain_hostnames(off); flush_lines(0); use_dns(no); use_fqdn(no);\n\
         \x20   owner(\"root\"); group(\"log\"); perm(0640); create_dirs(yes);\n\
         \x20   dir_owner(\"root\"); dir_group(\"log\"); dir_perm(0755);\n\
         \x20   keep_hostname(yes); stats(freq(0)); time_reopen(10);\n\
         };\n\
         source s_system { system(); internal(); };\n\
         destination d_messages   { file(\"/var/log/messages.log\"); };\n\
         destination d_auth       { file(\"/var/log/auth.log\"); };\n\
         destination d_kernel     { file(\"/var/log/kernel.log\"); };\n\
         destination d_daemon     { file(\"/var/log/daemon.log\"); };\n\
         destination d_everything { file(\"/var/log/everything.log\"); };\n\
         filter f_auth   { facility(auth, authpriv); };\n\
         filter f_kernel { facility(kern); };\n\
         filter f_daemon { facility(daemon); };\n\
         log { source(s_system); filter(f_auth);   destination(d_auth); };\n\
         log { source(s_system); filter(f_kernel); destination(d_kernel); };\n\
         log { source(s_system); filter(f_daemon); destination(d_daemon); };\n\
         log { source(s_system); destination(d_messages); };\n\
         log { source(s_system); destination(d_everything); };\n",
    ));

    // System log retention: keep ~1 week of logs, then auto-delete. syslog-ng
    // (enabled above) writes the whole system's logs as /var/log/*.log. We
    // control their rotation by OVERWRITING /etc/logrotate.d/syslog-ng (the
    // file syslog-ng's package ships) so there's exactly one rule for those
    // files — a second rule covering the same glob would make logrotate abort
    // with "duplicate log entry". Policy:
    //   daily      — consider rotation every day (fine granularity)
    //   rotate 7   — keep 7 rotated files → about one week of history
    //   maxage 7   — and hard-delete anything older than 7 days (the actual
    //                "expire after a week" guarantee, independent of count)
    //   maxsize 5G — but rotate IMMEDIATELY if a log reaches 5 GB, without
    //                waiting for the daily run. Guards against a misbehaving
    //                service flooding the log and filling the disk: the bloated
    //                file is rotated out at once, and (with compress + rotate/
    //                maxage) the old copies are then expired. So no single log
    //                grows past ~5 GB between rotations.
    //   compress/delaycompress — save space; newest rotated stays uncompressed
    //   postrotate syslog-ng-ctl reload — tell syslog-ng to reopen its files
    // The leading wildcard covers whatever exact names syslog-ng uses
    // (messages.log, auth.log, daemon.log, kernel.log, everything.log, …).
    plan.push(write_target_file(
        "/mnt/etc/logrotate.d/syslog-ng",
        "# Managed by the Artix installer: keep ~1 week of system logs, then delete.\n\
         /var/log/*.log {\n\
         \x20   daily\n\
         \x20   rotate 7\n\
         \x20   maxage 7\n\
         \x20   maxsize 5G\n\
         \x20   missingok\n\
         \x20   notifempty\n\
         \x20   sharedscripts\n\
         \x20   compress\n\
         \x20   delaycompress\n\
         \x20   postrotate\n\
         \x20       /usr/bin/syslog-ng-ctl reload >/dev/null 2>&1 || true\n\
         \x20   endscript\n\
         }\n",
    ));
    // logrotate has no schedule of its own. On systemd it's a timer; here we
    // drive it from cron. cronie (enabled above) reads /etc/cron.d, so drop a
    // job that runs logrotate once a day at 03:00. This is what actually makes
    // the weekly expiry happen — without a cron entry logrotate never runs and
    // logs would grow without bound. MAILTO="" silences cron mail.
    plan.push(write_target_file(
        "/mnt/etc/cron.d/logrotate",
        "# Managed by the Artix installer: run logrotate daily (expires logs ~weekly).\n\
         MAILTO=\"\"\n\
         0 3 * * * root /usr/bin/logrotate /etc/logrotate.conf\n",
    ));
    //   1) ensure base-devel + git are present,
    //   2) grant the user temporary passwordless sudo (makepkg needs it to
    //      install build deps); this line is removed afterwards,
    //   3) build paru-bin (prebuilt, fast) from the AUR as the user,
    //   4) paru -S --noconfirm the requested packages,
    //   5) revoke the temporary passwordless sudo.
    // The whole thing is best-effort: failures are logged but don't abort the
    // install (the base system is already complete).
    let aur_pkgs = effective_aur_packages(c);
    if !aur_pkgs.is_empty() && m.needs_user() {
        let user = &c.username;
        let pkgs = aur_pkgs.join(" ");
        // base-devel + git for building, plus the alpm headers paru links to.
        plan.push(chroot("pacman -S --needed --noconfirm base-devel git || true"));
        // Bring the system fully up to date FIRST. paru links against
        // libalpm.so (shipped by pacman); if the base we strapped has an older
        // pacman than the repos, a prebuilt paru-bin would fail with
        // "libalpm.so.NN not found". A full upgrade aligns pacman/libalpm with
        // the repos, and building paru from source (below) then links it
        // against exactly this libalpm — so it keeps working.
        plan.push(chroot("pacman -Syu --noconfirm || true"));
        // Temporary passwordless sudo for the user (so makepkg can install deps).
        plan.push(chroot(&format!(
            "echo '{user} ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/99-aur-temp"
        )));
        // Install paru (the AUR helper). With Chaotic-AUR enabled, paru is
        // already a PREBUILT binary in that repo — and built against the current
        // Arch libalpm, so it's alpm-compatible — meaning `pacman -S paru`
        // installs it INSTANTLY, with no Rust toolchain and no compile (this is
        // exactly why the user hit a long rust build before). Without Chaotic,
        // or if that pull fails, fall back to building paru FROM SOURCE against
        // the system's own libalpm: slower (a Rust build) but robust against a
        // version mismatch. Non-interactive either way.
        let build_paru_from_src = format!(
            "su - {user} -c 'cd ~ && rm -rf paru && git clone https://aur.archlinux.org/paru.git && cd paru && makepkg -si --noconfirm'"
        );
        if c.chaotic_aur {
            plan.push(chroot(&format!(
                "pacman -S --needed --noconfirm paru || {build_paru_from_src} || true"
            )));
        } else {
            plan.push(chroot(&format!("{build_paru_from_src} || true")));
        }
        // Install the requested AUR packages INTERACTIVELY under a PTY (no
        // --noconfirm) so the user picks providers; --skipreview skips the
        // PKGBUILD dump. --needed avoids reinstalling, Y/n auto-answered.
        // LC_ALL=C INSIDE the su - login shell: `su -` resets the environment
        // from the (localised) user profile, which would otherwise make paru's
        // prompts localised and unmatchable by our detection heuristics.
        plan.push(chroot_interactive(&format!(
            "su - {user} -c 'LANG=C LC_ALL=C LC_MESSAGES=C paru -S --needed --skipreview {pkgs}' || true"
        )));
        // Clean up the build dir and revoke the temporary sudo.
        plan.push(chroot(&format!("rm -rf /home/{user}/paru || true")));
        plan.push(chroot("rm -f /etc/sudoers.d/99-aur-temp"));

        // If the user picked auto-cpufreq, wire up its daemon. auto-cpufreq has
        // no dinit service and its `--install` only deploys a SYSTEMD unit
        // (upstream issues #91/#96), which is useless on Artix+dinit — so we
        // must NOT run `--install`. Instead we start the daemon mode at boot
        // via cron (cronie, enabled above, honours @reboot), exactly like the
        // hand-rolled setup that worked before. Two things:
        //   1) drop /etc/cron.d/auto-cpufreq with `@reboot ... auto-cpufreq
        //      --daemon` so the optimizer runs from boot onward;
        //   2) DISABLE power-profiles-daemon — it manages CPU power too and
        //      conflicts with auto-cpufreq (upstream masks it for the same
        //      reason). We remove its boot.d symlink so only one governor of
        //      CPU power is active. Both steps run only when the package is
        //      actually among the user's selections, and are best-effort.
        if aur_pkgs.iter().any(|x| x == "auto-cpufreq" || x == "auto-cpufreq-git") {
            plan.push(write_target_file(
                "/mnt/etc/cron.d/auto-cpufreq",
                "# Managed by the Artix installer: start the auto-cpufreq daemon at boot.\n\
                 # auto-cpufreq has no dinit service and its --install only targets\n\
                 # systemd, so we run its daemon mode from cron instead.\n\
                 MAILTO=\"\"\n\
                 @reboot root /usr/bin/auto-cpufreq --daemon\n",
            ));
            // Disable the conflicting power-profiles-daemon (remove its autostart
            // symlink; the package stays installed but won't run at boot).
            plan.push(chroot(
                "rm -f /etc/dinit.d/boot.d/power-profiles-daemon; \
                 echo 'disabled power-profiles-daemon (conflicts with auto-cpufreq)'",
            ));
        }
    }

    // Final step: drop the install log AND a logging help doc into the user's
    // home (or /root if there's no user), owned by them, so they're easy to find
    // and read. The log itself is captured live at /tmp/installer.log; the help
    // doc is localized to the chosen language. All best-effort.
    {
        let (home, owner) = if c.username.is_empty() {
            ("/root".to_string(), "root".to_string())
        } else {
            (format!("/home/{}", c.username), c.username.clone())
        };
        // Localized logging guide, written as the home file reading-logs.md.
        let doc = if c.lang == "uk" { LOG_HELP_UK } else { LOG_HELP_EN };
        plan.push(write_home_file(&home, "reading-logs.md", doc));
        // Copy the live install log into home (it lives at /tmp on the live FS,
        // so this runs live, not in the chroot). mkdir -p covers /root, which
        // may not pre-exist on the target until now.
        plan.push(act(
            "sh",
            &[
                "-c",
                &format!(
                    "echo '>>> Saving install log + logging guide to {home}/ ...'; \
                     mkdir -p /mnt{home} 2>/dev/null; \
                     if [ -f /tmp/installer.log ]; then \
                       cp /tmp/installer.log /mnt{home}/installer.log 2>/dev/null && \
                       echo \">>> Install log saved to {home}/installer.log\"; \
                     else \
                       echo '>>> No install log to save.'; \
                     fi; \
                     true",
                    home = home
                ),
            ],
        ));
        // Own + permission both files inside the chroot, where the username
        // resolves to its uid/gid. The log is 0600 (may carry transaction
        // detail); the guide is world-readable.
        plan.push(chroot(&format!(
            "chown {owner}:{owner} {home}/installer.log {home}/reading-logs.md 2>/dev/null || true; \
             chmod 600 {home}/installer.log 2>/dev/null || true; \
             chmod 644 {home}/reading-logs.md 2>/dev/null || true; \
             true",
            owner = owner,
            home = home
        )));
    }

    plan
}
