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
    /// Partition-table type as lsblk reports it: "gpt", "dos", or "" when the
    /// disk has no table yet. BIOS booting needs a 1 MiB BIOS-boot partition on
    /// GPT (GRUB embeds core.img there) but nothing on MBR, where it uses the
    /// post-MBR gap — so the manual editor can only ask for the right thing if
    /// it knows which table it is looking at.
    pub pttype: String,
    /// lsblk ROTA: true = spinning HDD, false = SSD/flash. The disk-wipe picker
    /// uses it to default to the right method (zero-fill for an HDD, a TRIM /
    /// secure-erase for an SSD) and to choose the fallback erase when a
    /// controller command is unavailable.
    pub rota: bool,
    /// lsblk TRAN: the bus transport ("nvme", "sata", "usb", …), or "" when
    /// lsblk reported none. Distinguishes an NVMe drive (erased with
    /// `nvme format`) from a SATA SSD (`blkdiscard`).
    pub tran: String,
}

// Only the real scanners construct these; the test build swaps those out.
#[cfg_attr(test, allow(dead_code))]
#[derive(Deserialize)]
struct LsblkRoot {
    blockdevices: Vec<LsblkDev>,
}

#[cfg_attr(test, allow(dead_code))]
#[derive(Deserialize)]
struct LsblkDev {
    #[serde(default)]
    pttype: Option<String>,
    name: String,
    size: Option<String>,
    #[serde(rename = "type")]
    dtype: Option<String>,
    model: Option<String>,
    rm: Option<bool>,
    #[serde(default)]
    rota: Option<bool>,
    #[serde(default)]
    tran: Option<String>,
    #[serde(default)]
    fstype: Option<String>,
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
    /// Exact size in bytes (0 when lsblk didn't report one). The manual
    /// partition editor needs real numbers to estimate the disk's free space.
    pub size_bytes: u64,
    pub fstype: String, // "ntfs", "ext4", "vfat", ...
    pub label: String,
    /// GPT partition UUID — the one name that survives renumbering.
    ///
    /// A partition NUMBER is not a stable identity: GPT reuses freed slots, so
    /// "partition 3" can be a different partition by the time the install runs
    /// (the user is invited to repartition externally with cfdisk, and the plan
    /// executes minutes after it was built). Deletions are verified against
    /// this before they fire. Empty when lsblk didn't report one.
    pub partuuid: String,
    /// GPT partition TYPE guid (lsblk PARTTYPE) — what the partition is FOR,
    /// as opposed to `partuuid`, which is which partition it is. This is the
    /// only reliable way to recognise an EFI System Partition or a BIOS-boot
    /// partition: both are identified by type, never by size or label (a 512 MiB
    /// vfat partition is not necessarily an ESP, and an ESP need not be either).
    /// The wipe warning uses it to tell the user they are about to destroy
    /// another operating system's bootloader. Empty on MBR or when unreported.
    #[allow(dead_code)] // read by the manual editor's wipe warning
    pub parttype: String,
}

/// GPT type GUID of an EFI System Partition — where bootloaders live.
pub const PARTTYPE_ESP: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
/// GPT type GUID of a BIOS-boot partition (GRUB's core.img on legacy GPT).
pub const PARTTYPE_BIOS_BOOT: &str = "21686148-6449-6e6f-744e-656564454649";
/// GPT type GUID of the Microsoft Reserved partition — a Windows marker.
pub const PARTTYPE_MSR: &str = "e3c9e316-0b5c-4db8-817d-f92df00215ae";
/// GPT type GUID of a Windows Recovery Environment partition.
pub const PARTTYPE_WINRE: &str = "de94bba4-06d1-4d40-a16a-bfd50179d6ac";

// ── Hardware access, and the one place tests are kept away from it ──────────
//
// `cargo test` runs on a developer's own machine, and these scanners shell out
// to lsblk. Left ungated, the developer's REAL devices flow into test output —
// a partition-editor dump announcing "/dev/sda2 WILL BE DELETED" about the disk
// holding their live root is alarming, and rightly so. Tests get virtio names,
// which cannot collide with a real SATA/NVMe device.
//
// It also makes the screen tests deterministic: lsblk reports whatever the
// machine happens to have, so "does this screen lay out correctly" silently
// depended on the tester's hardware.

#[cfg(not(test))]
pub fn list() -> Result<Vec<Disk>, String> {
    list_scan()
}
#[cfg(not(test))]
pub fn list_partitions() -> Result<Vec<Partition>, String> {
    list_partitions_scan()
}
#[cfg(not(test))]
pub fn list_partitions_all() -> Result<Vec<Partition>, String> {
    list_partitions_all_scan()
}

#[cfg(test)]
pub fn list() -> Result<Vec<Disk>, String> {
    const G: u64 = 1024 * 1024 * 1024;
    Ok(vec![
        Disk {
            path: "/dev/vda".into(),
            size: "240G".into(),
            model: "TEST DISK A".into(),
            removable: false,
            size_bytes: 240 * G,
            is_live: false,
            pttype: "gpt".into(),
            rota: false,
            tran: "sata".into(),
        },
        Disk {
            path: "/dev/vdb".into(),
            size: "500G".into(),
            model: "TEST DISK B".into(),
            removable: false,
            size_bytes: 500 * G,
            is_live: false,
            pttype: "dos".into(),
            rota: true,
            tran: "sata".into(),
        },
    ])
}

