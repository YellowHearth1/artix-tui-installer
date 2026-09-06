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
        sudo umount "$work/esp" 2>/dev/null || true
        sudo umount "$work/other" 2>/dev/null || true
        # NEVER DELETE A TREE THAT STILL HAS SOMETHING MOUNTED IN IT. The ESP
        # was still mounted when this ran, so `rm -rf` walked INTO it and tried
        # to delete vmlinuz, the microcode and grubx64.efi. Only the read-only
        # mount saved it. Refusing is the correct answer; the temporary
        # directory is a few empty folders and losing it costs nothing.
        if findmnt -rno TARGET 2>/dev/null | grep -q "^$work"; then
            red "!! something is still mounted under $work — NOT deleting it"
            findmnt -rno TARGET | grep "^$work" | sed 's/^/     /'
            return
        fi
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
    # WAIT FOR THE PARTITIONS, do not guess. A fixed `sleep 2` was enough
    # sometimes and not others: the device nodes survive a disconnect, so the
    # next run could read stale ones and report "no Linux filesystem at all" on
    # a disk that plainly has three. Poll until the kernel has actually read the
    # table, then give up loudly.
    sudo partprobe "$nbd_dev" 2>/dev/null || true
    ready=""
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        if sudo blkid -o device "$nbd_dev"p* 2>/dev/null | grep -q .; then
            ready=yes
            break
        fi
        sleep 1
    done
    [ -n "$ready" ] || { bad "$nbd_dev exposed no partitions — is the image empty?"; return; }

    # `|| true`, AND IT IS NOT COSMETIC. The loop's last command is a test that
    # is FALSE on a disk with no LUKS, so the command substitution exits
    # non-zero — and under `set -e` that killed the whole script right here,
    # printing nothing after "opening". A tool that dies silently on the
    # ordinary case is worse than one that never worked.
    root_src=$(sudo blkid -o device "$nbd_dev"p* 2>/dev/null | while read -r p; do
        [ "$(sudo blkid -o value -s TYPE "$p")" = crypto_LUKS ] && { echo "$p"; break; }
    done || true)
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
        # ANY LINUX FILESYSTEM, not just btrfs, and the BIGGEST one.
        #
        # This looked for btrfs alone, so an ext4 install produced an empty
        # device path — and then every mount failed on "" and the run ended
        # with nothing printed at all. A verifier that says nothing is worse
        # than one that says no.
        root_dev=""
        biggest=0
        for p in $(sudo blkid -o device "$nbd_dev"p* 2>/dev/null); do
            case "$(sudo blkid -o value -s TYPE "$p")" in
                ext4|ext3|ext2|xfs|btrfs|f2fs) ;;
                *) continue ;;
            esac
            sz=$(sudo blockdev --getsize64 "$p" 2>/dev/null || echo 0)
            if [ "$sz" -gt "$biggest" ]; then biggest=$sz; root_dev=$p; fi
        done
        if [ -z "$root_dev" ]; then
            bad "no Linux filesystem on this disk at all — nothing to inspect"
            return
        fi
        say "root looks like $root_dev ($(sudo blkid -o value -s TYPE "$root_dev"))"
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
        || sudo mount -o ro,rescue=nologreplay "$root_dev" "$work/root" 2>/dev/null \
        || sudo mount -o ro,noload "$root_dev" "$work/root" 2>/dev/null \
        || sudo mount -o ro,norecovery "$root_dev" "$work/root" 2>/dev/null; then
        # One spelling per filesystem for the same idea — DO NOT REPLAY THE LOG.
        # A machine switched off hard comes back with a dirty journal, and
        # replaying it needs to WRITE, which a read-only nbd export cannot do:
        # btrfs calls it rescue=nologreplay, ext2/3/4 noload, xfs norecovery.
        # Each one was added after a report of "the root filesystem will not
        # mount", which sounds like a failed install and means only that the VM
        # was switched off rather than shut down.
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
        # The ESP is not "another partition" — it is half the boot path, so it
        # gets its own report rather than a mount test.
        if [ "$ty" = vfat ]; then
            mkdir -p "$work/esp"
            if sudo mount -o ro "$p" "$work/esp" 2>/dev/null; then
                report_esp "$work/esp"
                sudo umount "$work/esp"
            else
                bad "$p is vfat (the ESP) and WILL NOT MOUNT — see: sudo dmesg | tail"
            fi
            continue
        fi
        case "$ty" in swap|crypto_LUKS|"") continue ;; esac
        mkdir -p "$work/other"
        if sudo mount -o ro "$p" "$work/other" 2>/dev/null; then
            ok "$p ($ty, PARTLABEL=${lbl:-none}) mounts"
            sudo umount "$work/other"
        else
            bad "$p ($ty, PARTLABEL=${lbl:-none}) DOES NOT MOUNT — see: sudo dmesg | tail"
        fi
    done
}

