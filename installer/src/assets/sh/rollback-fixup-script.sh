#!/bin/sh
# Managed by the Artix installer: post-rollback /boot reconciliation.
FLAG=/var/lib/artix-rollback/fixup-pending
[ -f "$FLAG" ] || exit 0
setsid sh -c '
    ARTIX_LOG_TAG=rollback-fixup
    if ! . /usr/local/lib/artix-installer/log.sh 2>/dev/null; then
        log() { :; }
        warn() { :; }
    fi
    log "post-rollback reconciliation started"
    # Reinstall the kernel image(s) that belong to the RESTORED root: every
    # /usr/lib/modules/<ver> ships its vmlinuz + pkgbase (linux, linux-lts, …).
    for d in /usr/lib/modules/*/; do
        [ -f "${d}vmlinuz" ] && [ -f "${d}pkgbase" ] || continue
        install -Dm644 "${d}vmlinuz" "/boot/vmlinuz-$(cat "${d}pkgbase")"
        log "restored kernel image for $(cat "${d}pkgbase")"
    done
    if mkinitcpio -P >/dev/null 2>&1; then
        log "initramfs rebuilt for every installed kernel"
    else
        warn "mkinitcpio -P FAILED - the restored system may not boot"
    fi
    if [ -d /boot/grub ] && command -v grub-mkconfig >/dev/null 2>&1; then
        grub-mkconfig -o /boot/grub/grub.cfg >/dev/null 2>&1
        log "grub.cfg regenerated"
    fi
    # This very boot proved the restored kernel works — refresh the rescue pair.
    live=""
    for k in /boot/vmlinuz-*; do
        case "$k" in *artix-rescue*) continue ;; esac
        [ -e "$k" ] && { live="$k"; break; }
    done
    if [ -n "$live" ]; then
        init="/boot/initramfs-${live#/boot/vmlinuz-}.img"
        [ -f "$init" ] && cp -f "$live" /boot/vmlinuz-artix-rescue                        && cp -f "$init" /boot/initramfs-artix-rescue.img
    fi
    sync
    rm -f /var/lib/artix-rollback/fixup-pending
    log "reconciliation complete; pending flag cleared"
' >/dev/null 2>&1 &
exit 0
