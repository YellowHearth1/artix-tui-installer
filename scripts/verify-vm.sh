#!/bin/sh
# Check an installed system by reading its VM disk image, without booting it.
#
# WHY THIS EXISTS. "Did the install work?" has been answered all week by
# booting the VM, watching it fail, and screenshotting the failure. Most of
# those failures were visible in the files: an fstab with no /var/log line, a
# /home that mounts an empty subvolume, a missing kernel. Those are questions a
# script can ask in a few seconds, before anybody looks at a screen.
#
#   sh scripts/verify-vm.sh                 # every disk in vm/
#   sh scripts/verify-vm.sh vm/disk1-ssd.qcow2
#
# READ-ONLY, and it refuses to touch a disk a running QEMU has open. libguestfs
# runs its own tiny virtual machine to do the reading, so this needs NO root —
# which is the whole reason it is preferred here over qemu-nbd, which would
# want `modprobe nbd` and a mount in the host kernel.
set -eu

# `unset CDPATH` rather than the `CDPATH= cd` idiom: they do the same thing, but
# the linter reads the second as a typo'd assignment (SC1007), and CI runs at
# `-S warning`, so it fails the build. An idiom the linter cannot recognise is
# not worth the cleverness.
#
# (A comment line must not START with the linter's own name either — that is
# read as a malformed directive, SC1073. This one cost a second CI run.)
SELF_DIR=$(unset CDPATH; cd -- "$(dirname -- "$0")" && pwd)
REPO_DIR=$(dirname -- "$SELF_DIR")
VM_DIR="${ARTIX_VM_DIR:-$REPO_DIR/vm}"

