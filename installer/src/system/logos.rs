//! Embedded Artix + pride fastfetch logos — the "slayfetch" option's icon set.
//!
//! Each PNG in `assets/ArtixFastFetchLogo/` is the Artix logo with a different
//! LGBTQ+ flag behind it. "slayfetch" installs the ordinary `fastfetch` package
//! but drops the user-chosen logo in place of the default one, so it is purely
//! a cosmetic choice over the same tool.
//!
//! Filenames are `ArtixFastFetch<Variant>.png`; the variant (e.g. "Rainbow",
//! "Transgender") is what the picker shows and what `config.fastfetch_logo`
//! stores. Everything is derived from the embedded files, so adding a PNG to the
//! folder adds a choice with no code change.

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "src/assets/ArtixFastFetchLogo/"]
struct LogoAssets;

const PREFIX: &str = "ArtixFastFetch";
const SUFFIX: &str = ".png";

/// The default variant picked when slayfetch is first selected — the full
/// rainbow, the most recognisable of the set. Falls back to whatever sorts
/// first if this file is ever removed.
pub const DEFAULT_VARIANT: &str = "Rainbow";

/// All variant names, in the order the picker shows them: the default
/// (Rainbow — the most recognisable pride flag) pinned FIRST, then the rest
/// alphabetically. So the picker opens on Rainbow and reads A→Z after it.
pub fn variants() -> Vec<String> {
    let mut v: Vec<String> = LogoAssets::iter()
        .filter_map(|f| {
            f.as_ref()
                .strip_prefix(PREFIX)?
                .strip_suffix(SUFFIX)
                .map(str::to_string)
        })
        .collect();
    v.sort();
    // Pin the default to the top (leaving the rest alphabetical).
    if let Some(pos) = v.iter().position(|x| x == DEFAULT_VARIANT) {
        let d = v.remove(pos);
        v.insert(0, d);
    }
    v
}

/// The PNG bytes for a variant, or None if the name doesn't match a file.
pub fn bytes(variant: &str) -> Option<Vec<u8>> {
    LogoAssets::get(&format!("{PREFIX}{variant}{SUFFIX}")).map(|f| f.data.into_owned())
}

