//! Package backend. Two sources:
//!   * `list()` — the full repo package list via `pacman -Sl` (cached), used as
//!     an instant offline fallback and for empty-query browsing.
//!   * `search(query)` — a live `pacman -Ss` search (needs network) that returns
//!     name + description, so the user can find packages by keyword.
//!
//! Service variants for *other* init systems (openrc/runit/s6/suite66) are
//! hidden, since this is a dinit distro. NOTE: we match only the exact
//! init-suffix forms to avoid hiding legitimate packages that merely end in
//! similar letters.

use std::sync::atomic::{AtomicBool, Ordering};

/// Tracks whether we've already run `pacman -Sy` this session, so we only sync
/// the databases once (the first search), not on every keystroke.
static SYNCED: AtomicBool = AtomicBool::new(false);

/// Ensure pacman's sync databases exist. In the live environment they are not
/// downloaded by default, so `pacman -Ss`/`-Sl` fail with "database file ...
/// does not exist". We sync once, lazily, on the first search. Requires network
/// + root (the installer already runs as root on the live console).
use std::sync::Mutex;

/// Serializes database sync so two concurrent searches can't both run
/// `pacman -Sy` and collide on the pacman DB lock (/var/lib/pacman/db.lck).
static SYNC_LOCK: Mutex<()> = Mutex::new(());

/// Ensure pacman's sync databases exist. In the live environment they are not
/// downloaded by default, so `pacman -Ss`/`-Sl` fail with "database file ...
/// does not exist". We sync once, lazily, on the first search. Requires network
/// + root (the installer already runs as root on the live console).
///
/// Thread-safety: search runs on a background thread and the user may trigger
/// several in quick succession. We take a mutex around the whole check-and-sync
/// so only ONE `-Sy` ever runs; the rest see SYNCED=true and skip. `-Ss`
/// (read-only) needs no lock, so concurrent searches after sync are fine.
fn ensure_synced() -> Result<(), String> {
    if SYNCED.load(Ordering::Acquire) {
        return Ok(());
    }
    let _guard = SYNC_LOCK
        .lock()
        .map_err(|_| "sync lock poisoned".to_string())?;
    // Re-check inside the lock: another thread may have synced while we waited.
    if SYNCED.load(Ordering::Acquire) {
        return Ok(());
    }
    super::runner::capture("pacman", &["-Sy", "--noconfirm"])?;
    SYNCED.store(true, Ordering::Release);
    Ok(())
}

