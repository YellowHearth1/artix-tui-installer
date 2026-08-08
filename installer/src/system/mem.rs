//! Memory tuning for machines that do not have much of it.
//!
//! Two knobs, both aimed at the same case: an old laptop with 2–4 GB where the
//! choice is between swapping to a slow disk and freezing solid.
//!
//! * **zswap** — a compressed cache in front of the swap device. Pages are
//!   compressed in RAM first and only written out when that pool fills, so a
//!   spinning disk is touched far less often.
//! * **earlyoom** — watches free memory and kills the biggest offender BEFORE
//!   the kernel's own OOM killer gets involved. The kernel waits until the
//!   machine is already thrashing, which on slow storage means minutes of a
//!   completely unresponsive system; earlyoom acts while there is still time.
//!
//! Both are configured in PERCENTAGES rather than megabytes. That was the
//! request, and it is the right unit: the same number then means the same thing
//! on a 2 GB netbook and a 32 GB desktop, so a chosen setting survives moving to
//! another machine.

/// Compressors zswap can be asked for, best-first.
///
/// zstd leads because it compresses far better than lzo at a similar speed on
/// anything modern; lzo-rle is the fallback on hardware old enough that zstd's
/// extra CPU cost is felt.
pub(crate) const COMPRESSORS: &[&str] = &["zstd", "lzo-rle", "lzo", "lz4", "deflate"];

/// Compression algorithms this kernel currently reports, from `/proc/crypto`.
///
/// Kept as a pure parse so it can be tested against real file contents.
///
/// Three type names count, not one: `scomp` and `compression` are the
/// synchronous ones, and **`acomp`** is asynchronous — which is where `zstd`
/// appears. Filtering on `scomp` alone drops the single best choice while
/// leaving the weaker ones visible, and the list looks plausible enough that
/// nothing seems wrong. Verified against a running kernel: `zstd` was in use by
/// zswap at the time and listed as `acomp`.
///
/// The list is what is REGISTERED, which is not the same as what the kernel can
/// do — crypto modules register lazily, so an algorithm nobody has asked for yet
/// is absent. That is why this only marks the offered choices as confirmed
/// rather than deciding which ones exist.
pub(crate) fn compressors_in(proc_crypto: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut name = String::new();
    for line in proc_crypto.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "name" => name = value.to_string(),
            "type" if !name.is_empty() && matches!(value, "compression" | "scomp" | "acomp") => {
                out.push(name.clone())
            }
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The same, read off the live system. Empty when it cannot be read, which
/// simply means nothing is marked as confirmed.
pub(crate) fn available_compressors() -> Vec<String> {
    std::fs::read_to_string("/proc/crypto")
        .map(|s| compressors_in(&s))
        .unwrap_or_default()
}

/// The kernel command line zswap needs, or empty when it is off.
///
/// Set on the kernel command line rather than in a module parameter file because
/// zswap has to be configured before userspace exists — it is built into the
/// kernel, not a module, so there is nothing to modprobe.
pub(crate) fn zswap_cmdline(enabled: bool, compressor: &str, percent: u8) -> String {
    if !enabled {
        return String::new();
    }
    let pct = percent.clamp(5, 50);
    format!("zswap.enabled=1 zswap.compressor={compressor} zswap.max_pool_percent={pct} ")
}

/// The dinit service that runs earlyoom.
///
/// Written by hand because the package ships a systemd unit and nothing else —
/// this distro has no systemd, so without this the package installs and never
/// runs.
///
/// `-m` is the free-memory percentage at which it starts killing; `-s` the same
/// for swap. `-r 0` silences the periodic report, which otherwise fills the log
/// on a machine that is short of memory anyway.
pub(crate) fn earlyoom_service(percent: u8) -> String {
    let pct = percent.clamp(2, 30);
    format!(
        "# Managed by the Artix installer.\n\
         #\n\
         # Kills the largest memory hog while the machine can still respond. The\n\
         # kernel's own OOM killer waits until it is already thrashing, which on\n\
         # a slow disk means minutes of a frozen desktop.\n\
         type = process\n\
         command = /usr/bin/earlyoom -m {pct} -s {pct} -r 0\n\
         restart = true\n\
         depends-on = local.target\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `zstd` is an `acomp`, and missing it is the whole trap.
    ///
    /// Taken verbatim from a running kernel: zswap was using zstd at that
    /// moment, and a filter that only accepted `scomp` reported lzo and lzo-rle
    /// while silently dropping the one compressor actually in use.
    #[test]
    fn zstd_is_found_even_though_it_is_an_async_compressor() {
        let sample = "\
name         : zstd
driver       : zstd-generic
module       : kernel
priority     : 0
refcnt       : 9
selftest     : passed
internal     : no
type         : acomp

name         : lzo-rle
driver       : lzo-rle-scomp
module       : kernel
type         : scomp

name         : cbc(aes)
driver       : cbc-aes-aesni
module       : kernel
type         : skcipher
";
        let got = compressors_in(sample);
        assert!(
            got.contains(&"zstd".to_string()),
            "zstd was dropped: {got:?}"
        );
        assert!(got.contains(&"lzo-rle".to_string()));
        assert!(
            !got.contains(&"cbc(aes)".to_string()),
            "a cipher was listed as a compressor: {got:?}"
        );
    }

    /// Off means no kernel parameters at all — not `zswap.enabled=0`, which
    /// would be a promise we do not need to make.
    #[test]
    fn zswap_off_adds_nothing_to_the_command_line() {
        assert_eq!(zswap_cmdline(false, "zstd", 20), "");
        let on = zswap_cmdline(true, "zstd", 20);
        assert!(on.contains("zswap.enabled=1"));
        assert!(on.contains("zswap.compressor=zstd"));
        assert!(on.contains("zswap.max_pool_percent=20"));
        assert!(on.ends_with(' '), "fragments must concatenate cleanly");
    }

    /// Percentages are clamped, because a pool of 0% does nothing and a pool of
    /// 90% is a way to run out of the memory it was meant to save.
    #[test]
    fn percentages_are_kept_inside_something_sensible() {
        assert!(zswap_cmdline(true, "zstd", 0).contains("max_pool_percent=5"));
        assert!(zswap_cmdline(true, "zstd", 99).contains("max_pool_percent=50"));
        assert!(earlyoom_service(0).contains("-m 2 -s 2"));
        assert!(earlyoom_service(90).contains("-m 30 -s 30"));
    }
}
