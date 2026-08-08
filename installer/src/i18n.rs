//! Minimal i18n. Locale files are embedded into the binary via rust-embed so
//! the live ISO needs no extra files.
//!
//! IMPORTANT: our locale files use dotted keys like `de.title = "..."`. In TOML
//! a dotted key denotes a NESTED table (`[de] title = "..."`), NOT a flat
//! string key. So we parse into `toml::Value` and FLATTEN the nested tables
//! back into a flat map keyed by the dotted path ("de.title"). This is what the
//! rest of the code expects from `t(lang, "de.title")`.
//!
//! Lookup falls back to English, then to the raw key, so a missing translation
//! never panics or shows blank.

use rust_embed::RustEmbed;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(RustEmbed)]
#[folder = "i18n/"]
struct Locales;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Uk,
    Es,
}

/// Recursively flatten a toml table into "a.b.c" -> string entries.
fn flatten(prefix: &str, value: &toml::Value, out: &mut HashMap<String, String>) {
    match value {
        toml::Value::Table(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&key, v, out);
            }
        }
        toml::Value::String(s) => {
            out.insert(prefix.to_string(), s.clone());
        }
        // Numbers/bools/etc. are stringified so nothing is silently lost.
        other => {
            out.insert(prefix.to_string(), other.to_string());
        }
    }
}

fn table(code: &str) -> Option<HashMap<String, String>> {
    let file = Locales::get(&format!("{code}.toml"))?;
    let text = std::str::from_utf8(file.data.as_ref()).ok()?;
    let root: toml::Value = toml::from_str(text).ok()?;
    let mut out = HashMap::new();
    flatten("", &root, &mut out);
    Some(out)
}

fn en() -> &'static HashMap<String, String> {
    static EN: OnceLock<HashMap<String, String>> = OnceLock::new();
    EN.get_or_init(|| table("en").unwrap_or_default())
}

fn uk() -> &'static HashMap<String, String> {
    static UK: OnceLock<HashMap<String, String>> = OnceLock::new();
    UK.get_or_init(|| table("uk").unwrap_or_default())
}

/// Latin American Spanish (es-419). Contributed by ich0x.
fn es() -> &'static HashMap<String, String> {
    static ES: OnceLock<HashMap<String, String>> = OnceLock::new();
    ES.get_or_init(|| table("es").unwrap_or_default())
}

impl Lang {
    /// The language the surrounding system is set to.
    ///
    /// Used by the rollback tool, which runs on the INSTALLED system (from a
    /// terminal, or from an initramfs hook during early boot) rather than
    /// inside the installer, so there's no App to carry the choice — the
    /// environment is all there is.
    pub fn from_env() -> Self {
        let v = std::env::var("LANG")
            .or_else(|_| std::env::var("LC_MESSAGES"))
            .or_else(|_| std::env::var("LC_ALL"))
            .unwrap_or_default();
        let v = v.to_lowercase();
        if v.starts_with("uk") {
            Lang::Uk
        } else if v.starts_with("es") {
            Lang::Es
        } else {
            Lang::En
        }
    }
}

