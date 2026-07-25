#!/bin/sh
# One-time baseline snapshot of the freshly installed system (first boot only).
MARKER=/var/lib/artix-installer/baseline-snapshot-done
[ -f "$MARKER" ] && exit 0

# Detach so boot is never blocked; retry until snapper/D-Bus are up.
# Output goes to /dev/null on purpose (never spam the console at boot), so the
# log file that artix-log.sh writes is the ONLY record this ran at all.
setsid sh -c '
    ARTIX_LOG_TAG=first-snapshot
    # No-op stubs if the library is missing (a hand-copied service, an older
    # install): logging must never be the reason this fails to run.
    if ! . /usr/local/lib/artix-installer/log.sh 2>/dev/null; then
        log() { :; }
        warn() { :; }
    fi
    log "waiting for snapper to become available (up to 120s)"
    i=0
    while [ "$i" -lt 60 ]; do
        if snapper -c root create -d "clean system (post-install baseline)" >/dev/null 2>&1; then
            mkdir -p /var/lib/artix-installer
            : > /var/lib/artix-installer/baseline-snapshot-done
            log "baseline snapshot created after ${i} retries"
            if [ -d /boot/grub ] && command -v grub-mkconfig >/dev/null 2>&1; then
                grub-mkconfig -o /boot/grub/grub.cfg >/dev/null 2>&1
                log "grub.cfg regenerated so the snapshot is offered at boot"
            fi
            rm -f /etc/dinit.d/boot.d/artix-first-snapshot 2>/dev/null
            log "one-time service removed; it will not run again"
            exit 0
        fi
        i=$((i + 1))
        sleep 2
    done
    warn "snapper never became available after 60 tries - NO baseline snapshot exists"
' >/dev/null 2>&1 &
exit 0
