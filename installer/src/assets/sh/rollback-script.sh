#!/bin/sh
# artix-rollback — roll the btrfs root subvolume (@) back to a snapshot.
# Installed by the Artix installer. Usage: sudo artix-rollback [NUMBER]
set -eu

UK=0
case "${LANG:-}" in uk*|*UA*|*ua*) UK=1 ;; esac
msg() { if [ "$UK" = 1 ]; then printf '%s\n' "$2"; else printf '%s\n' "$1"; fi; }

if [ "$(id -u)" -ne 0 ]; then
    msg "Please run as root: sudo artix-rollback" "Запусти від root: sudo artix-rollback"
    exit 1
fi

DEV=$(findmnt -no SOURCE / | sed 's/\[.*//')
if [ -z "$DEV" ]; then
    msg "Could not determine the root device." "Не вдалося визначити кореневий пристрій."
    exit 1
fi

msg "Available snapshots (config: root):" "Доступні знімки (конфіг: root):"
snapper -c root list || true
echo

N="${1:-}"
if [ -z "$N" ]; then
    if [ "$UK" = 1 ]; then printf 'Номер знімка для відкату: '; else printf 'Snapshot number to roll back to: '; fi
    read -r N
fi
case "$N" in ''|*[!0-9]*)
    msg "Invalid snapshot number." "Хибний номер знімка."
    exit 1 ;;
esac

TOP=$(mktemp -d)
cleanup() { umount "$TOP" 2>/dev/null || true; rmdir "$TOP" 2>/dev/null || true; }
trap cleanup EXIT INT TERM
mount -o subvolid=5 "$DEV" "$TOP"

if [ ! -d "$TOP/@snapshots/$N/snapshot" ]; then
    msg "Snapshot $N not found." "Знімок $N не знайдено."
    exit 1
fi

STAMP=$(date +%Y%m%d-%H%M%S)
msg "Backing up the current root to @.rollback-$STAMP ..." "Резервую поточний корінь у @.rollback-$STAMP ..."
mv "$TOP/@" "$TOP/@.rollback-$STAMP"
msg "Creating a new root from snapshot $N ..." "Створюю новий корінь зі знімка $N ..."
btrfs subvolume snapshot "$TOP/@snapshots/$N/snapshot" "$TOP/@" >/dev/null

# snap-pac takes its PRE snapshot while pacman still holds /var/lib/pacman/db.lck,
# so a snapshot can carry that stale lock. Drop it in the new root, otherwise the
# rolled-back system reports "unable to lock database". Also clear the resolved
# pacman sync caches' lock if present.
rm -f "$TOP/@/var/lib/pacman/db.lck" 2>/dev/null

# Repoint the btrfs default at the new @ so a default-subvolume boot follows the
# rollback too (a subvol=@ pin already mounts @ by name).
NEWID=$(btrfs subvolume show "$TOP/@" | sed -n 's/.*Subvolume ID:[[:space:]]*//p')
if [ -n "$NEWID" ]; then btrfs subvolume set-default "$NEWID" "$TOP"; fi

# Ask the restored root's first boot to reconcile /boot with the snapshot
# (kernel image, initramfs, boot menu, rescue pair) - consumed by the
# artix-rollback-fixup one-shot service.
mkdir -p "$TOP/@/var/lib/artix-rollback"
: > "$TOP/@/var/lib/artix-rollback/fixup-pending"

echo
msg "Done. Reboot to boot snapshot $N." "Готово. Перезавантаж, щоб працювати на знімку $N."
msg "The old root is kept as @.rollback-$STAMP (delete later with btrfs subvolume delete)." "Старий корінь збережено як @.rollback-$STAMP (видали згодом через btrfs subvolume delete)."
echo
if [ "$UK" = 1 ]; then printf 'Перезавантажити зараз? [y/N] '; else printf 'Reboot now? [y/N] '; fi
read -r ans
case "$ans" in y|Y) reboot ;; esac
