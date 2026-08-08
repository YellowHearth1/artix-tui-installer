#!/bin/sh
# Build the installer and the live ISO, in the right order and with the checks
# that are cheap now and expensive later.
#
# The three commands this replaces were:
#
#     cd installer && cargo build --release
#     cp target/release/artix-installer <profile>/root-overlay/usr/bin/...
#     sudo buildiso -p tui
#
# Two things went wrong with them, both silently.
#
# 1. THE OVERLAY. Which one reaches the image is decided by profile.yaml, not by
#    the directory's name — see the note further down. Guessing it wrong ships an
#    ISO built from files nobody copied, and the build says nothing.
#
# 2. THE FONT FILES. The installer offers console fonts that are NOT in any
#    package; they are carried in the profile. If they are missing from the
#    image, `setfont` fails and the font screen looks like it applies fonts
#    "sometimes" — the packaged ones work, the carried ones do nothing. So this
#    script checks that every font the chooser can ask for is actually there,
#    BEFORE spending ten minutes building an ISO that cannot use them.
#
# Usage:
#     sh scripts/build-iso.sh            # checks, build, sync, ISO
#     sh scripts/build-iso.sh --bin      # build and sync only, no ISO
#     sh scripts/build-iso.sh --fast     # skip tests/clippy/fmt (know why)
#     sh scripts/build-iso.sh --clean    # rebuild the cached rootfs from scratch
#     sh scripts/build-iso.sh --podman   # build in a container — works on ANY
#     sh scripts/build-iso.sh --docker   # Linux, with neither artools nor Rust
#                                        # installed. Name the engine; there is
#                                        # no auto-detect on purpose.
#
# Paths follow the maintainer's layout and can be overridden:
#     PROFILE_DIR=/srv/tui ISO_DIR=/mnt/big sh scripts/build-iso.sh
set -eu

# Where the checkout is, worked out from where THIS SCRIPT is — not from a
# hard-coded ~/artix-tui-installer. Anyone cloning the project somewhere else
# (and anyone at all who is not the author) had every path in here point at a
# directory they do not have.
SELF_DIR=$(cd "$(dirname "$0")" && pwd -P)
REPO_DIR="${REPO_DIR:-$(dirname "$SELF_DIR")}"
PROFILE="${PROFILE:-tui}"
# The artools workspace lives INSIDE THE CHECKOUT, next to iso/ and vm/.
#
# It used to be ~/artools-workspace, which is where artools puts it by default —
# so "everything the build needs is in the repo" was true of the SOURCE and not
# of the build: the deployed profile sat in the home directory, `sudo buildiso`
# left parts of it owned by root, and the next deploy could not overwrite them.
# Nothing about that was visible from the repository.
#
# `scripts/artools-prepare.sh` already patches the config so WORKSPACE_DIR
# honours the environment — this is what that change was for. Exported, because
# buildiso reads it, and `sudo -E` carries it across the same way it carries
# LIBDIR and DATADIR.
WORKSPACE_DIR="${WORKSPACE_DIR:-$REPO_DIR/workspace}"
export WORKSPACE_DIR
PROFILE_DIR="${PROFILE_DIR:-$WORKSPACE_DIR/iso-profiles/$PROFILE}"
# Finished images land HERE, inside the checkout, so there is one obvious place
# to look for them. buildiso is pointed at it with -t rather than the image being
# copied afterwards: a copy would mean two 1.5 GB files, and this machine's disk
# has run out before.
ISO_DIR="${ISO_DIR:-$REPO_DIR/iso}"
BIN="$REPO_DIR/installer/target/release/artix-installer"
REPO_PROFILE="$REPO_DIR/iso-profile"
# WHICH overlay reaches the image is decided by the profile, not by its name.
# artools builds a live layer only when profile.yaml has a top-level `livefs:`
# key (`HAS_LIVE=$(yq '. | has("livefs")')`), and copies `live-overlay` only
# into THAT layer. This profile has `live-session:` and `rootfs:` but no
# `livefs:`, so no live layer is built, `live-overlay` is never read, and
# everything must go through `root-overlay`.
#
# That cost a release. The console fonts sat in `live-overlay`, the ISO was
# built without them, and the font screen offered fonts the image did not have:
# `setfont` failed silently and the fonts "did not apply". The binary was going
# the same way. So the layer is DETECTED here rather than assumed, and it is
# checked again after the copy.
if grep -qE '^livefs:' "$REPO_PROFILE/profile.yaml" 2>/dev/null; then
    OVERLAY_NAME="live-overlay"
