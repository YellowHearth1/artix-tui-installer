#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ trap

error_function() {
    local func="$1"
    # first exit all subshells, then print the error
    if (( ! BASH_SUBSHELL )); then
        error "A failure occurred in %s()." "$func"
        plain "Aborting..."
    fi
    umount_overlayfs
    umount_img
    exit 2
}

run_safe() {
    local restoretrap func="$1"
    set -e
    set -E
    restoretrap=$(trap -p ERR)
    trap 'error_function $func' ERR

    "$func"

    eval "$restoretrap"
    set +E
    set +e
}

trap_exit() {
    local sig=$1; shift
    error "$@"
    umount_overlayfs
    trap -- "$sig"
    kill "-$sig" "$$"
}

prepare_traps(){
    for sig in TERM HUP QUIT; do
        # shellcheck disable=2064
        trap "trap_exit $sig \"$(gettext "%s signal caught. Exiting...")\" \"$sig\"" "$sig"
    done
    trap 'trap_exit INT "$(gettext "Aborted by user! Exiting...")"' INT
#     trap 'trap_exit USR1 "$(gettext "An unknown error has occurred. Exiting...")"' ERR
}

#}}}