# WOULD THIS SYSTEM ACTUALLY BOOT AND LOG IN?
#
# The earlier version printed three files and checked that fstab had a root
# line. That is not the question. A machine can have a perfect fstab and still
# stop at a login screen with no session behind it, or boot to a rescue prompt
# because the initramfs has no encrypt hook — both seen this week, and neither
# visible in what was printed.
#
# So each check below is one REASON A SYSTEM DOES NOT COME UP, asked separately,
# and named in the terms the failure appears in.
report_root() {
    r="$1"
    say "inside the installed system"

    # ── what it says about itself ──────────────────────────────────────────
    rec="$r/etc/artix-tui/install.conf"
    if sudo test -r "$rec"; then
        ok "the install left its layout record"
        sudo grep -E '^(version|date|hostname|scope|bootloader)' "$rec" 2>/dev/null | sed 's/^/       /'
    else
        bad "no /etc/artix-tui/install.conf — recovery would have to guess"
    fi

    # ── fstab ─────────────────────────────────────────────────────────────
    fst="$r/etc/fstab"
    if ! sudo test -r "$fst"; then
        # NOT "cannot boot" — that was wrong, and it is the same wrong sentence
        # the test aid used to print. The root is mounted from root=UUID= on the
        # kernel command line; fstab is read afterwards, by one dinit service
        # running `mount -a`, and nothing else. So a machine with no fstab comes
        # up looking healthy and quietly mounts none of the rest.
        bad "/etc/fstab is missing — it will still BOOT, and mount nothing else: no swap, no ESP"
    else
        n=$(sudo awk '!/^#/ && NF>=2' "$fst" | wc -l)
        [ "$n" -gt 0 ] || bad "/etc/fstab has no entries (the package stub)"
        sudo awk '!/^#/ && NF>=2 && $2 == "/"' "$fst" | grep -q . \
            && ok "fstab names a root" || bad "fstab has NO root line"
        sudo awk '!/^#/ && $2 ~ /^\/boot/' "$fst" | grep -q . \
            && ok "fstab mounts /boot or /boot/efi" \
            || bad "fstab has no /boot line — the kernel will not be updatable"
        # A btrfs layout keeps /var/log in its own subvolume. Without that line
        # every dinit service fails to open its log and the session dies.
        if sudo grep -q 'subvol=/*@' "$fst"; then
            sudo grep -q 'subvol=/*@log' "$fst" \
                && ok "fstab mounts @log" \
                || bad "btrfs layout but NO @log line — no dinit service will start"
        fi
        sudo grep -q 'subvolid=' "$fst" \
            && bad "fstab pins subvolid= — a rollback would mount the wrong snapshot" \
            || ok "subvolumes named by name, not by id"
        # THE SWAP LINE, because losing it is SILENT. Nothing about a machine
        # with no swap looks wrong: it boots, it logs in, and the only sign is
        # `swapon --show` printing nothing. That is exactly the damage a missing
        # fstab does on a simple layout — the boot never fails, so the only way
        # to tell the repair worked is to come and look.
        if sudo blkid -o value -s TYPE 2>/dev/null | grep -qx swap ||
            sudo grep -q '^swap|' "$rec" 2>/dev/null; then
            sudo awk '!/^#/ && NF>=3 && $3 == "swap"' "$fst" | grep -q . \
                && ok "fstab activates the swap partition" \
                || bad "there IS a swap partition, but no fstab line for it — it will never be used"
        fi
    fi

    # ── kernel side ───────────────────────────────────────────────────────
    # THE KERNEL MAY LIVE ON THE ESP. With an encrypted root the installer puts
    # the ESP at /boot, so the kernels are there and the root's own /boot is an
    # empty mount point — reporting "no kernel" for that is a false alarm on a
    # perfectly good system. The ESP is inspected separately; here the answer is
    # only recorded when it can be given.
    if sudo sh -c "ls $r/boot/vmlinuz-* >/dev/null 2>&1"; then
        ok "a kernel is present in the root's /boot"
        sudo sh -c "ls $r/boot/initramfs-*.img >/dev/null 2>&1" \
            && ok "an initramfs is present" || bad "kernel but NO initramfs in /boot"
    elif sudo sh -c "ls -A $r/boot 2>/dev/null | grep -q ." ; then
        bad "/boot has files but no kernel — is the right partition mounted there?"
    else
        skip "the root's /boot is empty — the kernels are on the ESP (checked below)"
    fi
    mk="$r/etc/mkinitcpio.conf"
    if sudo test -r "$mk" && sudo grep -q '/dev/mapper/' "$fst" 2>/dev/null; then
        sudo grep '^HOOKS=' "$mk" | grep -q encrypt \
            && ok "the initramfs is built with an encrypt hook" \
            || bad "encrypted root but NO encrypt hook — it will not unlock"
    fi

    # ── the login path, which is where "it boots but I cannot get in" lives ─
    users=$(sudo awk -F: '$3 >= 1000 && $3 < 65534 { print $1 " " $6 }' "$r/etc/passwd" 2>/dev/null)
    if [ -z "$users" ]; then
        bad "no ordinary user account exists"
    else
        printf '%s\n' "$users" | while read -r u h; do
            [ -n "$u" ] || continue
            if sudo test -d "$r$h"; then
                cnt=$(sudo ls -A "$r$h" 2>/dev/null | wc -l)
                if [ "$cnt" -gt 0 ]; then
                    ok "home for $u exists ($cnt entries)"
                else
                    bad "home for $u is EMPTY — the login will loop with no message"
                fi
            else
                bad "no home for $u at $h"
            fi
        done
    fi
    # A display manager with nothing to start is the exact shape of "SDDM comes
    # up and the password does nothing".
    dm=$(sudo sh -c "ls $r/etc/dinit.d/boot.d 2>/dev/null" | grep -iE 'sddm|lightdm|gdm|greetd' | head -1)
    ses=$(sudo sh -c "ls $r/usr/share/xsessions $r/usr/share/wayland-sessions 2>/dev/null" | grep -c '\.desktop$' || true)
    if [ -n "$dm" ]; then
        if [ "${ses:-0}" -gt 0 ]; then
            ok "login manager ($dm) and $ses session(s) to offer"
        else
            bad "login manager ($dm) is enabled but there is NO session to start"
        fi
    elif [ "${ses:-0}" -gt 0 ]; then
        skip "sessions exist but no display manager is enabled (console login)"
    else
        skip "no desktop installed — console only"
    fi
}