#[cfg(test)]
pub fn list_partitions() -> Result<Vec<Partition>, String> {
    const G: u64 = 1024 * 1024 * 1024;
    Ok(vec![Partition {
        path: "/dev/vdb1".into(),
        parent: "/dev/vdb".into(),
        size: "500G".into(),
        size_bytes: 500 * G,
        fstype: "ext4".into(),
        label: "DATA".into(),
        partuuid: "11111111-2222-3333-4444-555555555555".into(),
        parttype: String::new(),
    }])
}

#[cfg(test)]
pub fn list_partitions_all() -> Result<Vec<Partition>, String> {
    list_partitions()
}

/// List whole disks (type == "disk"), skipping loop/rom devices.
#[cfg_attr(test, allow(dead_code))]
fn list_scan() -> Result<Vec<Disk>, String> {
    // Human-readable sizes for display.
    let json_h = super::runner::capture(
        "lsblk",
        &["--json", "-o", "NAME,SIZE,TYPE,MODEL,RM,PTTYPE,ROTA,TRAN"],
    )?;
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
                pttype: d.pttype.unwrap_or_default(),
                rota: d.rota.unwrap_or(false),
                tran: d.tran.unwrap_or_default(),
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
/// Partitions carrying a REAL filesystem — what the extra-disks picker and
/// the format-confirmation screen want to list (raid/LVM members and bare
/// LUKS containers are not mountable things to offer there).
#[cfg_attr(test, allow(dead_code))]
fn list_partitions_scan() -> Result<Vec<Partition>, String> {
    let skip = ["swap", "crypto_LUKS", "linux_raid_member", "LVM2_member"];
    Ok(scan_partitions()?
        .into_iter()
        .filter(|p| !p.fstype.is_empty() && !skip.contains(&p.fstype.as_str()))
        .collect())
}

/// EVERY partition, unformatted ones and swap included — the manual
/// partitioning screen must see the blank partition the user just carved out
/// (formatting it is the whole point) and any existing swap to reuse.
#[cfg_attr(test, allow(dead_code))]
fn list_partitions_all_scan() -> Result<Vec<Partition>, String> {
    scan_partitions()
}

fn scan_partitions() -> Result<Vec<Partition>, String> {
    // `-b` so SIZE arrives as an exact byte count (a JSON number) — the manual
    // editor computes free space from it. Needs its own deserialize shape:
    // `LsblkDev` expects the human string form.
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
        #[serde(default)]
        fstype: Option<String>,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        partuuid: Option<String>,
        #[serde(default)]
        parttype: Option<String>,
        #[serde(default)]
        children: Vec<BytesDev>,
    }

    let json = super::runner::capture(
        "lsblk",
        &[
            "--json",
            "-b",
            "-o",
            "NAME,SIZE,TYPE,FSTYPE,LABEL,PARTUUID,PARTTYPE",
        ],
    )?;
    let parsed: BytesRoot = serde_json::from_str(&json).map_err(|e| e.to_string())?;
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
            let bytes = ch.size.unwrap_or(0);
            out.push(Partition {
                path: format!("/dev/{}", ch.name),
                parent: parent.clone(),
                size: human_size(bytes),
                size_bytes: bytes,
                fstype: ch.fstype.clone().unwrap_or_default(),
                label: ch.label.clone().unwrap_or_default(),
                partuuid: ch.partuuid.clone().unwrap_or_default(),
                parttype: ch.parttype.clone().unwrap_or_default().to_lowercase(),
            });
        }
    }
    Ok(out)
}

/// Bytes → a short human size ("512M", "40G", "1.5T"), locale-independent —
/// lsblk's own -h output follows the locale (decimal comma) which would leak
/// into shell-visible strings.
pub(crate) fn human_size(b: u64) -> String {
    const K: u64 = 1024;
    const M: u64 = K * K;
    const G: u64 = M * K;
    const T: u64 = G * K;
    if b >= T {
        let v = b as f64 / T as f64;
        if v < 10.0 {
            format!("{v:.1}T")
        } else {
            format!("{v:.0}T")
        }
    } else if b >= G {
        let v = b as f64 / G as f64;
        if v < 10.0 {
            format!("{v:.1}G")
        } else {
            format!("{v:.0}G")
        }
    } else if b >= M {
        format!("{}M", b / M)
    } else if b == 0 {
        "?".into()
    } else {
        format!("{}K", b.div_ceil(K))
    }
}

