#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ message

#set +u +o posix

MAKEPKG_LIBRARY=${MAKEPKG_LIBRARY:-/usr/share/makepkg}

# shellcheck disable=1091
source "${MAKEPKG_LIBRARY}"/util.sh

export LANG=C

if [[ -t 2 && "$TERM" != dumb ]] || [[ ${ARTOOLS_COLOR} == always ]]; then
    colorize
else
    # shellcheck disable=2034
    declare -gr ALL_OFF='' BOLD='' BLUE='' GREEN='' RED='' YELLOW=''
fi

stat_busy() {
    local mesg=$1; shift
    # shellcheck disable=2059
    printf "${GREEN}==>${ALL_OFF}${BOLD} ${mesg}...${ALL_OFF}" "$@" >&2
}

stat_progress() {
    # shellcheck disable=2059
    printf "${BOLD}.${ALL_OFF}" >&2
}

stat_done() {
    # shellcheck disable=2059
    printf "${BOLD}done${ALL_OFF}\n" >&2
}

lock_close() {
    local fd=$1
    exec {fd}>&-
}

lock() {
    if ! [[ "/dev/fd/$1" -ef "$2" ]]; then
        mkdir -p -- "$(dirname -- "$2")"
        eval "exec $1>"'"$2"'
    fi
    if ! flock -n "$1"; then
        stat_busy "$3"
        flock "$1"
        stat_done
    fi
}

slock() {
    if ! [[ "/dev/fd/$1" -ef "$2" ]]; then
        mkdir -p -- "$(dirname -- "$2")"
        eval "exec $1>"'"$2"'
    fi
    if ! flock -sn "$1"; then
        stat_busy "$3"
        flock -s "$1"
        stat_done
    fi
}

_setup_workdir=false
setup_workdir() {
    [[ -z ${WORKDIR:-} ]] && WORKDIR=$(mktemp -d --tmpdir "${0##*/}.XXXXXXXXXX")
    _setup_workdir=true
    trap 'trap_abort' INT QUIT TERM HUP
    trap 'trap_exit' EXIT
}

trap_abort() {
    trap - EXIT INT QUIT TERM HUP
    abort
}

trap_exit() {
    local r=$?
    trap - EXIT INT QUIT TERM HUP
    cleanup "$r"
}

cleanup() {
    if [[ -n ${WORKDIR:-} ]] && $_setup_workdir; then
        rm -rf "$WORKDIR"
    fi
    exit "${1:-0}"
}

abort() {
    error 'Aborting...'
    cleanup 255
}

die() {
    (( $# )) && error "$@"
    cleanup 255
}

msg_success() {
    local msg=$1
    local padding
    padding=$(echo "${msg}"|sed -E 's/( *).*/\1/')
    msg=$(echo "${msg}"|sed -E 's/ *(.*)/\1/')
    printf "%s %s\n" "${padding}${GREEN}✓${ALL_OFF}" "${msg}" >&2
}

msg_error() {
    local msg=$1
    local padding
    padding=$(echo "${msg}"|sed -E 's/( *).*/\1/')
    msg=$(echo "${msg}"|sed -E 's/ *(.*)/\1/')
    printf "%s %s\n" "${padding}${RED}x${ALL_OFF}" "${msg}" >&2
}

msg_warn() {
    local msg=$1
    local padding
    padding=$(echo "${msg}"|sed -E 's/( *).*/\1/')
    msg=$(echo "${msg}"|sed -E 's/ *(.*)/\1/')
    printf "%s %s\n" "${padding}${YELLOW}!${ALL_OFF}" "${msg}" >&2
}

#}}}
