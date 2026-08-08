#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ iso

get_disturl(){
    # shellcheck disable=SC2034
    . /usr/lib/os-release
    printf "%s\n" "${HOME_URL}"
}

get_osname(){
    # shellcheck disable=SC2034
    . /usr/lib/os-release
    printf "%s\n" "${NAME}"
}

assemble_iso(){
    msg "Creating ISO image..."
    local mod_date
    mod_date=$(date -u +%Y-%m-%d-%H-%M-%S-00  | sed -e s/-//g)
    local appid
    appid="$(get_osname) Live/Rescue CD"
    local publisher
    publisher="$(get_osname) <$(get_disturl)>"

    xorriso -as mkisofs \
        --modification-date="${mod_date}" \
        --protective-msdos-label \
        -volid "${iso_label}" \
        -appid "${appid}" \
        -publisher "${publisher}" \
        -preparer "Prepared by artools/${0##*/}" \
        -r -graft-points -no-pad \
        --sort-weight 0 / \
        --sort-weight 1 /boot \
        --grub2-mbr "${iso_root}"/boot/grub/i386-pc/boot_hybrid.img \
        -partition_offset 16 \
        -b boot/grub/i386-pc/eltorito.img \
        -c boot.catalog \
        -no-emul-boot -boot-load-size 4 -boot-info-table --grub2-boot-info \
        -eltorito-alt-boot \
        -append_partition 2 0xef "${iso_root}"/boot/efi.img \
        -e --interval:appended_partition_2:all:: -iso_mbr_part_type 0x00 \
        -no-emul-boot \
        -iso-level 3 \
        -o "${iso_dir}/${iso_file}" \
        "${iso_root}"/
}

#}}}
