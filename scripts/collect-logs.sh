#!/bin/sh
# Gather everything worth looking at after an install, into one archive.
#
# WHY THIS EXISTS. Reporting a problem currently means remembering which files
# matter, finding them under different names on the live image and on the
# installed system, and pasting them one at a time. Half the debugging in this
# project has been spent asking for a file that was already on the machine.
#
# Run it EITHER on the installed system, OR from the live ISO with the target
# mounted at /mnt (the recovery mode leaves it there). It works out which it is
# looking at and says so.
#
#   sh collect-logs.sh              # this system
#   sh collect-logs.sh /mnt         # a system mounted at /mnt
#
# Writes artix-logs-<host>-<date>.tar.gz into the current directory, and prints
# a short summary so the obvious problems are visible without unpacking it.
#
# Reads only. Nothing here writes to the system being examined.
set -eu

ROOT="${1:-/}"
[ -d "$ROOT" ] || { echo "no such directory: $ROOT" >&2; exit 1; }
case "$ROOT" in */) ROOT="${ROOT%/}" ;; esac
[ -n "$ROOT" ] || ROOT=""

say() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m..\033[0m %s\n' "$*"; }

host=$(cat "$ROOT/etc/hostname" 2>/dev/null || echo unknown)
stamp=$(date +%Y%m%d-%H%M%S)
out="artix-logs-$host-$stamp"
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
mkdir -p "$dir/$out"
D="$dir/$out"

if [ "$ROOT" = "" ]; then
    say "Collecting from the RUNNING system ($host)"
else
    say "Collecting from the system mounted at $ROOT ($host)"
fi

# ── Files that describe how the system was built ────────────────────────────
# The install record first: it is the only file that says what the layout was
# MEANT to be, which is what every other question here ends up being compared
# against.
for f in \
    /etc/artix-tui/install.conf \
    /boot/artix-tui-layout.conf \
    /boot/efi/artix-tui-layout.conf \
    /etc/fstab \
    /etc/crypttab \
    /etc/mkinitcpio.conf \
    /etc/vconsole.conf \
    /etc/locale.conf \
    /etc/locale.gen \
    /etc/hostname \
    /etc/doas.conf \
    /etc/default/grub \
    /etc/X11/xorg.conf.d/00-keyboard.conf \
    /etc/dconf/db/local.d/00-input-sources \
    /etc/sddm/Xsetup-artix
do
    [ -r "$ROOT$f" ] || continue
    mkdir -p "$D/files$(dirname "$f")"
    cp -a "$ROOT$f" "$D/files$f" 2>/dev/null || true
done

# fstab backups the recovery mode leaves behind: comparing them is often the
# fastest way to see what a repair changed.
for f in "$ROOT"/etc/fstab.bak "$ROOT"/etc/fstab.before-recovery.*; do
    [ -r "$f" ] || continue
    mkdir -p "$D/files/etc"
    cp -a "$f" "$D/files/etc/" 2>/dev/null || true
done

# Directory listings rather than the whole trees.
mkdir -p "$D/listings"
ls -la "$ROOT/boot" > "$D/listings/boot.txt" 2>&1 || true
ls -la "$ROOT/boot/efi/EFI" > "$D/listings/esp-efi.txt" 2>&1 || true
ls -la "$ROOT/etc/dinit.d" > "$D/listings/dinit.d.txt" 2>&1 || true
ls -la "$ROOT/etc/dinit.d/boot.d" > "$D/listings/dinit-boot.d.txt" 2>&1 || true
ls -la "$ROOT/etc/xdg/autostart" > "$D/listings/xdg-autostart.txt" 2>&1 || true
ls -la "$ROOT/var/log" > "$D/listings/var-log.txt" 2>&1 || true
ls -la "$ROOT/var/log/dinit" > "$D/listings/var-log-dinit.txt" 2>&1 || true
ls -la "$ROOT/home" > "$D/listings/home.txt" 2>&1 || true

# ── Logs ─────────────────────────────────────────────────────────────────────
mkdir -p "$D/logs"
# The installer's own log, wherever it ended up.
for f in "$ROOT"/root/installer.log "$ROOT"/home/*/installer.log "$ROOT"/var/log/artix-installer.log; do
    [ -r "$f" ] || continue
    cp -a "$f" "$D/logs/installer-$(basename "$(dirname "$f")").log" 2>/dev/null || true
done
# syslog-ng's collection, and dinit's per-service logs.
for f in everything.log messages errors daemon.log pacman.log Xorg.0.log; do
    [ -r "$ROOT/var/log/$f" ] || continue
    tail -c 2000000 "$ROOT/var/log/$f" > "$D/logs/$f" 2>/dev/null || true
done
if [ -d "$ROOT/var/log/dinit" ]; then
    mkdir -p "$D/logs/dinit"
    for f in "$ROOT"/var/log/dinit/*; do
        [ -r "$f" ] || continue
        tail -c 200000 "$f" > "$D/logs/dinit/$(basename "$f")" 2>/dev/null || true
    done
fi

# ── State that only exists while the system is running ───────────────────────
if [ "$ROOT" = "" ]; then
    mkdir -p "$D/state"
    { uname -a; echo; cat /proc/cmdline; } > "$D/state/kernel.txt" 2>&1 || true
    lsblk -o NAME,PATH,SIZE,TYPE,FSTYPE,UUID,MOUNTPOINTS > "$D/state/lsblk.txt" 2>&1 || true
    findmnt -A > "$D/state/mounts.txt" 2>&1 || true
    blkid > "$D/state/blkid.txt" 2>&1 || true
    free -h > "$D/state/memory.txt" 2>&1 || true
    df -h > "$D/state/disk-usage.txt" 2>&1 || true
    dinitctl list > "$D/state/services.txt" 2>&1 || true
    efibootmgr -v > "$D/state/efi-entries.txt" 2>&1 || true
    setxkbmap -query > "$D/state/keyboard.txt" 2>&1 || true
    pacman -Qqe > "$D/state/packages-explicit.txt" 2>&1 || true
    dmesg > "$D/state/dmesg.txt" 2>&1 || true
fi

# ── A summary worth reading before unpacking anything ────────────────────────
sum="$D/SUMMARY.txt"
{
    echo "Artix TUI installer — log collection"
    echo "host:      $host"
    echo "collected: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "source:    ${ROOT:-/ (running system)}"
    echo
    echo "== install record =="
    if [ -r "$ROOT/etc/artix-tui/install.conf" ]; then
        cat "$ROOT/etc/artix-tui/install.conf"
    else
        echo "(none — installed before the installer started writing one,"
        echo " or deleted; recovery then has to guess the layout)"
    fi
    echo
    echo "== fstab =="
    if [ -r "$ROOT/etc/fstab" ]; then
        n=$(awk '!/^#/ && NF>=2' "$ROOT/etc/fstab" | wc -l)
        echo "size $(wc -c < "$ROOT/etc/fstab") bytes, $n real entries"
        awk '!/^#/ && NF>=2 && $2 == "/"' "$ROOT/etc/fstab" | grep -q . \
            || echo "!! NO ROOT ENTRY — this system cannot boot from it"
        [ "$n" = 0 ] && echo "!! NO ENTRIES AT ALL — this is the filesystem package's stub"
        cat "$ROOT/etc/fstab"
    else
        echo "!! MISSING"
    fi
    echo
    echo "== boot =="
    ls "$ROOT"/boot/vmlinuz-* >/dev/null 2>&1 \
        && echo "kernel(s): $(ls "$ROOT"/boot/vmlinuz-* | tr '\n' ' ')" \
        || echo "!! no kernel in /boot (is the right partition mounted there?)"
    ls "$ROOT"/boot/initramfs-*.img >/dev/null 2>&1 \
        && echo "initramfs: present" || echo "!! no initramfs"
    echo
    echo "== homes =="
    awk -F: '$3 >= 1000 && $3 < 65534 { print $1 " " $6 }' "$ROOT/etc/passwd" 2>/dev/null \
    | while read -r u h; do
        if [ -d "$ROOT$h" ]; then
            echo "$u -> $h : $(ls -A "$ROOT$h" 2>/dev/null | wc -l) entries, owner $(stat -c '%U' "$ROOT$h" 2>/dev/null)"
        else
            echo "!! $u -> $h : DOES NOT EXIST (the login will loop)"
        fi
    done
    echo
    echo "== failed services =="
    if [ "$ROOT" = "" ]; then
        # dinit marks a running service with a `+` inside the brackets. Anything
        # WITHOUT one is stopped or failed, which is the only interesting half.
        if command -v dinitctl >/dev/null 2>&1; then
            bad=$(dinitctl list 2>/dev/null | awk '$0 !~ /\+/ && NF > 0')
            if [ -n "$bad" ]; then printf '%s\n' "$bad"; else echo "(none — every service is up)"; fi
        else
            echo "(dinitctl unavailable)"
        fi
    else
        echo "(not a running system)"
    fi
} > "$sum" 2>&1

tar -czf "./$out.tar.gz" -C "$dir" "$out"
say "Written: $out.tar.gz"
echo
cat "$sum"
echo
warn "Passwords are not collected, but check the archive before sharing it:"
warn "  tar -tzf $out.tar.gz"
