#!/bin/sh
# Shared logging for the Artix installer's own scripts.
# Installed to /usr/local/lib/artix-installer/log.sh and sourced by path — NOT prepended.
#
# Why a real file rather than a copy pasted into each script: the scripts that
# need this most detach with `setsid sh -c '...'`, and that subshell is a fresh
# shell which inherits no functions. Sourcing by path is the one mechanism that
# works inside such a block without nesting quotes into an already
# single-quoted string.
#
# Writes to a per-script FILE, and echoes to stdout only when stdout is still
# a terminal-ish stream worth writing to. The boot services deliberately run
# with >/dev/null 2>&1 so they never block or spam the console — for them the
# file is the only record, and until this existed they had none at all: a
# service that gave up after sixty tries left nothing behind to say so.
#
# Every call is best-effort. A logging failure must never be the reason a boot
# service dies, so the whole thing is wrapped and always returns success.

# Both are assign-if-unset: the caller sets ARTIX_LOG_TAG before sourcing to
# name its own file, and ARTIX_LOG_DIR stays overridable so this can be
# exercised for real outside a live system instead of only reasoned about.
: "${ARTIX_LOG_DIR:=/var/log/artix-installer}"
: "${ARTIX_LOG_TAG:=installer}"

log() {
    {
        mkdir -p "$ARTIX_LOG_DIR" 2>/dev/null &&
            printf '%s >>> %s\n' "$(date +%Y-%m-%dT%H:%M:%S)" "$*" \
                >>"$ARTIX_LOG_DIR/$ARTIX_LOG_TAG.log"
    } 2>/dev/null
    # stdout too: during installation this is captured into the installer's
    # log, which is where a person actually looks while it is running.
    printf '>>> %s\n' "$*" 2>/dev/null
    return 0
}

# Same, for something that went wrong but is not fatal. Kept distinct so the
# file can be skimmed for trouble instead of read end to end.
warn() {
    {
        mkdir -p "$ARTIX_LOG_DIR" 2>/dev/null &&
            printf '%s !!! %s\n' "$(date +%Y-%m-%dT%H:%M:%S)" "$*" \
                >>"$ARTIX_LOG_DIR/$ARTIX_LOG_TAG.log"
    } 2>/dev/null
    printf '!!! %s\n' "$*" >&2 2>/dev/null
    return 0
}
