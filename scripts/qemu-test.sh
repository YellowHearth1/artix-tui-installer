#!/bin/sh
# QEMU test rig for the Artix TUI installer — the whole stand in one command.
#
# THREE disks, one per medium type, and nothing else. Every medium-dependent
# code path is reachable in one boot, and the disk list stays readable:
#
#   /dev/sda      SATA  50G  reports itself an SSD  (rotation_rate=1)
#   /dev/sdb      SATA  50G  reports itself an HDD  (rotation_rate=7200)
#   /dev/nvme0n1  NVMe  50G  an M.2 drive
#
# The medium is what the guest kernel reports, not a guess: ATA IDENTIFY word
# 217 (rotation_rate) drives lsblk ROTA, which is exactly what the disk-wipe
# picker and the drive-wear (TBW) screen read. VERIFIED in this rig — the
# installer shows sda=SSD, sdb=HDD, nvme0n1=NVMe SSD.
#
# Three is enough for everything: install Artix twice on sda for dual boot, use
# sdb as the second drive for /home or storage, and nvme0n1 for the NVMe paths.
#
# Plus a removable USB stick (a further /dev/sdX) for the LUKS USB-key feature
# (ARTIXKEY) — that test needs a real removable device, so it is attached by
# default. It is the ONLY extra: what made the list unreadable before was two
# leftover virtio disks, and those are gone.
#   USBKEY=0   leave the stick out, e.g. to keep the disk list at exactly three.
# In the drive-wear screen the stick reads as "Unknown USB bridge" — smartctl
# cannot see through the USB-to-SATA bridge. Expected, not a fault.
#
# Other hardware modelled: UEFI with PERSISTENT NVRAM (pflash CODE+VARS), so the
# efibootmgr entries the installer writes survive a reboot exactly like on real
# hardware (-bios would lose them); user-mode network (mirrors/basestrap/AUR);
# PipeWire audio OUTPUT ONLY (the host has no capture nodes — a duplex codec
# spams "no target node available"); virtio-tablet for a sane absolute cursor.
#
# Modes:
#   install   — boot the newest ISO from ~/artools-workspace/iso/tui, no GL
#               (virgl on the NVIDIA 580.173 host can crash QEMU mid-install);
#   boot      — boot the installed system from disk, still without GL;
#   pinnacle  — boot the installed system WITH GL (virtio-vga-gl + gl=on):
#               the honest test for the Pinnacle Wayland session. If QEMU
#               crashes here, try  -display sdl,gl=on  instead of gtk.
#   bios      — install in LEGACY BIOS mode (SeaBIOS, no pflash), for the
#               manual-partitioning legacy path: BIOS-boot partition on GPT,
#               grub-install on the whole disk.
#
# Missing images are created on first run, thin — they cost ~200 KB until
# written to. For a clean slate:  rm ~/artix-media-*.qcow2
set -eu

CODE=/usr/share/edk2/x64/OVMF_CODE.4m.fd
VARS="$HOME/artix-ovmf-vars.fd"
SSD="$HOME/artix-media-ssd.qcow2"
HDD="$HOME/artix-media-hdd.qcow2"
NVME="$HOME/artix-media-nvme.qcow2"
STICK="$HOME/usbkey.img"
ISODIR="$HOME/artools-workspace/iso/tui"

[ -f "$VARS" ] || cp /usr/share/edk2/x64/OVMF_VARS.4m.fd "$VARS"
[ -f "$SSD" ]  || qemu-img create -f qcow2 "$SSD" 50G
[ -f "$HDD" ]  || qemu-img create -f qcow2 "$HDD" 50G
[ -f "$NVME" ] || qemu-img create -f qcow2 "$NVME" 50G

mode="${1:-install}"

# Defaults: the no-GL display (stable on this host), UEFI firmware, no CD.
gpu_args="-vga none -device virtio-vga -display gtk,show-cursor=on"
fw_args="-drive if=pflash,format=raw,readonly=on,file=$CODE -drive if=pflash,format=raw,file=$VARS"
cd_args=""

# Newest ISO wins: names carry the date (artix-tui-dinit-YYYYMMDD), so the last
# glob match is the latest build.
newest_iso() {
    iso=""
    for f in "$ISODIR"/*.iso; do iso="$f"; done
    if [ ! -f "$iso" ]; then
        echo "!! no ISO found in $ISODIR — build one first (release.sh)" >&2
        exit 1
    fi
    echo ">>> installing from: $iso" >&2
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
    *)
        echo "usage: $0 [install|boot|pinnacle|bios]   (USBKEY=0 drops the LUKS-key stick)" >&2
        exit 1
        ;;
esac

# The USB stick: on by default because the LUKS USB-key feature cannot be tested
# without a real removable device. USBKEY=0 leaves it out.
usb_args=""
if [ "${USBKEY:-1}" != "0" ]; then
    [ -f "$STICK" ] || qemu-img create -f raw "$STICK" 256M
    usb_args="-device qemu-xhci,id=xhci -drive file=$STICK,if=none,id=stick,format=raw -device usb-storage,bus=xhci.0,drive=stick,removable=on"
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
avail=$(df -Pk "$HOME" 2>/dev/null | awk 'NR==2 {print $4}')
if [ -n "$avail" ] && [ "$avail" -lt 20971520 ]; then
    echo ">>> WARNING: only $((avail / 1024 / 1024)) GiB free on \$HOME." >&2
    echo ">>> A full install needs several GiB; a zero-fill wipe needs 50." >&2
    echo ">>> If the host fills up, QEMU PAUSES the VM mid-install ([Paused] in" >&2
    echo ">>> the title) — free space, then Machine > Resume to carry on." >&2
    echo ">>> Biggest safe win on Artix:  sudo paccache -rk1" >&2
fi

# shellcheck disable=SC2086 # these are flag lists; splitting is the point
exec qemu-system-x86_64 -enable-kvm -cpu host -m 12G -smp 6 -machine q35 \
    $fw_args \
    -device ich9-ahci,id=ahci \
    -drive if=none,id=ssd,format=qcow2,file="$SSD" \
    -device ide-hd,drive=ssd,bus=ahci.0,rotation_rate=1,serial=TESTSSD001 \
    -drive if=none,id=hdd,format=qcow2,file="$HDD" \
    -device ide-hd,drive=hdd,bus=ahci.1,rotation_rate=7200,serial=TESTHDD001 \
    -drive if=none,id=m2,format=qcow2,file="$NVME" \
    -device nvme,drive=m2,serial=TESTNVME001 \
    $usb_args \
    -nic user,model=virtio-net-pci \
    -audiodev pipewire,id=snd0 -device intel-hda -device hda-output,audiodev=snd0 \
    -device virtio-tablet-pci \
    $gpu_args $cd_args \
    -name "Artix TUI Installer — $mode"
