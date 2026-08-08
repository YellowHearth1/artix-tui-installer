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
# It is uploaded through a HARD LINK with that name, not with gh's `file#name`
# syntax: `#name` sets the asset's display LABEL, while the download URL keeps
# using the real filename. So every release so far advertised
# .../latest/download/artix-tui-dinit-x86_64.iso and served a 404, because the
# file on the release was still called artix-tui-dinit-YYYYMMDD-x86_64.iso.
# A hard link costs no disk space and gives the file the name the URL needs.
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
# Where the checkout is, worked out from where THIS SCRIPT is — not from a
# hard-coded ~/artix-tui-installer. Anyone cloning the project somewhere else
# (and anyone at all who is not the author) had every path in here point at a
# directory they do not have.
SELF_DIR=$(cd "$(dirname "$0")" && pwd -P)
REPO_DIR="${REPO_DIR:-$(dirname "$SELF_DIR")}"
# Where to find the image. `scripts/build-iso.sh` writes into the checkout's own
# iso/ folder; older images may still be in the artools workspace, so that is
# tried second rather than failing on a build that predates the move.
if [ -n "${ISO_DIR:-}" ]; then
    :
elif ls "$REPO_DIR"/iso/artix-tui-dinit-*-x86_64.iso >/dev/null 2>&1; then
    ISO_DIR="$REPO_DIR/iso"
else
    ISO_DIR="$HOME/artools-workspace/iso/tui"
fi
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
# `[ -ot ]` is a bashism — undefined in POSIX sh (SC3013), so under dash this
# guard would silently misbehave. `find -newer` asks the same question in
# POSIX: "is BIN newer than ISO?" prints a line exactly when the ISO is stale.
if [ -n "$(find "$BIN" -newer "$ISO" 2>/dev/null)" ]; then
    echo "~~ WARNING: the ISO is older than the binary."
    echo "   The image probably does not include this installer build."
    echo "   Rebuild the ISO, or continue only if you know it's fine."
    printf "   Continue anyway? [y/N] "
    read -r answer
    case "$answer" in [Yy]*) ;; *) echo "aborted."; exit 1;; esac
fi

VERSION=$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$REPO_DIR/installer/Cargo.toml" | head -n 1)

# Version scheme, deliberately modest: every release adds one to the LAST digit
# (0.0.1, 0.0.2, ...). 1.0.0 is not a milestone this script may reach on its own
# — it is a statement that the installer is ready, and only the author gets to
# make it. This guard exists because a version number is a promise to someone
# about to format their disk, and an accidental bump would make that promise
# without anyone deciding to.
case "$VERSION" in
    0.*) ;;
    *)
        if [ "${ALLOW_STABLE:-0}" != "1" ]; then
            echo "!! Version is $VERSION, not 0.x." >&2
            echo "   1.0.0 means 'this is ready', and that is a decision, not a" >&2
            echo "   side effect. If you really mean it:  ALLOW_STABLE=1 sh \$0" >&2
            exit 1
        fi
        echo "~~ Publishing $VERSION as a stable release (ALLOW_STABLE=1)."
        ;;
esac

# One version per RELEASE DAY, not per run of this script. Fixing an ISO and
# re-uploading it an hour later is the same release, not the next one — so the
# version only moves when the previous release happened on an EARLIER day.
# Re-running today replaces today's release and keeps its number.
#
# Deliberately bumped BEFORE publishing, not after: bumping afterwards meant the
# second run of a day picked up the already-incremented number and published it,
# which is exactly the behaviour this avoids.
# But FIRST: a version that has never been published is not due a bump at all.
#
# The day rule alone was wrong the moment anybody set the version by hand. Having
# written 0.0.2 into Cargo.toml and a real 0.0.2 entry in the CHANGELOG, this
# script would see "the last release was an earlier day", bump to 0.0.3, insert a
# stub entry above the real one, and publish the stub — burying the notes
# somebody had just written under a heading that says "(describe this release)".
#
# So the question is not "when was the last release" but "has THIS version been
# released yet".
ALREADY_PUBLISHED=0
gh release view "v$VERSION" --repo "$GH_REPO" >/dev/null 2>&1 && ALREADY_PUBLISHED=1

LAST_PUB=""
if [ "$ALREADY_PUBLISHED" -eq 1 ]; then
    LAST_PUB=$(gh release list --repo "$GH_REPO" --limit 1 --json publishedAt \
        -q '.[0].publishedAt' 2>/dev/null || true)
else
    echo ">>> v$VERSION has never been published — releasing it as it stands."
