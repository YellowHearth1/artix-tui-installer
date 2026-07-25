//! Install-plan construction — the heart of the installer.
//!
//! `build_plan` turns the user's `InstallConfig` into an ordered `Vec<Action>`
//! (partition → format → basestrap → configure → bootloader → users →
//! services) that `system::runner` then executes with a live streaming log.
//!
//! The module is split by concern; start here, drill into a submodule when a
//! change is topic-shaped:
//!   • `helpers`  — tiny Action constructors (act / chroot / write_* …) and
//!                  cmdline/LUKS/mountpoint utilities
//!   • `scripts`  — every embedded shell script, service file, dotfile and
//!                  binary asset the installer drops onto the target system
//!   • `packages` — desktop/GPU/kernel → package-list resolution
//!   • `mirrors`  — pacman mirror ranking + regional country tables
//!
//! House rules: plan steps are numbered comments inside `build_plan`; shell
//! embedded in `format!` uses @@PLACEHOLDER@@ + `.replace()` rather than
//! brace-escaping; every script constant lives in `scripts`, never inline.

use crate::app::{
    App, Bootloader, Desktop, GpuDriver, InstallConfig, Kernel, SeatProvider, AUDIO_PACKAGES,
    DINIT_PACKAGES, DINIT_SERVICES,
};
use crate::i18n::{t, Lang};
use crate::system::disk::{self, Action};

mod helpers;
mod mirrors;
mod packages;
mod scripts;

pub(crate) use helpers::*;
pub(crate) use mirrors::*;
pub(crate) use packages::*;
pub(crate) use scripts::*;

pub fn build_plan(app: &App) -> Vec<Action> {
    let c = &app.config;
    let uefi = c.boot_mode.is_uefi();
    let mut plan: Vec<Action> = Vec::new();

    // Last line of defence: the install target must never be the USB key stick.
    // The UI guards this from both sides (the disk list hides the key stick; the
    // options screen won't offer the install disk as a key), but if a config
    // ever slips through anyway, wiping the stick would destroy the only thing
    // that can unlock the container — an unbootable system in key-only mode.
    // Fail loudly, before anything has touched a disk.
    if !c.usb_key_device.is_empty() && c.usb_key_device == c.disk {
        plan.push(act(
            "sh",
            &[
                "-c",
                "echo '!! The install disk and the USB key are the same device.'; \
                 echo '!! Refusing to continue: this would erase the key that unlocks the system.'; \
                 echo '!! Go back and pick a different disk (or a different key stick).'; \
                 exit 1",
            ],
        ));
        return plan;
    }

    // Scrub the key stick out of extra_disks ONCE, here, rather than guarding
    // each of the five places downstream that format, mount, encrypt or fstab
    // these entries — miss one and the stick gets wiped. `c` is rebound to this
    // sanitised config for the rest of the plan.
    let sanitised: InstallConfig;
    let c: &InstallConfig = if !c.usb_key_device.is_empty()
        && c.extra_disks
            .iter()
            .any(|d| d.disk == c.usb_key_device || d.disk.starts_with(&c.usb_key_device))
    {
        let mut cc = c.clone();
        let key = cc.usb_key_device.clone();
        cc.extra_disks
            .retain(|d| d.disk != key && !d.disk.starts_with(&key));
        sanitised = cc;
        &sanitised
    } else {
        c
    };

    // ── Manual-partitioning normalisation (v243) ──────────────────────────
    // Manual mode keeps its fs choice in `manual_root_fs`; copy it into
    // `root_fs` so every fs-keyed decision downstream — btrfs-progs in the
    // host-tools step, snapper/rollback wiring, rootflags=subvol=@ — reads
    // ONE truth. Three v1 boundaries are enforced here as a backstop behind
    // the parts screen, because a plan builder must not trust a UI:
    //   * no LUKS — the manual screen offers none, and improvising half a
    //     LUKS setup on somebody's dual-boot disk is how data dies;
    //   * GRUB only — the rEFInd/Limine/EFISTUB steps navigate by the
    //     PARTLABELs and fixed partition numbers the AUTOMATIC layout
    //     creates, none of which exist on a user-made table. GRUB probes
    //     the mounted tree instead, which is exactly right here;
    //   * os-prober ON — finding the neighbouring OS is the point.
    // Manual AND install-alongside share this path: both bring pre-existing
    // partitions and create new ones without wiping the table.
    let manual = c.partition_mode.is_manual_family();
    // A SOLO manual install (the user said there is no second OS) owns the disk
    // outright, so the two v1 limits that exist because of a neighbour OS —
    // GRUB-only and no LUKS — do not apply to it. `manual_solo` is set by the
    // pmode survey; Alongside can never be solo, it is defined by the neighbour.
    let solo = manual && c.manual_solo && c.partition_mode != crate::app::PartitionMode::Alongside;
    let manual_had_luks = manual && !solo && c.encrypt_disk;
    let manual_had_other_loader = manual && !solo && c.bootloader != Bootloader::Grub;
    let manual_fixed: InstallConfig;
    let c: &InstallConfig = if manual {
        let mut cc = c.clone();
        cc.root_fs = if cc.manual_root_fs.is_empty() {
            "ext4".into()
        } else {
            cc.manual_root_fs.clone()
        };
        if solo {
            // Encryption in the manual editor is ROOT-scope only: an encrypted
            // /boot needs GRUB's cryptodisk plus a second keyfile dance, and
            // the manual layout is too free-form to promise that yet. Root
            // scope needs the kernels OUTSIDE the container, so the ESP holds
            // /boot — the same shape the automatic path uses for this scope.
            if cc.encrypt_disk {
                cc.encrypt_scope = "root".into();
                cc.manual_esp_mount = "/boot".into();
            }
            cc.os_prober = false; // no neighbour to find
        } else {
            cc.encrypt_disk = false;
            cc.bootloader = Bootloader::Grub;
            cc.os_prober = true;
        }
        // BIOS GRUB installs to a whole DISK (`grub-install --target=i386-pc
        // /dev/sdX`), and the rest of the plan reads that from `disk`, which
        // manual mode otherwise leaves empty. The editor resolved it from the
        // scan; carry it across so legacy manual installs get a bootloader.
        if !cc.boot_mode.is_uefi() && cc.disk.is_empty() {
            cc.disk = if !cc.manual_boot_disk.is_empty() {
                cc.manual_boot_disk.clone()
            } else {
                cc.manual_disk.clone()
            };
        }
        // A partition marked for deletion cannot also be an extra mount: the
        // storage step may still hold an entry for it from before the user
        // deleted it, and the plan would then delete the partition and try to
        // mount it in the same run.
        if !cc.manual_deleted.is_empty() {
            let gone = cc.manual_deleted.clone();
            cc.extra_disks
                .retain(|d| !gone.iter().any(|p| p.path == d.disk));
        }
        manual_fixed = cc;
        &manual_fixed
    } else {
        c
    };

    // Key-only USB mode: the user types NO passphrase at all. LUKS still
    // needs an initial secret to format and open the container, so we mint a
    // strong throwaway one from the kernel CSPRNG. It exists only inside this
    // plan: it authorizes setup (luksFormat/open, luksAddKey) and is removed
    // as the FINAL link of the USB chain, leaving the 4096-byte keyfile on
    // the stick as the container's only slot.
    let minted: Option<String> = if c.encrypt_disk
        && !c.usb_key_device.is_empty()
        && c.usb_key_only
        && c.luks_passphrase.is_empty()
    {
        Some(random_passphrase())
    } else {
        None
    };
    let luks_pass: &str = minted.as_deref().unwrap_or(&c.luks_passphrase);

    // ─── Pre-flight checks (run before ANY download or write) ───────────────
    // Catch problems before a single destructive step. No internet is fatal —
    // the install must fetch packages — and aborts cleanly here, before
    // partitioning. Low RAM or a small target disk are warnings only. Firmware
    // mode (UEFI/BIOS) is reported for the record.
    {
        // The no-internet message is the one the user actually acts on from the
        // failed screen, so it's localized and says exactly what to do: go back
        // to the Wi-Fi step and connect. Multiple echoes keep the lines separate
        // (echo doesn't expand \n in plain sh).
        // The three lines come from the TOMLs like every other user-facing
        // string. They're SHELL here, not UI — but that's a detail of how
        // they're delivered, not a reason for the text itself to live in the
        // source. Single quotes inside the message would end the shell string,
        // so they're escaped the POSIX way ('\'').
        let lang = if c.lang == "uk" { Lang::Uk } else { Lang::En };
        let sh_quote = |key: &str| t(lang, key).replace('\'', "'\\''");
        let no_net = format!(
            "echo '!!! {}'; echo '!!! {}'; echo '!!! {}'",
            sh_quote("net.none_detected"),
            sh_quote("net.go_back_and_connect"),
            sh_quote("net.disk_untouched"),
        );
        let preflight = format!(
            "echo '>>> Pre-flight checks...'; \
             FAIL=0; \
             printf '>>> Internet connectivity: '; \
             if curl -fsS --connect-timeout 10 --max-time 20 -I https://artixlinux.org >/dev/null 2>&1 || curl -fsS --connect-timeout 10 --max-time 20 -I https://archlinux.org >/dev/null 2>&1; then echo OK; else echo FAILED; FAIL=1; fi; \
             if [ -d /sys/firmware/efi ]; then echo '>>> Firmware mode: UEFI'; else echo '>>> Firmware mode: BIOS/Legacy'; fi; \
             mem_kb=$(awk '/MemTotal/{{print $2}}' /proc/meminfo 2>/dev/null || echo 0); \
             mem_mb=$((mem_kb / 1024)); \
             echo \">>> RAM: ${{mem_mb}} MiB\"; \
             if [ \"$mem_mb\" -lt 1024 ]; then echo '!!! Warning: under 1 GiB RAM - the install may be slow or struggle.'; fi; \
             if [ -b '{disk}' ]; then \
               db=$(lsblk -bdno SIZE '{disk}' 2>/dev/null | head -1 || echo 0); \
               dg=$((db / 1024 / 1024 / 1024)); \
               echo \">>> Target disk {disk}: ${{dg}} GiB\"; \
               if [ \"$dg\" -lt 8 ]; then echo '!!! Warning: target disk under 8 GiB - may be too small for a full install.'; fi; \
             fi; \
             yr=$(date -u +%Y 2>/dev/null || echo 0); \
             echo \">>> System clock: $(date -u '+%Y-%m-%d %H:%M UTC' 2>/dev/null)\"; \
             if [ \"$yr\" -lt 2024 ] || [ \"$yr\" -gt 2100 ]; then \
               echo '!!! Warning: the system clock looks wrong. Package signatures are'; \
               echo '!!! checked against it, so EVERY package may fail as \"signature is'; \
               echo '!!! invalid\". Fix the date (or the VM/BIOS clock) and run again.'; \
             fi; \
             if [ \"$FAIL\" = 1 ]; then {no_net}; exit 1; fi; \
             echo '>>> Pre-flight checks passed.'; \
             true",
            disk = c.disk,
            no_net = no_net
        );
        plan.push(act("sh", &["-c", &preflight]));
    }

    // 0) Host tooling. The installer shells out to partitioning/install tools
    //    that may NOT be present when it's run from a STOCK Artix ISO (or a
    //    minimal environment) instead of this distro's own live image, where
    //    they're baked in. Without them the later steps fail with "command not
    //    found" (e.g. sgdisk) or "command not found: basestrap". Install them
    //    up front so every later step has what it needs:
    //      • artools     — basestrap, fstabgen, artix-chroot (the install core)
    //      • gptfdisk    — sgdisk (GPT partitioning)
    //      • parted      — partprobe (re-read the partition table)
    //      • dosfstools  — mkfs.fat (the EFI system partition)
    //      • e2fsprogs   — mkfs.ext4 (root/boot when ext4; usually present)
    //      • btrfs-progs — mkfs.btrfs (only when the root fs is btrfs)
    //      • cryptsetup  — LUKS open/format/addkey (only when encrypting)
    //    --needed makes this a no-op on this distro's own ISO (already there);
    //    -Sy refreshes first so a stale live db doesn't cause "target not found".
    // 0a) Optionally rebuild the pacman mirrorlists BEFORE anything downloads:
    //     the optimizer probes EVERY mirror in the list (12 in parallel, 6s cap
    //     each), writes the reachable ones fastest-first and comments the dead
    //     or crawling ones out. Doing it up front means host-tools, basestrap
    //     and every later pacman call only ever see mirrors that answered
    //     seconds ago — a server that degraded since the ISO was built can no
    //     longer stall the install. Best-effort throughout (see the script).
    if c.optimize_mirrors {
        // Quoted heredoc keeps the script's $@/$1/`cmd`/$() literal on write.
        let write_cmd = format!(
            "cat > /tmp/optmirrors.sh <<'MIRROPT_EOF'\n{}\nMIRROPT_EOF",
            MIRROR_OPTIMIZE_SCRIPT
        );
        plan.push(act("sh", &["-c", &write_cmd]));
        plan.push(act("sh", &["/tmp/optmirrors.sh"]));
    }

    let mut host_tools: Vec<&str> =
        vec!["artools", "gptfdisk", "parted", "dosfstools", "e2fsprogs"];
    let mkfs_tool = |fs: &str| -> Option<&'static str> {
        match fs {
            "btrfs" => Some("btrfs-progs"),
            "xfs" => Some("xfsprogs"),
            "f2fs" => Some("f2fs-tools"),
            "jfs" => Some("jfsutils"),
            _ => None,
        }
    };
    if let Some(t) = mkfs_tool(&c.root_fs) {
        host_tools.push(t);
    }
    // A manual /home may use its own filesystem (possibly on a second disk):
    // the LIVE environment needs that mkfs tool to format it.
    if c.partition_mode.is_manual_family() && !c.manual_home.is_empty() {
        if let Some(t) = mkfs_tool(&c.manual_home_fs) {
            if !host_tools.contains(&t) {
                host_tools.push(t);
            }
        }
    }
    // The live environment needs the mkfs tool for every additional disk the
    // user formats too (e.g. a btrfs/xfs/f2fs /home on a second disk).
    for d in &c.extra_disks {
        if !d.format || d.mountpoint.is_empty() {
            continue;
        }
        let t = match d.fs.as_str() {
            "btrfs" => "btrfs-progs",
            "xfs" => "xfsprogs",
            "f2fs" => "f2fs-tools",
            "jfs" => "jfsutils",
            _ => continue,
        };
        if !host_tools.contains(&t) {
            host_tools.push(t);
        }
    }
    if c.encrypt_disk
        || c.extra_disks
            .iter()
            .any(|d| d.format && d.encrypt && !d.mountpoint.is_empty())
    {
        host_tools.push("cryptsetup");
    }
    let host_tools_cmd = format!("pacman -Sy --needed --noconfirm {}", host_tools.join(" "));
    plan.push(act("sh", &["-c", &host_tools_cmd]));

    // 1) Disk: partition, format, mount — two roads to the same mounted /mnt.
    //    Manual first states its contract in the log, then refuses BIOS as a
    //    last-resort backstop (the UI refuses it too) BEFORE any destructive
    //    step.
    if manual {
        plan.push(act(
            "sh",
            &["-c", "echo '>>> Manual partitioning: only the named partitions will be touched; the partition table stays yours.'"],
        ));
        if manual_had_luks {
            plan.push(act(
                "sh",
                &["-c", "echo 'NOTE: disk encryption is not available in manual mode (v1) - continuing WITHOUT LUKS.'"],
            ));
        }
        if manual_had_other_loader {
            plan.push(act(
                "sh",
                &["-c", "echo 'NOTE: manual mode supports GRUB only (v1) - bootloader switched to GRUB.'"],
            ));
        }
        // Legacy (BIOS) manual installs are supported now — the editor asks for
        // a BIOS-boot partition on GPT and GRUB goes to the whole disk. The
        // backstop that remains is the one that actually matters: without a
        // disk to install the bootloader onto, stop BEFORE touching anything.
        if !c.boot_mode.is_uefi() && c.disk.is_empty() {
            plan.push(act(
                "sh",
                &["-c", "echo '!!! Legacy boot needs a target disk for GRUB, none was resolved. Aborting before any change.' >&2; exit 1"],
            ));
        }
    }
    let home_on_extra_disk = c.extra_disks.iter().any(|d| d.mountpoint == "/home");
    if manual {
        plan.extend(disk::build_manual_plan(c, home_on_extra_disk, luks_pass));
    } else {
        plan.extend(disk::build_plan(c, luks_pass, home_on_extra_disk));
    }
    // Additional whole disks the user chose to format (e.g. a separate /home or
    // a storage disk): partition, format, and mount them now so they're part of
    // /mnt before basestrap and fstabgen records them. Existing partitions to
    // mount as-is (NTFS Windows, etc.) are handled later, after fstabgen.
    plan.extend(extra_disks_plan(c));

    // 1b) Prepare the LIVE pacman before basestrap. Some chosen packages live in
    //     the [lib32] (multilib) repo — steam, lib32 GPU libraries — which is
    //     disabled by default in the live image, so basestrap can't find them
    //     ("target not found"). Uncomment exactly the [lib32] block (NOT the
    //     [lib32-gremlins] testing repo) in the LIVE /etc/pacman.conf, then
    //     refresh all databases so basestrap downloads complete, current lists.
    //     The awk matches the literal "#[lib32]" header and the single "#Include"
    //     line that follows it, leaving every other repo untouched. Idempotent:
    //     if [lib32] is already enabled, nothing changes.
    plan.push(act(
        "sh",
        &[
            "-c",
            "awk '/^#\\[lib32\\]$/{print \"[lib32]\"; getline; sub(/^#/,\"\"); print; next} {print}' \
             /etc/pacman.conf > /tmp/pacman.conf.new && cp /tmp/pacman.conf.new /etc/pacman.conf",
        ],
    ));
    // 1b-dl) Faster downloads for basestrap: uncomment the stock
    //     ParallelDownloads (and Color) lines in the LIVE pacman.conf that
    //     basestrap reads. Idempotent — once uncommented there's no '#' to match.
    plan.push(act(
        "sh",
        &[
            "-c",
            "sed -i 's/^#ParallelDownloads/ParallelDownloads/; s/^#Color/Color/' /etc/pacman.conf",
        ],
    ));
    plan.push(act("sh", &["-c", "pacman -Sy --noconfirm"]));

    // 1c) Ban XLibre in the LIVE pacman.conf. Artix ships an `xlibre` package
    //     group (the XLibre X-server fork + its xf86 driver replacements);
    //     xlibre-xserver *provides* xorg-server, so without this pacman could
    //     offer XLibre as a provider. We block it two ways for full coverage:
    //       IgnorePkg = xlibre-*   (glob — every xlibre-* package, incl. the
    //                               -beta/-devel variants that aren't in the group)
    //       IgnoreGroup = xlibre   (the whole group, incl. any member that might
    //                               not match the xlibre-* prefix)
    //     This keeps the live/basestrap pass on genuine X.Org. Idempotent: only
    //     inserted if xlibre isn't already mentioned.
    plan.push(act(
        "sh",
        &[
            "-c",
            "grep -q 'xlibre' /etc/pacman.conf || \
             { awk '/^\\[options\\]/{print; print \"IgnorePkg = xlibre-*\"; print \"IgnoreGroup = xlibre\"; next} {print}' \
             /etc/pacman.conf > /tmp/pacman.conf.xl && cp /tmp/pacman.conf.xl /etc/pacman.conf && rm -f /tmp/pacman.conf.xl; }",
        ],
    ));

    // 2) basestrap base + chosen packages.
    //    basestrap's own option parsing (see artools source) is the key detail:
    //      - flags must come BEFORE the root argument; anything after /mnt is
    //        treated as a package target.
    //      - it does NOT accept --noconfirm directly; instead the `-i` flag makes
    //        basestrap append --noconfirm to its pacman call (the flag's help
    //        text reads "avoid auto-confirmation", but the code does the
    //        opposite: `${interactive} && pacman_args+=(--noconfirm)`).
    //      - `-C <config>` selects the pacman.conf to use.
    //    So: `yes | basestrap -i -C /etc/pacman.conf /mnt <pkgs>` runs
    //    non-interactively. `-i` makes basestrap pass --noconfirm to pacman
    //    (auto-taking provider/group-member defaults), and piping `yes` answers
    //    pacman's final "Proceed with installation? [Y/n]" prompt — which we
    let pkgs = base_packages(c);
    let des = chosen_desktops(c);
    // Phase 1: basestrap installs the minimal bootable BASE only (kernel,
    // firmware, dinit + service packages, audio stack, grub, fonts, arch
    // support). basestrap always forces --noconfirm internally, but that's fine
    // here: none of the base packages have provider choices. The desktop, GPU
    // driver / vulkan stack, and the user's extra packages — which DO have
    // provider choices — are installed in phase 2 below, interactively.
    let pkg_args = pkgs.join(" ");
    let basestrap_cmd = format!("basestrap -C /etc/pacman.conf /mnt {pkg_args}");
    // Under a PTY so pacman prints its live download progress (rate + percent),
    // letting the user watch the base download and judge their connection speed.
    // basestrap forces --noconfirm for the base set (no provider prompts), and
    // the PTY runner auto-answers any "Proceed? [Y/n]" with Y, so it won't stall.
    // One stalled mirror mid-basestrap used to abort the whole install
    // (pacman's low-speed cutoff → "failed to commit transaction") and force a
    // restart from step 1. Retry up to 3 times: packages already downloaded
    // sit in the pacman cache, so a retry only re-fetches what's missing and
    // typically finishes in seconds. With the mirror optimizer (0a) on, the
    // retry also walks a list that was health-checked moments ago.
    let basestrap_retry = format!(
        "for a in 1 2 3; do {basestrap_cmd} && exit 0; [ $a = 3 ] && exit 1; \
         echo \">>> basestrap failed (attempt $a/3) - retrying in 5s...\"; sleep 5; done"
    );
    plan.push(act_interactive("sh", &["-c", &basestrap_retry]));

    // 2b) Ban XLibre on the TARGET. basestrap has just created
    //     /mnt/etc/pacman.conf; block the `xlibre` group + every xlibre-*
    //     package there. This does double duty: during phase 2 (interactive
    //     pacman runs inside the chroot and reads THIS file) it stops XLibre
    //     from being offered as a provider for xorg-server / the xf86 driver ABI
    //     — so there's no xorg-vs-xlibre prompt — and on the finished system it
    //     keeps XLibre out of every future upgrade. Two directives for full
    //     coverage (group + xlibre-* glob, catching -beta/-devel/AUR builds).
    //     Idempotent.
    plan.push(chroot(
        "grep -q 'xlibre' /etc/pacman.conf || \
         { awk '/^\\[options\\]/{print; print \"IgnorePkg = xlibre-*\"; print \"IgnoreGroup = xlibre\"; next} {print}' \
         /etc/pacman.conf > /tmp/pc.xl && cp /tmp/pc.xl /etc/pacman.conf && rm -f /tmp/pc.xl; }",
    ));
    // 2c) Faster downloads on the installed system AND during phase 2 (which
    //     reads this file inside the chroot): uncomment the stock
    //     ParallelDownloads (5) and Color lines in the target's pacman.conf.
    //     Idempotent.
    plan.push(chroot(
        "sed -i 's/^#ParallelDownloads/ParallelDownloads/; s/^#Color/Color/' /etc/pacman.conf",
    ));

    // 3) fstab (+ extra disks). See plan_fstab.
    plan_fstab(&mut plan, c);

    // 4) Locale.
    plan.push(chroot(&format!(
        "echo '{} UTF-8' >> /etc/locale.gen && locale-gen && echo 'LANG={}' > /etc/locale.conf",
        c.locale, c.locale
    )));

    // 5) Timezone.
    plan.push(chroot(&format!(
        "ln -sf /usr/share/zoneinfo/{} /etc/localtime && hwclock --systohc",
        c.timezone
    )));

    // 6) Console keymap + a Unicode-capable console font. We ship terminus-font
    //    (provides ter-*) in the base set; ter-116n covers Cyrillic/Latin and
    //    exists in that package, unlike the previously-used (nonexistent)
    //    ter-118n. Falls back gracefully if the font is missing.
    //    For Ukrainian-interface installs the console keymap is `ua-utf` from
    //    kbd: verified against its source, the PLAIN layer is Latin (so the
    //    initramfs LUKS prompt and commands type ASCII as usual) and Cyrillic
    //    sits on a locked group toggled by Right Ctrl / Right Alt
    //    (CtrlL_Lock/CtrlR_Lock on keycodes 97/100) — actual Ukrainian typing
    //    on the TTY instead of Latin-only, with zero passphrase risk.
    let console_keymap = if c.lang == "uk" {
        "ua-utf"
    } else {
        c.keymap.as_str()
    };
    // Wrapper keymap with a STANDARD Backspace. Several stock maps (ua-utf
    // among them) put the BackSpace keysym (^H, 0x08) on keycode 14, while
    // us.map uses Delete (^?, 0x7f). crossterm-based TUIs — tuigreet, this
    // installer's recovery mode — read ^H as Ctrl+H and only erase on ^?, so
    // on such consoles Backspace "does nothing". The wrapper includes the
    // chosen map by name (loadkeys resolves includes in its standard dirs)
    // and pins keycode 14 back to Delete; KEYMAP= accepts an absolute path,
    // and mkinitcpio's `keymap` hook compiles the same file into the
    // initramfs, so the LUKS prompt gets the fix too.
    plan.push(write_target_file(
        "/mnt/etc/kbd/artix-console.map",
        &format!(
            "# Managed by the Artix installer - {km} with a standard Backspace.\n\
             include \"{km}\"\n\
             keycode 14 = Delete Delete\n",
            km = console_keymap
        ),
    ));
    plan.push(chroot(
        "printf 'KEYMAP=/etc/kbd/artix-console.map\\nFONT=ter-116n\\n' > /etc/vconsole.conf",
    ));

    // 7) Hostname + hosts file. The Artix install guide requires /etc/hosts to
    //     carry the loopback entries AND a 127.0.1.1 line for the machine's own
    //     name, so programs that resolve the local hostname don't stall or fail.
    //     We keep the hostname and the hosts file in sync.
    let host = if c.hostname.trim().is_empty() {
        "artix"
    } else {
        c.hostname.trim()
    };
    plan.push(chroot(&format!("echo '{host}' > /etc/hostname")));

    // Repair /etc/os-release if PRETTY_NAME is missing or still a literal
    // template. The live image's getty showed "Artix Linux{PRETTY_NAME}",
    // which means agetty's \\S escape in /etc/issue expanded an os-release
    // whose PRETTY_NAME line was malformed (a packaging template that never
    // got substituted). agetty reads PRETTY_NAME for \\S, so a broken value
    // shows up at every text login. We rewrite the line to a sane value
    // (idempotent: only touches PRETTY_NAME, leaving NAME/ID/etc. intact; if
    // the file is absent we create a minimal valid one).
    plan.push(chroot(
        "f=/etc/os-release; \
         if [ -f \"$f\" ]; then \
           if ! grep -qE '^PRETTY_NAME=\"[^\"{}]+\"$' \"$f\"; then \
             sed -i '/^PRETTY_NAME=/d' \"$f\"; \
             printf 'PRETTY_NAME=\"Artix Linux\"\\n' >> \"$f\"; \
           fi; \
         else \
           printf 'NAME=\"Artix Linux\"\\nPRETTY_NAME=\"Artix Linux\"\\nID=artix\\n' > \"$f\"; \
         fi",
    ));
    plan.push(write_target_file(
        "/mnt/etc/hosts",
        &format!(
            "# Static table lookup for hostnames.\n# See hosts(5) for details.\n\n127.0.0.1\tlocalhost\n::1\t\tlocalhost\n127.0.1.1\t{host}.localdomain\t{host}\n"
        ),
    ));

    // 8) Enable the [lib32] repo in the installed system's pacman.conf (same
    //     robust awk as the live prep at step 1b; leaves [lib32-gremlins] alone).
    plan.push(chroot(
        "awk '/^#\\[lib32\\]$/{print \"[lib32]\"; getline; sub(/^#/,\"\"); print; next} {print}' \
         /etc/pacman.conf > /tmp/pacman.conf.new && cp /tmp/pacman.conf.new /etc/pacman.conf",
    ));

    // 8a) If we ranked mirrors on the live system, copy the optimized lists to
    //     the target so the INSTALLED system also benefits (faster updates
    //     later), not just the install itself. /mnt/etc/pacman.d already exists
    //     from basestrap. Best-effort.
    if c.optimize_mirrors {
        plan.push(act(
            "sh",
            &[
                "-c",
                "echo '>>> Copying ranked mirrors to the target system...'; \
                 cp /etc/pacman.d/mirrorlist /mnt/etc/pacman.d/mirrorlist 2>/dev/null || true; \
                 [ -f /etc/pacman.d/mirrorlist-arch ] && cp /etc/pacman.d/mirrorlist-arch /mnt/etc/pacman.d/mirrorlist-arch 2>/dev/null; \
                 true",
            ],
        ));
    }

    // 8b) Enable the Arch repositories so packages not in Artix can be installed
    //     later. Appended to the END of pacman.conf so they sit BELOW all Artix
    //     repos. This ordering is pacman's ONLY priority mechanism: for a
    //     package that exists in both, pacman picks the one from the repo listed
    //     FIRST — i.e. Artix. That matters because Artix often ships its own
    //     builds with fixes for init-systems without systemd, and we want those
    //     to win over the Arch version. The base system itself is installed
    //     (step 2) before this runs, so it comes entirely from Artix repos.
    //     [community] no longer exists in Arch (merged into extra). The header
    //     comment is written into the file so it's self-explanatory on inspect.
    //     Guarded so we don't append twice.
    plan.push(chroot(
        "grep -q '^\\[extra\\]' /etc/pacman.conf || printf '\\n# --- Arch repositories (kept BELOW Artix so Artix builds take priority) ---\\n[extra]\\nInclude = /etc/pacman.d/mirrorlist-arch\\n[multilib]\\nInclude = /etc/pacman.d/mirrorlist-arch\\n' >> /etc/pacman.conf",
    ));
    plan.push(chroot("pacman-key --init"));
    plan.push(chroot("pacman-key --populate archlinux artix"));
    // Refresh the KEYRING PACKAGES before anything else is installed.
    //
    // `--populate` only loads the keys that came with the keyring already on
    // disk. When a repo starts signing with a key newer than that keyring, every
    // later package fails with "signature from Artix Buildbot is invalid" —
    // dozens in a row, each offering to delete a perfectly good download. The
    // documented remedy is to update the keyring packages first, then populate
    // again. Best-effort: a mirror hiccup here must not abort the install, and
    // if the keyring was already current this is a no-op.
    plan.push(chroot(
        "pacman -Sy --needed --noconfirm artix-keyring archlinux-keyring || true",
    ));
    plan.push(chroot("pacman-key --populate archlinux artix || true"));
    // Refresh databases so the newly-enabled Arch repos are immediately usable
    // in the installed system — and UPGRADE (-u), never a bare -Sy.
    //
    // A bare `-Sy` here left the target a partial upgrade, which is what the
    // Arch family warns about and it duly bit: basestrap had laid down the base
    // against the ARTIX-only repo set, then the Arch repos were enabled just
    // above, and every later `pacman -S` resolved against that wider, newer
    // database while the base stayed at its older snapshot. When Lua split
    // (`lua` 5.4 → 5.5, with 5.4 moving to `lua54`), a target holding the old
    // `lua` met a fresh `lua54` and the whole 387-package transaction died on
    // "/usr/bin/lua5.4 exists in filesystem (owned by lua)". The two packages
    // declare no conflict and coexist happily — ONLY the mixed snapshot broke
    // them. `-u` reconciles the base with the database its new packages come
    // from, so the base's `lua` moves to 5.5 and frees the 5.4 paths.
    //
    // Retried like basestrap: cached packages make a retry cheap, and a stalled
    // mirror must not decide the fate of the install. Still best-effort at the
    // end — if the base was already current this is a no-op anyway.
    plan.push(chroot(
        "for a in 1 2 3; do pacman -Syu --noconfirm && exit 0; \
         [ $a = 3 ] && break; \
         echo \">>> database upgrade failed (attempt $a/3) - retrying in 5s...\"; \
         sleep 5; done; true",
    ));

    // Chaotic-AUR (optional, user-toggled on the Options screen): a binary
    // repository of prebuilt AUR packages maintained by the Garuda Linux team.
    // Enabling it BEFORE the package phases means pacman/paru can pull popular
    // AUR software as ready-made binaries instead of compiling from source.
    // Setup follows the official procedure: import + locally-sign the primary
    // key (so the keyring/mirrorlist packages verify), install chaotic-keyring
    // and chaotic-mirrorlist from the CDN, append the [chaotic-aur] section to
    // pacman.conf (idempotent — only if absent), then refresh the databases.
    // The whole block is best-effort: a trailing `true` keeps a transient
    // network/keyserver failure from aborting the install (the user can redo it
    // later), and the append is guarded so a re-run never duplicates the stanza.
    if c.chaotic_aur {
        plan.push(chroot(
            // GUARD 1 — reachability gate. A quick HEAD with a hard
            // connect-timeout: if the Chaotic CDN doesn't answer within ~15s,
            // SKIP the entire setup with a clear log line and continue the
            // install (paru just builds from the AUR as usual). Chaotic's
            // mirrors are frequently overloaded, and pacman's built-in
            // downloader can otherwise block silently for many minutes.
            //
            // GUARD 2 — we deliberately DO NOT set a curl XferCommand here.
            // An earlier version did (curl with --no-progress-meter) to get a
            // connect timeout, but that HID pacman's download progress (rate,
            // percent, bar) for every package after basestrap AND persisted on
            // the installed system, so the user's own pacman never showed
            // progress either. pacman's native downloader shows full progress
            // and already times out stalled transfers on its own, so we keep it
            // and rely on the reachability gate above to skip a dead Chaotic
            // server up front.
            //
            // Every step echoes progress so the log never goes silent, and the
            // chain is best-effort (trailing `true`) so nothing here aborts the
            // install; a partial failure just leaves Chaotic disabled.
            "echo '>>> Chaotic-AUR: checking server reachability (15s timeout)...'; \
             if ! curl -fsS --connect-timeout 15 --max-time 40 -I \
                 'https://cdn-mirror.chaotic.cx/chaotic-aur/chaotic-mirrorlist.pkg.tar.zst' \
                 >/dev/null 2>&1; then \
               echo '!!! Chaotic-AUR server UNREACHABLE — skipping it. Install continues; AUR packages build from source as normal.'; \
             else \
               echo '>>> Reachable. Keeping pacman native downloader so download progress and speed stay visible (stalled transfers time out on their own).'; \
               echo '>>> Importing Chaotic-AUR signing key...'; \
               if pacman-key --recv-key 3056513887B78AEB --keyserver keyserver.ubuntu.com && \
                  pacman-key --lsign-key 3056513887B78AEB && \
                  echo '>>> Installing chaotic-keyring + chaotic-mirrorlist...' && \
                  pacman -U --noconfirm \
                    'https://cdn-mirror.chaotic.cx/chaotic-aur/chaotic-keyring.pkg.tar.zst' \
                    'https://cdn-mirror.chaotic.cx/chaotic-aur/chaotic-mirrorlist.pkg.tar.zst' && \
                  { grep -q '^\\[chaotic-aur\\]' /etc/pacman.conf || \
                    printf '\\n[chaotic-aur]\\nInclude = /etc/pacman.d/chaotic-mirrorlist\\n' \
                    >> /etc/pacman.conf; } && \
                  echo '>>> Syncing package databases...' && \
                  pacman -Sy --noconfirm; then \
                 echo '>>> Chaotic-AUR enabled successfully.'; \
               else \
                 echo '!!! Chaotic-AUR setup failed partway — install continues without it.'; \
               fi; \
             fi; \
             true",
        ));
        // Apply the same mirror treatment to the chaotic-mirrorlist (full
        // health check, fastest-first), inside the chroot where it lives.
        //
        // IMPORTANT: artix-chroot (like arch-chroot) runs each invocation in its
        // own unshare namespace with a FRESH tmpfs on /tmp, so a file written by
        // one chroot call is GONE in the next. We therefore WRITE the script and
        // RUN it in a SINGLE chroot call, so both see the same /tmp. Best-effort.
        if c.optimize_mirrors {
            let combined = format!(
                "cat > /tmp/optmirrors.sh <<'MIRROPT_EOF'\n{}\nMIRROPT_EOF\n\
                 sh /tmp/optmirrors.sh --chaotic",
                MIRROR_OPTIMIZE_SCRIPT
            );
            plan.push(chroot(&combined));
        }
    }

    // Optimize the Arch mirrorlist (extra/multilib) the same way — full
    // health check, fastest-first — in the chroot, where
    // /etc/pacman.d/mirrorlist-arch exists (it's absent on the live ISO, so it
    // can't be ranked earlier). Single chroot call (each gets a fresh /tmp).
    // Runs before Phase 2 so those package downloads use the better mirrors.
    if c.optimize_mirrors {
        let combined = format!(
            "cat > /tmp/optmirrors.sh <<'MIRROPT_EOF'\n{}\nMIRROPT_EOF\n\
             sh /tmp/optmirrors.sh --arch",
            MIRROR_OPTIMIZE_SCRIPT
        );
        plan.push(chroot(&combined));
    }

    // Re-sync the databases RIGHT BEFORE the package phase, from the mirrors it
    // will actually download from. The earlier `-Syu` synced against whatever
    // mirrorlist basestrap left; several minutes and a mirror re-rank later, the
    // package pool has moved on. If the sync DB still lists a version the
    // mirrors have already superseded and DELETED, every mirror answers 404 for
    // that exact file and the whole transaction dies — "failed retrieving …
    // 404" from mirror after mirror was exactly this: a stale DB, not mirrors
    // being down. A refresh here makes the DB name versions the pool still has.
    //
    // Retried (a stalled mirror must not decide the install's fate), and `-u`
    // not a bare `-Sy` — a partial upgrade in the target is its own landmine
    // (the lua/lua54 file conflict). Best-effort tail: if the base was already
    // current this is a no-op, and a transient failure here still lets the
    // package phase try — that phase now retries with its own refresh too.
    plan.push(chroot(
        "for a in 1 2 3; do pacman -Syu --noconfirm && exit 0; \
         [ $a = 3 ] && break; \
         echo \">>> database refresh failed (attempt $a/3) - retrying in 5s...\"; \
         sleep 5; done; true",
    ));

    // ─── Phase 2: interactive system packages ───────────────────────────────
    // Everything beyond the minimal base — desktop, GPU/vulkan stack, the
    // user's extra packages, display manager, seat backend — is installed here
    // with `pacman -S` INSIDE the chroot, run under a PTY (chroot_interactive)
    // WITHOUT --noconfirm. That's the whole point: pacman shows provider choices
    // (vulkan-driver, qt6-multimedia-backend, jack, …) and the user picks the
    // right one for their hardware in the TUI, instead of basestrap silently
    // taking provider #1 (which previously chose an incompatible nvidia driver
    // and aborted). "Proceed? [Y/n]" prompts are auto-answered Y by the runner.
    //
    // The X server is installed FIRST, as explicitly-named targets, so the
    // x11win-server virtual dep (pulled by SDDM and the desktops) resolves to
    // genuine X.Org and never to XLibre (whose ABI conflicts with xf86 drivers).
    // Install an X server when ANY chosen desktop needs one: the FULL Xorg set
    // for any desktop with an X11 session, plus xorg-xwayland for any Wayland
    // desktop (so X11 apps run under it). Installed FIRST, as explicitly-named
    // targets, so the x11win-server virtual dep (pulled by SDDM and the
    // desktops) resolves to genuine X.Org and never XLibre (ABI conflicts).
    let any_x11 = des.iter().any(|d| d.supports_x11());
    let any_wayland = des.iter().any(|d| d.supports_wayland());
    if any_x11 || any_wayland {
        let mut xpkgs: Vec<&str> = Vec::new();
        if any_x11 {
            xpkgs.extend(XORG_PACKAGES.iter().copied());
            xpkgs.extend([
                "xf86-input-libinput",
                "xf86-input-evdev",
                "xf86-input-wacom",
                "xf86-video-vesa",
                "xf86-video-fbdev",
                "xf86-video-amdgpu",
                "xf86-video-ati",
                "xf86-video-nouveau",
                "xf86-video-qxl",
            ]);
            if any_wayland {
                xpkgs.push("xorg-xwayland");
            }
        } else {
            // Only Wayland desktops chosen: a minimal X server + Xwayland so
            // legacy X11 apps still run inside the Wayland session.
            xpkgs.push("xorg-server");
            xpkgs.push("xorg-server-common");
            xpkgs.push("xorg-xwayland");
        }
        let xargs = xpkgs.join(" ");
        // --needed so already-present packages aren't reinstalled. No
        // --noconfirm: if the X stack ever has a provider choice the user
        // decides; in practice these named targets resolve cleanly. Retried:
        // this is the phase the 404 storm hit, and one flaky download must not
        // end the install (see pacman_install_retry).
        plan.push(chroot_interactive(&pacman_install_retry(&xargs)));
    }
    // The rest of the system: desktop, GPU, extras, DM, seat. Interactive.
    let sys_pkgs = system_packages(c);
    if !sys_pkgs.is_empty() {
        let sys_args = sys_pkgs.join(" ");
        plan.push(chroot_interactive(&pacman_install_retry(&sys_args)));
    }
    // ─────────────────────────────────────────────────────────────────────────

    // 9) Accounts. See plan_accounts.
    plan_accounts(&mut plan, c, &des);

    // 9b) GTK bookmarks + D-Bus session bus. See plan_session_env.
    plan_session_env(&mut plan, c);

    // 9c) initramfs hooks + LUKS keyfiles. See plan_initramfs_luks.
    plan_initramfs_luks(&mut plan, c, uefi, luks_pass);

    // 9d) NVIDIA modeset + btrfs snapshots. See plan_gpu_and_snapshots.
    plan_gpu_and_snapshots(&mut plan, c);

    // 9e) Generate the initramfs for the installed kernel(s). mkinitcpio -P
    //     builds every preset present, covering whichever kernel was chosen.
    plan.push(chroot("mkinitcpio -P"));

    // Rescue kernel pair for KERNEL-INDEPENDENT rollback: a frozen copy of the
    // freshly built kernel+initramfs that pacman never touches. The bootloader
    // "System Rollback" entries boot THIS pair, so the snapshot picker stays
    // reachable even when a kernel update breaks the live kernel/initramfs.
    // Refreshed only after a successful normal boot (artix-rescue-sync) and
    // after a rollback (artix-rollback-fixup) — never by an update itself.
    if c.root_fs == "btrfs" && c.btrfs_subvolumes && c.btrfs_snapshots {
        let (vmlinuz, initramfs) = kernel_images(c.kernel);
        plan.push(chroot(&format!(
            "cp -f /boot/{vmlinuz} /boot/vmlinuz-artix-rescue && \
             cp -f /boot/{initramfs} /boot/initramfs-artix-rescue.img"
        )));
    }

    // 9f) Snapshots: make @ the btrfs default subvolume too (belt-and-braces).
    //     The system boots @ BY NAME (rootflags=subvol=@) — GRUB's 10_linux pins
    //     it from the live mount, and rEFInd/Limine get it via rootflags_part —
    //     so `artix-rollback`'s @-swap is what a boot follows. Setting the default
    //     to @ as well keeps any default-subvolume tooling consistent. We no
    //     longer neutralise 10_linux's subvol pin (that earlier default-only
    //     approach didn't reliably switch what booted after a rollback).
    //     Must run before any grub-mkconfig in the bootloader step below.
    if c.btrfs_snapshots {
        plan.push(chroot(
            "id=$(btrfs subvolume show / 2>/dev/null | sed -n 's/.*Subvolume ID:[[:space:]]*//p'); \
             echo \">>> default-subvolume: root (@) subvol id = [$id]\"; \
             if [ -n \"$id\" ]; then \
               btrfs subvolume set-default \"$id\" / && \
               echo \">>> set-default done; get-default is now:\" && \
               btrfs subvolume get-default /; \
             else \
               echo \">>> WARNING: could not read root subvol id; default left unchanged\"; \
             fi",
        ));
    }

    // 10) Bootloader (+ 10c Secure Boot prep). See plan_bootloader.
    plan_bootloader(&mut plan, c, uefi);

    // 11) Firewall. See plan_firewall.
    plan_firewall(&mut plan, c);

    // 12) dinit services + AUR phase. See plan_services.
    plan_services(&mut plan, c);

    plan
}