/// The partition NUMBER a path carries on `disk`, i.e. the inverse of
/// `part(disk, n)`. None when the path doesn't belong to this disk.
pub(crate) fn part_number(disk: &str, path: &str) -> Option<u32> {
    let rest = path.strip_prefix(disk)?;
    let rest = rest.strip_prefix('p').unwrap_or(rest);
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
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

/// Whether a recorded wipe mark for `disk` fires.
///
/// A whole-disk wipe is available in DUAL BOOT too, and on ANY drive the user
/// marked — including one holding Windows or another system's bootloader. That
/// is deliberate: Artix may be the first system on the machine, with a second
/// installed by hand later, so "erase the drive the other OS boots from" is a
/// legitimate intent rather than a mistake to block.
///
/// What guards it is consent, not prohibition: the editor names every foreign
/// partition and makes the user acknowledge the loss before the mark is even
/// recorded, and the final pre-install confirmation lists the wipe again. A mark
/// present here has already passed both.
pub(crate) fn wipe_applies(c: &InstallConfig, disk: &str) -> bool {
    !disk.is_empty() && c.manual_wipe.iter().any(|w| w.disk == disk)
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
    /// What the log shows INSTEAD of the real command, for a step whose text
    /// carries a secret (an account password, the LUKS passphrase). `None`
    /// logs the command verbatim. The install log is copied onto the finished
    /// system, so a secret echoed here outlives the installation entirely —
    /// and rides along in any bug report the log is pasted into.
    ///
    /// A ready-made rendering rather than "find the secret and blank it out":
    /// the secret reaches the command SHELL-ESCAPED (`shell_escape_dq`, or
    /// `'\''` for the LUKS one), so searching for its raw value misses any
    /// password containing a quote, and the USB-key passphrase is MINTED at
    /// plan time and never stored in the config, so there is nothing to search
    /// for at all. The builder knows exactly where the secret sits; it writes
    /// the safe line directly.
    pub redacted: Option<String>,
}

impl Action {
    /// Give a step a secret-free rendering for the log. The text should still
    /// say what the step does — the point is an auditable log, not a blank one.
    pub fn logged_as(mut self, text: &str) -> Self {
        self.redacted = Some(text.to_string());
        self
    }

    /// What the log may show for this step.
    pub fn log_line(&self) -> String {
        match &self.redacted {
            Some(safe) => format!("$ {safe}"),
            // A chroot step is shown as its script: the "artix-chroot /mnt sh
            // -c" prefix is the same on every one of them and only crowds out
            // the part that differs.
            None if self.args.len() > 4 && self.program == "artix-chroot" => {
                format!(
                    "$ chroot: {}",
                    self.args.last().cloned().unwrap_or_default()
                )
            }
            None => format!("$ {} {}", self.program, self.args.join(" ")),
        }
    }
}

fn act(program: &str, args: &[&str]) -> Action {
    Action {
        program: program.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        interactive: false,
        redacted: None,
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
        plan.push(
            act(
                "sh",
                &[
                    "-c",
                    &format!(
                        "printf '%s' '{pass}' | cryptsetup -q luksFormat --type luks1 {dev} -",
                        pass = pass_esc,
                        dev = boot_part
                    ),
                ],
            )
            .logged_as(&format!(
                "sh -c printf '%s' '***' | cryptsetup -q luksFormat --type luks1 {boot_part} -"
            )),
        );
        plan.push(
            act(
                "sh",
                &[
                    "-c",
                    &format!(
                        "printf '%s' '{pass}' | cryptsetup open {dev} cryptboot -",
                        pass = pass_esc,
                        dev = boot_part
                    ),
                ],
            )
            .logged_as(&format!(
                "sh -c printf '%s' '***' | cryptsetup open {boot_part} cryptboot -"
            )),
        );
        plan.push(act("mkfs.ext4", &["-F", "/dev/mapper/cryptboot"]));
    }

    // Root: optionally LUKS2 → cryptroot.
    let root_fsdev: String = if encrypt {
        plan.push(
            act(
                "sh",
                &[
                    "-c",
                    &format!(
                        "printf '%s' '{pass}' | cryptsetup -q luksFormat --type luks2 {dev} -",
                        pass = pass_esc,
                        dev = root_part
                    ),
                ],
            )
            .logged_as(&format!(
                "sh -c printf '%s' '***' | cryptsetup -q luksFormat --type luks2 {root_part} -"
            )),
        );
        plan.push(
            act(
                "sh",
                &[
                    "-c",
                    &format!(
                        "printf '%s' '{pass}' | cryptsetup open {dev} cryptroot -",
                        pass = pass_esc,
                        dev = root_part
                    ),
                ],
            )
            .logged_as(&format!(
                "sh -c printf '%s' '***' | cryptsetup open {root_part} cryptroot -"
            )),
        );
        "/dev/mapper/cryptroot".to_string()
    } else {
        root_part.clone()
    };

    // Format root with the chosen filesystem on the (possibly mapped) device.
    push_mkfs(&mut plan, root_fs, &root_fsdev);
    if let Some(n) = swap_n {
        let swap_dev = part(disk, n);
        plan.push(act("mkswap", &[&swap_dev]));
        plan.push(act("swapon", &[&swap_dev]));
    }

    // Mount-option assembly is shared with the manual plan below — one truth
    // for what "compress" or "noatime" means, whoever mounts the root.
    let (common, btrfs_extra) = mount_opts(c);

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
        let root_opt = subvol_opt(&btrfs_extra, "@");
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
            let opt = subvol_opt(&btrfs_extra, sv);
            plan.push(act("mount", &["-o", &opt, &root_fsdev, target]));
        }
    }

    plan
}

/// Mount-option strings from the config toggles, shared by the automatic and
/// manual disk plans: (`common`, `btrfs_extra`) — either may be "".
///
/// `noatime` is a common option valid on any filesystem; btrfs layers
/// compression and discard on top. fstabgen -U later copies whatever we mount
/// with into fstab, so getting the live mount right is all that's needed.
fn mount_opts(c: &InstallConfig) -> (String, String) {
    let common: Vec<&str> = if c.mount_noatime {
        vec!["noatime"]
    } else {
        vec![]
    };
    let mut btrfs_extra = common.clone();
    if c.btrfs_compress {
        btrfs_extra.push("compress=zstd");
    }
    if c.btrfs_discard {
        btrfs_extra.push("discard=async");
    }
    (common.join(","), btrfs_extra.join(","))
}