fi
if [ -n "$LAST_PUB" ]; then
    # Compare in LOCAL days — that is the day the person doing the release is
    # living in. publishedAt is UTC; date -d converts it.
    LAST_DAY=$(date -d "$LAST_PUB" +%Y-%m-%d 2>/dev/null || echo "${LAST_PUB%%T*}")
    TODAY=$(date +%Y-%m-%d)
    if [ "$LAST_DAY" != "$TODAY" ]; then
        NEXT="$(echo "$VERSION" | cut -d. -f1).$(echo "$VERSION" | cut -d. -f2).$(( $(echo "$VERSION" | cut -d. -f3) + 1 ))"
        echo ">>> Last release was $LAST_DAY; today is $TODAY — bumping $VERSION -> $NEXT."
        sed -i "s/^version = \"$VERSION\"/version = \"$NEXT\"/" "$REPO_DIR/installer/Cargo.toml"
        awk -v v="$NEXT" -v d="$TODAY" '
            !done && /^## \[/ {
                print "## [" v "] — " d; print ""
                print "### Додано"; print "- (опишіть зміни цього релізу)"; print ""
                done = 1
            }
            { print }
        ' "$REPO_DIR/CHANGELOG.md" > "$REPO_DIR/CHANGELOG.md.new" \
            && mv "$REPO_DIR/CHANGELOG.md.new" "$REPO_DIR/CHANGELOG.md"
        VERSION="$NEXT"
    else
        echo ">>> Already released today ($TODAY) — keeping v$VERSION and replacing it."
    fi
fi

TAG="v$VERSION"
ISO_SIZE=$(du -h "$ISO" | cut -f1)
BIN_SIZE=$(du -h "$BIN" | cut -f1)

# The release page says WHAT was built; the changelog says what CHANGED. Link
# them, or the only description a downloader gets is a table of file sizes.
#
# The link points at the tag this release creates, not at main: a release is a
# snapshot, and its notes must keep describing THAT build after main moves on.
#
# The anchor reproduces GitHub's own scheme for the "## [244] — date" heading:
# punctuation (the brackets and the em dash) is deleted, then EVERY remaining
# space becomes a hyphen. Spaces are deliberately not collapsed first — dropping
# the em dash leaves two spaces behind, and GitHub turns them into two hyphens:
# the real anchor is "244--2026-07-25". Collapsing them yielded one hyphen and a
# link that silently landed at the top of the file. Verified against the
# rendered page rather than assumed.
CHANGELOG_ANCHOR=$(grep -m1 "^## \[$VERSION\]" "$REPO_DIR/CHANGELOG.md" 2>/dev/null |
    sed 's/^## //; s/[][—.]//g; s/ /-/g' |
    tr '[:upper:]' '[:lower:]')
CHANGELOG_URL="https://github.com/$GH_REPO/blob/$TAG/CHANGELOG.md"
[ -n "$CHANGELOG_ANCHOR" ] && CHANGELOG_URL="$CHANGELOG_URL#$CHANGELOG_ANCHOR"

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
# Stage the ISO under the fixed asset name. A hard link when possible (same
# filesystem, no second copy of 1.2 GB); a real copy only as a fallback.
# Staged NEXT TO the ISO, not in /tmp: a hard link only works within one
# filesystem, and /tmp here is tmpfs — the fallback copy would put 1.2 GB into
# RAM. Beside the image the link is free and the fallback lands on real disk.
STAGE=$(mktemp -d "$(dirname "$ISO")/.release-XXXXXX")
trap 'rm -rf "$STAGE"' EXIT INT TERM
ISO_STAGED="$STAGE/$ISO_ASSET_NAME"
ln "$ISO" "$ISO_STAGED" 2>/dev/null || cp "$ISO" "$ISO_STAGED"

gh release create "$TAG" \
    --repo "$GH_REPO" \
    --title "artix-installer v$VERSION ($PRETTY_DATE)" \
    --notes "$(cat <<EOF
Готовий бінарний файл інсталятора та live-ISO зі вшитим інсталятором.
_Prebuilt installer binary and a live ISO with the installer baked in._

| Українська | English | |
|---|---|---|
| **Дата збірки** | _Build date_ | $PRETTY_DATE |
| **Версія інсталятора** | _Installer version_ | v$VERSION |
| **ISO** | _ISO_ | \`$ISO_ASSET_NAME\` — $ISO_SIZE |
| **Бінарник** | _Binary_ | \`artix-installer\` — $BIN_SIZE, x86_64 |

📝 **Що змінилося у v$VERSION** — [журнал змін]($CHANGELOG_URL)
_**What changed in v$VERSION** — [changelog]($CHANGELOG_URL)_

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
| Тримовний TUI-інсталятор (укр / англ / ісп) | Trilingual TUI installer (Ukrainian / English / Spanish) |
| **LUKS**-шифрування, ключ на USB | **LUKS** encryption, USB key file |
| **btrfs** зі знімками та відкатом | **btrfs** with snapshots and rollback |
| Вибір ядра, DE та Wayland-композиторів | Kernel, DE and Wayland compositor choice |
| **Chaotic-AUR**, оптимізація дзеркал | **Chaotic-AUR**, mirror optimization |
| **EFISTUB** / GRUB / rEFInd / Limine | **EFISTUB** / GRUB / rEFInd / Limine |
EOF
)" \
    "$BIN" \
    "$ISO_STAGED"

echo ">>> Done. The README's curl lines keep working unchanged:"
echo "    .../releases/latest/download/artix-installer"
echo "    .../releases/latest/download/$ISO_ASSET_NAME"
