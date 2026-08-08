#!/bin/sh
# QEMU test rig for the Artix TUI installer — the whole stand in one command.
#
# EVERYTHING THIS CREATES LIVES IN ONE FOLDER, INSIDE THE CHECKOUT:
#
#     <checkout>/vm/         (override with ARTIX_VM_DIR=...)
#       disk1-ssd.qcow2      the virtual drives, named for what they pretend to be
#       disk2-hdd.qcow2
#       disk3-nvme.qcow2
#       ovmf-vars.fd         UEFI NVRAM, so efibootmgr entries survive a reboot
#       usbkey.img           removable stick for the LUKS USB-key feature
#
# Next to iso/, for the same reason: everything this project produces is in one
# place. THE FOLDER IS NOT IN THE REPOSITORY — not even a placeholder file. A
# fresh checkout has no vm/; this script makes one the first time somebody wants
# to run the installer in a VM, and asks what should be in it. Nothing here is
# ever committed (.gitignore) or sent to a container build (.dockerignore).
#
# `rm -rf <checkout>/vm` is therefore always safe: the next run builds it again
# and asks the same questions. Nothing is lost that git was keeping.
#
# They were loose in $HOME before, then in ~/artix-tui-vm; both older locations
# are MOVED here on the first run, keeping whatever is installed on the drives.
#
# CAUTION: what the folder HOLDS is tens of gigabytes of installed test systems,
# and `git clean -xdf` in the checkout deletes them — it removes ignored files
# too. That is the one command to keep away from here.
#
# THE DRIVES ARE YOURS TO CHOOSE. With no disks in the folder, the script asks
# how many to make (1–3) and what each one should pretend to be:
#
#   ssd   SATA, rotation_rate=1      shows up as /dev/sdX, reads as an SSD
#   hdd   SATA, rotation_rate=7200   shows up as /dev/sdX, reads as an HDD
#   nvme  NVMe                       shows up as /dev/nvmeXn1
#
# The medium is what the guest kernel reports, not a guess: ATA IDENTIFY word
# 217 (rotation_rate) drives lsblk ROTA, which is exactly what the disk-wipe
# picker and the drive-wear (TBW) screen read. VERIFIED in this rig.
#
# One drive is enough to install; a second lets you put /home somewhere else or
# install twice for dual boot; a third covers the NVMe paths. Answer for the
# test you are running rather than paying for three 50 GiB images every time.
#
# NOTE ON TBW: virtio disks have no SMART at all, and QEMU's SATA reports only
# power-on hours (no attribute 241), while NVMe returns a full health log. The
# USB stick reads as "Unknown USB bridge" — smartctl cannot see through the
# bridge. Expected, not a fault.
#
# Other hardware modelled: UEFI with PERSISTENT NVRAM (pflash CODE+VARS), so the
# efibootmgr entries the installer writes survive a reboot exactly like on real
# hardware (-bios would lose them); user-mode network (mirrors/basestrap/AUR);
# audio OUTPUT ONLY (a duplex codec on a host with no capture nodes spams "no
# target node available"); virtio-tablet for a sane absolute cursor.
#
# NOTHING ABOUT THE HOST IS ASSUMED. Somebody weighing up a move to Artix should
# be able to build an image to their taste and then try it, and they are doing
# that FROM Debian, Fedora, openSUSE or whatever they are leaving. So the OVMF
# firmware is searched for across the paths those distributions use, the audio
# backend is whichever sound server is actually running (or none, silently), KVM
# is used when usable and its absence is explained rather than fatal, and memory
# and cores are sized to the machine — half of it, 4 GiB and 6 cores at most.
#   MEM=8G CPUS=4   override both.
#
# Modes:
#   install   — boot the newest ISO from the checkout's iso/, no GL (virgl on
#               the NVIDIA 580.173 host can crash QEMU mid-install);
#   boot      — boot the installed system from disk, still without GL;
#   pinnacle  — boot the installed system WITH GL (virtio-vga-gl + gl=on):
#               the honest test for the Pinnacle Wayland session. If QEMU
#               crashes here, try  -display sdl,gl=on  instead of gtk.
#   bios      — install in LEGACY BIOS mode (SeaBIOS, no pflash), for the
#               manual-partitioning legacy path: BIOS-boot partition on GPT,
#               grub-install on the whole disk.
#   verify    — UNATTENDED boot check: no window, boot the newest ISO, wait,
#               photograph the screen over QMP, quit. For answering "does this
#               image still boot?" without a human watching it. A dead image
#               reaches dinit and then prints [FAILED] against every service,
#               which looks exactly like a broken installer — so this is the
#               cheapest way to tell a boot failure from an installer bug. It
#               never asks anything and never creates a disk.
#                 WAIT=90  seconds before the screenshot (default 75)
#                 SHOT=... where the PNG goes (default iso/boot-check.png)
#   disks     — set up the stand without starting anything: list what is there,
#               add a drive, delete one, add or remove the USB key. The first-run
#               questions only fire when the folder is EMPTY, so this is how you
#               change your mind afterwards.
#
#   --settings  open the helper first, then carry on into the mode asked for.
#               An empty or absent vm/ opens it by itself; a folder that already
#               has drives goes straight to the VM. This is how you change your
#               mind without emptying the folder.
#
#   USBKEY=0  leave the stick out for one run without deleting it.
set -eu

