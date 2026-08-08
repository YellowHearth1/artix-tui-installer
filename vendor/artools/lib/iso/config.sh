#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ session

configure_user(){
    local -r grps="users,lp,video,network,storage,wheel,audio,power,log,optical,network,scanner"

    chroot "$1" useradd -m -G "$grps" -s /bin/bash "${LIVEUSER}"
    echo "${LIVEUSER}:${PASSWORD}" | chroot "$1" chpasswd
    echo "root:${PASSWORD}" | chroot "$1" chpasswd
}

configure_services(){
    local mnt="$1"
    add_svc_"${INITSYS}" "$mnt"

    if [[ "${INITSYS}" == "openrc" ]] || [[ "${INITSYS}" == "dinit" ]]; then
        add_user_svc_"${INITSYS}" "$mnt"
    fi
}


write_live_session_conf(){
    local conf=''
    conf+=$(printf '%s\n' '# live session configuration')
    conf+=$(printf "\nAUTOLOGIN=%s\n" "${AUTOLOGIN}")
    conf+=$(printf "\nLIVEUSER=%s\n" "${LIVEUSER}")
    printf '%s' "$conf"
}

configure_chroot(){
    local fs="$1"
    msg "Configuring [%s]" "${fs##*/}"
    configure_user "$fs"
    configure_services "$fs"
    configure_calamares "$fs"
    [[ ! -d "$fs/etc/artools" ]] && mkdir -p "$fs/etc/artools"
    msg2 "Writing: live.conf"
    write_live_session_conf > "$fs/etc/artools/live.conf"
    msg "Done configuring [%s]" "${fs##*/}"
}

#}}}
