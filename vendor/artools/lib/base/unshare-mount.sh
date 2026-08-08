#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ mount

chroot_add_mount_lazy() {
    mount "$@" && CHROOT_ACTIVE_LAZY=("$2" "${CHROOT_ACTIVE_LAZY[@]}")
}

chroot_bind_device() {
    touch "$2" && CHROOT_ACTIVE_FILES=("$2" "${CHROOT_ACTIVE_FILES[@]}")
    chroot_add_mount "$1" "$2" --bind
}

chroot_add_link() {
    ln -sf "$1" "$2" && CHROOT_ACTIVE_FILES=("$2" "${CHROOT_ACTIVE_FILES[@]}")
}

unshare_setup() {
    CHROOT_ACTIVE_MOUNTS=()
    CHROOT_ACTIVE_LAZY=()
    CHROOT_ACTIVE_FILES=()
    [[ $(trap -p EXIT) ]] && die '(BUG): attempting to overwrite existing EXIT trap'
    trap 'unshare_teardown' EXIT

    chroot_add_mount_lazy "$1" "$1" --bind &&
    chroot_add_mount proc "$1/proc" -t proc -o nosuid,noexec,nodev &&
    chroot_add_mount_lazy /sys "$1/sys" --rbind &&
    chroot_add_link /proc/self/fd "$1/dev/fd" &&
    chroot_add_link /proc/self/fd/0 "$1/dev/stdin" &&
    chroot_add_link /proc/self/fd/1 "$1/dev/stdout" &&
    chroot_add_link /proc/self/fd/2 "$1/dev/stderr" &&
    chroot_bind_device /dev/full "$1/dev/full" &&
    chroot_bind_device /dev/null "$1/dev/null" &&
    chroot_bind_device /dev/random "$1/dev/random" &&
    chroot_bind_device /dev/tty "$1/dev/tty" &&
    chroot_bind_device /dev/urandom "$1/dev/urandom" &&
    chroot_bind_device /dev/zero "$1/dev/zero" &&
    chroot_add_mount run "$1/run" -t tmpfs -o nosuid,nodev,mode=0755 &&
    chroot_add_mount tmp "$1/tmp" -t tmpfs -o mode=1777,strictatime,nodev,nosuid
}

unshare_teardown() {
    chroot_teardown

    if (( ${#CHROOT_ACTIVE_LAZY[@]} )); then
        umount --lazy "${CHROOT_ACTIVE_LAZY[@]}"
    fi
    unset CHROOT_ACTIVE_LAZY

    if (( ${#CHROOT_ACTIVE_FILES[@]} )); then
        rm "${CHROOT_ACTIVE_FILES[@]}"
    fi
    unset CHROOT_ACTIVE_FILES
}

pid_unshare="unshare --fork --pid"
mount_unshare="$pid_unshare --mount --map-auto --map-root-user --setuid 0 --setgid 0"

# This outputs code for declaring all variables to stdout. For example, if
# FOO=BAR, then running
#     declare -p FOO
# will result in the output
#     declare -- FOO="bar"
# This function may be used to re-declare all currently used variables and
# functions in a new shell.
declare_all() {
  # Remove read-only variables to avoid warnings. Unfortunately, declare +r -p
  # doesn't work like it looks like it should (declaring only read-write
  # variables). However, declare -rp will print out read-only variables, which
  # we can then use to remove those definitions.
  declare -p | grep -Fvf <(declare -rp)
  # Then declare functions
  declare -pf
}

#}}}
