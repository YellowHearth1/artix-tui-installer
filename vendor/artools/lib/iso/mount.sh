#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ mount

track_img() {
    msg2 "mount: [%s]" "$2"
    mount "$@" && IMG_ACTIVE_MOUNTS=("$2" "${IMG_ACTIVE_MOUNTS[@]}")
}

mount_img() {
    IMG_ACTIVE_MOUNTS=()
    mkdir -p "$2"
    track_img "$1" "$2"
}

umount_img() {
    if [[ -n "${IMG_ACTIVE_MOUNTS[*]}" ]];then
        msg2 "umount: [%s]" "${IMG_ACTIVE_MOUNTS[@]}"
        umount "${IMG_ACTIVE_MOUNTS[@]}"
        unset IMG_ACTIVE_MOUNTS
        rm -r "$1"
    fi
}

track_fs() {
    msg2 "overlayfs mount: [%s]" "$5"
    mount "$@" && FS_ACTIVE_MOUNTS=("$5" "${FS_ACTIVE_MOUNTS[@]}")
}

mount_overlayfs(){
    FS_ACTIVE_MOUNTS=()
    local lower upper="$1" work="$2"
    mkdir -p "${mnt_dir}/work"
    mkdir -p "$upper"
    case $upper in
        */livefs) lower="$work/rootfs" ;;
        */bootfs)
            lower="$work/rootfs"
            [[ -d "$work/livefs" ]] && lower="$work/livefs:$work/rootfs"
        ;;
    esac
    # shellcheck disable=2140
    track_fs -t overlay overlay -olowerdir="$lower",upperdir="$upper",workdir="${mnt_dir}/work" "$upper"
}

umount_overlayfs(){
    if [[ -n "${FS_ACTIVE_MOUNTS[*]}" ]];then
        msg2 "overlayfs umount: [%s]" "${FS_ACTIVE_MOUNTS[@]}"
        umount "${FS_ACTIVE_MOUNTS[@]}"
        unset FS_ACTIVE_MOUNTS
        rm -rf "${mnt_dir}/work"
    fi
}

#}}}
