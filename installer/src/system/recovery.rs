//! Recovery backend. Runs LIVE commands (not through the install plan): scans
//! partitions, optionally unlocks a LUKS root (passphrase or USB key), mounts
//! the system under /mnt (root + boot/EFI from its fstab), and detects the
//! installed bootloader. The screen then launches `artix-chroot /mnt` so the
//! user can repair the system by hand.

use crate::app::App;
use crate::i18n::{t, Lang};
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
/// WHAT A PARTITION IS, according to the person in front of the screen.
///
/// Recovery used to ask exactly one question — "which of these is the root?" —
/// and work everything else out from the system's own /etc/fstab. That is the
/// file MOST LIKELY TO BE MISSING OR WRONG, because a broken fstab is one of the
/// commonest reasons to be in recovery at all. When it was wrong, recovery
/// mounted half a system and the diagnosis then reported confidently on things
/// it could not see: no kernel, no bootloader, none of it true.
///
/// So the answer comes from the person instead. Each partition carries a role,
/// pre-filled with a suggestion from what it LOOKS like, and every suggestion
/// can be overridden. Automation you can argue with beats automation you cannot.
pub const ROLE_NONE: usize = 0;
pub const ROLE_ROOT: usize = 1;
pub const ROLE_ESP: usize = 2;
pub const ROLE_BOOT: usize = 3;
pub const ROLE_SWAP: usize = 4;
pub const ROLE_HOME: usize = 5;
pub const ROLE_DATA: usize = 6;
pub const ROLE_KEY: usize = 7;

/// i18n key suffixes, indexed by the constants above (`rec.role_none`, …).
pub const ROLE_KEYS: [&str; 8] = ["none", "root", "esp", "boot", "swap", "home", "data", "key"];

/// Preset mount points offered for `ROLE_DATA`, cycled with `m`. These are the
/// same paths the installer offers when it mounts an extra disk, so a partition
/// set up there is found again here under the name it was given.
pub const DATA_PATHS: [&str; 4] = ["/mnt/data", "/mnt/storage", "/data", "/srv"];

/// An lsblk SIZE ("20G", "512M", "1.5G", "9,5G") as whole MiB. Unparseable
/// sizes come back as 0, which every caller treats as "cannot tell" — a
/// partition of unknown size gets the full role list rather than a guess.
pub fn size_mib(size: &str) -> u64 {
    let s = size.trim().replace(',', ".");
    let (num, mult) = match s.chars().last() {
        Some('K') => (&s[..s.len() - 1], 1.0 / 1024.0),
        Some('M') => (&s[..s.len() - 1], 1.0),
        Some('G') => (&s[..s.len() - 1], 1024.0),
        Some('T') => (&s[..s.len() - 1], 1024.0 * 1024.0),
        Some(c) if c.is_ascii_digit() => (&s[..], 1.0 / (1024.0 * 1024.0)),
        _ => return 0,
    };
    num.parse::<f64>().map(|n| (n * mult) as u64).unwrap_or(0)
}

/// Which roles make sense for this partition, in the order `←`/`→` walks them.
///
/// The size bracket is the useful signal and the one the user named: anything
/// between 100 MiB and 2 GiB is a boot partition, an ESP or a key stick, and is
/// certainly not somebody's root. Offering "root" there is not merely useless,
/// it invites the mis-selection that this screen then has to explain.
pub fn roles_for(p: &Partition) -> Vec<usize> {
    let mib = size_mib(&p.size);
    let small = (100..=2048).contains(&mib);
    if p.fstype == "swap" {
        return vec![ROLE_NONE, ROLE_SWAP, ROLE_DATA];
    }
    if small {
        // vfat first on an ESP, ext* first on a plain /boot — the likelier
        // answer sits where the cursor already is.
        let mut v = if p.fstype == "vfat" || p.fstype == "exfat" {
            vec![ROLE_NONE, ROLE_ESP, ROLE_BOOT, ROLE_KEY]
        } else {
            vec![ROLE_NONE, ROLE_BOOT, ROLE_ESP, ROLE_KEY]
        };
        v.push(ROLE_DATA);
        return v;
    }
    vec![ROLE_NONE, ROLE_ROOT, ROLE_HOME, ROLE_DATA, ROLE_SWAP]
}

/// The role to start on. `root_taken` keeps a second big partition from also
/// being proposed as the root: the first one wins, the rest fall back to
/// "leave alone" rather than quietly competing for /mnt.
pub fn suggest_role(p: &Partition, root_taken: bool) -> usize {
    let mib = size_mib(&p.size);
    if p.fstype == "swap" {
        return ROLE_SWAP;
    }
    if (100..=2048).contains(&mib) {
        // A key stick is vfat too, so this cannot be told apart by size alone.
        // ESP is the overwhelmingly likelier answer and the wrong guess costs
        // one keypress, so it is the one offered.
        if p.label == "ARTIXKEY" {
            return ROLE_KEY;
        }
        return if p.fstype == "vfat" || p.fstype == "exfat" {
            ROLE_ESP
        } else {
            ROLE_BOOT
        };
    }
    let rootish = matches!(
        p.fstype.as_str(),
        "btrfs" | "ext4" | "ext3" | "ext2" | "xfs" | "f2fs" | "crypto_LUKS"
    );
    if rootish && !root_taken {
        ROLE_ROOT
    } else if p.label.eq_ignore_ascii_case("home") {
        ROLE_HOME
    } else {
        ROLE_NONE
    }
}

/// Where a role gets mounted, under the already-mounted root. `None` means the
/// role is not a mount at all (the root itself, swap, a key stick).
pub fn mount_point(role: usize, data_path: &str) -> Option<String> {
    match role {
        ROLE_ESP => Some("/boot/efi".into()),
        ROLE_BOOT => Some("/boot".into()),
        ROLE_HOME => Some("/home".into()),
        ROLE_DATA => Some(data_path.to_string()),
        _ => None,
    }
}

/// What the install recorded about itself, read from the ESP BEFORE anything is
/// unlocked or mounted.
///
/// The copy in /etc is inside the root filesystem, so on an encrypted system it
/// can only be read after everything is already mounted — which is after the
/// point where it would have helped. The ESP is vfat and unencrypted, so this
/// copy can be read while the partition list is still just a list, and the
/// roles can be filled in from fact instead of from a guess about sizes.
///
/// Returns the mount table it found as (mount point, UUID) pairs, plus the
/// bootloader name. Empty when there is no record — every caller treats that as
/// "carry on guessing", never as an error.
pub fn read_layout_record(parts: &[Partition]) -> (Vec<(String, String)>, String) {
    let mut mounts = Vec::new();
    let mut loader = String::new();
    for p in parts {
        if p.fstype != "vfat" {
            continue;
        }
        // Read-only, and unmounted again immediately: this runs while the user
        // is still looking at the list and must not change anything.
        let text = sh_out(&format!(
            "set -e; mkdir -p /run/artix-layout;              mount -o ro {dev} /run/artix-layout 2>/dev/null || exit 0;              cat /run/artix-layout/artix-tui-layout.conf 2>/dev/null;              umount /run/artix-layout 2>/dev/null || true",
            dev = shquote(&p.path)
        ));
        if text.trim().is_empty() {
            continue;
        }
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("bootloader=") {
                loader = rest.trim().to_string();
            }
            let f: Vec<&str> = line.split('|').collect();
            if f.len() >= 3 && f[0] == "mount" && !f[2].is_empty() {
                mounts.push((f[1].to_string(), f[2].to_string()));
            }
            // `swap|UUID|device` — swap has no mount point, so it is recorded
            // under its own verb and mapped to the swap role here.
            if f.len() >= 2 && f[0] == "swap" && !f[1].is_empty() {
                mounts.push(("swap".to_string(), f[1].to_string()));
            }
        }
        if !mounts.is_empty() {
            break;
        }
    }
    (mounts, loader)
}

/// Turn a recorded mount table into roles for the partitions on screen.
///
/// Matching is by UUID, never by device name: a disk that moved from sda to
/// nvme0n1 — or simply enumerated in a different order this boot — is the same
/// filesystem and must still be recognised.
pub fn roles_from_record(parts: &[Partition], record: &[(String, String)]) -> Option<Vec<usize>> {
    if record.is_empty() {
        return None;
    }
    // START FROM THE GUESS, then let the record overrule it partition by
    // partition. Starting from ROLE_NONE was wrong: the record only ever
    // describes what was a MOUNT POINT at install time, so a /home that is a
    // btrfs subvolume carries the root filesystem's UUID (unmatchable from
    // outside an encrypted root) and swap is not in it at all. Everything the
    // record could not speak for was therefore marked "leave alone" — which is
    // worse than the guess it replaced, and had to be fixed by hand.
    let mut roles = suggest_all(parts);
    let mut matched = 0;
    for (i, p) in parts.iter().enumerate() {
        let uuid = sh_out(&format!(
            "blkid -o value -s UUID {} 2>/dev/null",
            shquote(&p.path)
        ));
        let uuid = uuid.trim();
        if uuid.is_empty() {
            continue;
        }
        let Some((mp, _)) = record.iter().find(|(_, u)| u == uuid) else {
            continue;
        };
        // A record entry overrules the guess for THIS partition only.
        roles[i] = match mp.as_str() {
            "/" => ROLE_ROOT,
            "/boot" => ROLE_BOOT,
            "/boot/efi" => ROLE_ESP,
            "/home" => ROLE_HOME,
            "swap" => ROLE_SWAP,
            _ => ROLE_DATA,
        };
        matched += 1;
    }
    // An encrypted root never matches by UUID from outside — the container's
    // own UUID is the LUKS one, and the record holds the filesystem UUID from
    // inside it. So the record can describe /boot and the ESP correctly while
    // saying nothing about the root, and the root still has to be guessed.
    if !roles.contains(&ROLE_ROOT) {
        if let Some(i) = parts.iter().position(|p| {
            p.fstype == "crypto_LUKS"
                || matches!(p.fstype.as_str(), "btrfs" | "ext4" | "xfs" | "f2fs")
        }) {
            roles[i] = ROLE_ROOT;
        }
    }
    // Exactly one root, whatever the two sources between them produced.
    let mut seen_root = false;
    for r in roles.iter_mut() {
        if *r == ROLE_ROOT {
            if seen_root {
                *r = ROLE_NONE;
            }
            seen_root = true;
        }
    }
    if matched == 0 {
        None
    } else {
        Some(roles)
    }
}