/// The flag's stripe colours (top→bottom) for a variant, as RGB. Used to paint
/// the variant's NAME in the picker in its own flag colours. Raw hex from the
/// flags; the UI lifts too-dark ones so they read on the dark background. An
/// unknown name gets a neutral off-white so it still renders. Rarer flags are
/// close approximations — the images themselves are the accurate reference.
pub fn flag_palette(variant: &str) -> &'static [(u8, u8, u8)] {
    match variant {
        "Rainbow" => &[
            (0xE4, 0x03, 0x03),
            (0xFF, 0x8C, 0x00),
            (0xFF, 0xED, 0x00),
            (0x00, 0x80, 0x26),
            (0x00, 0x4D, 0xFF),
            (0x75, 0x07, 0x87),
        ],
        "ProgressPride" => &[
            (0xE4, 0x03, 0x03),
            (0xFF, 0x8C, 0x00),
            (0xFF, 0xED, 0x00),
            (0x00, 0x80, 0x26),
            (0x00, 0x4D, 0xFF),
            (0x75, 0x07, 0x87),
            (0xFF, 0xFF, 0xFF),
            (0xF5, 0xA9, 0xB8),
            (0x5B, 0xCE, 0xFA),
            (0x61, 0x39, 0x15),
            (0x00, 0x00, 0x00),
        ],
        "Transgender" => &[
            (0x5B, 0xCE, 0xFA),
            (0xF5, 0xA9, 0xB8),
            (0xFF, 0xFF, 0xFF),
            (0xF5, 0xA9, 0xB8),
            (0x5B, 0xCE, 0xFA),
        ],
        "Bisexual" => &[(0xD6, 0x02, 0x70), (0x9B, 0x4F, 0x96), (0x00, 0x38, 0xA8)],
        "Pansexual" => &[(0xFF, 0x21, 0x8C), (0xFF, 0xD8, 0x00), (0x21, 0xB1, 0xFF)],
        "Lesbian" => &[
            (0xD5, 0x2D, 0x00),
            (0xEF, 0x76, 0x27),
            (0xFF, 0x9A, 0x56),
            (0xFF, 0xFF, 0xFF),
            (0xD3, 0x62, 0xA4),
            (0xB5, 0x56, 0x90),
            (0xA3, 0x02, 0x62),
        ],
        "Gay" => &[
            (0x07, 0x8D, 0x70),
            (0x26, 0xCE, 0xAA),
            (0x98, 0xE8, 0xC1),
            (0xFF, 0xFF, 0xFF),
            (0x7B, 0xAD, 0xE2),
            (0x50, 0x49, 0xCC),
            (0x3D, 0x1A, 0x78),
        ],
        "Asexual" => &[
            (0x00, 0x00, 0x00),
            (0xA3, 0xA3, 0xA3),
            (0xFF, 0xFF, 0xFF),
            (0x80, 0x00, 0x80),
        ],
        "Aromantic" => &[
            (0x3D, 0xA5, 0x42),
            (0xA7, 0xD3, 0x79),
            (0xFF, 0xFF, 0xFF),
            (0xA9, 0xA9, 0xA9),
            (0x00, 0x00, 0x00),
        ],
        "Aroace" => &[
            (0xE2, 0x8C, 0x00),
            (0xEC, 0xCD, 0x00),
            (0xFF, 0xFF, 0xFF),
            (0x62, 0xAE, 0xDC),
            (0x20, 0x38, 0x56),
        ],
        "NonBinary" => &[
            (0xFC, 0xF4, 0x34),
            (0xFF, 0xFF, 0xFF),
            (0x9C, 0x59, 0xD1),
            (0x2C, 0x2C, 0x2C),
        ],
        "Genderfluid" => &[
            (0xFF, 0x75, 0xA2),
            (0xFF, 0xFF, 0xFF),
            (0xBE, 0x18, 0xD6),
            (0x00, 0x00, 0x00),
            (0x33, 0x3E, 0xBD),
        ],
        "Genderqueer" => &[(0xB5, 0x7E, 0xDC), (0xFF, 0xFF, 0xFF), (0x4A, 0x81, 0x23)],
        "Agender" => &[
            (0x00, 0x00, 0x00),
            (0xB9, 0xB9, 0xB9),
            (0xFF, 0xFF, 0xFF),
            (0xB8, 0xF4, 0x83),
            (0xFF, 0xFF, 0xFF),
            (0xB9, 0xB9, 0xB9),
            (0x00, 0x00, 0x00),
        ],
        "Bigender" => &[
            (0xC4, 0x79, 0xA2),
            (0xEC, 0xA6, 0xCB),
            (0xD6, 0xC7, 0xE8),
            (0xFF, 0xFF, 0xFF),
            (0x9A, 0xC7, 0xE8),
            (0x6D, 0x82, 0xD1),
        ],
        "Demiboy" => &[
            (0x7F, 0x7F, 0x7F),
            (0xC4, 0xC4, 0xC4),
            (0x9D, 0xD7, 0xEA),
            (0xFF, 0xFF, 0xFF),
            (0x9D, 0xD7, 0xEA),
            (0xC4, 0xC4, 0xC4),
            (0x7F, 0x7F, 0x7F),
        ],
        "Demigirl" => &[
            (0x7F, 0x7F, 0x7F),
            (0xC4, 0xC4, 0xC4),
            (0xFD, 0xAD, 0xC8),
            (0xFF, 0xFF, 0xFF),
            (0xFD, 0xAD, 0xC8),
            (0xC4, 0xC4, 0xC4),
            (0x7F, 0x7F, 0x7F),
        ],
        "Demiromantic" => &[
            (0x7F, 0x7F, 0x7F),
            (0xC4, 0xC4, 0xC4),
            (0x3D, 0xA5, 0x42),
            (0xFF, 0xFF, 0xFF),
            (0x3D, 0xA5, 0x42),
            (0xC4, 0xC4, 0xC4),
            (0x7F, 0x7F, 0x7F),
        ],
        "Demisexual" => &[
            (0x00, 0x00, 0x00),
            (0xFF, 0xFF, 0xFF),
            (0x6E, 0x00, 0x71),
            (0xD2, 0xD2, 0xD2),
        ],
        "Abrosexual" => &[
            (0x75, 0xC6, 0x9B),
            (0xB0, 0xE5, 0xCD),
            (0xFF, 0xFF, 0xFF),
            (0xE9, 0x99, 0xB4),
            (0xD9, 0x4A, 0x81),
        ],
        "Androgyne" => &[(0xFE, 0x00, 0x7F), (0x98, 0x32, 0xFF), (0x00, 0xB8, 0xE7)],
        "Graysexual" => &[
            (0x74, 0x01, 0x94),
            (0xA4, 0xA4, 0xA4),
            (0xFF, 0xFF, 0xFF),
            (0xA4, 0xA4, 0xA4),
            (0x74, 0x01, 0x94),
        ],
        "Omnisexual" => &[
            (0xFE, 0x9A, 0xCE),
            (0xFF, 0x53, 0xBF),
            (0x20, 0x00, 0x53),
            (0x67, 0x60, 0xFE),
            (0x8E, 0xA6, 0xFF),
        ],
        "Polysexual" => &[(0xF6, 0x1C, 0xB9), (0x07, 0xD5, 0x69), (0x1C, 0x92, 0xF6)],
        "Polyamory" => &[
            (0x1C, 0x5F, 0xE0),
            (0xE0, 0x12, 0x2E),
            (0xF5, 0xC5, 0x18),
            (0x00, 0x00, 0x00),
        ],
        "Intersex" => &[(0xFF, 0xD8, 0x00), (0x79, 0x02, 0xAA)],
        "IntersexInclusive" => &[
            (0xFF, 0xD8, 0x00),
            (0x79, 0x02, 0xAA),
            (0x5B, 0xCE, 0xFA),
            (0xF5, 0xA9, 0xB8),
            (0xFF, 0xFF, 0xFF),
            (0x61, 0x39, 0x15),
            (0x00, 0x00, 0x00),
        ],
        "Leather" => &[
            (0x00, 0x00, 0x00),
            (0x1E, 0x1E, 0xE6),
            (0xFF, 0xFF, 0xFF),
            (0xE6, 0x00, 0x00),
        ],
        "Toric" => &[
            (0x5B, 0xCE, 0xFA),
            (0xAA, 0xF0, 0xD1),
            (0xFF, 0xFF, 0xFF),
            (0xF6, 0xA4, 0xC8),
            (0xB5, 0x7E, 0xDC),
        ],
        "Trixic" => &[(0xF6, 0xCF, 0x43), (0xFF, 0xFF, 0xFF), (0xF4, 0x9A, 0xC2)],
        // Unknown → a single neutral off-white so the name still renders.
        _ => &[(0xC0, 0xCA, 0xF5)],
    }
}

