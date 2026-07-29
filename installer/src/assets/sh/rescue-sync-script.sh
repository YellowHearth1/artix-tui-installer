#!/bin/sh
# Managed by the Artix installer: keep /boot/vmlinuz-artix-rescue (+ its
# initramfs) in sync with the newest kernel that has PROVEN it can boot.
grep -qw artix.rollback /proc/cmdline 2>/dev/null && exit 0
# A pending post-rollback fixup owns the pair right now - let it finish (it
# refreshes the pair itself once /boot is reconciled).
[ -f /var/lib/artix-rollback/fixup-pending ] && exit 0
setsid sh -c '
    ARTIX_LOG_TAG=rescue-sync
    if ! . /usr/local/lib/artix-installer/log.sh 2>/dev/null; then
        log() { :; }
        warn() { :; }
    fi
    # 30 s of uptime as the "this kernel boots fine" proxy, then sync once.
    sleep 30
    live=""
    for k in /boot/vmlinuz-*; do
        [ -e "$k" ] || exit 0
        case "$k" in *artix-rescue*) continue ;; esac
        live="$k"; break
    done
    [ -n "$live" ] || { warn "no live kernel found in /boot - rescue pair left as is"; exit 0; }
    init="/boot/initramfs-${live#/boot/vmlinuz-}.img"
    [ -f "$init" ] || { warn "no initramfs beside $live - rescue pair left as is"; exit 0; }
    # Only sync when the RUNNING kernel IS the live one. Booting the rescue
    # (or any other) kernel proves nothing about the live pair - syncing then
    # could poison the known-good copy with a broken kernel. pacman ships the
    # image at /usr/lib/modules/<ver>/vmlinuz, so identity is a byte compare.
    run="/usr/lib/modules/$(uname -r)/vmlinuz"
    [ -f "$run" ] || exit 0
    l=$(md5sum < "$live" 2>/dev/null | cut -d" " -f1)
    r=$(md5sum < "$run" 2>/dev/null | cut -d" " -f1)
    [ "$l" = "$r" ] || { log "running kernel is not $live - proves nothing, not syncing"; exit 0; }
    a=$(md5sum < "$live" 2>/dev/null | cut -d" " -f1)
    b=$(md5sum < /boot/vmlinuz-artix-rescue 2>/dev/null | cut -d" " -f1)
    if [ "$a" != "$b" ]; then
        cp -f "$live" /boot/vmlinuz-artix-rescue
        cp -f "$init" /boot/initramfs-artix-rescue.img
        sync
        log "rescue pair refreshed from $live (it booted, so it is known good)"
    else
        log "rescue pair already matches $live - nothing to do"
    fi
' >/dev/null 2>&1 &
exit 0