red()  { printf '\033[1;31m%s\033[0m\n' "$*"; }
grn()  { printf '\033[1;32m%s\033[0m\n' "$*"; }
say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m..\033[0m %s\n' "$*"; }

command -v virt-cat >/dev/null 2>&1 || {
    red "guestfs-tools is not installed."
    echo "   doas pacman -S libguestfs guestfs-tools"
    exit 1
}

# ── The nbd route, for when libguestfs cannot open LUKS ─────────────────────
# Not every distribution builds libguestfs with the luks feature (Artix's does
# not, as of writing: "feature 'luks' is not available in this build"). That
# makes the tool useless for exactly the layout this installer produces by
# default, so there is a second way in: qemu-nbd exposes the image, cryptsetup
# opens the container with the KEYFILE OFF THE USB STICK IMAGE, and everything
# is mounted read-only.
#
# Everything is undone by the trap, in reverse: unmount, close the mapper,
# disconnect nbd, shred the copied key. A read-only inspection that leaves a
# device mapper open behind it is how the next run finds a busy disk.
#
#   sh scripts/verify-vm.sh --nbd            # uses vm/usbkey.img for the key
#   sh scripts/verify-vm.sh --nbd vm/disk1-nvme.qcow2
nbd_inspect() {
    img="$1"
    key_img="${ARTIX_USBKEY_IMG:-$VM_DIR/usbkey.img}"
    work=$(mktemp -d)
    nbd_dev=""
    # shellcheck disable=SC2317  # invoked through the EXIT trap
    undo() {
        # ALL OF THIS RAN AS ROOT, so undoing it has to as well. Without the
        # sudo the teardown failed silently and left the mapper open, and the
        # NEXT run then died on "the keyfile does not open" — blaming the key
        # for a name that was still taken.
        sudo umount -R "$work/root" 2>/dev/null || true
        findmnt -rno TARGET --source /dev/mapper/artix_verify 2>/dev/null |
            while read -r m; do sudo umount -R "$m" 2>/dev/null || true; done
        sudo cryptsetup close artix_verify 2>/dev/null || true
        [ -n "$nbd_dev" ] && sudo qemu-nbd -d "$nbd_dev" >/dev/null 2>&1
        sudo umount "$work/key" 2>/dev/null || true
        sudo rm -rf "$work"
    }
    trap undo EXIT INT TERM
    mkdir -p "$work/key" "$work/root"

    # CLEAR ANY WRECKAGE FROM A PREVIOUS RUN FIRST. An interrupted inspection
    # (or an nbd device pulled out from under it) leaves the mapper open and
    # holding a mount, and the next run then fails with "the keyfile does not
    # open" — which points at the key, the one thing that was fine. Whatever is
    # still mounted from it is unmounted BY SOURCE, because the temporary
    # directory it was mounted on belonged to the run that died.
    if [ -e /dev/mapper/artix_verify ]; then
        warn "clearing a mapper left open by an earlier run"
        findmnt -rno TARGET --source /dev/mapper/artix_verify 2>/dev/null |
            while read -r m; do sudo umount -R "$m" 2>/dev/null || true; done
        sudo cryptsetup close artix_verify 2>/dev/null || true
    fi

    if in_use "$img"; then
        skip "a running QEMU has this disk open — shut the VM down first"
        return
    fi

    say "opening $(basename "$img") through nbd (read-only)"
    sudo modprobe nbd max_part=8 || { bad "the nbd module will not load"; return; }
    for n in 0 1 2 3; do
        if sudo qemu-nbd --read-only --connect="/dev/nbd$n" "$img" 2>/dev/null; then
            nbd_dev="/dev/nbd$n"
            break
        fi
    done
    [ -n "$nbd_dev" ] || { bad "no free nbd device"; return; }
    sleep 2

    root_src=$(sudo blkid -o device "$nbd_dev"p* 2>/dev/null | while read -r p; do
        [ "$(sudo blkid -o value -s TYPE "$p")" = crypto_LUKS ] && { echo "$p"; break; }
    done)
    if [ -n "$root_src" ]; then
        [ -r "$key_img" ] || { skip "encrypted root and no $key_img to unlock it with"; return; }
        sudo mount -o loop,ro "$key_img" "$work/key" || { bad "the USB key image will not mount"; return; }
        kf=$(sudo find "$work/key" -maxdepth 1 -name '*.key' | head -1)
        [ -n "$kf" ] || { bad "no *.key on the USB key image"; return; }
        sudo cp "$kf" "$work/k.bin"; sudo umount "$work/key"
        sudo cryptsetup open --readonly --key-file "$work/k.bin" "$root_src" artix_verify \
            || { bad "the keyfile does not open $root_src"; return; }
        root_dev=/dev/mapper/artix_verify
    else
        root_dev=$(sudo blkid -o device "$nbd_dev"p* 2>/dev/null | while read -r p; do
            [ "$(sudo blkid -o value -s TYPE "$p")" = btrfs ] && { echo "$p"; break; }
        done)
    fi
    # A PLAIN READ-ONLY MOUNT FIRST, and `rescue=nologreplay` only if that
    # fails — with the fact said out loud.
    #
    # Skipping the log tree shows the filesystem as of the last full commit, so
    # anything written and fsync'd since is INVISIBLE. That is not a corner
    # case: it hid an /etc/fstab edit made minutes earlier, and the report then
    # said the line was missing from a system that was demonstrably booting with
    # it. A verifier that quietly reads an older copy is worse than one that
    # refuses, because its answer looks the same either way.
    stale=""
    if sudo mount -o ro,subvol=@ "$root_dev" "$work/root" 2>/dev/null; then
        :
    elif sudo mount -o ro "$root_dev" "$work/root" 2>/dev/null; then
        :
    elif sudo mount -o ro,rescue=nologreplay,subvol=@ "$root_dev" "$work/root" 2>/dev/null \
        || sudo mount -o ro,rescue=nologreplay "$root_dev" "$work/root" 2>/dev/null; then
        stale=yes
    else
        bad "the root filesystem will not mount"
        return
    fi
    [ -n "$stale" ] && warn "  !! the log tree could not be replayed, so this is the state at the
     last full commit — anything written just before shutdown is NOT shown.
     Shut the VM down cleanly and run again before trusting a missing line."

    report_root "$work/root"

    # EVERY OTHER PARTITION, mounted on its own. A partition that will not mount
    # is the failure this was written for: a stale filesystem left behind by an
    # earlier, larger version of the same partition still carries its old size,
    # and the kernel refuses it — silently, because nothing in the install ever
    # asks whether the mount succeeded.
    for p in $(sudo blkid -o device "$nbd_dev"p* 2>/dev/null); do
        [ "$p" = "$root_src" ] && continue
        ty=$(sudo blkid -o value -s TYPE "$p")
        lbl=$(sudo blkid -o value -s PARTLABEL "$p")
        case "$ty" in vfat|swap|crypto_LUKS|"") continue ;; esac
        mkdir -p "$work/other"
        if sudo mount -o ro "$p" "$work/other" 2>/dev/null; then
            ok "$p ($ty, PARTLABEL=${lbl:-none}) mounts"
            sudo umount "$work/other"
        else
            bad "$p ($ty, PARTLABEL=${lbl:-none}) DOES NOT MOUNT — see: sudo dmesg | tail"
        fi
    done
}