/// Fill in a suggested role for every partition. Called once, when the list is
/// first scanned; after that the roles are the user's and are left alone.
pub fn suggest_all(parts: &[Partition]) -> Vec<usize> {
    let mut root_taken = false;
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        let r = suggest_role(p, root_taken);
        if r == ROLE_ROOT {
            root_taken = true;
        }
        out.push(r);
    }
    out
}

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
/// `app.recovery_mounted = true`, which is what reveals the list of repairs.
///
/// Steps:
///   1) resolve the chosen partition (and unlock it if it's LUKS),
///   2) mount the (decrypted) root at /mnt,
///   3) mount /mnt/boot and the EFI partition if they exist as separate
///      filesystems (read from the mounted root's /etc/fstab),
///   4) bind /dev /proc /sys /run so the chroot works,
///   5) detect grub / refind / limine / systemd-boot.
pub fn mount_and_detect(app: &mut App, parts: &[Partition]) {
    // THE ROOT IS THE PARTITION MARKED AS THE ROOT, not the one under the
    // cursor. The cursor is for reading and assigning; conflating the two meant
    // a stray keypress on the way to setting some other partition's role
    // silently changed which system was about to be opened.
    //
    // No fallback to the cursor. There was one, and it made the error below
    // unreachable in the case it was written for: clearing the root role and
    // pressing Enter silently opened whatever the cursor happened to rest on.
    // Refusing and saying which key sets the role is the honest answer.
    let Some(root_idx) = app.recovery_roles.iter().position(|r| *r == ROLE_ROOT) else {
        app.recovery_status = t(app.lang, "rec.err_no_root");
        return;
    };
    let Some(part) = parts.get(root_idx) else {
        app.recovery_status = t(app.lang, "rec.err_no_root");
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
                    app.recovery_status = t(app.lang, "rec.err_no_pass");
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
            app.recovery_status = t(app.lang, "rec.err_unlock").replace("{e}", &e);
            return;
        }
        format!("/dev/mapper/{mapper}")
    };

    // 2) Mount the root filesystem at /mnt.
    if let Err(e) = run_shell(&format!(
        "mkdir -p /mnt && mount {} /mnt",
        shquote(&root_dev)
    )) {
        app.recovery_status = t(app.lang, "rec.err_mount_root").replace("{e}", &e);
        return;
    }

    // 2a) btrfs: the installer puts the system in the `@` subvolume, so a bare
    //     mount lands on the TOPLEVEL — a directory holding `@`, `@home`,
    //     `.snapshots` and no /etc at all. Every step after that then fails in
    //     the most confusing way possible: fstab "isn't there", the chroot
    //     opens into a shell with no system in it, and the user reasonably
    //     reports "the partitions wouldn't mount" (a real field report — the
    //     person gave up and reinstalled). Detect the layout and remount the
    //     actual root. Plain-btrfs installs (no `@`) are left as mounted.
    let _ = run_shell(&format!(
        "if [ -d /mnt/@ ] && [ ! -e /mnt/etc ]; then \
           umount /mnt && mount -o subvol=@ {} /mnt; \
         fi",
        shquote(&root_dev)
    ));

    // 2a-guard) Whatever was mounted must LOOK like a Linux root. Failing loud
    //     and early here beats every later step failing cryptically.
    //
    //     STRUCTURE, not /etc/fstab. This asked for /etc/fstab, and that was
    //     precisely wrong: a missing or broken fstab is one of the commonest
    //     reasons a system will not boot, so recovery refused to open exactly
    //     the system it exists for. Caught by testing recovery the honest way —
    //     break fstab, then try to repair it — and being told the root was not
    //     a root.
    //
    //     `/etc` and `/usr` are directories no Linux root can be missing and
    //     still be worth repairing, and no data partition or ESP has both, so
    //     they discriminate just as well as fstab did without being the thing
    //     that is likely broken.
    if run_shell("test -d /mnt/etc && test -d /mnt/usr").is_err() {
        let _ = run_shell("umount -R /mnt 2>/dev/null || true");
        if app.recovery_unlock != 0 {
            let _ = run_shell("cryptsetup close cryptroot 2>/dev/null || true");
        }
        app.recovery_status = t(app.lang, "rec.err_not_root").replace("{dev}", &part.path);
        return;
    }
    // A missing fstab is not a reason to refuse — it is a DIAGNOSIS, and very
    // often the answer to "why will it not boot". Say so instead of hiding it.
    let fstab_missing = run_shell("test -e /mnt/etc/fstab").is_err();

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

    // 3a) MOUNT WHAT THE USER SAID, over the top of what fstab claimed.
    //
    //     This runs after the fstab pass and wins where they disagree, which is
    //     the whole point: the person is looking at the real partition table
    //     and the fstab is a file that may describe a machine that no longer
    //     exists. Anything still marked "leave alone" is simply not touched.
    //
    //     Shallowest path first, so /boot is in place before /boot/efi goes
    //     inside it. Sorting by the number of separators is enough here — these
    //     are four fixed mount points, not arbitrary nesting.
    let mut planned: Vec<(String, String)> = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if i == root_idx {
            continue;
        }
        let role = app.recovery_roles.get(i).copied().unwrap_or(ROLE_NONE);
        // An unset path falls back to the first preset rather than mounting at
        // the root of /mnt, which would shadow the system being repaired.
        let data_path = app
            .recovery_data_path
            .get(i)
            .map(String::as_str)
            .filter(|p| p.starts_with('/'))
            .unwrap_or(DATA_PATHS[0]);
        if let Some(mp) = mount_point(role, data_path) {
            planned.push((mp, part.path.clone()));
        }
    }
    planned.sort_by_key(|(mp, _)| mp.matches('/').count());
    for (mp, dev) in &planned {
        // AN ENCRYPTED /boot HAS TO BE OPENED BEFORE IT CAN BE MOUNTED.
        //
        // Full-disk encryption puts /boot in its own LUKS container, and this
        // loop mounted the raw partition — which simply fails. The crypttab
        // replay further down does handle it, but only when the root's
        // /etc/crypttab and its keyfile are both intact, and a system whose
        // crypttab is intact is usually not the one being recovered.
        //
        // Two keys are tried, in the order that costs least: the keyfile the
        // installer leaves inside the (already mounted) root, then the
        // passphrase the person typed to unlock the root — with full-disk
        // encryption GRUB asks for one passphrase and it opens both.
        let name = format!("recov{}", mp.replace('/', "_"));
        let _ = run_shell(&format!(
            "mkdir -p /mnt{mp}; \
             # THE USER'S CHOICE OUTRANKS THE TARGET'S OWN fstab.
             #
             # The fstab pass runs before this one, so by now /home may already
             # carry whatever that file named. Bailing out on \"already mounted\"
             # meant a wrong fstab line could never be corrected: the repair
             # wrote /home = @home, the next run mounted @home from that very
             # file, the role for the real /home disk was skipped, and the
             # regenerated fstab said @home again. A loop that fed itself.
             cur=$(findmnt -no SOURCE --mountpoint /mnt{mp} 2>/dev/null | sed 's/\\[.*//'); \
             if [ -n \"$cur\" ]; then \
               [ \"$cur\" = {dev} ] && exit 0; \
               umount -R /mnt{mp} 2>/dev/null || exit 0; \
             fi; \
             src={dev}; \
             if [ \"$(blkid -o value -s TYPE {dev} 2>/dev/null)\" = crypto_LUKS ]; then \
               if [ -e /dev/mapper/{name} ]; then \
                 src=/dev/mapper/{name}; \
               else \
                 for kf in /mnt/etc/luks/boot.key /mnt/boot/luks/root.key; do \
                   [ -f \"$kf\" ] || continue; \
                   cryptsetup open {dev} {name} --key-file \"$kf\" 2>/dev/null && break; \
                 done; \
                 [ -e /dev/mapper/{name} ] || \
                   printf '%s' {pass} | cryptsetup open {dev} {name} - 2>/dev/null || true; \
                 [ -e /dev/mapper/{name} ] || exit 0; \
                 src=/dev/mapper/{name}; \
               fi; \
             fi; \
             mount \"$src\" /mnt{mp} || exit 0; \
             \
             # A PLAIN btrfs MOUNT LANDS ON THE TOP LEVEL, not on the subvolume
             # the system actually uses. A /home disk laid out with an @home
             # subvolume then looks empty at /mnt/home while the real files sit
             # at /mnt/home/@home — and the regenerated fstab records that empty
             # mount, so the login loops with no message. If the top level holds
             # a subvolume matching this mount point, use it.
             [ \"$(findmnt -no FSTYPE /mnt{mp} 2>/dev/null)\" = btrfs ] || exit 0; \
             for sv in {svs}; do \
               [ -d \"/mnt{mp}/$sv\" ] || continue; \
               umount /mnt{mp} 2>/dev/null || exit 0; \
               mount -o subvol=$sv \"$src\" /mnt{mp} 2>/dev/null || \
                 mount \"$src\" /mnt{mp} 2>/dev/null || true; \
               break; \
             done",
            mp = mp,
            dev = shquote(dev),
            name = name,
            pass = shquote(&app.recovery_passphrase),
            svs = match role_of_mp(mp) {
                ROLE_HOME => "@home home",
                ROLE_BOOT => "@boot boot",
                _ => "@data data",
            },
        ));
    }

    // 3a-bis) THE INSTALL'S OWN RECORD WINS OVER BOTH THE GUESS AND THE FSTAB.
    //
    //     A system installed by this tool leaves /etc/artix-tui/install.conf
    //     describing where each filesystem actually belongs (see
    //     `plan_install_manifest`). It is the only source here that is neither
    //     a guess from partition sizes nor the fstab that is probably why the
    //     machine will not boot — so when it exists, it decides.
    //
    //     This matters most for the layout this installer itself produces: with
    //     a root-scope encrypted root the kernels live on the ESP mounted at
    //     /boot, while nearly every other system in the world puts the ESP at
    //     /boot/efi. Guessing /boot/efi there leaves /boot empty and makes the
    //     diagnosis report a missing kernel, a missing initramfs and a missing
    //     bootloader — three findings, all false, each pointing at a repair
    //     that would not help.
    //
    //     Nothing depends on the file: absent or malformed, everything below
    //     carries on exactly as before.
    let _ = run_shell(
        "[ -f /mnt/etc/artix-tui/install.conf ] || exit 0; \
         while IFS='|' read -r kind mp uuid _fs _src; do \
           [ \"$kind\" = mount ] || continue; \
           case \"$mp\" in /boot|/boot/efi|/home) ;; *) continue;; esac; \
           [ -n \"$uuid\" ] || continue; \
           mountpoint -q \"/mnt$mp\" && continue; \
           dev=/dev/disk/by-uuid/$uuid; \
           [ -e \"$dev\" ] || continue; \
           mkdir -p \"/mnt$mp\" && mount \"$dev\" \"/mnt$mp\" 2>/dev/null || true; \
         done < /mnt/etc/artix-tui/install.conf",
    );

    // 3a-ter) THE OTHER SUBVOLUMES, after the roles. A btrfs install here is not one filesystem
    //     with directories in it — /var/log, /var/cache, /home and /.snapshots
    //     are SEPARATE SUBVOLUMES (see disk.rs). Mounting only `@` gives a root
    //     whose /var/log is an empty directory, and that is not cosmetic: dinit
    //     services cannot open their log files and fail one after another
    //     ("execution failed - opening log file: No such file or directory" for
    //     turnstiled, seatd, dbus, then sddm). Worse, "Regenerate fstab" writes
    //     down only what is mounted — so it produced an fstab with no @log or
    //     @cache line, and the repaired system failed to boot in exactly the
    //     same way. The repair was the bug.
    //
    //     Subvolumes that do not exist simply fail to mount and are skipped.
    //
    //     This runs AFTER the roles, and the `mountpoint -q` guard means a
    //     mount point the user already claimed is left alone. Running it
    //     first put @home at /home and then stacked the user's real /home
    //     partition on top of it — two mounts on one target, two /home
    //     lines in the regenerated fstab, and a login that bounced straight
    //     back to the display manager because the home that ended up
    //     visible was the empty one.
    let _ = run_shell(&format!(
        "[ \"$(findmnt -no FSTYPE /mnt 2>/dev/null)\" = btrfs ] || exit 0; \
         for pair in '@home /home' '@log /var/log' '@cache /var/cache' \
                     '@snapshots /.snapshots'; do \
           set -- $pair; \
           mountpoint -q \"/mnt$2\" && continue; \
           mkdir -p \"/mnt$2\"; \
           mount -o subvol=$1 {dev} \"/mnt$2\" 2>/dev/null || true; \
         done",
        dev = shquote(&root_dev)
    ));

    // 3b) /boot IS FOUND EVEN WITHOUT AN FSTAB TO NAME IT.
    //
    //     A missing or incomplete fstab is the commonest reason to be here, and
    //     it takes /boot down with it: nothing names the partition, so nothing
    //     mounts it, so /boot is an empty directory on the root — and then
    //     everything downstream is wrong in the same way. The diagnosis reports
    //     no kernel, no initramfs and no bootloader, all four false. Worse,
    //     "Regenerate fstab" writes an fstab describing only what was mounted,
    //     which is a NEW fstab with no /boot line in it: the repair quietly
    //     bakes the problem in.
    //
    //     So look for it. An ESP is a small vfat partition carrying an EFI
    //     directory or a kernel, and on a machine with one root that is enough
    //     to identify it — this is the same guess the firmware makes. Only the
    //     disk the root lives on is considered, and only when /boot is still
    //     empty, so a system whose fstab DID mount it is never touched.
    //
    //     Finding that disk WALKS UP, one parent at a time, until the device is
    //     of type `disk`. A single `pkname` was not enough: on an encrypted root
    //     the source is /dev/mapper/cryptroot and its parent is the PARTITION,
    //     so the search then listed one partition, found no vfat in it, and gave
    //     up — which is why /boot stayed empty on exactly the systems this was
    //     written for.
    //
    //     THE TEST IS "IS THERE A KERNEL", NOT "IS /boot EMPTY". Emptiness was
    //     wrong in the exact case this exists for: mounting the ESP at
    //     /boot/efi CREATES the directory /boot/efi, so /boot is no longer
    //     empty, the search bailed, and the kernels — which on a root-scope
    //     encrypted install live on that same ESP, belonging at /boot — were
    //     never found. The diagnosis then reported a missing kernel, a missing
    //     initramfs and a missing bootloader: three findings, all false, from
    //     one directory that existed.
    let _ = run_shell(
        "ls /mnt/boot/vmlinuz-* >/dev/null 2>&1 && exit 0; \
         mountpoint -q /mnt/boot && exit 0; \
         root_src=$(findmnt -no SOURCE /mnt) || exit 0; \
         dev=$root_src; \
         for _ in 1 2 3 4; do \
           [ \"$(lsblk -no TYPE \"$dev\" 2>/dev/null | head -1)\" = disk ] && break; \
           parent=$(lsblk -no pkname \"$dev\" 2>/dev/null | head -1); \
           [ -n \"$parent\" ] || break; \
           dev=/dev/$parent; \
         done; \
         disk=$dev; \
         [ -b \"$disk\" ] || exit 0; \
         for p in $(lsblk -lno PATH,FSTYPE \"$disk\" 2>/dev/null \
                    | awk '$2 == \"vfat\" {print $1}'); do \
           mount \"$p\" /mnt/boot 2>/dev/null || continue; \
           if ls /mnt/boot/vmlinuz-* >/dev/null 2>&1; then \
             echo \"mounted $p at /boot\"; exit 0; \
           fi; \
           umount /mnt/boot 2>/dev/null || true; \
         done",
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
    // A missing fstab is named FIRST, because when it is missing it is almost
    // certainly the answer — and because the mount below will have been partial
    // without it, which is worth knowing before wondering where /boot went.
    let fstab_note = if fstab_missing {
        format!("{}\n\n", t(app.lang, "rec.mounted_no_fstab"))
    } else {
        String::new()
    };

    // WHAT ACTUALLY GOT MOUNTED, listed. Not a promise that it did.
    //
    // This used to say "plus /boot, EFI and anything else from its fstab",
    // which is a description of the INTENT. When the intent did not happen —
    // and with a missing fstab it largely cannot — the screen still said it had,
    // and there was no way to tell from the outside. Somebody reasonably
    // reported "the partitions did not mount where they should" and could not
    // show it, because nothing displayed it. So show the mount table.
    let table = sh_out(
        "findmnt -nr -o TARGET,SOURCE,FSTYPE 2>/dev/null \
         | grep -E '^/mnt(/|$)' \
         | grep -Ev '^/mnt/(dev|proc|sys|run)' \
         | sort \
         | while read -r t src fs; do \
             t=${t#/mnt}; [ -n \"$t\" ] || t=/; \
             printf '  %-14s %-26s %s\\n' \"$t\" \"$src\" \"$fs\"; \
           done",
    );

    // WHAT TO DO NEXT, not a list of commands to type.
    //
    // This used to end with "press Enter for a chroot shell" and four example
    // command lines, because that was all recovery could do: mount it and hand
    // the person a prompt. It does named repairs now, and leaving the old text
    // in place meant the screen still told them to type by hand what a button
    // beneath it would do.
    app.recovery_status = format!(
        "{fstab_note}{head}\n\n{table}\n{loader}\n\n{next}",
        head = t(app.lang, "rec.mounted_head").replace("{root}", &root_dev),
        loader = t(app.lang, "rec.mounted_loader").replace("{boot}", &boot),
        next = t(app.lang, "rec.mounted_next"),
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

/// Reinstall the bootloader that is already on this system.
///
/// The case the person names when they arrive: "the bootloader is gone". They
/// know that much and not the six words that put it back — and those words
/// differ per bootloader, per firmware, and per where the ESP is mounted. So it
/// works all three out from the system itself instead of asking.
///
/// It reinstalls what is THERE; it does not choose a bootloader. Recovery
/// repairs a system, it does not redesign one.
pub const REINSTALL_BOOTLOADER: &str = r#"set -e
esp=""
for d in /boot/efi /efi /boot; do
    [ -d "$d/EFI" ] && { esp="$d"; break; }
done
if [ -d /boot/grub ] || [ -f /boot/grub/grub.cfg ]; then
    echo ">>> GRUB found — reinstalling."
    if [ -d /sys/firmware/efi ]; then
        [ -n "$esp" ] || { echo "!! no EFI system partition is mounted (looked in /boot/efi, /efi, /boot)"; exit 1; }
        echo ">>> UEFI, ESP at $esp"
        grub-install --target=x86_64-efi --efi-directory="$esp" --bootloader-id=artix --recheck
    else
        # BIOS: the loader goes on the whole DISK that carries /boot, worked out
        # from the mount rather than guessed from a device name — /dev/nvme0n1
        # is a disk and ends in a digit, so trimming digits is wrong.
        part=$(findmnt -no SOURCE /boot 2>/dev/null || findmnt -no SOURCE /)
        disk=/dev/$(lsblk -no pkname "$part")
        echo ">>> BIOS, installing to $disk"
        grub-install --target=i386-pc --recheck "$disk"
    fi
    grub-mkconfig -o /boot/grub/grub.cfg
elif [ -d /boot/EFI/refind ]; then
    echo ">>> rEFInd found — reinstalling."; refind-install
elif [ -f /boot/limine.conf ] || [ -f /boot/EFI/limine/limine.conf ]; then
    echo ">>> Limine found. Its files are in place; re-run limine-install for your layout if needed."
elif [ -d /boot/loader ]; then
    echo ">>> systemd-boot layout found, which this distribution does not manage. Nothing done."
else
    echo "!! No bootloader signature found in /boot — there is nothing to reinstall."
    echo "   Install one first (e.g. pacman -S grub && grub-install ...)."
    exit 1
fi
echo ">>> Done. Reboot and see."
"#;

/// Rebuild every installed kernel's initramfs.
///
/// The failure this fixes looks like the disk is gone: the kernel starts, finds
/// no root, and drops to a rescue prompt — because the initramfs is missing the
/// hook for encryption, or for the controller, or is simply stale after a
/// kernel update that went wrong.
pub const REBUILD_INITRAMFS: &str = r#"set -e
echo ">>> Rebuilding the initramfs for every installed kernel..."
mkinitcpio -P
echo ">>> Done. If the root is encrypted, check that 'encrypt' is in the HOOKS"
echo "    line of /etc/mkinitcpio.conf before rebooting."
"#;

/// Write a new /etc/fstab from what is mounted right now.
///
/// Deliberately keeps the old one. A generated fstab is a good guess and not a
/// promise: it describes what recovery managed to mount, which may be less than
/// the system had. The person can compare, and `fstab.bak` is often still there
/// from whatever they were doing when it broke.
pub const REGENERATE_FSTAB: &str = r#"set -e
# WRITE A COMPLETE fstab, OR WRITE NOTHING.
#
# fstabgen describes what is mounted RIGHT NOW. Run it with /boot missing and it
# produces an fstab that will never mount /boot: the system still does not boot,
# the next diagnosis reports no kernel and no bootloader, and the repair itself
# is the cause. That happened.
#
# The old guard asked whether /boot was EMPTY, which is the wrong question and
# missed the case exactly: mounting an ESP at /boot/efi CREATES the directory
# /boot/efi, so /boot is not empty, the guard passed, and a /boot-less fstab got
# written anyway. The question is whether a KERNEL is reachable at /boot.
#
# And rather than only refusing, try to fix it first: the kernels are on a
# partition somewhere, and finding it is the same search recovery already does.
if ! ls /boot/vmlinuz-* >/dev/null 2>&1; then
    echo ">>> /boot has no kernel in it. Looking for the partition that does..."
    for p in $(lsblk -lno PATH,FSTYPE 2>/dev/null | awk '$2 == "vfat" || $2 ~ /^ext[234]$/ {print $1}'); do
        mountpoint -q /boot && break
        mount "$p" /boot 2>/dev/null || continue
        if ls /boot/vmlinuz-* >/dev/null 2>&1; then
            echo ">>> Found the kernels on $p and mounted it at /boot."
            break
        fi
        umount /boot 2>/dev/null || true
    done
fi
if ! ls /boot/vmlinuz-* >/dev/null 2>&1; then
    echo "!! No kernel is reachable at /boot, and none was found to mount there."
    echo "   Writing an fstab now would leave out /boot entirely, and the system"
    echo "   still would not boot -- with this repair to blame. Nothing written."
    echo
    echo "   Find it by hand, then run this again:"
    echo "     lsblk -o PATH,FSTYPE,SIZE"
    echo "     mount /dev/sdXN /boot"
    exit 1
fi
# THE BTRFS SUBVOLUMES, or the new fstab is incomplete in the way that matters.
#
# /var/log, /var/cache, /home and /.snapshots are separate subvolumes on this
# layout, not directories. fstabgen writes down only what is mounted, so a run
# with just `@` produced an fstab with no @log line — and the "repaired" system
# then failed to boot with every dinit service reporting it could not open its
# log file. Mount them first so they end up in the file.
if [ "$(findmnt -no FSTYPE /)" = btrfs ]; then
    rootdev=$(findmnt -no SOURCE / | sed 's/\[.*//')
    echo ">>> btrfs root on $rootdev. Subvolumes that actually exist here:"
    btrfs subvolume list -o / 2>/dev/null | awk '{print "      " $NF}' || echo "      (could not list)"
    for pair in '@home /home' '@log /var/log' '@cache /var/cache' '@snapshots /.snapshots'; do
        set -- $pair
        if mountpoint -q "$2"; then
            echo "      $2 already mounted"
            continue
        fi
        mkdir -p "$2"
        if mount -o subvol=$1 "$rootdev" "$2" 2>/dev/null; then
            echo "      mounted $1 at $2"
        else
            echo "      $1 -> $2 : NOT MOUNTED (no such subvolume, or mount refused)"
        fi
    done
fi
# The dinit services write to /var/log/dinit/<name>.log and refuse to start when
# that directory is missing -- "execution failed - opening log file: No such file
# or directory" for turnstiled, seatd and dbus, which then takes logind and sddm
# down with them. If /var/log came back as an empty placeholder because @log is
# not mounted, creating the directory at least lets the system boot and be
# repaired from inside.
if [ ! -d /var/log/dinit ]; then
    mkdir -p /var/log/dinit
    echo ">>> Created the missing /var/log/dinit (dinit refuses to start without it)."
fi
# SWAP TOO. fstabgen only records swap that is switched ON, and recovery does
# not switch any on — so the regenerated file had no swap line and the boot
# reported "Service swap command failed with exit code 1".
swapline=""
for sp in $(blkid -t TYPE=swap -o device 2>/dev/null); do
    su=$(blkid -o value -s UUID "$sp" 2>/dev/null) || continue
    [ -n "$su" ] || continue
    swapline="$swapline
UUID=$su none swap defaults 0 0"
done
if [ -f /etc/fstab ]; then
    cp -a /etc/fstab "/etc/fstab.before-recovery.$(date +%Y%m%d-%H%M%S)"
    echo ">>> Kept the old one as /etc/fstab.before-recovery.*"
fi
echo ">>> Writing /etc/fstab from what is mounted now:"
# GENERATE IT OURSELVES. `fstabgen` is an artools command and `genfstab` comes
# from arch-install-scripts: BOTH live on the live ISO and NEITHER is installed
# in the target system. This repair runs INSIDE the target, so both calls failed
# with "command not found", the error went to /dev/null, and `set -e` killed the
# script before it could write anything. The fstab was never touched -- same 126
# bytes, same October timestamp, run after run, which is exactly what the
# diagnosis kept reporting while the repair claimed to have run.
#
# findmnt and blkid are util-linux. They are always there.
gen_fstab() {
    printf '# Generated by the Artix TUI recovery mode on %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '# <file system> <dir> <type> <options> <dump> <pass>\n'

    # ONE LINE PER MOUNT POINT. Mounts stack: something mounted at /home and
    # then something else mounted at /home leaves both visible to findmnt, and
    # writing both into an fstab means the next boot mounts them in the same
    # order and the wrong one wins. Keep the last, which is the one actually in
    # use, and drop the rest.
    findmnt -rn --real -o TARGET,SOURCE,FSTYPE,OPTIONS 2>/dev/null \
    | awk '{ last[$1] = $0 } END { for (t in last) print last[t] }' \
    | sort -k1,1 | while read -r tgt src fs opts; do
        case "$fs" in
            proc|sysfs|devtmpfs|devpts|tmpfs|cgroup|cgroup2|securityfs|efivarfs|bpf) continue ;;
            tracefs|debugfs|mqueue|hugetlbfs|fusectl|configfs|pstore|ramfs|autofs) continue ;;
            binfmt_misc|squashfs|overlay|selinuxfs|nfsd|rpc_pipefs) continue ;;
            fuse|fuse.*|fuseblk|nfs|nfs4|cifs|smb3|iso9660|udf) continue ;;
        esac
        # Transient mounts belong to whoever made them, not in an fstab. A
        # stray AppImage under /tmp/.mount_* was picked up on the first run of
        # this generator, and a line like that would fail the next boot.
        case "$tgt" in
            /tmp/*|/run/*|/media/*|/proc/*|/sys/*|/dev/*|/var/lib/docker/*) continue ;;
        esac
        dev=$(printf '%s' "$src" | sed 's/\[.*//')
        uuid=$(blkid -o value -s UUID "$dev" 2>/dev/null) || uuid=""
        # A UUID survives a disk being renamed or enumerated differently; the
        # device path does not. Falling back to the path is better than dropping
        # the line, but it is worth saying so.
        if [ -n "$uuid" ]; then
            spec="UUID=$uuid"
        else
            spec="$dev"
            echo "!! No UUID for $dev ($tgt) - wrote the device path instead." >&2
        fi
        if [ "$tgt" = / ]; then pass=1; else pass=2; fi
        # DROP subvolid=. findmnt reports both subvolid= and subvol=, and they
        # are redundant — but a pinned subvolid is actively wrong on a system
        # with snapper: a rollback gives the subvolume a NEW id, and an fstab
        # that names the old one keeps mounting the very snapshot that was
        # rolled back. Keep the name, which survives.
        opts=$(printf '%s' "$opts" | sed 's/subvolid=[0-9]*,//; s/,subvolid=[0-9]*//')
        printf '%-42s %-16s %-7s %s 0 %s\n' "$spec" "$tgt" "$fs" "$opts" "$pass"
    done
}
# fstabgen is Artix's own tool (artools) and is what the Artix install guide
# uses, so it is preferred whenever it is actually present. Arch's genfstab is
# deliberately NOT used as a fallback: on a system without artools there is no
# reason to expect arch-install-scripts either, and reaching for an Arch tool in
# an Artix installer is the wrong instinct even when it would work.
if command -v fstabgen >/dev/null 2>&1; then
    echo ">>> Using fstabgen."
    fstabgen -U / > /etc/fstab.new
