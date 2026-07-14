//! Disk backend (spec step 7). Two responsibilities:
//!   * enumerate real block devices via `lsblk --json` (stable machine format),
//!   * build an ordered, explainable plan of shell-out commands to partition,
//!     format, create mountpoints and mount — for both BIOS and UEFI.
//!
//! The plan is just data (Vec of (program, args)). The Summary screen runs it
//! through `runner::spawn` so every step streams into the log and a failure
//! halts the sequence and lets the user go back and fix things.

use crate::app::InstallConfig;
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct Disk {
    pub path: String, // /dev/sda
    pub size: String, // human, e.g. "256G"
    pub model: String,
    pub removable: bool,
    /// Total size in bytes (from `lsblk -b`), for the "disk may be too small"
    /// pre-flight warning. 0 if lsblk didn't report it.
    pub size_bytes: u64,
    /// True when one of this disk's partitions carries the live ISO filesystem
    /// (iso9660) — i.e. this is the USB/medium the installer itself booted from.
    /// Flagged so the disk list can warn "you are installing onto your boot
    /// medium"; the user is still allowed to proceed if they really mean to.
    pub is_live: bool,
}

#[derive(Deserialize)]
struct LsblkRoot {
    blockdevices: Vec<LsblkDev>,
}

#[derive(Deserialize)]
struct LsblkDev {
    name: String,
    size: Option<String>,
    #[serde(rename = "type")]
    dtype: Option<String>,
    model: Option<String>,
    rm: Option<bool>,
    #[serde(default)]
    fstype: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    children: Vec<LsblkDev>,
}

/// An existing partition that already carries a filesystem (so it can be mounted
/// as-is, without formatting — e.g. an NTFS Windows volume to dual-boot with).
#[derive(Clone, Debug)]
pub struct Partition {
    pub path: String,   // /dev/sdb1
    pub parent: String, // /dev/sdb
    pub size: String,
    pub fstype: String, // "ntfs", "ext4", "vfat", ...
    pub label: String,
}

/// List whole disks (type == "disk"), skipping loop/rom devices.
pub fn list() -> Result<Vec<Disk>, String> {
    // Human-readable sizes for display.
    let json_h = super::runner::capture("lsblk", &["--json", "-o", "NAME,SIZE,TYPE,MODEL,RM"])?;
    let parsed: LsblkRoot = serde_json::from_str(&json_h).map_err(|e| e.to_string())?;

    // Byte sizes (-b) for the size-threshold pre-flight check, keyed by device
    // name. A separate query keeps the human list above intact.
    let bytes_by_name = byte_sizes();

    // Which whole disks hold the live ISO (iso9660 on any child partition) —
    // that disk is the boot medium the installer is running from.
    let live_disks = live_iso_disks();

    let disks = parsed
        .blockdevices
        .into_iter()
        .filter(|d| d.dtype.as_deref() == Some("disk"))
        .map(|d| {
            let path = format!("/dev/{}", d.name);
            Disk {
                size_bytes: bytes_by_name.get(&d.name).copied().unwrap_or(0),
                is_live: live_disks.contains(&path),
                path,
                size: d.size.unwrap_or_else(|| "?".into()),
                model: d.model.unwrap_or_default().trim().to_string(),
                removable: d.rm.unwrap_or(false),
            }
        })
        .collect();
    Ok(disks)
}

/// Byte size of every whole disk, name → bytes, via `lsblk -b`. Used only for
/// the "disk may be too small" warning; a failure yields an empty map (the
/// check then simply never fires, which is the safe, non-blocking default).
///
/// NOTE: with `-b`, lsblk emits SIZE as a JSON *number* (e.g. 274877906944),
/// not the human string the main `LsblkDev` expects, so this needs its own
/// deserialize shape with `size: Option<u64>`. Reusing `LsblkDev` here would
/// make serde fail to parse the whole document and silently disable the check.
fn byte_sizes() -> std::collections::HashMap<String, u64> {
    #[derive(Deserialize)]
    struct BytesRoot {
        blockdevices: Vec<BytesDev>,
    }
    #[derive(Deserialize)]
    struct BytesDev {
        name: String,
        size: Option<u64>,
        #[serde(rename = "type")]
        dtype: Option<String>,
    }

    let mut m = std::collections::HashMap::new();
    if let Ok(json) = super::runner::capture(
        "lsblk",
        &["--json", "-o", "NAME,SIZE,TYPE", "-b", "--nodeps"],
    ) {
        if let Ok(root) = serde_json::from_str::<BytesRoot>(&json) {
            for d in root.blockdevices {
                if d.dtype.as_deref() == Some("disk") {
                    if let Some(b) = d.size {
                        m.insert(d.name, b);
                    }
                }
            }
        }
    }
    m
}