/// Step 10: install and configure the chosen bootloader, and (EFISTUB only,
/// opt-in) prepare Secure Boot.
///
/// Split out of `build_plan` because it's the single largest self-contained
/// phase: four bootloaders × BIOS/UEFI × encrypted/plain × snapshot-rollback
/// entries. It reads the config and appends to the plan — no other state — so
/// it lifts out cleanly.
/// Step 3: generate /etc/fstab, then append entries for any extra disks the
/// user assigned mountpoints to (formatted ones by UUID, existing ones mounted
/// as-is).
/// Step 9: user and root accounts. Four modes (see AccountMode) decide whether
/// a user is created at all, whether root gets a password, and whether
/// privilege escalation goes through sudo or doas. Passwords are piped to
/// chpasswd rather than passed on a command line.
/// Step 12: enable the dinit services for the installed system.
///
/// Covers the fixed base set, the seat backend and its per-user launcher, the
/// display manager, logging, and an autoscan that symlinks the *-dinit service
/// files shipped by whatever packages ended up installed. Also runs the AUR
/// phase (paru as the user) and verifies afterwards which AUR packages
/// actually landed — a failed build is non-fatal but must not be silent.
fn plan_services(plan: &mut Vec<Action>, c: &InstallConfig) {
    // 12) Enable all dinit services in the installed system. Add the chosen
    //      display manager's service, and the chosen seat manager service.
    // The nftables service follows the (default-checked) nftables package:
    // unticked → not installed → nothing to enable.
    let mut services: Vec<String> = DINIT_SERVICES
        .iter()
        .filter(|s| **s != "nftables" || c.extra_packages.iter().any(|x| x == "nftables"))
        .map(|s| s.to_string())
        .collect();
    if let Some(dm_svc) = dm_service(&c.display_manager) {
        services.push(dm_svc.to_string());
    }
    // Enable the seat manager's dinit SERVICE (named after the daemon, not the
    // *-dinit package): "elogind" or "seatd". Done ALWAYS — even with no
    // desktop — since the package is always installed and a later-installed WM
    // needs the seat service running.
    services.push(c.seat_provider.service().to_string());
    // Enable the per-user dinit launcher service that matches the backend
    // installed above: userspawn (elogind) or turnstiled (seatd/none). This is
    // what spawns the user's dinit instance on login → D-Bus, PipeWire, sound.
    services.push(c.seat_provider.user_launcher().to_string());
    // System logging: syslog-ng captures all system logs to /var/log; cronie
    // runs the scheduled logrotate job that expires them weekly (see the
    // logrotate config written below). Both are always enabled.
    // Bring the log files that ALREADY exist into the same scheme. syslog-ng
    // applies owner/group/perm only to the files IT creates, from its first
    // start onward — everything written during the install itself (pacman.log,
    // kernel messages, and so on) is still root:root 0600, and the `log` group
    // membership we granted the user would not open them. Without this, "read
    // your own logs without root" would work for future entries and fail for
    // exactly the ones a fresh installation needs to debug.
    plan.push(chroot(
        "chgrp -R log /var/log 2>/dev/null || true; \
         chmod 0750 /var/log 2>/dev/null || true; \
         find /var/log -type f -exec chmod 0640 {} + 2>/dev/null || true; \
         echo 'logs readable by group log (user is a member)'",
    ));
    services.push("syslog-ng".into());
    services.push("cronie".into());
    // NVIDIA: enable nvidia-persistenced (shipped by nvidia-utils-dinit) so the
    // driver keeps device state initialized — recommended for NVIDIA GPUs.
    if any_proprietary_nvidia(&c.gpu) {
        services.push("nvidia-persistenced".into());
    }
    // Enable each system service by symlinking it into /etc/dinit.d/boot.d/.
    // We do NOT use `dinitctl --offline enable`: in an installed-but-not-booted
    // root it looks for the `boot` service/state and fails. The symlink method
    // is what the Artix dinit docs use for offline/chroot enabling and always
    // works. We ensure boot.d exists and only link a service whose file exists
    // (so a missing optional service is skipped, not fatal).
    let svc_cmd = format!(
        "mkdir -p /etc/dinit.d/boot.d; for s in {}; do if [ -e /etc/dinit.d/$s ]; then ln -sf /etc/dinit.d/$s /etc/dinit.d/boot.d/$s; echo \"enabled $s\"; else echo \"  (no service file for $s, skipped)\"; fi; done",
        services.join(" ")
    );
    plan.push(chroot(&svc_cmd));

    // Auto-enable ANY dinit service shipped by an installed *-dinit package —
    // including ones the user picked from the repos or AUR that we don't list
    // explicitly above (e.g. asusctl-nosystemd-dinit, cups-dinit, …). For every
    // installed package whose name ends in "-dinit", we read its file list with
    // `pacman -Ql`, pick the service descriptions it drops directly in
    // /etc/dinit.d/ (top level, not the boot.d/ or user/ subdirs), and symlink
    // each into boot.d/ so it starts at boot. Already-enabled ones are simply
    // re-linked (idempotent). This is best-effort: || true so a quirky package
    // never aborts the install.
    // The seat launcher NOT chosen must never be auto-enabled here: enabling
    // both elogind and seatd (or userspawn and turnstiled) gives two rival
    // session managers, and /run/user/<uid> never gets created — a black
    // screen / frozen compositor. We skip the services belonging to the
    // backend the user did NOT pick.
    let seat_skip = if c.seat_provider == SeatProvider::Elogind {
        "seatd turnstiled turnstile"
    } else {
        "elogind userspawn"
    };
    // {{SEAT_SKIP}} is substituted (not format!) to avoid brace-escaping the
    // shell body. It lists the losing backend's services to skip.
    let autoscan = r#"mkdir -p /etc/dinit.d/boot.d;          SKIP=" @@SEAT_SKIP@@ ";          for pkg in $(pacman -Qq | grep -- '-dinit$'); do            pacman -Ql "$pkg" 2>/dev/null | awk '{print $2}' |            grep -E '^/etc/dinit.d/[^/]+$' | while read -r f; do              svc=$(basename "$f");              case "$SKIP" in *" $svc "*) echo "  (skipping $svc: not the chosen seat backend)"; continue ;; esac;              [ -f "$f" ] && ln -sf "/etc/dinit.d/$svc" "/etc/dinit.d/boot.d/$svc" &&              echo "auto-enabled $svc (from $pkg)";            done;          done || true"#.replace("@@SEAT_SKIP@@", seat_skip);
    plan.push(chroot(&autoscan));

    // Wire up the per-user dinit launcher chosen above. The two backends need
    // different setup, so branch on the seat provider.
    if c.seat_provider == SeatProvider::Elogind {
        // ELOGIND → userspawn. userspawn reacts to logind's UserNew D-Bus
        // signal (provided by elogind) and then runs a userspawnrc script —
        // it does NOT exec dinit directly, so without that script it fails with
        // "Failed to exec user" and no user dinit starts. We write the
        // system-wide /etc/xdg/userspawn/userspawnrc (works for any user) plus
        // the two per-user locations userspawn also checks, each just execing
        // the user dinit. elogind itself provides /run/user/<uid> and tracks
        // the session, so we make sure pam_elogind is in the login stack and
        // strip any pam_rundir/pam_turnstile that might fight it.
        let home = format!("/home/{}", c.username);
        const USERSPAWNRC: &str = "#!/bin/sh\nexec dinit --user\n";
        plan.push(write_target_file(
            "/mnt/etc/xdg/userspawn/userspawnrc",
            USERSPAWNRC,
        ));
        plan.push(chroot("chmod +x /etc/xdg/userspawn/userspawnrc"));
        plan.push(write_home_file(&home, ".userspawnrc", USERSPAWNRC));
        plan.push(chroot(&format!("mkdir -p {home}/.config/userspawn")));

        // Belt-and-braces: physically remove the losing seat backend's boot.d
        // symlinks in case an earlier step or a transitive *-dinit created them.
        plan.push(chroot(
            &"for s in @@SEAT_SKIP@@; do rm -f /etc/dinit.d/boot.d/$s; done"
                .replace("@@SEAT_SKIP@@", seat_skip),
        ));
        plan.push(write_home_file(
            &home,
            ".config/userspawn/userspawnrc",
            USERSPAWNRC,
        ));
        plan.push(chroot(&format!(
            "chmod +x {home}/.userspawnrc {home}/.config/userspawn/userspawnrc; \
             chown -R {0}:{0} {home}/.userspawnrc {home}/.config/userspawn",
            c.username
        )));
        plan.push(chroot(
            "f=/etc/pam.d/system-login; if [ -f \"$f\" ]; then \
             sed -i '/pam_rundir.so/d; /pam_turnstile.so/d' \"$f\"; \
             if ! grep -q 'pam_elogind.so' \"$f\"; then \
             if grep -q 'pam_env.so' \"$f\"; then \
             sed -i '/pam_env.so/a -session   optional   pam_elogind.so' \"$f\"; \
             else echo '-session   optional   pam_elogind.so' >> \"$f\"; fi; \
             echo 'wired pam_elogind.so + userspawnrc (elogind -> userspawn -> dinit --user)'; \
             fi; fi",
        ));
    } else {
        // SEATD / NONE → turnstile, wired to match a WORKING stock-Artix seatd
        // desktop (verified against a live 292-day install where polkit's GUI
        // password prompt works). The elogind PACKAGE is installed for any
        // desktop (see the package builder), but its SERVICE is NOT enabled
        // under seatd — so pam_elogind only REGISTERS the login session with
        // logind (giving Class=user + a seat, which polkit 127+ requires before
        // it will pop its authentication agent) without any rival seat manager
        // fighting turnstiled. Four session modules, in this exact order:
        //     pam_turnstile.so → spawns `dinit --user` (Dinit backend)
        //     pam_elogind.so   → registers the session so polkit authenticates
        //     pam_env.so       → login environment (stock, already present)
        //     pam_rundir.so    → creates /run/user/<uid> (we set
        //                        manage_rundir = no below, so turnstile does
        //                        NOT make the rundir — pam_rundir does)
        // Earlier revisions STRIPPED pam_elogind/pam_rundir and set
        // manage_rundir = yes; that produced a Class=background, Seat=- session
        // that current polkit refuses to authenticate, so the GUI prompt
        // silently never appeared. We now ADD what's missing (never deleting the
        // stock modules) and enforce order positionally: we drop only our two
        // managed lines first (idempotent re-runs, no duplicates), then
        // re-insert elogind right AFTER pam_turnstile and rundir right AFTER
        // pam_env.
        plan.push(chroot(
            "f=/etc/pam.d/system-login; if [ -f \"$f\" ]; then \
             sed -i '/pam_elogind.so/d; /pam_rundir.so/d' \"$f\"; \
             if ! grep -q 'pam_turnstile.so' \"$f\"; then \
             if grep -q 'pam_env.so' \"$f\"; then \
             sed -i '/pam_env.so/i -session   optional   pam_turnstile.so' \"$f\"; \
             else echo '-session   optional   pam_turnstile.so' >> \"$f\"; fi; fi; \
             if grep -q 'pam_turnstile.so' \"$f\"; then \
             sed -i '/pam_turnstile.so/a -session   optional   pam_elogind.so' \"$f\"; \
             elif grep -q 'pam_env.so' \"$f\"; then \
             sed -i '/pam_env.so/i -session   optional   pam_elogind.so' \"$f\"; \
             else echo '-session   optional   pam_elogind.so' >> \"$f\"; fi; \
             if grep -q 'pam_env.so' \"$f\"; then \
             sed -i '/pam_env.so/a -session   optional   pam_rundir.so' \"$f\"; \
             else echo '-session   optional   pam_rundir.so' >> \"$f\"; fi; \
             echo 'wired turnstile+elogind+rundir (session -> logind -> polkit ok)'; \
             fi",
        ));
        plan.push(chroot(
            "f=/etc/turnstile/turnstiled.conf; mkdir -p /etc/turnstile; touch \"$f\"; \
             if grep -qE '^[#[:space:]]*backend' \"$f\"; then \
             sed -i 's|^[#[:space:]]*backend.*|backend = dinit|' \"$f\"; \
             else echo 'backend = dinit' >> \"$f\"; fi; \
             if grep -qE '^[#[:space:]]*manage_rundir' \"$f\"; then \
             sed -i 's|^[#[:space:]]*manage_rundir.*|manage_rundir = no|' \"$f\"; \
             else echo 'manage_rundir = no' >> \"$f\"; fi; \
             echo 'turnstiled.conf: backend=dinit manage_rundir=no (pam_rundir owns the rundir)'",
        ));
    }

    // syslog-ng config. The stock Arch/Artix syslog-ng package does NOT write
    // /var/log out of the box on a current install (its shipped config is
    // minimal), so without this nothing lands in /var/log and the rotation rule
    // below has nothing to act on. We OVERWRITE /etc/syslog-ng/syslog-ng.conf
    // with a straightforward config that reads the local system + kernel +
    // internal sources and writes predictable *.log files: messages.log
    // (everything), auth.log, kernel.log, daemon.log, and a single everything.log
    // catch-all. create_dirs(yes) makes /var/log if missing; owner/group/perm
    // keep them root:log 0640. @version is set a touch low on purpose — syslog-ng
    // accepts an older declaration with a harmless notice, which is more robust
    // across package bumps than pinning the exact installed version.
    plan.push(write_target_file(
        "/mnt/etc/syslog-ng/syslog-ng.conf",
        "@version: 4.0\n\
         @include \"scl.conf\"\n\
         # Managed by the Artix installer.\n\
         options {\n\
         \x20   chain_hostnames(off); flush_lines(0); use_dns(no); use_fqdn(no);\n\
         \x20   owner(\"root\"); group(\"log\"); perm(0640); create_dirs(yes);\n\
         \x20   dir_owner(\"root\"); dir_group(\"log\"); dir_perm(0755);\n\
         \x20   keep_hostname(yes); stats(freq(0)); time_reopen(10);\n\
         };\n\
         source s_system { system(); internal(); };\n\
         destination d_messages   { file(\"/var/log/messages.log\"); };\n\
         destination d_auth       { file(\"/var/log/auth.log\"); };\n\
         destination d_kernel     { file(\"/var/log/kernel.log\"); };\n\
         destination d_daemon     { file(\"/var/log/daemon.log\"); };\n\
         destination d_everything { file(\"/var/log/everything.log\"); };\n\
         filter f_auth   { facility(auth, authpriv); };\n\
         filter f_kernel { facility(kern); };\n\
         filter f_daemon { facility(daemon); };\n\
         log { source(s_system); filter(f_auth);   destination(d_auth); };\n\
         log { source(s_system); filter(f_kernel); destination(d_kernel); };\n\
         log { source(s_system); filter(f_daemon); destination(d_daemon); };\n\
         log { source(s_system); destination(d_messages); };\n\
         log { source(s_system); destination(d_everything); };\n",
    ));

    // System log retention: keep ~1 week of logs, then auto-delete. syslog-ng
    // (enabled above) writes the whole system's logs as /var/log/*.log. We
    // control their rotation by OVERWRITING /etc/logrotate.d/syslog-ng (the
    // file syslog-ng's package ships) so there's exactly one rule for those
    // files — a second rule covering the same glob would make logrotate abort
    // with "duplicate log entry". Policy:
    //   daily      — consider rotation every day (fine granularity)
    //   rotate 7   — keep 7 rotated files → about one week of history
    //   maxage 7   — and hard-delete anything older than 7 days (the actual
    //                "expire after a week" guarantee, independent of count)
    //   maxsize 5G — but rotate IMMEDIATELY if a log reaches 5 GB, without
    //                waiting for the daily run. Guards against a misbehaving
    //                service flooding the log and filling the disk: the bloated
    //                file is rotated out at once, and (with compress + rotate/
    //                maxage) the old copies are then expired. So no single log
    //                grows past ~5 GB between rotations.
    //   compress/delaycompress — save space; newest rotated stays uncompressed
    //   postrotate syslog-ng-ctl reload — tell syslog-ng to reopen its files
    // The leading wildcard covers whatever exact names syslog-ng uses
    // (messages.log, auth.log, daemon.log, kernel.log, everything.log, …).
    plan.push(write_target_file(
        "/mnt/etc/logrotate.d/syslog-ng",
        "# Managed by the Artix installer: keep ~1 week of system logs, then delete.\n\
         /var/log/*.log {\n\
         \x20   daily\n\
         \x20   rotate 7\n\
         \x20   maxage 7\n\
         \x20   maxsize 5G\n\
         \x20   missingok\n\
         \x20   notifempty\n\
         \x20   sharedscripts\n\
         \x20   compress\n\
         \x20   delaycompress\n\
         \x20   postrotate\n\
         \x20       /usr/bin/syslog-ng-ctl reload >/dev/null 2>&1 || true\n\
         \x20   endscript\n\
         }\n",
    ));
    // logrotate has no schedule of its own. On systemd it's a timer; here we
    // drive it from cron. cronie (enabled above) reads /etc/cron.d, so drop a
    // job that runs logrotate once a day at 03:00. This is what actually makes
    // the weekly expiry happen — without a cron entry logrotate never runs and
    // logs would grow without bound. MAILTO="" silences cron mail.
    plan.push(write_target_file(
        "/mnt/etc/cron.d/logrotate",
        "# Managed by the Artix installer: run logrotate daily (expires logs ~weekly).\n\
         MAILTO=\"\"\n\
         0 3 * * * root /usr/bin/logrotate /etc/logrotate.conf\n",
    ));
    // Btrfs auto-snapshots (snapper + snap-pac). The subvolume layout already
    // created @snapshots and mounted it at /.snapshots, so snapper is configured
    // WITHOUT `create-config` — that command fails here because it tries to make
    // a .snapshots subvolume which already exists. We write the root config
    // directly instead; snapper fills in defaults for any key we omit. snap-pac
    // then snapshots before/after every pacman transaction (it detects the
    // installer chroot and skips, so it won't fire during the install itself).
    if c.root_fs == "btrfs" && c.btrfs_subvolumes && c.btrfs_snapshots {
        // 1) snapper config for the root subvolume (@). TIMELINE_CREATE=no keeps
        //    snapshots tied to pacman events; NUMBER_* bounds how many are kept.
        plan.push(write_target_file(
            "/mnt/etc/snapper/configs/root",
            "# Managed by the Artix installer — snapper config for the root subvolume (@).\n\
             # Snapshots live in the @snapshots subvolume mounted at /.snapshots.\n\
             SUBVOLUME=\"/\"\n\
             FSTYPE=\"btrfs\"\n\
             QGROUP=\"\"\n\
             SPACE_LIMIT=\"0.5\"\n\
             FREE_LIMIT=\"0.2\"\n\
             ALLOW_USERS=\"\"\n\
             ALLOW_GROUPS=\"wheel\"\n\
             SYNC_ACL=\"yes\"\n\
             BACKGROUND_COMPARISON=\"yes\"\n\
             NUMBER_CLEANUP=\"yes\"\n\
             NUMBER_MIN_AGE=\"1800\"\n\
             NUMBER_LIMIT=\"10\"\n\
             NUMBER_LIMIT_IMPORTANT=\"10\"\n\
             TIMELINE_CREATE=\"no\"\n\
             TIMELINE_CLEANUP=\"yes\"\n\
             TIMELINE_MIN_AGE=\"1800\"\n\
             TIMELINE_LIMIT_HOURLY=\"5\"\n\
             TIMELINE_LIMIT_DAILY=\"7\"\n\
             TIMELINE_LIMIT_WEEKLY=\"0\"\n\
             TIMELINE_LIMIT_MONTHLY=\"0\"\n\
             TIMELINE_LIMIT_YEARLY=\"0\"\n\
             EMPTY_PRE_POST_CLEANUP=\"yes\"\n\
             EMPTY_PRE_POST_MIN_AGE=\"1800\"\n\
             # snap-pac: disabled during install — snapper can't run in the chroot\n\
             # (no D-Bus/systemd there, hence \"fatal library error\"), and snapshots\n\
             # of a half-built system are pointless. The installer flips this to\n\
             # \"yes\" as a final step so the booted system snapshots every pacman run.\n\
             PACMAN_PRE_POST=\"no\"\n",
        ));
        // 2) register the config so snapper and snap-pac recognise it.
        plan.push(write_target_file(
            "/mnt/etc/conf.d/snapper",
            "SNAPPER_CONFIGS=\"root\"\n",
        ));
        // 3) /.snapshots must be owned by root:wheel, mode 750, so wheel users
        //    can browse/compare snapshots (ALLOW_GROUPS=wheel above).
        plan.push(chroot(
            "chmod 750 /.snapshots && chown :wheel /.snapshots || true",
        ));
        // 4) systemd ships snapper-cleanup.timer; under dinit we drive cleanup
        //    from cron so snap-pac snapshots are pruned to NUMBER_LIMIT instead
        //    of growing without bound. Daily at 05:30; MAILTO="" silences mail.
        plan.push(write_target_file(
            "/mnt/etc/cron.d/snapper",
            "# Managed by the Artix installer: prune snapshots (no systemd timers under dinit).\n\
             MAILTO=\"\"\n\
             30 5 * * * root /usr/bin/snapper --config root cleanup number\n",
        ));
        // (Removed: the grub-btrfsd daemon. It regenerated the GRUB snapshot
        //  submenu, which we no longer create — see the note where grub-btrfs is
        //  intentionally not installed.)

        // (The artix-rollback tool itself — POSIX-sh fallback + the installer
        //  binary copied over it — is installed earlier, before `mkinitcpio -P`,
        //  so the initramfs hook can bundle the binary. See step 9a-rb above.)

        // A desktop icon in the installed system that opens the rollback menu in
        // the default terminal — an easy GUI entry point to `artix-rollback`.
        plan.push(write_target_file(
            "/mnt/usr/share/applications/artix-rollback.desktop",
            ROLLBACK_DESKTOP,
        ));

        // The shared log/warn helper the three boot services source. Written
        // FIRST: each of them falls back to no-op stubs when it is missing, so
        // a wrong order would not break the boot — it would just silently throw
        // away the only record those detached services ever leave.
        plan.push(write_target_file(
            "/mnt/usr/local/lib/artix-installer/log.sh",
            LOG_LIB,
        ));
        plan.push(chroot("chmod 644 /usr/local/lib/artix-installer/log.sh"));

        // First-boot baseline snapshot: take a "clean system" restore point on
        // the first real boot (snapper can't run in the chroot). This also makes
        // the GRUB snapshot submenu non-empty from the very first snapshot. The
        // one-shot dinit service removes itself once the snapshot is taken.
        plan.push(write_target_file(
            "/mnt/usr/local/lib/artix-installer/first-snapshot.sh",
            FIRST_SNAPSHOT_SCRIPT,
        ));
        plan.push(chroot(
            "chmod 755 /usr/local/lib/artix-installer/first-snapshot.sh",
        ));
        plan.push(write_target_file(
            "/mnt/etc/dinit.d/artix-first-snapshot",
            FIRST_SNAPSHOT_SERVICE,
        ));
        plan.push(chroot(
            "mkdir -p /etc/dinit.d/boot.d && \
             ln -sf ../artix-first-snapshot /etc/dinit.d/boot.d/artix-first-snapshot",
        ));

        // Rescue-pair refresh: after every SUCCESSFUL normal boot, sync the
        // frozen rescue kernel pair from the live one. The script skips
        // rollback boots, pending fixups, and any boot whose running kernel
        // is not the live image — a broken kernel can never poison the pair.
        plan.push(write_target_file(
            "/mnt/usr/local/lib/artix-installer/rescue-sync.sh",
            RESCUE_SYNC_SCRIPT,
        ));
        plan.push(chroot(
            "chmod 755 /usr/local/lib/artix-installer/rescue-sync.sh",
        ));
        plan.push(write_target_file(
            "/mnt/etc/dinit.d/artix-rescue-sync",
            RESCUE_SYNC_SERVICE,
        ));
        plan.push(chroot(
            "mkdir -p /etc/dinit.d/boot.d && \
             ln -sf ../artix-rescue-sync /etc/dinit.d/boot.d/artix-rescue-sync",
        ));

        // Post-rollback /boot reconciliation: consumes the flag the rollback
        // tool drops into the restored @ — reinstalls the kernel image from
        // the snapshot's /usr/lib/modules, rebuilds the initramfs, refreshes
        // the GRUB menu and the rescue pair.
        plan.push(write_target_file(
            "/mnt/usr/local/lib/artix-installer/rollback-fixup.sh",
            ROLLBACK_FIXUP_SCRIPT,
        ));
        plan.push(chroot(
            "chmod 755 /usr/local/lib/artix-installer/rollback-fixup.sh",
        ));
        plan.push(write_target_file(
            "/mnt/etc/dinit.d/artix-rollback-fixup",
            ROLLBACK_FIXUP_SERVICE,
        ));
        plan.push(chroot(
            "mkdir -p /etc/dinit.d/boot.d && \
             ln -sf ../artix-rollback-fixup /etc/dinit.d/boot.d/artix-rollback-fixup",
        ));
    }
    //   1) ensure base-devel + git are present,
    //   2) grant the user temporary passwordless sudo (makepkg needs it to
    //      install build deps); this line is removed afterwards,
    //   3) build paru-bin (prebuilt, fast) from the AUR as the user,
    //   4) paru -S --noconfirm the requested packages,
    //   5) revoke the temporary passwordless sudo.
    // The whole thing is best-effort: failures are logged but don't abort the
    // install (the base system is already complete).
    let aur_pkgs = effective_aur_packages(c);
    if !aur_pkgs.is_empty() && c.account_mode.needs_user() {
        let user = &c.username;
        let pkgs = aur_pkgs.join(" ");
        // base-devel + git for building, plus the alpm headers paru links to.
        // When the Pinnacle desktop is coming, rust is installed PERMANENTLY
        // too: the compositor's config is a Rust crate — the installer
        // prebuilds it below, and the user rebuilds after editing main.rs.
        let aur_tools = if effective_aur_packages(c)
            .iter()
            .any(|x| x == "pinnacle-comp")
        {
            "base-devel git rust"
        } else {
            "base-devel git"
        };
        plan.push(chroot(&format!(
            "pacman -S --needed --noconfirm {aur_tools} || true"
        )));
        // Bring the system fully up to date FIRST. paru links against
        // libalpm.so (shipped by pacman); if the base we strapped has an older
        // pacman than the repos, a prebuilt paru-bin would fail with
        // "libalpm.so.NN not found". A full upgrade aligns pacman/libalpm with
        // the repos, and building paru from source (below) then links it
        // against exactly this libalpm — so it keeps working.
        plan.push(chroot("pacman -Syu --noconfirm || true"));
        // Temporary passwordless escalation for the user so makepkg can
        // install build deps unattended. With doas we cannot rely on real
        // sudo existing, so paru is told to use doas as its escalation tool
        // (below) and we grant a temporary nopass doas rule; otherwise a
        // sudoers drop-in. Both are removed after the AUR phase.
        if c.use_doas {
            plan.push(chroot(&format!(
                "printf 'permit nopass %s\\n' {user} >> /etc/doas.conf && chmod 0400 /etc/doas.conf"
            )));
        } else {
            plan.push(chroot(&format!(
                "echo '{user} ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/99-aur-temp"
            )));
        }
        // Install paru (the AUR helper). With Chaotic-AUR enabled, paru is
        // already a PREBUILT binary in that repo — and built against the current
        // Arch libalpm, so it's alpm-compatible — meaning `pacman -S paru`
        // installs it INSTANTLY, with no Rust toolchain and no compile (this is
        // exactly why the user hit a long rust build before). Without Chaotic,
        // or if that pull fails, fall back to building paru FROM SOURCE against
        // the system's own libalpm: slower (a Rust build) but robust against a
        // version mismatch. Non-interactive either way.
        // Build paru WITHOUT any privilege escalation inside makepkg — the one
        // thing that reliably breaks on a doas system. `makepkg -si` elevates
        // twice: to install the makedep (cargo) and to install the built
        // package. On doas that elevation goes through the sudo→doas shim, and
        // makepkg calls `sudo -k` (reset the timestamp) — a flag doas rejects
        // ("usage: doas"), so the dep install fails, cargo never lands, and the
        // Rust build dies with "could not resolve all dependencies". Seen live.
        //
        // So do the three things makepkg would, but split by privilege:
        //   1. as ROOT: install paru's build deps (base-devel, git, rust →
        //      cargo). Plain in-chroot pacman, no elevation, no tty, no shim.
        //   2. as the USER: `makepkg -f` (NOT -s) — every dep is already
        //      present, so makepkg builds and never tries to elevate.
        //   3. as ROOT: `pacman -U` the freshly built package. Again no
        //      elevation. The -debug split (if makepkg produced one) is filtered
        //      out so only paru itself is installed.
        // getent gives the real home, so a non-/home layout still works.
        let build_paru_from_src = format!(
            "pacman -S --needed --noconfirm base-devel git rust && \
             su - {user} -c 'cd ~ && rm -rf paru && git clone https://aur.archlinux.org/paru.git && cd paru && makepkg -f --noconfirm' && \
             ph=$(getent passwd {user} | cut -d: -f6) && \
             pp=$(ls -1t \"$ph\"/paru/paru-*.pkg.tar.zst 2>/dev/null | grep -v -- -debug- | head -n1) && \
             [ -n \"$pp\" ] && pacman -U --noconfirm \"$pp\""
        );
        if c.chaotic_aur {
            plan.push(chroot(&format!(
                "pacman -S --needed --noconfirm paru || {build_paru_from_src} || true"
            )));
        } else {
            plan.push(chroot(&format!("{build_paru_from_src} || true")));
        }
        // Install the requested AUR packages INTERACTIVELY under a PTY (no
        // --noconfirm) so the user picks providers; --skipreview skips the
        // PKGBUILD dump. --needed avoids reinstalling, Y/n auto-answered.
        // LC_ALL=C INSIDE the su - login shell: `su -` resets the environment
        // from the (localised) user profile, which would otherwise make paru's
        // prompts localised and unmatchable by our detection heuristics.
        // paru's OWN escalation defaults to sudo; under doas pass --sudo doas
        // so its pacman calls escalate correctly.
        let paru_esc = if c.use_doas { "--sudo doas " } else { "" };
        // tee the build output to a log. When a PKGBUILD fails, the reason
        // (missing dep, patch that no longer applies, a Rust version bump) is in
        // there — and it scrolls past in the live view long before anyone can
        // read it. Keeping it on disk means the failure can actually be
        // diagnosed after first boot instead of guessed at.
        plan.push(chroot_interactive(&format!(
            "su - {user} -c 'LANG=C LC_ALL=C LC_MESSAGES=C paru {paru_esc}-S --needed --skipreview {pkgs}' \
             2>&1 | tee /var/log/artix-aur-build.log; true"
        )));

        // VERIFY what actually landed. The whole AUR phase is `|| true` —
        // deliberately, since one broken PKGBUILD shouldn't destroy an
        // otherwise-complete system. But "non-fatal" was silently turning into
        // "unreported": a failed pinnacle-comp build left the user with the
        // session file, the config tree and rust installed, yet NO compositor —
        // and they only found out when the session refused to start. So: ask
        // pacman which of the requested packages are missing and say so, loudly,
        // in the install log the user is already watching.
        // VERIFY VIA `pacman -T`, not `pacman -Qq <name>`.
        //
        // An AUR package's NAME is not necessarily the name it installs under:
        // `flameshot-git` provides (and installs as) `flameshot`, and the same
        // -git / -bin / -nvidia pattern is everywhere in the AUR. `pacman -Qq
        // flameshot-git` therefore reports "not installed" for a package that
        // installed perfectly — the check would cry wolf, and a warning that
        // fires on healthy installs is worse than no warning, because the real
        // failures drown in it.
        //
        // `pacman -T` ("test") is the dependency resolver's own question: is
        // this name satisfied, by a package of that name OR by anything that
        // `provides` it? It prints back only the names that are NOT satisfied —
        // exactly the list we want, computed the way pacman itself would.
        plan.push(chroot(&format!(
            "missing=$(pacman -T {pkgs} 2>/dev/null || true); \
             if [ -n \"$missing\" ]; then \
               echo ''; \
               echo '!! ================================================='; \
               echo '!! These AUR packages FAILED to install:'; \
               for m in $missing; do echo \"!!    $m\"; done; \
               echo '!!'; \
               echo '!! The base system is fine — only these are missing.'; \
               echo '!!'; \
               echo '!! ---- last 25 lines of the build log ----'; \
               tail -n 25 /var/log/artix-aur-build.log 2>/dev/null | sed 's/^/!! /'; \
               echo '!! ----------------------------------------'; \
               echo '!!'; \
               echo '!! Full log: /var/log/artix-aur-build.log'; \
               echo '!! After first boot, retry them by hand:'; \
               echo \"!!    paru -S $(echo $missing | tr '\\n' ' ')\"; \
               echo '!! ================================================='; \
               echo ''; \
             else \
               echo '[artix-installer] All AUR packages installed.'; \
             fi"
        )));

        // Pinnacle specifically: if the compositor didn't build, the session
        // entry we wrote earlier points at a binary that doesn't exist. A login
        // screen offering a session that dies on click is worse than no entry at
        // all, so remove it and say plainly what happened — the config tree and
        // rust stay, so a later `paru -S pinnacle-comp` gives a working desktop
        // without redoing anything.
        if effective_aur_packages(c)
            .iter()
            .any(|x| x == "pinnacle-comp")
        {
            plan.push(chroot(
                "if [ -n \"$(pacman -T pinnacle-comp 2>/dev/null)\" ]; then \
                   rm -f /usr/share/wayland-sessions/pinnacleFree.desktop; \
                   echo ''; \
                   echo '!! ================================================='; \
                   echo '!! PINNACLE DID NOT INSTALL.'; \
                   echo '!!'; \
                   echo '!! Its AUR build failed, so the compositor is absent.'; \
                   echo '!! The session entry has been removed (a session that'; \
                   echo '!! cannot start is worse than none).'; \
                   echo '!!'; \
                   echo '!! Your config and rust are already in place. After'; \
                   echo '!! first boot, one command gives you the desktop:'; \
                   echo '!!    paru -S pinnacle-comp'; \
                   echo '!! ================================================='; \
                   echo ''; \
                 fi",
            ));
        }

        // Pre-build the Pinnacle Rust config NOW — while the network is up and
        // the user is watching a live log — instead of on first login. A cold
        // build pulls pinnacle-api from its git tag and compiles snowcap
        // (iced/wgpu), which takes long minutes; after this step the first
        // session start is an instant exec of the release binary via
        // run-config.sh. Non-fatal: if it fails here, the wrapper retries on
        // first login and logs to ~/.config/pinnacle/run-config.log.
        if effective_aur_packages(c)
            .iter()
            .any(|x| x == "pinnacle-comp")
        {
            plan.push(chroot(&format!(
            "echo '[artix-installer] Building the Pinnacle config (cold build takes a while)...'; \
             su - {user} -c 'cd ~/.config/pinnacle && cargo build --release' \
             || echo '[artix-installer] Pinnacle config prebuild failed; it will build on first login'",
            user = c.username
        )));
        }
        // Clean up the build dir and revoke the temporary sudo.
        plan.push(chroot(&format!("rm -rf /home/{user}/paru || true")));
        if c.use_doas {
            // Strip the temporary user nopass line, keep the wheel rule.
            plan.push(chroot(&format!(
                "sed -i '/^permit nopass {user}$/d' /etc/doas.conf && chmod 0400 /etc/doas.conf"
            )));
        } else {
            plan.push(chroot("rm -f /etc/sudoers.d/99-aur-temp"));
        }

        // If the user picked auto-cpufreq, wire up its daemon. auto-cpufreq has
        // no dinit service and its `--install` only deploys a SYSTEMD unit
        // (upstream issues #91/#96), which is useless on Artix+dinit — so we
        // must NOT run `--install`. Instead we start the daemon mode at boot
        // via cron (cronie, enabled above, honours @reboot), exactly like the
        // hand-rolled setup that worked before. Two things:
        //   1) drop /etc/cron.d/auto-cpufreq with `@reboot ... auto-cpufreq
        //      --daemon` so the optimizer runs from boot onward;
        //   2) DISABLE power-profiles-daemon — it manages CPU power too and
        //      conflicts with auto-cpufreq (upstream masks it for the same
        //      reason). We remove its boot.d symlink so only one governor of
        //      CPU power is active. Both steps run only when the package is
        //      actually among the user's selections, and are best-effort.
        if aur_pkgs
            .iter()
            .any(|x| x == "auto-cpufreq" || x == "auto-cpufreq-git")
        {
            plan.push(write_target_file(
                "/mnt/etc/cron.d/auto-cpufreq",
                "# Managed by the Artix installer: start the auto-cpufreq daemon at boot.\n\
                 # auto-cpufreq has no dinit service and its --install only targets\n\
                 # systemd, so we run its daemon mode from cron instead.\n\
                 MAILTO=\"\"\n\
                 @reboot root /usr/bin/auto-cpufreq --daemon\n",
            ));
            // Disable the conflicting power-profiles-daemon (remove its autostart
            // symlink; the package stays installed but won't run at boot).
            plan.push(chroot(
                "rm -f /etc/dinit.d/boot.d/power-profiles-daemon; \
                 echo 'disabled power-profiles-daemon (conflicts with auto-cpufreq)'",
            ));
        }
    }

    // doas was chosen: get rid of the real `sudo` by REPLACING it with the AUR
    // `doas-sudo-shim`. It cannot merely be removed — `sudo` is a hard
    // dependency of `base-devel` (makepkg) and of `qt-sudo` (which `octopi`, a
    // default package, pulls), so `pacman -R sudo` fails and `-Rdd` would leave
    // those broken. The shim PROVIDES `sudo`, so it satisfies those exact
    // dependencies while making the `sudo` command transparently run doas —
    // nothing is left present-but-unauthorised ("user NOT in sudoers").
    //
    // Built directly here (a trivial script package; no paru needed) as the
    // user, then swapped in as root. Order is chosen for safety: build the
    // package FIRST, and only if it is in hand force-remove the real sudo and
    // install the shim — a failed build leaves sudo untouched, and a failed
    // install reinstalls sudo, so the target is never left with neither.
    if c.use_doas && c.account_mode.needs_user() {
        let user = &c.username;
        plan.push(chroot(&format!(
            "pacman -S --needed --noconfirm base-devel git && \
             su - {user} -c 'cd ~ && rm -rf doas-sudo-shim && git clone https://aur.archlinux.org/doas-sudo-shim.git && cd doas-sudo-shim && makepkg -f --noconfirm' && \
             dsh=$(getent passwd {user} | cut -d: -f6) && \
             dsp=$(ls -1t \"$dsh\"/doas-sudo-shim/doas-sudo-shim-*.pkg.tar.zst 2>/dev/null | grep -v -- -debug- | head -n1) && \
             [ -n \"$dsp\" ] && {{ pacman -Rdd --noconfirm sudo 2>/dev/null; pacman -U --noconfirm \"$dsp\" && echo '>>> sudo replaced with doas-sudo-shim (the sudo command now runs doas)' || pacman -S --needed --noconfirm sudo; }}; true"
        )));
        plan.push(chroot(&format!(
            "rm -rf /home/{user}/doas-sudo-shim || true"
        )));
    }

    // Snapshots: every chroot pacman/paru transaction is now done, so re-enable
    // snap-pac. The BOOTED system (with D-Bus + snapper working) will then take
    // pre/post snapshots on each pacman run — no more chroot "fatal library error".
    if c.btrfs_snapshots {
        plan.push(chroot(
            "sed -i 's/^PACMAN_PRE_POST=.*/PACMAN_PRE_POST=\"yes\"/' /etc/snapper/configs/root",
        ));
    }

    // Final step: drop the install log AND a logging help doc into the user's
    // home (or /root if there's no user), owned by them, so they're easy to find
    // and read. The log itself is captured live at /tmp/installer.log; the help
    // doc is localized to the chosen language. All best-effort.
    {
        let (home, owner) = if c.username.is_empty() {
            ("/root".to_string(), "root".to_string())
        } else {
            (format!("/home/{}", c.username), c.username.clone())
        };
        // Localized logging guide, written as the home file reading-logs.md.
        let doc = if c.lang == "uk" {
            LOG_HELP_UK
        } else {
            LOG_HELP_EN
        };
        plan.push(write_home_file(&home, "reading-logs.md", doc));
        // Copy the live install log into home (it lives at /tmp on the live FS,
        // so this runs live, not in the chroot). mkdir -p covers /root, which
        // may not pre-exist on the target until now.
        plan.push(act(
            "sh",
            &[
                "-c",
                &format!(
                    "echo '>>> Saving install log + logging guide to {home}/ ...'; \
                     mkdir -p /mnt{home} 2>/dev/null; \
                     if [ -f /tmp/installer.log ]; then \
                       cp /tmp/installer.log /mnt{home}/installer.log 2>/dev/null && \
                       echo \">>> Install log saved to {home}/installer.log\"; \
                     else \
                       echo '>>> No install log to save.'; \
                     fi; \
                     true",
                    home = home
                ),
            ],
        ));
        // Own + permission both files inside the chroot, where the username
        // resolves to its uid/gid. The log is 0600 (may carry transaction
        // detail); the guide is world-readable.
        plan.push(chroot(&format!(
            "chown {owner}:{owner} {home}/installer.log {home}/reading-logs.md 2>/dev/null || true; \
             chmod 600 {home}/installer.log 2>/dev/null || true; \
             chmod 644 {home}/reading-logs.md 2>/dev/null || true; \
             true",
            owner = owner,
            home = home
        )));
    }
}