# Built images live in the checkout's iso/ folder (scripts/build-iso.sh writes
# them there). The artools workspace is tried second, so an image built before
# that move still boots.
# Where the checkout is, worked out from where THIS SCRIPT is — not from a
# hard-coded ~/artix-tui-installer. Anyone cloning the project somewhere else
# (and anyone at all who is not the author) had every path in here point at a
# directory they do not have.
SELF_DIR=$(cd "$(dirname "$0")" && pwd -P)
REPO_DIR="${REPO_DIR:-$(dirname "$SELF_DIR")}"

VMDIR="${ARTIX_VM_DIR:-$REPO_DIR/vm}"
VARS="$VMDIR/ovmf-vars.fd"
STICK="$VMDIR/usbkey.img"
if [ -n "${ISODIR:-}" ]; then
    :  # asked for explicitly — honour it, empty or not, and fail loudly below
elif ls "$REPO_DIR"/iso/*.iso >/dev/null 2>&1; then
    ISODIR="$REPO_DIR/iso"
else
    ISODIR="$HOME/artools-workspace/iso/tui"
fi

# `--settings` is a FLAG, not a mode: it means "let me change the stand first,
# then carry on with whatever I asked for". `disks` remains its own mode for
# setting things up and stopping there.
#
# The rule the flag exists to bend: an empty (or absent) vm/ folder opens the
# helper, and a folder that already has drives goes straight to the VM. That is
# right almost always, and wrong exactly when you want to change something.
want_settings=0
args=""
for a in "$@"; do
    case "$a" in
        --settings|--setup) want_settings=1 ;;
        *) args="$args $a" ;;
    esac
done
# shellcheck disable=SC2086 # deliberately re-split: it is an argument list
set -- $args
mode="${1:-install}"

# The mode is checked HERE, before anything else has an opinion. Otherwise a
# typo produced a complaint about the terminal, or about a missing ISO, from a
# run that was never going to work anyway.
case "$mode" in
    install|boot|pinnacle|bios|verify|disks|drive|key|type|shot|stop) ;;
    *)
        echo "usage: $0 [install|boot|pinnacle|bios|verify|disks|drive|key|type|shot|stop] [--settings]" >&2
        echo "       drive      — boot the ISO headless and LEAVE IT RUNNING" >&2
        echo "       key/type/shot/stop — drive that VM and photograph it" >&2
        echo "       disks      — set the stand up and stop there" >&2
        echo "       --settings — set it up, then carry on into the mode above" >&2
        echo "       USBKEY=0   — leave the stick out for this run" >&2
        exit 2
        ;;
esac

# ── The host, whatever it is ─────────────────────────────────────────────────
# This rig has to work for somebody trying Artix out FROM another distribution —
# the whole point of being able to build an image to taste is being able to try
# it before committing a real machine to it. So nothing about the host is
# assumed; every piece is looked for, and when one is missing the message says
# what to install rather than letting QEMU fail in its own words.

need() {  # need <command> <what it is for>
    command -v "$1" >/dev/null 2>&1 && return 0
    echo "!! $1 not found — $2" >&2
    echo "   Debian/Ubuntu: apt install qemu-system-x86 qemu-utils ovmf" >&2
    echo "   Fedora:        dnf install qemu-system-x86 qemu-img edk2-ovmf" >&2
    echo "   Arch/Artix:    pacman -S qemu-full edk2-ovmf" >&2
    echo "   openSUSE:      zypper install qemu-x86 qemu-tools qemu-ovmf-x86_64" >&2
    exit 1
}
need qemu-system-x86_64 "the emulator itself"
need qemu-img "creating the virtual drives"

# UEFI firmware. Every distribution puts OVMF somewhere else and names the
# 4 MB variant differently, so the pairs are searched rather than hard-coded —
# a single Artix path meant this script could not run anywhere else at all.
# CODE and VARS must come from the SAME build: mixing a 4 MB code image with a
# 2 MB variable store gives a guest that never finds its boot entries.
CODE=""
VARS_TEMPLATE=""
for pair in \
    "/usr/share/edk2/x64/OVMF_CODE.4m.fd:/usr/share/edk2/x64/OVMF_VARS.4m.fd" \
    "/usr/share/edk2/x64/OVMF_CODE.fd:/usr/share/edk2/x64/OVMF_VARS.fd" \
    "/usr/share/OVMF/OVMF_CODE_4M.fd:/usr/share/OVMF/OVMF_VARS_4M.fd" \
    "/usr/share/OVMF/OVMF_CODE.fd:/usr/share/OVMF/OVMF_VARS.fd" \
    "/usr/share/edk2/ovmf/OVMF_CODE.fd:/usr/share/edk2/ovmf/OVMF_VARS.fd" \
    "/usr/share/edk2-ovmf/OVMF_CODE.fd:/usr/share/edk2-ovmf/OVMF_VARS.fd" \
    "/usr/share/qemu/ovmf-x86_64-code.bin:/usr/share/qemu/ovmf-x86_64-vars.bin" \
    "/usr/share/qemu/edk2-x86_64-code.fd:/usr/share/qemu/edk2-i386-vars.fd"
do
    c=${pair%%:*}; v=${pair#*:}
    if [ -f "$c" ] && [ -f "$v" ]; then
        CODE=$c; VARS_TEMPLATE=$v
        break
    fi
done
if [ -z "$CODE" ] && [ "$mode" != disks ] && [ "$mode" != bios ]; then
    echo "!! no UEFI firmware (OVMF) found." >&2
    echo "   Debian/Ubuntu: apt install ovmf      Fedora: dnf install edk2-ovmf" >&2
    echo "   Arch/Artix:    pacman -S edk2-ovmf   openSUSE: zypper install qemu-ovmf-x86_64" >&2
    echo "   Or test the Legacy BIOS path instead, which needs no firmware file:" >&2
    echo "     sh $0 bios" >&2
    exit 1
fi

# KVM, or an honest warning. Without it QEMU still runs — under TCG, perhaps ten
# times slower — and an install that takes an hour instead of five minutes looks
# like a hung installer rather than a missing kernel module.
KVM_ARGS="-enable-kvm -cpu host"
if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
    KVM_ARGS="-cpu max"
    echo ">>> WARNING: /dev/kvm is not usable, so this runs emulated and SLOWLY." >&2
    if [ -e /dev/kvm ]; then
        echo ">>> The device exists but you cannot use it — usually a group:" >&2
        echo ">>>   sudo usermod -aG kvm \"\$USER\"   (then log out and back in)" >&2
    else
        echo ">>> No /dev/kvm at all — virtualisation may be off in the firmware," >&2
        echo ">>> or this is itself a VM without nested virtualisation." >&2
    fi
fi

# Saved machine settings, if the picker has ever been used. Precedence is
# environment > stand.conf > half the host: a variable on the command line is
# someone overriding this run on purpose, and must not be silently ignored in
# favour of a file.
if [ -f "$VMDIR/stand.conf" ]; then
    conf_mem=$(sed -n 's/^MEM_GIB=\([0-9]*\).*/\1/p' "$VMDIR/stand.conf" | tail -1)
    conf_cpus=$(sed -n 's/^CPUS=\([0-9]*\).*/\1/p' "$VMDIR/stand.conf" | tail -1)
    conf_audio=$(sed -n 's/^AUDIO=\(.*\)/\1/p' "$VMDIR/stand.conf" | tail -1)
    [ -n "$conf_mem" ] && MEM="${MEM:-${conf_mem}G}"
    [ -n "$conf_cpus" ] && CPUS="${CPUS:-$conf_cpus}"
    [ -n "$conf_audio" ] && AUDIO="${AUDIO:-$conf_audio}"