/// Whole disks that carry the live ISO: any disk with a child partition whose
/// filesystem is iso9660 is the medium the installer booted from. Returns the
/// `/dev/<name>` paths. Best-effort — on failure the set is empty and nothing
/// is flagged (never blocks; only informs).
fn live_iso_disks() -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    if let Ok(json) = super::runner::capture("lsblk", &["--json", "-o", "NAME,TYPE,FSTYPE"]) {
        if let Ok(root) = serde_json::from_str::<LsblkRoot>(&json) {
            for d in root.blockdevices {
                if d.dtype.as_deref() != Some("disk") {
                    continue;
                }
                let this_is_live = d.fstype.as_deref() == Some("iso9660")
                    || d.children
                        .iter()
                        .any(|c| c.fstype.as_deref() == Some("iso9660"));
                if this_is_live {
                    set.insert(format!("/dev/{}", d.name));
                }
            }
        }
    }
    set
}

/// Cached whole-disk list (lsblk runs once). Shared by the disk and storage
/// screens so neither re-shells lsblk on every frame.
pub fn disks_list() -> &'static Vec<Disk> {
    static D: OnceLock<Vec<Disk>> = OnceLock::new();
    D.get_or_init(|| list().unwrap_or_default())
}

/// All partitions that already have a filesystem, across every disk. Used to
/// offer mounting an existing volume (notably NTFS Windows partitions) without
/// formatting. Swap/LUKS/RAID/LVM members are skipped (not plain data mounts).
pub fn list_partitions() -> Result<Vec<Partition>, String> {
    let json = super::runner::capture("lsblk", &["--json", "-o", "NAME,SIZE,TYPE,FSTYPE,LABEL"])?;
    let parsed: LsblkRoot = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let skip = ["swap", "crypto_LUKS", "linux_raid_member", "LVM2_member"];
    let mut out = Vec::new();
    for dev in &parsed.blockdevices {
        if dev.dtype.as_deref() != Some("disk") {
            continue;
        }
        let parent = format!("/dev/{}", dev.name);
        for ch in &dev.children {
            if ch.dtype.as_deref() != Some("part") {
                continue;
            }
            let fstype = ch.fstype.clone().unwrap_or_default();
            if fstype.is_empty() || skip.iter().any(|s| *s == fstype) {
                continue;
            }
            out.push(Partition {
                path: format!("/dev/{}", ch.name),
                parent: parent.clone(),
                size: ch.size.clone().unwrap_or_else(|| "?".into()),
                fstype,
                label: ch.label.clone().unwrap_or_default(),
            });
        }
    }
    Ok(out)
}

/// Partition naming differs for nvme/mmc (need a 'p' before the number).
pub(crate) fn part(disk: &str, n: u32) -> String {
    let needs_p = disk
        .rsplit('/')
        .next()
        .map(|n| {
            n.chars()
                .last()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
        })
        .unwrap_or(false);
    if needs_p {
        format!("{disk}p{n}")
    } else {
        format!("{disk}{n}")
    }
}

/// One planned action. Kept as owned strings so the plan can cross threads.
#[derive(Debug, Clone)]
pub struct Action {
    pub program: String,
    pub args: Vec<String>,
    /// If true, run this step on the foreground terminal (stdin/stdout/stderr
    /// inherited) so the user can answer interactive prompts (e.g. pacman
    /// provider selection), instead of streaming output into the TUI.
    pub interactive: bool,
}

fn act(program: &str, args: &[&str]) -> Action {
    Action {
        program: program.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        interactive: false,
    }
}