/// Mount options for a MANUAL /home on its own partition.
///
/// Deliberately built from `manual_home_*`, not the root's `btrfs_*`: a separate
/// /home is a separate filesystem, and the UI now configures the two apart. It
/// used to inherit the root's flags, so ticking compression for root quietly
/// compressed /home as well (and the /home row's toggles wrote to root).
fn home_mount_opts(c: &InstallConfig, home_fs: &str) -> String {
    let mut opts: Vec<&str> = if c.mount_noatime {
        vec!["noatime"]
    } else {
        vec![]
    };
    if home_fs == "btrfs" {
        if c.manual_home_compress {
            opts.push("compress=zstd");
        }
        if c.manual_home_discard {
            opts.push("discard=async");
        }
    }
    opts.join(",")
}

/// "subvol=NAME" plus any btrfs extras, comma-joined.
fn subvol_opt(btrfs_extra: &str, name: &str) -> String {
    if btrfs_extra.is_empty() {
        format!("subvol={name}")
    } else {
        format!("subvol={name},{btrfs_extra}")
    }
}

/// Push the mkfs command for `fs` onto the plan. One switch for root, /home
/// and both plan flavours — a new filesystem gets added exactly once.
fn push_mkfs(plan: &mut Vec<Action>, fs: &str, dev: &str) {
    match fs {
        "btrfs" => plan.push(act("mkfs.btrfs", &["-f", dev])),
        "xfs" => plan.push(act("mkfs.xfs", &["-f", dev])),
        "f2fs" => plan.push(act("mkfs.f2fs", &["-f", dev])),
        "jfs" => plan.push(act("mkfs.jfs", &["-q", dev])),
        "ext3" => plan.push(act("mkfs.ext3", &["-F", dev])),
        "ext2" => plan.push(act("mkfs.ext2", &["-F", dev])),
        _ => plan.push(act("mkfs.ext4", &["-F", dev])),
    }
}

/// The shell that erases a whole disk, for the manual editor's disk-wipe
/// feature. Returns the action(s) that run either immediately ("now", from the
/// editor) or as the FIRST steps of the install plan ("later").
///
/// This is the ONE place the manual path is allowed to `--zap-all` a whole
/// disk (see the invariant on `build_manual_plan`): only a disk the user
/// explicitly marked, and only after the final confirmation. Every method ends
/// by clearing the partition table so the disk scans as empty and new
/// partitions on it number from 1.
///
/// `allow_suspend` gates the frozen-drive thaw: the ATA path may briefly
/// suspend the machine (via rtcwake, which schedules its own wake) to clear a
/// firmware freeze — safe from the editor before any install work ("now"), but
/// never mid-install ("later"), where it would suspend the running installer.
/// A frozen or unsupported controller never aborts: the erase FALLS BACK to
/// blkdiscard (SSD) or shred (HDD), and even a failed fallback only warns.
pub fn wipe_actions(
    disk: &str,
    method: crate::app::WipeMethod,
    rota: bool,
    tran: &str,
    allow_suspend: bool,
) -> Vec<Action> {
    use crate::app::WipeMethod;
    let d = disk;
    // Reset the table + signatures at the end of every method so the disk scans
    // empty (numbering restarts at 1) even when the erase itself left a table.
    let finish = format!(
        "wipefs -a {d} 2>/dev/null; sgdisk --zap-all {d} 2>/dev/null; \
         partprobe {d} 2>/dev/null; udevadm settle 2>/dev/null; true"
    );
    // Fallback erase when the controller command is unavailable: an HDD gets a
    // zero pass, an SSD a TRIM (then a zero pass if TRIM is refused). Only ever
    // warns — a wipe must not stop the install.
    let fallback = if rota {
        format!("shred -v -n0 -z {d} || echo \"!! fallback zero-fill failed, continuing\"")
    } else {
        format!(
            "blkdiscard -f {d} 2>/dev/null || shred -v -n0 -z {d} \
             || echo \"!! fallback erase failed, continuing\""
        )
    };

    let script = match method {
        WipeMethod::Quick => format!(
            "echo \">>> Quick wipe {d}: clearing partition table and all signatures\"; \
             wipefs -a {d} || echo \"!! wipefs failed, continuing\"; \
             sgdisk --zap-all {d} || echo \"!! zap failed, continuing\"; \
             partprobe {d} 2>/dev/null; udevadm settle 2>/dev/null; true"
        ),
        WipeMethod::ZeroHdd => format!(
            "echo \">>> Zero-fill {d}: one pass of zeros (this can take a long time)\"; \
             shred -v -n0 -z {d} || echo \"!! zero-fill failed, continuing\"; \
             {finish}"
        ),
        WipeMethod::CryptoSsd => {
            let erase = if tran == "nvme" {
                format!(
                    "nvme format {d} --ses=2 --force 2>/dev/null \
                     || nvme format {d} --ses=1 --force 2>/dev/null \
                     || blkdiscard -f {d} 2>/dev/null \
                     || echo \"!! SSD erase failed, continuing\""
                )
            } else {
                format!(
                    "blkdiscard -f {d} 2>/dev/null || echo \"!! blkdiscard failed, continuing\""
                )
            };
            format!("echo \">>> SSD erase {d} (tran={tran})\"; {erase}; {finish}")
        }
        WipeMethod::AtaSecure => {
            // Thaw a frozen controller only when allowed, and only via rtcwake
            // (it schedules the wake — a bare `echo mem > /sys/power/state`
            // with no wake source would hang the machine forever).
            let thaw = if allow_suspend {
                "if command -v rtcwake >/dev/null 2>&1 && grep -qw mem /sys/power/state 2>/dev/null; then \
                   echo \">>> drive frozen — brief suspend via rtcwake to thaw\"; \
                   rtcwake -m mem -s 4 >/dev/null 2>&1 || true; sleep 1; \
                 else \
                   echo \">>> drive frozen and no safe way to suspend — will fall back\"; \
                 fi"
            } else {
                "echo \">>> drive frozen — not suspending during install; will fall back\""
            };
            format!(
                "D={d}; echo \">>> ATA Secure Erase $D\"; ata_ok=no; \
                 if hdparm -I \"$D\" 2>/dev/null | grep -qi 'Security:'; then \
                   if hdparm -I \"$D\" 2>/dev/null | grep -qi frozen \
                      && ! hdparm -I \"$D\" 2>/dev/null | grep -qiE 'not[[:space:]]+frozen'; then \
                     {thaw}; \
                   fi; \
                   if hdparm -I \"$D\" 2>/dev/null | grep -qiE 'not[[:space:]]+frozen'; then \
                     if hdparm --user-master u --security-set-pass p \"$D\" >/dev/null 2>&1; then \
                       if hdparm -I \"$D\" 2>/dev/null | grep -qi 'enhanced erase'; then \
                         hdparm --user-master u --security-erase-enhanced p \"$D\" >/dev/null 2>&1 \
                           && ata_ok=yes; \
                       fi; \
                       if [ \"$ata_ok\" != yes ]; then \
                         hdparm --user-master u --security-erase p \"$D\" >/dev/null 2>&1 \
                           && ata_ok=yes; \
                       fi; \
                       if [ \"$ata_ok\" != yes ]; then \
                         hdparm --user-master u --security-disable p \"$D\" >/dev/null 2>&1 || true; \
                       fi; \
                     fi; \
                   fi; \
                 fi; \
                 if [ \"$ata_ok\" = yes ]; then echo \">>> ATA Secure Erase OK\"; \
                 else echo \"!! ATA Secure Erase unavailable or drive still frozen — falling back\"; \
                   {fallback}; fi; \
                 {finish}"
            )
        }
    };
    vec![act("sh", &["-c", &script])]
}