fi

# Memory and cores, sized to the HOST rather than to the author's desktop.
#
# `-m 12G -smp 6` was fixed, which on a laptop with 8 GB is either a refusal to
# start or an hour of swapping — and the person it hits is exactly the one
# trying the project out for the first time.
#
# The replacement was "half the machine", which was a ceiling dressed up as a
# default: on a 32 GiB desktop that is twelve gigabytes handed to a guest that
# installs comfortably in four. So: half, capped at 4 and floored at 2. Four is
# also inside the band zswap and earlyoom were written for, which makes the
# ordinary default the interesting one to test. MEM= and CPUS= override, and the
# setup helper remembers whatever you choose.
if [ -z "${MEM:-}" ]; then
    kb=$(awk '/^MemTotal:/ {print $2}' /proc/meminfo 2>/dev/null || echo 0)
    half_gb=$((kb / 1024 / 1024 / 2))
    [ "$half_gb" -gt 4 ] && half_gb=4
    [ "$half_gb" -lt 2 ] && half_gb=2
    MEM="${half_gb}G"
fi
if [ -z "${CPUS:-}" ]; then
    all=$(nproc 2>/dev/null || echo 2)
    CPUS=$((all / 2))
    [ "$CPUS" -gt 6 ] && CPUS=6
    [ "$CPUS" -lt 2 ] && CPUS=2
fi

# Audio: the guest has a sound device either way, but the BACKEND has to be one
# this host actually runs. `-audiodev pipewire` was hard-coded, which is simply
# wrong on a machine using PulseAudio or bare ALSA — QEMU refuses to start.
# Output only: a duplex codec on a host with no capture nodes fills the log with
# "no target node available".
# AUDIO names a backend, or `auto`, or `off`. Naming one is the point: on a
# machine running PulseAudio you may well want to test against ALSA, and the
# probing below can only ever find what the host happens to be using.
audio_backend=""
case "${AUDIO:-auto}" in
    off) audio_choices="" ;;
    auto) audio_choices="pipewire pa alsa" ;;
    *)
        # Asked for by name — but still checked, because a backend QEMU was not
        # built with is a refusal to start, and "no sound" beats "no VM".
        if qemu-system-x86_64 -audiodev help 2>&1 | grep -q "^ *${AUDIO}\$"; then
            audio_choices="$AUDIO"
        else
            echo ">>> WARNING: this QEMU has no '$AUDIO' audio backend - falling back to auto." >&2
            audio_choices="pipewire pa alsa"
        fi
        ;;
