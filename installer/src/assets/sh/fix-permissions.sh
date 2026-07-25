#!/bin/sh
# Repair file ownership and permissions on an installed system, after a
# `chmod 777 /`, a `chown -R` gone wrong, or the classic slip of typing `chown`
# where `chmod` was meant (the mode lands as a UID: /home ends up owned by user
# "777", which does not exist).
#
# Runs INSIDE the chroot of the target system, from the recovery screen.
#
# Two passes, in this order on purpose:
#
#   1. A fixed table of paths whose correct mode is not negotiable and is not
#      recorded anywhere on disk: /, /tmp, /root, the shadow files, ssh host
#      keys, each user's own home. This needs no network and no package
#      database, so it works even on a badly damaged system — and it is the
#      part that gets a machine usable again.
#
#   2. Everything a package owns, restored from the package database, which is
#      the only authority on the other ~200k files (including every setuid
#      binary). `pacman -Qkk` reports the mismatches; reinstalling just the
#      affected packages rewrites their modes and owners. Needs the packages
#      themselves — from the cache when they are there, otherwise the network.
#      Best-effort: pass 1 has already made the system workable.
#
# Nothing here deletes anything. Wrong permissions are repaired in place, and a
# path that is already correct is left untouched.
set -u

# Everything is addressed under $ROOT, empty in production (the script already
# runs inside the target's chroot). A test can point it at a throwaway tree and
# watch the repair actually happen, instead of the script being taken on trust —
# it changes permissions on someone's system, so it gets to be verifiable.
ROOT="${FIXPERM_ROOT:-}"

echo "=== Repairing ownership and permissions ==="
[ -n "$ROOT" ] && echo "=== (test mode: operating under $ROOT) ==="
echo

# ── Pass 1: paths with a fixed, well-known answer ───────────────────────────
echo ">>> Pass 1: core system paths"

fix() { # fix <mode> <owner> <path...>
    mode=$1; owner=$2; shift 2
    for p in "$@"; do
        p="$ROOT$p"
        [ -e "$p" ] || continue
        now="$(stat -c '%a %U:%G' "$p" 2>/dev/null)"
        [ "$now" = "$mode $owner" ] && continue
        # Applied and reported independently: a mode that can be fixed should
        # not be skipped because the ownership change failed (or vice versa).
        chmod "$mode" "$p" 2>/dev/null
        chown "$owner" "$p" 2>/dev/null
        echo "    $p: $now -> $(stat -c '%a %U:%G' "$p" 2>/dev/null)"
    done
}

fix 755 root:root / /boot /etc /usr /var /opt /srv /mnt /media /home /usr/bin /usr/lib
# Sticky world-writable: everyone may create, only the owner may delete. Without
# the sticky bit any user can delete another's files there.
fix 1777 root:root /tmp /var/tmp
fix 750 root:root /root
fix 755 root:root /var/empty /var/log /var/cache /var/lib
# Account databases: the shadow pair must not be world-readable — that is what
# keeps password hashes out of reach of every local process.
fix 644 root:root /etc/passwd /etc/group /etc/hosts /etc/fstab
fix 600 root:root /etc/shadow /etc/gshadow
fix 440 root:root /etc/sudoers
fix 755 root:root /etc/sudoers.d
# doas.conf is a FILE and must never pass through a permissive mode on its way
# to the right one — doas refuses to run when it is writable by anyone else.
fix 400 root:root /etc/doas.conf

# SSH host keys: a readable private host key lets anyone impersonate this host.
# `fix` prefixes $ROOT itself, so strip it back off the expanded glob.
for k in "$ROOT"/etc/ssh/ssh_host_*_key; do
    [ -e "$k" ] && fix 600 root:root "${k#"$ROOT"}"
done
for k in "$ROOT"/etc/ssh/ssh_host_*_key.pub; do
    [ -e "$k" ] && fix 644 root:root "${k#"$ROOT"}"
done

# Each real user's home belongs to that user — read from the passwd database,
# never guessed from the directory name. Only homes under /home are touched, so
# service accounts pointing at /var/lib/... are left alone.
echo ">>> Pass 1: user home directories"
awk -F: '$3 >= 1000 && $3 < 65534 && $6 ~ "^/home/" {print $1 ":" $3 ":" $4 ":" $6}' \
    "$ROOT/etc/passwd" |
