#!/bin/sh
# Produce the artools config this project builds with — at build time, from the
# vendored one.
#
# WHY THIS EXISTS. vendor/artools is a verbatim copy of the Artix artools
# packages, and it needs exactly two settings changed. Those changes used to be
# edited straight into the vendored file, which meant every refresh of artools
# ("copy the new package files in") silently reverted them, and the build went
# looking for a workspace that was not there or produced an image named
# `artix-tui-openrc-*.iso`. Neither failure names its cause.
#
# So the vendored tree stays byte-identical to the package — refreshing it is
# just a copy — and the two settings are applied here, into a throwaway config
# directory, on every build. If a future artools renames or drops either
# setting, this dies with the name of the setting instead of building something
# subtly wrong.
#
# Usage:
#     SYSCONFDIR=$(sh scripts/artools-prepare.sh <vendor/artools> <dest-dir>)
#
# Both changes are to CONFIGURATION, which is what configuration is for. No
# artools *tool* is patched anywhere in this repo.
set -eu

VENDOR=${1:?usage: artools-prepare.sh <vendor/artools> <dest-dir>}
DEST=${2:?usage: artools-prepare.sh <vendor/artools> <dest-dir>}

[ -f "$VENDOR/etc/artools-iso.conf" ] \
    || { echo "!! $VENDOR/etc/artools-iso.conf is missing" >&2; exit 1; }

rm -rf "$DEST"
mkdir -p "$DEST"
cp -a "$VENDOR"/etc/. "$DEST"/
conf="$DEST/artools-iso.conf"

# A COPY OF lib/, TOO — for one upstream line that ships a broken link.
#
# artools links /usr/lib/dinit.d/dinit-user-spawn into the live image's boot.d.
# That service was replaced and the file no longer exists, but `ln -s` does not
# check its target, so every image gets a DANGLING link and dinit complains
# about the missing service on every single boot. Nothing is actually broken by
# it — which is worse, because a permanent error in the log trains people to
# ignore the log.
#
# Patched on a copy, like the config: the vendored tree stays byte-identical to
# the packages, so updating artools is still just copying files in.
LIBSRC="$VENDOR/lib"
LIBDEST="$DEST/lib"
if [ -d "$LIBSRC" ]; then
    rm -rf "$LIBDEST"
    cp -a "$LIBSRC" "$LIBDEST"
    svc="$LIBDEST/iso/services.sh"
    if [ -f "$svc" ] && grep -q 'dinit-user-spawn' "$svc"; then
        # Link it only if it is really there, so this keeps working if the
        # service ever comes back under that name.
        #
        # THE GUARD MUST BE AN `if`, NOT `[ … ] && …`. That line is the LAST
        # statement of add_svc_dinit, so the function returns whatever it
        # returns — and buildiso runs the whole phase under `set -e` with an ERR
        # trap. An `&&` whose test fails returns 1, which aborted the build with
        # "A failure occurred in make_rootfs()" and named nothing that had
        # anything to do with a symlink. An `if` with no else returns 0.
        sed -i 's|^\([[:space:]]*\)chroot "$mnt" ln -s /usr/lib/dinit.d/dinit-user-spawn \(.*\)$|\1if [ -e "$mnt/usr/lib/dinit.d/dinit-user-spawn" ]; then chroot "$mnt" ln -s /usr/lib/dinit.d/dinit-user-spawn \2; fi|' "$svc"
        grep -q 'if \[ -e "$mnt/usr/lib/dinit.d/dinit-user-spawn" \]; then' "$svc" || {
            echo "!! could not guard the dinit-user-spawn link in $svc" >&2
            echo "   upstream artools changed that line; the image will keep a" >&2
            echo "   dangling service link and dinit will report it every boot." >&2
            exit 1
        }
        # And PROVE it still returns 0 with the file absent, rather than trust
        # the shape of the text. This is the exact call buildiso makes, with the
        # chroot stubbed out and no services to set: the one thing left for it
        # to return is the guard.
        bash -c '
            set -e
            msg2() { :; }; warning() { :; }; chroot() { :; }
            SERVICES=(); INITSYS=dinit
            . "$1"
            add_svc_dinit /nonexistent-root
        ' _ "$svc" 2>/dev/null || {
            echo "!! the patched $svc still fails when dinit-user-spawn is absent" >&2
            echo "   buildiso runs it under 'set -e', so this would abort the" >&2
            echo "   build inside make_rootfs() with no mention of the cause." >&2
            exit 1
        }
    fi
fi

# Each change is a substitution PLUS a check that the substitution landed. A sed
# that matches nothing exits 0 and leaves the file alone, so without the check
# this script's whole purpose — noticing that upstream moved — would be exactly
# the thing it fails to do.
patch_setting() {
    label=$1 pattern=$2 want=$3
    sed -i "$pattern" "$conf"
    grep -qxF "$want" "$conf" || {
        echo "!! artools setting '$label' did not apply to $conf" >&2
        echo "   upstream artools probably renamed or dropped it; expected line:" >&2
        echo "   $want" >&2
        exit 1
    }
}

# 1. Honour an existing WORKSPACE_DIR. Upstream assigns it unconditionally, so
#    exporting the variable had no effect: the container set one workspace and
#    buildiso looked in another, then reported the profile as missing.
# A LITERAL TILDE IS NOT A PATH. artools computes USER_HOME as `~name` when it
# runs under sudo, and that string inside double quotes is never expanded by the
# shell — so the fallback below would create a directory actually called
# `~<username>` in whatever the current directory happened to be, owned by
# root, once per user who ever builds. One turned up inside installer/ and was
# not even gitignored, so it would have been committed.
#
# Refuse rather than build junk: the caller (build-iso.sh) always exports a real
# WORKSPACE_DIR, so reaching this means something upstream changed.
case "${WORKSPACE_DIR:-}" in
    "~"*)
        echo "!! WORKSPACE_DIR is a literal tilde path: $WORKSPACE_DIR" >&2
        echo "   The shell will not expand that, and a directory with that name" >&2
        echo "   would be created wherever this runs. Set an absolute path." >&2
        exit 1
        ;;
esac

patch_setting WORKSPACE_DIR \
    's|^#\{0,1\}[[:space:]]*WORKSPACE_DIR=.*|WORKSPACE_DIR="${WORKSPACE_DIR:-${USER_HOME}/artools-workspace}"|' \
    'WORKSPACE_DIR="${WORKSPACE_DIR:-${USER_HOME}/artools-workspace}"'

# 2. This project IS dinit. At the package default (openrc) the build produced
#    `artix-tui-openrc-*.iso` — the wrong init system, from a repo whose whole
#    reason for existing is the other one, and it showed up only in the
#    filename.
patch_setting INITSYS \
    's|^#\{0,1\}[[:space:]]*INITSYS=.*|INITSYS="dinit"|' \
    'INITSYS="dinit"'

printf '%s\n' "$DEST"
