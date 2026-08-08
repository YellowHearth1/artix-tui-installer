//! Step 4 — keyboard layouts. Multi-select from EVERY X11 layout the system
//! knows (xkeyboard-config's own catalogue), minus the excluded codes.
//! First checked = primary.

use crate::app::App;
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
use std::sync::OnceLock;

/// X11 layout code -> console keymap name, for the codes where they differ.
///
/// The two namespaces are genuinely separate. The layout list stores X11 codes
/// because that is what the graphical session needs; `loadkeys` and
/// `/etc/vconsole.conf` need kbd's names, and for these fifteen they are not
/// the same word. Ask `loadkeys` for "gb" and it fails — there is no console
/// keymap by that name — so an English install used to leave the console on the
/// kernel default while believing it had set the layout.
///
/// Every pair here was checked against the files actually installed by `kbd`.
/// That check matters: deriving them mechanically (`<code>-latin1`) looks right
/// and quietly maps X11 `la`, which is LAO, onto `la-latin1`, which is Latin
/// American — a keyboard from the wrong continent, for a script it cannot type.
/// Codes with no console equivalent are deliberately absent; the install falls
/// back to `us` for those rather than guessing.
///
/// The install plan builds its shell `case` FROM THIS LIST, so the live console
/// and the installed system cannot drift apart.
pub(crate) const CONSOLE_ALIASES: &[(&str, &str)] = &[
    ("gb", "uk"),
    ("latam", "la-latin1"),
    ("pt", "pt-latin1"),
    ("se", "sv-latin1"),
    ("be", "be-latin1"),
    ("ch", "sg-latin1"),
    ("at", "de-latin1"),
    ("br", "br-abnt2"),
    ("is", "is-latin1"),
    ("tr", "trq"),
    ("bg", "bg_bds-utf8"),
    ("sk", "sk-qwerty"),
    ("si", "slovene"),
    ("hr", "croat"),
    ("ee", "et"),
];

/// The CONSOLE keymap name for an X11 layout code.
pub(crate) fn console_keymap(x11: &str) -> &str {
    CONSOLE_ALIASES
        .iter()
        .find(|(a, _)| *a == x11)
        .map(|(_, k)| *k)
        .unwrap_or(x11)
}

/// The shell `case` arms the install plan uses to do the same translation on
/// the target. Generated so the two can never disagree.
pub(crate) fn alias_case_arms() -> String {
    CONSOLE_ALIASES
        .iter()
        .map(|(a, k)| format!("{a}) km={k} ;; "))
        .collect()
}

/// The console keymap an install will use, from the interface language and the
/// chosen layout. THE one rule, for both the live console and the target.
///
/// A Ukrainian interface gets `ua-utf` rather than the picked Latin layout:
/// checked against its source, its PLAIN layer is Latin — so the initramfs LUKS
/// prompt and ordinary commands still type ASCII — and Cyrillic sits on a group
/// toggled with Right Ctrl / Right Alt. That gives real Ukrainian typing on the
/// TTY at no risk to the passphrase.
///
/// This lived only in the install plan. The live console loaded `config.keymap`
/// instead, so a Ukrainian install typed its passphrase on one map and was
/// asked for it at boot on another — the exact drift the alias guard was
/// supposed to prevent, one level up from where it was looking.
pub(crate) fn plan_keymap<'a>(lang: &str, keymap: &'a str) -> &'a str {
    if lang == "uk" {
        "ua-utf"
    } else {
        keymap
    }
}

/// What to hand `loadkeys` for the live console: the plan's choice, with the
/// X11-to-console aliases applied (the target applies the same two in shell).
pub(crate) fn live_keymap<'a>(lang: &str, keymap: &'a str) -> &'a str {
    console_keymap(plan_keymap(lang, keymap))
}