esac
# shellcheck disable=SC2086 # a word list on purpose
for cand in $audio_choices; do
    if [ "${AUDIO:-auto}" = auto ]; then
        # Only AUTO probes. A backend the user named is used as named — second
        # -guessing it would make the choice meaningless.
        case "$cand" in
            pipewire) [ -S "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/pipewire-0" ] || continue ;;
            pa)       [ -S "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/pulse/native" ] || continue ;;
            alsa)     [ -d /dev/snd ] || continue ;;
        esac
    fi
    # And QEMU has to have been built with it.
    if qemu-system-x86_64 -audiodev help 2>&1 | grep -q "^ *$cand\$"; then
        audio_backend=$cand
        break
    fi
done
if [ -n "$audio_backend" ]; then
    AUDIO_ARGS="-audiodev $audio_backend,id=snd0 -device intel-hda -device hda-output,audiodev=snd0"
else
    # Silence is fine — no sound server is not a reason to refuse to boot.
    AUDIO_ARGS=""
fi

# ── The VM folder ────────────────────────────────────────────────────────────
mkdir -p "$VMDIR"

# One-time move from the two places this stuff used to live: loose in $HOME, and
# then ~/artix-tui-vm. `mv -n` so an existing file in the new folder always
# wins: this must never be able to overwrite an installed system.
move_in() {  # move_in <source dir> <old name>:<new name>...
    src=$1
    shift
    [ -d "$src" ] || return 0
    for pair in "$@"; do
        old="$src/${pair%%:*}"
        new="$VMDIR/${pair#*:}"
        [ -f "$old" ] || continue
        [ -e "$new" ] && continue
        mv -n "$old" "$new" && echo ">>> moved ${pair%%:*} into ${VMDIR}/" >&2
    done
    rmdir "$src" 2>/dev/null || true   # only if we emptied it
}
move_in "$HOME" \
    "artix-media-ssd.qcow2:disk1-ssd.qcow2" \
    "artix-media-hdd.qcow2:disk2-hdd.qcow2" \
    "artix-media-nvme.qcow2:disk3-nvme.qcow2" \
    "artix-ovmf-vars.fd:ovmf-vars.fd" \
    "usbkey.img:usbkey.img"
move_in "$HOME/artix-tui-vm" \
    "disk1-ssd.qcow2:disk1-ssd.qcow2" \
    "disk2-hdd.qcow2:disk2-hdd.qcow2" \
    "disk3-nvme.qcow2:disk3-nvme.qcow2" \
    "ovmf-vars.fd:ovmf-vars.fd" \
    "usbkey.img:usbkey.img"
# `verify` writes a throwaway NVRAM; it is regenerated every run, so the old one
# is deleted rather than carried over — otherwise it is the single file left
# behind keeping the old folder alive.
rm -f "$HOME/artix-tui-vm/verify-vars.fd"
rmdir "$HOME/artix-tui-vm" 2>/dev/null || true

# Disks are found, never assumed: the name carries the medium, so there is no
# second file to keep in step with reality.
list_disks() { ls "$VMDIR"/disk*-*.qcow2 2>/dev/null | sort; }

ask() {  # ask <prompt> <default>
    printf '%s [%s]: ' "$1" "$2" >&2
    read -r reply || reply=
    [ -n "$reply" ] && printf '%s' "$reply" || printf '%s' "$2"
}

kind_of()  { b=${1##*/}; b=${b%.qcow2}; printf '%s' "${b##*-}"; }
kind_name() {
    case "$1" in
        ssd)  printf 'SSD  (SATA, reads as non-rotating)' ;;
        hdd)  printf 'HDD  (SATA, reads as 7200 rpm)' ;;
        nvme) printf 'NVMe (M.2)' ;;
        *)    printf '%s' "$1" ;;
    esac
}

# The next free disk number, so adding one after deleting another reuses the gap
# rather than climbing forever.
next_slot() {
    n=1
    while [ -n "$(ls "$VMDIR"/disk$n-*.qcow2 2>/dev/null)" ]; do n=$((n + 1)); done
    printf '%s' "$n"
}

make_disk() {  # make_disk <slot> <kind> <size, with its unit: 40G or 512M>
    # The SIZE ARRIVES WITH ITS UNIT. It used to be a bare number with a G
    # stapled on here, which made 512 MiB unsayable — and a size box that cannot
    # express a sub-gigabyte drive forces a wrong answer rather than refusing.
    case "$3" in
        *[GM]) size=$3 ;;
        *) size="${3}G" ;;   # older callers, and the text fallback below
    esac
    qemu-img create -f qcow2 "$VMDIR/disk$1-$2.qcow2" "$size" >/dev/null
    echo ">>> created disk$1-$2.qcow2 ($size, thin — it costs ~200 KB until written to)" >&2
}

ask_kind() {  # ask_kind <slot> <default>
    while :; do
        k=$(ask "Drive $1 pretends to be (ssd/hdd/nvme)" "$2")
        case "$k" in ssd|hdd|nvme) printf '%s' "$k"; return 0 ;; esac
        echo "!! expected ssd, hdd or nvme" >&2
    done
}

