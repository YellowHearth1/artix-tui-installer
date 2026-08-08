#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ iso conf

prepare_dir(){
    [[ ! -d $1 ]] && mkdir -p "$1"
    return 0
}

if [[ -n $SUDO_USER ]]; then
    eval "USER_HOME=~$SUDO_USER"
else
    USER_HOME=$HOME
fi

USER_CONF_DIR="${XDG_CONFIG_HOME:-$USER_HOME/.config}/artools"

prepare_dir "${USER_CONF_DIR}"

load_iso_config(){

    local conf="$1/artools-iso.conf"

    [[ -f "$conf" ]] || return 1

    # shellcheck source=config/conf/artools-iso.conf
    [[ -r "$conf" ]] && source "$conf"

    CHROOTS_DIR=${CHROOTS_DIR:-'/var/lib/artools'}

    WORKSPACE_DIR=${WORKSPACE_DIR:-"${USER_HOME}/artools-workspace"}

    ARCH=${ARCH:-"$(uname -m)"}

    STABILITY=${STABILITY:-'stable'}

    ISO_POOL=${ISO_POOL:-"${WORKSPACE_DIR}/iso"}

    ISO_VERSION=${ISO_VERSION:-"$(date +%Y%m%d)"}

    INITSYS=${INITSYS:-'openrc'}

    GPG_KEY=${GPG_KEY:-''}

    COMPRESSION="${COMPRESSION:-zstd}"

    COMPRESSION_LEVEL="${COMPRESSION_LEVEL:-15}"

    if [[ -z "${COMPRESSION_ARGS[*]}" ]]; then
        COMPRESSION_ARGS=(-Xcompression-level "${COMPRESSION_LEVEL}")
    fi

    if [[ "${COMPRESSION}" == 'xz' ]]; then
        COMPRESSION_ARGS=(-Xbcj x86)
    fi

    return 0
}

#}}}

load_iso_config "${USER_CONF_DIR}" || load_iso_config "${SYSCONFDIR}"

prepare_dir "${ISO_POOL}"