# What the installed root has to say for itself.
report_root() {
    r="$1"
    say "inside the installed system"
    for f in etc/fstab etc/X11/xorg.conf.d/00-keyboard.conf etc/dconf/db/local.d/00-input-sources; do
        if sudo test -r "$r/$f"; then
            printf '\n--- /%s ---\n' "$f"
            sudo grep -v '^#' "$r/$f" | awk 'NF'
        else
            bad "/$f is missing"
        fi
    done
    sudo awk '!/^#/ && NF>=2 && $2 == "/"' "$r/etc/fstab" | grep -q . \
        && ok "fstab names a root" || bad "fstab has NO root line"
    sudo awk '!/^#/ && $2 == "/home"' "$r/etc/fstab" | grep -q . \
        && ok "fstab mounts a separate /home" \
        || skip "no /home line in fstab (only a problem if you planned one)"
}

fails=0
checks=0
ok()   { checks=$((checks+1)); grn "  ok    $*"; }
bad()  { checks=$((checks+1)); fails=$((fails+1)); red "  FAIL  $*"; }
skip() { warn "  --    $*"; }

# A disk a running QEMU has open must not be read: the guest's writes are in
# flight and what we would see is a torn copy of them. Refusing is the only
# honest answer — and it protects the image, which may hold a test system that
# took twenty minutes to install.
in_use() {
    pgrep -af 'qemu-system' 2>/dev/null | grep -qF "$1"
}