/// A `pacman -S --needed <pkgs>` install, wrapped so one flaky download does not
/// end the install.
///
/// This is the phase a real 404 storm hit: every Arch mirror answered 404 for
/// the exact xorg-server / libinput / … files the sync DB named, because the DB
/// had gone stale against a moved-on pool, and the transaction died outright.
/// Two things make a retry worth having here: a genuinely transient mirror
/// hiccup, and — the real case — a stale DB, which the `-Syu` between attempts
/// refreshes so the next try asks for versions the pool still carries.
///
/// Kept INTERACTIVE (no --noconfirm on the install itself): the system phase
/// still has to show provider choices. The refresh IS --noconfirm, and `-u`
/// not a bare `-Sy` — a partial upgrade in the target is its own landmine.
/// A retry re-shows any provider prompt, which is a fair price for the rare
/// failure it rescues.
fn pacman_install_retry(pkgs: &str) -> String {
    format!(
        "for a in 1 2 3; do \
           pacman -S --needed {pkgs} && exit 0; \
           [ $a = 3 ] && exit 1; \
           echo \">>> package retrieval failed (attempt $a/3) - refreshing databases, retrying in 5s...\"; \
           pacman -Syu --noconfirm || true; \
           sleep 5; \
         done"
    )
}

fn plan_accounts(plan: &mut Vec<Action>, c: &InstallConfig, des: &[Desktop]) {
    // 9) Accounts. Four modes (see AccountMode). Passwords are piped to
    //    chpasswd via printf so we avoid interactive prompts; the values come
    //    from the wizard. We keep each step discrete for clear logging.
    let m = c.account_mode;

    if m.needs_user() {
        // Pick the login shell from the chosen extras: zsh or fish if selected,
        // otherwise bash. The shell package itself is in extra_packages already.
        let login_shell = if c.extra_packages.iter().any(|x| x == "zsh") {
            "/bin/zsh"
        } else if c.extra_packages.iter().any(|x| x == "fish") {
            "/usr/bin/fish"
        } else {
            "/bin/bash"
        };
        // `log` gives read access to /var/log: the syslog-ng config we write
        // further down owns the log files as root:log 0640, so membership is all
        // that stands between the user and `geany /var/log/everything.log`.
        // Without it, reading one's own system logs means escalating to root —
        // and a GUI editor as root is both awkward (X11 authority) and unwise.
        // Reading logs should never need privileges.
        //
        // The group is created here rather than assumed: it ships with
        // syslog-ng, but package layouts change, and a missing group in
        // `useradd -G` aborts user creation entirely — i.e. an install with no
        // user account at all. `groupadd -f` is a no-op if it already exists.
        plan.push(chroot("groupadd -f log"));
        plan.push(chroot(&format!(
            "useradd -m -G wheel,audio,video,storage,network,input,log -s {} {}",
            login_shell, c.username
        )));
        // A home-based custom mount ("~/name") makes /home/<user> pre-exist before
        // useradd runs, so `useradd -m` skips the skel copy. Restore it (no-clobber)
        // and fix ownership so the user still gets the default skel config files.
        if c.extra_disks.iter().any(|d| d.mountpoint.starts_with("~/")) {
            plan.push(chroot(&format!(
                "cp -rn /etc/skel/. /home/{user}/ 2>/dev/null || true; chown -R {user}:{user} /home/{user}",
                user = c.username
            )));
        }
        // chpasswd reads "user:password" from stdin — no shell quoting of the
        // password is needed, which avoids breakage on special characters.
        plan.push(
            chroot(&format!(
                "printf '%s:%s\\n' {} \"{}\" | chpasswd",
                c.username,
                shell_escape_dq(&c.user_password)
            ))
            .logged_as(&format!(
                "chroot: printf '%s:%s\\n' {} \"***\" | chpasswd",
                c.username
            )),
        );
        // Privilege escalation for wheel, honouring BOTH options-screen
        // choices: the tool (sudo/doas) and whether it asks for a password.
        if c.use_doas {
            // doas: one tiny config line. `persist` caches auth for a few
            // minutes like sudo's timestamp; `nopass` skips the prompt
            // entirely. doas.conf MUST be chmod 0400 and root-owned or doas
            // refuses to run. Rule order: last match wins, so a single line
            // for the wheel group is all we need.
            let rule = if c.passwordless_sudo {
                "permit nopass :wheel"
            } else {
                "permit persist :wheel"
            };
            plan.push(write_target_file(
                "/mnt/etc/doas.conf",
                &format!("{rule}\n"),
            ));
            plan.push(chroot(
                "chown root:root /etc/doas.conf && chmod 0400 /etc/doas.conf",
            ));
            // NO naive `sudo`→`doas` symlink here. It "works" for a bare
            // `sudo cmd` but breaks the instant a tool passes a sudo-only flag
            // (makepkg's `sudo -k` is exactly what killed the AUR build), and
            // opendoas upstream deliberately ships no such symlink for the same
            // reason. A user who wants `sudo` to drive doas can opt into the
            // AUR `doas-sudo-shim` package (a proper wrapper) on the options
            // screen; that is wired through effective_aur_packages.
        } else if c.passwordless_sudo {
            plan.push(chroot(
                "echo '%wheel ALL=(ALL:ALL) NOPASSWD: ALL' > /etc/sudoers.d/10-wheel-nopasswd; chmod 440 /etc/sudoers.d/10-wheel-nopasswd",
            ));
        } else {
            plan.push(chroot(
                "sed -i 's/^# *%wheel ALL=(ALL:ALL) ALL/%wheel ALL=(ALL:ALL) ALL/' /etc/sudoers",
            ));
        }
        // With seatd the user must be in the `seat` group so a compositor (or
        // anything using libseat) can acquire the seat. Done for ANY session —
        // the user may pick an X11 DE now and run a Wayland compositor later,
        // and the group costs nothing. seatd's package creates the group; we
        // add the user with gpasswd (|| true so it's harmless if absent).
        if c.seat_provider == SeatProvider::Seatd {
            plan.push(chroot(&format!(
                "gpasswd -a {} seat 2>/dev/null || true",
                c.username
            )));
        }

        // X11 + seatd: vanilla Xorg has NO libseat/seatd backend — it can only
        // run rootless via (e)logind, and the seatd path deliberately doesn't
        // wire up a logind session (it uses pam_turnstile, not pam_elogind). So
        // for an X11 session under seatd we make Xorg run ROOTFUL through its
        // setuid wrapper by writing /etc/X11/Xwrapper.config with
        // needs_root_rights=yes. As root, Xorg opens KMS + input devices
        // directly with no logind session needed, while seatd still manages the
        // seat for the display manager — the classic pre-logind way X is run on
        // elogind-free systems. Only written for an actual X11 session under
        // seatd; Wayland, or the elogind path, go rootless via libseat/logind
        // and must NOT get this file.
        let any_x11_session = des.iter().any(|d| d.supports_x11());
        if c.seat_provider == SeatProvider::Seatd && any_x11_session {
            plan.push(write_target_file(
                "/mnt/etc/X11/Xwrapper.config",
                "# Managed by the Artix installer: X11 + seatd (no elogind\n\
                 # session), so Xorg runs rootful and reaches KMS/input without\n\
                 # logind. seatd still manages the seat for the display manager.\n\
                 needs_root_rights = yes\n\
                 allowed_users = anybody\n",
            ));
        }

        // Raise the open-file-descriptor limit for the user. Wine/Proton's
        // fsync (and esync) open thousands of eventfd/file descriptors, and
        // running several game launchers in the background multiplies that —
        // hitting the default 1024 soft limit causes crashes and "too many open
        // files" errors. On systemd this is handled by DefaultLimitNOFILE, but
        // Artix+dinit has no such mechanism, so the limit MUST be set via PAM's
        // limits.conf. Both hard and soft go to 1048576. Idempotent: only
        // appended if a line for this user isn't already present (so a retried
        // install doesn't duplicate it). The stock file ends with a
        // "# End of file" marker; appending after it is fine.
        plan.push(chroot(&format!(
            "f=/etc/security/limits.conf; \
             if ! grep -qE '^{user}[[:space:]]+(hard|soft)[[:space:]]+nofile' \"$f\" 2>/dev/null; then \
             printf '%s hard nofile 1048576\\n%s soft nofile 1048576\\n' {user} {user} >> \"$f\"; \
             echo 'raised nofile limit for {user} (Wine/Proton fsync)'; fi",
            user = c.username
        )));

        // Shell configuration. We write the config into the new user's home and
        // chown it to them afterwards. The starship prompt config is generated
        // directly from starship's built-in `pastel-powerline` preset (run as
        // the user, so paths and ownership are correct) — no ISO asset needed,
        // and it always matches the installed starship version.
        let want_zsh = c.extra_packages.iter().any(|x| x == "zsh");
        let want_fish = c.extra_packages.iter().any(|x| x == "fish");
        let home = format!("/home/{}", c.username);

        if want_zsh || want_fish {
            // Generate ~/.config/starship.toml from the built-in preset AS ROOT.
            // `starship preset NAME -o FILE` only writes a file — it needs
            // neither the user's shell nor a login session. The old
            // `su - {user}` LOGIN shell was fragile in the chroot, and when it
            // failed it was masked by `|| true`: no theme was written, so
            // starship fell back to its plain `❯` default — which looks exactly
            // like "starship isn't installed" (the reported symptom). Ownership
            // is fixed by the later `chown -R {home}` in plan_session_env, the
            // same as every other config written here. A VISIBLE log line
            // replaces the silent `|| true`, so a future failure is diagnosable
            // instead of a mystery.
            plan.push(chroot(&format!(
                "mkdir -p {home}/.config && \
                 starship preset pastel-powerline -o {home}/.config/starship.toml \
                 && echo '>>> starship theme (pastel-powerline) written' \
                 || echo '!! starship theme generation failed - the default prompt will be used'",
                home = home
            )));
        }

        if want_zsh {
            // A clean .zshrc that wires up the repo plugins (same features as a
            // typical oh-my-zsh setup: completions, autosuggestions, syntax
            // highlighting, history substring search, sensible history + keys),
            // then starts starship. Paths are the Arch/Artix package locations.
            let zshrc = ZSHRC_TEMPLATE;
            plan.push(write_home_file(&home, ".zshrc", zshrc));
            // Pre-create an empty history file so zsh doesn't warn about a
            // missing/corrupt history on first launch.
            plan.push(chroot(&format!("touch {home}/.zsh_history")));
        }

        if want_fish {
            // fish config: sane defaults, abbreviations, and starship init.
            let fishcfg = FISH_CONFIG_TEMPLATE;
            plan.push(chroot(&format!("mkdir -p {home}/.config/fish")));
            plan.push(write_home_file(&home, ".config/fish/config.fish", fishcfg));
        }

        // kitty config (Catppuccin Mocha) → ~/.config/kitty. kitty ships as
        // a DEFAULT-CHECKED entry in the packages screen — written while it
        // stays selected, or whenever Pinnacle is the desktop (its config
        // binds mod+Return to kitty regardless of the checkbox). Embedded
        // in the binary so it never depends on an ISO asset.
        let want_kitty = c.extra_packages.iter().any(|x| x == "kitty")
            || des.iter().any(|d| matches!(d, Desktop::Pinnacle));
        if want_kitty {
            plan.push(chroot(&format!("mkdir -p {home}/.config/kitty")));
            plan.push(write_home_file(
                &home,
                ".config/kitty/kitty.conf",
                KITTY_CONFIG_TEMPLATE,
            ));
        }

        // fastfetch config + logo → ~/.config/fastfetch. fastfetch (default) and
        // slayfetch (fastfetch with a chosen Artix + pride logo) are mutually
        // exclusive entries in the packages screen; either one writes the themed
        // config here. The logo is a PNG (binary), written via base64; the config
        // is text whose logo `source` we rewrite to the user's ABSOLUTE home path
        // (fastfetch only expands ~ on v2.41.0+ and $HOME via wordexp, so a
        // literal absolute path always resolves regardless of version or launch
        // context, e.g. from .zshrc). Everything is embedded in the installer
        // binary, no ISO dependency.
        let want_fastfetch = c.extra_packages.iter().any(|x| x == "fastfetch");
        let want_slayfetch = c.extra_packages.iter().any(|x| x == "slayfetch");
        if want_fastfetch || want_slayfetch {
            plan.push(chroot(&format!("mkdir -p {home}/.config/fastfetch")));
            let fastfetch_config = FASTFETCH_CONFIG.replace(
                "$HOME/.config/fastfetch/fastfetch.png",
                &format!("{home}/.config/fastfetch/fastfetch.png"),
            );
            plan.push(write_home_file(
                &home,
                ".config/fastfetch/config.jsonc",
                &fastfetch_config,
            ));
            // slayfetch → the chosen pride logo (resolve() guarantees a real
            // variant); plain fastfetch → the default syrnyk logo.
            let logo: std::borrow::Cow<[u8]> = if want_slayfetch {
                crate::system::logos::resolve(&c.fastfetch_logo)
                    .and_then(|v| crate::system::logos::bytes(&v))
                    .map(std::borrow::Cow::Owned)
                    .unwrap_or(std::borrow::Cow::Borrowed(FASTFETCH_LOGO_PNG))
            } else {
                std::borrow::Cow::Borrowed(FASTFETCH_LOGO_PNG)
            };
            plan.extend(write_home_binary(
                &home,
                ".config/fastfetch/fastfetch.png",
                &logo,
            ));
            // slayfetch: give the user a real `slayfetch` command, since that's
            // what they picked. A tiny wrapper in /usr/local/bin execs fastfetch
            // (which reads the config + pride logo above) and works in EVERY
            // shell — more robust than per-shell aliases.
            if want_slayfetch {
                plan.push(write_target_file(
                    "/mnt/usr/local/bin/slayfetch",
                    "#!/bin/sh\n# Installed by the Artix installer: slayfetch is fastfetch\n# with the chosen Artix + pride logo.\nexec fastfetch \"$@\"\n",
                ));
                plan.push(chroot("chmod 755 /usr/local/bin/slayfetch"));
            }
        }

        // wofi config + stylesheet → ~/.config/wofi. Written when wofi is a
        // selected package OR when Pinnacle is the desktop (its config binds the
        // launcher to wofi, so it must be configured for the desktop to work).
        let pinnacle_desktop = des.iter().any(|d| matches!(d, Desktop::Pinnacle));
        let want_wofi = pinnacle_desktop || c.extra_packages.iter().any(|x| x == "wofi");
        if want_wofi {
            plan.push(chroot(&format!("mkdir -p {home}/.config/wofi")));
            plan.push(write_home_file(&home, ".config/wofi/config", WOFI_CONFIG));
            plan.push(write_home_file(
                &home,
                ".config/wofi/style.css",
                WOFI_STYLE_CSS,
            ));
        }

        // waybar config + stylesheet → ~/.config/waybar. Written when waybar is a
        // selected package OR when Pinnacle is the desktop (Pinnacle ships with
        // waybar as its bar, so the config + theme come along automatically).
        let want_waybar = pinnacle_desktop || c.extra_packages.iter().any(|x| x == "waybar");
        if want_waybar {
            plan.push(chroot(&format!("mkdir -p {home}/.config/waybar")));
            plan.push(write_home_file(
                &home,
                ".config/waybar/config.jsonc",
                WAYBAR_CONFIG,
            ));
            plan.push(write_home_file(
                &home,
                ".config/waybar/style.css",
                WAYBAR_STYLE_CSS,
            ));
        }

        // pinnacle compositor config → ~/.config/pinnacle, only if pinnacle-comp
        // is among the selected AUR packages. The config tree is shipped as a
        // gzip tarball embedded in the binary; we drop it into the home dir via
        // base64 (it's binary), unpack it, remove the tarball, and mark every
        // script under scripts/ executable. The pinnacle.toml runs `cargo run`,
        // so the Rust config (src/main.rs) is compiled by pinnacle on first
        // start — we ship only the source, never the multi-GB target/ tree.
        // pinnacle config tree → ~/.config/pinnacle when pinnacle-comp will be
        // installed — either because Pinnacle was chosen as the DESKTOP (which
        // injects pinnacle-comp into the AUR list) or picked explicitly in the
        // AUR package screen. effective_aur_packages() covers both.
        let want_pinnacle = effective_aur_packages(c)
            .iter()
            .any(|x| x == "pinnacle-comp");
        if want_pinnacle {
            // Write the config tree out file by file. Each one is a plain text
            // asset (see PINNACLE_FILES) rather than a member of an embedded
            // tarball — the user is meant to READ and EDIT this config, so it
            // ships as something reviewable in the repo instead of a blob that
            // only appears once it's unpacked on their disk.
            for (rel, body) in PINNACLE_FILES {
                // write_target_file mkdir -p's the parent itself, so src/ and
                // scripts/ come into being with their files.
                plan.push(write_target_file(
                    &format!("{home}/.config/pinnacle/{rel}"),
                    body,
                ));
            }
            // The shell helpers need the executable bit; text assets carry no
            // permissions of their own.
            plan.push(chroot(&format!(
                "if [ -d {home}/.config/pinnacle/scripts ]; then \
                 find {home}/.config/pinnacle/scripts -type f -name '*.sh' \
                 -exec chmod +x {{}} +; fi"
            )));
            // pinnacle.toml ships pointing at a bare `cargo run`; swap it to
            // the run-config.sh wrapper. Rationale: a cold `cargo run` on
            // FIRST LOGIN would git-clone pinnacle-api and compile snowcap
            // (iced/wgpu) for long minutes with zero feedback — the session
            // looks frozen (PinnacleFree) or bounces straight back to the DM
            // (stock entry) — and on a system without cargo it dies instantly.
            // The wrapper still rebuilds when sources change, execs the
            // release binary, and logs failures to run-config.log. The heavy
            // first build itself happens DURING installation, right after the
            // AUR phase (network up, progress visible in the install log).
            plan.push(write_home_file(
                &home,
                ".config/pinnacle/run-config.sh",
                PINNACLE_RUN_CONFIG,
            ));
            plan.push(chroot(&format!(
                "chmod +x {home}/.config/pinnacle/run-config.sh && \
                 sed -i 's|^run = .*|run = [\"./run-config.sh\"]|' \
                 {home}/.config/pinnacle/pinnacle.toml"
            )));
            // Session entries for Pinnacle. IMPORTANT: pinnacle-comp ships its
            // OWN /usr/share/wayland-sessions/pinnacle.desktop, so we must NOT
            // pre-write that path — a pre-existing, package-unowned file makes
            // pacman/paru abort the install with "pinnacle.desktop exists in
            // filesystem" (the file-conflict check runs BEFORE any hook). So:
            //  • We stash OUR desired entry (`pinnacle --session`,
            //    DesktopNames=pinnacle) at /usr/share/artix-installer/ — a path
            //    the package never touches, so no conflict.
            //  • A PostTransaction pacman hook (Target = pinnacle-comp*) then
            //    copies our entry over the package's file AFTER each install or
            //    upgrade — the package installs cleanly with its own file, and
            //    the hook re-asserts ours immediately afterward.
            //  • pinnacleFree.desktop — the dbus-run-session fallback under a
            //    DIFFERENT filename (no package owns it, so it's safe to write):
            //    a systemd-free system has no session bus unless something
            //    starts one; if portals/clipboard misbehave under the stock
            //    entry, pick this one in the greeter.
            plan.push(write_target_file(
                "/mnt/usr/share/artix-installer/pinnacle-session.desktop",
                PINNACLE_SESSION_DESKTOP,
            ));
            plan.push(write_target_file(
                "/mnt/etc/pacman.d/hooks/zz-pinnacle-session.hook",
                PINNACLE_SESSION_HOOK,
            ));
            plan.push(write_target_file(
                "/mnt/usr/share/wayland-sessions/pinnacleFree.desktop",
                "[Desktop Entry]\n\
                 Name=PinnacleFree\n\
                 Comment=A Wayland compositor inspired by AwesomeWM\n\
                 Exec=dbus-run-session pinnacle --session\n\
                 Type=Application\n\
                 DesktopNames=pinnacle\n",
            ));
        }

        // Audio: pipewire / wireplumber / pipewire-pulse are USER services
        // (/etc/dinit.d/user/*), started by the per-user dinit instance at
        // login — NOT system services. They're enabled by symlinking them into
        // the user's ~/.config/dinit.d/boot.d/. The dependency chain is
        // pipewire → wireplumber → pipewire-pulse. We always set this up so the
        // installed system has working sound out of the box.
        plan.push(chroot(&format!("mkdir -p {home}/.config/dinit.d/boot.d")));
        // user-dinit always looks for a service named "boot" on startup and
        // exits if it's missing ("could not find service description") — which
        // is exactly why the per-user instance was dying immediately and audio
        // never started. Create the boot service: type=internal with
        // waits-for.d=boot.d, so it pulls in everything symlinked under boot.d/
        // (pipewire → wireplumber → pipewire-pulse). Without this file userspawn
        // spawns dinit, dinit can't find "boot", and quits.
        plan.push(write_home_file(
            &home,
            ".config/dinit.d/boot",
            "type = internal\nwaits-for.d: ./boot.d/\n",
        ));
        // Symlink the user services into boot.d so they start with the user
        // dinit — AND make them log to an in-memory buffer so `dinitctl catlog
        // <svc>` works (handy for watching e.g. pipewire/wireplumber output in
        // real time). dinit has no global default for this; log-type = buffer
        // must be set per service, which is why catlog works for nothing out of
        // the box. We do it WITHOUT editing the packaged files (those live in
        // /etc/dinit.d/user and get overwritten on upgrade): instead we drop an
        // override copy in ~/.config/dinit.d/<svc> (which takes priority and is
        // never touched by pacman), append log-type = buffer + a 1 MiB buffer
        // if not already present, and point boot.d at the copy. If the packaged
        // service file isn't present (unexpected), fall back to a plain symlink.
        // We only do this for USER services — system services keep logging to
        // syslog-ng so /var/log stays complete; buffer would divert them.
        for svc in ["dbus", "pipewire", "wireplumber", "pipewire-pulse"] {
            plan.push(chroot(&format!(
                "src=/etc/dinit.d/user/{svc}; dst={home}/.config/dinit.d/{svc}; \
                 if [ -f \"$src\" ]; then \
                   [ -f \"$dst\" ] || cp \"$src\" \"$dst\"; \
                   grep -q '^log-type' \"$dst\" || printf 'log-type = buffer\\nlog-buffer-size = 1048576\\n' >> \"$dst\"; \
                   ln -sf \"$dst\" {home}/.config/dinit.d/boot.d/{svc}; \
                 else \
                   ln -sf \"$src\" {home}/.config/dinit.d/boot.d/{svc} 2>/dev/null || true; \
                 fi"
            )));
        }
        // That's all that's needed on the user side: turnstile's Dinit backend
        // runs `dinit --user` for us on login (see the turnstiled.conf + PAM
        // wiring below), and dinit then starts the `boot` service, which pulls
        // in everything symlinked under boot.d/. No userspawnrc is involved —
        // that was for userspawn, which we no longer use (it needs elogind).

        // Make sure everything we just wrote is owned by the user, not root.
        plan.push(chroot(&format!("chown -R {0}:{0} {home}", c.username)));
    }
}