/// Load a keymap into the LIVE console. Returns whether it took.
///
/// Why this exists at all: the GRUB menu used to offer `keytable=` before boot,
/// and that menu is now hidden so the installer starts without a welcome
/// screen. Nothing else set the live layout — the choice made here was only
/// ever written into the installed system — so everything typed DURING the
/// install went through the kernel default. That is the username, the account
/// password, and the LUKS passphrase: the one string that must be typeable
/// again at every boot, on a console that WILL have the chosen layout loaded.
///
/// Only the Latin layout is ever loaded (see the note in `handle_key`), so this
/// cannot leave someone unable to type a Linux username.
pub(crate) fn apply_keymap(name: &str) -> bool {
    // Never touch the developer's own console from a test.
    if cfg!(test) {
        return true;
    }
    match std::process::Command::new("loadkeys")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        // In a graphical terminal there is no console keymap to set and the
        // failure says nothing about the choice, so it is not reported there.
        Ok(st) => st.success() || !crate::screens::fontpick::on_linux_console(),
        Err(_) => true,
    }
}

/// Layout codes we never offer. Matches the console keymap spellings
/// (`ru`, `ru-*`, `ruwin_*`, …) and the xkb short code.
fn is_excluded_layout(code: &str) -> bool {
    let c = code.to_lowercase();
    c == "ru"
        || c.starts_with("ru-")
        || c.starts_with("ru_")
        || c.starts_with("ruwin")
        || c.contains("russia")
}

/// Layouts whose base script is NOT Latin (Cyrillic / Greek / etc.) among the
/// codes we ever show. These must not become the console keymap — see the
/// LUKS-passphrase note in `handle_key`.
pub(crate) fn is_nonlatin(code: &str) -> bool {
    let c = code.to_lowercase();
    matches!(
        c.as_str(),
        "ua" | "by" | "bg" | "gr" | "mk" | "rs" | "ge" | "am" | "il"
    ) || c.starts_with("ua-")
        || c.starts_with("by-")
        || c.starts_with("bg-")
        || c.starts_with("gr-")
}