# WHAT IS ON THE ESP, which is the other half of "does it boot".
#
# Run against the ESP mounted read-only. The bootloader-id matters as much as
# the files: a repair that installs under a NEW name leaves the firmware
# pointing at the old, broken entry, and everything else here would still pass.
report_esp() {
    e="$1"
    say "on the EFI system partition"
    # DIRECTORIES ONLY. `ls` also listed artix-tui-layout.conf, a file this
    # installer drops beside them, and the count then reported "more than one
    # entry" on a perfectly normal ESP — a false alarm on the very check meant
    # to catch a repair that added a second entry.
    ids=$(sudo sh -c "find $e/EFI -mindepth 1 -maxdepth 1 -type d -printf '%f\\n' 2>/dev/null" \
          | grep -viE '^(boot|microsoft|tools)$' || true)
    if [ -n "$ids" ]; then
        ok "firmware entry director(y|ies): $(printf '%s' "$ids" | tr '\n' ' ')"
        [ "$(printf '%s\n' "$ids" | wc -l)" -gt 1 ] \
            && bad "MORE THAN ONE — a repair added a second entry instead of restoring the first"
    else
        bad "no bootloader directory on the ESP at all"
    fi
    sudo sh -c "find $e/EFI -iname '*.efi' 2>/dev/null" | grep -q . \
        && ok "at least one .efi binary is present" \
        || bad "no .efi binary anywhere on the ESP — nothing for the firmware to load"
    # The kernels, when the ESP is what /boot is. Reported here so that "no
    # kernel in the root" above has an answer rather than a shrug.
    sudo sh -c "ls $e/vmlinuz-* >/dev/null 2>&1" \
        && ok "kernel(s) on the ESP: $(sudo sh -c "ls -1 $e/vmlinuz-* 2>/dev/null" | xargs -n1 basename | tr '\n' ' ')" \
        || skip "no kernel on the ESP (expected when /boot is a separate partition)"
    sudo test -f "$e/EFI/BOOT/BOOTX64.EFI" \
        && ok "the removable fallback is in place" \
        || skip "no \\EFI\\BOOT\\BOOTX64.EFI (only needed if the NVRAM entry is lost)"
    for f in artix-tui-layout.conf artix-test-install.log; do
        sudo test -r "$e/$f" && ok "$f is on the ESP"
    done
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