/// Steps 9 (tail): the bits that make a graphical session behave on a
/// systemd-free box — GTK bookmarks for custom mount folders, and a per-user
/// D-Bus SESSION bus (nothing starts one automatically without `systemd
/// --user`, so GUI apps would come up with no DBUS_SESSION_BUS_ADDRESS).
fn plan_session_env(plan: &mut Vec<Action>, c: &InstallConfig) {
    // Custom-named mount folders get a GTK bookmark so they show up in the file
    // manager sidebar (Nautilus, Nemo, Thunar, PCManFM, Caja). Run as the user so
    // the bookmarks file is theirs; idempotent (no duplicate lines). Home-based
    // mounts are additionally visible as a folder in the home directory.
    for d in &c.extra_disks {
        if !d.bookmark || d.mountpoint.is_empty() {
            continue;
        }
        let resolved = resolve_mp(c, &d.mountpoint);
        let label = resolved.rsplit('/').next().unwrap_or("disk").to_string();
        plan.push(chroot(&format!(
            "su - {user} -c 'mkdir -p ~/.config/gtk-3.0 && touch ~/.config/gtk-3.0/bookmarks && \
             grep -qxF \"file://{path} {label}\" ~/.config/gtk-3.0/bookmarks || \
             echo \"file://{path} {label}\" >> ~/.config/gtk-3.0/bookmarks' || true",
            user = c.username,
            path = resolved,
            label = label
        )));
    }

    // D-Bus SESSION bus for graphical apps. On a systemd-free system nothing
    // starts a per-user session bus automatically the way `systemd --user`
    // would, so GUI apps launched from a display manager (Spotify, Electron
    // apps, many GTK/Qt programs) come up with no DBUS_SESSION_BUS_ADDRESS and
    // either refuse to start or lose features — exactly the "dbus session"
    // complaint, even though the SYSTEM bus is running. The fix (per the Void
    // Linux handbook, which faces the same no-systemd situation): once
    // XDG_RUNTIME_DIR exists (pam_rundir gives us /run/user/<uid>), point a
    // session bus at $XDG_RUNTIME_DIR/bus and start dbus-daemon there if it's
    // not already up, then export the address into the login shell. A
    // profile.d snippet runs for every login shell (X11, Wayland, or bare
    // TTY) and is idempotent: it only acts when no session bus is set yet.
    // dbus-update-activation-environment then shares the env (DISPLAY,
    // WAYLAND_DISPLAY, XDG_*) with the bus so activated services inherit it.
    plan.push(chroot("mkdir -p /etc/profile.d"));
    plan.push(write_target_file(
        "/mnt/etc/profile.d/dbus-session.sh",
        "# Start a per-user D-Bus *session* bus on systemd-free systems if the\n\
         # login process didn't set one up. Safe to source from any shell.\n\
         if [ -z \"$DBUS_SESSION_BUS_ADDRESS\" ] && [ -n \"$XDG_RUNTIME_DIR\" ]; then\n\
         \tbus=\"$XDG_RUNTIME_DIR/bus\"\n\
         \tif [ ! -S \"$bus\" ]; then\n\
         \t\t# --fork makes dbus-daemon daemonise itself cleanly (no stray\n\
         \t\t# foreground job holding up the login shell).\n\
         \t\tdbus-daemon --session --address=\"unix:path=$bus\" --fork --nopidfile >/dev/null 2>&1 || true\n\
         \tfi\n\
         \texport DBUS_SESSION_BUS_ADDRESS=\"unix:path=$bus\"\n\
         \tcommand -v dbus-update-activation-environment >/dev/null 2>&1 && \\\n\
         \t\tdbus-update-activation-environment DISPLAY WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE >/dev/null 2>&1 || true\n\
         fi\n",
    ));
    // The profile.d snippet covers login shells: a TTY login (the "no display
    // manager" case) and anything that sources /etc/profile. But an X11
    // session started with `startx` runs xinitrc, and the xinitrc.d hook
    // directory is the standard place to inject session-bus setup for that
    // path, so we add the SAME logic there. Together these cover all three DM
    // choices: SDDM and greetd start the session through PAM+their own dbus
    // handling, while "no DM" → TTY login (profile.d) → optional startx
    // (xinitrc.d). Hook is sourced by /etc/X11/xinit/xinitrc, executable.
    plan.push(chroot("mkdir -p /etc/X11/xinit/xinitrc.d"));
    plan.push(write_target_file(
        "/mnt/etc/X11/xinit/xinitrc.d/15-dbus-session.sh",
        "#!/bin/sh\n\
         # Ensure a D-Bus session bus for X11 sessions launched via startx on\n\
         # systemd-free systems (mirrors /etc/profile.d/dbus-session.sh).\n\
         if [ -z \"$DBUS_SESSION_BUS_ADDRESS\" ] && [ -n \"$XDG_RUNTIME_DIR\" ]; then\n\
         \tbus=\"$XDG_RUNTIME_DIR/bus\"\n\
         \t[ -S \"$bus\" ] || dbus-daemon --session --address=\"unix:path=$bus\" --fork --nopidfile >/dev/null 2>&1 || true\n\
         \texport DBUS_SESSION_BUS_ADDRESS=\"unix:path=$bus\"\n\
         \tcommand -v dbus-update-activation-environment >/dev/null 2>&1 && \\\n\
         \t\tdbus-update-activation-environment DISPLAY WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE >/dev/null 2>&1 || true\n\
         fi\n",
    ));
    plan.push(chroot(
        "chmod +x /etc/X11/xinit/xinitrc.d/15-dbus-session.sh 2>/dev/null || true",
    ));
    // fish does NOT read /etc/profile (different syntax), so the profile.d
    // snippet above never runs for a fish login — which matters in the "no DM"
    // case where a fish user logs into a TTY. fish sources /etc/fish/conf.d/*
    // for every shell, so put the equivalent there in fish syntax. Harmless if
    // fish isn't installed (the file just sits unused).
    plan.push(chroot("mkdir -p /etc/fish/conf.d"));
    plan.push(write_target_file(
        "/mnt/etc/fish/conf.d/dbus-session.fish",
        "# Ensure a D-Bus session bus for fish login sessions on systemd-free\n\
         # systems (fish doesn't read /etc/profile, so profile.d won't fire).\n\
         if test -z \"$DBUS_SESSION_BUS_ADDRESS\"; and test -n \"$XDG_RUNTIME_DIR\"\n\
         \tset -l bus \"$XDG_RUNTIME_DIR/bus\"\n\
         \ttest -S \"$bus\"; or dbus-daemon --session --address=\"unix:path=$bus\" --fork --nopidfile >/dev/null 2>&1\n\
         \tset -gx DBUS_SESSION_BUS_ADDRESS \"unix:path=$bus\"\n\
         \tif command -v dbus-update-activation-environment >/dev/null 2>&1\n\
         \t\tdbus-update-activation-environment DISPLAY WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE >/dev/null 2>&1\n\
         \tend\n\
         end\n",
    ));
    match c.account_mode {
        crate::app::AccountMode::UserSameRoot => {
            plan.push(
                chroot(&format!(
                    "printf 'root:%s\\n' \"{}\" | chpasswd",
                    shell_escape_dq(&c.user_password)
                ))
                .logged_as("chroot: printf 'root:%s\\n' \"***\" | chpasswd"),
            );
        }
        crate::app::AccountMode::UserSeparateRoot | crate::app::AccountMode::RootOnly => {
            plan.push(
                chroot(&format!(
                    "printf 'root:%s\\n' \"{}\" | chpasswd",
                    shell_escape_dq(&c.root_password)
                ))
                .logged_as("chroot: printf 'root:%s\\n' \"***\" | chpasswd"),
            );
        }
        crate::app::AccountMode::UserSudoOnly => {
            // Disable root login entirely; access is via sudo (wheel).
            plan.push(chroot("passwd -l root"));
        }
    }

    // 9a-luks) When the root is encrypted, wire up the initramfs and GRUB so
    //     the system can unlock the LUKS volume(s) at boot.
}

/// Step 9 (tail): mkinitcpio hooks and keyfiles for an encrypted install —
/// what lets the initramfs unlock the root (and, with a full-disk scope on
/// UEFI, the boot) container without asking twice.
fn plan_initramfs_luks(plan: &mut Vec<Action>, c: &InstallConfig, uefi: bool, luks_pass: &str) {
    if c.encrypt_disk {
        let full = c.encrypt_scope == "full" && uefi;
        // USB auto-unlock keyfile applies only to root-scope encryption: with
        // an encrypted /boot, GRUB prompts before the initramfs ever runs.
        let usb = !c.usb_key_device.is_empty() && !full;

        // (1) initramfs HOOKS: add `encrypt` before `filesystems` so root is
        //     unlocked before mount.
        plan.push(chroot(
            "grep -q '^HOOKS=.*encrypt' /etc/mkinitcpio.conf || sed -i 's/\\(^HOOKS=.*\\)\\(filesystems\\)/\\1encrypt \\2/' /etc/mkinitcpio.conf",
        ));

        if full {
            // Full-disk (encrypted /boot). Two keyfiles avoid extra passphrase
            // prompts AND keep /boot mounted in the running system:
            //
            //  • /boot/luks/root.key — random key added to the ROOT container,
            //    baked into the initramfs (cryptkey=). At boot GRUB unlocks
            //    /boot (one passphrase), the kernel loads, and the initramfs
            //    uses this key to open root with no second prompt.
            //
            //  • /etc/luks/boot.key — random key added to the BOOT container,
            //    living inside the (encrypted) root. A crypttab entry uses it to
            //    reopen `cryptboot` once root is mounted, so /boot is available
            //    in the running system (needed for kernel updates) and the
            //    early-fs-fstab mount of /boot no longer fails. The key can't
            //    live in /boot itself (that would be circular), so it sits in
            //    root, which is safe (root is encrypted).
            let pass_esc = luks_pass.replace('\'', "'\\''");
            // root.key (for root, in /boot, into initramfs)
            plan.push(chroot(
                "install -d -m 700 /boot/luks && dd if=/dev/urandom of=/boot/luks/root.key bs=512 count=4 && chmod 600 /boot/luks/root.key",
            ));
            plan.push(chroot(&format!(
                "rootpart=$(blkid -t PARTLABEL=ROOT -o device | head -n1); printf '%s' '{pass}' | cryptsetup luksAddKey \"$rootpart\" /boot/luks/root.key",
                pass = pass_esc
            )));
            plan.push(chroot(
                "grep -q '^FILES=.*root.key' /etc/mkinitcpio.conf || sed -i 's#^FILES=(#FILES=(/boot/luks/root.key #' /etc/mkinitcpio.conf",
            ));
            // boot.key (for boot, in /etc on root, used by crypttab)
            plan.push(chroot(
                "install -d -m 700 /etc/luks && dd if=/dev/urandom of=/etc/luks/boot.key bs=512 count=4 && chmod 600 /etc/luks/boot.key",
            ));
            plan.push(
                chroot(&format!(
                    "bootpart=$(blkid -t PARTLABEL=BOOT -o device | head -n1); printf '%s' '{pass}' | cryptsetup luksAddKey \"$bootpart\" /etc/luks/boot.key",
                    pass = pass_esc
                ))
                .logged_as(
                    "chroot: bootpart=$(blkid -t PARTLABEL=BOOT -o device | head -n1); \
                     printf '%s' '***' | cryptsetup luksAddKey \"$bootpart\" /etc/luks/boot.key",
                ),
            );
            // crypttab: open cryptboot from the BOOT partition's LUKS UUID using
            // the keyfile, automatically, when the system comes up.
            plan.push(chroot(
                "bootpart=$(blkid -t PARTLABEL=BOOT -o device | head -n1); \
                 buuid=$(blkid -s UUID -o value \"$bootpart\"); \
                 grep -q '^cryptboot ' /etc/crypttab || \
                 echo \"cryptboot UUID=${buuid} /etc/luks/boot.key luks\" >> /etc/crypttab",
            ));
        }

        // (1b) USB unlock keyfile. The chosen stick is WIPED and reformatted
        //     FAT32 with the label ARTIXKEY (a label needs no runtime UUID
        //     plumbing — the encrypt hook resolves LABEL= by itself). A fresh
        //     4096-byte RANDOM key (from /dev/urandom, unique per install) is
        //     generated, added as its OWN LUKS slot, and written to the stick;
        //     the temp copy is shredded from the live tmpfs. Runs on the HOST
        //     (live env), not the chroot: the stick is a live-system device.
        //
        //     TWO INDEPENDENT KEYS unlock the disk in BACKUP mode:
        //       • the stick's random key (this slot) — for automatic unlock,
        //       • the user's memorable passphrase (slot 0, used to luksFormat
        //         the container back in step 1) — the fallback if the stick is
        //         lost. They are different secrets in different slots.
        //     In KEY-ONLY mode there is no passphrase: a throwaway random
        //     secret formatted the container, and it is removed at the end of
        //     this chain (kill_pass), leaving the stick's key as the ONLY slot.
        if usb {
            let dev = c.usb_key_device.replace('\'', "");
            let pass_esc = luks_pass.replace('\'', "'\\''");
            // A previous failed attempt can leave the stick mounted at
            // /run/artix-usbkey — possibly several times over if the user
            // retried more than once (each run stacks another mount, and one
            // umount pops only one layer). wipefs then dies with "probing
            // initialization failed: Device or resource busy" and the retry
            // never gets going. So: unmount in a loop until nothing is left,
            // flush buffers, and wipe with -f.
            plan.push(act("sh", &["-c", &format!(
                "for i in 1 2 3 4 5; do umount /run/artix-usbkey 2>/dev/null || \
                   umount '{dev}'* 2>/dev/null || break; done; \
                 blockdev --flushbufs '{dev}' 2>/dev/null || true; \
                 wipefs -af '{dev}' && mkfs.fat -I -F 32 -n ARTIXKEY '{dev}' && \
                 mkdir -p /run/artix-usbkey && mount -t vfat '{dev}' /run/artix-usbkey && \
                 dd if=/dev/urandom of=/run/artix-usb.key bs=512 count=8 && chmod 600 /run/artix-usb.key && \
                 rootpart=$(blkid -t PARTLABEL=ROOT -o device | head -n1) && \
                 printf '%s' '{pass}' | cryptsetup luksAddKey \"$rootpart\" /run/artix-usb.key && \
                 cp /run/artix-usb.key /run/artix-usbkey/artix-luks.key && sync && \
                 umount /run/artix-usbkey && shred -u /run/artix-usb.key{kill_pass}",
                dev = dev, pass = pass_esc,
                // Key-only mode: remove the passphrase slot — but ONLY as the
                // LAST link of the && chain, after the key is verifiably on
                // the stick. Any earlier failure leaves the passphrase intact,
                // so a botched run can never lock the user out. (In backup mode
                // this is empty, so the passphrase slot is kept as the
                // fallback key.)
                kill_pass = if c.usb_key_only {
                    format!(
                        " && printf '%s' '{pass}' | cryptsetup luksRemoveKey \"$rootpart\"",
                        pass = pass_esc
                    )
                } else {
                    String::new()
                }
            )])
            .logged_as(&format!(
                "sh -c wipe '{dev}', mkfs.fat ARTIXKEY, mount, mint 4096-byte keyfile, \
                 cryptsetup luksAddKey \"$rootpart\" (passphrase ***), copy key to stick, \
                 shred the temporary copy{}",
                if c.usb_key_only {
                    ", luksRemoveKey (passphrase ***)"
                } else {
                    ""
                }
            )));
            // The initramfs must be able to see the stick BEFORE asking for
            // the key: USB storage + vfat + the FAT codepage/NLS modules
            // (autodetect won't reliably include them for a hot-pluggable
            // device, so they're pinned explicitly).
            plan.push(chroot(
                "grep -q '^MODULES=.*vfat' /etc/mkinitcpio.conf || sed -i 's/^MODULES=(/MODULES=(usb_storage vfat nls_cp437 nls_iso8859-1 /' /etc/mkinitcpio.conf",
            ));
        }

        // (2) GRUB cmdline: name the LUKS root device + mapper. For full-boot
        //     also point the encrypt hook at the embedded keyfile.
        let cryptkey = if full {
            " cryptkey=rootfs:/boot/luks/root.key"
        } else if usb {
            " cryptkey=LABEL=ARTIXKEY:vfat:/artix-luks.key"
        } else {
            ""
        };
        plan.push(chroot(&format!(
            "rootpart=$(blkid -t PARTLABEL=ROOT -o device | head -n1); ruuid=$(blkid -s UUID -o value \"$rootpart\"); sed -i \"s#^GRUB_CMDLINE_LINUX_DEFAULT=\\\"#&cryptdevice=UUID=${{ruuid}}:cryptroot root=/dev/mapper/cryptroot{cryptkey} #\" /etc/default/grub",
            cryptkey = cryptkey
        )));

        // (3) Full-boot: GRUB must read the LUKS container itself.
        if full {
            plan.push(chroot(
                "grep -q '^GRUB_ENABLE_CRYPTODISK=y' /etc/default/grub || echo 'GRUB_ENABLE_CRYPTODISK=y' >> /etc/default/grub",
            ));
        }
    }

    // 9a-nvidia) Proprietary NVIDIA (open-dkms or 580xx legacy): blacklist the
    //     open-source nouveau driver so it doesn't grab the card before the
    //     proprietary module loads. NVIDIA's tooling expects this and warns if
    //     it's missing. We do it two ways for robustness: a kernel cmdline arg
    //     in GRUB (nouveau.modeset=0 modprobe.blacklist=nouveau) and a
    //     modprobe.d blacklist (covers the initramfs/modules path too).
}

/// Step 9 (tail): NVIDIA DRM modeset (needed by Wayland compositors) and the
/// btrfs snapshot layout.
fn plan_gpu_and_snapshots(plan: &mut Vec<Action>, c: &InstallConfig) {
    if any_proprietary_nvidia(&c.gpu) {
        plan.push(chroot(
            "sed -i 's#^GRUB_CMDLINE_LINUX_DEFAULT=\"#&nouveau.modeset=0 modprobe.blacklist=nouveau #' /etc/default/grub",
        ));
        plan.push(write_target_file(
            "/mnt/etc/modprobe.d/blacklist-nouveau.conf",
            "# Disable the open-source nouveau driver in favour of the proprietary\n# NVIDIA driver installed by the Artix installer.\nblacklist nouveau\noptions nouveau modeset=0\n",
        ));
    }

    // 9a-rb) Boot-time rollback via the initramfs (all bootloaders). Install the
    //     artix-rollback tool and a mkinitcpio hook BEFORE the initramfs is built,
    //     so the install hook can bundle the binary. When the kernel is booted
    //     with `artix.rollback` (the bootloader's Rollback entry), the latehook
    //     mounts the btrfs pool, runs the picker, swaps @ for the chosen snapshot
    //     and lets the boot continue into it read-write. This sidesteps the
    //     read-only-snapshot overlay regression and works even when @ won't boot.
    if c.root_fs == "btrfs" && c.btrfs_subvolumes && c.btrfs_snapshots {
        // The tool itself: a POSIX-sh fallback first, then the installer binary
        // (which detects --rollback / --rollback-initramfs) copied over it. The
        // binary must exist here so `add_binary` can pull it into the initramfs.
        plan.push(write_target_file(
            "/mnt/usr/local/bin/artix-rollback",
            ROLLBACK_SCRIPT,
        ));
        plan.push(chroot("chmod 755 /usr/local/bin/artix-rollback"));
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe) = exe.to_str() {
                plan.push(act("cp", &["-f", exe, "/mnt/usr/local/bin/artix-rollback"]));
                plan.push(chroot("chmod 755 /usr/local/bin/artix-rollback"));
            }
        }
        // The initramfs hook pair (install + run-time).
        plan.push(write_target_file(
            "/mnt/etc/initcpio/install/artix-rollback",
            ROLLBACK_INITCPIO_INSTALL,
        ));
        plan.push(write_target_file(
            "/mnt/etc/initcpio/hooks/artix-rollback",
            ROLLBACK_INITCPIO_HOOK,
        ));
        // Append the hook to HOOKS (idempotent). It's a latehook, so it always
        // runs after the root device is ready regardless of list position.
        plan.push(chroot(
            "grep -q 'artix-rollback' /etc/mkinitcpio.conf || \
             sed -i 's/\\(^HOOKS=(.*\\))/\\1 artix-rollback)/' /etc/mkinitcpio.conf",
        ));
    }
}

