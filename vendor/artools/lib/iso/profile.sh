#!/hint/bash
#
# SPDX-License-Identifier: GPL-3.0-or-later

#{{{ profile

show_profile(){
    msg2 "iso_file: %s" "${iso_file}"
    msg2 "AUTOLOGIN: %s" "${AUTOLOGIN}"
    msg2 "LIVEUSER: %s" "${LIVEUSER}"
    msg2 "PASSWORD: %s" "${PASSWORD}"
    msg2 "SERVICES: %s" "${SERVICES[*]}"
}

load_profile(){
    local profile_dir="${DATADIR}/iso-profiles"
    [[ -d "${WORKSPACE_DIR}"/iso-profiles ]] && profile_dir="${WORKSPACE_DIR}"/iso-profiles

    root_overlay="$profile_dir/${profile}/root-overlay"

    [[ -d "$profile_dir/${profile}/live-overlay" ]] && live_overlay="$profile_dir/${profile}/live-overlay"

    common_dir="${DATADIR}/iso-profiles/common"
    [[ -d "$profile_dir"/common ]] && common_dir="${profile_dir}"/common

    profile_yaml=$profile_dir/${profile}/profile.yaml

    common_yaml="${common_dir}/common.yaml"

    [[ -f $profile_yaml ]] || return 1

    LIVEUSER=$(yq -P '.live-session.user' "$profile_yaml")

    PASSWORD=$(yq -P '.live-session.password' "$profile_yaml")

    AUTOLOGIN=$(yq -P '.live-session.autologin' "$profile_yaml")

    USE_XLIBRE=$(yq -P '.live-session.use-xlibre' "$profile_yaml")

    mapfile -t SERVICES < <(yq -P '.live-session.services[]' "$profile_yaml")

    mapfile -t USER_SERVICES < <(yq -P '.live-session.user-services[]' "$profile_yaml")

    HAS_LIVE=$(yq -P '. | has("livefs")' "$profile_yaml")

    HAS_LIVE_INIT=$(yq -P '.livefs | has("packages-init")' "$profile_yaml")

    return 0
}

load_pkgs(){
    local list="$1"

    local common_base
    local common_apps
    local common_misc
    local common_xorg
    local common_boot
    local common_init
    local packages_root
    local packages_live
    local packages_root_init
    local packages_live_init

    local common_key_init=".packages-init.${INITSYS}[]"
    local root_key_init=".rootfs.packages-init.${INITSYS}[]"
    local live_key_init=".livefs.packages-init.${INITSYS}[]"

    packages=()

    case "$list" in
        rootfs)
            msg2 "Loading Packages: [%s] ..." "common.packages-base"
            mapfile -t common_base < <(yq -P '.packages-base[]' "$common_yaml")

            msg2 "Loading Packages: [%s] ..." "common.packages-init.${INITSYS}"
            mapfile -t common_init < <(common_key_init="$common_key_init" yq -P 'eval(strenv(common_key_init))' "$common_yaml")

            msg2 "Loading Packages: [%s] ..." "common.packages-apps"
            mapfile -t common_apps < <(yq -P '.packages-apps[]' "$common_yaml")

            if "${HAS_LIVE}"; then
                if ${USE_XLIBRE}; then
                    msg2 "Loading Packages: [%s] ..." "common.packages-xlibre"
                    mapfile -t common_xorg < <(yq -P '.packages-xlibre[]' "$common_yaml")
                else
                    msg2 "Loading Packages: [%s] ..." "common.packages-xorg"
                    mapfile -t common_xorg < <(yq -P '.packages-xorg[]' "$common_yaml")
                fi

                msg2 "Loading Packages: [%s] ..." "common.packages-misc"
                mapfile -t common_misc < <(yq -P '.packages-misc[]' "$common_yaml")
            fi

            msg2 "Loading Packages: [%s] ..." "rootfs.packages"
            mapfile -t packages_root < <(yq -P '.rootfs.packages[]' "$profile_yaml")

            msg2 "Loading Packages: [%s] ..." "rootfs.packages-init.${INITSYS}"
            mapfile -t packages_root_init < <(root_key_init="$root_key_init" yq -P 'eval(strenv(root_key_init))' "$profile_yaml")

            packages+=(
                "${common_base[@]}"
                "${common_init[@]}"
                "${common_apps[@]}"
                "${packages_root[@]}"
                "${packages_root_init[@]}"
                "${common_xorg[@]}"
                "${common_misc[@]}"
            )
        ;;
        livefs)
            msg2 "Loading Packages: [%s] ..." "livefs.packages"
            mapfile -t packages_live < <(yq -P '.livefs.packages[]' "$profile_yaml")

            if "${HAS_LIVE_INIT}"; then
                msg2 "Loading Packages: [%s] ..." "livefs.packages-init.${INITSYS}"
                mapfile -t packages_live_init < <(live_key_init="$live_key_init" yq -P 'eval(strenv(live_key_init))' "$profile_yaml")
            fi

            packages+=(
                "${packages_live[@]}"
                "${packages_live_init[@]}"
            )
        ;;
        bootfs)
            msg2 "Loading Packages: [%s] ..." "common.packages-boot"
            mapfile -t common_boot < <(yq -P '.packages-boot[]' "$common_yaml")

            packages+=(
                "${common_boot[@]}"
            )
        ;;
    esac
}

#}}}
