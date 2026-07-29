#!/bin/bash
# Managed by the Artix installer.
run_latehook() {
    if grep -qw artix.rollback /proc/cmdline 2>/dev/null; then
        local dev
        # $root and resolve_device come from the initramfs environment that
        # sources this hook (mkinitcpio's init) — they are never assigned here.
        # shellcheck disable=SC2154
        dev="$(resolve_device "${root}" 2>/dev/null)" || dev="${root}"
        [ -n "$dev" ] || return 0
        mkdir -p /run/artix-rb
        if mount -o subvolid=5 "$dev" /run/artix-rb 2>/dev/null; then
            /usr/local/bin/artix-rollback --rollback-initramfs /run/artix-rb <>/dev/console 1>&0 2>&0
            # Second commit layer (the picker already commits, but it may exit
            # early or crash mid-swap): flush page cache and force a btrfs
            # transaction commit so the swapped @ is fully on disk BEFORE we
            # release the pool. Then release cleanly — falling back to a lazy
            # umount so a transient EBUSY can't leave the pool mounted and block
            # the init's root mount. This is what guarantees the very next mount
            # of the SAME device (the real root, subvol=@) sees the new @ on the
            # first try, on every bootloader.
            sync 2>/dev/null
            btrfs filesystem sync /run/artix-rb 2>/dev/null
            umount /run/artix-rb 2>/dev/null || umount -l /run/artix-rb 2>/dev/null
        fi
    fi
}