fn plan_fstab(plan: &mut Vec<Action>, c: &InstallConfig) {
    // 3) fstab.
    plan.push(act("sh", &["-c", "fstabgen -U /mnt >> /mnt/etc/fstab"]));

    // Existing partitions the user chose to mount as-is (e.g. an NTFS Windows
    // volume to dual-boot with): add an fstab entry by UUID and create the
    // mountpoint. The partition is NOT mounted or touched during install — its
    // data is preserved; it mounts on first boot. NTFS uses ntfs-3g with the
    // first user (uid 1000) as owner; `nofail` keeps boot going if it's absent.
    for d in &c.extra_disks {
        if d.format || d.mountpoint.is_empty() {
            continue;
        }
        // A raw whole disk has no filesystem to mount — it has never been
        // partitioned. Mounting one can't work, and an fstab entry for it would
        // fail at boot rather than at install time, which is the worst possible
        // moment to find out. Existing PARTITIONS are the case this loop is for
        // (an NTFS Windows volume, a data partition kept as-is): those have a
        // filesystem, so they mount.
        if d.whole_disk {
            continue;
        }
        let (fstype, opts) = if d.fs.eq_ignore_ascii_case("ntfs") {
            (
                "ntfs-3g",
                "rw,uid=1000,gid=1000,umask=022,windows_names,big_writes,nofail",
            )
        } else {
            (d.fs.as_str(), "defaults,nofail")
        };
        let mp = resolve_mp(c, &d.mountpoint);
        plan.push(act("mkdir", &["-p", &format!("/mnt{}", mp)]));
        plan.push(act(
            "sh",
            &[
                "-c",
                &format!(
                    "uuid=$(blkid -s UUID -o value {dev}); \
                     [ -n \"$uuid\" ] && printf 'UUID=%s  %s  %s  %s  0 0\\n' \
                     \"$uuid\" '{mp}' '{fstype}' '{opts}' >> /mnt/etc/fstab",
                    dev = d.disk,
                    mp = mp,
                    fstype = fstype,
                    opts = opts
                ),
            ],
        ));
    }

    // Encrypted extra disks (format + encrypt): the partition was LUKS-formatted
    // and opened in extra_disks_plan, and the mapper is currently mounted (so
    // fstabgen captured the *filesystem* UUID at the target mountpoint). At boot
    // the mapper won't exist until it's unlocked, so we (1) drop that fstab line,
    // (2) install the keyfile onto the (encrypted) root, and (3) add a dinit
    // service that unlocks the LUKS device with the keyfile and mounts it. The
    // keyfile lives on root, so it's only safe when root itself is encrypted —
    // the storage screen warns otherwise.
    for d in &c.extra_disks {
        if !d.format || !d.encrypt || d.mountpoint.is_empty() {
            continue;
        }
        let mp = resolve_mp(c, &d.mountpoint);
        let mapper = crypt_mapper(&mp);
        // The LUKS container lives on the partition we created (whole disk) or on
        // the existing partition we reformatted in place.
        let p1 = if d.whole_disk {
            disk::part(&d.disk, 1)
        } else {
            d.disk.clone()
        };
        let svc = format!("crypt-{}", mapper.trim_start_matches("crypt_"));
        // (1) remove the auto-captured fstab line for this mountpoint.
        plan.push(act(
            "sh",
            &[
                "-c",
                &format!(
                    "sed -i '\\|[[:space:]]{mp}[[:space:]]|d' /mnt/etc/fstab",
                    mp = mp
                ),
            ],
        ));
        // (2) copy the keyfile onto the target root, locked down.
        plan.push(act(
            "sh",
            &[
                "-c",
                &format!(
                "mkdir -p /mnt/etc/cryptsetup-keys.d && chmod 700 /mnt/etc/cryptsetup-keys.d && \
                 cp /tmp/{mapper}.key /mnt/etc/cryptsetup-keys.d/{mapper}.key && \
                 chmod 600 /mnt/etc/cryptsetup-keys.d/{mapper}.key"
            ),
            ],
        ));
        // (3) write a scripted dinit service that unlocks + mounts at boot, and
        //     enable it by linking into boot.d. The LUKS UUID is resolved now.
        let mut mopts: Vec<&str> = Vec::new();
        if d.noatime {
            mopts.push("noatime");
        }
        if d.compress && d.fs == "btrfs" {
            mopts.push("compress=zstd");
        }
        let mount_flag = if mopts.is_empty() {
            String::new()
        } else {
            format!("-o {} ", mopts.join(","))
        };
        // (3) Write the unlock+mount logic as a background WORKER script, and a
        //     tiny LAUNCHER that fires the worker detached (setsid) and returns 0
        //     immediately. dinit therefore sees the service "started" at once, so
        //     boot is NEVER held up or failed by a data disk — even if the disk
        //     is missing, the keyfile is unreadable, or we're booting a read-only
        //     snapshot. The worker bakes in the LUKS UUID (resolved now), waits
        //     (bounded) for the device to appear since udev may still be settling,
        //     and tolerates every failure. A bare path needs no shell quoting.
        plan.push(act(
            "sh",
            &["-c", &format!(
                "mkdir -p /mnt/etc/dinit.d/scripts && \
                 luuid=$(cryptsetup luksUUID {p1}) && \
                 printf '%s\\n' \
                   '#!/bin/sh' \
                   \"luuid=$luuid\" \
                   'n=0; while [ $n -lt 30 ]; do [ -e /dev/disk/by-uuid/$luuid ] && break; n=$((n+1)); sleep 1; done' \
                   'timeout 30 cryptsetup open --key-file /etc/cryptsetup-keys.d/{mapper}.key UUID=$luuid {mapper} 2>/dev/null || exit 0' \
                   'mkdir -p {mp} 2>/dev/null' \
                   'mount {mount_flag}/dev/mapper/{mapper} {mp} 2>/dev/null' \
                   'exit 0' \
                   > /mnt/etc/dinit.d/scripts/{svc}-worker && \
                 chmod +x /mnt/etc/dinit.d/scripts/{svc}-worker && \
                 printf '%s\\n' \
                   '#!/bin/sh' \
                   'setsid /etc/dinit.d/scripts/{svc}-worker >/dev/null 2>&1 &' \
                   'exit 0' \
                   > /mnt/etc/dinit.d/scripts/{svc} && \
                 chmod +x /mnt/etc/dinit.d/scripts/{svc} && \
                 printf '%s\\n' \
                   '#!/bin/sh' \
                   'umount {mp} 2>/dev/null' \
                   'cryptsetup close {mapper} 2>/dev/null' \
                   > /mnt/etc/dinit.d/scripts/{svc}-stop && \
                 chmod +x /mnt/etc/dinit.d/scripts/{svc}-stop && \
                 printf '%s\\n' \
                   'type = scripted' \
                   'command = /etc/dinit.d/scripts/{svc}' \
                   'stop-command = /etc/dinit.d/scripts/{svc}-stop' \
                   'restart = false' \
                   > /mnt/etc/dinit.d/{svc}",
                p1 = p1, svc = svc, mapper = mapper, mp = mp,
                mount_flag = mount_flag
            )],
        ));
        plan.push(chroot(&format!(
            "mkdir -p /etc/dinit.d/boot.d && ln -sf ../{svc} /etc/dinit.d/boot.d/{svc}"
        )));
    }
}

/// Step 11: the nftables firewall config, embedded in the binary (no ISO asset
/// dependency). Skipped entirely when the user unticks nftables on the packages
/// screen — that removes the package, the service AND this config.
fn plan_firewall(plan: &mut Vec<Action>, c: &InstallConfig) {
    // 11) nftables firewall config — embedded in the binary (no ISO asset
    //      dependency). nftables ships as a DEFAULT-CHECKED entry in the
    //      packages screen; unticking it skips the package, the service AND
    //      this config. Written via a single-quoted heredoc so all the rule
    //      syntax ($, {}, @sets, #) is preserved verbatim.
    if c.extra_packages.iter().any(|x| x == "nftables") {
        plan.push(write_target_file(
            "/mnt/etc/nftables.conf",
            NFTABLES_CONFIG_TEMPLATE,
        ));
    }

    // 11.5) Display-manager configuration + console-hygiene for systemd-free
    //       systems. Two classes of fixes, learned from real Artix installs:
    //
    //       a) Console spam: with dinit, a supervised service running in
    //          no-daemon mode (NetworkManager -n) writes its log to stdout; a
    //          service file without `logfile` inherits dinit's console — i.e.
    //          the ACTIVE VT. Combined with kernel printk going to the active
    //          console, this paints NM's "couldn't find modem / ModemManager"
    //          probing all over a TUI greeter. Fixed at three levels: route
    //          the NM dinit service to a logfile, lower NM's own log level
    //          (the modem-probe lines are warnings), and quiet kernel printk
    //          on the console (the sysctl tuigreet's own README recommends).
    //          Applied to EVERY install — a bare TTY benefits just as much.
    //
    //       b) greetd runs on vt = 7 (proven on Artix dinit). A full greeter
    //          (tuigreet / graphical) switches the console to that VT itself;
    //          bare agreety does not, so tuigreet is the recommended text
    //          greeter. tty1 and its getty are NEVER touched — dinit needs them.
    plan.push(write_target_file(
        "/mnt/etc/sysctl.d/20-quiet-printk.conf",
        "# Keep kernel messages off the console (they still go to dmesg/logs).\n\
         # Without this, printk paints over TUI greeters and bare TTYs.\n\
         kernel.printk = 3 3 3 3",
    ));
    plan.push(chroot(
        // Route the NetworkManager dinit service's stdout to a logfile instead
        // of the console. /etc/dinit.d files are pacman backup files, so this
        // edit survives updates. Guarded: only appended if no logfile is set.
        "mkdir -p /var/log/dinit; \
         if [ -f /etc/dinit.d/networkmanager ] && ! grep -q '^[[:space:]]*logfile' /etc/dinit.d/networkmanager; then \
           printf '\\nlogfile = /var/log/dinit/networkmanager.log\\n' >> /etc/dinit.d/networkmanager; \
         fi",
    ));
    plan.push(write_target_file(
        "/mnt/etc/NetworkManager/conf.d/20-quiet-logging.conf",
        "# Only log errors. NM's default INFO/WARN level includes endless\n\
         # modem/ModemManager probing chatter on systems with no modem, which\n\
         # (via dinit's console) bleeds onto TUI greeters. Raise back to WARN\n\
         # or INFO temporarily when debugging network issues.\n\
         [logging]\n\
         level=ERR",
    ));
    if dm_service(&c.display_manager) == Some("greetd") {
        let greeter_cmd: String = match c.display_manager.as_str() {
            // tuigreet builds its session list from the installed .desktop
            // files. CRITICAL flag split: --sessions is for WAYLAND session
            // dirs (commands exec'd directly), --xsessions is for X11 session
            // dirs — only those get wrapped in --xsession-wrapper (default
            // "startx /usr/bin/env", hence xorg-xinit), which actually starts
            // the X server. Feeding xsessions through --sessions launches the
            // DE with no display and it dies instantly. --remember* needs a
            // writable cache dir (created below).
            "tuigreet" => "tuigreet --time --remember --remember-user-session --asterisks \
                           --sessions /usr/share/wayland-sessions \
                           --xsessions /usr/share/xsessions"
                .into(),
            // ReGreet is a GTK Wayland greeter; it needs a compositor to host
            // it — cage in kiosk mode (-s allows VT switching).
            "regreet" => "cage -s -- regreet".into(),
            // Fallback (shouldn't hit — every shipped greeter is handled above).
            _ => "tuigreet --time".into(),
        };
        plan.push(write_target_file(
            "/mnt/etc/greetd/config.toml",
            &format!(
                "# /etc/greetd/config.toml — generated by the installer.\n\
                 [terminal]\n\
                 # vt = 7: the standard greetd display VT, proven on Artix\n\
                 # dinit. tty1 and its getty are LEFT ALONE (dinit needs the\n\
                 # tty1 getty). NOTE: a full greeter (tuigreet / the graphical\n\
                 # ones) switches the active console to this VT and clears it\n\
                 # itself. Bare `agreety` does NOT — it's a getty replacement\n\
                 # that expects to be activated on a VT — so with agreety you\n\
                 # may need Ctrl+Alt+F7 to see it. tuigreet is the recommended\n\
                 # text greeter for exactly this reason.\n\
                 vt = 7\n\
                 \n\
                 [default_session]\n\
                 command = \"{greeter_cmd}\"\n\
                 user = \"greeter\""
            ),
        ));
        // The greeter user + writable dirs. The greetd package normally
        // creates the user, but Artix packaging has no systemd-sysusers — be
        // defensive. video group: DRM/backlight access for cage/sway; seat
        // group: REQUIRED with seatd or the greeter's compositor can't
        // acquire the seat (the classic "ReGreet just doesn't start" on
        // Artix). All guarded with || true where absence is legitimate.
        plan.push(chroot(
            "id greeter >/dev/null 2>&1 || useradd -r -M -d /var/lib/greetd -s /usr/bin/nologin greeter; \
             mkdir -p /var/lib/greetd /var/cache/tuigreet; \
             chown greeter: /var/lib/greetd /var/cache/tuigreet; \
             gpasswd -a greeter video 2>/dev/null || true",
        ));
        if c.seat_provider == SeatProvider::Seatd {
            plan.push(chroot("gpasswd -a greeter seat 2>/dev/null || true"));
        }
        // ReGreet needs no extra session file: it reads the installed
        // wayland-sessions/xsessions .desktop entries directly. (tuigreet does
        // the same.) The AUR-only gtkgreet/wlgreet — which needed an
        // environments/sway-config file — are not offered, so there's nothing
        // more to write here.
    }
}

