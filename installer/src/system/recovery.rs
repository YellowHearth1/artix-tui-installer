//! Recovery backend. Runs LIVE commands (not through the install plan): scans
//! partitions, optionally unlocks a LUKS root (passphrase or USB key), mounts
//! the system under /mnt (root + boot/EFI from its fstab), and detects the
//! installed bootloader. The screen then launches `artix-chroot /mnt` so the
//! user can repair the system by hand.

use crate::app::App;
use crate::system::runner::capture;

/// A block-device partition as shown in the recovery picker.
#[derive(Debug, Clone)]
pub struct Partition {
    pub path: String,   // /dev/sda2, /dev/nvme0n1p2
    pub size: String,   // human, e.g. "200G"
    pub fstype: String, // ext4 / btrfs / crypto_LUKS / vfat / ""
    pub label: String,  // filesystem / partition label, may be empty
}

/// Enumerate partitions (TYPE=part) via lsblk's stable JSON output. We avoid a
/// serde struct here (keeps the dependency surface small) and parse the flat
/// fields we need with a tiny tolerant scan — lsblk's JSON is line-oriented.
pub fn list_partitions() -> Result<Vec<Partition>, String> {
    let json = capture(
        "lsblk",
        &["--json", "-o", "NAME,SIZE,TYPE,FSTYPE,LABEL", "-p"],
    )?;
    // Each device object is on its own region; we pull NAME/SIZE/FSTYPE/LABEL
    // per object. lsblk -p gives full /dev paths in NAME. We only keep TYPE=part
    // and TYPE=crypt (an already-open LUKS mapping is a valid mount source too).
    let mut out = Vec::new();
    // Split into per-object chunks on '{' so each chunk has one device's fields.
    for chunk in json.split('{').skip(1) {
        let name = json_field(chunk, "name");
        let size = json_field(chunk, "size");
        let typ = json_field(chunk, "type");
        let fstype = json_field(chunk, "fstype");
        let label = json_field(chunk, "label");
        if name.is_empty() {
            continue;
        }
        if typ == "part" || typ == "crypt" {
            out.push(Partition {
                path: name,
                size: if size.is_empty() { "?".into() } else { size },
                fstype,
                label,
            });
        }
    }
    Ok(out)
}

/// Pull a `"key": "value"` string field out of an lsblk JSON chunk. Returns ""
/// if absent or null. Tolerant of spacing; values here never contain quotes.
fn json_field(chunk: &str, key: &str) -> String {
    let pat = format!("\"{key}\":");
    let Some(i) = chunk.find(&pat) else {
        return String::new();
    };
    let rest = &chunk[i + pat.len()..];
    let rest = rest.trim_start();
    if rest.starts_with("null") {
        return String::new();
    }
    if let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        if let Some(end) = after.find('"') {
            return after[..end].to_string();
        }
    }
    String::new()
}