/// Translate `key` for `lang`. Falls back en -> key.
pub fn t(lang: Lang, key: &str) -> String {
    let primary = match lang {
        Lang::En => en(),
        Lang::Uk => uk(),
        Lang::Es => es(),
    };
    if let Some(v) = primary.get(key) {
        return v.clone();
    }
    if let Some(v) = en().get(key) {
        return v.clone();
    }
    key.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn keys(lang: Lang) -> HashSet<String> {
        match lang {
            Lang::En => en(),
            Lang::Uk => uk(),
            Lang::Es => es(),
        }
        .keys()
        .cloned()
        .collect()
    }

    /// Every locale file PARSES, and none of them is empty.
    ///
    /// `table()` ends in `unwrap_or_default()`, so a syntax error — a duplicate
    /// key, an unclosed quote — does not fail: it yields an EMPTY map, and every
    /// string in that language silently becomes its own identifier. The parity
    /// test above cannot catch it either, because two empty maps agree perfectly
    /// with each other.
    ///
    /// This happened while adding a key by hand: `[de]` ended up with two
    /// `footer` entries, en.toml stopped parsing, and nothing said so.
    #[test]
    fn every_locale_file_parses_and_is_not_empty() {
        for (lang, name) in [
            (Lang::Uk, "uk.toml"),
            (Lang::En, "en.toml"),
            (Lang::Es, "es.toml"),
        ] {
            let n = keys(lang).len();
            assert!(
                n > 100,
                "{name} yielded {n} keys — it almost certainly failed to parse, \
                 and every string in that language will render as its own key"
            );
        }
    }

    /// EVERY shipped translation defines exactly the same keys.
    ///
    /// A key in one file and not another renders as the raw identifier on
    /// screen — in the language whoever wrote it probably doesn't read. The
    /// check is against Ukrainian because that is where new strings are written
    /// first; it is a reference point, not a privilege.
    ///
    /// This deliberately iterates over a list rather than comparing a pair:
    /// adding Spanish to a two-language assert would have meant a test that
    /// still passed while es.toml quietly drifted.
    #[test]
    fn every_translation_defines_the_same_keys() {
        let reference = keys(Lang::Uk);
        for (lang, name) in [(Lang::En, "en.toml"), (Lang::Es, "es.toml")] {
            let other = keys(lang);
            let missing: Vec<_> = reference.difference(&other).collect();
            assert!(
                missing.is_empty(),
                "keys present in uk.toml but missing from {name}: {missing:?}"
            );
            let extra: Vec<_> = other.difference(&reference).collect();
            assert!(
                extra.is_empty(),
                "keys present in {name} but missing from uk.toml: {extra:?}"
            );
        }
    }

    /// Every key the SOURCE asks for actually exists.
    ///
    /// The parity test above only proves the two files agree — delete a key
    /// from both and it stays happily silent while the screen renders a raw
    /// identifier. That is not hypothetical: a file-wide `sed` meant for one
    /// section deleted `along.hint_disk` from both files at once, and nothing
    /// failed until it was noticed by eye.
    ///
    /// Only literal keys can be checked; a handful are built with `format!`
    /// (`disk.fsopt_{id}`) and are skipped by construction.
    #[test]
    fn every_key_the_code_asks_for_exists() {
        let defined = keys(Lang::Uk);
        let mut missing: Vec<String> = Vec::new();
        let mut stack = vec![std::path::PathBuf::from("src")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                // This module tests the lookup ITSELF, including what a missing
                // key does, so its literals are deliberately not translations.
                if path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .ends_with("src/i18n.rs")
                {
                    continue;
                }
                let Ok(src) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for line in src.lines() {
                    // Comments carry examples ("sec.key"), not real lookups.
                    if line.trim_start().starts_with("//") {
                        continue;
                    }
                    let b = line.as_bytes();
                    for (i, _) in line.match_indices("t(") {
                        // `t(` must be the whole call name, not the tail of
                        // `act(` or `format!(`.
                        if i > 0 {
                            let prev = b[i - 1] as char;
                            if prev.is_alphanumeric() || prev == '_' || prev == '!' {
                                continue;
                            }
                        }
                        let rest = &line[i + 2..];
                        let Some(q1) = rest.find('"') else { continue };
                        // The key is the SECOND argument, so a comma separates
                        // it from the language — that alone rejects `act("…")`.
                        if !rest[..q1].contains(',') {
                            continue;
                        }
                        let after = &rest[q1 + 1..];
                        let Some(q2) = after.find('"') else { continue };
                        let key = &after[..q2];
                        if !key.contains('.') || key.contains('{') || key.contains(' ') {
                            continue;
                        }
                        if !defined.contains(key) && !missing.contains(&key.to_string()) {
                            missing.push(key.to_string());
                        }
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "the code asks for keys that no translation defines: {missing:?}"
        );
    }

    /// No translation is left as an empty string — an empty value renders as a
    /// blank label, which reads as a broken screen rather than a missing string.
    #[test]
    fn no_translation_is_empty() {
        for lang in [Lang::Uk, Lang::En, Lang::Es] {
            let table = match lang {
                Lang::En => en(),
                Lang::Uk => uk(),
                Lang::Es => es(),
            };
            for (k, v) in table {
                assert!(!v.trim().is_empty(), "{lang:?}: '{k}' is empty");
            }
        }
    }

    /// A missing key falls back to the key itself rather than panicking — but
    /// that fallback must stay a LAST resort, not a habit. See the
    /// no_hardcoded_ui_strings test in main.rs.
    #[test]
    fn a_missing_key_falls_back_to_the_key() {
        assert_eq!(t(Lang::Uk, "no.such.key"), "no.such.key");
    }
}
