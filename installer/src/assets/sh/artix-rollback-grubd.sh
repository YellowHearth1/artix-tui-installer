#!/bin/sh
# Managed by the Artix installer. Adds a "System Rollback" GRUB entry that boots
# the FROZEN RESCUE kernel/initramfs (falling back to the live pair) with
# artix.rollback so the early-boot snapshot picker runs — even when a kernel
# update broke the live kernel. Device access mirrors 10_linux via
# grub-mkconfig_lib.
set -e
. /usr/share/grub/grub-mkconfig_lib
RESCUE=1
K="/boot/vmlinuz-artix-rescue"
I="/boot/initramfs-artix-rescue.img"
if [ ! -e "$K" ] || [ ! -e "$I" ]; then
    RESCUE=0
    K="/boot/@@VMLINUZ@@"
    I="/boot/@@INITRD@@"
fi
[ -e "$K" ] || exit 0
. /etc/default/grub 2>/dev/null || true
dev="$("${grub_probe:-grub-probe}" --target=device "$K")"
rk="$(make_system_path_relative_to_its_root "$K")"
ri="$(make_system_path_relative_to_its_root "$I")"
# root= is REQUIRED on the kernel cmdline. 10_linux adds it automatically for the
# normal entries, but a hand-written menuentry must set it itself — without it
# the initramfs has an EMPTY root device and drops straight to an emergency
# shell ("device '' not found", "Failed to mount '' on real root"). The rollback
# hook ALSO reads ${root} from this same parameter, so an empty root= makes the
# whole rollback a no-op AND breaks the boot. Resolve the ROOT filesystem (`/`),
# not where the kernel file happens to live: the two differ when /boot is a
# separate or ESP partition (the encrypted-/boot layout the rescue entry exists
# for). Encrypted root resolves to the btrfs UUID inside the LUKS container,
# exactly as the normal entry does; cryptdevice= rides in GRUB_CMDLINE_LINUX.
rootdev="$("${grub_probe:-grub-probe}" --target=device / 2>/dev/null)"
rootuuid="$("${grub_probe:-grub-probe}" --device "$rootdev" --target=fs_uuid 2>/dev/null)"
if [ -n "$rootuuid" ]; then
    rootarg="root=UUID=$rootuuid"
else
    rootarg="root=$rootdev"
fi
uc=""
for u in intel-ucode.img amd-ucode.img; do
    [ -f "/boot/$u" ] && uc="$uc $(make_system_path_relative_to_its_root "/boot/$u")"
done
printf "%s\n" "menuentry 'Artix - System Rollback (pick a snapshot)' --class artix --class gnu-linux {"
prepare_grub_to_access_device "$dev"
printf "\tlinux %s %s rootflags=subvol=@ %s %s artix.rollback\n" "$rk" "$rootarg" "$GRUB_CMDLINE_LINUX" "$GRUB_CMDLINE_LINUX_DEFAULT"
printf "\tinitrd%s %s\n" "$uc" "$ri"
printf "}\n"
# Plain boot on the frozen rescue pair - the escape hatch when a kernel update
# broke the live kernel and /boot lives OUTSIDE the btrfs pool (encrypted
# /boot partition), where a snapshot rollback alone cannot restore /boot.
if [ "$RESCUE" = 1 ]; then
printf "%s\n" "menuentry 'Artix - rescue kernel (known good)' --class artix --class gnu-linux {"
prepare_grub_to_access_device "$dev"
printf "\tlinux %s %s rootflags=subvol=@ %s %s\n" "$rk" "$rootarg" "$GRUB_CMDLINE_LINUX" "$GRUB_CMDLINE_LINUX_DEFAULT"
printf "\tinitrd%s %s\n" "$uc" "$ri"
printf "}\n"
fi