else
    echo ">>> fstabgen is not installed here (it lives on the ISO), generating directly."
    gen_fstab > /etc/fstab.new
fi
if [ -n "$swapline" ] && ! awk '!/^#/ && $3 == "swap"' /etc/fstab.new | grep -q .; then
    printf '%s\n' "$swapline" >> /etc/fstab.new
    echo ">>> Added the swap partition(s) fstabgen could not see."
fi
# CHECK WHAT WAS PRODUCED BEFORE INSTALLING IT. A generator that silently emits
# nothing useful is how the last attempt left a 126-byte comments-only file in
# place and reported success.
if ! awk '!/^#/ && NF>=2 && $2 == "/"' /etc/fstab.new | grep -q .; then
    echo "!! The generated fstab has no root entry. Refusing to install it."
    rm -f /etc/fstab.new
    exit 1
fi
mv /etc/fstab.new /etc/fstab
cat /etc/fstab
echo
if [ "$(findmnt -no FSTYPE /)" = btrfs ] && ! awk '!/^#/ && $4 ~ /subvol=\/?@log/' /etc/fstab | grep -q .; then
    echo "!! Note: no /var/log subvolume line. If this system uses @log, dinit"
    echo "   services will fail to open their log files on the next boot."
fi
if ! awk '!/^#/ && NF>=2 && $2 == "/boot"' /etc/fstab | grep -q .; then
    echo "!! Note: there is no /boot line. That is correct ONLY if this system"
    echo "   keeps its kernels on the root filesystem. If /boot is its own"
    echo "   partition, mount it and run this again."