ask_size() {
    while :; do
        s=$(ask "Size in GiB" 50)
        case "$s" in ''|*[!0-9]*|0) echo "!! a whole number of GiB, please" >&2 ;;
                     *) printf '%s' "$s"; return 0 ;; esac
    done
}

show_stand() {
    echo >&2
    echo ">>> Drives in $VMDIR:" >&2
    n=0
    for img in $(list_disks); do
        n=$((n + 1))
        used=$(du -h "$img" 2>/dev/null | cut -f1)
        printf '      %s  %-20s %-34s %s on disk\n' \
            "$n" "${img##*/}" "$(kind_name "$(kind_of "$img")")" "$used" >&2
    done
    [ "$n" -eq 0 ] && echo "      (none)" >&2
    if [ -f "$STICK" ]; then
        echo "      +  usbkey.img          USB stick for the LUKS-key test    attached" >&2
    else
        echo "      -  usbkey.img          USB stick for the LUKS-key test    not present" >&2
    fi
    echo >&2
}

# ── The ratatui picker ───────────────────────────────────────────────────────
# tools/vm-setup is a small TUI in the project's own colours. It DECIDES and
# prints a plan; every file operation stays here, in one place, so the text
# fallback below and the pretty version can never drift into doing different
# things. The same split the installer is built on.
#
# Anything at all going wrong — no cargo, a build error, Esc — falls back to the
# questions. A test rig that cannot be used because its own helper will not
# compile would be a poor trade for nicer colours.
VM_SETUP_DIR="$REPO_DIR/tools/vm-setup"

apply_plan() {  # apply_plan <plan file>
    while read -r verb a b c; do
        case "$verb" in
            create) make_disk "$a" "$b" "$c" ;;
            rename)
                # `mv -n` returns 0 when it DECLINES to move, so the success
                # message has to be earned rather than assumed: a rename that
                # quietly did nothing while reporting that it did is how you end
                # up testing the medium you were trying to change away from.
                if [ -e "$VMDIR/$b" ]; then
                    echo "!! $b already exists — $a left alone" >&2
                elif mv -n "$VMDIR/$a" "$VMDIR/$b"; then
                    echo ">>> $a is now $b (the image itself is untouched)" >&2
                else
                    echo "!! could not rename $a" >&2
                fi
                ;;
            delete) rm -f "$VMDIR/$a" && echo ">>> deleted $a" >&2 ;;
            set)
                # Machine settings live in stand.conf beside the drives, so a
                # choice made once survives every later run. Rewritten key by
                # key rather than regenerated, so a setting this script does not
                # know about is not quietly dropped.
                conf="$VMDIR/stand.conf"
                touch "$conf"
                if grep -q "^$a=" "$conf" 2>/dev/null; then
                    tmp="$conf.new"
                    sed "s|^$a=.*|$a=$b|" "$conf" > "$tmp" && mv -f "$tmp" "$conf"
                else
                    echo "$a=$b" >> "$conf"
                fi
                echo ">>> $a = $b" >&2
                ;;
            usbkey)
                if [ "$a" = on ]; then
                    qemu-img create -f raw "$STICK" 256M >/dev/null
                    echo ">>> created usbkey.img (256M)" >&2
                else
                    rm -f "$STICK" && echo ">>> removed usbkey.img" >&2
                fi
                ;;
        esac
    done < "$1"
}

# Returns 0 when the TUI ran and its plan was applied (even an empty one), 1 when
# it could not run at all.
tui_pick() {
    [ -t 0 ] || return 1
    command -v cargo >/dev/null 2>&1 || return 1
    [ -f "$VM_SETUP_DIR/Cargo.toml" ] || return 1

    bin="$VM_SETUP_DIR/target/release/vm-setup"
    if [ ! -x "$bin" ]; then
        echo ">>> building the picker (once)..." >&2
        cargo build --release --quiet --manifest-path "$VM_SETUP_DIR/Cargo.toml" >&2 || return 1
    fi
    [ -x "$bin" ] || return 1

    plan="${TMPDIR:-/tmp}/artix-vm-plan.$$"
    if "$bin" "$VMDIR" > "$plan"; then
        apply_plan "$plan"
        rm -f "$plan"
        return 0
    fi
    # Esc, or the terminal refused to go raw: change nothing.
    rm -f "$plan"
    echo ">>> stand left as it was" >&2
    return 0
}

# First run: build a stand from nothing.
#
# The USB stick is ASKED ABOUT rather than just appearing. It is a real drive in
# the guest, so a stand of "three drives" that shows four in the installer is
# simply wrong — and that is what happened: the stick was created silently and
# the count nobody chose was the count on screen.
create_disks() {
    echo >&2
    echo ">>> No virtual drives in $VMDIR yet." >&2
    echo ">>> Answer for the test you are about to run." >&2
    echo >&2

    # ONE by default. Drives are the only thing here that costs real gigabytes,
    # and somebody testing a single scenario on a single disk should not have to
    # go and delete two they never asked for.
    while :; do
        count=$(ask "How many drives? (1-3)" 1)
        case "$count" in 1|2|3) break ;; esac
        echo "!! expected 1, 2 or 3" >&2
    done
    size=$(ask_size)

    i=1
    while [ "$i" -le "$count" ]; do
        # A sensible default per slot, so pressing Enter three times gives the
        # one-of-each stand this rig was built around.
        case "$i" in 1) def=ssd ;; 2) def=hdd ;; *) def=nvme ;; esac
        make_disk "$i" "$(ask_kind "$i" "$def")" "$size"
        i=$((i + 1))
    done

    # Default yes: testing the LUKS USB-key feature needs a real removable
    # device, and there is no other way to get one.
    case "$(ask "Also attach a USB stick, for the LUKS-key test? (y/n)" n)" in
        y|Y|yes) qemu-img create -f raw "$STICK" 256M >/dev/null
                 echo ">>> created usbkey.img (256M)" >&2 ;;
    esac
    echo >&2
}

