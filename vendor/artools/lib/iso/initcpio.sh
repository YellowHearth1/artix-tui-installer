#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ initcpio

prepare_initramfs_mkinitcpio() {
    local mnt="$1" mkinitcpio_conf k

    mkinitcpio_conf=mkinitcpio-default.conf
    [[ "${profile}" == 'base' ]] && mkinitcpio_conf=mkinitcpio-pxe.conf
    k=$(<"$mnt"/usr/src/linux/version)

    if [[ -v key_export ]]; then
        exec {ARTIX_GNUPG_FD}<"${key_export}"
        export ARTIX_GNUPG_FD
    fi

    artix-chroot "$mnt" mkinitcpio -k "$k" \
        -c /etc/"$mkinitcpio_conf" \
        -g /boot/initramfs.img

    if [[ -v key_export ]]; then
        exec {ARTIX_GNUPG_FD}<&-
        unset ARTIX_GNUPG_FD
    fi
    rm -rf -- "${key_export}"
    cp "$mnt"/boot/initramfs.img "${iso_root}"/boot/initramfs-"${arch}".img
    prepare_boot_extras "$mnt"
}

configure_grub_mkinitcpio() {
    msg "Configuring grub kernel options ..."
    local ro_opts=()
    local rw_opts=()
    local kopts=("label=${iso_label}")

    [[ "${profile}" != 'base' ]] && kopts+=('overlay=livefs')

    sed -e "s|@kopts@|${kopts[*]}|" \
        -e "s|@ro_opts@|${ro_opts[*]}|" \
        -e "s|@rw_opts@|${rw_opts[*]}|" \
        -i "${iso_root}"/boot/grub/kernels.cfg
}

#}}}
