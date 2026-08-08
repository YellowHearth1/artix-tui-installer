#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ mount

ignore_error() {
    "$@" 2>/dev/null
    return 0
}

chroot_add_mount() {
#     msg2 "mount: [%s]" "$2"
    mount "$@" && CHROOT_ACTIVE_MOUNTS=("$2" "${CHROOT_ACTIVE_MOUNTS[@]}")
}

chroot_maybe_add_mount() {
    local cond=$1; shift
    if eval "$cond"; then
        chroot_add_mount "$@"
    fi
}

chroot_setup(){
    local mnt="$1"
    local tmpfs_opts="${2:-mode=1777,strictatime,nodev,nosuid}"

    CHROOT_ACTIVE_MOUNTS=()
    [[ $(trap -p EXIT) ]] && die 'Error! Attempting to overwrite existing EXIT trap'
    trap 'chroot_teardown' EXIT

    #chroot_maybe_add_mount "! mountpoint -q '$mnt'" "$mnt" "$mnt" --bind &&
    chroot_add_mount proc "$mnt/proc" -t proc -o nosuid,noexec,nodev &&
    chroot_add_mount sys "$mnt/sys" -t sysfs -o nosuid,noexec,nodev,ro &&
    ignore_error chroot_maybe_add_mount "[[ -d '$mnt/sys/firmware/efi/efivars' ]]" \
        efivarfs "$mnt/sys/firmware/efi/efivars" -t efivarfs -o nosuid,noexec,nodev &&
    chroot_add_mount udev "$mnt/dev" -t devtmpfs -o mode=0755,nosuid &&
    chroot_add_mount devpts "$mnt/dev/pts" -t devpts -o mode=0620,gid=5,nosuid,noexec &&
    chroot_add_mount shm "$mnt/dev/shm" -t tmpfs -o mode=1777,nosuid,nodev &&
    chroot_add_mount /run "$mnt/run" -t tmpfs -o nosuid,nodev,mode=0755 &&
    chroot_add_mount tmp "$mnt/tmp" -t tmpfs -o "${tmpfs_opts}"
}

chroot_teardown() {
    if (( ${#CHROOT_ACTIVE_MOUNTS[@]} )); then
#         msg2 "umount: [%s]" "${CHROOT_ACTIVE_MOUNTS[@]}"
        umount "${CHROOT_ACTIVE_MOUNTS[@]}"
    fi
    unset CHROOT_ACTIVE_MOUNTS
}

resolve_link() {
    local target=$1
    local root=$2

    # If a root was given, make sure it ends in a slash.
    [[ -n $root && $root != */ ]] && root=$root/

    while [[ -L $target ]]; do
        target=$(readlink -m "$target")
        # If a root was given, make sure the target is under it.
        # Make sure to strip any leading slash from target first.
        [[ -n $root && $target != $root* ]] && target=$root${target#/}
    done

    printf %s "$target"
}

chroot_add_resolv_conf() {
    local chrootdir=$1
    local src
    local dest="$chrootdir/etc/resolv.conf"

    src=$(resolve_link /etc/resolv.conf)

    # If we don't have a source resolv.conf file, there's nothing useful we can do.
    [[ -e $src ]] || return 0

    if [[ ! -e "$dest" && ! -h "$dest" ]]; then
            # There may be no resolv.conf in the chroot. In this case, we'll just exit.
            # The chroot environment must not be concerned with DNS resolution.
            return 0
    fi

    chroot_add_mount "$src" "$dest" -c --bind
}

#}}}
