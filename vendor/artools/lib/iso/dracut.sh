#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ dracut

prepare_initramfs_dracut(){
    local mnt="$1"
    local kver
    kver=$(<"$mnt"/usr/src/linux/version)

    printf "%s\n" 'add_dracutmodules+=" dmsquash-live"' > "$mnt"/etc/dracut.conf.d/50-live.conf

    msg "Starting build: %s" "${kver}"
    artix-chroot "$mnt" dracut -fqM /boot/initramfs.img "$kver"
    msg "Image generation successful"

    cp "$mnt"/boot/initramfs.img "${iso_root}"/boot/initramfs-"${arch}".img

    prepare_boot_extras "$mnt"
}

configure_grub_dracut(){
    msg "Configuring grub kernel options ..."
    local kopts=()
    kopts=(
        "root=live:LABEL=${iso_label}"
        'rd.live.squashimg=rootfs.img'
        'rd.live.image'
        'rootflags=auto'
    )
    [[ "${profile}" != 'base' ]] && kopts+=("rd.live.join=livefs.img")

    local ro_opts=()
    local rw_opts=()
#         'rd.writable.fsimg=1'

    sed -e "s|@kopts@|${kopts[*]}|" \
        -e "s|@ro_opts@|${ro_opts[*]}|" \
        -e "s|@rw_opts@|${rw_opts[*]}|" \
        -i "${iso_root}"/boot/grub/kernels.cfg
}

#}}}