while IFS=: read -r name uid gid home; do
    home="$ROOT$home"
    [ -d "$home" ] || continue
    owner="$(stat -c '%u:%g' "$home" 2>/dev/null)"
    if [ "$owner" != "$uid:$gid" ]; then
        echo "    $home: recursively giving it back to $name ($uid:$gid)"
        chown -R "$uid:$gid" "$home" 2>/dev/null
    fi
    chmod 755 "$home" 2>/dev/null
    # Private by convention and by requirement: ssh and gnupg refuse to work at
    # all when their directories are group- or world-readable.
    [ -d "$home/.ssh" ] && chmod 700 "$home/.ssh" 2>/dev/null
    [ -d "$home/.gnupg" ] && chmod 700 "$home/.gnupg" 2>/dev/null
    for f in "$home"/.ssh/id_* "$home"/.ssh/*.pem; do
        [ -f "$f" ] && case "$f" in *.pub) ;; *) chmod 600 "$f" 2>/dev/null ;; esac
    done
done

echo
echo ">>> Pass 1 done — the system is usable again."
echo

# ── Pass 2: everything the package manager owns ─────────────────────────────
echo ">>> Pass 2: checking every packaged file against the package database"
echo "    (this reads ~200k files and takes a few minutes)"

# Hard stop under a test root: pacman has no --root here and would happily
# inspect and reinstall over the HOST system instead of the tree under test.
if [ -n "$ROOT" ]; then
    echo ">>> (test mode: pass 2 skipped — it would act on the host, not \$ROOT)"
    exit 0
fi

if ! command -v pacman >/dev/null 2>&1; then
    echo "!! pacman not found — skipping the packaged-file pass."
    exit 0
fi

# -Qkk compares mode, owner and checksum. Only ownership/permission mismatches
# matter here: a changed checksum is usually just an edited config file, and
# reinstalling over it would throw the user's edits away.
damaged="$(pacman -Qkk 2>/dev/null |
    grep -E '\((Permissions|UID|GID) mismatch\)' |
    grep -v '^backup file:' |
    sed 's/^[^:]*: *//; s/:.*//' |
    sort -u)"

if [ -z "$damaged" ]; then
    echo ">>> No packaged file has wrong ownership or permissions. Nothing to do."
    exit 0
fi

count="$(printf '%s\n' "$damaged" | wc -l)"
echo ">>> $count package(s) have files with wrong ownership or permissions:"
printf '%s\n' "$damaged" | head -40 | sed 's/^/    /'
[ "$count" -gt 40 ] && echo "    … and $((count - 40)) more"
echo
echo ">>> Reinstalling them to restore the modes and owners the packages declare."
echo "    Config files you edited are kept (only mode and owner are rewritten)."

# --overwrite '*' because the files ARE on disk already; without it pacman stops
# at the first "exists in filesystem". Try the cache first so a machine with no
# network still gets repaired as far as its cache allows.
# shellcheck disable=SC2086 # $damaged is a deliberately word-split package list
if pacman -S --noconfirm --overwrite '*' $damaged; then
    echo
    echo ">>> Done. Re-checking…"
    left="$(pacman -Qkk 2>/dev/null |
        grep -E '\((Permissions|UID|GID) mismatch\)' |
        grep -v '^backup file:' | wc -l)"
    if [ "$left" -eq 0 ]; then
        echo ">>> All packaged files are back to their declared ownership."
    else
        echo "!! $left file(s) still mismatch — see 'pacman -Qkk' for details."
    fi
else
    echo
    echo "!! Reinstalling failed — most likely no network and nothing cached."
    echo "!! Pass 1 already repaired the core paths, so the system should boot."
    echo "!! With a connection, finish the job from the installed system:"
    echo "!!     sudo pacman -S \$(pacman -Qkk 2>/dev/null | grep -E '\\((Permissions|UID|GID) mismatch\\)' | grep -v '^backup file:' | sed 's/^[^:]*: *//; s/:.*//' | sort -u)"
fi