fi
# COMPARE AGAINST WHAT THE INSTALL WROTE DOWN, if it wrote anything.
#
# fstabgen can only describe what is mounted, so "it produced a file" is not the
# same as "it produced the right file". The install record names every mount
# point this system was built with, which makes it possible to say concretely
# what is still missing instead of hoping.
rec=""
for c in /etc/artix-tui/install.conf /boot/artix-tui-layout.conf /boot/efi/artix-tui-layout.conf; do
    [ -f "$c" ] && { rec=$c; break; }
done
if [ -n "$rec" ]; then
    missing=""
    while IFS='|' read -r kind mp _rest; do
        [ "$kind" = mount ] || continue
        [ "$mp" = / ] && continue
        awk -v m="$mp" '!/^#/ && NF>=2 && $2 == m' /etc/fstab | grep -q . || missing="$missing $mp"
    done < "$rec"
    if [ -n "$missing" ]; then
        echo "!! The install record lists mount points this fstab does NOT cover:$missing"
        echo "   Mount them and run this again, or the system will boot without them."
    else
        echo ">>> Checked against the install record: every mount point it names is covered."
    fi
else
    echo ">>> No install record on this system, so this could not be cross-checked."
fi
# WHOSE HOME IS WHERE. A login that bounces straight back to the display
# manager is almost always a home directory that is not where the session
# expects it -- and from the fstab alone that is invisible, because a /home line
# that mounts an EMPTY subvolume looks exactly like one that mounts the right
# filesystem. So list what is actually there.
rm -f /run/home-missing
echo ">>> Home directories as they stand right now:"
awk -F: '$3 >= 1000 && $3 < 65534 { print $1 " " $6 }' /etc/passwd 2>/dev/null | while read -r u h; do
    if [ -d "$h" ]; then
        n=$(ls -A "$h" 2>/dev/null | wc -l)
        owner=$(stat -c '%U' "$h" 2>/dev/null)
        echo "      $u -> $h : exists, $n entries, owned by $owner"
        [ "$n" = 0 ] && echo "         !! EMPTY. The session will fail and the login will loop."
        [ "$owner" = "$u" ] || echo "         !! Owned by $owner, not $u. The session cannot write here."
    else
        echo "      $u -> $h : DOES NOT EXIST. The login will loop with no message."
        echo missing >> /run/home-missing
    fi
done
# GO AND FIND IT. Being told the home is missing still leaves the question of
# where it went, and the answer is on some partition that nothing mounted --
# which is not something to make somebody work out from a partition table at
# midnight. Look inside every filesystem that is not already in use and say
# which one holds it.
# A flag file, not a variable: the loop above runs in a pipeline, so it is a
# SUBSHELL and anything it assigns is gone by the time this line is reached.
# That is why the search below silently never ran.
if [ -s /run/home-missing ]; then
    echo ">>> Looking for those home directories on the other partitions..."
    mkdir -p /run/homeprobe
    for d in $(blkid -o device 2>/dev/null); do
        findmnt -S "$d" >/dev/null 2>&1 && continue
        case "$(blkid -o value -s TYPE "$d" 2>/dev/null)" in
            btrfs|ext2|ext3|ext4|xfs|f2fs) ;;
            *) continue ;;
        esac
        mount -o ro "$d" /run/homeprobe 2>/dev/null || continue
        awk -F: '$3 >= 1000 && $3 < 65534 { print $1 }' /etc/passwd 2>/dev/null | while read -r u; do
            # A btrfs disk mounted plainly shows its TOP LEVEL, so a home
            # inside an @home subvolume appears as @home/<user> rather than
            # <user>. Checking only the first two spellings would miss exactly
            # the layout this installer creates.
            for cand in "/run/homeprobe/$u" "/run/homeprobe/home/$u" \
                        "/run/homeprobe/@home/$u" "/run/homeprobe/@/home/$u"; do
                [ -d "$cand" ] || continue
                sub=${cand#/run/homeprobe}
                echo "      FOUND $u at $d$sub"
                case "$sub" in
                    "/$u")
                        echo "         -> give $d the /home role in recovery, then run this again." ;;
                    "/home/$u")
                        echo "         -> $d holds a whole /home tree; give it the /home role." ;;
                    *)
                        echo "         -> it is inside a btrfs subvolume: mount it with" 
                        echo "            mount -o subvol=$(printf '%s' "$sub" | cut -d/ -f2) $d /home" ;;
                esac
            done
        done
        umount /run/homeprobe 2>/dev/null || true
    done
    rmdir /run/homeprobe 2>/dev/null || true
    echo "      (nothing more found — if the home was on a btrfs subvolume, it may"
    echo "       need mounting with -o subvol=NAME by hand)"
fi
echo ">>> Check it before rebooting: this describes what recovery could mount,"
echo "    which may be less than the system actually had."
"#;

