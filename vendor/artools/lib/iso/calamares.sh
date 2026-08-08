#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ calamares

yaml_array() {
    local array yaml

    for entry in "$@"; do
        yaml="{name: ${entry}, action: enable}"
        array="${array:-}${array:+,} ${yaml}"
    done
    printf "%s\n" "[${array}]"
}

configure_calamares(){
    for config in online offline; do
        local mods="$1/etc/calamares-$config/modules"
        if [[ -d "$mods" ]];then
            msg2 "Configuring: Calamares %s" "$config"

            if [[ -f "$mods"/services-artix.conf ]]; then
                local svc usvc
                svc=$(yaml_array "${SERVICES[@]}") \
                    yq -P 'with(.;
                            .command = "artix-service" |
                            .services = env(svc) )' \
                        -i "$mods"/services-artix.conf
            fi
            if [[ -f "$mods"/postcfg.conf ]]; then
                local  usvc initsys
                initsys="${INITSYS}" \
                usvc=$(yaml_array "${USER_SERVICES[@]}") \
                    yq -P 'with(.;
                            .initsys = env(initsys) |
                            .user-services = env(usvc) )' \
                        -i "$mods"/postcfg.conf
            fi
        fi
    done
    if [[ -d "$1"/etc/calamares-offline ]]; then
        ln -sf calamares-offline "$1"/etc/calamares
    fi
}

#}}}