/// Mount the selected system under /mnt and detect its bootloader, writing a
/// human-readable summary into `app.recovery_status`. On success sets
/// `app.recovery_mounted = true` so the next Enter opens the chroot shell.
///
/// Steps:
///   1) resolve the chosen partition (and unlock it if it's LUKS),
///   2) mount the (decrypted) root at /mnt,
///   3) mount /mnt/boot and the EFI partition if they exist as separate
///      filesystems (read from the mounted root's /etc/fstab),
///   4) bind /dev /proc /sys /run so the chroot works,
///   5) detect grub / refind / limine / systemd-boot.
pub fn mount_and_detect(app: &mut App, parts: &[Partition]) {
    let Some(part) = parts.get(app.recovery_disk_cursor) else {
        app.recovery_status = "No partition selected.".into();
        return;
    };

    // 1) Determine the root source device, unlocking LUKS if requested.
    let root_dev = if app.recovery_unlock == 0 {
        // Unencrypted: mount the partition directly.
        part.path.clone()
    } else {
        // LUKS: open it as a mapper device. Use the same name the installer
        // uses ("cryptroot") so the mounted system's crypttab/fstab line up and
        // a chroot won't try to re-open it under a different name.
        let mapper = "cryptroot";
        let open_res = match app.recovery_unlock {
            1 => {
                // Passphrase via stdin.
                if app.recovery_passphrase.is_empty() {
                    app.recovery_status = "Enter the LUKS passphrase first.".into();
                    return;
                }
                run_shell(&format!(
                    "printf '%s' {pass} | cryptsetup open {dev} {mapper} -",
                    pass = shquote(&app.recovery_passphrase),
                    dev = shquote(&part.path),
                ))
            }
            _ => {
                // USB key: find the key file on the ARTIXKEY stick and use it.
                // Mirrors how the installer provisions the stick (FAT32 label
                // ARTIXKEY, key at artix-luks.key).
                run_shell(&format!(
                    "set -e; mkdir -p /run/recovery-key; \
                     keydev=$(blkid -t LABEL=ARTIXKEY -o device | head -n1); \
                     if [ -z \"$keydev\" ]; then echo 'no ARTIXKEY stick found' >&2; exit 1; fi; \
                     mount -t vfat \"$keydev\" /run/recovery-key; \
                     cryptsetup open {dev} {mapper} \
                       --key-file /run/recovery-key/artix-luks.key; \
                     umount /run/recovery-key",
                    dev = shquote(&part.path),
                ))
            }
        };
        if let Err(e) = open_res {
            app.recovery_status = format!("Unlock failed: {e}");
            return;
        }
        format!("/dev/mapper/{mapper}")
    };

    // 2) Mount the root filesystem at /mnt.
    if let Err(e) = run_shell(&format!(
        "mkdir -p /mnt && mount {} /mnt",
        shquote(&root_dev)
    )) {
        app.recovery_status = format!("Mounting root failed: {e}");
        return;
    }

    // 2b) Full-disk-encryption case: the installer puts an encrypted /boot on
    //     its own LUKS (mapper "cryptboot") and records it in the target's
    //     /etc/crypttab, unlocked by a keyfile that lives on the now-mounted
    //     root (e.g. /etc/luks/boot.key). Replay crypttab here so /boot can be
    //     mounted from fstab below. Harmless no-op for unencrypted or
    //     root-only-encrypted systems (no crypttab, or nothing left to open).
    let _ = run_shell(
        "if [ -f /mnt/etc/crypttab ]; then \
           while read -r name dev keyfile _rest; do \
             case \"$name\" in ''|\\#*) continue;; esac; \
             [ \"$name\" = cryptroot ] && continue; \
             [ -e \"/dev/mapper/$name\" ] && continue; \
             case \"$dev\" in UUID=*) dev=\"/dev/disk/by-uuid/${dev#UUID=}\";; \
                              PARTLABEL=*) dev=\"/dev/disk/by-partlabel/${dev#PARTLABEL=}\";; \
                              LABEL=*) dev=\"/dev/disk/by-label/${dev#LABEL=}\";; esac; \
             case \"$keyfile\" in \
               /*) kf=\"/mnt$keyfile\";; \
               ''|none|-) kf='';; \
               *) kf=\"/mnt/$keyfile\";; esac; \
             if [ -n \"$kf\" ] && [ -f \"$kf\" ]; then \
               cryptsetup open \"$dev\" \"$name\" --key-file \"$kf\" 2>/dev/null || true; \
             fi; \
           done < /mnt/etc/crypttab; \
         fi",
    );

    // 3) Mount everything else the system expects, by reading its own fstab.
    //    mount --all with --target-prefix handles /boot, EFI, /home, etc. as
    //    declared — falling back silently if the util-linux is older.
    let _ = run_shell(
        "if [ -f /mnt/etc/fstab ]; then \
           mount --fstab /mnt/etc/fstab --target-prefix /mnt --all 2>/dev/null || \
           ( awk '!/^#/ && $2 ~ /^\\/(boot|efi|home)/ {print $2}' /mnt/etc/fstab | \
             while read m; do mount --fstab /mnt/etc/fstab \"$m\" 2>/dev/null || true; done ); \
         fi",
    );

    // 4) Bind the kernel virtual filesystems so chroot tools work. artix-chroot
    //    normally does this itself, but doing it here means /boot etc. are
    //    already in place and a plain chroot would also work.
    let _ = run_shell(
        "for d in dev proc sys run; do \
           mkdir -p /mnt/$d; \
           mountpoint -q /mnt/$d || mount --rbind /$d /mnt/$d; \
         done",
    );

    // 5) Detect the bootloader from files present in the mounted system.
    let boot = detect_bootloader();

    app.recovery_mounted = true;
    app.recovery_status = format!(
        "Mounted {root} at /mnt, plus /boot, EFI and anything else from its \
         fstab (an encrypted /boot is unlocked automatically if present).\n\
         Detected bootloader: {boot}.\n\n\
         Press Enter to open a root chroot shell. Common repairs:\n\
           reinstall bootloader  e.g. grub-install … && grub-mkconfig -o /boot/grub/grub.cfg\n\
           rebuild initramfs     mkinitcpio -P\n\
           reset a password      passwd <user>\n\
           act as the user       su - <user>\n\
         Type 'exit' to leave — everything is then unmounted and re-locked.",
        root = root_dev,
        boot = boot,
    );
}

/// Inspect the mounted system for the bootloader in use.
fn detect_bootloader() -> String {
    // Order matters: a system can have leftover dirs, so report the most
    // specific signal first. These checks are cheap file-existence tests.
    let checks = [
        ("/mnt/boot/grub/grub.cfg", "GRUB"),
        ("/mnt/boot/grub", "GRUB"),
        ("/mnt/boot/EFI/refind/refind.conf", "rEFInd"),
        ("/mnt/boot/refind_linux.conf", "rEFInd"),
        ("/mnt/boot/limine.conf", "Limine"),
        ("/mnt/boot/EFI/limine/limine.conf", "Limine"),
        ("/mnt/boot/limine/limine.conf", "Limine"),
        ("/mnt/boot/loader/loader.conf", "systemd-boot"),
    ];
    for (path, name) in checks {
        if std::path::Path::new(path).exists() {
            return name.to_string();
        }
    }
    "unknown (no grub/refind/limine/systemd-boot signature found)".to_string()
}

/// Unmount everything recovery mounted, in reverse, and close the LUKS mapping.
/// Best-effort: called after the chroot shell exits. Never panics.
pub fn cleanup() {
    let _ = run_shell(
        "umount -R /mnt 2>/dev/null || true; \
         cryptsetup close cryptboot 2>/dev/null || true; \
         cryptsetup close cryptroot 2>/dev/null || true",
    );
}

/// Run a /bin/sh -c script, returning Ok(()) on exit 0 or the captured output
/// as the error message otherwise.
fn run_shell(script: &str) -> Result<(), String> {
    capture("sh", &["-c", script]).map(|_| ())
}

/// Single-quote a string for safe use inside a /bin/sh command.
fn shquote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
