#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ grub

prepare_grub(){
    local platform=i386-pc img='core.img' prefix=/boot/grub
    local lib="$1"/usr/lib/grub
    local theme="$1"/usr/share/grub
    local livecfg="$2"/usr/share/grub
    local grub
    grub=${iso_root}/boot/grub efi=${iso_root}/efi/boot

    prepare_dir "${grub}/${platform}"

    cp "${livecfg}"/cfg/*.cfg "${grub}"

    cp "${lib}/${platform}"/* "${grub}/${platform}"

    msg2 "Building %s ..." "${img}"

    grub-mkimage -d "${grub}/${platform}" -o "${grub}/${platform}/${img}" -O "${platform}" -p "${prefix}" biosdisk iso9660

    cat "${grub}/${platform}"/cdboot.img "${grub}/${platform}/${img}" > "${grub}/${platform}"/eltorito.img

    platform=x86_64-efi
    img=bootx64.efi

    prepare_dir "${efi}"
    prepare_dir "${grub}/${platform}"

    cp "${lib}/${platform}"/* "${grub}/${platform}"

    msg2 "Building %s ..." "${img}"

    grub-mkimage -d "${grub}/${platform}" -o "${efi}/${img}" -O "${platform}" -p "${prefix}" iso9660

    prepare_dir "${grub}"/themes

    cp -r "${theme}"/themes/artix "${grub}"/themes
    cp -r "${livecfg}"/{locales,tz} "${grub}"

    if [[ -f /usr/share/grub/unicode.pf2 ]];then
        msg2 "Copying %s ..." "unicode.pf2"
        cp /usr/share/grub/unicode.pf2 "${grub}"/unicode.pf2
    else
        msg2 "Creating %s ..." "unicode.pf2"
        grub-mkfont -o "${grub}"/unicode.pf2 /usr/share/fonts/misc/unifont.bdf
    fi

    local size=4M mnt="${mnt_dir}/efiboot" efi_img="${iso_root}/boot/efi.img"
    msg2 "Creating fat image of %s ..." "${size}"
    truncate -s "${size}" "${efi_img}"
    mkfs.fat -n ARTIX_EFI "${efi_img}" &>/dev/null
    prepare_dir "${mnt}"
    mount_img "${efi_img}" "${mnt}"
    prepare_dir "${mnt}"/efi/boot
    msg2 "Building %s ..." "${img}"
    grub-mkimage -d "${grub}/${platform}" -o "${mnt}"/efi/boot/"${img}" -O "${platform}" -p "${prefix}" iso9660
    umount_img "${mnt}"
}

#}}}