/// Build the full partition→format→mount plan.
///
/// Layout (without encryption):
///   UEFI: GPT — p1 ESP 512M (fat32, /boot), [swap], pN root (/)
///   BIOS: GPT — p1 bios_boot 1M, [swap], pN root (/)
///
/// Layout (encryption, scope="root"): same as above, but root is LUKS2; the
/// ESP / BIOS boot area stays plaintext (kernel+initramfs unencrypted).
///
/// Layout (encryption, scope="full", UEFI): a real encrypted /boot —
///   p1 ESP 512M (fat32, /boot/efi — holds only GRUB),
///   p2 BOOT 1G (LUKS1 → cryptboot, ext4, /boot — holds kernel+initramfs+grub),
///   [swap], pN ROOT (LUKS2 → cryptroot, /).
/// GRUB (GRUB_ENABLE_CRYPTODISK) unlocks /boot at boot to read the kernel.
/// /boot uses LUKS1 because GRUB's LUKS2 support is limited/fragile.
///
/// `swap_gib == 0` means no swap partition.
/// Takes the config rather than a dozen loose arguments.
///
/// It used to take twelve — SIX OF THEM CONSECUTIVE BOOLS:
///
///     encrypt, btrfs_subvolumes, btrfs_compress, btrfs_discard,
///     mount_noatime, home_external
///
/// Swap any two of those at a call site and the compiler says nothing. In a
/// function that PARTITIONS A DISK, that mistake encrypts the wrong volume or
/// wipes the wrong partition — and it would type-check perfectly. The config is
/// already at hand at the only call site; passing it means the field names do
/// the disambiguating, and the compiler checks them.
pub fn build_plan(c: &InstallConfig, luks_pass: &str, home_external: bool) -> Vec<Action> {
    let disk = c.disk.as_str();
    let uefi = c.boot_mode.is_uefi();
    let swap_gib = c.swap_gib;
    let root_fs = c.root_fs.as_str();
    let encrypt = c.encrypt_disk;
    let passphrase = luks_pass;
    let scope = c.encrypt_scope.as_str();
    let btrfs_subvolumes = c.btrfs_subvolumes;
    let btrfs_compress = c.btrfs_compress;
    let btrfs_discard = c.btrfs_discard;
    let mount_noatime = c.mount_noatime;

    let mut plan = Vec::new();
    let swap = swap_gib > 0;
    // A real separate encrypted /boot only applies to UEFI full-disk encryption.
    let enc_boot = encrypt && scope == "full" && uefi;
    let pass_esc = passphrase.replace('\'', "'\\''");

    // Tear down any leftover state from a previous (possibly failed) run before
    // touching the disks, so a retry doesn't fail with "Device or resource busy":
    // unmount everything under /mnt, switch off swap, and close every open LUKS
    // mapper (safe in the installer/live environment, which uses none of its own).
    // Best-effort — a clean first run has nothing to undo. This frees both the
    // root disk and any extra disks (their mappers/mounts) for re-wiping.
    plan.push(act(
        "sh",
        &["-c", "umount -R /mnt 2>/dev/null; awk 'NR>1 && $1 !~ /zram/ {print $1}' /proc/swaps 2>/dev/null | while read s; do swapoff \"$s\" 2>/dev/null; done; for m in $(dmsetup ls --target crypt 2>/dev/null | grep -v 'No devices' | awk '{print $1}'); do cryptsetup close \"$m\" 2>/dev/null; done; udevadm settle 2>/dev/null; true"],
    ));

    // Wipe existing signatures and GPT, then a fresh GPT.
    plan.push(act("wipefs", &["-a", disk]));
    plan.push(act("sgdisk", &["--zap-all", disk]));

    let mut idx = 1u32;
    let (esp_n, bootpart_n, swap_n, root_n);

    if uefi {
        // ESP (always plaintext FAT32).
        plan.push(act(
            "sgdisk",
            &[
                "-n",
                &format!("{idx}:0:+512M"),
                "-t",
                &format!("{idx}:ef00"),
                "-c",
                &format!("{idx}:EFI"),
                disk,
            ],
        ));
        esp_n = Some(idx);
        idx += 1;
    } else {
        // BIOS boot partition (1 MiB, type ef02).
        plan.push(act(
            "sgdisk",
            &[
                "-n",
                &format!("{idx}:0:+1M"),
                "-t",
                &format!("{idx}:ef02"),
                "-c",
                &format!("{idx}:BIOSBOOT"),
                disk,
            ],
        ));
        esp_n = Some(idx);
        idx += 1;
    }

    // Separate encrypted /boot partition (UEFI full only).
    if enc_boot {
        plan.push(act(
            "sgdisk",
            &[
                "-n",
                &format!("{idx}:0:+1G"),
                "-t",
                &format!("{idx}:8300"),
                "-c",
                &format!("{idx}:BOOT"),
                disk,
            ],
        ));
        bootpart_n = Some(idx);
        idx += 1;
    } else {
        bootpart_n = None;
    }

    if swap {
        plan.push(act(
            "sgdisk",
            &[
                "-n",
                &format!("{idx}:0:+{swap_gib}G"),
                "-t",
                &format!("{idx}:8200"),
                "-c",
                &format!("{idx}:SWAP"),
                disk,
            ],
        ));
        swap_n = Some(idx);
        idx += 1;
    } else {
        swap_n = None;
    }

    // Root takes the rest.
    plan.push(act(
        "sgdisk",
        &[
            "-n",
            &format!("{idx}:0:0"),
            "-t",
            &format!("{idx}:8300"),
            "-c",
            &format!("{idx}:ROOT"),
            disk,
        ],
    ));
    root_n = idx;

    plan.push(act("partprobe", &[disk]));

    let esp_dev = esp_n.map(|n| part(disk, n));
    let root_part = part(disk, root_n);

    // ESP: plaintext FAT32 (UEFI only).
    if uefi {
        if let Some(ref e) = esp_dev {
            plan.push(act("mkfs.fat", &["-F32", e]));
        }
    }

    // Encrypted /boot (UEFI full): LUKS1 container → cryptboot → ext4.
    if let Some(bn) = bootpart_n {
        let boot_part = part(disk, bn);
        plan.push(act(
            "sh",
            &[
                "-c",
                &format!(
                    "printf '%s' '{pass}' | cryptsetup -q luksFormat --type luks1 {dev} -",
                    pass = pass_esc,
                    dev = boot_part
                ),
            ],
        ));
        plan.push(act(
            "sh",
            &[
                "-c",
                &format!(
                    "printf '%s' '{pass}' | cryptsetup open {dev} cryptboot -",
                    pass = pass_esc,
                    dev = boot_part
                ),
            ],
        ));
        plan.push(act("mkfs.ext4", &["-F", "/dev/mapper/cryptboot"]));
    }

    // Root: optionally LUKS2 → cryptroot.
    let root_fsdev: String = if encrypt {
        plan.push(act(
            "sh",
            &[
                "-c",
                &format!(
                    "printf '%s' '{pass}' | cryptsetup -q luksFormat --type luks2 {dev} -",
                    pass = pass_esc,
                    dev = root_part
                ),
            ],
        ));
        plan.push(act(
            "sh",
            &[
                "-c",
                &format!(
                    "printf '%s' '{pass}' | cryptsetup open {dev} cryptroot -",
                    pass = pass_esc,
                    dev = root_part
                ),
            ],
        ));
        "/dev/mapper/cryptroot".to_string()
    } else {
        root_part.clone()
    };

    // Format root with the chosen filesystem on the (possibly mapped) device.
    match root_fs {
        "btrfs" => plan.push(act("mkfs.btrfs", &["-f", &root_fsdev])),
        "xfs" => plan.push(act("mkfs.xfs", &["-f", &root_fsdev])),
        "f2fs" => plan.push(act("mkfs.f2fs", &["-f", &root_fsdev])),
        "jfs" => plan.push(act("mkfs.jfs", &["-q", &root_fsdev])),
        "ext3" => plan.push(act("mkfs.ext3", &["-F", &root_fsdev])),
        "ext2" => plan.push(act("mkfs.ext2", &["-F", &root_fsdev])),
        _ => plan.push(act("mkfs.ext4", &["-F", &root_fsdev])),
    }
    if let Some(n) = swap_n {
        let swap_dev = part(disk, n);
        plan.push(act("mkswap", &[&swap_dev]));
        plan.push(act("swapon", &[&swap_dev]));
    }

    // Build the mount-option strings from the chosen toggles. `noatime` is a
    // common option valid on any filesystem; btrfs and f2fs layer their own
    // extras on top. fstabgen -U later copies whatever we mount with into fstab,
    // so getting the live mount right is all that's needed.
    let common: Vec<&str> = if mount_noatime {
        vec!["noatime"]
    } else {
        vec![]
    };

    let mut btrfs_extra = common.clone();
    if btrfs_compress {
        btrfs_extra.push("compress=zstd");
    }
    if btrfs_discard {
        btrfs_extra.push("discard=async");
    }
    let btrfs_extra = btrfs_extra.join(","); // "" when nothing selected

    let common = common.join(",");

    // Helper: "subvol=NAME" plus any btrfs extras, comma-joined.
    let subvol_opt = |name: &str| -> String {
        if btrfs_extra.is_empty() {
            format!("subvol={name}")
        } else {
            format!("subvol={name},{btrfs_extra}")
        }
    };

    if root_fs == "btrfs" && btrfs_subvolumes {
        // Plain mount first so we can create the @-style subvolumes, then
        // remount @ as / (the rest are mounted after /boot, below).
        plan.push(act("mount", &[&root_fsdev, "/mnt"]));
        for sv in ["@", "@home", "@snapshots", "@log", "@cache"] {
            plan.push(act(
                "btrfs",
                &["subvolume", "create", &format!("/mnt/{sv}")],
            ));
        }
        plan.push(act("umount", &["/mnt"]));
        let root_opt = subvol_opt("@");
        plan.push(act("mount", &["-o", &root_opt, &root_fsdev, "/mnt"]));
    } else if root_fs == "btrfs" && !btrfs_extra.is_empty() {
        plan.push(act("mount", &["-o", &btrfs_extra, &root_fsdev, "/mnt"]));
    } else if !common.is_empty() {
        // Any other filesystem with noatime selected.
        plan.push(act("mount", &["-o", &common, &root_fsdev, "/mnt"]));
    } else {
        plan.push(act("mount", &[&root_fsdev, "/mnt"]));
    }

    if enc_boot {
        // Encrypted /boot is its own filesystem; the ESP nests under /boot/efi.
        plan.push(act("mkdir", &["-p", "/mnt/boot"]));
        plan.push(act("mount", &["/dev/mapper/cryptboot", "/mnt/boot"]));
        plan.push(act("mkdir", &["-p", "/mnt/boot/efi"]));
        if let Some(ref e) = esp_dev {
            plan.push(act("mount", &[e, "/mnt/boot/efi"]));
        }
    } else if uefi {
        // Plain UEFI: ESP is /boot directly.
        plan.push(act("mkdir", &["-p", "/mnt/boot"]));
        if let Some(ref e) = esp_dev {
            plan.push(act("mount", &[e, "/mnt/boot"]));
        }
    }

    // With btrfs subvolumes, mount the rest at their targets (after / and /boot
    // exist). @snapshots → /.snapshots is where snapper/Timeshift store images;
    // @log and @cache keep /var/log and /var/cache OUT of root snapshots.
    if root_fs == "btrfs" && btrfs_subvolumes {
        plan.push(act(
            "mkdir",
            &[
                "-p",
                "/mnt/home",
                "/mnt/.snapshots",
                "/mnt/var/log",
                "/mnt/var/cache",
            ],
        ));
        for (sv, target) in [
            ("@home", "/mnt/home"),
            ("@snapshots", "/mnt/.snapshots"),
            ("@log", "/mnt/var/log"),
            ("@cache", "/mnt/var/cache"),
        ] {
            // When /home is provided by a separate disk, don't mount @home over
            // its mountpoint — the dedicated disk will be mounted there instead.
            if sv == "@home" && home_external {
                continue;
            }
            let opt = subvol_opt(sv);
            plan.push(act("mount", &["-o", &opt, &root_fsdev, target]));
        }
    }

    plan
}
