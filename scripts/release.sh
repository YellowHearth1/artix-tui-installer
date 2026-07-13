#!/bin/sh
# Publish a GitHub release: the installer binary + the live ISO.
#
# The trick that keeps the README's curl line frozen forever:
#
#     https://github.com/<user>/<repo>/releases/latest/download/artix-installer
#
# `latest` is not a tag — it's an alias GitHub resolves to the most recent
# published release. So every run here can create a FRESH, date-stamped tag
# (which makes the release date honest, unlike re-uploading assets onto one
# eternal tag, where the header keeps saying "5 days ago"), while that download
# URL never changes. Best of both.
#
# Asset names must stay stable for that URL to work, so the ISO is uploaded
# under a fixed name (artix-tui-dinit-x86_64.iso) even though the file on disk
# carries a build date. The date lives in the tag and the notes instead.
#
# Usage:  sh scripts/release.sh              # build date = today
#         sh scripts/release.sh 20260713     # or pin the ISO's build date
#
# Requires: gh (pacman -S github-cli), authenticated once via `gh auth login`.
set -eu

# Paths default to the maintainer's layout but can be overridden from the
# environment, so the script isn't wedded to one machine:
#
#   REPO_DIR=/srv/artix ISO_DIR=/mnt/build sh scripts/release.sh
#
REPO_DIR="${REPO_DIR:-$HOME/artix-tui-installer}"
ISO_DIR="${ISO_DIR:-$HOME/artools-workspace/iso/tui}"
BIN="${BIN:-$REPO_DIR/installer/target/release/artix-installer}"
ISO_ASSET_NAME="artix-tui-dinit-x86_64.iso"
GH_REPO="${GH_REPO:-YellowHearth1/artix-tui-installer}"

command -v gh >/dev/null 2>&1 || {
    echo "!! gh not found — install it:  sudo pacman -S github-cli" >&2
    echo "   then authenticate once:     gh auth login" >&2
    exit 1
}

# ── locate the ISO ───────────────────────────────────────────────────────────
# Either the date was passed in, or we take the newest ISO in the workspace —
# whichever build actually happened last, rather than assuming it was today's.
if [ $# -ge 1 ]; then
    ISO="$ISO_DIR/artix-tui-dinit-$1-x86_64.iso"
    [ -f "$ISO" ] || { echo "!! no such ISO: $ISO" >&2; exit 1; }
else
    # Sort by the DATE IN THE NAME, not by mtime: touching or copying an older
    # image would otherwise make it look like the newest build and get shipped.
    # The name's date is what the image actually is; the file's timestamp isn't.
    ISO=$(ls -1 "$ISO_DIR"/artix-tui-dinit-*-x86_64.iso 2>/dev/null | sort -r | head -n 1 || true)
    [ -n "$ISO" ] || { echo "!! no ISO found in $ISO_DIR" >&2; exit 1; }
fi

# Build date comes from the ISO's own filename — that's the date the image was
# actually built, which is what a user downloading it cares about.
ISO_DATE=$(basename "$ISO" | sed -n 's/^artix-tui-dinit-\([0-9]\{8\}\)-x86_64\.iso$/\1/p')
[ -n "$ISO_DATE" ] || { echo "!! can't read a build date out of: $(basename "$ISO")" >&2; exit 1; }
PRETTY_DATE=$(printf '%s-%s-%s' \
    "$(echo "$ISO_DATE" | cut -c1-4)" \
    "$(echo "$ISO_DATE" | cut -c5-6)" \
    "$(echo "$ISO_DATE" | cut -c7-8)")

[ -f "$BIN" ] || {
    echo "!! binary not found: $BIN" >&2
    echo "   build it first:  cd $REPO_DIR/installer && cargo build --release" >&2
    exit 1
}

# Warn if the ISO predates the binary: it then does NOT contain this build, and
# shipping them together would quietly hand users a stale installer.
if [ "$ISO" -ot "$BIN" ]; then
    echo "~~ WARNING: the ISO is older than the binary."
    echo "   The image probably does not include this installer build."
    echo "   Rebuild the ISO, or continue only if you know it's fine."
    printf "   Continue anyway? [y/N] "
    read -r answer
    case "$answer" in [Yy]*) ;; *) echo "aborted."; exit 1;; esac
