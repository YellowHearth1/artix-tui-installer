//! Pacman mirrorlist optimization.
//!
//! `MIRROR_OPTIMIZE_SCRIPT` (v2) health-checks EVERY mirror in a list right
//! before packages are installed: alive ones are rewritten fastest-first,
//! dead or crawling ones are commented out. v1 ranked only the nearest
//! countries and trusted the rest — a mirror that degraded mid-install then
//! killed the whole transaction ("Operation too slow ... failed to commit")
//! at 95%. The old timezone→countries table is gone with it: a full probe of
//! the real population beats geographic guessing.
//!
//! The same script serves three lists via a mode flag: Artix (default, on the
//! live system before basestrap), `--arch` and `--chaotic` (inside the chroot,
//! where those lists exist). Best-effort by design: on any failure it leaves
//! the original list untouched and exits 0.

pub(crate) const MIRROR_OPTIMIZE_SCRIPT: &str = r###"#!/bin/sh
# Artix installer — mirror optimizer v2 (full-population health check).
#
# WHY v2: v1 speed-ranked only the ~5 nearest countries' mirrors and left the
# rest active-but-untested below. A mirror that was fine at ranking time could
# degrade to a crawl mid-install; pacman's low-speed cutoff then killed the
# whole transaction at 95% ("Operation too slow ... failed to commit"), and
# the user had to restart from scratch. v2 probes EVERY mirror in the list
# right before packages are installed: alive ones are written fastest-first,
# dead or crawling ones are commented out with a reason. One bad server can
# no longer take the install down — it simply isn't in the active list.
#
# Modes:  (default)  Artix   /etc/pacman.d/mirrorlist          probe: system.db
#         --arch     Arch    /etc/pacman.d/mirrorlist-arch     probe: core.db
#         --chaotic  Chaotic /etc/pacman.d/chaotic-mirrorlist  probe: chaotic-aur.db
# Best-effort: any failure leaves the original list in place and exits 0.
set -u
log() { echo ">>> $*"; }

MODE=artix
case "${1:-}" in
  --arch) MODE=arch;;
  --chaotic) MODE=chaotic;;
esac

command -v curl >/dev/null 2>&1 || {
  log "curl not found - skipping mirror optimization (lists left as-is)."
  exit 0
}

# Project stance: excluded mirrors are removed entirely - section headers,
# server lines, and the chaotic ru-mirror hostnames. Nothing tested, nothing
# kept, nothing mentioned.
strip_excluded() {
  awk '
    /[Rr]ussia/ { next }
    /[Ss]erver[[:space:]]*=.*\.ru\//                          { next }
    /[Ss]erver[[:space:]]*=.*\/\/ru-?[0-9]*-?mirror\.chaotic/ { next }
    { print }
  '
}

TAB=$(printf '\t')

optimize() {
  file="$1"; label="$2"; repo="$3"; db="$4"
  if [ ! -f "$file" ]; then
    log "[$label] $file not found - skipping."
    return 0
  fi
  cp -f "$file" "$file.bak-installer" 2>/dev/null || true

  # Candidate set: every Server line, active or commented - the stock lists
  # ship the whole mirror population commented out, which is exactly what we
  # want to test. Deduped; excluded mirrors dropped before any probing.
  strip_excluded < "$file" \
    | sed -n 's/^[#[:space:]]*Server[[:space:]]*=[[:space:]]*//p' \
    | sed 's/[[:space:]].*$//' \
    | sort -u > /tmp/mo_cand
  total=$(wc -l < /tmp/mo_cand)
  if [ "$total" -eq 0 ]; then
    log "[$label] no candidate mirrors found in $file - skipping."
    return 0
  fi
  log "[$label] probing all $total mirrors (12 in parallel, 6s cap each)..."

  # Build probe jobs: substitute $repo/$arch in the server template and point
  # at the repo database - a small file every healthy mirror must serve.
  : > /tmp/mo_jobs
  while IFS= read -r srv; do
    base=$(printf '%s' "$srv" | sed "s|\$repo|$repo|g; s|\$arch|x86_64|g; s|/*$||")
    printf '%s\t%s\n' "$base/$db" "$srv" >> /tmp/mo_jobs
  done < /tmp/mo_cand

  : > /tmp/mo_ok
  : > /tmp/mo_dead
  n=0
  while IFS="$TAB" read -r probe srv; do
    (
      t=$(curl -fsS --max-time 6 -o /dev/null -w '%{time_total}' "$probe" 2>/dev/null) \
        && printf '%s %s\n' "$t" "$srv" >> /tmp/mo_ok \
        || printf '%s\n' "$srv" >> /tmp/mo_dead
    ) &
    n=$((n+1))
    if [ $((n % 12)) -eq 0 ]; then wait; fi
  done < /tmp/mo_jobs
  wait

  ok=$(wc -l < /tmp/mo_ok)
  dead=$(wc -l < /tmp/mo_dead)
  if [ "$ok" -eq 0 ]; then
    log "[$label] every mirror failed the probe - network trouble? Keeping the original list."
    return 0
  fi

  sort -n /tmp/mo_ok > /tmp/mo_sorted
  {
    echo "# $label mirrorlist - rebuilt by the Artix installer (full health check)."
    echo "# $ok reachable mirrors, fastest first; $dead unreachable/too-slow disabled below."
    echo "# Original saved next to this file as *.bak-installer."
    echo ""
    while read -r t srv; do
      printf 'Server = %s\n' "$srv"
    done < /tmp/mo_sorted
    if [ "$dead" -gt 0 ]; then
      echo ""
      echo "# Failed the pre-install health check (unreachable or >6s):"
      while IFS= read -r srv; do
        printf '#Server = %s\n' "$srv"
      done < /tmp/mo_dead
    fi
  } > "$file"

  fastest=$(head -n 1 /tmp/mo_sorted | awk '{printf "%s (%ss)", $2, $1}')
  log "[$label] done: $ok active, $dead disabled. Fastest: $fastest"
}

case "$MODE" in
  chaotic) optimize /etc/pacman.d/chaotic-mirrorlist "Chaotic-AUR" chaotic-aur chaotic-aur.db;;
  arch)    optimize /etc/pacman.d/mirrorlist-arch    "Arch"       core        core.db;;
  *)       optimize /etc/pacman.d/mirrorlist         "Artix"      system      system.db;;
esac
log "Mirror check complete."
"###;
