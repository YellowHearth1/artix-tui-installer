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
    App, Desktop, GpuDriver, InstallConfig, Kernel, AUDIO_PACKAGES, DINIT_PACKAGES, DINIT_SERVICES,
};
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
    let uefi = c.boot_mode == "uefi";
    let mut plan: Vec<Action> = Vec::new();

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
        let no_net = if c.lang == "uk" {
            "echo '!!! Немає підключення до інтернету — для встановлення потрібно завантажувати пакунки.'; \
             echo '!!! Натисніть Esc, щоб повернутися назад, відкрийте крок Wi-Fi (підключення до інтернету), під’єднайтеся, і запустіть встановлення знову.'; \
             echo '!!! Нічого не змінено — ваш диск недоторканий.'"
        } else {
            "echo '!!! No internet connection — an install must download packages.'; \
             echo '!!! Press Esc to go back, open the Wi-Fi (internet) step, connect, then start the install again.'; \
             echo '!!! Nothing has been changed — your disk is untouched.'"
        };
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
    if c.root_fs == "btrfs" {
        host_tools.push("btrfs-progs");
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

    // 1) Disk: partition, format, mount.
    plan.extend(disk::build_plan(
        &c.disk,
        uefi,
        c.swap_gib,
        &c.root_fs,
        c.encrypt_disk,
        luks_pass,
        &c.encrypt_scope,
        c.btrfs_subvolumes,
        c.btrfs_compress,
        c.btrfs_discard,
        c.mount_noatime,
        c.extra_disks.iter().any(|d| d.mountpoint == "/home"),
    ));
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
    // Refresh databases so the newly-enabled Arch repos are immediately usable
    // in the installed system (|| true: don't fail the install if a mirror is
    // momentarily unreachable — the user can re-sync later).
    plan.push(chroot("pacman -Sy --noconfirm || true"));

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
        // decides; in practice these named targets resolve cleanly.
        plan.push(chroot_interactive(&format!("pacman -S --needed {xargs}")));
    }
    // The rest of the system: desktop, GPU, extras, DM, seat. Interactive.
    let sys_pkgs = system_packages(c);
    if !sys_pkgs.is_empty() {
        let sys_args = sys_pkgs.join(" ");
        plan.push(chroot_interactive(&format!(
            "pacman -S --needed {sys_args}"
        )));
    }
    // ─────────────────────────────────────────────────────────────────────────

    // 9) Accounts. Four modes (see AccountMode). Passwords are piped to
    //    chpasswd via printf so we avoid interactive prompts; the values come
    //    from the wizard. We keep each step discrete for clear logging.
    let m = match c.account_mode.as_str() {
        "UserSameRoot" => crate::app::AccountMode::UserSameRoot,
        "UserSudoOnly" => crate::app::AccountMode::UserSudoOnly,
        "RootOnly" => crate::app::AccountMode::RootOnly,
        _ => crate::app::AccountMode::UserSeparateRoot,
    };

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
        plan.push(chroot(&format!(
            "useradd -m -G wheel,audio,video,storage,network,input -s {} {}",
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
        plan.push(chroot(&format!(
            "printf '%s:%s\\n' {} \"{}\" | chpasswd",
            c.username,
            shell_escape_dq(&c.user_password)
        )));
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
            // Some scripts/tools call `sudo` by name; a doas-backed shim
            // keeps them working without pulling in real sudo.
            plan.push(chroot("ln -sf $(command -v doas) /usr/local/bin/sudo"));
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
        if c.seat_provider == "seatd" {
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
        if c.seat_provider == "seatd" && any_x11_session {
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
            // Generate ~/.config/starship.toml from the built-in preset, as the
            // user. `mkdir -p ~/.config` first so the output path exists.
            plan.push(chroot(&format!(
                "su - {user} -c 'mkdir -p ~/.config && starship preset pastel-powerline -o ~/.config/starship.toml' || true",
                user = c.username
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

        // fastfetch config + logo → ~/.config/fastfetch. fastfetch ships as
        // a DEFAULT-CHECKED entry in the packages screen; while it stays
        // selected, the themed config + syrnyk logo are written here. The
        // logo is a PNG (binary), written via base64; the config is text
        // whose logo `source` we rewrite to the user's ABSOLUTE home path
        // (fastfetch only expands ~ on v2.41.0+ and $HOME via wordexp, so a
        // literal absolute path always resolves regardless of version or
        // launch context, e.g. from .zshrc). Embedded in the installer
        // binary, no ISO dependency.
        if c.extra_packages.iter().any(|x| x == "fastfetch") {
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
            plan.extend(write_home_binary(
                &home,
                ".config/fastfetch/fastfetch.png",
                FASTFETCH_LOGO_PNG,
            ));
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
            plan.push(chroot(&format!("mkdir -p {home}/.config/pinnacle")));
            plan.extend(write_home_binary(
                &home,
                ".config/pinnacle/.pinnacle-config.tar.gz",
                PINNACLE_CONFIG_TARBALL,
            ));
            // Unpack into the pinnacle config dir, then delete the tarball.
            plan.push(chroot(&format!(
                "tar -xzf {home}/.config/pinnacle/.pinnacle-config.tar.gz \
                 -C {home}/.config/pinnacle && \
                 rm -f {home}/.config/pinnacle/.pinnacle-config.tar.gz"
            )));
            // Make every script executable (the archive preserves the bit for
            // some, but normalise so launcher.sh / mango-tags.sh are runnable
            // too). Guarded by a dir test so it never fails if scripts/ is
            // absent. find … -exec chmod marks each *.sh file.
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
    match m {
        crate::app::AccountMode::UserSameRoot => {
            plan.push(chroot(&format!(
                "printf 'root:%s\\n' \"{}\" | chpasswd",
                shell_escape_dq(&c.user_password)
            )));
        }
        crate::app::AccountMode::UserSeparateRoot | crate::app::AccountMode::RootOnly => {
            plan.push(chroot(&format!(
                "printf 'root:%s\\n' \"{}\" | chpasswd",
                shell_escape_dq(&c.root_password)
            )));
        }
        crate::app::AccountMode::UserSudoOnly => {
            // Disable root login entirely; access is via sudo (wheel).
            plan.push(chroot("passwd -l root"));
        }
    }

    // 9a-luks) When the root is encrypted, wire up the initramfs and GRUB so
    //     the system can unlock the LUKS volume(s) at boot.
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
            plan.push(chroot(&format!(
                "bootpart=$(blkid -t PARTLABEL=BOOT -o device | head -n1); printf '%s' '{pass}' | cryptsetup luksAddKey \"$bootpart\" /etc/luks/boot.key",
                pass = pass_esc
            )));
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
            )]));
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

    // 9b) Generate the initramfs for the installed kernel(s). mkinitcpio -P
    //     builds every preset present, covering whichever kernel was chosen.
    plan.push(chroot("mkinitcpio -P"));

    // Rescue kernel pair for KERNEL-INDEPENDENT rollback: a frozen copy of the
    // freshly built kernel+initramfs that pacman never touches. The bootloader
    // "System Rollback" entries boot THIS pair, so the snapshot picker stays
    // reachable even when a kernel update breaks the live kernel/initramfs.
    // Refreshed only after a successful normal boot (artix-rescue-sync) and
    // after a rollback (artix-rollback-fixup) — never by an update itself.
    if c.root_fs == "btrfs" && c.btrfs_subvolumes && c.btrfs_snapshots {
        let (vmlinuz, initramfs) = kernel_images(&c.kernel);
        plan.push(chroot(&format!(
            "cp -f /boot/{vmlinuz} /boot/vmlinuz-artix-rescue && \
             cp -f /boot/{initramfs} /boot/initramfs-artix-rescue.img"
        )));
    }

    // 9c) Snapshots: make @ the btrfs default subvolume too (belt-and-braces).
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

    // 10) Bootloader. GRUB is the default and the only one that can decrypt an
    //     encrypted /boot; rEFInd and Limine are offered for plaintext and
    //     root-only LUKS (the UI blocks full-disk encryption with them).
    match c.bootloader.as_str() {
        "refind" => {
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
        "limine" => {
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
                let (vmlinuz, initramfs) = kernel_images(&c.kernel);
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
        "efistub" => {
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
                let (vmlinuz, initramfs) = kernel_images(&c.kernel);
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
        _ => {
            // GRUB (default).
            if uefi {
                let efi_dir = if c.encrypt_disk && c.encrypt_scope == "full" {
                    "/boot/efi"
                } else {
                    "/boot"
                };
                plan.push(chroot(&format!(
                    "grub-install --target=x86_64-efi --efi-directory={efi} --bootloader-id='{id}'",
                    efi = efi_dir,
                    id = c.bootloader_id.replace('\'', "")
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
            }
            // Boot-time rollback entry for GRUB: a generator that adds a
            // top-level "System Rollback" menuentry (booting with artix.rollback)
            // next to the normal entries. Must land before grub-mkconfig runs.
            if c.root_fs == "btrfs" && c.btrfs_subvolumes && c.btrfs_snapshots {
                let (vmlinuz, initramfs) = kernel_images(&c.kernel);
                let script = ARTIX_ROLLBACK_GRUBD
                    .replace("@@VMLINUZ@@", vmlinuz)
                    .replace("@@INITRD@@", initramfs);
                plan.push(write_target_file(
                    "/mnt/etc/grub.d/45_artix_rollback",
                    &script,
                ));
                plan.push(chroot("chmod 755 /etc/grub.d/45_artix_rollback"));
            }
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
    if c.bootloader == "efistub" && c.prepare_secureboot && uefi {
        // Generate the key pair now (safe in chroot). Signing/enrolling is left
        // to the user on first boot, per the instructions below.
        plan.push(chroot("sbctl create-keys || true"));
        // Bilingual instruction file written to the user's home. Kept as a
        // placeholder-substituted heredoc so none of the shell/`sbctl` syntax
        // needs escaping. @@KERNEL@@ is the actual vmlinuz name for their kernel.
        let (vmlinuz, _initramfs) = kernel_images(&c.kernel);
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
        if c.seat_provider == "seatd" {
            plan.push(chroot("gpasswd -a greeter seat 2>/dev/null || true"));
        }
        // ReGreet needs no extra session file: it reads the installed
        // wayland-sessions/xsessions .desktop entries directly. (tuigreet does
        // the same.) The AUR-only gtkgreet/wlgreet — which needed an
        // environments/sway-config file — are not offered, so there's nothing
        // more to write here.
    }

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
    services.push(match c.seat_provider.as_str() {
        "elogind" => "elogind".into(),
        _ => "seatd".into(),
    });
    // Enable the per-user dinit launcher service that matches the backend
    // installed above: userspawn (elogind) or turnstiled (seatd/none). This is
    // what spawns the user's dinit instance on login → D-Bus, PipeWire, sound.
    services.push(match c.seat_provider.as_str() {
        "elogind" => "userspawn".into(),
        _ => "turnstiled".into(),
    });
    // System logging: syslog-ng captures all system logs to /var/log; cronie
    // runs the scheduled logrotate job that expires them weekly (see the
    // logrotate config written below). Both are always enabled.
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
    let seat_skip = if c.seat_provider == "elogind" {
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
    if c.seat_provider == "elogind" {
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
    if !aur_pkgs.is_empty() && m.needs_user() {
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
        // Under doas, makepkg -si must escalate via doas, not sudo: point it
        // at doas through the --asdeps/-- install step by exporting
        // PACMAN's escalation. makepkg reads $PACMAN? No — it calls sudo
        // directly, so the sudo→doas symlink we created above is what makes
        // `makepkg -si` work. No extra flag needed here.
        let build_paru_from_src = format!(
            "su - {user} -c 'cd ~ && rm -rf paru && git clone https://aur.archlinux.org/paru.git && cd paru && makepkg -si --noconfirm'"
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
        plan.push(chroot_interactive(&format!(
            "su - {user} -c 'LANG=C LC_ALL=C LC_MESSAGES=C paru {paru_esc}-S --needed --skipreview {pkgs}' || true"
        )));

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
             su - {user} -c 'cd ~/.config/pinnacle && cargo build --release --locked' \
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

    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_text(plan: &[Action]) -> String {
        plan.iter()
            .map(|a| format!("{} {}", a.program, a.args.join(" ")))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn resolve_mp_expands_tilde_home() {
        let mut c = InstallConfig::default();
        c.username = "alice".into();
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
        a.config.boot_mode = "uefi".into();
        a.config.bootloader = "grub".into();
        a.config.root_fs = "btrfs".into();
        a
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
        let mut c = InstallConfig::default();
        c.root_fs = "btrfs".into();
        c.btrfs_subvolumes = true;

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
}