# `disks` mode: change the stand without starting a VM.
#
# The first-run questions used to be the ONLY way to choose, and they only fire
# when the folder is empty — so once a stand existed there was no way to say
# "actually, two drives" short of deleting files by hand.
manage_disks() {
    while :; do
        show_stand
        echo "  a  add a drive          d  delete a drive" >&2
        echo "  u  add/remove the USB stick" >&2
        echo "  Enter  done" >&2
        printf '> ' >&2
        read -r act || act=
        case "$act" in
            a|A)
                slot=$(next_slot)
                make_disk "$slot" "$(ask_kind "$slot" ssd)" "$(ask_size)"
                ;;
            d|D)
                which=$(ask "Delete which number? (blank to cancel)" "")
                [ -z "$which" ] && continue
                img=$(list_disks | sed -n "${which}p")
                if [ -z "$img" ]; then
                    echo "!! no drive number $which" >&2
                    continue
                fi
                # A drive can hold an installed system, and this cannot be
                # undone — so the confirmation is the file's own name, not "y".
                echo "!! ${img##*/} may hold an installed system. This deletes it." >&2
                sure=$(ask "Type the file name to confirm" "")
                if [ "$sure" = "${img##*/}" ]; then
                    rm -f "$img" && echo ">>> deleted ${img##*/}" >&2
                else
                    echo ">>> left alone" >&2
                fi
                ;;
            u|U)
                if [ -f "$STICK" ]; then
                    rm -f "$STICK" && echo ">>> removed usbkey.img" >&2
                else
                    qemu-img create -f raw "$STICK" 256M >/dev/null
                    echo ">>> created usbkey.img (256M)" >&2
                fi
                ;;
            '') return 0 ;;
            *) echo "!! a, d, u or Enter" >&2 ;;
        esac
    done
}

# ── driving a running VM ─────────────────────────────────────────────────────
# `drive` boots the image and stays out of the way; `key`, `type`, `shot` and
# `stop` then talk to it. Together they let the interface be WALKED — which is
# the one thing the unit tests cannot do. They prove a screen does not panic and
# that a plan comes out right; they cannot say whether the hint names the right
# key, or whether a choice made on step 2 survives to step 11.
DRIVE_SOCK=/tmp/artix-qemu-drive.sock
SHOTS="${SHOTS:-$REPO_DIR/iso/walk}"

case "$mode" in
    key|type|shot)
        [ -S "$DRIVE_SOCK" ] || { echo "!! no VM is being driven — start one with: $0 drive" >&2; exit 1; }
        shift
        if [ "$mode" = shot ]; then
            mkdir -p "$SHOTS"
            out="$SHOTS/${1:-shot}.png"
            python3 "$SELF_DIR/qmp.py" "$DRIVE_SOCK" shot "$out"
            echo "$out"
        else
            python3 "$SELF_DIR/qmp.py" "$DRIVE_SOCK" "$mode" "$@"
        fi
        exit 0
        ;;
    stop)
        [ -S "$DRIVE_SOCK" ] && python3 "$SELF_DIR/qmp.py" "$DRIVE_SOCK" quit 2>/dev/null
        rm -f "$DRIVE_SOCK"
        echo ">>> stopped" >&2
        exit 0
        ;;
esac

if [ "$mode" = disks ]; then
    [ -t 0 ] || { echo "!! 'disks' needs a terminal to ask on." >&2; exit 1; }
    tui_pick || manage_disks
    exit 0
fi

# Asked for the helper explicitly: show it whatever state the folder is in, then
# carry on into the mode that was requested.
if [ "$want_settings" -eq 1 ] && [ "$mode" != verify ] && [ "$mode" != drive ]; then
    [ -t 0 ] || { echo "!! --settings needs a terminal to ask on." >&2; exit 1; }
    tui_pick || manage_disks
fi

# `verify` is unattended by definition — it must never stop on a question, and a
# boot check needs no drive at all.
if [ -z "$(list_disks)" ] && [ "$mode" != verify ] && [ "$mode" != drive ]; then
    if [ -t 0 ]; then
        tui_pick || create_disks
    else
        echo "!! no drives in $VMDIR and nothing to ask on (not a terminal)." >&2
        echo "   Run  sh scripts/qemu-test.sh disks  from a terminal to set them up." >&2
        exit 1
    fi
    # Cancelling the picker is a valid answer, so this is a warning and not a
    # refusal — booting the ISO with no drive is a fine way to look around. It
    # is said out loud because otherwise the installer reaches its disk step,
    # finds nothing, and looks broken.
    if [ -z "$(list_disks)" ]; then
        echo ">>> No drives: the VM will boot with nowhere to install." >&2
        echo ">>> Add some with:  sh scripts/qemu-test.sh disks" >&2
    fi