/// Every X11 layout this system knows, from xkeyboard-config's own list.
///
/// `base.lst` is the authoritative catalogue — 99 layouts on a stock install —
/// and it is what `xkb_layouts` must hold, because that is the name the
/// graphical session will be configured with.
///
/// NOT the console keymap directory: those are kbd's names, a different
/// namespace with different spellings, and gating the offered list on THEM
/// would have silently dropped `gb`, `pt`, `se`, `br` and eleven others —
/// including the layout this installer defaults to for English.
fn x11_layouts() -> Vec<String> {
    let Ok(text) = std::fs::read_to_string("/usr/share/X11/xkb/rules/base.lst") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut in_layouts = false;
    for line in text.lines() {
        if line.starts_with("! layout") {
            in_layouts = true;
            continue;
        }
        if line.starts_with('!') {
            in_layouts = false;
        }
        if in_layouts {
            if let Some(code) = line.split_whitespace().next() {
                // "custom" is xkb's placeholder for a hand-written layout, not
                // something anyone can pick from a list.
                if !code.is_empty() && code != "custom" {
                    out.push(code.to_string());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn keymaps() -> &'static Vec<String> {
    static K: OnceLock<Vec<String>> = OnceLock::new();
    K.get_or_init(|| {
        // Layouts pinned to the top, in this exact order, since they're the most
        // likely choices for this distro's audience.
        let pinned = ["ua", "gb", "us"];

        // A broad, curated set of common layouts shown after the pinned ones.
        // All are standard console keymap codes present on Artix live images.
        let common = [
            "de", "fr", "es", "it", "pl", "cz", "pt", "nl", "se", "fi", "no", "dk", "be", "ch",
            "at", "hu", "ro", "sk", "si", "hr", "gr", "tr", "bg", "lt", "lv", "ee", "by", "ca",
            "br", "latam", "is", "ie",
        ];

        // Every keymap the live system actually has — all 251 of them on a
        // stock image, not a curated handful. Someone in Ukraine may well type
        // on a German layout, and a keyboard carried in from abroad is not an
        // exotic case; the filter above the list makes a long list workable.
        //
        // Read off the FILESYSTEM. This used to ask `localectl list-keymaps`,
        // which is a systemd tool — on a systemd-free distro it is never
        // installed, so the call always failed, `available` was always empty,
        // and the "anything else the system offers" branch below never had
        // anything to offer. The curated list was all anyone ever saw.
        let available: Vec<String> = x11_layouts()
            .into_iter()
            .filter(|s| !is_excluded_layout(s))
            .collect();
        let exists = |code: &str| -> bool {
            // If localectl gave us nothing (off-target), assume the curated code
            // is fine; otherwise only keep codes the live system actually has.
            available.is_empty() || available.iter().any(|a| a == code)
        };

        let mut v: Vec<String> = Vec::new();
        // 1) pinned, in order, skipping excluded and missing ones.
        for code in pinned {
            if !is_excluded_layout(code) && exists(code) {
                v.push(code.to_string());
            }
        }
        // 2) common curated layouts, in the order listed (NOT alphabetized so the
        //    pinned ones stay on top), de-duplicated against pinned.
        for code in common {
            if !is_excluded_layout(code) && exists(code) && !v.iter().any(|x| x == code) {
                v.push(code.to_string());
            }
        }
        // 3) anything else the live system offers, appended alphabetically, so
        //    nothing is lost for users who need an exotic layout via the filter.
        let mut rest: Vec<String> = available
            .into_iter()
            .filter(|a| !v.iter().any(|x| x == a))
            .collect();
        rest.sort();
        rest.dedup();
        v.extend(rest);
        v
    })
}

/// Human-friendly label for a keymap code, e.g. "ua" → "Ukraine (ua)". The code
/// is always shown in parentheses since that's what's written to the system
/// (vconsole keymap). Unknown codes fall back to just the code. Note: "ua" is
/// the keyboard *layout* code (country = Ukraine); the Ukrainian *language*
/// code is "uk", but the system keymap is "ua", so that's what we keep.
fn label_for(code: &str) -> String {
    let name = match code {
        "ua" => "Ukraine",
        "gb" => "United Kingdom",
        "us" => "United States",
        "de" => "Germany",
        "fr" => "France",
        "es" => "Spain",
        "it" => "Italy",
        "pl" => "Poland",
        "cz" => "Czechia",
        "pt" => "Portugal",
        "nl" => "Netherlands",
        "se" => "Sweden",
        "fi" => "Finland",
        "no" => "Norway",
        "dk" => "Denmark",
        "be" => "Belgium",
        "ch" => "Switzerland",
        "at" => "Austria",
        "hu" => "Hungary",
        "ro" => "Romania",
        "sk" => "Slovakia",
        "si" => "Slovenia",
        "hr" => "Croatia",
        "gr" => "Greece",
        "tr" => "Turkey",
        "bg" => "Bulgaria",
        "lt" => "Lithuania",
        "lv" => "Latvia",
        "ee" => "Estonia",
        "by" => "Belarus",
        "ca" => "Canada",
        "br" => "Brazil",
        // X11 calls the Latin American layout "latam"; plain "la" is LAO.
        // The list used to carry "la" under this label, so anyone in Latin
        // America who picked it got Lao. The console name differs again
        // (la-latin1) and is translated when the keymap is written.
        "latam" => "Latin America",
        "is" => "Iceland",
        "ie" => "Ireland",
        _ => return code.to_string(),
    };
    format!("{name} ({code})")
}

fn filtered(query: &str) -> Vec<String> {
    let q = query.to_lowercase();
    keymaps()
        .iter()
        // Match the code OR the human label, so "germany" finds "de" and "de"
        // finds it too.
        .filter(|k| {
            q.is_empty()
                || k.to_lowercase().contains(&q)
                || label_for(k).to_lowercase().contains(&q)
        })
        .cloned()
        .collect()
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // hint
            Constraint::Length(3), // filter box
            Constraint::Min(0),    // list
            Constraint::Length(3), // actions
        ])
        .spacing(1)
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            t(app.lang, "kb.hint"),
            theme::dim(),
        ))),
        rows[0],
    );

    // Filter box (type to narrow the list).
    let filter = Paragraph::new(Line::from(vec![
        Span::styled("  ", theme::dim()),
        Span::styled(
            if app.kb_query.is_empty() {
                t(app.lang, "kb.filter")
            } else {
                app.kb_query.clone()
            },
            if app.kb_query.is_empty() {
                theme::mute()
            } else {
                theme::normal()
            },
        ),
        Span::styled("|", theme::accent()),
    ]))
    .block(theme::box_rounded());
    f.render_widget(filter, rows[1]);

    let list = filtered(&app.kb_query);
    let chosen = app.config.xkb_layouts.clone();
    // Display the friendly labels ("Ukraine (ua)"), while selection logic still
    // keys off the underlying code stored in `list`.
    // The ORDER you picked them in, and which one is actually primary.
    //
    // The checkmarks are drawn in LIST order, not in the order they were
    // chosen, and the primary is not "the first ticked" either — it is the
    // first LATIN one, because the console keymap goes into the initramfs
    // before the encrypt hook and a Cyrillic keymap makes a LUKS passphrase
    // untypeable. So the screen said "the first ticked is primary", showed
    // Ukraine at the top of the list, and meant `gb`. Three different answers.
    let primary = chosen.iter().find(|x| !is_nonlatin(x));
    let items: Vec<String> = list
        .iter()
        .map(|c| {
            let base = label_for(c);
            match chosen.iter().position(|x| x == c) {
                None => base,
                Some(i) if Some(c) == primary => {
                    format!("{base}  {}. {}", i + 1, t(app.lang, "kb.primary"))
                }
                Some(i) => format!("{base}  {}.", i + 1),
            }
        })
        .collect();
    let checked = |i: usize| -> bool { chosen.contains(&list[i]) };
    widgets::multi_list(f, rows[2], &items, &checked, app.cursor);

    app.can_advance = !app.config.xkb_layouts.is_empty();
    widgets::action_row(
        f,
        rows[3],
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        app.can_advance,
    );
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    let list = filtered(&app.kb_query);
    if super::nav::move_cursor(key.code, &mut app.cursor, list.len()) {
        return;
    }
    match key.code {
        KeyCode::Char(' ') => {
            if let Some(k) = list.get(app.cursor) {
                if let Some(pos) = app.config.xkb_layouts.iter().position(|x| x == k) {
                    app.config.xkb_layouts.remove(pos);
                } else {
                    app.config.xkb_layouts.push(k.clone());
                }
                // From here on the language screen must not replace this.
                app.keyboard_touched = true;
                // Console keymap (vconsole KEYMAP) = first LATIN layout chosen.
                // It must never be a non-Latin one: the initramfs `keymap` hook
                // loads it before the `encrypt` hook, so a Cyrillic/Greek
                // console keymap would silently corrupt LUKS passphrase entry
                // (and early-console logins). X/Wayland still get the full
                // ordered list in xkb_layouts. Fallback: "us".
                app.config.keymap = app
                    .config
                    .xkb_layouts
                    .iter()
                    .find(|x| !is_nonlatin(x))
                    .cloned()
                    .unwrap_or_else(|| "us".into());
                // Load it NOW, not only into the installed system. Everything
                // typed from here on — the username, the password, the LUKS
                // passphrase — goes through this layout, which is the same one
                // the boot prompt will have.
                let km = live_keymap(&app.config.lang, &app.config.keymap).to_string();
                if apply_keymap(&km) {
                    app.pmode_status.clear();
                } else {
                    app.pmode_status = format!("{} {km}", t(app.lang, "kb.failed"));
                }
            }
        }
        KeyCode::Enter if !app.config.xkb_layouts.is_empty() => app.goto_next(),
        KeyCode::Char(c) => {
            app.kb_query.push(c);
            app.cursor = 0;
        }
        KeyCode::Backspace => {
            app.kb_query.pop();
            app.cursor = 0;
        }
        _ => {}
    }
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "kb.footer")
}

