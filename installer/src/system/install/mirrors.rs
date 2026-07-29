//! Pacman mirrorlist hygiene and optimization.
//!
//! `MIRROR_OPTIMIZE_SCRIPT` (v3) does two independent jobs, in this order:
//!
//! 1. **Purge** — excluded mirrors are deleted from the list outright, before
//!    anything is probed, ranked, backed up or written back. This half runs
//!    unconditionally, needs no network, and is deliberately NOT tied to the
//!    "optimize mirrors" option: the stock lists ship an excluded mirror
//!    *active*, so gating the purge behind a checkbox left it live for every
//!    later pacman call — including in the installed system.
//! 2. **Rank** — health-check every remaining mirror (12 in parallel, 6 s cap
//!    each), rewrite the reachable ones fastest-first and comment out the dead
//!    or crawling ones. `--no-rank` skips this half; the purge still happens.
//!
//! v1 ranked only the nearest countries and trusted the rest — a mirror that
//! degraded mid-install then killed the whole transaction ("Operation too slow
//! ... failed to commit") at 95%. The old timezone→countries table is gone
//! with it: a full probe of the real population beats geographic guessing.
//!
//! The same script serves three lists via a mode flag: Artix (default — run on
//! the live system before basestrap, and again in the chroot afterwards, since
//! installing `artix-mirrorlist` can drop a fresh stock list into the target),
//! `--arch` and `--chaotic` (in the chroot, where those lists exist). Ranking
//! is best-effort: on failure the list is left as it is after the purge.

pub(crate) const MIRROR_OPTIMIZE_SCRIPT: &str = r###"#!/bin/sh
# Artix installer — mirrorlist purge + optimizer v3.
#
# PURGE (always, no network needed): excluded mirrors are deleted from the
# list before anything else happens. Not optional, and not tied to the
# "optimize mirrors" checkbox - the stock Artix list ships an excluded mirror
# ACTIVE, so a user who declined ranking used to keep it.
#
# RANK (skipped by --no-rank): v1 speed-ranked only the ~5 nearest countries'
# mirrors and left the rest active-but-untested below. A mirror that was fine
# at ranking time could degrade to a crawl mid-install; pacman's low-speed
# cutoff then killed the whole transaction at 95% ("Operation too slow ...
# failed to commit"), and the user had to restart from scratch. v2+ probes
# EVERY remaining mirror right before packages are installed: alive ones are
# written fastest-first, dead or crawling ones are commented out with a
# reason. One bad server can no longer take the install down.
#
# Modes:  (default)  Artix   /etc/pacman.d/mirrorlist          probe: system.db
#         --arch     Arch    /etc/pacman.d/mirrorlist-arch     probe: core.db
#         --chaotic  Chaotic /etc/pacman.d/chaotic-mirrorlist  probe: chaotic-aur.db
#         --no-rank  purge only, skip the health check entirely
# Ranking is best-effort: any failure leaves the purged list in place, exit 0.
set -u
log() { echo ">>> $*"; }

MODE=artix
RANK=1
for arg in "$@"; do
  case "$arg" in
    --arch)    MODE=arch;;
    --chaotic) MODE=chaotic;;
    --no-rank) RANK=0;;
  esac
done

# Excluded mirrors never reach the candidate list. Two independent nets, because
# a mirrorlist states a server's origin in two different ways:
#
#   1. SECTION HEADERS. Arch groups servers under "## Country"; Artix uses
#      "# Country" beneath a "## Continent". Everything under an excluded
#      header is dropped until the next header. This is the net that matters:
#      10 of the 27 servers in the Arch list's excluded section sit on
#      hostnames that give nothing away (.gay, .lol, .me, .su ...), and a
#      hostname-only filter promoted them into the ACTIVE list, because the
#      candidate scan deliberately harvests commented-out servers too.
#   2. HOSTNAMES. Lists with no country sections (Chaotic) get nothing from
#      net 1, so the host itself is matched: the .ru and .su ccTLDs, the "ru."
#      and "ruN-mirror." host prefixes, and a few named hosts.
#
# "#Server =" lines are candidates, never headers - the header rule must not
# match them, or an excluded section would end at its first server line.
strip_excluded() {
  awk '
    function is_server(l) {
      return l ~ /^[[:space:]]*#*[[:space:]]*[Ss]erver[[:space:]]*=/
    }
    /^[[:space:]]*#/ && !is_server($0) {
      skip = (tolower($0) ~ /russia|russian federation/)
      if (skip) next
      print; next
    }
    skip                                          { next }
    tolower($0) ~ /russia/                        { next }
    /\/\/[^\/]*\.(ru|su)([\/:]|$)/                { next }
    /\/\/ru\./                                    { next }
    /\/\/ru-?[0-9]*-?mirror\./                    { next }
    /\/\/(archlinux\.gay|mirror\.murmellow\.lol)/ { next }
    { print }
  '
}