/// True if a package name is a service variant for a *non-dinit* init system.
/// Matched strictly against known suffixes to avoid false positives.
fn is_other_init(name: &str) -> bool {
    const SUFFIXES: [&str; 4] = ["-openrc", "-runit", "-s6", "-suite66"];
    SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// One search result: package name plus its short description.
#[derive(Clone, Debug)]
pub struct Pkg {
    pub name: String,
    pub desc: String,
}

/// A small, curated set of commonly-wanted packages, shown at the top of the
/// list before the user types anything. Each is (name, short description). The
/// descriptions are filled from the live search if available, but these provide
/// an instant, useful starting point. Chosen to cover browsers, office, media,
/// dev and everyday tools.
pub fn popular() -> Vec<Pkg> {
    const ITEMS: &[(&str, &str)] = &[
        // ── Default-checked set ────────────────────────────────────────
        // Pre-selected on a fresh run (InstallConfig::default): the distro's
        // own configs ride on these. zsh sits first so the shell choice is
        // the first thing the user sees — keep it, swap to fish, or untick
        // both and stay on bash. Every entry unticks like any other.
        (
            "zsh",
            "Powerful interactive shell (default; ships .zshrc + starship)",
        ),
        ("fish", "Friendly interactive shell (alternative to zsh)"),
        (
            "kitty",
            "GPU terminal emulator (default; ships Catppuccin config)",
        ),
        (
            "fastfetch",
            "System info tool (default; ships the syrnyk logo config)",
        ),
        (
            "nftables",
            "Firewall (default; ships the default-deny ruleset)",
        ),
        (
            "octopi",
            "Graphical package manager (default; pacman frontend)",
        ),
        // ── Plain suggestions below ──────────────────────────────────
        ("firefox", "Fast, private web browser"),
        ("ungoogled-chromium", "Chromium without Google dependency"),
        ("rustdesk", "Remote desktop (open-source)"),
        ("libreoffice-fresh", "Full office suite (latest features)"),
        ("libreoffice-still", "Full office suite (stable)"),
        ("thunderbird", "Email and news client"),
        ("vlc", "Multimedia player and framework"),
        ("mpv", "Minimalist video player"),
        ("gimp", "GNU image manipulation program"),
        ("inkscape", "Vector graphics editor"),
        ("obs-studio", "Streaming and screen recording"),
        ("blender", "3D creation suite"),
        ("krita", "Digital painting studio"),
        ("audacity", "Audio editor and recorder"),
        ("signal-desktop", "Private encrypted messenger"),
        ("keepassxc", "Password manager"),
        ("htop", "Interactive process viewer"),
        ("btop", "Modern resource monitor"),
        ("neofetch", "System information tool"),
        ("git", "Distributed version control"),
        ("docker", "Container platform"),
        ("zed", "High-performance code editor"),
        ("neovim", "Hyperextensible Vim-based editor"),
        ("tmux", "Terminal multiplexer"),
        ("wofi", "Wayland application launcher (Wayland only)"),
        ("waybar", "Highly customisable Wayland status bar"),
        ("flatpak", "Universal application packaging"),
        ("steam", "Valve's gaming platform"),
        ("lutris", "Open gaming platform"),
        ("wine", "Run Windows applications"),
        ("wine-staging", "Wine with experimental patches"),
        ("file-roller", "Archive manager (GTK, GNOME)"),
        ("ark", "Archive manager (KDE)"),
        ("xarchiver", "Lightweight GTK archive manager"),
        ("unrar", "RAR archive extractor"),
        ("nmap", "Network discovery and security"),
        ("openssh", "SSH client and server"),
        ("rsync", "Fast file transfer and sync"),
        ("ffmpeg", "Audio/video converter"),
        ("yt-dlp", "Media downloader"),
        ("qbittorrent", "BitTorrent client"),
        ("transmission-gtk", "Lightweight BitTorrent client"),
    ];
    ITEMS
        .iter()
        .filter(|(n, _)| !is_other_init(n))
        .map(|(n, d)| Pkg {
            name: n.to_string(),
            desc: d.to_string(),
        })
        .collect()
}

/// Live keyword search via `pacman -Ss`. Returns name+description pairs.
/// `pacman -Ss` output is two lines per hit:
///   repo/name version (groups)
///       description
/// On failure (e.g. no network) returns an Err with a short message.
pub fn search(query: &str) -> Result<Vec<Pkg>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    // Make sure the sync databases exist (downloads them once if needed).
    ensure_synced()?;
    let raw = super::runner::capture("pacman", &["-Ss", q])?;
    let mut out = Vec::new();
    let mut lines = raw.lines().peekable();
    while let Some(head) = lines.next() {
        // header line starts at column 0 with "repo/name"
        if head.starts_with(char::is_whitespace) || !head.contains('/') {
            continue;
        }
        let after_slash = head.split('/').nth(1).unwrap_or("");
        let name = after_slash
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        if name.is_empty() || is_other_init(&name) {
            continue;
        }
        // description is the following indented line, if present.
        let desc = if let Some(next) = lines.peek() {
            if next.starts_with(char::is_whitespace) {
                let d = next.trim().to_string();
                lines.next();
                d
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        out.push(Pkg { name, desc });
    }
    // De-duplicate by name (a package can appear in several repos).
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    Ok(out)
}

/// Curated popular AUR packages, shown when the AUR search box is empty. These
/// are well-known AUR staples; the user can still search for anything else.
pub fn aur_popular() -> Vec<Pkg> {
    [
        ("yay", "AUR helper (Go)"),
        ("paru", "AUR helper (Rust)"),
        (
            "asusctl-nosystemd-dinit",
            "ASUS laptop control (asusctl) for dinit, no systemd",
        ),
        (
            "visual-studio-code-bin",
            "Microsoft's VS Code editor (official binary)",
        ),
        ("spotify", "Spotify music streaming client"),
        (
            "vesktop-bin",
            "Discord client with Vencord + better Linux screenshare",
        ),
        ("librewolf-bin", "Privacy-oriented Firefox fork (binary)"),
        ("obsidian", "Markdown knowledge base / note-taking app"),
        (
            "heroic-games-launcher-bin",
            "Launcher for Epic Games, GOG and Amazon Prime",
        ),
        (
            "protonup-qt",
            "Install & manage Proton-GE / Wine-GE for Steam & Lutris",
        ),
        (
            "grayjay-bin",
            "FUTO Grayjay: multi-platform video/streaming app",
        ),
        (
            "spicetify-cli",
            "Command-line tool to customize the Spotify client",
        ),
        (
            "pinnacle-comp",
            "Smithay Wayland compositor, AwesomeWM-like (Lua/Rust)",
        ),
        // Two MangoWM packages — both install the SAME Mango compositor and
        // conflict, so the picker enforces choosing only one (see the
        // mutual-exclusion in handle_key). Descriptions are the packages' own
        // AUR descriptions: the tagged `mangowm` is the full build with smooth
        // animations (scenefx); `mangowm-wlonly-git` is the same compositor
        // built without scenefx.
        ("mangowm", "A Wayland compositor with smooth animation"),
        ("mangowm-wlonly-git", "mangowm without scenefx"),
        (
            "jellyfin-desktop",
            "Native desktop client for Jellyfin media server",
        ),
        (
            "jellyfin-desktop-git",
            "Jellyfin desktop client, built from git",
        ),
        (
            "flameshot-git",
            "Powerful screenshot tool with annotation (git)",
        ),
        (
            "appimagelauncher",
            "Integrate, manage and run AppImage apps",
        ),
        ("authme-bin", "Desktop 2FA/TOTP authenticator (binary)"),
        (
            "chatterino2-7tv-native-git",
            "Chatterino Twitch chat client with 7TV support (git)",
        ),
        (
            "dracula-gtk-theme-full",
            "Dracula dark theme for GTK (full variant)",
        ),
        ("geany-git", "Geany: lightweight GTK IDE, built from git"),
        (
            "grim-git",
            "Grim: screenshot utility for Wayland compositors (git)",
        ),
        ("neo-candy-icons", "Neon Candy SVG icon theme"),
        (
            "obs-multi-rtmp",
            "OBS Studio plugin for streaming to multiple RTMP targets",
        ),
        ("pokesay-bin", "Pokémon-themed cowsay (binary)"),
        (
            "satty-git",
            "Satty: screenshot annotation tool for Wayland (git)",
        ),
        (
            "steamguard-cli-bin",
            "Command-line Steam Guard 2FA generator (binary)",
        ),
        ("ttf-apple-emoji", "Apple color emoji font"),
        ("ttf-hanazono", "Hanazono: free CJK kanji font"),
        ("ttf-joypixels", "JoyPixels color emoji font"),
        (
            "wireguard-dkms",
            "WireGuard kernel module via DKMS (for older kernels)",
        ),
        (
            "xone-dkms-git",
            "xone: Xbox One/Series controller driver, DKMS (git)",
        ),
        (
            "xone-dongle-firmware",
            "Firmware for the Xbox wireless dongle (xone)",
        ),
    ]
    .iter()
    .map(|(n, d)| Pkg {
        name: n.to_string(),
        desc: d.to_string(),
    })
    .collect()
}

/// Live AUR search via the AUR RPC v5 API (JSON over HTTPS). The live ISO has
/// network and `curl`. We search by name-and-description and return name + desc.
/// Parsed with a tiny hand-rolled scan (no JSON crate needed) since the schema
/// is flat and stable: objects with "Name" and "Description" fields.
pub fn aur_search(query: &str) -> Result<Vec<Pkg>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    // URL-encode the query minimally (spaces → %20; AUR accepts most chars).
    let encoded = q.replace(' ', "%20");
    let url = format!("https://aur.archlinux.org/rpc/v5/search/{encoded}?by=name-desc");
    // -f fail on HTTP errors, -s silent, -m timeout so a stalled net doesn't hang.
    let raw = super::runner::capture("curl", &["-fsS", "-m", "10", &url])?;
    Ok(parse_aur_json(&raw))
}

/// Minimal parser for the AUR RPC JSON. The response looks like:
///   {"resultcount":N,"results":[{"Name":"x","Description":"y",...},...],...}
/// We scan for "Name" and "Description" string fields per result object. Good
/// enough for our needs and avoids pulling in a JSON dependency.
fn parse_aur_json(raw: &str) -> Vec<Pkg> {
    let mut out = Vec::new();
    // Split on "Name": occurrences; each chunk after the first belongs to a result.
    for chunk in raw.split("\"Name\":").skip(1) {
        let name = json_string_value(chunk);
        if name.is_empty() || is_other_init(&name) {
            continue;
        }
        // Description appears after Name within the same object.
        let desc = if let Some(idx) = chunk.find("\"Description\":") {
            json_string_value(&chunk[idx + "\"Description\":".len()..])
        } else {
            String::new()
        };
        out.push(Pkg { name, desc });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

/// Extract the first JSON string value from `s`, where `s` begins at (or just
/// after) a `:` whose value is a string like `"foo"` (or `null`). Handles basic
/// backslash escapes for quotes.
fn json_string_value(s: &str) -> String {
    let s = s.trim_start();
    let bytes = s.as_bytes();
    // find opening quote (skip leading spaces/colon); stop early if it's null.
    let mut i = 0;
    while i < bytes.len() && bytes[i] != b'"' {
        // "null" (no quote before a comma/brace) → empty.
        if bytes[i] == b',' || bytes[i] == b'}' {
            return String::new();
        }
        i += 1;
    }
    if i >= bytes.len() {
        return String::new();
    }
    i += 1; // past opening quote
    let mut val = String::new();
    let mut esc = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if esc {
            // Keep the escaped char as-is (good enough for display).
            val.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '"' {
            break;
        } else {
            val.push(c);
        }
        i += 1;
    }
    val
}