/// Look the mounted system over and say what is wrong with it.
///
/// READ-ONLY, on purpose. This is the answer to "it will not boot and I do not
/// know why" — and the worst thing such a tool can do is change something before
/// the person understands what they are looking at. Every line here inspects;
/// nothing repairs. The repairs are separate actions the person chooses.
///
/// The checks are the failures that actually stop a boot, in the order they
/// stop it: no kernel, no initramfs, no bootloader, an fstab naming volumes
/// that no longer exist, a crypttab naming a device that is not there, a full
/// root. Each one is phrased as what it means rather than what was run.
pub fn diagnose(lang: Lang) -> (String, usize) {
    let mut out = String::new();
    let mut problems = 0u32;
    // Which repair to point at afterwards. 0 means "no single obvious one".
    let mut suggest = 0usize;
    // Checks that could not be RUN. Not problems, and emphatically not passes.
    let mut unknowns = 0u32;

    // The three markers a line can start with. Localised like everything else:
    // somebody reading a report about their broken system in a language they do
    // not speak learns nothing from it, and this screen exists for the moment
    // when they are least able to go looking for a translation.
    let ok_tag = t(lang, "rec.dg_ok");
    let bad_tag = t(lang, "rec.dg_problem");

    // WHICH SYSTEM IS THIS? The report used to be anonymous, and on a machine
    // with two installs that is unusable: told "/etc/fstab is present" by
    // somebody who deleted it themselves, you cannot tell whether the tool is
    // wrong or looking at the other disk. Name the root, and say so first.
    let root_src = sh_out("findmnt -no SOURCE /mnt 2>/dev/null");
    let host = sh_out("cat /mnt/etc/hostname 2>/dev/null");
    let mut ident = t(lang, "rec.dg_system").replace("{dev}", root_src.trim());
    if !host.trim().is_empty() {
        ident.push_str(&t(lang, "rec.dg_system_host").replace("{host}", host.trim()));
    }
    // What the install left behind about itself, if anything.
    let manifest = sh_out("cat /mnt/etc/artix-tui/install.conf 2>/dev/null");
    if !manifest.trim().is_empty() {
        let when = manifest
            .lines()
            .find_map(|l| l.strip_prefix("installed="))
            .unwrap_or("")
            .to_string();
        ident.push_str(&t(lang, "rec.dg_system_made").replace("{when}", when.trim()));
    } else {
        // SAY WHEN THERE IS NO RECORD. Without this the screen simply behaves
        // differently on older systems with no explanation, and "mount the root
        // and everything else is picked up automatically" quietly does not
        // happen — which reads as the feature being broken rather than absent.
        ident.push_str(&t(lang, "rec.dg_system_no_record"));
    }

    // IS /boot ACTUALLY MOUNTED? Everything below that looks inside /boot is
    // meaningless if it is not, and saying so is the difference between a
    // diagnosis and a scare.
    //
    // On a system with a separate /boot — the normal case here — recovery
    // mounts it FROM /etc/fstab. With no fstab there is nothing to mount it
    // from, so /boot is an empty directory on the root and every check below it
    // reports a catastrophe: no kernel, no initramfs, no bootloader, no EFI
    // binary. All four were false, and the advice ("reinstall the kernel
    // package") would have been damaging to follow.
    //
    //     "Is /boot EMPTY" was the wrong question and it made this branch miss
    //     the very case it guards. Mounting an ESP at /boot/efi creates the
    //     directory /boot/efi, so /boot is not empty — and the checks below then
    //     confidently reported a missing kernel on a system whose kernels were
    //     simply on a partition nothing had mounted. The question is whether a
    //     KERNEL IS THERE, and if not, whether anything is mounted at /boot at
    //     all: nothing mounted means the answer is unknown, not absent.
    let boot_sep = sh_out("findmnt -rno SOURCE /mnt/boot 2>/dev/null");
    let boot_mounted = !boot_sep.trim().is_empty();
    let has_kernel = !sh_out("ls /mnt/boot/vmlinuz-* 2>/dev/null | head -1")
        .trim()
        .is_empty();
    let boot_unknown = !has_kernel && !boot_mounted;

    // A plain fn rather than a closure: the closure borrowed `out` for the whole
    // body, and the /boot section needs to write its own line into it.
    //
    // Each finding is ONE line and is not wrapped here. It used to be, with
    // hand-placed continuation indents — which the panel's `Wrap { trim: true }`
    // strips, so the report arrived on screen as ragged half-lines. Wrapping is
    // the renderer's job; it also cannot be done here, because the width that
    // suits English does not suit the same sentence in Ukrainian.
    fn check(
        out: &mut String,
        problems: &mut u32,
        ok_tag: &str,
        bad_tag: &str,
        ok: bool,
        good: &str,
        bad: &str,
    ) {
        if ok {
            out.push_str(&format!("{ok_tag}  {good}\n"));
        } else {
            out.push_str(&format!("{bad_tag}  {bad}\n"));
            *problems += 1;
        }
    }

    // A kernel and an initramfs to load it with — unless /boot is not mounted,
    // in which case there is nothing to see and saying otherwise is a scare.
    if boot_unknown {
        unknowns += 1;
        // The REASON, worked out rather than assumed. This said "/etc/fstab —
        // which is missing", which was true of the case it was written for and
        // a plain contradiction two lines under "ok /etc/fstab is present": a
        // restored fstab that simply has no /boot line lands here too.
        let why = if !std::path::Path::new("/mnt/etc/fstab").exists() {
            t(lang, "rec.dg_why_no_fstab")
        } else if sh_out("awk '!/^#/ && $2 == \"/boot\"' /mnt/etc/fstab")
            .trim()
            .is_empty()
        {
            t(lang, "rec.dg_why_no_boot_line")
        } else {
            t(lang, "rec.dg_why_mount_failed")
        };
        out.push_str(&format!(
            "{}  {}\n",
            t(lang, "rec.dg_unknown"),
            t(lang, "rec.dg_boot_unmounted").replace("{why}", &why)
        ));
    } else {
        let kernels = sh_out("ls /mnt/boot/vmlinuz-* 2>/dev/null | wc -l");
        // SHOW THE EVIDENCE. "No kernel in /boot" is a claim, and when it is
        // wrong the only way anyone can tell is by seeing what the tool sees.
        // So the failure carries what /boot holds and where it came from.
        let boot_from = sh_out("findmnt -no SOURCE /mnt/boot 2>/dev/null");
        let boot_ls = sh_out("ls -A /mnt/boot 2>/dev/null | head -6 | tr '\n' ' '");
        let kernel_bad = t(lang, "rec.dg_kernel_bad")
            .replace(
                "{from}",
                if boot_from.trim().is_empty() {
                    "—"
                } else {
                    boot_from.trim()
                },
            )
            .replace(
                "{ls}",
                if boot_ls.trim().is_empty() {
                    "—"
                } else {
                    boot_ls.trim()
                },
            );
        check(
            &mut out,
            &mut problems,
            &ok_tag,
            &bad_tag,
            kernels.trim() != "0",
            &t(lang, "rec.dg_kernel_ok"),
            &kernel_bad,
        );
        let initrd = sh_out("ls /mnt/boot/initramfs-*.img 2>/dev/null | wc -l");
        if initrd.trim() == "0" {
            suggest = 2; // rebuild the initramfs
        }
        check(
            &mut out,
            &mut problems,
            &ok_tag,
            &bad_tag,
            initrd.trim() != "0",
            &t(lang, "rec.dg_initramfs_ok"),
            &t(lang, "rec.dg_initramfs_bad"),
        );
    }

    // Modules matching that kernel: a mismatch is why a kernel boots to a
    // panic rather than to a login.
    let mods = sh_out("ls -d /mnt/usr/lib/modules/*/ 2>/dev/null | wc -l");
    check(
        &mut out,
        &mut problems,
        &ok_tag,
        &bad_tag,
        mods.trim() != "0",
        &t(lang, "rec.dg_modules_ok"),
        &t(lang, "rec.dg_modules_bad"),
    );

    // fstab, and whether what it names still exists.
    if std::path::Path::new("/mnt/etc/fstab").exists() {
        let missing = sh_out(
            "awk '!/^#/ && NF>=2 && $1 ~ /^(UUID=|PARTUUID=|LABEL=|\\/dev\\/)/ {print $1}' \
             /mnt/etc/fstab | while read -r spec; do \
               findmnt --source \"$spec\" >/dev/null 2>&1 && continue; \
               case $spec in \
                 UUID=*)     blkid -U \"${spec#UUID=}\" >/dev/null 2>&1 || echo \"$spec\" ;; \
                 PARTUUID=*) blkid -t PARTUUID=\"${spec#PARTUUID=}\" >/dev/null 2>&1 || echo \"$spec\" ;; \
                 LABEL=*)    blkid -L \"${spec#LABEL=}\" >/dev/null 2>&1 || echo \"$spec\" ;; \
                 /dev/*)     [ -e \"$spec\" ] || echo \"$spec\" ;; \
               esac; \
             done",
        );
        // DOES IT NAME A ROOT AT ALL? This asked only whether the volumes it
        // names still exist — and an fstab that names NOTHING passes that
        // vacuously. Which is exactly what happens here: deleting /etc/fstab
        // gets the `filesystem` package's default put back, 126 bytes of pure
        // comments, and the check cheerfully reported "present, and everything
        // it names exists" about a file that cannot boot anything. The person
        // reading that had just failed to boot the machine.
        let entries = sh_out("awk '!/^#/ && NF>=2' /mnt/etc/fstab 2>/dev/null | wc -l");
        let has_root = !sh_out("awk '!/^#/ && NF>=2 && $2 == \"/\"' /mnt/etc/fstab 2>/dev/null")
            .trim()
            .is_empty();
        if entries.trim() == "0" {
            suggest = 3;
            check(
                &mut out,
                &mut problems,
                &ok_tag,
                &bad_tag,
                false,
                "",
                &t(lang, "rec.dg_fstab_empty"),
            );
        } else if !has_root {
            suggest = 3;
            check(
                &mut out,
                &mut problems,
                &ok_tag,
                &bad_tag,
                false,
                "",
                &t(lang, "rec.dg_fstab_no_root"),
            );
        }
        check(
            &mut out,
            &mut problems,
            &ok_tag,
            &bad_tag,
            missing.trim().is_empty(),
            &t(lang, "rec.dg_fstab_ok")
                .replace("{n}", entries.trim())
                .replace(
                    "{stat}",
                    sh_out("stat -c '%s B, %y' /mnt/etc/fstab 2>/dev/null | cut -c1-28").trim(),
                ),
            &t(lang, "rec.dg_fstab_bad").replace(
                "{list}",
                &missing.split_whitespace().collect::<Vec<_>>().join(", "),
            ),
        );
    } else {
        // The loudest single cause there is, and it cascades: with no fstab
        // nothing else gets mounted either, so it is fixed FIRST and everything
        // else re-checked afterwards.
        suggest = 3; // regenerate fstab
        check(
            &mut out,
            &mut problems,
            &ok_tag,
            &bad_tag,
            false,
            "",
            &t(lang, "rec.dg_fstab_missing"),
        );
    }

    // The bootloader, and whether its files are where its type expects them.
    if !boot_unknown {
        let boot = detect_bootloader();
        if boot.starts_with("unknown") {
            suggest = 1; // reinstall the bootloader
        }
        check(
            &mut out,
            &mut problems,
            &ok_tag,
            &bad_tag,
            !boot.starts_with("unknown"),
            &t(lang, "rec.dg_boot_ok").replace("{name}", &boot),
            &t(lang, "rec.dg_boot_bad"),
        );
    }

    // An EFI system partition with something bootable on it. Only meaningful on
    // a UEFI machine, so it is reported as information there and skipped on BIOS.
    if std::path::Path::new("/sys/firmware/efi").exists() && !boot_unknown {
        let efi = sh_out("find /mnt/boot -maxdepth 4 -iname '*.efi' 2>/dev/null | head -5");
        check(
            &mut out,
            &mut problems,
            &ok_tag,
            &bad_tag,
            !efi.trim().is_empty(),
            &t(lang, "rec.dg_efi_ok"),
            &t(lang, "rec.dg_efi_bad"),
        );
        let entries = sh_out("efibootmgr 2>/dev/null | grep -c '^Boot0'");
        check(
            &mut out,
            &mut problems,
            &ok_tag,
            &bad_tag,
            entries.trim() != "0",
            &t(lang, "rec.dg_nvram_ok"),
            &t(lang, "rec.dg_nvram_bad"),
        );
    }

    // crypttab: a named device that is not present hangs the boot waiting.
    if std::path::Path::new("/mnt/etc/crypttab").exists() {
        let bad = sh_out(
            "awk '!/^#/ && NF>=2 {print $2}' /mnt/etc/crypttab | while read -r spec; do \
               case $spec in \
                 UUID=*) blkid -U \"${spec#UUID=}\" >/dev/null 2>&1 || echo \"$spec\" ;; \
                 /dev/*) [ -e \"$spec\" ] || echo \"$spec\" ;; \
               esac; \
             done",
        );
        check(
            &mut out,
            &mut problems,
            &ok_tag,
            &bad_tag,
            bad.trim().is_empty(),
            &t(lang, "rec.dg_crypt_ok"),
            &t(lang, "rec.dg_crypt_bad").replace(
                "{list}",
                &bad.split_whitespace().collect::<Vec<_>>().join(", "),
            ),
        );
    }

    // THE DIRECTORY dinit REFUSES TO START WITHOUT.
    //
    // Every dinit service here logs to /var/log/dinit/<name>.log and will not
    // start if that directory is absent — turnstiled, seatd and dbus fail, and
    // logind and sddm fall over behind them. On this layout /var/log is its own
    // btrfs subvolume, so an fstab that does not mount @log leaves /var/log as
    // an empty placeholder and takes the whole session down. It looks like a
    // display-manager problem and is not one.
    if std::path::Path::new("/mnt/var").exists() {
        let has_dir = std::path::Path::new("/mnt/var/log/dinit").is_dir();
        check(
            &mut out,
            &mut problems,
            &ok_tag,
            &bad_tag,
            has_dir,
            &t(lang, "rec.dg_logdir_ok"),
            &t(lang, "rec.dg_logdir_bad"),
        );
        if !has_dir && suggest == 0 {
            suggest = 3; // regenerate fstab: the usual reason it is missing
        }
    }

    // THE BOOT CHAIN ITSELF: can this kernel actually find and open its root?
    //
    // Everything above asks whether the FILES are there. None of it asks the
    // question that decides whether the machine starts: does the bootloader
    // hand the kernel a root it can reach, and can the initramfs open it. On an
    // encrypted system that is two separate things and either one alone is
    // fatal — with no message beyond a rescue prompt.
    let root_is_luks = !sh_out(
        "src=$(findmnt -no SOURCE /mnt 2>/dev/null | sed 's/\\[.*//'); \
         case \"$src\" in /dev/mapper/*) echo yes ;; esac",
    )
    .trim()
    .is_empty();

    if root_is_luks {
        // The initramfs must be built WITH the encrypt hook, or the kernel
        // starts and then has no way to unlock anything.
        let hooks = sh_out(
            "awk -F'[()]' '/^HOOKS=/ {print $2}' /mnt/etc/mkinitcpio.conf 2>/dev/null | tail -1",
        );
        if !hooks.trim().is_empty() {
            let has_encrypt = hooks
                .split_whitespace()
                .any(|h| h == "encrypt" || h == "sd-encrypt");
            check(
                &mut out,
                &mut problems,
                &ok_tag,
                &bad_tag,
                has_encrypt,
                &t(lang, "rec.dg_hook_ok"),
                &t(lang, "rec.dg_hook_bad"),
            );
            if !has_encrypt && suggest == 0 {
                suggest = 2; // rebuild the initramfs
            }
        }
        // And the bootloader must TELL it which container to open.
        let cmdline = sh_out(
            "cat /mnt/boot/grub/grub.cfg /mnt/boot/loader/entries/*.conf \
                 /mnt/boot/refind_linux.conf /mnt/boot/limine.conf 2>/dev/null \
             | grep -c 'cryptdevice=\\|rd.luks'",
        );
        let named = cmdline.trim() != "0" && !cmdline.trim().is_empty();
        // efibootmgr carries it instead on an EFISTUB setup.
        let in_nvram = sh_out("efibootmgr -v 2>/dev/null | grep -c 'cryptdevice=\\|rd.luks'");
        check(
            &mut out,
            &mut problems,
            &ok_tag,
            &bad_tag,
            named || in_nvram.trim() != "0",
            &t(lang, "rec.dg_crypt_cmdline_ok"),
            &t(lang, "rec.dg_crypt_cmdline_bad"),
        );
        if !named && in_nvram.trim() == "0" && suggest == 0 {
            suggest = 1; // reinstall the bootloader, which regenerates it
        }
    }

    // The bootloader's own config must name a kernel that is actually there.
    // A config left behind by a removed kernel points at a file that no longer
    // exists, and the firmware stops at a black screen with no explanation.
    if std::path::Path::new("/mnt/boot/grub/grub.cfg").exists() {
        let dangling = sh_out(
            "awk '/^[[:space:]]*linux/ {print $2}' /mnt/boot/grub/grub.cfg 2>/dev/null \
             | sort -u | while read -r k; do \
                 [ -e \"/mnt/boot$k\" ] || [ -e \"/mnt$k\" ] || echo \"$k\"; \
               done",
        );
        check(
            &mut out,
            &mut problems,
            &ok_tag,
            &bad_tag,
            dangling.trim().is_empty(),
            &t(lang, "rec.dg_grubcfg_ok"),
            &t(lang, "rec.dg_grubcfg_bad").replace(
                "{list}",
                &dangling.split_whitespace().collect::<Vec<_>>().join(", "),
            ),
        );
        if !dangling.trim().is_empty() && suggest == 0 {
            suggest = 1;
        }
    }

    // A HOME THAT IS NOT THERE BOUNCES THE LOGIN STRAIGHT BACK.
    //
    // The display manager accepts the password, the session cannot start
    // because there is no home to write .Xauthority into, and the user lands
    // back on the login screen with no message at all. It looks like a wrong
    // password or a broken desktop; it is usually a /home that no longer gets
    // mounted, which is exactly what a lost fstab causes.
    let homeless = sh_out(
        "awk -F: '$3 >= 1000 && $3 < 65534 && $6 ~ /^\\/home\\// {print $1 \" \" $6}' \
           /mnt/etc/passwd 2>/dev/null | while read -r u h; do \
             [ -d \"/mnt$h\" ] || echo \"$u\"; \
           done",
    );
    if !sh_out("awk -F: '$3 >= 1000 && $3 < 65534' /mnt/etc/passwd 2>/dev/null")
        .trim()
        .is_empty()
    {
        check(
            &mut out,
            &mut problems,
            &ok_tag,
            &bad_tag,
            homeless.trim().is_empty(),
            &t(lang, "rec.dg_home_ok"),
            &t(lang, "rec.dg_home_bad").replace(
                "{list}",
                &homeless.split_whitespace().collect::<Vec<_>>().join(", "),
            ),
        );
    }

    // A root with no room left cannot finish a boot either.
    let full = sh_out("df -P /mnt | awk 'NR==2 {gsub(/%/,\"\",$5); print $5}'");
    let pct: u32 = full.trim().parse().unwrap_or(0);
    check(
        &mut out,
        &mut problems,
        &ok_tag,
        &bad_tag,
        pct < 99,
        &t(lang, "rec.dg_space_ok").replace("{pct}", &pct.to_string()),
        &t(lang, "rec.dg_space_bad").replace("{pct}", &pct.to_string()),
    );

    // The summary says only what was actually established. "Nothing obviously
    // wrong" printed above "the kernel and the bootloader cannot be checked"
    // was a contradiction, and the reassuring half is the one people read.
    let head = match (problems, unknowns) {
        (0, 0) => t(lang, "rec.dg_head_clean"),
        (0, _) => t(lang, "rec.dg_head_unknown").replace("{n}", &unknowns.to_string()),
        (_, 0) => t(lang, "rec.dg_head_problems").replace("{n}", &problems.to_string()),
        _ => t(lang, "rec.dg_head_both")
            .replace("{n}", &problems.to_string())
            .replace("{u}", &unknowns.to_string()),
    };
    // POINT AT THE REPAIR. Being told what is broken and left to work out which
    // of six buttons addresses it is only half an answer — and the person who
    // needs this screen is by definition the one who does not know.
    //
    // A check that could not be RUN gets the same treatment: the fstab is what
    // decides whether /boot is mounted, so an unanswered report points there.
    if suggest == 0 && unknowns > 0 {
        suggest = 3;
    }
    let tail = match suggest {
        1 => format!("\n{}\n", t(lang, "rec.dg_next_bootloader")),
        2 => format!("\n{}\n", t(lang, "rec.dg_next_initramfs")),
        3 => format!("\n{}\n", t(lang, "rec.dg_next_fstab")),
        _ => String::new(),
    };
    (format!("{ident}\n\n{head}\n\n{out}{tail}"), suggest)
}