else
    OVERLAY_NAME="root-overlay"
fi
REPO_OVERLAY="$REPO_PROFILE/$OVERLAY_NAME"
DEST_OVERLAY="$PROFILE_DIR/$OVERLAY_NAME"
DEST_BIN="$DEST_OVERLAY/usr/bin/artix-tui-installer"

# ── artools, carried in the repo ─────────────────────────────────────────────
# `vendor/artools/` holds the ISO tooling itself, so a build does not need the
# `artools` package installed. buildiso finds its libraries through three
# environment variables, so nothing had to be patched to make it run from here.
#
# A system buildiso is still used when the vendored copy is missing — a checkout
# with `vendor/` stripped should not simply stop working on a machine that has
# artools anyway.
#
# The vendored tree is byte-identical to the packages — refreshing artools is a
# plain copy, with no diff to re-apply and none to forget. The two settings this
# project needs changed are applied on every build by scripts/artools-prepare.sh
# into a throwaway config directory, which is what SYSCONFDIR then points at.
VENDOR="$REPO_DIR/vendor/artools"
if [ -x "$VENDOR/bin/buildiso" ]; then
    BUILDISO="$VENDOR/bin/buildiso"
    export DATADIR="$VENDOR"
    ART_ETC="${TMPDIR:-/tmp}/artix-tui-artools-etc-$(id -u)"
    SYSCONFDIR=$(sh "$REPO_DIR/scripts/artools-prepare.sh" "$VENDOR" "$ART_ETC")
    export SYSCONFDIR
    # The prepare step also leaves a patched lib/ — it guards the upstream line
    # that links a dinit service which no longer exists, and so puts a dangling
    # link in every image. Use it when it is there.
    if [ -d "$ART_ETC/lib" ]; then
        export LIBDIR="$ART_ETC/lib"
    else
        export LIBDIR="$VENDOR/lib"
    fi
    # basestrap and artix-chroot are called by name from inside buildiso.
    export PATH="$VENDOR/bin:$PATH"
elif command -v buildiso >/dev/null 2>&1; then
    BUILDISO=buildiso
else
    BUILDISO=""
fi

do_iso=1
do_checks=1
do_clean=0
do_container=0
engine_want=
for arg in "$@"; do
    case "$arg" in
        --bin)   do_iso=0 ;;
        --fast)  do_checks=0 ;;
        --clean) do_clean=1 ;;
        --docker)    do_container=1; engine_want=docker ;;
        --podman)    do_container=1; engine_want=podman ;;
        # Kept only to answer for itself. It used to pick an engine on its own,
        # and a build running under something you did not name is exactly the
        # kind of thing people keep the two apart to avoid.
        --container) die "say which one: --docker or --podman" ;;
        -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
        *) echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
    esac
done

# Files under a directory that are NOT ours.
#
# Deliberately not `find -user`: on this machine `find` is bfs, which does not
# recurse the way GNU find does — it answered "nothing" for a tree full of
# root-owned files, and the check that was supposed to catch this problem is
# what missed it. `ls -R` and a per-file test are boring and correct.
find_not_ours() {
    me=$(id -un)
    ls -RA "$1" 2>/dev/null | awk -v dir="$1" '
        /:$/ { sub(/:$/, "", $0); cur = $0; next }
        NF   { print cur "/" $0 }
    ' | while read -r f; do
        [ -e "$f" ] || continue
        owner=$(stat -c '%U' "$f" 2>/dev/null) || continue
        [ "$owner" = "$me" ] || { printf '%s\n' "$f"; break; }
    done
}

say() { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
die() { printf '\n\033[1;31m!!\033[0m %s\n' "$*" >&2; exit 1; }

# The profile is created below if it is not there — a fresh machine needs only
# artools and this checkout, which is the point of keeping the profile in the
# repo at all.

# ── 0a. Build in a container instead ─────────────────────────────────────────
# The whole build inside an Artix container: nothing is required on the host but
# Docker or Podman. Not artools, not a Rust toolchain, not Artix — which is what
# makes this project buildable on Ubuntu, Fedora or anything else.
#
# --privileged is not optional. `buildiso` creates a root filesystem, mounts an
# overlay over it, loop-mounts a FAT image for the EFI payload and runs
# mksquashfs; those are kernel operations a normal container may not perform.
# It is doing exactly what the same build does on the host.
if [ "$do_container" -eq 1 ]; then
    # The engine is NAMED, never guessed. Podman and Docker are kept apart on
    # purpose — one runs without a daemon and often without root — so a build
    # that quietly picked the other is a build running somewhere you did not ask
    # for.
    command -v "$engine_want" >/dev/null 2>&1 \
        || die "$engine_want is not installed"
    engine="$engine_want"

    # PODMAN NEEDS TO BE ROOT FOR THIS ONE, and that is the kernel's rule rather
    # than a setting anybody can change. `buildiso` does two things a user
    # namespace cannot:
    #
    #   mount -t devtmpfs udev "$mnt/dev"    (lib/base/mount.sh)
    #   mount efi.img "$mnt/efiboot"         (lib/iso/grub.sh — an auto-loop mount)
    #
    # The kernel allows only a short list of filesystem types inside a user
    # namespace — proc, sysfs, tmpfs, devpts, bind — and devtmpfs is not on it.
    # Loop mounts need a real loop device, which is why the host's /dev is
    # handed in at all. Rootless podman therefore fails at make_rootfs with
    # "permission denied", and no amount of --privileged changes that.
    #
    # Docker does not hit this because its daemon already runs as root. So for
    # podman we ask for root the same way this script ALREADY does for a host
    # build (`sudo -E env … buildiso` further down) — it is the same escalation,
    # for the same reason, and it is announced before it happens.
    engine_cmd="$engine"
    if [ "$engine" = podman ] && [ "$(id -u)" -ne 0 ]; then
        rootless=$(podman info --format '{{.Host.Security.Rootless}}' 2>/dev/null || echo true)
        if [ "$rootless" = true ]; then
            printf '\n\033[1;33m..\033[0m podman is rootless here, and this build needs real root:\n' >&2
            echo "   buildiso mounts a devtmpfs and loop-mounts the EFI image, neither" >&2
            echo "   of which a user namespace permits. Running the container through" >&2
            echo "   sudo — the same escalation a host build already asks for." >&2
            echo "   (Use --docker instead to avoid it: that daemon is already root.)" >&2
            engine_cmd="sudo podman"
        fi
    fi

    # HOST NETWORKING FOR PODMAN, because its bridge is very often dead on a
    # machine that also has Docker: Docker sets `iptables -P FORWARD DROP`, and
    # podman's own bridge traffic is then dropped along with everything else.
    # The symptom is not a networking error but this, minutes in:
    #
    #     error: failed retrieving file 'world.db' ... Resolving timed out
    #
    # Verified on this machine: default bridge cannot resolve at all, host
    # networking resolves immediately. The container is already --privileged with
    # the host's /dev handed in, so sharing the network namespace gives away
    # nothing it did not already have — and it downloads packages, nothing else.
    # Docker is left alone: its own networking works, and changing what works is
    # how working things stop.
    net_args=""
    [ "$engine" = podman ] && net_args="--network=host"

    say "building in a container with $engine (host needs nothing else)"
    # shellcheck disable=SC2086 # engine_cmd is deliberately "sudo podman" or one word
    $engine_cmd build $net_args -t artix-tui-iso -f "$REPO_DIR/Containerfile" "$REPO_DIR"
    # buildiso's work directory must live on the HOST filesystem, not inside the
    # container's own. Docker's storage is overlayfs, and overlayfs refuses to be
    # the upperdir of another overlayfs — which is exactly what buildiso mounts
    # when it assembles the boot filesystem. The build gets all the way through
    # installing packages and then dies with "not supported as upperdir".
    #
    # It doubles as a cache: the rootfs survives between runs, so a second build
    # does not download every package again.
    # A cache PER ENGINE. Docker runs its container as real root, so everything
    # under the work directory ends up owned by uid 0; rootless podman maps its
    # own root to your user and cannot then read those files at all — the build
    # got as far as xorriso and died with "Access to file is not allowed". The
    # two trees simply cannot be shared, so they are not.
    cache="${BUILD_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/artix-tui-iso-$engine}"
    mkdir -p "$cache"
    say "work directory: $cache (kept between runs)"
    # /dev from the host: --privileged alone exposes /dev/loop-control but no
    # free loop devices, and buildiso loop-mounts a FAT image to assemble the
    # EFI payload. Without this it builds everything and dies at the last step
    # with "failed to set up loop device".
    # shellcheck disable=SC2086 # same as above
    $engine_cmd run --rm --privileged $net_args \
        -v /dev:/dev \
        -v "$REPO_DIR:/repo" \
        -v "$cache:/var/lib/artools/buildiso" \
        -e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" \
        -e "PROFILE=$PROFILE" \
        artix-tui-iso
    exit 0
fi

# ── 0. Optional: throw away the cached chroot ────────────────────────────────
# buildiso builds the rootfs ONCE and locks it (work_dir/rootfs.lock); every
# later run reuses it. That is why builds are fast — and why a file that was in
# an overlay months ago is still in the image today, long after the overlay
# stopped shipping it. This machine's cached rootfs still carries an
# `installer-start` and an `installer-console` service from a root-overlay that
# is now empty; the livefs layer covers the first, but nothing removes them.
#
# --clean deletes the cache so the rootfs is rebuilt from packages and the
# CURRENT overlays. It costs a full package download, which is why it is not
# the default.
if [ "$do_clean" -eq 1 ]; then
    say "deleting the cached chroot (a full rebuild follows)"
    victim="/var/lib/artools/buildiso/$PROFILE"
    # CHECK FOR MOUNTS FIRST. This is not caution for its own sake: an earlier
    # version of this repo bind-mounted /dev into a build tree and then deleted
    # that tree, and the delete followed the mount into the REAL /dev. The
    # machine lost /dev/pts — no new terminal could be opened — and /dev/shm came
    # back without its 1777 permissions, which crashes anything using shared
    # memory. `--one-file-system` alone was not enough to rely on.
    #
    # Nothing this script does may damage the machine it runs on. A refusal is
    # always better than a delete that walks somewhere it was not asked to go.
    if findmnt -rno TARGET 2>/dev/null | grep -q "^$victim"; then
        printf '\n\033[1;31m!!\033[0m something is still mounted under %s\n' "$victim" >&2
        findmnt -rno TARGET | grep "^$victim" | sed 's/^/     /' >&2
        die "refusing to delete a tree with mounts inside — unmount them first"
    fi
    sudo rm -rf --one-file-system "$victim"
fi

# ── 0c. What this machine is missing ─────────────────────────────────────────
# NAMED UP FRONT, WITH THE COMMAND THAT INSTALLS THEM.
#
# artools itself rides in vendor/, so a checkout needs nothing installed for
# that — but the tools artools CALLS come from other packages, and when one is
# absent the build dies minutes later, deep inside buildiso, with a message
# about a file it could not write. Nothing in that message says "install
# libisoburn". Somebody trying this project for the first time reads it as "the
# installer is broken", and they are not being unreasonable.
#
# Checked by BINARY, not by package: `pacman -Q` would only work on Arch-family
# systems, and the container path deliberately supports any distribution.
missing=""
add_missing() { command -v "$1" >/dev/null 2>&1 || missing="$missing $2"; }
if [ "$do_container" -eq 0 ]; then
    add_missing cargo rust
    add_missing mkinitcpio mkinitcpio
    add_missing mksquashfs squashfs-tools
    add_missing xorriso libisoburn
    add_missing yq go-yq
    add_missing pacman pacman
    if [ -n "$missing" ]; then
        printf '\n\033[1;31m!!\033[0m this machine is missing what the build needs:\n' >&2
        for p in $missing; do echo "     - $p" >&2; done
        echo >&2
        echo "   On Artix or Arch:" >&2
        # shellcheck disable=SC2086  # a deliberate word-split into arguments
        echo "       sudo pacman -S --needed$missing" >&2
        echo >&2
        echo "   On any other distribution there is no need to hunt these down —" >&2
        echo "   build in a container instead, which carries them itself:" >&2
        echo "       sh scripts/build-iso.sh --docker      # or --podman" >&2
        exit 1
    fi
fi

# ── 1. Checks ────────────────────────────────────────────────────────────────
# Before the build, not after: an ISO takes minutes and a failing test takes
# seconds. The order is cheapest-first so the fastest failure surfaces first.
if [ "$do_checks" -eq 1 ]; then
    say "checks (fmt, clippy, tests)"
    cd "$REPO_DIR/installer"
    cargo fmt --check || die "cargo fmt --check failed — run 'cargo fmt'"
    cargo clippy --all-targets -- -D warnings || die "clippy failed"
    cargo test --quiet || die "tests failed"
else
    say "checks SKIPPED (--fast)"
fi

# ── 2. Build ─────────────────────────────────────────────────────────────────
say "building the installer"
cd "$REPO_DIR/installer"
cargo build --release
[ -f "$BIN" ] || die "the build produced no binary at $BIN"

version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$REPO_DIR/installer/Cargo.toml" | head -1)
say "built $version  ($(du -h "$BIN" | cut -f1))"

# ── 3. Deploy the profile ────────────────────────────────────────────────────
# The profile lives IN THIS REPO and is copied out to the artools workspace on
# every build. That direction matters: the workspace copy used to be the only
# one, hand-maintained, and it drifted from the repo until the two disagreed
# about which overlay the binary goes in and which fonts exist. Now the repo is
# the source of truth and this step makes the workspace match it, so a fresh
# machine needs nothing but artools and this checkout.
say "deploying the profile to $PROFILE_DIR (overlay: $OVERLAY_NAME)"
mkdir -p "$PROFILE_DIR"
cp -f "$REPO_PROFILE/profile.yaml" "$PROFILE_DIR/profile.yaml"
mkdir -p "$DEST_OVERLAY"
# Console fonts are cleared before copying. Copying alone only ever ADDS: a font
# dropped from the repo stayed in the profile and shipped in the image, so the
# chooser and the image disagreed about which fonts exist — the drift this whole
# deploy step is meant to remove. Only this directory is cleared, because it is
# entirely ours; the rest of the overlay is merged so the profile can still
# carry files of its own.
rm -f "$DEST_OVERLAY"/usr/share/kbd/consolefonts/*.gz 2>/dev/null || true
# Ownership first. `sudo buildiso` runs as root and leaves parts of the profile
# owned by root; the next deploy, running as you, then cannot overwrite them —
# and `cp` says "Permission denied" for each one and carries on.
#
# Bounded and announced: one directory, the profile this script owns, and
# nothing is touched if something is mounted inside it (a chown that walks into
# a mount is somebody else's files).
if [ -n "$(find_not_ours "$PROFILE_DIR")" ]; then
    if findmnt -rno TARGET 2>/dev/null | grep -q "^$PROFILE_DIR"; then
        die "something is mounted inside $PROFILE_DIR — refusing to touch its ownership"
    fi
    say "parts of the profile are owned by root (an earlier sudo build) — taking them back"
    sudo chown -R "$(id -u):$(id -g)" "$PROFILE_DIR"
fi

# -a keeps the executable bits on installer-start and the wifi-test scripts.
#
# --remove-destination, BECAUSE SOME OF WHAT WE SHIP IS READ-ONLY BY DESIGN.
# The sudoers drop-in is mode 0440 — sudo refuses a group- or world-writable
# file — so `cp` could write it the first time and never again: it opens the
# existing destination, gets EACCES, and every later build silently kept the
# FIRST version of that file. Unlinking first is what `cp -f` is popularly
# believed to do and does not.
#
# AND THE COPY HAS TO BE ABLE TO FAIL. This used to be `find -exec cp`, with a
# comment saying find does not propagate a failing cp — which is true, and the
# code tested find's exit status anyway. find returned 0, `set -e` saw nothing,
# and the build carried on with whatever the workspace already held. That is the
# same shape as the dead image: an ISO assembled from stale files, with nothing
# but a scrolled-past warning to say so. The loop runs in THIS shell, not a
# subshell, so the flag it sets is still set when it ends.
copy_failed=""
for entry in "$REPO_OVERLAY"/* "$REPO_OVERLAY"/.[!.]* "$REPO_OVERLAY"/..?*; do
    [ -e "$entry" ] || continue          # an unmatched glob stays literal
    cp -a --remove-destination "$entry" "$DEST_OVERLAY/" || copy_failed=yes
done
if [ -n "$copy_failed" ]; then
    printf '\n\033[1;31m!!\033[0m the profile could not be deployed in full.\n' >&2
    echo "   Files above could not be written — the image would be built from" >&2
    echo "   whatever was there before, which is how a stale ISO gets shipped." >&2
    echo >&2
    echo "   Almost always this is ownership left by an earlier sudo build:" >&2
    echo "       sudo chown -R \"$(id -un):$(id -gn)\" \"$PROFILE_DIR\"" >&2
    echo "   Then run this again." >&2
    exit 1
fi
if [ -d "$REPO_PROFILE/grub-overrides" ]; then
    cp -a "$REPO_PROFILE/grub-overrides" "$PROFILE_DIR/"
fi

# ── 3b. Does the overlay carry what the live boot cannot start without? ──────
# /etc/fstab is the one that bites. The `filesystem` package already ships an
# fstab, so a missing overlay copy does not leave a hole — it leaves a
# plausible-looking file with nothing but comments in it. dinit's `root-ro` then
# runs `mount -o remount,ro,rshared /` against a root it cannot name, exits 32,
# takes the `boot` service down with it, and every service after that prints
# [FAILED]. The image boots far enough to look like the installer is broken.
#
# This cost a build to find: the workspace copy of the profile had the file and
# the repo did not, so the host build kept working and only the container build
# — which has no workspace to fall back on — produced a dead image.
say "verifying the overlay carries the live-boot essentials"
grep -qE '^[[:space:]]*[^#[:space:]]+[[:space:]]+/[[:space:]]' "$DEST_OVERLAY/etc/fstab" 2>/dev/null \
    || die "$OVERLAY_NAME/etc/fstab has no entry for '/': root-ro fails with exit 32 and the image will not boot"

# Services the OVERLAY is supposed to provide, and that did not arrive.
#
# This used to ask "does anything provide this service?" and answer it by
# looking on the HOST — which cannot know. acpid, cronie, cupsd, syslog-ng and
# seatd all come from packages installed INTO THE IMAGE, so it warned about five
# services that were perfectly fine, every single build. A check that cries wolf
# is a check people learn to scroll past.
#
# The answerable question is narrower: the repo overlay carries a file for this
# service, and it is not in the deployed profile. That is a deployment failure,
# and it is knowable from here.
for svc in $(sed -n '/^  services:/,/^  [a-z-]*:/p' "$REPO_PROFILE/profile.yaml" \
             | sed -n 's/^    - \([A-Za-z0-9@._-]*\).*/\1/p'); do
    [ -f "$REPO_OVERLAY/etc/dinit.d/$svc" ] || continue
    [ -f "$DEST_OVERLAY/etc/dinit.d/$svc" ] && continue
    die "the overlay carries etc/dinit.d/$svc but it did not reach $DEST_OVERLAY"
done

say "installing the binary into $OVERLAY_NAME"
mkdir -p "$(dirname "$DEST_BIN")"
install -m 0755 "$BIN" "$DEST_BIN"

fonts=$(find "$DEST_OVERLAY/usr/share/kbd/consolefonts" -name '*.gz' 2>/dev/null | wc -l)
say "profile in place: $fonts console font(s), binary $version"

# ── 4. Can the image actually load every font on offer? ──────────────────────
# The font chooser names files. A name with no file behind it is not an error
# anywhere: setfont fails, the installer keeps the old font, and the screen
# looks broken in a way that is hard to attribute — the packaged fonts work and
# the carried ones do nothing, so it reads as "fonts apply sometimes".
say "verifying every offered font is present"
missing=""
fontpick="$REPO_DIR/installer/src/screens/fontpick.rs"
names=$(sed -n 's/.*("[0-9]*×[0-9]*", "\([A-Za-z0-9.-]*\)").*/\1/p' "$fontpick")
for name in $names; do
    # Carried in the repo -> must have reached the profile.
    if [ -f "$REPO_OVERLAY/usr/share/kbd/consolefonts/$name.psfu.gz" ]; then
        [ -f "$DEST_OVERLAY/usr/share/kbd/consolefonts/$name.psfu.gz" ] \
            || missing="$missing $name(not deployed)"
        continue
    fi
    # Otherwise it comes from a package (kbd, terminus-font). Those land in the
    # rootfs at build time; the host has the same packages, so it can answer.
    if ! ls /usr/share/kbd/consolefonts/"$name".psf* >/dev/null 2>&1; then
        missing="$missing $name(no package file)"
    fi
done
if [ -n "$missing" ]; then
    printf '\n\033[1;33m!!\033[0m fonts the chooser offers but cannot load:%s\n' "$missing" >&2
    printf '   the font screen will appear "not to apply" these.\n' >&2
    die "fix the font list or the profile before building"
fi
say "all fonts accounted for"

# ── 4b. Has artix-grub-live changed its kernels.cfg? ─────────────────────────
# Our copy skips the boot menu, and it is a MODIFIED copy: three appended lines
# plus an `--id` and a rename on two menu entries. It is copied rather than
# written because the entry bodies decide how the kernel is located, and getting
# that subtly wrong produces an ISO that does not boot — the one failure with no
# way to debug it from inside. The cost of a copy is that it rots.
#
# So the question is "has the PACKAGE changed since we copied it", and the
# pristine copy to compare against lives in iso-profile/upstream/.
#
# It used to ask a different question and get it wrong: it stripped the appended
# block off our file and compared the rest to the one in the BUILT ROOTFS —
# which is our own file, because the overlay is copied into that rootfs. So it
# compared our file to itself, minus the edits we had made on purpose, and
# warned on every single build. The command it offered as the fix would have
# copied our file over itself.
upstream="$REPO_PROFILE/upstream/kernels.cfg"
grub_pkg=$(ls -t /var/cache/pacman/pkg/artix-grub-live-*.pkg.tar.zst 2>/dev/null | head -1)
if [ -f "$upstream" ] && [ -n "$grub_pkg" ] && command -v bsdtar >/dev/null 2>&1; then
    fresh="${TMPDIR:-/tmp}/artix-kernels-upstream.$$"
    if bsdtar -xOf "$grub_pkg" usr/share/grub/cfg/kernels.cfg > "$fresh" 2>/dev/null \
       && ! cmp -s "$fresh" "$upstream"; then
        printf '\n\033[1;33m--\033[0m artix-grub-live changed its kernels.cfg.\n' >&2
        echo "   Ours is based on the old one and will keep booting the old way." >&2
        echo "   What changed:" >&2
        diff -u "$upstream" "$fresh" | sed 's/^/     /' >&2
        echo "   Carry anything real into root-overlay/usr/share/grub/cfg/kernels.cfg," >&2
        echo "   then refresh the pristine copy:" >&2
        echo "       bsdtar -xOf $grub_pkg usr/share/grub/cfg/kernels.cfg > $upstream" >&2
    else
        say "GRUB config still matches artix-grub-live"
    fi
    rm -f "$fresh"
else
    say "skipping the GRUB drift check (no cached artix-grub-live to compare with)"
fi

if [ "$do_iso" -eq 0 ]; then
    say "done (--bin: no ISO built)"
    exit 0
fi

# ── 5. The ISO ───────────────────────────────────────────────────────────────
# buildiso cleans its work directory on every run (-c disables that), so there
# is nothing to reset by hand: running it again IS the restart.
# buildiso locks each stage (`rootfs.lock`, `bootfs.lock`, `grub.lock`) and
# skips it on a later run. Its default is to clean the whole work directory
# first, so the locks usually die with it — but `-c` keeps them, and then a
# changed GRUB config would deploy, get copied, and never be read. Removing the
# two cheap locks costs seconds and makes that impossible; the rootfs lock is
# left alone because rebuilding it means downloading every package again.
sudo rm -f "/var/lib/artools/buildiso/$PROFILE/artix/grub.lock" \
           "/var/lib/artools/buildiso/$PROFILE/artix/bootfs.lock" 2>/dev/null || true

say "building the ISO into $ISO_DIR (sudo, takes a while)"
mkdir -p "$ISO_DIR"
[ -n "$BUILDISO" ] || die "no buildiso: vendor/artools is missing and artools is not installed"
# sudo -E keeps LIBDIR/DATADIR/SYSCONFDIR, or the vendored copy would look for
# its libraries in /usr/share and fail on a machine without artools.
#
# PATH has to be handed over separately: sudo replaces it from `secure_path` in
# sudoers no matter what -E says, so buildiso called `basestrap` and got
# "command not found" while the binary sat right beside it.
sudo -E env PATH="$VENDOR/bin:$PATH" "$BUILDISO" -p "$PROFILE" -t "$ISO_DIR"

# buildiso writes into <target>/<profile>/. Lift the artefacts one level so the
# folder has the images in it and nothing else to open. A move inside the same
# filesystem is instant and does not duplicate 1.5 GB.
if [ -d "$ISO_DIR/$PROFILE" ]; then
    find "$ISO_DIR/$PROFILE" -maxdepth 1 -type f -exec sudo mv -f {} "$ISO_DIR/" \;
    sudo rmdir "$ISO_DIR/$PROFILE" 2>/dev/null || true
fi
# buildiso runs as root, so its output does too. Hand it back, or deleting an
# old image needs sudo.
sudo chown -R "$(id -u):$(id -g)" "$ISO_DIR"

# Newest ISO by modification time. `ls -t` rather than find's -newermt: this
# machine's `find` is bfs, which rejects relative timestamps.
iso=$(ls -t "$ISO_DIR"/*.iso 2>/dev/null | head -1)
if [ -n "$iso" ]; then
    say "ISO: $iso  ($(du -h "$iso" | cut -f1))"
    printf '   test it:  sh scripts/qemu-test.sh\n'
    printf '   publish:  sh scripts/release.sh\n'
else
    say "ISO built; look in $ISO_DIR"
fi