fi

# ── Firmware, display, media ─────────────────────────────────────────────────
# `verify` gets a THROWAWAY NVRAM and no drives at all, and both halves matter.
# The persistent NVRAM remembers the efibootmgr entry of whatever was installed
# last, and the firmware's own boot order outranks `-boot d` — so the check
# booted the installed system off the disk and photographed its login screen,
# which says nothing whatsoever about the ISO. With no drive and no remembered
# entry there is exactly one thing to boot.
if [ "$mode" = verify ] || [ "$mode" = drive ]; then
    VARS="$VMDIR/verify-vars.fd"
    rm -f "$VARS"
fi

gpu_args="-vga none -device virtio-vga -display gtk,show-cursor=on"
fw_args="-drive if=pflash,format=raw,readonly=on,file=$CODE -drive if=pflash,format=raw,file=$VARS"
cd_args=""

# Newest ISO wins — by MODIFICATION TIME, not by name.
#
# The glob used to decide it: names carry a date (artix-tui-dinit-YYYYMMDD), so
# the last match alphabetically was assumed to be the newest. That holds only
# while every file follows the pattern and no two builds share a day. Rebuild
# twice in one day with a different name, or keep an image someone renamed, and
# the script quietly boots the wrong one — which is the worst possible failure
# here, because everything afterwards looks like the code did not change.
newest_iso() {
    iso=$(ls -t "$ISODIR"/*.iso 2>/dev/null | head -1)
    if [ -z "$iso" ] || [ ! -f "$iso" ]; then
        echo "!! no ISO found in $ISODIR — build one first: sh scripts/build-iso.sh" >&2
        exit 1
    fi
    echo ">>> booting from: $iso" >&2
    printf '%s' "$iso"
}

case "$mode" in
    install)
        cd_args="-cdrom $(newest_iso) -boot d"
        ;;
    boot)
        ;;
    pinnacle)
        gpu_args="-vga none -device virtio-vga-gl -display gtk,gl=on,show-cursor=on"
        ;;
    bios)
        # Legacy boot: SeaBIOS, so no pflash at all.
        fw_args=""
        cd_args="-cdrom $(newest_iso) -boot d"
        ;;
    verify | drive)
        cd_args="-cdrom $(newest_iso) -boot d"
        gpu_args="-vga none -device virtio-vga -display none"
        ;;
    *)
        echo "usage: $0 [install|boot|pinnacle|bios|verify|disks]" >&2
        echo "       disks — add/remove virtual drives and the LUKS-key USB stick" >&2
        echo "       USBKEY=0 — leave the stick out for this run" >&2
        exit 1
        ;;
esac

[ -f "$VARS" ] || cp "$VARS_TEMPLATE" "$VARS"

# ── Attach whatever drives the folder holds ──────────────────────────────────
# SATA drives share one AHCI controller and take consecutive buses; NVMe drives
# are their own devices. Serials are distinct because the drive-wear screen
# shows them, and two drives with one serial is a confusing screen.
disk_args=""
ahci=0
n=0
# `verify` only boots the image and photographs it, so it gets no drives —
# nothing should be able to touch a test system during a smoke check. `drive`
# is the opposite: it exists to WALK the installer, and a walkthrough with no
# disks cannot reach the disk step at all. It said "no disks detected" on a
# stand that had two, and the harness was the thing at fault.
for img in $(case "$mode" in verify) ;; *) list_disks ;; esac); do
    n=$((n + 1))
    base=${img##*/}
    kind=${base%.qcow2}; kind=${kind##*-}
    disk_args="$disk_args -drive if=none,id=d$n,format=qcow2,file=$img"
    case "$kind" in
        nvme)
            disk_args="$disk_args -device nvme,drive=d$n,serial=TESTNVME00$n"
            ;;
        hdd)
            [ "$ahci" -eq 0 ] && disk_args="-device ich9-ahci,id=ahci $disk_args"
            disk_args="$disk_args -device ide-hd,drive=d$n,bus=ahci.$ahci,rotation_rate=7200,serial=TESTHDD00$n"
            ahci=$((ahci + 1))
            ;;
        *)
            [ "$ahci" -eq 0 ] && disk_args="-device ich9-ahci,id=ahci $disk_args"
            disk_args="$disk_args -device ide-hd,drive=d$n,bus=ahci.$ahci,rotation_rate=1,serial=TESTSSD00$n"
            ahci=$((ahci + 1))
            ;;
    esac
done
[ "$n" -gt 0 ] && echo ">>> drives: $(list_disks | sed "s|$VMDIR/||" | tr '\n' ' ')" >&2

# The USB stick, for testing the LUKS USB-key feature — which cannot be tested
# without a real removable device.
#
# It is attached only when it EXISTS. It used to be created on the spot whenever
# it was missing, which is how a stand the user had built as three drives showed
# four in the installer: the fourth was this, and nothing had ever asked. Now the
# stand is exactly what was chosen — `disks` mode adds it and removes it.
# USBKEY=0 still leaves it out for a single run without deleting it.
usb_args=""
if [ "${USBKEY:-1}" != "0" ] && [ -f "$STICK" ]; then
    usb_args="-device qemu-xhci,id=xhci -drive file=$STICK,if=none,id=stick,format=raw -device usb-storage,bus=xhci.0,drive=stick,removable=on"
    echo ">>> USB key: usbkey.img attached" >&2