/// The MANUAL disk plan: existing partitions are used as told, and (v2 editor)
/// NEW partitions are created in the free space of `manual_disk` — but this
/// still touches ONLY what it was explicitly told to touch.
///
/// The contract, in order:
///   * no wipefs, no --zap-all — the partition table is the user's, and on a
///     dual-boot disk "the rest" is another OS. New partitions are ADDED with
///     `sgdisk -n` (first-fit into free space); a fresh GPT is written only
///     when the disk has NO partition table at all (a blank disk);
///   * the ESP is REUSED as-is by default (a Windows ESP must survive);
///     it is formatted only when `manual_esp_format` says so;
///   * root is always formatted with the chosen fs; btrfs gets the same
///     @-subvolume layout and mount options as the automatic path, so
///     snapshots and rollback work identically;
///   * swap and /home are optional; /home may live on any disk and is
///     formatted with its own fs choice;
///   * everything ends up mounted under /mnt exactly like the automatic
///     path (ESP at /mnt/boot), so basestrap, fstabgen and the GRUB steps
///     that follow cannot tell the two modes apart.
///
/// V1 boundaries — enforced by the parts screen and re-checked by
/// build_plan's sanitiser: UEFI only, GRUB only, no LUKS.
/// The disk the root partition lives on, as the editor resolved it from the
/// scan. Never derived from the device name — see `DeletedPart`.
fn parent_of_root(c: &InstallConfig) -> String {
    if !c.manual_boot_disk.is_empty() {
        c.manual_boot_disk.clone()
    } else {
        c.manual_disk.clone()
    }
}