/// The variant to actually use for a config value: the stored choice if it
/// still exists, else DEFAULT_VARIANT, else the first available — so slayfetch
/// always resolves to a real logo.
pub fn resolve(chosen: &str) -> Option<String> {
    let all = variants();
    if all.iter().any(|v| v == chosen) {
        return Some(chosen.to_string());
    }
    if all.iter().any(|v| v == DEFAULT_VARIANT) {
        return Some(DEFAULT_VARIANT.to_string());
    }
    all.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The logo set must be embedded and non-empty, and the default must exist —
    /// otherwise slayfetch has nothing to install.
    #[test]
    fn logos_are_embedded_and_have_a_default() {
        let v = variants();
        assert!(!v.is_empty(), "no fastfetch logos were embedded");
        assert!(
            v.iter().any(|x| x == DEFAULT_VARIANT),
            "the default variant {DEFAULT_VARIANT} must be present"
        );
        // Every listed variant must resolve to real bytes.
        for name in &v {
            assert!(bytes(name).is_some(), "{name} has no PNG bytes");
        }
        // Rainbow leads the list (the picker opens on it); the rest are A→Z.
        assert_eq!(v[0], DEFAULT_VARIANT, "the default must be pinned first");
        assert!(
            v[1..].windows(2).all(|w| w[0] <= w[1]),
            "after the pinned default, variants must be alphabetical"
        );
    }

    /// Every embedded variant must have its OWN flag palette (not the neutral
    /// fallback), or its name is misspelled in `flag_palette`.
    #[test]
    fn every_variant_has_its_own_palette() {
        let fallback = flag_palette("__definitely_not_a_flag__");
        for name in variants() {
            let p = flag_palette(&name);
            assert!(!p.is_empty(), "{name} has an empty palette");
            assert!(
                !std::ptr::eq(p.as_ptr(), fallback.as_ptr()),
                "{name} fell through to the neutral fallback — misspelled in flag_palette()"
            );
        }
    }

    /// resolve() always yields a usable variant.
    #[test]
    fn resolve_always_finds_a_logo() {
        assert_eq!(resolve("Rainbow").as_deref(), Some("Rainbow"));
        // An unknown name falls back to the default.
        assert_eq!(resolve("Nonexistent").as_deref(), Some(DEFAULT_VARIANT));
    }
}