fi

# Host free space. The images are thin, so they grow as the guest writes: a full
# install is several GiB and a zero-fill wipe of a 50 GiB drive writes all 50.
#
# What happens when the host runs out is easy to misread: QEMU's default write
# policy is werror=enospc, so it PAUSES the guest instead of failing the write.
# The window title gains "[Paused]", the install freezes mid-progress, and
# nothing says why. That is a host problem, not an installer bug — free space
# and resume with Machine ▸ Resume. Warn loudly rather than refuse, since a
# partitioning-only or TBW session needs almost nothing.
avail=$(df -Pk "$VMDIR" 2>/dev/null | awk 'NR==2 {print $4}')
if [ -n "$avail" ] && [ "$avail" -lt 20971520 ]; then
    echo ">>> WARNING: only $((avail / 1024 / 1024)) GiB free on $VMDIR." >&2
    echo ">>> A full install needs several GiB; a zero-fill wipe needs 50." >&2
    echo ">>> If the host fills up, QEMU PAUSES the VM mid-install ([Paused] in" >&2
    echo ">>> the title) — free space, then Machine > Resume to carry on." >&2
    echo ">>> Biggest safe win on Artix:  sudo paccache -rk1" >&2
fi

run_qemu() {
# shellcheck disable=SC2086 # these are flag lists; splitting is the point
qemu-system-x86_64 $KVM_ARGS -m "$MEM" -smp "$CPUS" -machine q35 \
    $fw_args \
    $disk_args \
    $usb_args \
    -nic user,model=virtio-net-pci \
    $AUDIO_ARGS \
    -device virtio-tablet-pci \
    $gpu_args $cd_args \
    -name "Artix TUI Installer — $mode" "$@"
}

# `drive` starts the VM and RETURNS, leaving it running: the whole point is to
# come back and press keys at it from another command.
if [ "$mode" = drive ]; then
    rm -f "$DRIVE_SOCK"
    run_qemu -qmp "unix:$DRIVE_SOCK,server,nowait" >/dev/null 2>&1 &
    i=0
    while [ ! -S "$DRIVE_SOCK" ] && [ "$i" -lt 30 ]; do sleep 1; i=$((i + 1)); done
    [ -S "$DRIVE_SOCK" ] || { echo "!! QEMU never opened the QMP socket" >&2; exit 1; }
    echo ">>> driving. Give it ~${WAIT:-60}s to reach the installer, then:" >&2
    echo ">>>   sh $0 shot 01-language     # photograph" >&2
    echo ">>>   sh $0 key down ret         # press keys" >&2
    echo ">>>   sh $0 type Kyiv            # type a word" >&2
    echo ">>>   sh $0 stop" >&2
    exit 0
fi

if [ "$mode" != verify ]; then
    run_qemu
    exit $?
fi

# ── verify: boot it, photograph it, kill it ──────────────────────────────────
# The QMP socket path goes in /tmp deliberately: a unix socket address is capped
# at ~108 bytes, and a checkout path plus a name already crowds that.
sock=/tmp/artix-qemu-verify.sock
shot="${SHOT:-$REPO_DIR/iso/boot-check.png}"
wait_s="${WAIT:-75}"
rm -f "$sock" "$shot"

run_qemu -qmp "unix:$sock,server,nowait" &
qpid=$!
trap 'kill "$qpid" 2>/dev/null; rm -f "$sock"' EXIT INT TERM

echo ">>> booting headless, screenshot in ${wait_s}s -> $shot" >&2
i=0
while [ ! -S "$sock" ] && [ "$i" -lt 30 ]; do sleep 1; i=$((i + 1)); done
[ -S "$sock" ] || { echo "!! QEMU never opened the QMP socket" >&2; exit 1; }
sleep "$wait_s"

python3 - "$sock" "$shot" <<'PY'
import json, socket, sys

sock, shot = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sock)
f = s.makefile("rw", buffering=1, encoding="utf-8", newline="\n")
f.readline()                                   # greeting


def cmd(name, **args):
    f.write(json.dumps({"execute": name, "arguments": args}) + "\n")
    while True:                                # events interleave with replies
        reply = json.loads(f.readline())
        if "return" in reply or "error" in reply:
            return reply


cmd("qmp_capabilities")
# QEMU >= 8.1 writes PNG directly; older builds only know PPM. Ask, then fall
# back, rather than assuming — a PPM named .png silently defeats every viewer.
r = cmd("screendump", filename=shot, format="png")
if "error" in r:
    r = cmd("screendump", filename=shot)
    if "error" in r:
        sys.exit("screendump failed: " + r["error"]["desc"])
cmd("quit")
PY

wait "$qpid" 2>/dev/null || true
trap - EXIT INT TERM
rm -f "$sock"
[ -s "$shot" ] || { echo "!! no screenshot was written" >&2; exit 1; }
echo ">>> screenshot: $shot" >&2
