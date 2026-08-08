#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ firmware

prepare_boot_extras(){
    local src="$1" dest
    dest=${iso_root}/boot

    for fw in intel amd; do
        cp "$src"/boot/"$fw"-ucode.img "$dest/$fw"-ucode.img
    done

    cp "$src"/boot/memtest86+/memtest.bin "$dest"/memtest
}

#}}}