/// Which role a planned mount point came from, for picking the subvolume names
/// worth trying when a plain btrfs mount lands on the top level.
fn role_of_mp(mp: &str) -> usize {
    match mp {
        "/home" => ROLE_HOME,
        "/boot" => ROLE_BOOT,
        "/boot/efi" => ROLE_ESP,
        _ => ROLE_DATA,
    }
}

/// A shell command's OUTPUT, for the inspections above. Errors come back empty,
/// which every caller above treats as "could not tell" rather than "fine".
fn sh_out(script: &str) -> String {
    capture("sh", &["-c", script]).unwrap_or_default()
}

/// Unmount everything recovery mounted, in reverse, and close the LUKS mapping.
/// Best-effort: called after the chroot shell exits. Never panics.
pub fn cleanup() {
    let _ = run_shell(
        "umount -R /mnt 2>/dev/null || true; \
         for m in /dev/mapper/recov_*; do \
           [ -e \"$m\" ] || continue; \
           cryptsetup close \"${m#/dev/mapper/}\" 2>/dev/null || true; \
         done; \
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

#[cfg(test)]
mod tests {
    use super::*;

    fn part(path: &str, size: &str, fstype: &str, label: &str) -> Partition {
        Partition {
            path: path.into(),
            size: size.into(),
            fstype: fstype.into(),
            label: label.into(),
        }
    }

    /// The diagnosis must ask whether the machine can actually BOOT, not only
    /// whether the files are on disk.
    ///
    /// Every earlier check answered "is it there". None asked the question that
    /// decides whether it starts: does the bootloader hand the kernel a root it
    /// can reach, and can the initramfs open it. On an encrypted system those
    /// are two separate things and either one alone is fatal, with no message
    /// beyond a rescue prompt. This was deferred three times while precisely
    /// that class of failure was being chased by hand.
    #[test]
    fn the_diagnosis_checks_the_boot_chain_not_just_the_files() {
        let src = std::fs::read_to_string("src/system/recovery.rs").expect("readable");
        let code = &src[..src.find("#[cfg(test)]").expect("test module")];
        // The initramfs must carry a hook that can unlock the root.
        assert!(code.contains("rec.dg_hook_bad") && code.contains("sd-encrypt"));
        // The bootloader must name the container to unlock.
        assert!(code.contains("cryptdevice=") && code.contains("rec.dg_crypt_cmdline_bad"));
        // EFISTUB keeps it in NVRAM instead, so that counts too.
        assert!(
            code.contains("efibootmgr -v"),
            "an EFISTUB setup passes it through the firmware entry, not a config"
        );
        // And a config pointing at a kernel that is gone is a black screen.
        assert!(code.contains("rec.dg_grubcfg_bad"));
        // Each of these points at the repair that addresses it.
        assert!(code.contains("suggest = 2; // rebuild the initramfs"));
    }

    /// THE ROLE THE USER SET OUTRANKS THE TARGET'S OWN fstab.
    ///
    /// The fstab pass runs first, so by the time the roles are mounted /home
    /// may already carry whatever that file named. Skipping on "already
    /// mounted" meant a wrong line could never be corrected: the repair wrote
    /// /home = @home, the next run mounted @home from that very file, the role
    /// pointing at the real /home disk was skipped, and the regenerated fstab
    /// said @home again. The loop fed itself, run after run, while every role
    /// on screen looked right.
    #[test]
    fn a_role_replaces_whatever_the_targets_fstab_mounted_there() {
        let src = std::fs::read_to_string("src/system/recovery.rs").expect("readable");
        let code = &src[..src.find("#[cfg(test)]").expect("test module")];
        assert!(
            code.contains("findmnt -no SOURCE --mountpoint /mnt{mp}"),
            "it asks WHAT is mounted there, not merely whether something is"
        );
        assert!(
            code.contains("umount -R /mnt{mp}"),
            "a different device gets unmounted so the user's choice can take over"
        );
        assert!(
            !code.contains("\"mountpoint -q /mnt{mp} && exit 0; \\"),
            "the blanket skip that caused the loop is gone"
        );
    }

    /// The missing-home search must actually run. Its trigger was a variable
    /// set inside a pipeline — a SUBSHELL — so it was always empty by the time
    /// the search checked it, and the search silently never happened.
    #[test]
    fn the_missing_home_search_is_triggered_across_the_subshell() {
        let s = REGENERATE_FSTAB;
        assert!(
            s.contains("/run/home-missing"),
            "a flag file crosses the subshell boundary; a variable does not"
        );
        assert!(
            s.contains("rm -f /run/home-missing"),
            "the flag is cleared before the loop, or a stale one fires it"
        );
    }

    /// A btrfs partition mounted for a role must land on its SUBVOLUME, not on
    /// the filesystem's top level.
    ///
    /// A separate /home disk laid out with an @home subvolume mounts plainly to
    /// a top level that merely CONTAINS @home — so /home looks empty, the real
    /// files sit at /home/@home, and the regenerated fstab writes that empty
    /// mount down. The login then loops with no message at all, and nothing on
    /// screen says why. Reported as "all the partitions were set correctly and
    /// it still will not let me in", which was exactly true.
    #[test]
    fn a_btrfs_role_mount_lands_on_its_subvolume() {
        let src = std::fs::read_to_string("src/system/recovery.rs").expect("readable");
        let code = &src[..src.find("#[cfg(test)]").expect("test module")];
        assert!(
            code.contains("mount -o subvol=$sv"),
            "a role mount retries with the subvolume it found"
        );
        assert!(
            code.contains("ROLE_HOME => \"@home home\""),
            "/home tries @home"
        );
        // And it never leaves the mount point worse than it found it.
        assert!(
            code.contains("mount \\\"$src\\\" /mnt{mp} 2>/dev/null || true"),
            "a failed subvolume mount falls back to the plain one"
        );
    }

    /// When a home directory is missing, the repair GOES AND FINDS IT.
    ///
    /// Being told "/home/<user> does not exist" still leaves the question of
    /// where it went, and the answer is always some partition that nothing
    /// mounted. Working that out from a partition table, at midnight, after
    /// five evenings of this, is not a reasonable thing to ask of anybody.
    #[test]
    fn a_missing_home_is_searched_for_on_the_other_partitions() {
        let s = REGENERATE_FSTAB;
        assert!(s.contains("Looking for those home directories"));
        // Read-only, and only on filesystems that could hold a home.
        assert!(s.contains("mount -o ro") && s.contains("btrfs|ext2|ext3|ext4|xfs|f2fs"));
        // Anything already in use is left alone.
        assert!(s.contains("findmnt -S \"$d\" >/dev/null 2>&1 && continue"));
        // All four spellings, including the btrfs subvolume layout this
        // installer itself produces.
        assert!(s.contains("/run/homeprobe/@home/$u"));
        assert!(s.contains("FOUND $u at"), "it names the device it found");
    }

    /// A generated fstab must name subvolumes BY NAME, never by id.
    ///
    /// findmnt reports both `subvolid=` and `subvol=`. Writing the id down is
    /// not merely redundant on a system with snapper: a rollback gives the
    /// subvolume a NEW id, so an fstab pinned to the old one keeps mounting the
    /// very snapshot that was just rolled back. The name survives; the id does
    /// not.
    #[test]
    fn a_generated_fstab_names_subvolumes_not_their_ids() {
        assert!(
            REGENERATE_FSTAB.contains("s/subvolid=[0-9]*,//"),
            "subvolid is stripped from the options it writes"
        );
    }

    /// The repair says whose home is where, because a login that bounces back
    /// to the display manager is invisible in the fstab alone: a /home line
    /// that mounts an EMPTY subvolume looks exactly like a correct one.
    #[test]
    fn the_repair_reports_the_state_of_each_home_directory() {
        let s = REGENERATE_FSTAB;
        assert!(s.contains("Home directories as they stand right now"));
        assert!(
            s.contains("DOES NOT EXIST. The login will loop"),
            "a missing home is named as the cause of the loop"
        );
        assert!(
            s.contains("EMPTY. The session will fail"),
            "an empty home is called out too — the commonest wrong mount"
        );
    }

    /// THE REPAIR MUST NOT DEPEND ON TOOLS THAT ONLY EXIST ON THE ISO.
    ///
    /// It called `fstabgen`, which is an artools command, falling back to
    /// `genfstab` from arch-install-scripts. Both live on the live image and
    /// NEITHER is installed in the target — and this repair runs inside the
    /// target. So both failed with "command not found", the error was sent to
    /// /dev/null, and `set -e` killed the script before it wrote anything. The
    /// fstab was never touched: the same 126 bytes with the same timestamp,
    /// run after run, while the screen reported the repair as done. Four
    /// evenings were spent on that.
    #[test]
    fn the_fstab_repair_does_not_need_tools_the_target_lacks() {
        let s = REGENERATE_FSTAB;
        assert!(s.contains("gen_fstab() {"), "it carries its own generator");
        assert!(
            s.contains("command -v fstabgen"),
            "Artix's own fstabgen is used when it is actually present"
        );
        // Arch's genfstab is deliberately not a fallback here: this is an Artix
        // installer, and a system without artools has no reason to carry
        // arch-install-scripts either.
        assert!(
            !s.contains("genfstab -U /"),
            "no Arch tool is invoked as a fallback"
        );
        // One line per mount point, or a stacked mount writes two.
        assert!(
            s.contains("last[$1] = $0"),
            "duplicate mount points are collapsed to the one actually in use"
        );
        assert!(
            !s.contains("fstabgen -U / > /etc/fstab.new 2>/dev/null"),
            "the silent call that could not work is gone"
        );
        // findmnt and blkid are util-linux: present in any Linux root.
        assert!(s.contains("findmnt -rn --real") && s.contains("blkid -o value -s UUID"));
        // Transient mounts must never reach an fstab.
        assert!(
            s.contains("fuse|fuse.*|fuseblk") && s.contains("/tmp/*|/run/*"),
            "AppImage and other throwaway mounts are filtered out"
        );
    }

    /// The repair CHECKS ITS OWN RESULT against the install record.
    ///
    /// "It produced a file" is not "it produced the right file": fstabgen can
    /// only describe what happens to be mounted. The record names every mount
    /// point the system was built with, so the repair can say concretely what
    /// is still missing rather than reporting success and leaving the machine
    /// unbootable for the fifth time.
    #[test]
    fn the_repair_cross_checks_its_result_against_the_install_record() {
        let s = REGENERATE_FSTAB;
        assert!(
            s.contains("/boot/artix-tui-layout.conf"),
            "it looks for the record on the ESP as well as in /etc"
        );
        assert!(
            s.contains("does NOT cover"),
            "it names the mount points the new fstab is missing"
        );
        // The root is skipped in that comparison: it is checked separately and
        // refusing on it would be a second, contradictory gate.
        assert!(s.contains("[ \"$mp\" = / ] && continue"));
    }

    /// The install record SUPPLEMENTS the guess; it never wipes it.
    ///
    /// The record only describes what was a mount point at install time. A
    /// /home that is a btrfs subvolume carries the root filesystem's UUID,
    /// which cannot be matched from outside an encrypted root, and swap has no
    /// mount point at all. Starting the table at ROLE_NONE therefore marked
    /// every partition the record could not speak for as "leave alone" — worse
    /// than the guess it replaced, and it had to be corrected by hand.
    #[test]
    fn the_record_only_overrules_the_guess_where_it_has_something_to_say() {
        let src = std::fs::read_to_string("src/system/recovery.rs").expect("readable");
        let code = &src[..src.find("#[cfg(test)]").expect("test module")];
        let f = &code[code.find("pub fn roles_from_record").expect("exists")..];
        assert!(
            f.contains("let mut roles = suggest_all(parts);"),
            "the table starts as the guess, not as a row of ROLE_NONE"
        );
        // With no record at all, nothing is claimed.
        let parts = vec![Partition {
            path: "/dev/sda1".into(),
            size: "20G".into(),
            fstype: "btrfs".into(),
            label: String::new(),
        }];
        assert!(roles_from_record(&parts, &[]).is_none());
    }

    /// Swap is in the record under its own verb, because it has no mount point
    /// and the mount loop that built the record could never have seen it.
    #[test]
    fn swap_is_recorded_and_read_back() {
        let src = std::fs::read_to_string("src/system/recovery.rs").expect("readable");
        let code = &src[..src.find("#[cfg(test)]").expect("test module")];
        assert!(
            code.contains("f[0] == \"swap\""),
            "the reader understands swap lines"
        );
        assert!(
            code.contains("\"swap\" => ROLE_SWAP"),
            "and maps them to the swap role"
        );
    }

    /// A REGENERATED fstab MUST CARRY THE BTRFS SUBVOLUMES AND THE SWAP.
    ///
    /// On this layout /var/log, /var/cache, /home and /.snapshots are separate
    /// subvolumes, not directories. Recovery mounted only `@`, so fstabgen —
    /// which writes down what is mounted — produced a file with no @log line.
    /// The "repaired" system then failed to boot in exactly the same way as
    /// before: every dinit service reporting "opening log file: No such file or
    /// directory", then swap failing too, because fstabgen cannot see swap that
    /// is not switched on. Reported four times as "the fstab repair did
    /// nothing", and it was true every time.
    #[test]
    fn a_regenerated_fstab_carries_the_subvolumes_and_the_swap() {
        let s = REGENERATE_FSTAB;
        for sv in ["@home /home", "@log /var/log", "@cache /var/cache"] {
            assert!(s.contains(sv), "the repair mounts {sv} before generating");
        }
        assert!(
            s.contains("blkid -t TYPE=swap"),
            "swap is found by scanning, not by hoping it is switched on"
        );
        assert!(
            s.contains("$3 == \"swap\""),
            "a swap line is only added when fstabgen did not produce one"
        );
        // And it warns when the result still lacks the log subvolume.
        assert!(
            s.contains("no /var/log subvolume line"),
            "an incomplete result is reported, not passed off as success"
        );
    }

    /// An encrypted /boot must be OPENED before it is mounted, and closed again
    /// on the way out.
    ///
    /// Full-disk encryption puts /boot in its own LUKS container. The role
    /// mounting handed the raw partition to `mount`, which cannot work, so a
    /// system with an encrypted /boot got a root and nothing else — on exactly
    /// the layout this installer offers as its strongest option.
    #[test]
    fn an_encrypted_boot_partition_is_unlocked_and_then_released() {
        let src = std::fs::read_to_string("src/system/recovery.rs").expect("readable");
        let code = &src[..src.find("#[cfg(test)]").expect("test module")];
        assert!(
            code.contains("blkid -o value -s TYPE"),
            "the mount step asks whether the device is a LUKS container"
        );
        assert!(
            code.contains("cryptsetup open") && code.contains("--key-file"),
            "it opens it, trying the installer's keyfile first"
        );
        // And whatever it opened is closed again, or a second run finds the
        // mapper already there and the disks still held.
        let cleanup = &code[code.find("pub fn cleanup").expect("cleanup exists")..];
        assert!(
            cleanup.contains("/dev/mapper/recov_*"),
            "cleanup releases the mappers recovery itself created"
        );
    }

    /// The record must place partitions BY UUID, never by device name.
    ///
    /// A disk that came up as sda last boot and nvme0n1 this one is the same
    /// filesystem; matching on the name would silently mis-assign every role on
    /// exactly the machines where recovery matters.
    #[test]
    fn the_layout_record_places_partitions_by_uuid() {
        let src = std::fs::read_to_string("src/system/recovery.rs").expect("readable");
        let code = &src[..src.find("#[cfg(test)]").expect("test module")];
        let f = &code[code
            .find("pub fn roles_from_record")
            .expect("the function exists")..];
        let f = &f[..f.find("\n/// ").unwrap_or(f.len())];
        assert!(
            f.contains("blkid -o value -s UUID"),
            "each partition is identified by its UUID"
        );
        assert!(
            f.contains("find(|(_, u)| u == uuid)"),
            "the record is matched on that UUID"
        );
    }

    /// An encrypted root cannot be matched from outside: from there the
    /// partition carries the LUKS container's UUID, while the record holds the
    /// filesystem UUID from inside it. The record still places /boot and the
    /// ESP correctly, so the root must fall back to a guess rather than being
    /// left unset — which would leave the screen with nothing to mount.
    #[test]
    fn an_encrypted_root_still_gets_a_role_from_a_partial_record() {
        let parts = vec![
            Partition {
                path: "/dev/nvme0n1p1".into(),
                size: "512M".into(),
                fstype: "vfat".into(),
                label: String::new(),
            },
            Partition {
                path: "/dev/nvme0n1p2".into(),
                size: "20G".into(),
                fstype: "crypto_LUKS".into(),
                label: String::new(),
            },
        ];
        // A record naming a root UUID that matches nothing visible from here.
        let record = vec![
            ("/".to_string(), "unreachable-inside-luks".to_string()),
            ("/boot".to_string(), "no-such-uuid-either".to_string()),
        ];
        // blkid finds nothing for these fake devices, so nothing matches and the
        // function reports "use the guess" rather than a table of ROLE_NONE.
        assert!(roles_from_record(&parts, &record).is_none());
        // And an empty record never claims to know anything.
        assert!(roles_from_record(&parts, &[]).is_none());
    }

    /// The fstab repair must never install a file that cannot boot the system.
    ///
    /// It did. Its guard asked whether /boot was EMPTY, and an ESP mounted at
    /// /boot/efi creates the directory /boot/efi — so the guard passed, an
    /// fstab with no /boot line was written, and the machine still would not
    /// boot with the repair itself to blame. Reported as "regenerating the
    /// fstab did nothing".
    #[test]
    fn the_fstab_repair_refuses_to_install_a_file_that_cannot_boot() {
        let s = REGENERATE_FSTAB;
        assert!(
            s.contains("ls /boot/vmlinuz-*"),
            "the gate is a kernel, not an empty directory"
        );
        assert!(
            !s.contains("ls -A /boot"),
            "the emptiness test is gone, not merely unused"
        );
        // It writes to a scratch file, checks it, and only then installs it.
        assert!(s.contains("/etc/fstab.new"), "generated to a scratch file");
        let install = s.find("mv /etc/fstab.new /etc/fstab").expect("it installs");
        let verify = s
            .find("$2 == \"/\"")
            .expect("it verifies a root entry exists");
        assert!(
            verify < install,
            "the check happens BEFORE the file is used"
        );
        assert!(
            s.contains("rm -f /etc/fstab.new"),
            "a rejected file is not left behind"
        );
        // And it says so when /boot is missing from the result.
        assert!(
            s.contains("$2 == \"/boot\""),
            "it reports a missing /boot line"
        );
    }

    /// /boot IS JUDGED BY WHETHER A KERNEL IS THERE, never by whether the
    /// directory is empty.
    ///
    /// Emptiness failed in the exact case both of these guards exist for.
    /// Mounting an ESP at /boot/efi CREATES the directory /boot/efi, so /boot
    /// stops being empty — the search for the real /boot then bailed, and the
    /// diagnosis reported a missing kernel, a missing initramfs and a missing
    /// bootloader on a system whose kernels were simply on a partition that
    /// nothing had mounted. Three findings, all false, from one directory that
    /// existed. On this installer's own root-scope encrypted layout the kernels
    /// live on that ESP, so this is not an exotic case: it is the default one.
    ///
    /// The mounting runs real commands against real disks, so what a test can
    /// check is that neither guard has drifted back to asking about emptiness.
    #[test]
    fn boot_is_judged_by_a_kernel_not_by_an_empty_directory() {
        let src = std::fs::read_to_string("src/system/recovery.rs")
            .expect("src/system/recovery.rs is readable");
        let code = &src[..src.find("#[cfg(test)]").expect("the test module exists")];

        // The discovery step must gate on a kernel being present.
        assert!(
            code.contains("ls /mnt/boot/vmlinuz-* >/dev/null 2>&1 && exit 0"),
            "the /boot search gives up only when a kernel is already there"
        );
        // The diagnosis must decide "unknown" from a kernel plus a mount, not
        // from a directory listing.
        assert!(
            code.contains("let boot_unknown = !has_kernel && !boot_mounted;"),
            "boot_unknown is computed from a kernel and a mount"
        );
        assert!(
            !code.contains("boot_dir_empty"),
            "the emptiness test is gone, not merely unused"
        );
    }

    /// lsblk sizes are human strings, and the whole role suggestion turns on
    /// reading them. A misparse does not fail loudly — it silently offers
    /// "root" on a 512 MiB ESP, which is exactly the mis-selection the role
    /// list exists to prevent.
    #[test]
    fn lsblk_sizes_parse_into_mib() {
        assert_eq!(size_mib("512M"), 512);
        assert_eq!(size_mib("20G"), 20 * 1024);
        assert_eq!(size_mib("1.5G"), 1536);
        // Some locales render the decimal separator as a comma.
        assert_eq!(size_mib("1,5G"), 1536);
        assert_eq!(size_mib("2T"), 2 * 1024 * 1024);
        // Unreadable sizes must come back as 0 rather than a wrong number.
        assert_eq!(size_mib(""), 0);
        assert_eq!(size_mib("?"), 0);
    }

    /// A partition small enough to be a boot partition is never offered as a
    /// root, and a big one is never offered as an ESP. This is the user-visible
    /// promise of the role ring.
    #[test]
    fn the_role_ring_never_offers_a_nonsense_role() {
        let esp = part("/dev/nvme0n1p1", "512M", "vfat", "");
        assert!(!roles_for(&esp).contains(&ROLE_ROOT));
        assert!(roles_for(&esp).contains(&ROLE_ESP));

        let root = part("/dev/nvme0n1p2", "20G", "btrfs", "");
        assert!(!roles_for(&root).contains(&ROLE_ESP));
        assert!(roles_for(&root).contains(&ROLE_ROOT));

        // Swap is unmistakable, so its ring stays short.
        let sw = part("/dev/nvme0n1p3", "9G", "swap", "");
        assert_eq!(roles_for(&sw), vec![ROLE_NONE, ROLE_SWAP, ROLE_DATA]);
    }

    /// AN ENCRYPTED ROOT MUST STILL BE SUGGESTED AS THE ROOT. Its filesystem
    /// reads as `crypto_LUKS` — there is no way to see btrfs inside it without
    /// unlocking first — so a suggestion that only recognised real filesystems
    /// would leave the encrypted install, the one this project ships by
    /// default, with no root proposed at all.
    #[test]
    fn an_encrypted_partition_is_still_a_root_candidate() {
        let luks = part("/dev/nvme0n1p2", "20G", "crypto_LUKS", "");
        assert_eq!(suggest_role(&luks, false), ROLE_ROOT);
        assert!(roles_for(&luks).contains(&ROLE_ROOT));
    }

    /// Only ONE root is ever suggested. Two would mean two candidate mounts at
    /// /mnt and a silent choice between them.
    #[test]
    fn only_the_first_big_filesystem_is_proposed_as_the_root() {
        let parts = vec![
            part("/dev/sda1", "512M", "vfat", ""),
            part("/dev/sda2", "20G", "btrfs", ""),
            part("/dev/sdb1", "40G", "ext4", ""),
        ];
        let roles = suggest_all(&parts);
        assert_eq!(roles[0], ROLE_ESP);
        assert_eq!(roles[1], ROLE_ROOT);
        assert_eq!(roles.iter().filter(|r| **r == ROLE_ROOT).count(), 1);
    }

    /// The key stick is vfat and boot-partition sized, so nothing but its label
    /// tells it apart — and mounting it at /boot/efi would be wrong twice over.
    #[test]
    fn the_artixkey_stick_is_recognised_by_its_label() {
        let key = part("/dev/sdc1", "1G", "vfat", "ARTIXKEY");
        assert_eq!(suggest_role(&key, false), ROLE_KEY);
    }

    /// /boot has to be mounted before /boot/efi goes inside it. The mount step
    /// orders by separator count, so the two must differ in that count.
    #[test]
    fn boot_sorts_before_the_esp_inside_it() {
        let boot = mount_point(ROLE_BOOT, "").unwrap();
        let esp = mount_point(ROLE_ESP, "").unwrap();
        assert!(boot.matches('/').count() < esp.matches('/').count());
        // Roles that are not mounts must not claim one.
        assert!(mount_point(ROLE_ROOT, "").is_none());
        assert!(mount_point(ROLE_SWAP, "").is_none());
        assert!(mount_point(ROLE_KEY, "").is_none());
    }

    /// Recovery must not use /etc/fstab to decide whether something is a root.
    ///
    /// It did, and that was exactly backwards: a missing or broken fstab is one
    /// of the commonest reasons a system will not boot, so recovery refused to
    /// open precisely the system it exists for. Found by testing it honestly —
    /// `mv /etc/fstab /etc/fstab.bak`, then trying to repair it — and being told
    /// the root was "not a Linux root".
    ///
    /// The mounting logic runs real commands against real disks, so there is
    /// nothing here a unit test can drive. What CAN be checked is that the gate
    /// asks about structure, which is what this does.
    #[test]
    fn the_root_check_does_not_depend_on_the_file_most_likely_to_be_broken() {
        let whole = std::fs::read_to_string("src/system/recovery.rs")
            .expect("src/system/recovery.rs is readable");
        // Everything past #[cfg(test)] is this test, which quotes the very
        // string it forbids — searching the whole file finds itself.
        let src = match whole.find("#[cfg(test)]") {
            Some(i) => &whole[..i],
            None => &whole[..],
        };
        let guard = src
            .split("2a-guard")
            .nth(1)
            .expect("the root guard is gone")
            .split("2b)")
            .next()
            .expect("the guard has no end");

        assert!(
            guard.contains("test -d /mnt/etc && test -d /mnt/usr"),
            "the root guard no longer checks the directory structure"
        );
        // The old gate, exactly. It may still be TESTED for — that is the
        // diagnosis below — but never used to refuse.
        assert!(
            !src.contains(r#"if run_shell("test -e /mnt/etc/fstab").is_err()"#),
            "the root guard is gating on /etc/fstab again — the very file a \
             broken system is most likely to be missing"
        );
        // A missing fstab must still be REPORTED: it is the diagnosis.
        assert!(
            src.contains("fstab_missing"),
            "a missing fstab is no longer mentioned to the person repairing it"
        );
    }
}