fi

VERSION=$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$REPO_DIR/installer/Cargo.toml" | head -n 1)
TAG="build-$ISO_DATE"
ISO_SIZE=$(du -h "$ISO" | cut -f1)
BIN_SIZE=$(du -h "$BIN" | cut -f1)

echo ">>> ISO:     $(basename "$ISO")  ($ISO_SIZE)"
echo ">>> Binary:  $BIN  ($BIN_SIZE)"
echo ">>> Tag:     $TAG   (installer v$VERSION)"
echo ""

# Re-running for the same build date replaces that release rather than failing.
if gh release view "$TAG" --repo "$GH_REPO" >/dev/null 2>&1; then
    echo ">>> A release for $TAG already exists — replacing it."
    gh release delete "$TAG" --repo "$GH_REPO" --yes --cleanup-tag
fi

# The ISO is uploaded under a FIXED name (see the note at the top) so that the
# latest/download URL stays valid; gh's `local#name` syntax renames on upload.
gh release create "$TAG" \
    --repo "$GH_REPO" \
    --title "artix-installer $PRETTY_DATE" \
    --notes "$(cat <<EOF
Готовий бінарний файл інсталятора та live-ISO зі вшитим інсталятором.
_Prebuilt installer binary and a live ISO with the installer baked in._

| Українська | English | |
|---|---|---|
| **Дата збірки** | _Build date_ | $PRETTY_DATE |
| **Версія інсталятора** | _Installer version_ | v$VERSION |
| **ISO** | _ISO_ | \`$ISO_ASSET_NAME\` — $ISO_SIZE |
| **Бінарник** | _Binary_ | \`artix-installer\` — $BIN_SIZE, x86_64 |

---

### ⬇️ Завантажити / Download

**ISO-образ** — записати на флешку й завантажитись.
_**ISO image** — write it to a USB stick and boot._

\`\`\`sh
curl -LO https://github.com/$GH_REPO/releases/latest/download/$ISO_ASSET_NAME
\`\`\`

**Лише інсталятор** — якщо система вже завантажена (напр. з офіційного Artix-ISO).
_**Installer only** — if you're already booted (e.g. from the official Artix ISO)._

\`\`\`sh
curl -LO https://github.com/$GH_REPO/releases/latest/download/artix-installer
chmod +x artix-installer
sudo ./artix-installer
\`\`\`

Обидва посилання **завжди** ведуть на найсвіжішу збірку — README міняти не треба.
_Both links **always** resolve to the newest build — no README edits needed._

---

### 📦 Що всередині / What's inside

| Українська | English |
|---|---|
| Artix Linux на **dinit** — без systemd | Artix Linux on **dinit** — systemd-free |
| Двомовний TUI-інсталятор (укр / англ) | Bilingual TUI installer (Ukrainian / English) |
| **LUKS**-шифрування, ключ на USB | **LUKS** encryption, USB key file |
| **btrfs** зі знімками та відкатом | **btrfs** with snapshots and rollback |
| Вибір ядра, DE та Wayland-композиторів | Kernel, DE and Wayland compositor choice |
| **Chaotic-AUR**, оптимізація дзеркал | **Chaotic-AUR**, mirror optimization |
| **EFISTUB** / GRUB / rEFInd / Limine | **EFISTUB** / GRUB / rEFInd / Limine |
EOF
)" \
    "$BIN#artix-installer" \
    "$ISO#$ISO_ASSET_NAME"

echo ""
echo ">>> Done. The README's curl lines keep working unchanged:"
echo "    .../releases/latest/download/artix-installer"
echo "    .../releases/latest/download/$ISO_ASSET_NAME"
