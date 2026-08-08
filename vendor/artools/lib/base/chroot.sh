#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ chroot

check_root() {
    local keepenv=$1
    shift
    local orig_argv=("$@")

    (( EUID == 0 )) && return
    if type -P sudo >/dev/null; then
        exec sudo --preserve-env="${keepenv}" -- "${orig_argv[@]}"
    else
        exec su root -c "$(printf ' %q' "${orig_argv[@]}")"
    fi
}


is_btrfs() {
    [[ -e "$1" && "$(stat -f -c %T "$1")" == btrfs ]]
}

is_subvolume() {
    [[ -e "$1" && "$(stat -f -c %T "$1")" == btrfs && "$(stat -c %i "$1")" == 256 ]]
}

# is_same_fs() {
#     [[ "$(stat -c %d "$1")" == "$(stat -c %d "$2")" ]]
# }

subvolume_delete_recursive() {
    local subvol

    is_subvolume "$1" || return 0

    while IFS= read -d $'\0' -r subvol; do
        if ! subvolume_delete_recursive "$subvol"; then
            return 1
        fi
    done < <(find "$1" -mindepth 1 -xdev -depth -inum 256 -print0)
    if ! btrfs subvolume delete "$1" &>/dev/null; then
        error "Unable to delete subvolume %s" "$subvol"
        return 1
    fi

    return 0
}

# }}}