pub fn build_manual_plan(
    c: &InstallConfig,
    home_on_extra_disk: bool,
    luks_pass: &str,
) -> Vec<Action> {
    let esp = c.manual_esp.as_str();
    let root = c.manual_root.as_str();
    let root_fs = c.manual_root_fs.as_str();
    let mut plan = Vec::new();

    // Same teardown as the automatic path: a retry after a failed run must
    // not die on "Device or resource busy".
    plan.push(act(
        "sh",
        &["-c", "umount -R /mnt 2>/dev/null; awk 'NR>1 && $1 !~ /zram/ {print $1}' /proc/swaps 2>/dev/null | while read s; do swapoff \"$s\" 2>/dev/null; done; for m in $(dmsetup ls --target crypt 2>/dev/null | grep -v 'No devices' | awk '{print $1}'); do cryptsetup close \"$m\" 2>/dev/null; done; udevadm settle 2>/dev/null; true"],
    ));

    // WIPE whole disks the user marked (the "later" path), FIRST of all — a
    // wiped disk is erased outright, so its old partitions vanish and new ones
    // number from 1 (renumber_planned bases them at 0 for these disks). Each
    // mark is self-contained (rota/tran captured at scan time); `false` forbids
    // the frozen-drive suspend here, mid-install.
    //
    // Available in dual boot as well — erasing a SECOND drive to use as /home or
    // storage is an ordinary thing to want. `wipe_applies` is what keeps the
    // other system's boot drive out of reach, and it is checked here (not only
    // in the UI) so a mark that survived a mode switch cannot fire.
    for w in &c.manual_wipe {
        if !wipe_applies(c, &w.disk) {
            continue;
        }
        plan.extend(wipe_actions(&w.disk, w.method, w.rota, &w.tran, false));
    }

    // DELETE the partitions the user marked, before anything is created — the
    // freed space is what the new ones are planned into.
    //
    // This is the most destructive thing the installer does, so it is kept
    // narrow: only paths the user explicitly marked, resolved to a partition
    // NUMBER on their own disk, one `sgdisk -d` each. Partitions on any disk may
    // be listed (a /home built on a second drive), so they are grouped per disk
    // and each disk is re-read once afterwards.
    //
    // Numbering safety: a freed number CAN be reused by a new partition (so a
    // resized partition keeps its number). That is safe only because the delete
    // below is keyed to the PARTUUID, not the slot: it removes the marked
    // partition wherever it currently sits and does nothing if it is already
    // gone — so a slot reoccupied by our own fresh partition (a retry) is never
    // mistaken for the marked one, and a create refuses to clobber a slot it does
    // not own.
    if !c.manual_deleted.is_empty() {
        let mut disks: Vec<String> = Vec::new();
        for p in &c.manual_deleted {
            if !p.disk.is_empty() && !disks.contains(&p.disk) {
                disks.push(p.disk.clone());
            }
        }
        for d in &disks {
            // A disk being fully wiped is erased outright above; deleting its
            // individual partitions here is redundant and would fail once the
            // table is gone. Skip them.
            if wipe_applies(c, d) {
                continue;
            }
            let mut nums: Vec<(u32, String)> = c
                .manual_deleted
                .iter()
                .filter(|p| p.disk == *d)
                .filter_map(|p| {
                    let n = part_number(d, &p.path)?;
                    // Round-trip or refuse: (disk, n) must rebuild the exact
                    // path the user marked. Anything else means the pair is
                    // inconsistent, and `sgdisk -d` on a guess is unthinkable.
                    (part(d, n) == p.path).then_some((n, p.partuuid.clone()))
                })
                .collect();
            // Highest number first: deleting low numbers first would be fine on
            // GPT, but descending order keeps the remaining numbers stable for
            // anything reading the table between calls.
            nums.sort_unstable();
            nums.reverse();
            for (n, uuid) in nums {
                if uuid.is_empty() {
                    // No identity to check against (lsblk reported none, e.g. an
                    // exotic table). Fall back to the number — no worse than
                    // before, and refusing would block a legitimate layout.
                    plan.push(act("sgdisk", &["-d", &n.to_string(), d]));
                    continue;
                }
                // Delete BY IDENTITY, not by slot. The plan is built from a scan
                // taken minutes earlier; GPT reuses freed numbers and the user
                // may repartition on console 2, so "partition n" can be a
                // different partition — or the marked one may have moved. So:
                //   * fast path: the recorded slot still holds the marked PARTUUID
                //     → delete it there;
                //   * else find that PARTUUID anywhere on the disk → delete it
                //     there (it moved);
                //   * else it is GONE (a previous run deleted it, or the user
                //     did) → do nothing.
                // It never deletes a partition whose PARTUUID was not the one
                // marked, and it never aborts: a slot reoccupied by our own fresh
                // partition (the retry case that reuse makes routine) simply is
                // not the marked UUID, so it is left alone. Any genuine clash — a
                // foreign partition sitting on a number we need — surfaces at the
                // create step, which refuses to clobber a slot it does not own.
                //
                // Identity comes from the ON-DISK GPT via `sgdisk` (uppercase
                // GUID, lowercased to match the scan's), never `lsblk`: lsblk
                // reads the udev database, stale right after a table change, which
                // is what once made this guard refuse a live partition with
                // "found none". sgdisk reads the disk directly, no settle needed.
                let dev = part(d, n);
                let script = format!(
                    "want={uuid}; \
                     got=$(sgdisk -i {n} {d} 2>/dev/null | awk '/unique GUID/{{print tolower($NF)}}'); \
                     if [ \"$got\" = \"$want\" ]; then sgdisk -d {n} {d}; exit $?; fi; \
                     for k in $(sgdisk -p {d} 2>/dev/null | awk '/^[[:space:]]*[0-9]+[[:space:]]/{{print $1}}'); do \
                     u=$(sgdisk -i \"$k\" {d} 2>/dev/null | awk '/unique GUID/{{print tolower($NF)}}'); \
                     if [ \"$u\" = \"$want\" ]; then sgdisk -d \"$k\" {d}; exit $?; fi; \
                     done; \
                     echo \">>> {dev} ({uuid}) already gone - nothing to delete.\"; exit 0"
                );
                plan.push(act("sh", &["-c", &script]));
            }
            plan.push(act(
                "sh",
                &[
                    "-c",
                    &format!("partprobe {d} 2>/dev/null; udevadm settle 2>/dev/null; sleep 1"),
                ],
            ));
        }
    }

    // Create the NEW partitions the editor planned (v2). Additions only:
    // `sgdisk -n` first-fits each into free space, the existing table survives
    // untouched. The rest-of-disk slot (MANUAL_REST) is created LAST so the
    // fixed-size ones aren't squeezed. Partition numbers/paths were predicted
    // by the editor from the same scan the user assigned roles on.
    // Group the planned partitions BY DISK. They are no longer confined to one:
    // root can be carved from one drive and a fresh /home from another, which
    // is an ordinary layout and used to be refused outright.
    let mut new_slots: Vec<(String, u32, u32, &str, &str)> = Vec::new();
    for (dev, mib, code, label, on) in [
        (
            c.manual_esp.as_str(),
            c.manual_esp_new_mib,
            "ef00",
            "EFI",
            c.manual_esp_disk.as_str(),
        ),
        // BIOS boot: 1 MiB, type ef02, no filesystem — GRUB writes core.img
        // straight into it. Never formatted, never mounted.
        (
            c.manual_bios.as_str(),
            c.manual_bios_new_mib,
            "ef02",
            "BIOSBOOT",
            c.manual_bios_disk.as_str(),
        ),
        (
            c.manual_root.as_str(),
            c.manual_root_new_mib,
            "8300",
            "ROOT",
            c.manual_root_disk.as_str(),
        ),
        (
            c.manual_swap.as_str(),
            c.manual_swap_new_mib,
            "8200",
            "SWAP",
            c.manual_swap_disk.as_str(),
        ),
        (
            c.manual_home.as_str(),
            c.manual_home_new_mib,
            "8300",
            "HOME",
            c.manual_home_disk.as_str(),
        ),
    ] {
        if mib == 0 || dev.is_empty() {
            continue;
        }
        // Fall back to the legacy single-disk field for configs written before
        // slots carried their own disk.
        let on = if on.is_empty() {
            c.manual_disk.as_str()
        } else {
            on
        };
        if on.is_empty() {
            continue;
        }
        if let Some(n) = part_number(on, dev) {
            new_slots.push((on.to_string(), n, mib, code, label));
        }
    }
    // Data-storage partitions the editor planned: created here exactly like the
    // role slots; their mkfs/mount rides in `extra_disks_plan` (each carries a
    // format+mount entry keyed by its predicted path).
    for dp in &c.manual_data_new {
        if dp.mib == 0 || dp.path.is_empty() || dp.disk.is_empty() {
            continue;
        }
        if let Some(n) = part_number(&dp.disk, &dp.path) {
            new_slots.push((dp.disk.clone(), n, dp.mib, "8300", "DATA"));
        }
    }
    // Fixed sizes first, the rest-of-disk slot last, per disk.
    new_slots.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then((a.2 == crate::app::MANUAL_REST).cmp(&(b.2 == crate::app::MANUAL_REST)))
            .then(a.1.cmp(&b.1))
    });

    let mut disks: Vec<String> = Vec::new();
    for (d, _, _, _, _) in &new_slots {
        if !disks.contains(d) {
            disks.push(d.clone());
        }
    }
    for d in &disks {
        // A blank disk (no partition table at all) gets a fresh GPT; a disk
        // that already has one is left exactly as it is.
        plan.push(act(
            "sh",
            &[
                "-c",
                &format!(
                    "[ -n \"$(blkid -o value -s PTTYPE {d} 2>/dev/null)\" ] || sgdisk -og {d}"
                ),
            ],
        ));
        for (on, n, mib, code, label) in new_slots.iter().filter(|s| &s.0 == d) {
            let size_spec = if *mib == crate::app::MANUAL_REST {
                "0".to_string()
            } else {
                format!("+{mib}M")
            };
            // Idempotent for retries: a partition we already created in an
            // earlier attempt carries our PARTLABEL, and `sgdisk -n` on an
            // existing number fails outright — which would make every retry
            // after a partly-finished install die here. Recognise our own work
            // and move on; anything else on that number is left alone and the
            // creation is attempted, so a genuine clash still surfaces.
            let dev_path = part(on, *n);
            // Identity from the ON-DISK GPT (`sgdisk -i` → "Partition name"),
            // NOT from `lsblk`. Each `sgdisk -n` in this loop rewrites the GPT
            // and triggers a partition-table re-read, so a sibling slot's
            // lsblk-based check could read a stale udev database — the same
            // race that made the DELETE guard refuse a live partition. sgdisk
            // reads the disk directly, so no settle is needed between creates.
            plan.push(act(
                "sh",
                &["-c", &format!(
                    "if [ \"$(sgdisk -i {n} {on} 2>/dev/null | awk -F\"'\" '/^Partition name:/{{print $2}}')\" = \"{label}\" ]; then \
                     echo \">>> {dev_path} already exists from an earlier attempt - keeping it.\"; exit 0; fi; \
                     sgdisk -n {n}:0:{size_spec} -t {n}:{code} -c {n}:{label} {on}"
                )],
            ));
        }
        plan.push(act(
            "sh",
            &[
                "-c",
                &format!("partprobe {d} 2>/dev/null; udevadm settle 2>/dev/null; sleep 1"),
            ],
        ));
    }

    if c.manual_esp_format && c.boot_mode.is_uefi() && !esp.is_empty() {
        plan.push(act("mkfs.fat", &["-F32", esp]));
    }
    // Clear any stale signature (a leftover LUKS/old-fs header) before mkfs, so
    // reformatting an existing — e.g. previously-ENCRYPTED — partition as root
    // or /home starts clean and libblkid can't later misdetect it. Harmless on
    // a partition we just created. Best-effort.
    plan.push(act(
        "sh",
        &["-c", &format!("wipefs -a {root} 2>/dev/null; true")],
    ));

    // Everything downstream — crypttab, the initramfs keyfile, the GRUB
    // cmdline, the rollback entries — finds the root by `blkid -t
    // PARTLABEL=ROOT`. The automatic layout gets that for free from `sgdisk -c
    // N:ROOT`; a REUSED partition carries whatever label it had. Stamp it, or
    // an encrypted manual install boots to a rescue prompt looking for a device
    // that, as far as blkid is concerned, does not exist.
    if let Some(n) = part_number(&parent_of_root(c), root) {
        let d = parent_of_root(c);
        plan.push(act("sgdisk", &["-c", &format!("{n}:ROOT"), &d]));
    }

    // Root-scope LUKS. The container is opened as `cryptroot` and everything
    // after this point — mkfs, mounts, fstab — works on the mapper device, so
    // the rest of the plan needs no idea encryption happened.
    let root_dev: String = if c.encrypt_disk {
        let pass_esc = luks_pass.replace('\'', "'\\''");
        plan.push(
            act(
                "sh",
                &[
                    "-c",
                    &format!(
                        "printf '%s' '{pass_esc}' | cryptsetup -q luksFormat --type luks2 {root} -"
                    ),
                ],
            )
            .logged_as(&format!(
                "sh -c printf '%s' '***' | cryptsetup -q luksFormat --type luks2 {root} -"
            )),
        );
        plan.push(
            act(
                "sh",
                &[
                    "-c",
                    &format!("printf '%s' '{pass_esc}' | cryptsetup open {root} cryptroot -"),
                ],
            )
            .logged_as(&format!(
                "sh -c printf '%s' '***' | cryptsetup open {root} cryptroot -"
            )),
        );
        "/dev/mapper/cryptroot".to_string()
    } else {
        root.to_string()
    };
    let root = root_dev.as_str();

    push_mkfs(&mut plan, root_fs, root);
    if !c.manual_swap.is_empty() {
        plan.push(act("mkswap", &[c.manual_swap.as_str()]));
        plan.push(act("swapon", &[c.manual_swap.as_str()]));
    }

    let (common, btrfs_extra) = mount_opts(c);
    // A dedicated /home — own partition here, or a whole extra disk from the
    // storage step — wins over the @home subvolume.
    let home_external = home_on_extra_disk || !c.manual_home.is_empty();

    if root_fs == "btrfs" && c.btrfs_subvolumes {
        plan.push(act("mount", &[root, "/mnt"]));
        for sv in ["@", "@home", "@snapshots", "@log", "@cache"] {
            plan.push(act(
                "btrfs",
                &["subvolume", "create", &format!("/mnt/{sv}")],
            ));
        }
        plan.push(act("umount", &["/mnt"]));
        let root_opt = subvol_opt(&btrfs_extra, "@");
        plan.push(act("mount", &["-o", &root_opt, root, "/mnt"]));
    } else if root_fs == "btrfs" && !btrfs_extra.is_empty() {
        plan.push(act("mount", &["-o", &btrfs_extra, root, "/mnt"]));
    } else if !common.is_empty() {
        plan.push(act("mount", &["-o", &common, root, "/mnt"]));
    } else {
        plan.push(act("mount", &[root, "/mnt"]));
    }

    // ESP mount: "/boot" (kernels on the ESP — needs a roomy one) or
    // "/boot/efi" (kernels stay on root, only the bootloader on the ESP — the
    // layout that fits a small reused Windows ESP). Either way the mountpoint
    // is under the already-mounted root, so a single mkdir -p makes it.
    // Legacy boot has no ESP at all — the BIOS-boot partition carries no
    // filesystem and is never mounted, so this whole block is UEFI-only.
    if c.boot_mode.is_uefi() && !esp.is_empty() {
        let esp_mp = if c.manual_esp_mount == "/boot/efi" {
            "/mnt/boot/efi"
        } else {
            "/mnt/boot"
        };
        plan.push(act("mkdir", &["-p", esp_mp]));
        plan.push(act("mount", &[esp, esp_mp]));
    }

    if root_fs == "btrfs" && c.btrfs_subvolumes {
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
            if sv == "@home" && home_external {
                continue;
            }
            let opt = subvol_opt(&btrfs_extra, sv);
            plan.push(act("mount", &["-o", &opt, root, target]));
        }
    }

    if !c.manual_home.is_empty() {
        let home = c.manual_home.as_str();
        let home_fs = if c.manual_home_fs.is_empty() {
            "ext4"
        } else {
            c.manual_home_fs.as_str()
        };
        // Same signature scrub as root — a /home on a separate, possibly
        // previously-encrypted, disk reformats cleanly.
        plan.push(act(
            "sh",
            &["-c", &format!("wipefs -a {home} 2>/dev/null; true")],
        ));
        push_mkfs(&mut plan, home_fs, home);
        plan.push(act("mkdir", &["-p", "/mnt/home"]));
        let opts = home_mount_opts(c, home_fs);
        if opts.is_empty() {
            plan.push(act("mount", &[home, "/mnt/home"]));
        } else {
            plan.push(act("mount", &["-o", &opts, home, "/mnt/home"]));
        }
    }

    plan
}