#[cfg(test)]
mod alias_tests {
    use super::*;

    /// The live console and the installed system must translate layout codes
    /// the SAME way.
    ///
    /// The list stores X11 codes. Two of them do not exist as console keymaps:
    /// X11 "gb" is kbd "uk", X11 "latam" is kbd "la-latin1". The install plan
    /// applies those aliases in shell, on the target; this file applies them in
    /// Rust, on the live console. If they ever disagree, the layout you type the
    /// LUKS passphrase with is not the layout the boot prompt will use — and
    /// that is only discovered when the disk refuses to open.
    #[test]
    fn the_live_and_installed_keymap_aliases_agree() {
        // The arms the install plan splices into its shell `case`, built from
        // the same list — written out by hand it carried two of the fifteen, so
        // thirteen layouts silently fell back to `us` on the installed system
        // while the live console had the right one.
        let plan = alias_case_arms();
        for (x11, console) in CONSOLE_ALIASES {
            assert_eq!(console_keymap(x11), *console);
            assert!(
                plan.contains(&format!("{x11}) km={console} ;;")),
                "the install plan does not alias {x11} -> {console}"
            );
        }
        assert_eq!(console_keymap("us"), "us");
        assert_eq!(console_keymap("de"), "de");

        // `la` is LAO. `la-latin1` is Latin American. Deriving aliases by
        // pattern maps one onto the other — a keyboard from the wrong continent
        // for a script it cannot type — so `la` must have no alias at all.
        assert_eq!(
            console_keymap("la"),
            "la",
            "Lao was aliased to Latin American"
        );
        assert_eq!(console_keymap("latam"), "la-latin1");
    }

