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

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Uk => "uk",
        }
    }
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
