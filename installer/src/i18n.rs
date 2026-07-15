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
        if v.to_lowercase().starts_with("uk") {
            Lang::Uk
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
        }
        .keys()
        .cloned()
        .collect()
    }

    /// Both translations define exactly the same keys. A key in one and not the
    /// other renders as the raw identifier on screen — in the language whoever
    /// wrote it probably doesn't read.
    #[test]
    fn the_two_translations_define_the_same_keys() {
        let (u, e) = (keys(Lang::Uk), keys(Lang::En));

        let missing_en: Vec<_> = u.difference(&e).collect();
        assert!(
            missing_en.is_empty(),
            "keys present in uk.toml but missing from en.toml: {missing_en:?}"
        );

        let missing_uk: Vec<_> = e.difference(&u).collect();
        assert!(
            missing_uk.is_empty(),
            "keys present in en.toml but missing from uk.toml: {missing_uk:?}"
        );
    }

    /// No translation is left as an empty string — an empty value renders as a
    /// blank label, which reads as a broken screen rather than a missing string.
    #[test]
    fn no_translation_is_empty() {
        for lang in [Lang::Uk, Lang::En] {
            let table = match lang {
                Lang::En => en(),
                Lang::Uk => uk(),
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