# Candidates, each tagged with the country it is listed under: "country<TAB>url".
#
# The country is the LAST comment line before a run of Server lines. That rule
# is what the real files support, and it took reading them to find out: the
# in-code description of the formats was wrong. Artix writes "##Denmark" with no
# space, puts "##Europe" above as a continent and occasionally uses
# "# Czech Republic"; Arch writes "##Germany" and "#Worldwide". A preamble line
# never sits directly above a Server, so the licence blurb cannot be mistaken
# for a country.
#
# Keeping the country is the whole point of the rebuilt list: a mirror is only
# useful near you, and someone who moves needs to comment out one block and
# uncomment another. A list sorted purely by speed, with the countries thrown
# away, cannot be edited that way by hand.
collect_with_country() {
  awk -v TAB="$(printf '\t')" '
    function is_server(l) {
      return l ~ /^[[:space:]]*#*[[:space:]]*[Ss]erver[[:space:]]*=/
    }
    /^[[:space:]]*#/ && !is_server($0) {
      skip = (tolower($0) ~ /russia|russian federation/)
      if (!skip) {
        h = $0
        sub(/^[[:space:]]*#+[[:space:]]*/, "", h)
        sub(/[[:space:]]+$/, "", h)
        if (h != "") country = h
      }
      next
    }
    skip                                          { next }
    tolower($0) ~ /russia/                        { next }
    /\/\/[^\/]*\.(ru|su)([\/:]|$)/                { next }
    /\/\/ru\./                                    { next }
    /\/\/ru-?[0-9]*-?mirror\./                    { next }
    /\/\/(archlinux\.gay|mirror\.murmellow\.lol)/ { next }
    is_server($0) {
      srv = $0
      sub(/^[#[:space:]]*[Ss]erver[[:space:]]*=[[:space:]]*/, "", srv)
      sub(/[[:space:]].*$/, "", srv)
      if (srv != "") printf "%s%s%s\n", (country == "" ? "Unknown" : country), TAB, srv
    }
  '
}

# Count Server lines (active or commented) - used to report what the purge took.
count_servers() {
  sed -n 's/^[#[:space:]]*[Ss]erver[[:space:]]*=.*/x/p' "$1" | wc -l
}

# Rewrite $1 with every excluded mirror gone. Runs before the backup is taken,
# so not even the .bak-installer copy carries one.
purge() {
  file="$1"; label="$2"
  before=$(count_servers "$file")
  if strip_excluded < "$file" > "$file.purge-tmp" 2>/dev/null \
     && [ -s "$file.purge-tmp" ]; then
    after=$(count_servers "$file.purge-tmp")
    mv -f "$file.purge-tmp" "$file"
    removed=$((before - after))
    [ "$removed" -gt 0 ] && log "[$label] purged $removed excluded mirror(s)."
    return 0
  fi
  # awk missing or failed. A line-level grep loses section awareness, so say so
  # loudly - but never leave the raw list in place.
  rm -f "$file.purge-tmp"
  log "[$label] WARNING: awk filter failed, falling back to a coarse filter."
  grep -viE '(russia|//[^/]*\.(ru|su)([/:]|$)|//ru\.|//ru-?[0-9]*-?mirror\.)' \
    "$file" > "$file.purge-tmp" 2>/dev/null && [ -s "$file.purge-tmp" ] \
    && mv -f "$file.purge-tmp" "$file"
  rm -f "$file.purge-tmp"
  return 0
}

TAB=$(printf '\t')

optimize() {
  file="$1"; label="$2"; repo="$3"; db="$4"
  cp -f "$file" "$file.bak-installer" 2>/dev/null || true

  # Candidate set: every Server line, active or commented - the stock lists
  # ship the whole mirror population commented out, which is exactly what we
  # want to test. Deduped. The file is already purged; filtering again here is
  # belt-and-braces, so a future caller of optimize() can't skip the purge.
  collect_with_country < "$file" | sort -u -t"$TAB" -k2,2 > /tmp/mo_cand
  total=$(wc -l < /tmp/mo_cand)
  if [ "$total" -eq 0 ]; then
    log "[$label] no candidate mirrors found in $file - skipping."
    return 0
  fi
  log "================================================="
  log " Repository mirror optimization: $label"
  log "================================================="
  log "[$label] probing all $total mirrors (12 in parallel, 6s cap each)..."

  # Build probe jobs: substitute $repo/$arch in the server template and point
  # at the repo database - a small file every healthy mirror must serve.
  : > /tmp/mo_jobs
  while IFS="$TAB" read -r country srv; do
    base=$(printf '%s' "$srv" | sed "s|\$repo|$repo|g; s|\$arch|x86_64|g; s|/*$||")
    printf '%s\t%s\t%s\n' "$base/$db" "$country" "$srv" >> /tmp/mo_jobs
  done < /tmp/mo_cand

  : > /tmp/mo_ok
  : > /tmp/mo_dead
  n=0
  while IFS="$TAB" read -r probe country srv; do
    (
      t=$(curl -fsS --max-time 6 -o /dev/null -w '%{time_total}' "$probe" 2>/dev/null) \
        && printf '%s\t%s\t%s\n' "$t" "$country" "$srv" >> /tmp/mo_ok \
        || printf '%s\t%s\n' "$country" "$srv" >> /tmp/mo_dead
    ) &
    n=$((n+1))
    if [ $((n % 12)) -eq 0 ]; then
      wait
      alive_now=$(wc -l < /tmp/mo_ok)
      log "[$label] $n/$total probed, $alive_now alive so far..."
    fi
  done < /tmp/mo_jobs
  wait

  ok=$(wc -l < /tmp/mo_ok)
  dead=$(wc -l < /tmp/mo_dead)
  if [ "$ok" -eq 0 ]; then
    log "[$label] every mirror failed the probe - network trouble? Keeping the original list."
    return 0
  fi

  sort -n /tmp/mo_ok > /tmp/mo_sorted
  # Grouped by COUNTRY, countries ordered by their fastest mirror, and fastest
  # first inside each country. Speed still decides the order — but the blocks
  # survive it, so moving country is "comment out one heading, uncomment
  # another" instead of hand-sorting a flat list of URLs by hostname guesswork.
  {
    echo "# $label mirrorlist - rebuilt by the Artix installer (full health check)."
    echo "# $ok reachable mirrors, fastest first within each country;"
    echo "# $dead unreachable or too slow, commented out at the bottom."
    echo "# Countries are ordered by their fastest mirror."
    echo "#"
    echo "# Moved to another country? Comment out the block you are leaving and"
    echo "# uncomment the one you arrived in - that is all this file needs."
    echo "# Original saved next to this file as *.bak-installer."
    echo ""
    awk -F"$TAB" '
      { if (!(($2) in seen)) { order[++n] = $2; seen[$2] = 1 }
        line[$2] = line[$2] sprintf("Server = %s\n", $3) }
      END { for (i = 1; i <= n; i++) printf "## %s\n%s\n", order[i], line[order[i]] }
    ' /tmp/mo_sorted
    if [ "$dead" -gt 0 ]; then
      echo "# ---------------------------------------------------------------"
      echo "# Failed the pre-install health check (unreachable or slower than 6s)."
      echo "# Kept, not deleted: a mirror that is down today may be the closest"
      echo "# one tomorrow, or after a move. Uncomment to use."
      echo ""
      sort -t"$TAB" -k1,1 /tmp/mo_dead | awk -F"$TAB" '
        { if ($1 != cur) { if (cur != "") printf "\n"; printf "## %s\n", $1; cur = $1 }
          printf "#Server = %s\n", $2 }
      '
    fi
  } > "$file"

  fastest=$(head -n 1 /tmp/mo_sorted | awk -F"$TAB" '{printf "%s, %s (%ss)", $3, $2, $1}')
  log "[$label] done: $ok active, $dead disabled. Fastest: $fastest"
}

# Purge always; rank only when asked and only when curl is around to do it.
run() {
  file="$1"; label="$2"; repo="$3"; db="$4"
  if [ ! -f "$file" ]; then
    log "[$label] $file not found - skipping."
    return 0
  fi
  purge "$file" "$label"
  [ "$RANK" -eq 1 ] || return 0
  command -v curl >/dev/null 2>&1 || {
    log "[$label] curl not found - skipping the health check (list is purged)."
    return 0
  }
  optimize "$file" "$label" "$repo" "$db"
}

case "$MODE" in
  chaotic) run /etc/pacman.d/chaotic-mirrorlist "Chaotic-AUR" chaotic-aur chaotic-aur.db;;
  arch)    run /etc/pacman.d/mirrorlist-arch    "Arch"       core        core.db;;
  *)       run /etc/pacman.d/mirrorlist         "Artix"      system      system.db;;
esac
log "Mirror check complete."
"###;
