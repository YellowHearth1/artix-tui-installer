#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ squash

make_checksum(){
    local file="$1"
    msg2 "Creating md5sum ..."
    cd "${iso_root}${live_dir}"
    md5sum "$file" > "$file".md5
    cd "${OLDPWD}"
}

make_sig () {
    local file="$1"
    msg2 "Creating signature file..."

    chown "${owner}:$(id --group "${owner}")" "${iso_root}${live_dir}"
    su "${owner}" -c "gpg --batch --no-armor --no-include-key-block --output $file.sig  --detach-sign \
        --default-key ${GPG_KEY} ${gpg_options[@]} $file"
    chown "root:root" "${iso_root}${live_dir}"
}

make_ext_img(){
    local src="$1"
    local size=32G
    local mnt="${mnt_dir}/${src##*/}"
    mkdir -p "${work_dir}"/embed"${live_dir}"
    local extimg="${work_dir}/embed${live_dir}/${src##*/}".img

    msg2 "Creating ext4 image of %s ..." "${size}"
    truncate -s ${size} "${extimg}"
    local ext4_args=()
    ext4_args+=("-O ^has_journal,^resize_inode" -E lazy_itable_init=0 -m 0)
    mkfs.ext4 "${ext4_args[@]}" -F "${extimg}" &>/dev/null
    tune2fs -c 0 -i 0 "${extimg}" &> /dev/null
    mount_img "${extimg}" "${mnt}"
    msg2 "Copying %s ..." "${src}/"
    cp -aT "${src}/" "${mnt}/"
    umount_img "${mnt}"
}

has_changed(){
    local src="$1" dest="$2"
    if [[ -f "${dest}" ]]; then
        local has_changes
        has_changes=$(find "${src}" -newer "${dest}")
        if [[ -n "${has_changes}" ]]; then
            msg2 "Possible changes for %s ..." "${src}"
            msg2 "%s" "${has_changes}"
            msg2 "SquashFS image %s is not up to date, rebuilding..." "${dest}"
            rm "${dest}"
        else
            msg2 "SquashFS image %s is up to date, skipping." "${dest}"
            return 1
        fi
    fi
}

# $1: image path
make_sfs() {
    local sfs_in="$1"
    if [[ ! -e "${sfs_in}" ]]; then
        error "The path %s does not exist" "${sfs_in}"
        retrun 1
    fi

    mkdir -p "${iso_root}${live_dir}"

    local img_name=${sfs_in##*/}.img

    local sfs_out="${iso_root}${live_dir}/${img_name}"

    if has_changed "${sfs_in}" "${sfs_out}"; then

        msg "Generating SquashFS image for %s" "${sfs_in}"

        local mksfs_args=()

        if ${persist};then
            make_ext_img "${sfs_in}"
            mksfs_args+=("${work_dir}/embed")
        else
            mksfs_args+=("${sfs_in}")
        fi

        mksfs_args+=("${sfs_out}")

        mksfs_args+=(-comp "${COMPRESSION}" "${COMPRESSION_ARGS[@]}" -noappend)

        mksquashfs "${mksfs_args[@]}"

        if ! ${use_dracut}; then
            make_checksum "${img_name}"
            if [[ -n ${GPG_KEY} ]];then
                make_sig "${iso_root}${live_dir}/${img_name}"
            fi
        fi
        if ${persist}; then
            rm -r "${work_dir}/embed"
        fi
    fi
}

#}}}
