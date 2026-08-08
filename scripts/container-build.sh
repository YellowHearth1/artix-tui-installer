#!/bin/sh
# What runs INSIDE the build container. Not meant to be called directly —
# `scripts/build-iso.sh --container` starts it.
#
# It does the same three things the host build does, in the same order, so there
# is one behaviour to reason about rather than two:
#
#   1. compile the installer
#   2. deploy this repo's profile into the artools workspace
#   3. buildiso
#
# The repo is bind-mounted read-write at /repo; the finished image lands in
# /repo/iso, which is the same `iso/` directory a host build writes to.
set -eu

say() { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
die() { printf '\n\033[1;31m!!\033[0m %s\n' "$*" >&2; exit 1; }

# artools comes from the repo, not from a package — the container image installs
# only what artools itself calls out to (pacman, mksquashfs, xorriso, yq, …).
VENDOR=/repo/vendor/artools
if [ -x "$VENDOR/bin/buildiso" ]; then
    BUILDISO="$VENDOR/bin/buildiso"
    export DATADIR="$VENDOR"
    # The vendored tree stays byte-identical to the packages; everything this
    # project changes is applied to COPIES here, in /build, so nothing is ever
    # written back into the bind-mounted repo.
    SYSCONFDIR=$(sh /repo/scripts/artools-prepare.sh "$VENDOR" /build/artools-etc)
    export SYSCONFDIR
    # Prefer the patched lib/ when the prepare step produced one — that is where
    # the guard against the dangling dinit-user-spawn link lives.
    if [ -d /build/artools-etc/lib ]; then
        export LIBDIR=/build/artools-etc/lib
    else
        export LIBDIR="$VENDOR/lib"
    fi
    export PATH="$VENDOR/bin:$PATH"
else
    BUILDISO=buildiso
fi

PROFILE="${PROFILE:-tui}"
WS="${WORKSPACE_DIR:-/build/workspace}"
PROFILE_DIR="$WS/iso-profiles/$PROFILE"
ISO_DIR=/repo/iso

[ -d /repo/installer ] || die "the repo is not mounted at /repo"

# ── 1. The installer ─────────────────────────────────────────────────────────
# Cargo's output goes to a directory of its own rather than the repo's target/,
# so a container build never fights the host's own build over the same files —
# different toolchain versions there means a full rebuild each way, every time.
say "compiling the installer"
cd /repo/installer
CARGO_TARGET_DIR=/build/target cargo build --release --offline 2>/dev/null \
    || CARGO_TARGET_DIR=/build/target cargo build --release
BIN=/build/target/release/artix-installer
[ -f "$BIN" ] || die "the build produced no binary"

# ── 2. The profile ───────────────────────────────────────────────────────────
# Same rule as on the host: WHICH overlay reaches the image is decided by
# profile.yaml, not by the directory's name. artools builds a live layer only
# when there is a top-level `livefs:` key, and copies `live-overlay` only into
# that layer — so with none, everything must go through `root-overlay`.
if grep -qE '^livefs:' /repo/iso-profile/profile.yaml 2>/dev/null; then
    OVERLAY=live-overlay
else
    OVERLAY=root-overlay
fi
say "deploying the profile (overlay: $OVERLAY)"
mkdir -p "$PROFILE_DIR"
cp -f /repo/iso-profile/profile.yaml "$PROFILE_DIR/profile.yaml"
mkdir -p "$PROFILE_DIR/$OVERLAY"
rm -f "$PROFILE_DIR/$OVERLAY"/usr/share/kbd/consolefonts/*.gz 2>/dev/null || true
(cd "/repo/iso-profile/$OVERLAY" \
    && find . -mindepth 1 -maxdepth 1 -exec cp -a {} "$PROFILE_DIR/$OVERLAY/" \;)
[ -d /repo/iso-profile/grub-overrides ] \
    && cp -a /repo/iso-profile/grub-overrides "$PROFILE_DIR/"
install -Dm0755 "$BIN" "$PROFILE_DIR/$OVERLAY/usr/bin/artix-tui-installer"

# The container has no hand-maintained workspace to fall back on: whatever the
# repo's overlay is missing is simply missing from the image. That is the point
# of building this way, and it is also how a dead image got shipped — the repo
# had no /etc/fstab, the `filesystem` package's comment-only one took its place,
# and dinit's `root-ro` failed to remount a root it could not name (exit 32),
# which took `boot` down and printed [FAILED] against every service after it.
grep -qE '^[[:space:]]*[^#[:space:]]+[[:space:]]+/[[:space:]]' \
    "$PROFILE_DIR/$OVERLAY/etc/fstab" 2>/dev/null \
    || die "$OVERLAY/etc/fstab has no entry for '/': root-ro fails with exit 32 and the image will not boot"

fonts=$(find "$PROFILE_DIR/$OVERLAY/usr/share/kbd/consolefonts" -name '*.gz' 2>/dev/null | wc -l)
say "profile in place: $fonts console font(s)"

# ── 3. The image ─────────────────────────────────────────────────────────────
say "building the ISO"
mkdir -p "$ISO_DIR"
# A MARKER, not a stopwatch. `buildiso` prints "Finished building" and exits 0
# even when xorriso aborted — so the only honest question is whether a NEW image
# exists afterwards. Without this the podman build failed at the last step,
# reported success, and named the previous run's image: the worst kind of
# failure, one that looks like a working build.
marker="$ISO_DIR/.build-started"
: > "$marker"

"$BUILDISO" -p "$PROFILE" -t "$ISO_DIR"

# buildiso writes into <target>/<profile>/; lift it so `iso/` holds the images
# and nothing else, matching what a host build leaves behind.
if [ -d "$ISO_DIR/$PROFILE" ]; then
    find "$ISO_DIR/$PROFILE" -maxdepth 1 -type f -exec mv -f {} "$ISO_DIR/" \;
    rmdir "$ISO_DIR/$PROFILE" 2>/dev/null || true
fi

# Ownership of the finished image, and ONLY when it needs fixing.
#
# Under docker the container is real root, so everything it writes is owned by
# uid 0 on the host and would need sudo to delete on a machine that never asked
# for root. Under ROOTLESS PODMAN the opposite is true: the container's root is
# already your user, the files already belong to you, and chowning them to 1000
# maps THROUGH the userns to some subuid — which is how a 1.2 GB image ended up
# owned by 100999 and undeletable without podman unshare.
#
# /proc/self/uid_map says which case this is: container-uid 0 maps to host-uid 0
# only when there is no user namespace in the way.
root_maps_to=$(awk '$1 == 0 { print $2; exit }' /proc/self/uid_map 2>/dev/null || echo 0)
if [ "${root_maps_to:-0}" = 0 ] && [ -n "${HOST_UID:-}" ] && [ -n "${HOST_GID:-}" ]; then
    chown -R "$HOST_UID:$HOST_GID" "$ISO_DIR"
fi

iso=$(find "$ISO_DIR" -maxdepth 1 -name '*.iso' -newer "$marker" 2>/dev/null | head -1)
rm -f "$marker"
[ -n "$iso" ] || die "the build produced no new image — look above for the real error \
(xorriso and mksquashfs failures do not stop buildiso)"
say "ISO: ${iso#/repo/}  ($(du -h "$iso" | cut -f1))"
