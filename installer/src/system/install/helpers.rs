//! Small building blocks shared across the install plan.
//!
//! Action constructors (`act`, `chroot`, `write_target_file`, …) wrap the
//! runner's `Action` type so plan code reads like a recipe; the rest are
//! pure helpers for LUKS kernel-cmdline fragments, btrfs rootflags, extra-
//! disk planning and mountpoint/mapper naming. No global state, no I/O at
//! plan-build time — everything here just returns values.

use super::*;

pub(crate) fn act(program: &str, args: &[&str]) -> Action {
    Action {
        program: program.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        interactive: false,
    }
}

/// Like `act`, but runs under a PTY so pacman thinks it's on a terminal and
/// prints its live download progress (transfer rate, percent, the bar). The PTY
/// runner also answers any prompt (Proceed? -> Y). Used for basestrap so the
/// user can watch the base download — and gauge their connection speed.
pub(crate) fn act_interactive(program: &str, args: &[&str]) -> Action {
    Action {
        program: program.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        interactive: true,
    }
}

/// Like `act`, but the step runs on the foreground terminal so the user can
/// answer interactive prompts (used for interactive-mode basestrap).
pub(crate) fn chroot(script: &str) -> Action {
    Action {
        program: "artix-chroot".to_string(),
        args: vec!["/mnt".into(), "sh".into(), "-c".into(), script.to_string()],
        interactive: false,
    }
}

/// Like `chroot`, but runs under a PTY so the user can answer interactive
/// prompts (e.g. paru's provider-number selection for AUR dependencies).
pub(crate) fn chroot_interactive(script: &str) -> Action {
    Action {
        program: "artix-chroot".to_string(),
        args: vec!["/mnt".into(), "sh".into(), "-c".into(), script.to_string()],
        interactive: true,
    }
}

/// Escape a string for safe inclusion inside a double-quoted shell context.
/// Backslash, double-quote, dollar and backtick are the characters that retain
/// special meaning inside double quotes.
pub(crate) fn shell_escape_dq(s: &str) -> String {
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
pub(crate) fn write_home_file(home: &str, rel_path: &str, content: &str) -> Action {
    // 'EOF' is single-quoted so the body isn't expanded by the shell. We pick a
    // marker unlikely to appear in configs.
    let script =
        format!("cat > {home}/{rel_path} <<'ARTIX_INSTALLER_EOF'\n{content}\nARTIX_INSTALLER_EOF");
    chroot(&script)
}

/// Standard base64 encoder (RFC 4648). Small enough to inline so we don't pull
/// in a crate just to ship one embedded image.
pub(crate) fn base64_encode(data: &[u8]) -> String {
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
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
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
pub(crate) fn write_home_binary(home: &str, rel_path: &str, bytes: &[u8]) -> Vec<Action> {
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
pub(crate) fn write_target_file(mnt_path: &str, content: &str) -> Action {
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

/// The LUKS portion of the kernel command line for non-GRUB bootloaders.
/// Empty when not encrypting. Root itself is added at install time in the
/// chroot (resolved to a UUID dynamically), so this only carries the
/// cryptdevice mapping for root-only LUKS.
pub(crate) fn luks_cmdline_part(c: &InstallConfig) -> String {
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
pub(crate) fn rootflags_part(c: &InstallConfig) -> String {
    if c.root_fs == "btrfs" && c.btrfs_subvolumes {
        // Always pin subvol=@ — boot the root subvolume BY NAME. This is what
        // makes `artix-rollback` (which swaps the @ subvolume for a snapshot)
        // reliable: after the swap the name @ points at the restored content, so
        // a subvol=@ boot lands on it regardless of the btrfs default subvolume
        // (which proved unreliable to switch from early boot / a temp mount).
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
pub(crate) fn extra_disks_plan(c: &InstallConfig) -> Vec<Action> {
    let mut plan = Vec::new();
    for d in &c.extra_disks {
        if !d.format || d.mountpoint.is_empty() {
            continue;
        }
        // Never touch the USB key stick, whatever the config claims. The UI
        // hides it from the disk lists and retracts stale entries when a key is
        // picked — but this list is what actually drives mkfs, so it gets the
        // same guard. Formatting the key carrier would leave a key-only install
        // permanently unopenable.
        if !c.usb_key_device.is_empty()
            && (d.disk == c.usb_key_device || d.disk.starts_with(&c.usb_key_device))
        {
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
                &[
                    "-c",
                    &format!(
                        "partprobe {} 2>/dev/null; udevadm settle 2>/dev/null; sleep 1",
                        d.disk
                    ),
                ],
            ));
            p1
        } else {
            // Reformatting an existing partition: clear any old filesystem
            // signature so mkfs starts clean, but keep the partition itself.
            plan.push(act("wipefs", &["-a", &d.disk]));
            d.disk.clone()
        };

        // Clear stale signatures on the TARGET PARTITION itself before formatting
        // or encrypting it. A whole-disk wipefs above only clears the disk's own
        // signatures (its partition table), not an old magic INSIDE a freshly
        // recreated partition — e.g. a leftover crypto_LUKS header from an earlier
        // install at the partition's offset 0. mkfs.btrfs writes its superblock at
        // 64 KiB and leaves that old magic untouched, so mount/libblkid would then
        // misdetect the partition as crypto_LUKS and fail. (Harmless when there's
        // nothing to wipe, and redundant-but-safe before luksFormat, which also
        // overwrites offset 0.)
        plan.push(act(
            "sh",
            &["-c", &format!("wipefs -a {base_dev} 2>/dev/null; true")],
        ));

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
                &[
                    "-c",
                    &format!("cryptsetup -q luksFormat --key-file {keyfile} {base_dev}"),
                ],
            ));
            plan.push(act(
                "sh",
                &[
                    "-c",
                    &format!("cryptsetup open --key-file {keyfile} {base_dev} {mapper}"),
                ],
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
pub(crate) fn resolve_mp(c: &InstallConfig, mp: &str) -> String {
    if let Some(rest) = mp.strip_prefix("~/") {
        format!("/home/{}/{}", c.username, rest)
    } else {
        mp.to_string()
    }
}

/// Stable device-mapper name for an encrypted extra disk, derived from its
/// mountpoint (unique on the storage screen): "/mnt/storage" -> "crypt_mnt_storage".
pub(crate) fn crypt_mapper(mp: &str) -> String {
    let s: String = mp
        .trim_start_matches('/')
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect();
    format!(
        "crypt_{}",
        if s.is_empty() { "data".to_string() } else { s }
    )
}

/// A 64-hex-char throwaway passphrase from the kernel CSPRNG, for key-only
/// USB encryption (the user never sees or needs it). /dev/urandom can't
/// realistically fail on the live system; the fallback only exists so a
/// broken environment degrades to a weaker secret instead of a mid-install
/// panic — and that secret is still removed from the container at the end.
pub(crate) fn random_passphrase() -> String {
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