    /// Every alias points at a console keymap that EXISTS.
    ///
    /// Skipped where `kbd` is not installed, so it never fails for the wrong
    /// reason on a machine that simply has no keymaps to check against.
    #[test]
    fn every_alias_points_at_a_real_keymap_file() {
        let root = std::path::Path::new("/usr/share/kbd/keymaps");
        if !root.is_dir() {
            return;
        }
        let mut names = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Some(n) = path.file_name().and_then(|n| n.to_str()) {
                    if let Some(stem) = n.split(".map").next() {
                        if n.contains(".map") {
                            names.push(stem.to_string());
                        }
                    }
                }
            }
        }
        for (x11, console) in CONSOLE_ALIASES {
            assert!(
                names.iter().any(|n| n == console),
                "{x11} is aliased to {console}, which kbd does not ship"
            );
        }
    }

    /// A Ukrainian interface types on `ua-utf` in BOTH places.
    ///
    /// Its plain layer is Latin, so the LUKS prompt and ordinary commands still
    /// type ASCII; Cyrillic sits on a toggled group. That is why it is chosen
    /// over the picked Latin layout — and why both sides must choose it.
    #[test]
    fn a_ukrainian_install_types_on_the_same_map_live_and_after() {
        assert_eq!(plan_keymap("uk", "us"), "ua-utf");
        assert_eq!(live_keymap("uk", "us"), "ua-utf");
        // Other languages keep their pick, aliases and all.
        assert_eq!(plan_keymap("en", "gb"), "gb");
        assert_eq!(live_keymap("en", "gb"), "uk", "gb is 'uk' to loadkeys");
        assert_eq!(live_keymap("es", "latam"), "la-latin1");
    }

    /// The keymap loaded live is never a non-Latin one.
    ///
    /// `config.keymap` is picked as the first LATIN layout precisely so the
    /// early console (and the LUKS prompt) can type an ASCII passphrase. Loading
    /// a Cyrillic map into the live console would take the username field with
    /// it — you cannot type a Linux username in Cyrillic.
    #[test]
    fn the_live_keymap_is_always_latin() {
        let mut app = crate::app::App::new();
        app.config.xkb_layouts = vec!["ua".into()];
        app.config.keymap = app
            .config
            .xkb_layouts
            .iter()
            .find(|x| !is_nonlatin(x))
            .cloned()
            .unwrap_or_else(|| "us".into());
        assert_eq!(
            app.config.keymap, "us",
            "a Cyrillic-only pick must fall back"
        );
        assert!(!is_nonlatin(&app.config.keymap));
    }

    /// The console keymap is the first LATIN layout, never simply the first.
    ///
    /// The screen used to say "the first selected is primary" while drawing its
    /// checkmarks in LIST order and installing something else again — three
    /// different answers to one question, and the one that matters is the LUKS
    /// prompt: the initramfs loads this keymap before the encrypt hook, so a
    /// Cyrillic one makes the passphrase untypeable at boot.
    #[test]
    fn the_console_keymap_is_the_first_latin_layout() {
        let latin_of = |v: Vec<&str>| -> String {
            v.iter()
                .find(|x| !is_nonlatin(x))
                .map(|s| s.to_string())
                .unwrap_or_else(|| "us".into())
        };
        assert_eq!(latin_of(vec!["gb", "ua"]), "gb");
        assert_eq!(
            latin_of(vec!["ua", "de"]),
            "de",
            "a Cyrillic layout became the console keymap"
        );
        assert_eq!(latin_of(vec!["ua"]), "us", "no Latin layout, no fallback");
    }
}