check_disk() {
    img="$1"
    say "$(basename "$img")"
    if in_use "$img"; then
        skip "a running QEMU has this disk open — shut the VM down first"
        return
    fi

    # AN ENCRYPTED ROOT IS THE DEFAULT LAYOUT HERE, so a verifier that cannot
    # look inside one checks nothing that matters. libguestfs opens LUKS given a
    # key; point ARTIX_LUKS_KEYFILE at a file holding the passphrase, or set
    # ARTIX_LUKS_PASS.
    #
    # A file is preferred and is what the passphrase form is written into: an
    # argument is visible to every process on the machine through `ps`.
    keyargs=""
    if [ -n "${ARTIX_LUKS_KEYFILE:-}" ] && [ -r "${ARTIX_LUKS_KEYFILE}" ]; then
        keyargs="--key all:file:${ARTIX_LUKS_KEYFILE}"
    elif [ -n "${ARTIX_LUKS_PASS:-}" ]; then
        tmpkey=$(mktemp); chmod 600 "$tmpkey"
        printf '%s' "$ARTIX_LUKS_PASS" > "$tmpkey"
        keyargs="--key all:file:$tmpkey"
    fi

    # One `guestfish` session for everything: starting the appliance is the slow
    # part, so asking it twenty questions costs barely more than asking one.
    # shellcheck disable=SC2086  # keyargs is a deliberate word-split option pair
    out=$(guestfish --ro -a "$img" $keyargs -i <<'EOF' 2>/dev/null || true
echo "--ROOTS--"
inspect-get-roots
echo "--FSTAB--"
cat /etc/fstab
echo "--PASSWD--"
cat /etc/passwd
echo "--BOOTLS--"
ls /boot
echo "--HOMELS--"
ls /home
echo "--RECORD--"
cat /etc/artix-tui/install.conf
echo "--MKINIT--"
cat /etc/mkinitcpio.conf
EOF
)
    [ -n "${tmpkey:-}" ] && { rm -f "$tmpkey"; unset tmpkey; }
    if [ -z "$out" ]; then
        if [ -z "$keyargs" ]; then
            skip "nothing readable here — an empty disk, or an ENCRYPTED root."
            skip "  for an encrypted install, give it the passphrase:"
            skip "    ARTIX_LUKS_KEYFILE=/path/to/file sh scripts/verify-vm.sh"
        else
            skip "nothing readable even with the key — wrong passphrase, or an empty disk"
        fi
        return
    fi

    sec() { printf '%s\n' "$out" | sed -n "/^--$1--$/,/^--/p" | sed '1d;$d'; }

    fstab=$(sec FSTAB)
    if [ -z "$fstab" ]; then
        bad "/etc/fstab is missing entirely"
    else
        n=$(printf '%s\n' "$fstab" | awk '!/^#/ && NF>=2' | wc -l)
        if [ "$n" -eq 0 ]; then
            bad "/etc/fstab has no entries — the filesystem package's stub"
        else
            ok "/etc/fstab has $n entries"
        fi
        printf '%s\n' "$fstab" | awk '!/^#/ && NF>=2 && $2 == "/"' | grep -q . \
            && ok "fstab names a root" || bad "fstab has NO root line — cannot boot"
        # A btrfs layout here keeps /var/log in its own subvolume. Without that
        # line every dinit service fails to open its log and the session dies.
        if printf '%s\n' "$fstab" | grep -q 'subvol=/*@'; then
            printf '%s\n' "$fstab" | grep -q 'subvol=/*@log' \
                && ok "fstab mounts the @log subvolume" \
                || bad "btrfs layout but NO @log line — dinit services will not start"
        fi
        printf '%s\n' "$fstab" | grep -q 'subvolid=' \
            && bad "fstab pins subvolid= — a snapper rollback will mount the wrong snapshot" \
            || ok "subvolumes named by name, not by id"
    fi

    bootls=$(sec BOOTLS)
    printf '%s\n' "$bootls" | grep -q '^vmlinuz-' \
        && ok "a kernel is present in /boot" \
        || bad "no kernel in /boot"
    printf '%s\n' "$bootls" | grep -q '^initramfs-.*\.img' \
        && ok "an initramfs is present" \
        || bad "no initramfs in /boot"

    # Every real user must have a home that exists and is not empty, or the
    # display manager takes the password and bounces straight back.
    # NOT a `while read` in a pipeline: that runs in a SUBSHELL, so a failure
    # counted inside it would never reach the total and a broken home would be
    # reported on screen while the script still exited 0. (Learned the hard way
    # in the recovery repair, where the same shape made a whole search silently
    # never run.)
    homels=$(sec HOMELS)
    users=$(printf '%s\n' "$(sec PASSWD)" | awk -F: '$3 >= 1000 && $3 < 65534 { print $1 }')
    for u in $users; do
        [ -n "$u" ] || continue
        if printf '%s\n' "$homels" | grep -qx "$u"; then
            ok "home for $u exists"
        else
            bad "no home for $u — the login will loop with no message"
        fi
    done

    [ -n "$(sec RECORD)" ] \
        && ok "the install left its layout record" \
        || bad "no /etc/artix-tui/install.conf — recovery will have to guess"

    mk=$(sec MKINIT)
    if printf '%s\n' "$fstab" | grep -q '/dev/mapper/'; then
        printf '%s\n' "$mk" | grep '^HOOKS=' | grep -q 'encrypt' \
            && ok "the initramfs is built with an encrypt hook" \
            || bad "encrypted root but NO encrypt hook — it will not unlock"
    fi
}

if [ "${1:-}" = "--nbd" ]; then
    shift
    img="${1:-$VM_DIR/disk1-nvme.qcow2}"
    [ -r "$img" ] || { red "no such image: $img"; exit 1; }
    nbd_inspect "$img"
elif [ $# -gt 0 ]; then
    for f in "$@"; do check_disk "$f"; done
else
    [ -d "$VM_DIR" ] || { red "no VM directory at $VM_DIR"; exit 1; }
    found=0
    for f in "$VM_DIR"/*.qcow2; do
        [ -e "$f" ] || continue
        found=1
        check_disk "$f"
    done
    [ "$found" = 1 ] || { warn "no disk images in $VM_DIR"; exit 0; }
fi

echo
if [ "$checks" -eq 0 ]; then
    warn "nothing was checked — no readable system was found on any disk."
    warn "That is not a pass. See the hint above about encrypted roots."
    exit 2
elif [ "$fails" -eq 0 ]; then
    grn "$checks checks, all passed"
else
    red "$checks checks, $fails failed"
    exit 1
fi
