#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ services

add_svc_openrc(){
    local mnt="$1" rlvl="${2:-default}"
    for svc in "${SERVICES[@]}"; do
        if [[ -f $mnt/etc/init.d/$svc ]];then
            msg2 "Setting %s: [%s]" "${INITSYS}" "$svc"
            chroot "$mnt" rc-update add "$svc" "$rlvl" &>/dev/null
        else
            warning "Service %s not found. Skipping." "$svc"
        fi
    done
}

add_user_svc_openrc(){
    local mnt="$1" rlvl="${2:-default}"
    for svc in "${USER_SERVICES[@]}"; do
        if [[ -f "$mnt"/etc/user/init.d/"$svc" ]];then
            msg2 "Setting user %s: [%s]" "${INITSYS}" "$svc"
            local rc=".config/rc/runlevels/default"
            chroot "$mnt" mkdir -p /home/"${LIVEUSER}/$rc"
            chroot "$mnt" ln -s /etc/user/init.d/"$svc" /home/"${LIVEUSER}/$rc/$svc"
            chroot "$mnt" chown -R "${LIVEUSER}:${LIVEUSER}" /home/"${LIVEUSER}"/.config/rc
        else
            warning "Service %s not found. Skipping." "$svc"
        fi
    done
}

add_svc_runit(){
    local mnt="$1" rlvl="${2:-default}"
    for svc in "${SERVICES[@]}"; do
        if [[ -d $mnt/etc/runit/sv/$svc ]]; then
            msg2 "Setting %s: [%s]" "${INITSYS}" "$svc"
            chroot "$mnt" ln -s /etc/runit/sv/"$svc" /etc/runit/runsvdir/"$rlvl" &>/dev/null
        else
            warning "Service %s not found. Skipping." "$svc"
        fi
    done
}

add_svc_s6(){
    local mnt="$1" rlvl="${2:-default}" dep
    local display_manager
    local supported_dms=(sddm gdm lightdm mdm greetd lxdm xdm)

    for dm in "${supported_dms[@]}"; do
        if in_array "$dm" "${SERVICES[@]}"; then
            display_manager="$dm"
        fi
    done

    dep="$mnt"/etc/s6/sv/"$display_manager"-srv/dependencies.d

    for svc in "${SERVICES[@]}"; do
        msg2 "Setting %s: [%s]" "${INITSYS}" "$svc"
        if [[ -d "$mnt"/etc/s6/sv/"$svc" ]] || [[ -d "$mnt"/etc/s6/sv/"$svc"-srv ]]; then
            if [[ "$svc" == "$display_manager" ]]; then
                if [[ -d "$dep" ]]; then
                    touch "$dep"/artix-live
                fi
            fi
            chroot "$mnt" s6 set enable --pull-dependencies "$svc"
        else
            warning "Service %s not found. Skipping." "$svc"
        fi
    done

    chroot "$mnt" s6 repository sync && chroot "$mnt" s6 set commit && chroot "$mnt" s6 live install --init

    local src=/etc/s6/current skel=/etc/s6/skel getty='/usr/bin/agetty -L -8 tty7 115200'
    # rebuild s6-linux-init binaries
    chroot "$mnt" rm -r "$src"
    chroot "$mnt" s6-linux-init-maker -1 -N -f "$skel" -G "$getty" -c "$src" "$src"
    chroot "$mnt" mv "$src"/bin/init "$src"/bin/s6-init
    chroot "$mnt" cp -a "$src"/bin /usr
}

add_svc_dinit(){
    local mnt="$1"
    for svc in "${SERVICES[@]}"; do
        if [[ -f "$mnt"/etc/dinit.d/"$svc" ]]; then
            msg2 "Setting %s: [%s]" "${INITSYS}" "$svc"
            chroot "$mnt" ln -s /etc/dinit.d/"$svc" /etc/dinit.d/boot.d/ &>/dev/null
        else
            warning "Service %s not found. Skipping." "$svc"
        fi
    done
    chroot "$mnt" ln -s /usr/lib/dinit.d/dinit-user-spawn /etc/dinit.d/boot.d/ &>/dev/null
}

add_user_svc_dinit(){
    local mnt="$1"
    for svc in "${USER_SERVICES[@]}"; do
        if [[ -f "$mnt"/etc/dinit.d/user/"$svc" ]]; then
            msg2 "Setting user %s: [%s]" "${INITSYS}" "$svc"
            local usr_sv="/home/${LIVEUSER}/.config/dinit.d"
            chroot "$mnt" mkdir -p "$usr_sv"/boot.d
            chroot "$mnt" ln -s /etc/dinit.d/user/"$svc" "$usr_sv"/boot.d/"$svc"
            chroot "$mnt" chown -R "${LIVEUSER}:${LIVEUSER}" "$usr_sv"
        else
            warning "Service %s not found. Skipping." "$svc"
        fi
    done
}

#}}}