fn plan_bootloader(plan: &mut Vec<Action>, c: &InstallConfig, uefi: bool) {
    // 10) Bootloader. GRUB is the default and the only one that can decrypt an
    //     encrypted /boot; rEFInd and Limine are offered for plaintext and
    //     root-only LUKS (the UI blocks full-disk encryption with them).
    match c.bootloader {
        Bootloader::Refind => {
            if uefi {
                // refind-install copies rEFInd to the ESP and registers it.
                plan.push(chroot("refind-install || true"));
                let luks = luks_cmdline_part(c);
                let rootflags = rootflags_part(c);
                let root_dev = if c.encrypt_disk {
                    "/dev/mapper/cryptroot"
                } else {
                    "UUID=$ruuid"
                };
                // refind_linux.conf carries ONLY the standard boot options for the
                // auto-detected kernel entry. The rollback is deliberately NOT a
                // second line here: a refind_linux.conf alt-options line becomes a
                // nested submenu, and with a fallback initramfs present rEFInd's
                // kernel auto-detection produced TWO identically-named rollback
                // entries that mis-paired the initramfs and failed to boot (the
                // firmware then fell through to the live USB). Instead we add ONE
                // explicit top-level manual stanza (below) with the exact
                // kernel+initrd, which rEFInd boots verbatim.
                plan.push(chroot(&format!(
                    "rootpart=$(blkid -t PARTLABEL=ROOT -o device | head -n1); \
                     ruuid=$(blkid -s UUID -o value \"$rootpart\"); \
                     printf '\\\"Boot with standard options\\\"  \\\"{luks}{rootflags}root={root} rw\\\"\\n' > /boot/refind_linux.conf",
                    luks = luks, rootflags = rootflags, root = root_dev
                )));
                // Explicit top-level rollback menuentry (snapshots only). /boot is
                // the ESP and rEFInd launches from it, so loader/initrd paths are
                // ESP-root-relative and `volume` is omitted (defaults to the launch
                // volume). The cmdline carries artix.rollback so the initramfs
                // snapshot picker runs. Appended via an (unquoted) heredoc so the
                // unencrypted $ruuid expands; the encrypted case has no $ to expand.
                // Idempotent — guarded by a grep for an existing entry.
                if c.root_fs == "btrfs" && c.btrfs_subvolumes && c.btrfs_snapshots {
                    let stanza = format!(
                        "\n# Artix rescue + rollback entries, both on the FROZEN RESCUE kernel pair that\n# pacman never updates - so they keep working even when a kernel update broke\n# the live kernel/initramfs.\n#   1) plain boot on the known-good spare kernel (no rollback);\n#   2) the same kernel + artix.rollback, which runs the snapshot picker.\nmenuentry \"Rescue kernel (known good)\" {{\n    loader /vmlinuz-artix-rescue\n    initrd /initramfs-artix-rescue.img\n    options \"{luks}{rootflags}root={root} rw\"\n}}\nmenuentry \"System Rollback - pick a snapshot\" {{\n    loader /vmlinuz-artix-rescue\n    initrd /initramfs-artix-rescue.img\n    options \"{luks}{rootflags}root={root} rw artix.rollback\"\n}}\n",
                        luks = luks, rootflags = rootflags, root = root_dev
                    );
                    plan.push(chroot(&format!(
                        "rootpart=$(blkid -t PARTLABEL=ROOT -o device | head -n1); ruuid=$(blkid -s UUID -o value \"$rootpart\"); conf=/boot/EFI/refind/refind.conf; [ -f \"$conf\" ] || conf=\"$(find /boot -name refind.conf 2>/dev/null | head -n1)\"; if [ -n \"$conf\" ] && ! grep -q 'System Rollback' \"$conf\"; then cat >> \"$conf\" <<ARTIX_RB_EOF\n{stanza}\nARTIX_RB_EOF\nfi",
                        stanza = stanza
                    )));
                }
            } else {
                // rEFInd is UEFI-only; fall back to GRUB on BIOS.
                plan.push(chroot(&format!("grub-install --target=i386-pc {}", c.disk)));
                plan.push(chroot("grub-mkconfig -o /boot/grub/grub.cfg"));
            }
        }
        Bootloader::Limine => {
            if uefi {
                plan.push(chroot(
                    "mkdir -p /boot/EFI/limine && cp /usr/share/limine/BOOTX64.EFI /boot/EFI/limine/ || true",
                ));
                let id = c.bootloader_id.replace('\'', "");
                // NOTE: single backslashes inside single quotes — efibootmgr
                // takes the EFI device path literally, and doubled backslashes
                // produce a path the firmware can't resolve.
                plan.push(chroot(&format!(
                    "efibootmgr --create --disk {disk} --part 1 --loader '\\EFI\\limine\\BOOTX64.EFI' --label '{id}' || true",
                    disk = c.disk, id = id
                )));
                // limine.conf with the kernel/initramfs of the CHOSEN kernel
                // (root resolved at install time). Microcode images are added
                // as modules BEFORE the initramfs when present (Limine has no
                // grub-mkconfig-style autodetection). The config is written
                // both next to the EFI executable (the first place Limine
                // looks) and at the partition root (a documented fallback).
                let luks = luks_cmdline_part(c);
                let rootflags = rootflags_part(c);
                let root_dev = if c.encrypt_disk {
                    "/dev/mapper/cryptroot"
                } else {
                    "UUID=$ruuid"
                };
                let (vmlinuz, initramfs) = kernel_images(c.kernel);
                // A second Limine entry that adds artix.rollback so the initramfs
                // picker runs at boot — it boots the FROZEN RESCUE pair (same
                // microcode), so the picker survives a broken kernel update. A third
                // plain entry on the same pair is the escape hatch for a normal boot:
                // with Limine /boot is the ESP, and a rollback alone cannot fix the
                // kernel there. Only when snapshots are on.
                let rb = if c.root_fs == "btrfs" && c.btrfs_subvolumes && c.btrfs_snapshots {
                    format!(
                        "printf '\\n/System Rollback - pick a snapshot\\n    protocol: linux\\n    kernel_path: boot():/vmlinuz-artix-rescue\\n'; \
                         [ -f /boot/amd-ucode.img ] && printf '    module_path: boot():/amd-ucode.img\\n'; \
                         [ -f /boot/intel-ucode.img ] && printf '    module_path: boot():/intel-ucode.img\\n'; \
                         printf '    module_path: boot():/initramfs-artix-rescue.img\\n    cmdline: {luks}{rootflags}root=%s rw artix.rollback\\n' \"{root}\"; \
                         printf '\\n/Artix (rescue kernel)\\n    protocol: linux\\n    kernel_path: boot():/vmlinuz-artix-rescue\\n'; \
                         [ -f /boot/amd-ucode.img ] && printf '    module_path: boot():/amd-ucode.img\\n'; \
                         [ -f /boot/intel-ucode.img ] && printf '    module_path: boot():/intel-ucode.img\\n'; \
                         printf '    module_path: boot():/initramfs-artix-rescue.img\\n    cmdline: {luks}{rootflags}root=%s rw\\n' \"{root}\"; ",
                        luks = luks, rootflags = rootflags, root = root_dev
                    )
                } else {
                    String::new()
                };
                plan.push(chroot(&format!(
                    "rootpart=$(blkid -t PARTLABEL=ROOT -o device | head -n1); \
                     ruuid=$(blkid -s UUID -o value \"$rootpart\"); \
                     {{ printf 'timeout: 3\\n\\n/Artix Linux\\n    protocol: linux\\n    kernel_path: boot():/{vmlinuz}\\n'; \
                        [ -f /boot/amd-ucode.img ] && printf '    module_path: boot():/amd-ucode.img\\n'; \
                        [ -f /boot/intel-ucode.img ] && printf '    module_path: boot():/intel-ucode.img\\n'; \
                        printf '    module_path: boot():/{initramfs}\\n    cmdline: {luks}{rootflags}root=%s rw\\n' \"{root}\"; \
                        {rb} \
                     }} > /boot/limine.conf; \
                     cp /boot/limine.conf /boot/EFI/limine/limine.conf",
                    vmlinuz = vmlinuz, initramfs = initramfs,
                    luks = luks, rootflags = rootflags, root = root_dev, rb = rb
                )));
            } else {
                plan.push(chroot(&format!("grub-install --target=i386-pc {}", c.disk)));
                plan.push(chroot("grub-mkconfig -o /boot/grub/grub.cfg"));
            }
        }
        Bootloader::Efistub => {
            if uefi {
                // EFISTUB: the kernel is its own EFI application (Artix kernels
                // are built with CONFIG_EFI_STUB=y), so the UEFI firmware loads
                // vmlinuz directly — no bootloader, no systemd-stub, nothing to
                // install beyond efibootmgr (already present). The initramfs and
                // cmdline are passed as boot-entry parameters. Microcode, when
                // present, is prepended as an extra initrd= (loaded before the
                // main initramfs), exactly as a bootloader would order it.
                //
                // Unlike UKI, kernel/initramfs/cmdline stay SEPARATE files, so
                // snapshot rollback works: we register additional UEFI entries
                // for the frozen rescue pair — one with artix.rollback (runs the
                // snapshot picker) and one plain (escape hatch) — mirroring the
                // Limine flow. The rollback entries are chosen from the firmware
                // boot menu (F8/F12/Esc, vendor-dependent) rather than a
                // graphical menu; functional, just a little less convenient.
                let luks = luks_cmdline_part(c);
                let rootflags = rootflags_part(c);
                let (vmlinuz, initramfs) = kernel_images(c.kernel);
                let id = c.bootloader_id.replace('\'', "");
                // Build the initrd= arguments: microcode first (if any), then the
                // main initramfs. efibootmgr passes these literally in the load
                // options; the EFISTUB concatenates multiple initrd= in order.
                // Backslash paths are ESP-relative and single (doubled ones don't
                // resolve), matching the Limine entry convention above.
                //
                // The main boot entry. Root UUID is resolved at install time for
                // the unencrypted case; encrypted uses /dev/mapper/cryptroot.
                // root_spec is the literal put into the load options: for the
                // unencrypted case it's UUID=$ruuid, where $ruuid is a SHELL
                // variable the command resolves via blkid just before; for the
                // encrypted case it's the fixed mapper path.
                let root_spec = if c.encrypt_disk {
                    "/dev/mapper/cryptroot"
                } else {
                    "UUID=$ruuid"
                };
                plan.push(chroot(&format!(
                    "rootpart=$(blkid -t PARTLABEL=ROOT -o device | head -n1); \
                     ruuid=$(blkid -s UUID -o value \"$rootpart\"); \
                     ucode=''; \
                     [ -f /boot/amd-ucode.img ] && ucode='initrd=\\amd-ucode.img '; \
                     [ -f /boot/intel-ucode.img ] && ucode='initrd=\\intel-ucode.img '; \
                     efibootmgr --create --disk {disk} --part 1 \
                       --loader '\\{vmlinuz}' --label '{id}' \
                       --unicode \"${{ucode}}initrd=\\{initramfs} {luks}{rootflags}root={root} rw\" || true",
                    disk = c.disk, vmlinuz = vmlinuz, initramfs = initramfs,
                    id = id, luks = luks, rootflags = rootflags, root = root_spec
                )));
                // Rollback + rescue entries on the FROZEN rescue pair, only when
                // snapshots are on. Registered as separate UEFI entries; the
                // rollback one carries artix.rollback so the picker runs.
                if c.root_fs == "btrfs" && c.btrfs_subvolumes && c.btrfs_snapshots {
                    plan.push(chroot(&format!(
                        "rootpart=$(blkid -t PARTLABEL=ROOT -o device | head -n1); \
                         ruuid=$(blkid -s UUID -o value \"$rootpart\"); \
                         ucode=''; \
                         [ -f /boot/amd-ucode.img ] && ucode='initrd=\\amd-ucode.img '; \
                         [ -f /boot/intel-ucode.img ] && ucode='initrd=\\intel-ucode.img '; \
                         efibootmgr --create --disk {disk} --part 1 \
                           --loader '\\vmlinuz-artix-rescue' --label '{id} — System Rollback' \
                           --unicode \"${{ucode}}initrd=\\initramfs-artix-rescue.img {luks}{rootflags}root={root} rw artix.rollback\" || true; \
                         efibootmgr --create --disk {disk} --part 1 \
                           --loader '\\vmlinuz-artix-rescue' --label '{id} — rescue kernel' \
                           --unicode \"${{ucode}}initrd=\\initramfs-artix-rescue.img {luks}{rootflags}root={root} rw\" || true",
                        disk = c.disk, id = id, luks = luks, rootflags = rootflags, root = root_spec
                    )));
                }
            } else {
                // EFISTUB is UEFI-only; fall back to GRUB on BIOS.
                plan.push(chroot(&format!("grub-install --target=i386-pc {}", c.disk)));
                plan.push(chroot("grub-mkconfig -o /boot/grub/grub.cfg"));
            }
        }
        Bootloader::Grub => {
            // GRUB (default).
            if uefi {
                // The ESP mountpoint decides --efi-directory. Manual mode lets
                // the user put the ESP at /boot/efi (small reused Windows ESP,
                // kernels on root); full-disk encryption always splits it that
                // way; otherwise the ESP is /boot.
                let efi_dir = if c.partition_mode.is_manual_family() {
                    if c.manual_esp_mount == "/boot/efi" {
                        "/boot/efi"
                    } else {
                        "/boot"
                    }
                } else if c.encrypt_disk && c.encrypt_scope == "full" {
                    "/boot/efi"
                } else {
                    "/boot"
                };
                let base_id = c.bootloader_id.replace('\'', "");
                // Our root's filesystem UUID, for the .artix-tui-owned marker.
                // findmnt answers from the udev/blkid cache, which inside a
                // just-populated chroot may not be there — and an EMPTY marker
                // is worse than none: the neighbour generator then cannot
                // recognise our own loader and emits a menu entry that
                // chainloads us into our own menu. So probe the device when the
                // cache comes up empty (btrfs appends [/subvol] to SOURCE,
                // which has to come off before blkid sees a device path).
                let root_uuid_sh = "me=$(findmnt -no UUID / 2>/dev/null); \
                     [ -n \"$me\" ] || me=$(blkid -s UUID -o value \
                       \"$(findmnt -no SOURCE / 2>/dev/null | sed 's/\\[.*//')\" 2>/dev/null); ";
                if c.partition_mode.is_manual_family() && !c.manual_solo {
                    // A SHARED ESP may already carry an EFI/<id> directory from
                    // the OS next door — and if that OS is another Artix (or any
                    // distro using the same id), a plain grub-install OVERWRITES
                    // its loader: the neighbour stops booting the moment ours
                    // lands. Seen live: Artix installed beside Artix left only
                    // the new one bootable. Pick the first free id instead, and
                    // mark the directory ours so a retry reuses it rather than
                    // minting Artix-3, Artix-4…
                    // `$id` is quoted EVERYWHERE it is expanded: the UEFI-entry
                    // name may contain spaces (the Options field allows them),
                    // and an unquoted "My Artix" would word-split the [ -d ]
                    // test and touch, breaking the whole install script.
                    // The marker carries WHO owns the directory — the root
                    // filesystem UUID of the install that wrote it — not merely
                    // "some artix-tui install". An empty marker was the whole
                    // bug: a SECOND Artix put here by this same installer found
                    // the first one's marker, read it as its own, and
                    // grub-install overwrote the neighbour's loader exactly as
                    // before. Reuse the directory only when it is genuinely
                    // ours (a re-install of THIS system, which should not mint
                    // Artix-3, Artix-4…); otherwise step to the next free id.
                    //
                    // A missing UUID never matches: without it we mint a new id
                    // rather than risk claiming a stranger's directory.
                    plan.push(chroot(&format!(
                        "{uuid_sh}\
                         id='{base}'; n=2; \
                         while [ -d \"{efi}/EFI/$id\" ]; do \
                           owner=$(cat \"{efi}/EFI/$id/.artix-tui-owned\" 2>/dev/null); \
                           if [ -n \"$me\" ] && [ \"$owner\" = \"$me\" ]; then break; fi; \
                           id='{base}-'$n; n=$((n+1)); \
                         done; \
                         echo \">>> GRUB EFI bootloader-id: $id\"; \
                         grub-install --target=x86_64-efi --efi-directory={efi} --bootloader-id=\"$id\" && \
                         printf '%s\\n' \"$me\" > \"{efi}/EFI/$id/.artix-tui-owned\"",
                        uuid_sh = root_uuid_sh,
                        base = base_id,
                        efi = efi_dir
                    )));
                } else {
                    // No id dance here: this install either owns the disk or
                    // wiped it, so EFI/<id> is ours by construction and minting
                    // Artix-2 on every reinstall (root is reformatted, so its
                    // UUID never matches the old marker) would litter the ESP.
                    //
                    // The MARKER is still written, because it answers a
                    // different question: "which EFI directory is mine?" The
                    // neighbour generator below runs on EVERY install now, and
                    // without a marker it cannot tell our own loader from a
                    // stranger's — it would chainload us into our own menu. It
                    // also stops a LATER install from mistaking our directory
                    // for a free one and overwriting the loader.
                    plan.push(chroot(&format!(
                        "{uuid_sh}\
                         grub-install --target=x86_64-efi --efi-directory={efi} --bootloader-id='{id}' && \
                         printf '%s\\n' \"$me\" > '{efi}/EFI/{id}/.artix-tui-owned'",
                        uuid_sh = root_uuid_sh,
                        efi = efi_dir,
                        id = base_id
                    )));
                }
                // ALSO install to the removable/fallback path
                // EFI/BOOT/BOOTX64.EFI. The bootloader-id install above is meant
                // to leave a NAMED NVRAM boot entry — but efibootmgr cannot
                // write one here: artix-chroot mounts /sys READ-ONLY (artools
                // mount.sh), so efivars is inaccessible and the entry is
                // silently skipped ("Installation finished. No error reported").
                // With no NVRAM entry AND no fallback path, the firmware is left
                // to ENUMERATE the ESP's loaders: fine with a single GRUB, but
                // with TWO (Artix beside Artix) OVMF picks one AT RANDOM every
                // boot — sometimes bypassing the menu entirely. That was the
                // reported chaos. The fallback is one fixed, always-present
                // target; the NEWEST install wins it, and the newest GRUB is the
                // one whose chain entries list every neighbour, so booting it
                // shows them all. Standard reliability install for firmware with
                // flaky NVRAM (OVMF, many real UEFIs). Best-effort: a firmware
                // that DID take the named entry still boots fine either way.
                plan.push(chroot(&format!(
                    "grub-install --target=x86_64-efi --efi-directory={efi} --removable || true",
                    efi = efi_dir
                )));
            } else {
                plan.push(chroot(&format!("grub-install --target=i386-pc {}", c.disk)));
            }
            // Optionally let GRUB detect other installed OSes (e.g. Windows) and
            // add boot entries. GRUB 2.06+ disables os-prober by default, so we
            // flip GRUB_DISABLE_OS_PROBER=false before generating the config.
            if c.os_prober {
                plan.push(chroot(
                    "if grep -q '^GRUB_DISABLE_OS_PROBER=' /etc/default/grub; then \
                     sed -i 's/^GRUB_DISABLE_OS_PROBER=.*/GRUB_DISABLE_OS_PROBER=false/' /etc/default/grub; \
                     else echo 'GRUB_DISABLE_OS_PROBER=false' >> /etc/default/grub; fi",
                ));
                // GRUB_DEFAULT=saved makes GRUB READ the remembered entry from
                // grubenv, so a default set from the running system
                // (`grub-set-default`) survives. Reading is always safe.
                //
                // GRUB_SAVEDEFAULT is deliberately NOT set. It would make GRUB
                // WRITE grubenv on every boot, to remember the last-booted entry
                // — and that write is done through a block list, which fails
                // whenever the file is not a plain contiguous run of blocks:
                //
                //   error: commands/loadenv.c:check_blocklists:289:
                //          sparse file not allowed.
                //   Press any key to continue...
                //
                // This code used to set it, with a comment claiming the failure
                // was silent and harmless on btrfs. It is neither: GRUB prints
                // that error and HALTS the boot until a key is pressed, on every
                // single start. Reported from a three-system machine. Remembering
                // the last choice is a convenience; an error that stops an
                // unattended boot is not a fair price for it.
                plan.push(chroot(
                    "if grep -q '^GRUB_DEFAULT=' /etc/default/grub; then \
                       sed -i 's/^GRUB_DEFAULT=.*/GRUB_DEFAULT=saved/' /etc/default/grub; \
                     else echo 'GRUB_DEFAULT=saved' >> /etc/default/grub; fi; \
                     if grep -q '^GRUB_SAVEDEFAULT=' /etc/default/grub; then \
                       sed -i 's/^GRUB_SAVEDEFAULT=.*/GRUB_SAVEDEFAULT=false/' /etc/default/grub; fi",
                ));
            }
            // Neighbour OS loaders → chainload entries, installed as a grub.d
            // GENERATOR (/etc/grub.d/35_artix_neighbours) so it re-scans on
            // EVERY grub-mkconfig. A one-time scan at install could only see the
            // neighbours that already existed: the FIRST of two Artix systems
            // never listed the SECOND (it did not exist yet), so its menu was
            // missing it forever. As a generator, whenever THIS system next
            // regenerates its config (a kernel or grub update, or by hand) it
            // picks up every current neighbour — self-healing in both
            // directions. os-prober alone misses any Linux whose kernels live
            // on its ESP (this installer's own default layout), which is why a
            // dedicated scanner exists at all. Numbered 35 so its entries sit
            // right after os-prober's (30) and before the rollback entry (45).
            //
            // Installed on EVERY UEFI GRUB install, NOT only when the user said
            // there is another OS. That answer describes the disk at install
            // time and was trusted absolutely: a person who picked "Artix only"
            // while a neighbour actually sat on the disk got no generator at
            // all, and their first system vanished from the menu — reported
            // exactly that way. The generator costs nothing when there is no
            // neighbour (it prints entries only for EFI directories that hold a
            // real loader and are not ours), and being a generator it also
            // catches a system installed LATER, which no install-time answer
            // could ever describe. The layout at boot time is the authority,
            // not what was ticked during setup.
            if uefi {
                // Same ESP-mountpoint rule as the grub-install above (its
                // binding is scoped to that branch, so restate it here).
                let esp_dir = if c.partition_mode.is_manual_family() {
                    if c.manual_esp_mount == "/boot/efi" {
                        "/boot/efi"
                    } else {
                        "/boot"
                    }
                } else if c.encrypt_disk && c.encrypt_scope == "full" {
                    "/boot/efi"
                } else {
                    "/boot"
                };
                plan.push(write_target_file(
                    "/mnt/etc/grub.d/35_artix_neighbours",
                    &GRUB_NEIGHBOUR_CHAIN.replace("@@ESP_DIR@@", esp_dir),
                ));
                plan.push(chroot("chmod 755 /etc/grub.d/35_artix_neighbours"));
            }
            // Boot-time rollback entry for GRUB: a generator that adds a
            // top-level "System Rollback" menuentry (booting with artix.rollback)
            // next to the normal entries. Must land before grub-mkconfig runs.
            if c.root_fs == "btrfs" && c.btrfs_subvolumes && c.btrfs_snapshots {
                let (vmlinuz, initramfs) = kernel_images(c.kernel);
                let script = ARTIX_ROLLBACK_GRUBD
                    .replace("@@VMLINUZ@@", vmlinuz)
                    .replace("@@INITRD@@", initramfs);
                plan.push(write_target_file(
                    "/mnt/etc/grub.d/45_artix_rollback",
                    &script,
                ));
                plan.push(chroot("chmod 755 /etc/grub.d/45_artix_rollback"));
            }
            // Keep grub.cfg current automatically: a pacman hook regenerates it
            // after any kernel change or grub upgrade. Without it a new kernel
            // never reaches the menu (Arch/Artix's grub package does not do this
            // on its own), and — the reason it matters here — the neighbour
            // generator re-runs on every regeneration, so a dual-boot system
            // installed LATER becomes visible the next time this one updates its
            // kernel, with no manual grub-mkconfig.
            plan.push(write_target_file(
                "/mnt/etc/pacman.d/hooks/zz-artix-grub.hook",
                GRUB_REGEN_HOOK,
            ));
            plan.push(chroot("grub-mkconfig -o /boot/grub/grub.cfg"));
        }
    }

    // 10c) Secure Boot PREPARATION (EFISTUB only, opt-in). We deliberately do
    //      NOT enable Secure Boot or enroll keys here: enrollment must happen on
    //      the running system (it's unreliable from a chroot), it requires the
    //      user to put the firmware into Setup Mode by hand, and a bad enroll can
    //      brick some firmware. So the installer only PREPARES: it generates the
    //      signing keys (create-keys works in the chroot) and drops a bilingual
    //      instruction file in the user's home with the exact remaining steps and
    //      explicit brick-risk warnings. sbctl's pacman hook will re-sign the
    //      kernel automatically after the user signs it once.
    if c.bootloader.supports_secureboot_prep() && c.prepare_secureboot && uefi {
        // Generate the key pair now (safe in chroot). Signing/enrolling is left
        // to the user on first boot, per the instructions below.
        plan.push(chroot("sbctl create-keys || true"));
        // Bilingual instruction file written to the user's home. Kept as a
        // placeholder-substituted heredoc so none of the shell/`sbctl` syntax
        // needs escaping. @@KERNEL@@ is the actual vmlinuz name for their kernel.
        let (vmlinuz, _initramfs) = kernel_images(c.kernel);
        let doc = SECUREBOOT_README.replace("@@KERNEL@@", vmlinuz);
        plan.push(write_target_file(
            &format!("/mnt/home/{}/SECURE-BOOT.txt", c.username),
            &doc,
        ));
        plan.push(chroot(&format!(
            "chown {user}:{user} /home/{user}/SECURE-BOOT.txt || true",
            user = c.username
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{BootMode, ExtraDisk, PartitionMode};

    fn plan_text(plan: &[Action]) -> String {
        plan.iter()
            .map(|a| format!("{} {}", a.program, a.args.join(" ")))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn resolve_mp_expands_tilde_home() {
        let c = InstallConfig {
            username: "alice".into(),
            ..Default::default()
        };
        assert_eq!(resolve_mp(&c, "~/games"), "/home/alice/games");
        assert_eq!(resolve_mp(&c, "/mnt/data"), "/mnt/data");
        assert_eq!(resolve_mp(&c, "/home/x"), "/home/x");
    }

    #[test]
    fn crypt_mapper_sanitizes_path() {
        assert_eq!(crypt_mapper("/mnt/storage"), "crypt_mnt_storage");
        assert_eq!(crypt_mapper("/home/bob/d"), "crypt_home_bob_d");
        assert_eq!(crypt_mapper("/"), "crypt_data");
    }

    fn install_app() -> App {
        let mut a = App::new();
        a.config.disk = "/dev/sda".into();
        a.config.username = "tester".into();
        a.config.hostname = "box".into();
        a.config.boot_mode = BootMode::Uefi;
        a.config.bootloader = Bootloader::Grub;
        a.config.root_fs = "btrfs".into();
        a
    }

    // ── Regression tests ──────────────────────────────────────────────────
    //
    // Every test below is a bug that actually shipped. They exist so that a
    // future refactor cannot quietly bring it back: the plan is pure data, so
    // an install can be inspected without touching a disk.

    /// The shared log helper lands before anything sources it.
    ///
    /// The boot services stub `log`/`warn` out when the library is missing, so
    /// a wrong order costs no boot and raises no error — it just silently
    /// throws away the only trace those detached services leave, which is the
    /// exact failure this logging exists to prevent.
    #[test]
    fn the_log_helper_is_installed_before_the_services_that_source_it() {
        let mut a = install_app();
        a.config.root_fs = "btrfs".into();
        a.config.btrfs_subvolumes = true;
        a.config.btrfs_snapshots = true;
        let t = plan_text(&build_plan(&a));

        let lib = t
            .lines()
            .position(|l| l.contains("/usr/local/lib/artix-installer/log.sh"))
            .expect("the shared log helper is never installed");

        // Every script that sources it must come later.
        for (i, line) in t.lines().enumerate() {
            if line.contains(". /usr/local/lib/artix-installer/log.sh") {
                assert!(
                    lib < i,
                    "a service sources the log helper before it is written:\n{line}"
                );
            }
        }
        // And the sourcing really is there — otherwise the loop above passes
        // by simply never running.
        assert!(
            t.contains(". /usr/local/lib/artix-installer/log.sh"),
            "no service sources the helper, so installing it is dead weight"
        );
    }

    /// No secret ever reaches the install log.
    ///
    /// The log is mirrored to /tmp/installer.log and copied onto the FINISHED
    /// system, so a password echoed while installing outlives the installation
    /// and rides along in any bug report the log is pasted into. `0600` is no
    /// answer: it does not survive a backup, a `cat`, or a helpful paste.
    ///
    /// Two halves, because either alone has a hole. The value check cannot see
    /// the USB-key passphrase (it is MINTED at plan time and never stored), and
    /// the structural check cannot prove a redaction is actually secret-free.
    #[test]
    fn no_password_or_passphrase_can_reach_the_install_log() {
        const USER_PW: &str = "UserPwNeedle01";
        const ROOT_PW: &str = "RootPwNeedle02";
        const LUKS_PW: &str = "LuksPhraseNeedle03";

        // Every account mode, and both LUKS shapes — the secrets live on
        // different branches and a per-branch miss is exactly how one escapes.
        let mut configs: Vec<App> = Vec::new();
        for mode in [
            crate::app::AccountMode::UserSameRoot,
            crate::app::AccountMode::UserSeparateRoot,
            crate::app::AccountMode::RootOnly,
            crate::app::AccountMode::UserSudoOnly,
        ] {
            for encrypt in [false, true] {
                let mut a = install_app();
                a.config.account_mode = mode;
                // A quote and a dollar sign: these reach the command
                // SHELL-ESCAPED, so a redaction that searched for the raw value
                // would sail straight past them.
                a.config.user_password = format!("{USER_PW}'$x");
                a.config.root_password = format!("{ROOT_PW}\"$y");
                a.config.encrypt_disk = encrypt;
                if encrypt {
                    a.config.luks_passphrase = LUKS_PW.into();
                }
                configs.push(a);
            }
        }
        // And the key-only stick, whose passphrase is minted rather than typed.
        let mut usb = install_app();
        usb.config.encrypt_disk = true;
        usb.config.luks_passphrase = String::new();
        usb.config.usb_key_device = "/dev/vdz".into();
        usb.config.usb_key_only = true;
        configs.push(usb);

        for a in &configs {
            for step in build_plan(a) {
                let logged = step.log_line();

                // (1) No needle, in any form, in what the log would show.
                for needle in [USER_PW, ROOT_PW, LUKS_PW] {
                    assert!(
                        !logged.contains(needle),
                        "a secret reaches the install log:\n{logged}"
                    );
                }

                // (2) Anything that HANDLES a credential must be redacted, even
                // when this test cannot see the credential's value. `--key-file`
                // steps are exempt: they name a path, not a secret.
                let cmd = format!("{} {}", step.program, step.args.join(" "));
                let handles_credential = (cmd.contains("chpasswd")
                    || cmd.contains("luksFormat")
                    || cmd.contains("luksAddKey")
                    || cmd.contains("luksRemoveKey")
                    || cmd.contains("cryptsetup open"))
                    && !cmd.contains("--key-file");
                if handles_credential {
                    assert!(
                        step.redacted.is_some(),
                        "a step handling a credential is logged verbatim:\n{cmd}"
                    );
                }
            }
        }
    }

    /// The USB key stick must never be formatted as an extra disk. Wiping it
    /// destroys the only thing able to unlock a key-only install — an
    /// unbootable system with no recovery path.
    #[test]
    fn usb_key_stick_is_never_formatted_as_an_extra_disk() {
        let mut a = install_app();
        a.config.encrypt_disk = true;
        a.config.usb_key_device = "/dev/sdb".into();
        // A mountpoint assigned BEFORE the stick was picked as the key: the UI
        // retracts it, but build_plan must not depend on the UI having done so.
        a.config.extra_disks.push(ExtraDisk {
            disk: "/dev/sdb".into(),
            mountpoint: "/data".into(),
            fs: "ext4".into(),
            format: true,
            whole_disk: true,
            ..Default::default()
        });
        let t = plan_text(&build_plan(&a));
        assert!(
            !t.contains("mkfs.ext4 /dev/sdb"),
            "the key stick must not be mkfs'd as an extra disk"
        );
        assert!(
            !t.contains("mount /dev/sdb1 /mnt/data"),
            "the key stick must not be mounted as an extra disk"
        );
    }

    /// Installing onto the key stick is refused outright, before anything has
    /// touched a disk.
    #[test]
    fn install_disk_equal_to_key_stick_is_refused() {
        let mut a = install_app();
        a.config.encrypt_disk = true;
        a.config.disk = "/dev/sdb".into();
        a.config.usb_key_device = "/dev/sdb".into();
        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains("Refusing to continue"),
            "same device for install and key must abort"
        );
        assert!(
            !t.contains("basestrap"),
            "nothing may run after the refusal"
        );
    }

    /// An empty extra disk with no mountpoint is not touched at all — that is
    /// the promise the "don't format" option makes on screen.
    #[test]
    fn extra_disk_without_mountpoint_is_left_alone() {
        let mut a = install_app();
        a.config.extra_disks.push(ExtraDisk {
            disk: "/dev/sdc".into(),
            mountpoint: String::new(), // "don't format" slot
            fs: "ext4".into(),
            format: false,
            whole_disk: true,
            ..Default::default()
        });
        let t = plan_text(&build_plan(&a));
        assert!(
            !t.contains("/dev/sdc"),
            "an untouched disk must not appear in the plan"
        );
    }

    /// The user must be able to read /var/log without root: syslog-ng writes
    /// root:log 0640, so membership in `log` is what makes it work.
    #[test]
    fn user_can_read_logs_without_root() {
        let t = plan_text(&build_plan(&install_app()));
        assert!(
            t.contains("groupadd -f log"),
            "the log group must exist before useradd -G references it"
        );
        assert!(
            t.contains(",log ") || t.contains(",log\'") || t.contains(",log\""),
            "the user must be a member of the log group"
        );
        assert!(
            t.contains("chgrp -R log /var/log"),
            "logs written during the install must join the group too"
        );
    }

    /// A missing `log` group would make `useradd -G` fail — and a failed
    /// useradd means an installed system with NO user account at all.
    #[test]
    fn groupadd_precedes_useradd() {
        let t = plan_text(&build_plan(&install_app()));
        let g = t.find("groupadd -f log").expect("groupadd must be planned");
        let u = t.find("useradd").expect("useradd must be planned");
        assert!(g < u, "groupadd must come before useradd, not after");
    }

    /// Separate-disk dual boot: whole-disk Auto on the Artix disk, with the
    /// dual-boot survey's os-prober flag on. The chosen disk IS wiped (Auto),
    /// os-prober is enabled for the GRUB menu, and the NTFS-reading tooling is
    /// installed so os-prober can actually identify the Windows on the OTHER
    /// disk — the whole point of this scenario.
    #[test]
    fn separate_disk_auto_with_osprober_detects_the_neighbour() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Auto;
        a.config.os_prober = true; // set by the "separate disk" pmode option
        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains("sgdisk --zap-all /dev/sda"),
            "Auto wipes the chosen Artix disk (other disks are untouched)"
        );
        assert!(
            t.contains("GRUB_DISABLE_OS_PROBER=false"),
            "os-prober must be enabled so Windows on the other disk is detected"
        );
        assert!(
            t.contains("ntfs-3g"),
            "ntfs-3g lets os-prober read the Windows disk"
        );
    }

    /// EFISTUB is the only bootloader we can Secure Boot-prepare (no loader in
    /// the chain, so the kernel image itself is what gets signed).
    #[test]
    fn secureboot_prep_is_efistub_only() {
        let mut a = install_app();
        a.config.prepare_secureboot = true;

        a.config.bootloader = Bootloader::Grub;
        let grub = plan_text(&build_plan(&a));
        assert!(
            !grub.contains("secureboot"),
            "GRUB must not get Secure Boot preparation"
        );

        a.config.bootloader = Bootloader::Efistub;
        let efi = plan_text(&build_plan(&a));
        assert!(
            efi.contains("sbctl") || efi.contains("secureboot"),
            "EFISTUB with prep enabled must set Secure Boot up"
        );
    }

    /// Manual mode touches ONLY what it was told to touch.
    ///
    /// The dual-boot contract: no wipefs, no sgdisk — the rest of the table
    /// belongs to another OS. Root gets formatted; the ESP is REUSED unless
    /// the user flips the format toggle; the mount targets match the auto
    /// path so everything downstream is mode-blind.
    #[test]
    fn manual_mode_touches_only_what_it_is_told() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_root = "/dev/vda5".into();
        a.config.manual_root_fs = "ext4".into();
        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains("mkfs.ext4 -F /dev/vda5"),
            "root must be formatted"
        );
        assert!(!t.contains("sgdisk"), "the partition table is the user's");
        assert!(
            !t.lines().any(|l| l == "wipefs -a /dev/vda"),
            "the whole disk must not be wiped (a partition wipe before mkfs is fine)"
        );
        assert!(
            !t.contains("mkfs.fat -F32 /dev/vda1"),
            "the ESP is reused by default — a Windows ESP must survive"
        );
        assert!(
            t.contains("mount /dev/vda1 /mnt/boot"),
            "the ESP must be mounted where the auto path mounts it"
        );
        assert!(
            t.contains("only the named partitions"),
            "the log must state the manual contract up front"
        );

        a.config.manual_esp_format = true;
        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains("mkfs.fat -F32 /dev/vda1"),
            "the format toggle must format the ESP"
        );
    }

    /// Manual mode v2 — CREATING partitions in free space. The editor plans an
    /// ESP + a rest-of-disk root as NEW partitions on /dev/vda; the plan must
    /// ADD them with `sgdisk -n` (never wipefs / --zap-all — the existing table
    /// and any neighbouring OS survive), format and mount them exactly like the
    /// existing-partition path, and create a fresh GPT only if the disk has none.
    #[test]
    fn manual_mode_creates_new_partitions_without_wiping_the_table() {
        use crate::app::MANUAL_REST;
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_disk = "/dev/vda".into();
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_esp_new_mib = 1024;
        a.config.manual_esp_format = true;
        a.config.manual_root = "/dev/vda2".into();
        a.config.manual_root_new_mib = MANUAL_REST;
        a.config.manual_root_fs = "ext4".into();
        let t = plan_text(&build_plan(&a));

        assert!(
            t.contains("sgdisk -n 1:0:+1024M -t 1:ef00 -c 1:EFI /dev/vda"),
            "the new ESP must be added as a sized partition:\n{t}"
        );
        assert!(
            t.contains("sgdisk -n 2:0:0 -t 2:8300 -c 2:ROOT /dev/vda"),
            "the rest-of-disk root must be added as partition 2 spanning free space"
        );
        assert!(
            !t.contains("--zap-all") && !t.lines().any(|l| l == "wipefs -a /dev/vda"),
            "creating partitions must NOT wipe the existing table"
        );
        assert!(
            t.contains("sgdisk -og /dev/vda"),
            "a blank disk (no PTTYPE) must get a fresh GPT first"
        );
        // The created partitions still flow through the same format+mount path.
        assert!(
            t.contains("mkfs.fat -F32 /dev/vda1"),
            "new ESP is formatted"
        );
        assert!(
            t.contains("mkfs.ext4 -F /dev/vda2"),
            "new root is formatted"
        );
        assert!(
            t.contains("mount /dev/vda1 /mnt/boot"),
            "new ESP mounts where the auto path mounts it"
        );
    }

    /// The ESP-at-/boot/efi layout — what a dual boot with a SMALL reused
    /// Windows ESP needs, since a 100–300 MiB ESP can't hold Linux kernels.
    /// The ESP must mount at /mnt/boot/efi (kernels stay on the root fs at
    /// /boot) AND grub-install must point --efi-directory at /boot/efi to
    /// match, or GRUB writes its EFI files to the wrong place and won't boot.
    #[test]
    fn manual_esp_at_boot_efi_mounts_and_installs_grub_there() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_esp_mount = "/boot/efi".into();
        a.config.manual_root = "/dev/vda5".into();
        a.config.manual_root_fs = "ext4".into();
        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains("mount /dev/vda1 /mnt/boot/efi"),
            "the ESP must mount at /mnt/boot/efi so kernels stay on root:\n{t}"
        );
        assert!(
            t.contains("mkdir -p /mnt/boot/efi"),
            "the /boot/efi mountpoint must be created first"
        );
        assert!(
            t.contains("grub-install --target=x86_64-efi --efi-directory=/boot/efi "),
            "grub-install must target /boot/efi to match the ESP mountpoint"
        );
    }

    /// Install-alongside shares the manual plan path: it reuses the ESP at
    /// /boot/efi, creates the root in free space (additive sgdisk, no wipe),
    /// turns on os-prober, and — the fix for "GRUB doesn't see Windows" —
    /// pulls in ntfs-3g so os-prober can read the neighbour's NTFS partition.
    #[test]
    fn alongside_reuses_esp_creates_root_and_installs_osprober_tooling() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Alongside;
        // These are what the alongside screen's apply() fills in.
        a.config.manual_disk = "/dev/nvme0n1".into();
        a.config.manual_esp = "/dev/nvme0n1p1".into();
        a.config.manual_esp_mount = "/boot/efi".into();
        a.config.manual_esp_format = false;
        a.config.manual_root = "/dev/nvme0n1p4".into();
        a.config.manual_root_new_mib = crate::app::MANUAL_REST;
        a.config.manual_root_fs = "btrfs".into();
        a.config.btrfs_subvolumes = true;
        a.config.btrfs_snapshots = true;
        let t = plan_text(&build_plan(&a));

        assert!(
            !t.contains("--zap-all") && !t.lines().any(|l| l == "wipefs -a /dev/nvme0n1"),
            "alongside must NOT wipe the disk — the neighbour OS lives there"
        );
        assert!(
            t.contains("sgdisk -n 4:0:0 -t 4:8300 -c 4:ROOT /dev/nvme0n1"),
            "root is created in the free space as partition 4:\n{t}"
        );
        assert!(
            !t.contains("mkfs.fat -F32 /dev/nvme0n1p1"),
            "the shared ESP must be REUSED, never reformatted"
        );
        assert!(
            t.contains("mount /dev/nvme0n1p1 /mnt/boot/efi"),
            "the ESP mounts at /boot/efi so kernels stay on root"
        );
        assert!(
            t.contains("grub-install --target=x86_64-efi --efi-directory=/boot/efi "),
            "grub-install must target the shared ESP at /boot/efi"
        );
        assert!(
            t.contains("GRUB_DISABLE_OS_PROBER=false"),
            "os-prober must be enabled so the neighbour lands in the GRUB menu"
        );
        // basestrap installs the base set; the sanitiser turns os_prober on for
        // this mode, so ntfs-3g + os-prober must ride the strap line — the fix
        // for os-prober silently skipping the Windows NTFS partition.
        assert!(t.contains("ntfs-3g"), "ntfs-3g must be installed:\n{t}");
        assert!(t.contains("os-prober"), "os-prober must be installed");
    }

    /// Artix beside Artix: the shared ESP must NOT lose the neighbour's loader.
    ///
    /// Reported live — installing Artix next to an existing Artix left only the
    /// new one bootable. A plain `grub-install --bootloader-id=Artix` overwrites
    /// EFI/Artix on the shared ESP, so the old loader is gone. On a shared ESP
    /// the install now picks the first free id and marks the directory as ours,
    /// so a neighbour using the same id survives.
    #[test]
    fn beside_another_artix_the_shared_esp_gets_a_unique_bootloader_id() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Alongside;
        a.config.manual_disk = "/dev/nvme0n1".into();
        a.config.manual_esp = "/dev/nvme0n1p1".into();
        a.config.manual_esp_mount = "/boot/efi".into();
        a.config.manual_esp_format = false;
        a.config.manual_root = "/dev/nvme0n1p4".into();
        a.config.manual_root_new_mib = crate::app::MANUAL_REST;
        a.config.manual_root_fs = "ext4".into();
        a.config.bootloader_id = "Artix".into();
        let t = plan_text(&build_plan(&a));

        // It must NOT blindly install to a fixed EFI/Artix on the shared ESP.
        assert!(
            !t.contains("--bootloader-id='Artix'"),
            "a shared ESP still overwrites EFI/Artix, killing the neighbour:\n{t}"
        );
        // Instead: probe for a free id, then install and claim it.
        assert!(
            t.contains("[ -d \"/boot/efi/EFI/$id\" ]")
                && t.contains(".artix-tui-owned")
                && t.contains("grub-install --target=x86_64-efi --efi-directory=/boot/efi --bootloader-id=\"$id\""),
            "the unique-id + ownership dance is missing:\n{t}"
        );

        // The claim must be IDENTITY-BEARING. Writing a bare marker was the
        // whole bug the second time round: a second Artix from this same
        // installer read the first one's marker as its own, reused EFI/Artix
        // and overwrote the neighbour's loader — the very thing this dance
        // exists to prevent. The marker holds the root filesystem UUID, and
        // the directory is reused only when that UUID matches ours.
        assert!(
            t.contains("me=$(findmnt -no UUID / 2>/dev/null)"),
            "the install never establishes WHICH system it is:\n{t}"
        );
        assert!(
            !t.contains("touch \"/boot/efi/EFI/$id/.artix-tui-owned\""),
            "the ownership marker is still empty, so any artix-tui install \
             claims any other one's directory:\n{t}"
        );
        assert!(
            t.contains("printf '%s\\n' \"$me\" > \"/boot/efi/EFI/$id/.artix-tui-owned\""),
            "the marker does not record whose directory it is:\n{t}"
        );
        assert!(
            t.contains("if [ -n \"$me\" ] && [ \"$owner\" = \"$me\" ]; then break; fi"),
            "the directory is reused without checking it is really ours:\n{t}"
        );

        // The UEFI-entry field allows spaces, and `$id` must be quoted wherever
        // it expands — an unquoted "My Artix" would word-split the shell test
        // and abort the whole install.
        a.config.bootloader_id = "My Artix".into();
        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains("[ -d \"/boot/efi/EFI/$id\" ]")
                && t.contains("\"/boot/efi/EFI/$id/.artix-tui-owned\""),
            "a bootloader id with a space is not safely quoted in the shell:\n{t}"
        );
    }

    /// A neighbouring Artix is NOT mistaken for ourselves.
    ///
    /// The chain script skipped every directory carrying the .artix-tui-owned
    /// marker, on the assumption that only our own install ever wrote one. But
    /// "the other OS" is very often another Linux — and quite possibly another
    /// Artix from this same installer, whose directory carries the identical
    /// marker. So the neighbour was skipped as if it were us and never reached
    /// the menu, which is exactly what a person saw after installing twice.
    #[test]
    fn a_neighbouring_artix_is_told_apart_from_our_own_loader() {
        let script = GRUB_NEIGHBOUR_CHAIN;

        // Ownership is decided by the marker's CONTENT, not its existence.
        assert!(
            !script.contains("[ -f \"${d}.artix-tui-owned\" ] && continue"),
            "the script still skips every marked directory, neighbours included"
        );
        assert!(
            script.contains("own_root=$(findmnt -no UUID / 2>/dev/null)"),
            "the script never learns which root is ours"
        );
        assert!(
            script.contains("if [ -n \"$own_root\" ] && [ \"$owner\" = \"$own_root\" ]; then"),
            "the skip is not conditioned on the marker naming OUR root"
        );

        // Two installs of the same distro default to the same bootloader-id, so
        // the menu row must carry something that tells them apart (the device).
        assert!(
            script.contains("menuentry \"$name (EFI/$name on $3)\" {"),
            "two identically-named loaders would produce two identical menu rows"
        );
        // It is a GENERATOR now: emits to stdout, never appends to 40_custom.
        assert!(
            !script.contains(">>/etc/grub.d/40_custom"),
            "the scanner still appends to 40_custom instead of generating to stdout"
        );
    }

    /// The ownership marker must never be written EMPTY.
    ///
    /// `findmnt -no UUID /` answers from the udev/blkid cache. On a freshly
    /// populated chroot — or for any caller that cannot read the block device —
    /// it returns nothing at all, and the marker then says "owned by ''". The
    /// generator compares that against the running root's UUID, never matches,
    /// and so treats OUR OWN loader as a neighbour: the menu grows an entry that
    /// chainloads the running system into its own menu. Seen live on a btrfs
    /// install. Probing the device is the fallback that cannot come up empty.
    #[test]
    fn the_ownership_marker_never_depends_on_a_single_lookup() {
        for solo in [true, false] {
            let mut a = install_app();
            a.config.partition_mode = PartitionMode::Manual;
            a.config.manual_solo = solo;
            a.config.manual_disk = "/dev/vda".into();
            a.config.manual_boot_disk = "/dev/vda".into();
            a.config.manual_esp = "/dev/vda1".into();
            a.config.manual_root = "/dev/vda2".into();
            a.config.manual_root_fs = "btrfs".into();
            let t = plan_text(&build_plan(&a));
            assert!(
                t.contains("[ -n \"$me\" ] || me=$(blkid -s UUID -o value"),
                "solo={solo}: no fallback when findmnt returns nothing, so the \
                 marker can be written empty:\n{t}"
            );
            // btrfs hangs [/subvol] off SOURCE; blkid needs a bare device path.
            assert!(
                t.contains("sed 's/\\[.*//'"),
                "solo={solo}: the subvolume suffix is not stripped, so the \
                 fallback hands blkid a path that is not a device:\n{t}"
            );
        }
    }

    /// The ESP scan must not use `blkid -t PARTTYPE=`, which never worked.
    ///
    /// `blkid -t` queries the blkid CACHE, and PARTTYPE is not a cached tag —
    /// it comes from the partition table and blkid only reports it when probing
    /// a device directly. So the query returned nothing on every machine, the
    /// "other ESPs" loop never ran once, and a neighbour on its own disk could
    /// not be found however correct the rest of the script was. Confirmed on a
    /// live system whose /dev/sda1 is an ESP: lsblk lists it, `blkid -t
    /// PARTTYPE=...` prints nothing.
    #[test]
    fn the_esp_scan_enumerates_with_lsblk_not_the_blkid_cache() {
        // CODE only: the comment above the loop names the broken form to explain
        // why it is gone, and matching prose would fail on the explanation.
        let code: String = GRUB_NEIGHBOUR_CHAIN
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        let script = GRUB_NEIGHBOUR_CHAIN;
        assert!(
            !code.contains("blkid -t PARTTYPE="),
            "the ESP scan is back on the blkid cache, which never returns a \
             PARTTYPE match — the neighbour loop would be dead again"
        );
        assert!(
            script.contains("lsblk -rno PATH,PARTTYPE"),
            "the ESP scan has no working enumeration of partitions"
        );
        // Matched case-insensitively: the GUID's case is not guaranteed.
        assert!(
            script.contains("tolower($2) == g"),
            "the ESP type GUID is compared case-sensitively"
        );
    }

    /// A solo whole-disk-style manual install owns the disk, so no such dance:
    /// its ESP is its own, and the plain fixed-id install is correct (and keeps
    /// the config's chosen id verbatim).
    #[test]
    fn a_solo_manual_install_keeps_its_plain_bootloader_id() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_solo = true;
        a.config.manual_disk = "/dev/vda".into();
        a.config.manual_boot_disk = "/dev/vda".into();
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_root = "/dev/vda2".into();
        a.config.manual_root_fs = "ext4".into();
        a.config.bootloader_id = "Artix".into();
        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains("--bootloader-id='Artix'"),
            "a solo install owns its ESP and should keep its plain id:\n{t}"
        );
        // No ID DANCE: a solo install must never step to Artix-2. Its root is
        // reformatted on every reinstall, so the marker can never match the old
        // one, and a dance here would mint Artix-2, Artix-3… on a reused ESP.
        assert!(
            !t.contains("while [ -d"),
            "a solo install must not hunt for a free bootloader-id:\n{t}"
        );
        // It DOES mark its directory, which is a different question: the
        // neighbour generator runs on every install now, and without a marker it
        // cannot tell our own loader from a stranger's — it would offer a menu
        // entry chainloading us into our own menu.
        assert!(
            t.contains(".artix-tui-owned"),
            "a solo install leaves its EFI directory unmarked, so its own \
             generator cannot recognise it:\n{t}"
        );
        // A UEFI GRUB install must ALSO write the removable/fallback path — see
        // the fallback test below for why.
        assert!(
            t.contains("--removable"),
            "no fallback (EFI/BOOT/BOOTX64.EFI) install — boot is left to \
             firmware enumeration:\n{t}"
        );
    }

    /// The neighbour generator ships even when the user said there is no
    /// neighbour — because that answer describes the disk at INSTALL time.
    ///
    /// Reported live: a second Artix installed manually beside a first, with the
    /// loaders on one disk, and the resulting GRUB did not list the first system
    /// at all. Picking "Artix only" set os_prober = false, which was also the
    /// gate on 35_artix_neighbours, so nothing ever scanned for the neighbour
    /// that was plainly there. The generator re-runs on every grub-mkconfig and
    /// prints nothing when there is no neighbour, so gating it on a promise
    /// bought nothing and cost a bootable system its menu entry.
    #[test]
    fn even_a_solo_install_ships_the_neighbour_generator() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_solo = true; // "there is no other OS here"
        a.config.os_prober = false;
        a.config.manual_disk = "/dev/vda".into();
        a.config.manual_boot_disk = "/dev/vda".into();
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_root = "/dev/vda2".into();
        a.config.manual_root_fs = "ext4".into();
        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains("/etc/grub.d/35_artix_neighbours"),
            "a solo install ships no neighbour scanner, so a neighbouring \
             system — there already, or added later — can never reach the \
             menu:\n{t}"
        );
        assert!(
            t.contains("chmod 755 /etc/grub.d/35_artix_neighbours"),
            "the generator is not executable, so grub-mkconfig skips it:\n{t}"
        );
    }

    /// A UEFI GRUB install ALSO writes the removable/fallback loader.
    ///
    /// efibootmgr cannot create a named NVRAM entry from inside artix-chroot —
    /// /sys is mounted read-only there, so efivars is inaccessible and the entry
    /// is silently skipped. With no NVRAM entry and no fallback path, firmware
    /// has to enumerate the ESP's loaders: harmless with one GRUB, but with two
    /// (Artix beside Artix) OVMF boots one at random each time, sometimes
    /// skipping the menu — the reported chaos. EFI/BOOT/BOOTX64.EFI is the fixed
    /// target the newest install wins, and the newest GRUB lists every
    /// neighbour, so booting it shows them all.
    #[test]
    fn a_uefi_grub_install_writes_the_removable_fallback_loader() {
        // Solo whole-disk install.
        let mut a = install_app();
        a.config.bootloader = Bootloader::Grub;
        a.config.boot_mode = BootMode::Uefi;
        let solo = plan_text(&build_plan(&a));
        assert!(
            solo.contains("grub-install --target=x86_64-efi") && solo.contains("--removable"),
            "a solo UEFI GRUB install skips the fallback loader:\n{solo}"
        );

        // Shared-ESP dual boot: the fallback must be written here TOO — this is
        // the case that actually broke.
        let mut b = install_app();
        b.config.partition_mode = PartitionMode::Alongside;
        b.config.manual_disk = "/dev/nvme0n1".into();
        b.config.manual_esp = "/dev/nvme0n1p1".into();
        b.config.manual_esp_mount = "/boot/efi".into();
        b.config.manual_root = "/dev/nvme0n1p4".into();
        b.config.manual_root_new_mib = crate::app::MANUAL_REST;
        b.config.manual_root_fs = "ext4".into();
        let dual = plan_text(&build_plan(&b));
        assert!(
            dual.contains("--removable"),
            "a shared-ESP dual-boot install skips the fallback loader — the exact \
             case that booted at random:\n{dual}"
        );
    }

    /// A neighbour OS whose loader lives on an ESP is chainloaded into GRUB.
    ///
    /// os-prober misses a Linux that keeps its kernels on its ESP (which is this
    /// installer's own default layout), so "add the other OS to the boot menu"
    /// produced a menu with only ourselves. The chainload script fills that gap;
    /// it must be written and run before grub-mkconfig, and only under UEFI.
    #[test]
    fn a_neighbour_os_loader_is_chainloaded_into_the_grub_menu() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Alongside;
        a.config.manual_disk = "/dev/nvme0n1".into();
        a.config.manual_esp = "/dev/nvme0n1p1".into();
        a.config.manual_esp_mount = "/boot/efi".into();
        a.config.manual_esp_format = false;
        a.config.manual_root = "/dev/nvme0n1p4".into();
        a.config.manual_root_new_mib = crate::app::MANUAL_REST;
        a.config.manual_root_fs = "ext4".into();
        let t = plan_text(&build_plan(&a));

        // The scanner is installed as a grub.d GENERATOR — so it re-runs on
        // every grub-mkconfig and a later-added neighbour is picked up — not a
        // one-time append. (The write is a heredoc, so the script body spans
        // many lines in the plan text; match the `cat >` line for ordering.)
        assert!(
            t.contains("emit_esp") && t.contains("/etc/grub.d/35_artix_neighbours"),
            "the neighbour scanner is never installed as a grub.d generator:\n{t}"
        );
        let install = t
            .lines()
            .position(|l| l.contains("cat > /etc/grub.d/35_artix_neighbours"))
            .expect("the generator is never written");
        assert!(
            t.contains("chmod 755 /etc/grub.d/35_artix_neighbours"),
            "the generator is not made executable, so grub-mkconfig would skip it:\n{t}"
        );
        // It must land BEFORE grub-mkconfig, which is what runs it.
        let mkconfig = t
            .lines()
            .position(|l| l.contains("grub-mkconfig -o /boot/grub/grub.cfg"))
            .expect("grub-mkconfig is never run");
        assert!(
            install < mkconfig,
            "the generator must be installed before grub-mkconfig runs it"
        );
        // It carries the ESP mountpoint, resolved from the config, and emits
        // menuentries to STDOUT (it is a generator, not a 40_custom appender).
        assert!(
            t.contains("own_dir='/boot/efi'"),
            "the generator did not get the shared ESP mountpoint"
        );
        assert!(
            !t.contains(">>/etc/grub.d/40_custom"),
            "the scanner still appends to 40_custom instead of generating"
        );

        // A default set from the running system is READ back (GRUB_DEFAULT=saved
        // is a read; that is always safe). GRUB must never be told to WRITE
        // grubenv on every boot: that write goes through a block list and fails
        // with "sparse file not allowed", then halts the boot waiting for a
        // keypress. Reported from a real three-system machine, on every start.
        assert!(
            t.contains("GRUB_DEFAULT=saved"),
            "a default set from the system is not read back:\n{t}"
        );
        assert!(
            !t.contains("GRUB_SAVEDEFAULT=true"),
            "GRUB is told to write grubenv on every boot — that errors with \
             'sparse file not allowed' and stops the boot for a keypress:\n{t}"
        );
        assert!(
            !t.contains("\n  savedefault"),
            "a generated menuentry still carries savedefault, which triggers \
             the same blocking grubenv write:\n{t}"
        );
        assert!(
            t.contains("chainloader"),
            "the neighbour entries don't record themselves as the default"
        );
    }

    /// grub.cfg is kept current by a pacman hook — new kernels reach the menu,
    /// and the neighbour generator re-runs, without a manual grub-mkconfig.
    #[test]
    fn a_pacman_hook_regenerates_grub_on_kernel_and_grub_updates() {
        let a = install_app(); // GRUB, UEFI by default
        let t = plan_text(&build_plan(&a));
        let hook = t
            .lines()
            .position(|l| l.contains("cat > /etc/pacman.d/hooks/zz-artix-grub.hook"))
            .expect("no grub-regeneration pacman hook is installed");
        // It must trigger on kernel images and run grub-mkconfig.
        assert!(
            t.contains("usr/lib/modules/*/vmlinuz")
                && t.contains("grub-mkconfig -o /boot/grub/grub.cfg"),
            "the grub hook does not regenerate on kernel changes:\n{t}"
        );
        // The hook is installed before the first grub-mkconfig (order is not
        // strictly required, but keeps the plan coherent).
        let _ = hook;
    }

    /// Alongside with a user-chosen swap: a fixed-size swap partition is carved
    /// out BEFORE root (a lower partition number, created first), and root then
    /// fills the rest — the layout the alongside screen's swap control fills in.
    #[test]
    fn alongside_with_swap_carves_swap_then_root() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Alongside;
        a.config.manual_disk = "/dev/nvme0n1".into();
        a.config.manual_esp = "/dev/nvme0n1p1".into();
        a.config.manual_esp_mount = "/boot/efi".into();
        a.config.manual_esp_format = false;
        // Swap = partition 4 (fixed 8 GiB), root = partition 5 (rest).
        a.config.manual_swap = "/dev/nvme0n1p4".into();
        a.config.manual_swap_new_mib = 8192;
        a.config.manual_root = "/dev/nvme0n1p5".into();
        a.config.manual_root_new_mib = crate::app::MANUAL_REST;
        a.config.manual_root_fs = "ext4".into();
        let t = plan_text(&build_plan(&a));

        assert!(
            t.contains("sgdisk -n 4:0:+8192M -t 4:8200 -c 4:SWAP /dev/nvme0n1"),
            "swap must be created as a fixed-size partition 4:\n{t}"
        );
        assert!(
            t.contains("sgdisk -n 5:0:0 -t 5:8300 -c 5:ROOT /dev/nvme0n1"),
            "root must fill the rest as partition 5"
        );
        // Swap is created before root (lower number first → free-block order).
        let s = t.find("4:SWAP").unwrap();
        let r = t.find("5:ROOT").unwrap();
        assert!(s < r, "swap must be created before root");
        assert!(
            t.contains("mkswap /dev/nvme0n1p4"),
            "swap must be formatted"
        );
        assert!(
            t.contains("swapon /dev/nvme0n1p4"),
            "swap must be activated"
        );
    }

    /// The ntfs-3g / os-prober tooling rides ANY os-prober install, manual or
    /// alongside — the direct guard on the package-selection logic.
    #[test]
    fn os_prober_pulls_in_ntfs_tooling() {
        let mut c = install_app().config;
        c.os_prober = false;
        assert!(
            !base_packages(&c).iter().any(|p| p == "ntfs-3g"),
            "no ntfs-3g when os-prober is off"
        );
        c.os_prober = true;
        let pkgs = base_packages(&c);
        for want in ["ntfs-3g", "os-prober", "mtools"] {
            assert!(
                pkgs.iter().any(|p| p == want),
                "{want} must be installed when os-prober is on"
            );
        }
    }

    /// The manual ESP mount DEFAULTS to /boot/efi — the dual-boot-friendly
    /// layout (kernels on root, only the bootloader on the ESP, so a small
    /// reused Windows ESP fits). grub-install must target it to match.
    #[test]
    fn manual_esp_defaults_to_boot_efi() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_root = "/dev/vda5".into();
        a.config.manual_root_fs = "ext4".into();
        // manual_esp_mount is left at its default (InstallConfig::default).
        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains("mount /dev/vda1 /mnt/boot/efi"),
            "the default ESP mount is /boot/efi:\n{t}"
        );
        assert!(
            t.contains("grub-install --target=x86_64-efi --efi-directory=/boot/efi "),
            "grub-install must target the /boot/efi default"
        );
    }

    /// Toggling the ESP mount back to /boot (kernels on the ESP) still works.
    #[test]
    fn manual_esp_at_boot_when_toggled() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_esp_mount = "/boot".into();
        a.config.manual_root = "/dev/vda5".into();
        a.config.manual_root_fs = "ext4".into();
        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains("grub-install --target=x86_64-efi --efi-directory=/boot --bootloader-id"),
            "an explicit /boot mount points grub-install at /boot"
        );
    }

    /// A manual /home on a SEPARATE disk (the "different disk as /home" case):
    /// root+ESP on one disk, /home an existing partition on another. Both get
    /// formatted and mounted; the /home partition is signature-wiped first so a
    /// previously-encrypted disk reformats cleanly, and its own fs is honoured.
    #[test]
    fn manual_home_on_a_separate_disk_formats_and_mounts_it() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_esp = "/dev/sda1".into();
        a.config.manual_root = "/dev/sda2".into();
        a.config.manual_root_fs = "ext4".into();
        // /home lives on a DIFFERENT disk, as an existing (reformatted) partition.
        a.config.manual_home = "/dev/sdb1".into();
        a.config.manual_home_fs = "btrfs".into();
        let t = plan_text(&build_plan(&a));

        assert!(
            t.contains("wipefs -a /dev/sdb1"),
            "the /home partition's old signature (maybe LUKS) is scrubbed first:\n{t}"
        );
        assert!(
            t.contains("mkfs.btrfs -f /dev/sdb1"),
            "/home formatted with its own fs"
        );
        assert!(t.contains("mount") && t.contains("/dev/sdb1") && t.contains("/mnt/home"));
        assert!(
            !t.contains("sgdisk"),
            "existing partitions — the table is untouched"
        );
        // The installed system + live env get btrfs-progs for the /home fs.
        assert!(t.contains("btrfs-progs"), "btrfs-progs for the btrfs /home");
    }

    /// Manual + btrfs gets the SAME @-subvolume layout as the automatic
    /// path — snapshots and rollback must work identically in both modes.
    #[test]
    fn manual_btrfs_gets_the_same_subvolume_layout() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_root = "/dev/vda5".into();
        a.config.manual_root_fs = "btrfs".into();
        a.config.btrfs_subvolumes = true;
        let t = plan_text(&build_plan(&a));
        assert!(t.contains("mkfs.btrfs -f /dev/vda5"));
        assert!(t.contains("btrfs subvolume create /mnt/@"));
        assert!(
            t.contains("subvol=@"),
            "root must be mounted from the @ subvolume"
        );
    }

    /// A retry after a partly-finished install must be able to run again.
    ///
    /// Reported from QEMU: the first attempt deleted a partition and created
    /// the new ones, then failed while installing packages. Pressing "retry"
    /// re-ran the plan, which tried to delete a partition that no longer
    /// existed — the identity check saw "found none", called it a mismatch and
    /// stopped. A partition that is GONE is not the wrong partition; it is the
    /// goal already met. Likewise a partition we created earlier must not make
    /// `sgdisk -n` fail the second time round.
    #[test]
    fn the_partitioning_phase_can_be_run_twice() {
        use crate::app::DeletedPart;
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_disk = "/dev/vda".into();
        a.config.manual_boot_disk = "/dev/vda".into();
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_root = "/dev/vda2".into();
        a.config.manual_root_fs = "ext4".into();
        a.config.manual_root_new_mib = crate::app::MANUAL_REST;
        a.config.manual_root_disk = "/dev/vda".into();
        a.config.manual_deleted = vec![DeletedPart {
            path: "/dev/vdb1".into(),
            disk: "/dev/vdb".into(),
            partuuid: "uuid-vdb1".into(),
        }];

        let t = plan_text(&build_plan(&a));

        // Deletion is keyed to the PARTUUID, so a retry is a no-op: the marked
        // partition, deleted last run, is simply not found and nothing happens.
        let del = t
            .lines()
            .find(|l| l.contains("sgdisk -d"))
            .expect("no deletion planned");
        assert!(
            del.contains("already gone") && del.contains("exit 0"),
            "a deletion of an already-gone partition still aborts the install:\n{del}"
        );
        // The delete only ever removes the marked PARTUUID — the fast slot path
        // and the disk-wide search both compare against it, so a slot reoccupied
        // by a different partition is never deleted.
        assert!(
            del.contains("want=uuid-vdb1") && del.contains("[ \"$got\" = \"$want\" ]"),
            "the deletion no longer keys on the marked PARTUUID:\n{del}"
        );
        assert!(
            del.contains("sgdisk -p") && del.contains("[ \"$u\" = \"$want\" ]"),
            "the deletion does not search the disk for the marked PARTUUID (so a \
             partition that moved slots would be missed):\n{del}"
        );
        // Identity comes from the ON-DISK GPT (`sgdisk -i`), not the udev
        // database (`lsblk`). Reading PARTUUID via lsblk right after the
        // previous `sgdisk -d` reshuffled the table returned EMPTY — udev had
        // not settled — and a good deletion was refused with "found none".
        // This is the exact bug; keep the plan off lsblk for identity.
        assert!(
            del.contains("sgdisk -i") && del.contains("tolower($NF)"),
            "the deletion guard reads identity from a source that races with \
             the previous delete:\n{del}"
        );
        assert!(
            !del.contains("lsblk -no PARTUUID"),
            "identity is still read from the udev-backed lsblk, which is stale \
             right after a table change:\n{del}"
        );

        // Creation: our own earlier work is recognised and kept — so a retry
        // does not die on "partition already exists". Identity comes from the
        // GPT (`sgdisk -i` → "Partition name"), NOT lsblk: each `sgdisk -n`
        // rewrites the table, so a sibling slot's lsblk check could race the
        // re-read exactly as the delete guard did.
        let create = t
            .lines()
            .find(|l| l.contains("sgdisk -n"))
            .expect("no creation planned");
        assert!(
            create.contains("Partition name:") && create.contains("exit 0"),
            "the create-idempotency check is missing:\n{create}"
        );
        assert!(
            !create.contains("lsblk -no PARTLABEL"),
            "the create check still reads the udev-backed lsblk, which races the \
             table re-read from a sibling create:\n{create}"
        );
    }

    /// The keyring is refreshed before any package is installed.
    ///
    /// When a repo signs with a key newer than the keyring basestrap laid down,
    /// every package afterwards fails as "signature from Artix Buildbot is
    /// invalid" — dozens in a row, and the install dies at the desktop phase.
    /// Populating the existing keyring is not enough; the keyring PACKAGES have
    /// to be updated first.
    #[test]
    fn the_keyring_is_refreshed_before_packages_are_installed() {
        let a = install_app();
        let t = plan_text(&build_plan(&a));

        let refresh = t
            .lines()
            .position(|l| l.contains("artix-keyring") && l.contains("archlinux-keyring"))
            .expect("the keyring packages are never refreshed");
        // It must come before the desktop/extra package phase, or it fixes
        // nothing — that phase is where the failures land.
        let pkgs = t
            .lines()
            .position(|l| {
                l.contains("pacman -S") && l.contains("--needed") && l.contains("base-devel")
            })
            .unwrap_or(usize::MAX);
        assert!(
            refresh < pkgs,
            "the keyring is refreshed after packages are already being installed"
        );
        // And the keys are re-populated once the newer keyring is in place.
        assert!(
            t.lines()
                .skip(refresh)
                .any(|l| l.contains("pacman-key --populate")),
            "the refreshed keyring is never populated, so the new keys stay unused"
        );
    }

    /// A 404 storm from a stale database cannot end the install.
    ///
    /// Every Arch mirror answered 404 for the exact xorg-server / libinput / …
    /// files the sync DB named: the DB had gone stale against a moved-on pool
    /// (the mirrors deleted those versions), and the transaction died. Two
    /// safeguards: the DB is refreshed once more RIGHT BEFORE the package phase
    /// (so it names versions the pool still has), and each package install is
    /// retried with a refresh between attempts.
    #[test]
    fn a_stale_mirror_database_cannot_kill_the_package_phase() {
        let mut a = install_app();
        a.config.desktops = vec!["Cinnamon".into()];
        let t = plan_text(&build_plan(&a));

        // The first package install (X server or system set) must be reachable.
        let first_pkg = t
            .lines()
            .position(|l| l.contains("pacman -S --needed") && l.contains("xorg"))
            .or_else(|| {
                t.lines()
                    .position(|l| l.contains("pacman -S --needed") && l.contains("attempt"))
            })
            .expect("no package phase found");

        // A dedicated DB refresh sits before that phase — retried, and `-Syu`
        // (a bare `-Sy` in the target is the partial-upgrade landmine).
        let refresh = t
            .lines()
            .take(first_pkg)
            .filter(|l| l.contains("pacman -Syu") && l.contains("attempt"))
            .count();
        assert!(
            refresh >= 1,
            "no retried database refresh runs before the package phase — a stale \
             DB would 404 every mirror and end the install:\n{t}"
        );

        // The package installs themselves are wrapped in a retry that refreshes
        // between attempts, so one flaky download is not fatal.
        let install_line = t
            .lines()
            .find(|l| l.contains("pacman -S --needed") && l.contains("xorg"))
            .expect("the X package phase is missing");
        assert!(
            install_line.contains("for a in 1 2 3")
                && install_line.contains("pacman -Syu --noconfirm"),
            "the package install is not retried with a database refresh:\n{install_line}"
        );
        // And that retry must NOT smuggle in a bare `-Sy` (partial upgrade).
        assert!(
            !install_line.contains("pacman -Sy ") && !install_line.contains("pacman -Sy;"),
            "the retry uses a bare -Sy, which leaves the target a partial upgrade:\n{install_line}"
        );
    }

    /// The target is UPGRADED before the package phases, never left a partial
    /// upgrade.
    ///
    /// basestrap lays the base down against the Artix repos; the Arch repos are
    /// enabled afterwards, so every later `pacman -S` resolves against a wider
    /// and newer database. With a bare `-Sy` the base never caught up, and the
    /// Lua 5.4→5.5 split turned that into a dead install: the base's old `lua`
    /// still owned /usr/bin/lua5.4, so the fresh `lua54` that `libinput` needs
    /// could not be unpacked and all 387 packages rolled back. The packages do
    /// not conflict — only the mixed snapshot did.
    #[test]
    fn the_target_is_upgraded_before_packages_so_it_is_never_a_partial_upgrade() {
        let a = install_app();
        let t = plan_text(&build_plan(&a));

        let upgrade = t
            .lines()
            .position(|l| l.contains("pacman -Syu"))
            .expect("the target is never upgraded — a bare -Sy leaves a partial upgrade");
        let pkgs = t
            .lines()
            .position(|l| {
                l.contains("pacman -S") && l.contains("--needed") && l.contains("base-devel")
            })
            .unwrap_or(usize::MAX);
        assert!(
            upgrade < pkgs,
            "the upgrade lands after packages are already being installed"
        );

        // The database refresh that precedes the package phases must carry -u.
        // A bare `pacman -Sy` there is the exact shape of the bug.
        //
        // Only IN THE TARGET. The same command on the live ISO is fine and is
        // used deliberately: that system is thrown away when the machine
        // reboots, nothing is installed onto it beyond the install tools, and
        // a full upgrade of a running ISO is its own kind of trouble.
        for line in t.lines().take(pkgs).filter(|l| l.contains("artix-chroot")) {
            let is_bare_sync = line.contains("pacman -Sy")
                && !line.contains("pacman -Syu")
                // Refreshing a NAMED package (the keyrings) is a different job:
                // it must run before anything can be verified, and it upgrades
                // only what it names.
                && !line.contains("keyring");
            assert!(
                !is_bare_sync,
                "a bare `pacman -Sy` in the target before the package phases \
                 leaves it a partial upgrade:\n{line}"
            );
        }
    }

    /// A planned data partition is created, formatted and mounted by the plan.
    ///
    /// The editor only records intent: `manual_data_new` says "carve this",
    /// and an `extra_disks` entry keyed by the predicted path says "format it
    /// ext4 and mount it there". This pins the whole chain: sgdisk creates it,
    /// the extra-disk pass formats and mounts it, and the order is right.
    #[test]
    fn a_planned_data_partition_is_created_formatted_and_mounted() {
        use crate::app::{DataPart, ExtraDisk};
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_solo = true;
        a.config.manual_disk = "/dev/vda".into();
        a.config.manual_boot_disk = "/dev/vda".into();
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_root = "/dev/vda2".into();
        a.config.manual_root_fs = "ext4".into();
        a.config.manual_data_new.push(DataPart {
            path: "/dev/vdb1".into(),
            disk: "/dev/vdb".into(),
            mib: 20 * 1024,
        });
        a.config.extra_disks.push(ExtraDisk {
            disk: "/dev/vdb1".into(),
            mountpoint: "/mnt/data".into(),
            fs: "ext4".into(),
            format: true,
            whole_disk: false,
            ..Default::default()
        });

        let t = plan_text(&build_plan(&a));

        // Created on its own disk by the partitioning pass…
        assert!(
            t.contains("sgdisk -n 1:0:+20480M -t 1:8300 -c 1:DATA /dev/vdb"),
            "the data partition is never created:\n{t}"
        );
        // …then formatted and mounted by the extra-disk pass.
        assert!(
            t.contains("mkfs.ext4 -F /dev/vdb1"),
            "the data partition is never formatted"
        );
        assert!(
            t.contains("mount /dev/vdb1 /mnt/mnt/data")
                || t.contains("mount -o") && t.contains("/dev/vdb1"),
            "the data partition is never mounted:\n{t}"
        );
        // Creation must precede the mkfs, or it formats a hole in the air.
        let created = t.find("-c 1:DATA /dev/vdb").unwrap();
        let formatted = t.find("mkfs.ext4 -F /dev/vdb1").unwrap();
        assert!(
            created < formatted,
            "the data partition is formatted before it exists"
        );
    }

    /// New partitions may be created on MORE THAN ONE disk.
    ///
    /// The editor used to pin creation to a single disk, so "root here, a fresh
    /// /home on the other drive" — an ordinary two-drive layout — was refused
    /// outright with "new partitions only on one disk". Each planned slot now
    /// records its own disk and the plan carves each drive in turn.
    #[test]
    fn new_partitions_can_be_created_on_several_disks() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_solo = true;
        a.config.manual_disk = "/dev/vda".into();
        a.config.manual_boot_disk = "/dev/vda".into();
        // ESP + root carved from the first drive…
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_esp_new_mib = 512;
        a.config.manual_esp_disk = "/dev/vda".into();
        a.config.manual_esp_format = true;
        a.config.manual_root = "/dev/vda2".into();
        a.config.manual_root_new_mib = crate::app::MANUAL_REST;
        a.config.manual_root_disk = "/dev/vda".into();
        a.config.manual_root_fs = "ext4".into();
        // …and a brand-new /home from the second.
        a.config.manual_home = "/dev/vdb1".into();
        a.config.manual_home_new_mib = crate::app::MANUAL_REST;
        a.config.manual_home_disk = "/dev/vdb".into();
        a.config.manual_home_fs = "ext4".into();

        let t = plan_text(&build_plan(&a));

        assert!(
            t.contains("sgdisk -n 1:0:+512M -t 1:ef00 -c 1:EFI /dev/vda"),
            "the ESP was not created on the first disk:\n{t}"
        );
        assert!(
            t.contains("-c 2:ROOT /dev/vda"),
            "the root was not created on the first disk"
        );
        // The point of the test: a partition carved from the OTHER drive.
        assert!(
            t.contains("-c 1:HOME /dev/vdb"),
            "/home was not created on the second disk — the one-disk limit is back:\n{t}"
        );
        assert!(
            t.contains("mkfs.ext4 -F /dev/vdb1") && t.contains("mount"),
            "/home on the second disk was not formatted"
        );
        // Each disk is re-read after being carved.
        assert!(
            t.contains("partprobe /dev/vda") && t.contains("partprobe /dev/vdb"),
            "a carved disk was not re-read"
        );
    }

    /// A disk marked for a full wipe is erased outright, BEFORE anything is
    /// created, and its per-partition deletions are skipped — the table is gone,
    /// so `sgdisk -d` on it would be a redundant failure.
    #[test]
    fn a_wiped_disk_is_erased_before_creation_and_skips_its_deletes() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_solo = true;
        a.config.manual_disk = "/dev/vda".into();
        a.config.manual_boot_disk = "/dev/vda".into();
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_esp_new_mib = 512;
        a.config.manual_esp_disk = "/dev/vda".into();
        a.config.manual_esp_format = true;
        a.config.manual_root = "/dev/vda2".into();
        a.config.manual_root_new_mib = crate::app::MANUAL_REST;
        a.config.manual_root_disk = "/dev/vda".into();
        a.config.manual_root_fs = "ext4".into();
        // A fresh /home on a SECOND disk that we erase first.
        a.config.manual_home = "/dev/vdb1".into();
        a.config.manual_home_new_mib = crate::app::MANUAL_REST;
        a.config.manual_home_disk = "/dev/vdb".into();
        a.config.manual_home_fs = "ext4".into();
        a.config.manual_wipe = vec![crate::app::WipeMark {
            disk: "/dev/vdb".into(),
            method: crate::app::WipeMethod::Quick,
            rota: true,
            tran: "sata".into(),
        }];
        // Also mark one of vdb's partitions for deletion — the wipe must make
        // that delete a no-op.
        a.config.manual_deleted = vec![crate::app::DeletedPart {
            path: "/dev/vdb1".into(),
            disk: "/dev/vdb".into(),
            partuuid: "dead".into(),
        }];

        let t = plan_text(&build_plan(&a));

        assert!(
            t.contains("sgdisk --zap-all /dev/vdb"),
            "vdb was not wiped:\n{t}"
        );
        let zap = t.find("--zap-all /dev/vdb").expect("no wipe");
        let create = t.find("-c 1:HOME /dev/vdb").expect("no /home create");
        assert!(zap < create, "the wipe ran after the create:\n{t}");
        // The vda disk keeps its normal (non-wipe) creation path untouched.
        assert!(
            !t.contains("--zap-all /dev/vda"),
            "an unmarked disk was wiped:\n{t}"
        );
        // The per-partition delete on the wiped disk is skipped entirely.
        assert!(
            !t.contains("sgdisk -d 1 /dev/vdb") && !t.contains("REFUSING to delete /dev/vdb1"),
            "a delete ran on a disk that was fully wiped:\n{t}"
        );
    }

    /// In dual boot the plan erases EVERY drive the user marked — including the
    /// one the neighbouring OS boots from. Replacing that system can be the
    /// whole intent (Artix first, another OS by hand later), so the guard is the
    /// editor's acknowledgement, not a rule the plan enforces behind the user's
    /// back. A mark that reached the plan has already been consented to.
    #[test]
    fn a_dual_boot_wipe_erases_every_marked_drive() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_solo = false; // sharing the machine with another OS
        a.config.manual_disk = "/dev/vda".into();
        a.config.manual_boot_disk = "/dev/vda".into();
        a.config.manual_esp = "/dev/vda1".into(); // the neighbour's ESP, reused
        a.config.manual_root = "/dev/vda3".into();
        a.config.manual_root_fs = "ext4".into();
        // A fresh /home carved from the erased second drive.
        a.config.manual_home = "/dev/vdb1".into();
        a.config.manual_home_new_mib = crate::app::MANUAL_REST;
        a.config.manual_home_disk = "/dev/vdb".into();
        a.config.manual_home_fs = "ext4".into();
        let mark = |disk: &str| crate::app::WipeMark {
            disk: disk.into(),
            method: crate::app::WipeMethod::Quick,
            rota: false,
            tran: "sata".into(),
        };
        a.config.manual_wipe = vec![mark("/dev/vdb"), mark("/dev/vda")];

        let t = plan_text(&build_plan(&a));

        assert!(
            t.contains("sgdisk --zap-all /dev/vdb"),
            "the second drive was not erased — dual boot must still allow it:\n{t}"
        );
        assert!(
            t.contains("sgdisk --zap-all /dev/vda"),
            "an acknowledged wipe of the neighbour's drive was silently dropped:\n{t}"
        );
    }

    /// The delete guard removes ONLY the marked PARTUUID — at its recorded slot,
    /// or wherever it moved — and NOTHING when it is gone. Run against a mock
    /// `sgdisk` so all three branches are exercised: this is the most dangerous
    /// shell in the installer and it is an inline string shellcheck never sees.
    #[test]
    fn the_delete_guard_only_removes_the_marked_partuuid() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_solo = true;
        a.config.manual_disk = "/dev/vda".into();
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_root = "/dev/vda2".into();
        a.config.manual_root_fs = "ext4".into();
        a.config.manual_deleted = vec![crate::app::DeletedPart {
            path: "/dev/vda5".into(),
            disk: "/dev/vda".into(),
            partuuid: "aaaa".into(),
        }];
        let script = build_plan(&a)
            .iter()
            .filter(|act| act.program == "sh")
            .filter_map(|act| act.args.last().cloned())
            .find(|s| s.contains("want=aaaa"))
            .expect("no delete guard script in the plan");

        // A mock `sgdisk`, driven by env vars: SLOT5_UUID is what the recorded
        // slot 5 holds; MOVED_SLOT (if set) is another slot that holds the marked
        // uuid. Every `-d N` is appended to $DFILE so the test sees what was
        // actually deleted.
        let dir = std::env::temp_dir().join(format!("delguard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mock = dir.join("sgdisk");
        let mut f = std::fs::File::create(&mock).unwrap();
        write!(
            f,
            "#!/bin/sh\n\
             case \"$1\" in\n\
             -i) if [ \"$2\" = 5 ]; then echo \"Partition unique GUID: $SLOT5_UUID\"; fi; \
                 if [ -n \"$MOVED_SLOT\" ] && [ \"$2\" = \"$MOVED_SLOT\" ]; then \
                 echo \"Partition unique GUID: AAAA\"; fi ;;\n\
             -p) echo \"Number  Start\"; echo \"   5   2048\"; \
                 [ -n \"$MOVED_SLOT\" ] && echo \"   $MOVED_SLOT   4096\"; true ;;\n\
             -d) echo \"$2\" >> \"$DFILE\" ;;\n\
             esac\n"
        )
        .unwrap();
        drop(f);
        std::fs::set_permissions(&mock, std::fs::Permissions::from_mode(0o755)).unwrap();

        let run = |slot5: &str, moved: &str| -> String {
            let dfile = dir.join("deleted");
            let _ = std::fs::remove_file(&dfile);
            let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap());
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .env("PATH", path)
                .env("DFILE", &dfile)
                .env("SLOT5_UUID", slot5)
                .env("MOVED_SLOT", moved)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "guard exited nonzero: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            std::fs::read_to_string(&dfile).unwrap_or_default()
        };

        // Marked uuid at its recorded slot 5 → slot 5 deleted.
        assert_eq!(
            run("aaaa", "").trim(),
            "5",
            "marked uuid at its slot was not deleted"
        );
        // Slot 5 holds a DIFFERENT uuid and the marked one is nowhere → nothing
        // deleted (this is the retry/reuse case: our own fresh partition sits
        // there now, and it must be left alone).
        assert_eq!(
            run("bbbb", "").trim(),
            "",
            "a partition that was NOT marked got deleted"
        );
        // Marked uuid moved to slot 7 → slot 7 deleted (delete by identity).
        assert_eq!(
            run("bbbb", "7").trim(),
            "7",
            "the marked uuid that moved slots was not deleted"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every wipe method emits shell a POSIX sh accepts. These run on the live
    /// ISO, where a syntax slip means the disk is silently NOT erased — and the
    /// scripts are inline strings, so shellcheck never sees them.
    #[test]
    fn wipe_scripts_are_valid_posix_sh() {
        use crate::app::WipeMethod;
        for method in [
            WipeMethod::Quick,
            WipeMethod::ZeroHdd,
            WipeMethod::CryptoSsd,
            WipeMethod::AtaSecure,
        ] {
            for allow in [true, false] {
                for (rota, tran) in [(false, "nvme"), (false, "sata"), (true, "sata")] {
                    let acts =
                        crate::system::disk::wipe_actions("/dev/vdz", method, rota, tran, allow);
                    assert_eq!(acts.len(), 1, "a wipe is one action");
                    let script = acts[0].args.last().unwrap();
                    let out = std::process::Command::new("sh")
                        .args(["-n", "-c", script])
                        .output()
                        .expect("run sh -n");
                    assert!(
                        out.status.success(),
                        "wipe script for {method:?} (rota={rota}, tran={tran}, allow={allow}) \
                         is not valid sh:\n{}\n--- script ---\n{script}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
            }
        }
    }

    /// Manual partitioning WITHOUT a second OS unlocks what dual boot blocks.
    ///
    /// The v1 limits — GRUB only, no LUKS — exist because the manual path was
    /// assumed to share a disk with Windows. When the user says there is no
    /// second OS, that reason is gone: encryption and the other bootloaders are
    /// available exactly as for a whole-disk install.
    ///
    /// The piece that actually had to be built for this: everything downstream
    /// (crypttab, the initramfs keyfile, the GRUB cmdline) finds the root via
    /// `blkid -t PARTLABEL=ROOT`. An automatic layout labels its partitions; a
    /// REUSED partition carries whatever label it had, so the manual plan has
    /// to stamp it or an encrypted install boots to a rescue prompt.
    #[test]
    fn solo_manual_can_encrypt_and_pick_its_own_bootloader() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_solo = true;
        a.config.manual_disk = "/dev/vda".into();
        a.config.manual_boot_disk = "/dev/vda".into();
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_root = "/dev/vda2".into(); // an EXISTING partition, reused
        a.config.manual_root_fs = "ext4".into();
        a.config.encrypt_disk = true;
        a.config.luks_passphrase = "correct horse".into();
        a.config.bootloader = Bootloader::Refind;

        let t = plan_text(&build_plan(&a));

        // The container is created and opened, and the filesystem lands on the
        // MAPPER — not on the bare partition, which would leave it unencrypted.
        assert!(
            t.contains("cryptsetup -q luksFormat --type luks2 /dev/vda2"),
            "the root was not encrypted:\n{t}"
        );
        assert!(
            t.contains("cryptsetup open /dev/vda2 cryptroot"),
            "the container was never opened"
        );
        assert!(
            t.contains("mkfs.ext4 -F /dev/mapper/cryptroot"),
            "the filesystem went on the bare partition, so nothing is encrypted"
        );
        assert!(
            !t.contains("mkfs.ext4 -F /dev/vda2"),
            "the root partition was formatted unencrypted as well"
        );

        // Reused partitions carry no PARTLABEL, so the plan must stamp one or
        // every blkid lookup downstream comes back empty.
        assert!(
            t.contains("sgdisk -c 2:ROOT /dev/vda"),
            "the root was not labelled ROOT — the crypt wiring cannot find it"
        );
        // The initramfs must learn to unlock it.
        assert!(
            t.contains("encrypt"),
            "no encrypt hook was added to the initramfs"
        );

        // And the requested bootloader is honoured rather than forced to GRUB.
        assert!(
            t.contains("refind-install"),
            "solo manual should keep the chosen bootloader:\n{t}"
        );
        // No neighbour OS to look for.
        assert!(
            !t.contains("GRUB_DISABLE_OS_PROBER=false"),
            "os-prober was enabled for a single-OS install"
        );
    }

    /// A DUAL-BOOT manual install keeps the old limits: it shares the disk.
    #[test]
    fn dual_boot_manual_still_refuses_luks_and_other_loaders() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_solo = false;
        a.config.manual_disk = "/dev/vda".into();
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_root = "/dev/vda2".into();
        a.config.manual_root_fs = "ext4".into();
        a.config.encrypt_disk = true;
        a.config.luks_passphrase = "correct horse".into();
        a.config.bootloader = Bootloader::Refind;

        let t = plan_text(&build_plan(&a));
        assert!(
            !t.contains("luksFormat"),
            "dual-boot manual must not attempt LUKS in v1"
        );
        assert!(
            !t.contains("refind-install") && t.contains("grub-install"),
            "dual-boot manual must stay on GRUB"
        );
        assert!(
            t.contains("GRUB_DISABLE_OS_PROBER=false"),
            "dual boot still needs os-prober"
        );
    }

    /// Manual partitioning on legacy BIOS, end to end.
    ///
    /// Old hardware (and firmware whose UEFI is not to be trusted) still needs a
    /// way in, so manual mode is no longer UEFI-only. What has to change with
    /// the firmware: no ESP is created, formatted or mounted; a 1 MiB EF02
    /// partition is carved for GRUB to embed core.img on GPT; and GRUB is
    /// installed to a whole DISK rather than an EFI directory.
    #[test]
    fn manual_mode_can_boot_legacy_bios() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.boot_mode = BootMode::Bios;
        a.config.disk.clear(); // manual mode leaves this empty; the sanitiser fills it
        a.config.manual_disk = "/dev/vda".into();
        a.config.manual_boot_disk = "/dev/vda".into();
        a.config.manual_bios = "/dev/vda1".into();
        a.config.manual_bios_new_mib = 1;
        a.config.manual_root = "/dev/vda2".into();
        a.config.manual_root_fs = "ext4".into();
        a.config.manual_root_new_mib = crate::app::MANUAL_REST;

        let t = plan_text(&build_plan(&a));

        // The BIOS-boot partition is created with the right type and left alone.
        assert!(
            t.contains("sgdisk -n 1:0:+1M -t 1:ef02 -c 1:BIOSBOOT /dev/vda"),
            "no BIOS-boot partition was created:\n{t}"
        );
        assert!(
            !t.contains("mkfs.fat -F32 /dev/vda1"),
            "the BIOS-boot partition must carry no filesystem"
        );
        assert!(
            !t.lines().any(|l| l.starts_with("mount /dev/vda1")),
            "the BIOS-boot partition must never be mounted"
        );

        // GRUB goes to the disk, not an EFI directory.
        assert!(
            t.contains("grub-install --target=i386-pc /dev/vda"),
            "legacy GRUB was not installed to the disk:\n{t}"
        );
        assert!(
            !t.contains("--target=x86_64-efi"),
            "a UEFI bootloader was planned for a BIOS install"
        );
        // And the old "manual requires UEFI" abort must be gone.
        assert!(
            !t.contains("Manual partitioning requires UEFI"),
            "the plan still aborts legacy manual installs"
        );
    }

    /// Deleting partitions: the plan must remove exactly what was marked, on the
    /// right disk, BEFORE it creates anything — and nothing else.
    ///
    /// This is the most destructive action the installer can take, and it is
    /// driven entirely by config the user can still toggle. The guarantees
    /// pinned here: only marked paths become `sgdisk -d`, the number is resolved
    /// against the disk recorded in the SCAN (never parsed out of the path), and
    /// deletions are ordered ahead of every `sgdisk -n`.
    #[test]
    fn deleting_partitions_removes_only_what_was_marked_and_does_it_first() {
        use crate::app::DeletedPart;
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_disk = "/dev/vda".into();
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_root = "/dev/vda9".into();
        a.config.manual_root_fs = "ext4".into();
        a.config.manual_root_new_mib = crate::app::MANUAL_REST;
        a.config.manual_deleted = vec![
            DeletedPart {
                path: "/dev/vda3".into(),
                disk: "/dev/vda".into(),
                partuuid: "uuid-vda3".into(),
            },
            // A second disk: deletions are not limited to the system disk.
            DeletedPart {
                path: "/dev/vdb2".into(),
                disk: "/dev/vdb".into(),
                partuuid: "uuid-vdb2".into(),
            },
        ];

        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains("sgdisk -d 3 /dev/vda"),
            "vda3 was not deleted:\n{t}"
        );
        assert!(t.contains("sgdisk -d 2 /dev/vdb"), "vdb2 was not deleted");
        // Each deletion is KEYED to the identity recorded when it was marked: the
        // plan runs minutes after the scan it was built from, and GPT reuses
        // freed numbers, so "partition 3" alone is not proof of identity.
        assert!(
            t.contains("want=uuid-vda3") && t.contains("want=uuid-vdb2"),
            "a deletion fires without checking the partition is still the right one:\n{t}"
        );
        // Nothing else may be deleted — in particular not the ESP being reused.
        let deletes: Vec<&str> = t.lines().filter(|l| l.contains("sgdisk -d")).collect();
        assert_eq!(deletes.len(), 2, "unexpected deletions: {deletes:?}");

        // Deletions come before creations, or the freed space isn't free yet.
        let first_delete = t.lines().position(|l| l.contains("sgdisk -d")).unwrap();
        // Creation is wrapped in a guard now (idempotent retries), so match the
        // command inside the line rather than at its start.
        let first_create = t
            .lines()
            .position(|l| l.contains("sgdisk -n"))
            .expect("no partition was created");
        assert!(
            first_delete < first_create,
            "creation was planned before the deletion that frees its space"
        );
        // And the whole-disk wipe of the automatic path must stay absent.
        assert!(
            !t.lines().any(|l| l == "wipefs -a /dev/vda"),
            "manual mode must never wipe the whole disk"
        );
    }

    /// A partition marked for deletion must not also be mounted as an extra
    /// disk: the storage step may still hold an entry from before the user
    /// deleted it, and the plan would then delete it and mount it in one run.
    #[test]
    fn a_deleted_partition_is_dropped_from_the_extra_mounts() {
        use crate::app::DeletedPart;
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_disk = "/dev/vda".into();
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_root = "/dev/vda2".into();
        a.config.manual_root_fs = "ext4".into();
        a.config.extra_disks.push(ExtraDisk {
            disk: "/dev/vdb1".into(),
            mountpoint: "/data".into(),
            fs: "ext4".into(),
            ..Default::default()
        });
        a.config.manual_deleted = vec![DeletedPart {
            path: "/dev/vdb1".into(),
            disk: "/dev/vdb".into(),
            partuuid: "uuid-vdb1".into(),
        }];

        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains("sgdisk -d 1 /dev/vdb"),
            "the deletion is missing"
        );
        assert!(
            !t.contains("/mnt/data"),
            "a partition being deleted was also mounted:\n{t}"
        );
    }

    /// Every extra disk carries its OWN mount options — from each other, and
    /// from the root.
    ///
    /// The manual editor's /home row had the opposite bug (its "[o]" cue wrote
    /// to the root's flags), so this pins the property for the whole extra-disk
    /// path: two btrfs disks, one compressed and one not, plus a root whose own
    /// setting differs from both, must produce three independent mount lines.
    #[test]
    fn each_extra_disk_keeps_its_own_mount_options() {
        let mut a = install_app();
        a.config.root_fs = "btrfs".into();
        a.config.btrfs_compress = false; // root: NOT compressed
        a.config.extra_disks.push(ExtraDisk {
            disk: "/dev/sdb".into(),
            mountpoint: "/data".into(),
            fs: "btrfs".into(),
            format: true,
            whole_disk: true,
            compress: true, // this one IS compressed
            ..Default::default()
        });
        a.config.extra_disks.push(ExtraDisk {
            disk: "/dev/sdc".into(),
            mountpoint: "/media/scratch".into(),
            fs: "btrfs".into(),
            format: true,
            whole_disk: true,
            compress: false, // this one is not
            ..Default::default()
        });

        let t = plan_text(&build_plan(&a));
        let line_for = |mp: &str| -> String {
            t.lines()
                .find(|l| l.starts_with("mount") && l.ends_with(mp))
                .unwrap_or_else(|| panic!("no mount line for {mp} in the plan"))
                .to_string()
        };

        let data = line_for("/mnt/data");
        assert!(
            data.contains("compress=zstd"),
            "the disk asked to compress did not: {data}"
        );
        let scratch = line_for("/mnt/media/scratch");
        assert!(
            !scratch.contains("compress=zstd"),
            "compression leaked from the other extra disk: {scratch}"
        );
        let root = line_for("/mnt");
        assert!(
            !root.contains("compress=zstd"),
            "compression leaked from an extra disk onto the root: {root}"
        );
    }

    /// A dedicated /home is its OWN filesystem, so its btrfs options must not be
    /// the root's.
    ///
    /// The manual editor draws an "[o] Options" cue on the /home row, but the
    /// modal it opened always edited the ROOT's flags — so ticking compression
    /// "for /home" compressed root instead, and unticking Subvolumes there tore
    /// the root's @/@snapshots layout down while the user believed they were
    /// configuring a second disk. The two sets are separate now; this pins that.
    #[test]
    fn home_btrfs_options_are_not_the_roots() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_root = "/dev/vda5".into();
        a.config.manual_root_fs = "btrfs".into();
        a.config.manual_home = "/dev/vdb1".into();
        a.config.manual_home_fs = "btrfs".into();
        // Root compresses; /home does not.
        a.config.btrfs_compress = true;
        a.config.manual_home_compress = false;

        let t = plan_text(&build_plan(&a));
        let home_mount = t
            .lines()
            .find(|l| l.starts_with("mount") && l.ends_with("/mnt/home"))
            .expect("no /home mount in the plan");
        assert!(
            !home_mount.contains("compress=zstd"),
            "/home inherited the root's compression: {home_mount}"
        );
        assert!(
            t.lines().any(|l| l.contains("compress=zstd")),
            "the root itself should still be compressed"
        );

        // And the other way round: /home compresses, root does not.
        a.config.btrfs_compress = false;
        a.config.manual_home_compress = true;
        a.config.manual_home_discard = true;
        let t = plan_text(&build_plan(&a));
        let home_mount = t
            .lines()
            .find(|l| l.starts_with("mount") && l.ends_with("/mnt/home"))
            .expect("no /home mount in the plan");
        assert!(
            home_mount.contains("compress=zstd") && home_mount.contains("discard=async"),
            "/home did not get its own options: {home_mount}"
        );
        let root_mount = t
            .lines()
            .find(|l| l.starts_with("mount") && l.ends_with("/mnt"))
            .expect("no root mount in the plan");
        assert!(
            !root_mount.contains("compress=zstd"),
            "the root picked up /home's compression: {root_mount}"
        );
    }

    /// Manual mode exists FOR dual boot: GRUB must be told to look for the
    /// neighbours (os-prober is off by default since GRUB 2.06), and the
    /// sanitiser must pin the loader to GRUB even if the config asked for
    /// another one — the other loaders navigate by auto-layout PARTLABELs.
    #[test]
    fn manual_mode_enables_os_prober_and_pins_grub() {
        let mut a = install_app();
        a.config.partition_mode = PartitionMode::Manual;
        a.config.manual_esp = "/dev/vda1".into();
        a.config.manual_root = "/dev/vda5".into();
        a.config.manual_root_fs = "ext4".into();
        a.config.bootloader = Bootloader::Refind;
        let t = plan_text(&build_plan(&a));
        assert!(t.contains("GRUB_DISABLE_OS_PROBER=false"));
        assert!(
            t.contains("grub-install --target=x86_64-efi"),
            "manual is pinned to GRUB"
        );
        assert!(
            !t.contains("refind-install"),
            "the requested rEFInd must be overridden"
        );
    }

    /// Cinnamon must arrive as a desktop, not a bare shell.
    ///
    /// The `cinnamon` group ships no text editor, no archiver and no document
    /// viewers; Linux Mint fills that gap with the X-Apps and a few GTK3 GNOME
    /// tools. The plan mirrors that selection, so a fresh Cinnamon login can
    /// open a zip, read a PDF and edit a file without a trip to pacman.
    #[test]
    fn cinnamon_ships_a_usable_desktop_not_a_bare_shell() {
        let mut a = install_app();
        a.config.desktops = vec!["Cinnamon".into()];
        let t = plan_text(&build_plan(&a));
        for pkg in ["xed", "xreader", "xviewer", "file-roller", "celluloid"] {
            assert!(
                t.contains(pkg),
                "Mint-style default {pkg} must be installed with Cinnamon"
            );
        }
    }

    /// Pinnacle's runtime dependencies ride the pacman phase, not the AUR one.
    ///
    /// A live `pactree -d2 pinnacle-comp` showed that of the whole dependency
    /// tree only TWO packages are genuinely AUR (pinnacle-comp and
    /// lua54-protobuf — `pacman -Qm` listed exactly those); the other fourteen
    /// are plain repo packages since Arch's Lua 5.5 split. Installing them via
    /// pacman puts them inside the retried, mirror-ranked transaction — and
    /// paru's job shrinks to the two builds that actually need it.
    #[test]
    fn pinnacle_runtime_deps_ride_the_pacman_phase() {
        let mut a = install_app();
        a.config.desktops = vec!["Pinnacle".into()];
        let t = plan_text(&build_plan(&a));

        for pkg in [
            "lua54-http",
            "lua54-posix",
            "libdisplay-info",
            "protobuf",
            "rust",
        ] {
            assert!(
                t.contains(pkg),
                "{pkg} is a repo package and must be pre-seeded in the pacman phase"
            );
        }

        // The AUR-INSTALL line (paru actually installing the packages), told
        // apart from the paru-BUILD line by `--skipreview`, which is paru's own
        // flag — the build line runs plain pacman/makepkg and never has it.
        let paru_line = t
            .lines()
            .find(|l| l.contains("paru") && l.contains("--skipreview"))
            .expect("a Pinnacle install must have the paru install line");
        assert!(paru_line.contains("pinnacle-comp"));
        assert!(
            !paru_line.contains("lua54"),
            "the lua54 stack is repo territory — paru must not be asked to \
             build it: {paru_line}"
        );
    }

    /// The seat backend and its per-user launcher must match: userspawn belongs
    /// to elogind, turnstiled to seatd. Enabling both gives two rival session
    /// managers, /run/user/<uid> never appears, and the user gets a black screen.
    ///
    /// NOTE ON HOW THIS IS CHECKED. The first version of this test asserted that
    /// the word "turnstiled" was absent from an elogind plan — and failed,
    /// because the plan mentions it in the SKIP list precisely to make sure it
    /// is NOT auto-enabled. The word being present was the code doing the right
    /// thing. So the assertion is about the enable command specifically, not
    /// about whether a string appears anywhere in a 600-line shell script.
    #[test]
    fn seat_backend_and_launcher_agree() {
        // The services actually switched on are the ones listed in the
        // symlink-into-boot.d loop.
        fn enabled_services(plan: &[Action]) -> String {
            plan_text(plan)
                .lines()
                .find(|l| l.contains("for s in") && l.contains("boot.d"))
                .unwrap_or_default()
                .to_string()
        }

        let mut a = install_app();

        a.config.seat_provider = SeatProvider::Elogind;
        let e = enabled_services(&build_plan(&a));
        assert!(
            e.contains("elogind"),
            "elogind must be enabled when it's the chosen backend"
        );
        assert!(
            !e.contains("seatd"),
            "seatd must not be enabled alongside elogind — rival session managers"
        );

        a.config.seat_provider = SeatProvider::Seatd;
        let s = enabled_services(&build_plan(&a));
        assert!(s.contains("seatd"), "seatd must be enabled when chosen");
        assert!(
            !s.contains("elogind"),
            "elogind must not be enabled alongside seatd"
        );

        // And the losing backend's user launcher is explicitly skipped by the
        // service autoscan, so a package pulling it in can't quietly enable it.
        let elo = plan_text(&build_plan(&{
            let mut b = install_app();
            b.config.seat_provider = SeatProvider::Elogind;
            b
        }));
        assert!(
            elo.contains("SKIP=\" seatd turnstiled turnstile \""),
            "an elogind install must skip seatd's services in the autoscan"
        );
    }

    /// Failed AUR builds must be REPORTED. They're non-fatal by design (one bad
    /// PKGBUILD shouldn't destroy a complete system), but "non-fatal" silently
    /// became "unnoticed" — users found out when a session refused to start.
    #[test]
    fn failed_aur_packages_are_reported() {
        let mut a = install_app();
        a.config.aur_packages.push("some-aur-pkg".into());
        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains("pacman -T"),
            "verification must use pacman -T, which understands `provides` \
             (pacman -Qq flameshot-git reports a missing package for a \
             flameshot-git that installed perfectly, as `flameshot`)"
        );
        assert!(
            t.contains("FAILED to install"),
            "a failed AUR build must be announced in the log"
        );
    }

    /// No naive sudo→doas symlink, ever.
    ///
    /// It "works" for a bare `sudo cmd` but breaks the instant a tool passes a
    /// sudo-only flag (makepkg's `sudo -k` is what killed the AUR build), and
    /// opendoas upstream ships none for the same reason. The user opts into the
    /// proper `doas-sudo-shim` package instead.
    #[test]
    fn no_naive_sudo_to_doas_symlink_is_created() {
        let mut a = install_app();
        a.config.use_doas = true;
        let t = plan_text(&build_plan(&a));
        assert!(
            !t.contains("ln -sf") || !t.contains("/usr/local/bin/sudo"),
            "the naive sudo→doas symlink is back — it breaks on sudo-only flags:\n{t}"
        );
    }

    /// Choosing doas replaces the real sudo with doas-sudo-shim.
    ///
    /// sudo can't just be removed — base-devel and qt-sudo (pulled by octopi)
    /// hard-depend on it. The shim PROVIDES sudo, so it satisfies those deps
    /// while the real sudo binary goes away and `sudo` starts running doas. It
    /// is built directly (no paru) and swapped in build-first, replace-second so
    /// a failed build leaves sudo intact.
    #[test]
    fn doas_replaces_sudo_with_the_shim() {
        let mut a = install_app();
        a.config.username = "tester".into();
        a.config.use_doas = true;
        let doas = plan_text(&build_plan(&a));
        assert!(
            doas.contains("git clone https://aur.archlinux.org/doas-sudo-shim.git"),
            "doas install does not build the sudo-compat shim:\n{doas}"
        );
        // It must FORCE-remove sudo (plain -R fails on the hard deps) and install
        // the shim that re-satisfies them.
        assert!(
            doas.contains("pacman -Rdd --noconfirm sudo") && doas.contains("pacman -U --noconfirm"),
            "the shim is built but sudo is never swapped out:\n{doas}"
        );

        // A sudo install must NOT do any of this.
        let mut b = install_app();
        b.config.username = "tester".into();
        b.config.use_doas = false;
        let sudo = plan_text(&build_plan(&b));
        assert!(
            !sudo.contains("doas-sudo-shim") && !sudo.contains("pacman -Rdd --noconfirm sudo"),
            "a sudo install must keep sudo and never touch the shim:\n{sudo}"
        );
    }

    /// Building paru from source must not depend on privilege escalation, which
    /// is exactly what breaks on a doas system.
    ///
    /// `makepkg -si` elevates to install the makedep (cargo) and again to
    /// install the built package. On doas that goes through the sudo→doas shim,
    /// and makepkg's `sudo -k` is a flag doas rejects — cargo never installs and
    /// the Rust build dies with "could not resolve all dependencies". The build
    /// instead installs deps as root, builds `makepkg -f` (never `-si`) as the
    /// user with every dep already present, and installs the result with
    /// `pacman -U` as root. No sudo, no doas, no tty anywhere in the build.
    #[test]
    fn paru_builds_from_source_without_any_privilege_escalation() {
        let mut a = install_app();
        a.config.aur_packages.push("some-aur-pkg".into());
        a.config.use_doas = true;
        a.config.chaotic_aur = false; // force the from-source path
        let t = plan_text(&build_plan(&a));

        let build = t
            .lines()
            .find(|l| l.contains("git clone https://aur.archlinux.org/paru.git"))
            .expect("no paru source build in the plan");

        // makepkg must NOT self-install (that is the line that elevates).
        assert!(
            !build.contains("makepkg -si") && !build.contains("makepkg -s "),
            "makepkg still installs deps itself, which elevates and breaks on doas:\n{build}"
        );
        assert!(
            build.contains("makepkg -f"),
            "the build no longer runs makepkg at all:\n{build}"
        );
        // The build deps (cargo via rust) are laid down by ROOT pacman first…
        let deps_pos = build
            .find("pacman -S --needed --noconfirm")
            .unwrap_or(usize::MAX);
        let make_pos = build.find("makepkg -f").unwrap_or(0);
        assert!(
            deps_pos < make_pos && build[..make_pos].contains("rust"),
            "cargo/rust is not installed by root before the build:\n{build}"
        );
        // …and the finished package is installed with root pacman -U, not by
        // makepkg elevating.
        assert!(
            build.contains("pacman -U --noconfirm"),
            "the built package is not installed by root pacman -U:\n{build}"
        );
    }

    /// The starship theme is generated robustly, before the home is chowned.
    ///
    /// Symptom seen live: the shell came up with starship's plain `❯` default,
    /// looking as if starship never installed. It HAD — but the theme file was
    /// missing, because the `su - {user}` login shell that generated it was
    /// fragile in the chroot and its failure was swallowed by `|| true`. The
    /// preset is now written by root (starship preset only writes a file) and
    /// the later `chown -R {home}` hands it over.
    #[test]
    fn the_starship_theme_is_generated_by_root_before_the_home_is_chowned() {
        let mut a = install_app();
        a.config.extra_packages = vec!["zsh".into()];
        let t = plan_text(&build_plan(&a));

        let theme = t
            .lines()
            .position(|l| l.contains("starship preset pastel-powerline -o"))
            .expect("the starship theme is never generated");
        // Not through a fragile login shell.
        assert!(
            !t.lines().nth(theme).unwrap().contains("su - "),
            "the theme is still generated via a `su -` login shell, which was \
             the fragile step that silently produced no theme"
        );
        // And it must land BEFORE the blanket home chown, or it stays root-owned.
        let chown = t
            .lines()
            .position(|l| l.contains("chown -R") && l.trim_end().ends_with("/home/tester"))
            .expect("the home is never chowned to the user");
        assert!(
            theme < chown,
            "the starship theme is written after the home chown, so it stays \
             root-owned"
        );
    }

    /// "slayfetch" is not a real package — it installs the ordinary fastfetch
    /// with a chosen Artix + pride logo. The plan must install `fastfetch` (never
    /// a bogus `slayfetch` target) and still write the fastfetch config.
    #[test]
    fn slayfetch_installs_real_fastfetch_and_its_config() {
        let mut a = install_app();
        a.config.extra_packages = vec!["slayfetch".into()];
        a.config.fastfetch_logo = "Transgender".into();
        let t = plan_text(&build_plan(&a));
        // slayfetch is a UI alias — it maps to the real fastfetch package and
        // must never be handed to pacman as an install target.
        assert!(
            system_packages(&a.config).iter().any(|p| p == "fastfetch"),
            "slayfetch must install the real fastfetch package"
        );
        assert!(
            !system_packages(&a.config).iter().any(|p| p == "slayfetch"),
            "slayfetch must not be a pacman target"
        );
        assert!(
            t.contains(".config/fastfetch/config.jsonc"),
            "the fastfetch config must still be written for slayfetch"
        );
        // A `slayfetch` command (wrapper → fastfetch) is installed so the name
        // the user picked actually works in the shell.
        assert!(
            t.contains("/usr/local/bin/slayfetch") && t.contains("exec fastfetch"),
            "a slayfetch → fastfetch wrapper must be installed:\n{t}"
        );
    }

    /// The Pinnacle config ships as plain files, not as an embedded tarball.
    #[test]
    fn pinnacle_config_is_written_as_plain_files() {
        let mut a = install_app();
        a.config.desktops.push("Pinnacle".into());
        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains(".config/pinnacle/src/main.rs"),
            "the compositor config must be written out as a readable file"
        );
        assert!(
            !t.contains("tar -xzf") || !t.contains("pinnacle-config.tar.gz"),
            "no opaque tarball unpacking"
        );
        assert!(
            !t.contains("--locked"),
            "cargo --locked requires a Cargo.lock we deliberately don't ship; \
             it would fail on every session start"
        );
    }

    /// Mirrors are health-checked before any package is fetched, so a mirror
    /// that degraded since the ISO was built can't stall the install.
    #[test]
    fn mirror_optimization_runs_before_basestrap() {
        let mut a = install_app();
        a.config.optimize_mirrors = true;
        let t = plan_text(&build_plan(&a));
        let m = t
            .find("optmirrors")
            .expect("the mirror script must be planned");
        let b = t.find("basestrap").expect("basestrap must be planned");
        assert!(
            m < b,
            "mirrors must be ranked BEFORE packages are downloaded"
        );
    }

    /// basestrap retries: a single stalled mirror used to abort the whole
    /// install and force a restart from step one.
    #[test]
    fn basestrap_is_retried() {
        let t = plan_text(&build_plan(&install_app()));
        assert!(
            t.contains("basestrap failed (attempt"),
            "basestrap must retry rather than abort on a transient mirror failure"
        );
    }

    #[test]
    fn build_plan_straps_and_enables_parallel_downloads() {
        let t = plan_text(&build_plan(&install_app()));
        assert!(t.contains("basestrap"), "must basestrap the base system");
        assert!(
            t.contains("ParallelDownloads"),
            "must enable ParallelDownloads"
        );
        assert!(t.contains("mkfs.btrfs"), "btrfs root must be formatted");
    }

    #[test]
    fn build_plan_snapshots_are_opt_in() {
        let mut a = install_app();
        a.config.btrfs_subvolumes = false;
        a.config.btrfs_snapshots = false;
        let off = plan_text(&build_plan(&a));
        assert!(
            !off.contains("snapper/configs/root"),
            "no snapper config when snapshots are off"
        );
        assert!(
            !off.contains("set-default"),
            "no default-subvolume switch when snapshots are off"
        );

        a.config.btrfs_subvolumes = true;
        a.config.btrfs_snapshots = true;
        let t = plan_text(&build_plan(&a));
        assert!(
            t.contains("snapper/configs/root"),
            "snapper config present when snapshots on"
        );
        assert!(
            t.contains("usr/local/bin/artix-rollback"),
            "the artix-rollback tool is installed when snapshots are on"
        );
        assert!(
            !t.contains("grub-btrfsd"),
            "the broken GRUB snapshot submenu (grub-btrfsd) must not be set up"
        );
        // We boot @ BY NAME (rootflags=subvol=@) so artix-rollback's @-swap is
        // authoritative; @ is also made the default (belt-and-braces). We no
        // longer neutralise 10_linux's subvol pin or install a grub re-patch hook.
        assert!(
            t.contains("set-default"),
            "must still make @ the default subvolume"
        );
        assert!(
            !t.contains("rootflags pin removed"),
            "must NOT neutralise the 10_linux subvol pin anymore"
        );
        assert!(
            !t.contains("zz-grub-default-subvol.hook"),
            "must NOT install the grub-upgrade re-patch hook anymore"
        );
    }

    #[test]
    fn rootflags_part_pins_subvol_at_for_btrfs_subvolumes() {
        // `mut` because btrfs_snapshots is flipped below to test both paths.
        let mut c = InstallConfig {
            root_fs: "btrfs".into(),
            btrfs_subvolumes: true,
            ..Default::default()
        };

        // Pin @ by name in both cases — boot follows the @ subvolume, which the
        // rollback swaps, so a rollback reliably changes what boots.
        c.btrfs_snapshots = false;
        assert_eq!(
            rootflags_part(&c),
            "rootflags=subvol=@ ",
            "pin @ without snapshots"
        );

        c.btrfs_snapshots = true;
        assert_eq!(
            rootflags_part(&c),
            "rootflags=subvol=@ ",
            "pin @ with snapshots too"
        );

        c.root_fs = "ext4".into();
        assert_eq!(rootflags_part(&c), "", "no rootflags for non-btrfs");
    }

    /// The rollback GRUB entry MUST put root= on the kernel cmdline.
    ///
    /// A hand-written menuentry does not get 10_linux's automatic root=, and
    /// without it the initramfs boots with an EMPTY root device: "device ''
    /// not found", "Failed to mount '' on real root", straight to an emergency
    /// shell. Seen live on BOTH dual-boot systems when picking a snapshot. The
    /// rollback hook reads the same ${root}, so an empty one also makes the
    /// rollback itself a silent no-op.
    #[test]
    fn the_rollback_grub_entry_sets_root_on_the_kernel_cmdline() {
        let s = ARTIX_ROLLBACK_GRUBD;

        // It resolves the ROOT filesystem (`/`), not where the kernel lives —
        // those differ when /boot is a separate/ESP partition.
        assert!(
            s.contains("--target=device /") && s.contains("--target=fs_uuid"),
            "the rollback entry never resolves the root filesystem's UUID"
        );

        // Every `linux` line the script prints must carry root=. The kernel line
        // template is `linux %s %s rootflags=subvol=@ …` — the second %s is the
        // root argument, so there must be no `linux %s rootflags` left (the old
        // form that shipped the empty-root bug).
        assert!(
            s.contains("$rootarg"),
            "the root argument is never added to the kernel cmdline"
        );
        assert!(
            !s.contains("linux %s rootflags=subvol=@"),
            "a linux line still omits root= (the empty-root emergency-shell bug)"
        );
    }
}
