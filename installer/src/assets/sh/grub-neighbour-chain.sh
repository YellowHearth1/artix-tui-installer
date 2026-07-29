#!/bin/sh
# GRUB.D GENERATOR — Managed by the Artix installer. Installed as
# /etc/grub.d/35_artix_neighbours and run by grub-mkconfig on EVERY regeneration,
# so the dual-boot menu ALWAYS lists the current neighbouring OS loader(s) on
# every EFI System Partition — including a system installed AFTER this one, which
# a one-time scan at install could never have seen.
#
# Why os-prober alone is not enough: linux-boot-prober mounts the neighbour's
# root and looks for kernels — but a system whose ESP is mounted at /boot keeps
# its kernels ON THE ESP (this installer's own default layout does exactly
# that). The mounted root then shows an empty /boot mountpoint, os-prober finds
# nothing, and the neighbour silently vanishes from the menu. Chainloading its
# EFI loader boots it regardless of where its kernels live.
#
# As a generator it prints menuentries to STDOUT (grub-mkconfig captures that
# into grub.cfg); progress notes go to STDERR. Windows is skipped — os-prober
# handles it. It is best-effort: any failure must not break grub-mkconfig, so
# nothing runs under `set -e` and it always exits 0.
#
# "The other OS" is very often another LINUX, quite possibly another Artix put
# here by this very installer. So the only directory skipped is OUR OWN,
# identified by the root filesystem UUID stored inside the .artix-tui-owned
# marker — skipping every marked directory would hide a neighbouring Artix.
# findmnt answers from the udev/blkid cache, which can be missing or empty on a
# freshly installed system. Fall back to probing the root device directly (the
# [/subvol] suffix btrfs adds to SOURCE has to come off first). This matters:
# with an empty own_root the ownership test below can never match, so the script
# treats OUR OWN loader as a neighbour and offers a menu entry that chainloads
# us into our own menu — seen exactly that way on a btrfs install.
own_root=$(findmnt -no UUID / 2>/dev/null)
if [ -z "$own_root" ]; then
    own_src=$(findmnt -no SOURCE / 2>/dev/null | sed 's/\[.*//')
    own_root=$(blkid -s UUID -o value "$own_src" 2>/dev/null)
fi
# Records "uuid:name" pairs already emitted, so a neighbour reachable via two
# ESPs is not listed twice in one run.
seen=$(mktemp 2>/dev/null) || seen="/tmp/artix-neigh-seen.$$"
: >"$seen" 2>/dev/null

emit_esp() {
    # $1 = mounted root of the ESP, $2 = its filesystem UUID, $3 = its device
    for d in "$1"/EFI/*/; do
        [ -d "$d" ] || continue
        name=$(basename "$d")
        case "$name" in
            [Bb][Oo][Oo][Tt] | [Mm]icrosoft | tools) continue ;;
        esac
        # Ours only if the marker names THIS system's root. A neighbour put here
        # by the same installer carries the same FILE with a different UUID, and
        # must be chainloaded like any other OS.
        owner=$(cat "${d}.artix-tui-owned" 2>/dev/null)
        if [ -n "$own_root" ] && [ "$owner" = "$own_root" ]; then
            continue
        fi
        for l in grubx64.efi shimx64.efi systemd-bootx64.efi refind_x64.efi BOOTX64.EFI; do
            if [ -f "${d}${l}" ]; then
                key="$2:$name"
                grep -qxF "$key" "$seen" 2>/dev/null && break
                printf '%s\n' "$key" >>"$seen" 2>/dev/null
                # The title carries the DEVICE as well as the name: two installs
                # on different disks can both be called "Artix" (same distro,
                # same default bootloader-id), and two identical rows are useless
                # to choose between. Booting still goes by fs-uuid, so a device
                # name that shifts between boots only ever affects the label.
                # No `savedefault` here. It writes grubenv through a block list,
                # which errors with "sparse file not allowed" and then halts the
                # boot on "Press any key to continue" whenever the file is not a
                # contiguous run of blocks. Remembering which neighbour was
                # picked last is not worth stopping every boot to say so.
                cat <<EOF
menuentry "$name (EFI/$name on $3)" {
  insmod part_gpt
  insmod fat
  insmod chain
  search --no-floppy --fs-uuid --set=root $2
  chainloader /EFI/$name/$l
}
EOF
                echo ">>> Boot menu: neighbour '$name' (ESP $2 on $3)" >&2
                break
            fi
        done
    done
}

# Our own (already mounted) ESP first — it is the one a same-disk neighbour
# shares with us, so it is where "Artix beside Artix" finds its predecessor.
own_dir='@@ESP_DIR@@'
own_dev=$(findmnt -no SOURCE "$own_dir" 2>/dev/null)
own_uuid=$(blkid -s UUID -o value "$own_dev" 2>/dev/null)
[ -n "$own_uuid" ] && emit_esp "$own_dir" "$own_uuid" "$own_dev"

# Every OTHER EFI System Partition (a neighbour OS on its own disk keeps its
# loader on its own ESP), mounted read-only just long enough to look.
#
# Enumerated with lsblk, NOT `blkid -t PARTTYPE=... -o device`. That form asks
# the blkid CACHE, and PARTTYPE is not cached — it is a partition-table
# attribute blkid only reports when probing a device directly (`blkid -p`). So
# the query returned nothing on every system, and this whole loop was dead:
# neighbours on their own disk were never found. Verified on a live machine
# whose /dev/sda1 is an ESP — lsblk lists it, `blkid -t PARTTYPE=` prints
# nothing at all.
tmp="/run/artix-espscan.$$"
mkdir -p "$tmp" 2>/dev/null
esp_guid=c12a7328-f81f-11d2-ba4b-00a0c93ec93b
for dev in $(lsblk -rno PATH,PARTTYPE 2>/dev/null |
    awk -v g="$esp_guid" 'tolower($2) == g { print $1 }'); do
    [ "$dev" = "$own_dev" ] && continue
    uuid=$(blkid -s UUID -o value "$dev" 2>/dev/null)
    [ -n "$uuid" ] || continue
    if mount -o ro "$dev" "$tmp" 2>/dev/null; then
        emit_esp "$tmp" "$uuid" "$dev"
        umount "$tmp" 2>/dev/null
    fi
done
rmdir "$tmp" 2>/dev/null
rm -f "$seen" 2>/dev/null
true
