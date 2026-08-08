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
HOME_TZ=
for arg in "$@"; do
  case "$arg" in
    --arch)    MODE=arch;;
    --chaotic) MODE=chaotic;;
    --no-rank) RANK=0;;
    --home-tz=*) HOME_TZ=${arg#--home-tz=};;
  esac
done

# The user's own country, worked out from the timezone they already chose. The
# mapping is tzdata's own: zone.tab gives Europe/Kyiv -> UA, iso3166.tab gives
# UA -> "Ukraine", and "Ukraine" is exactly how the mirrorlists spell their
# section headers. No table of our own to go stale.
#
# It is used for ONE thing: putting your own country's block at the top. A
# country label says where a mirror is REGISTERED, not where its bytes come
# from — several of the biggest are CDNs that answer from a node near you — so
# the label alone cannot be trusted to mean "near". Measured speed still orders
# everything else.
HOME_COUNTRY=
if [ -n "$HOME_TZ" ] && [ -r /usr/share/zoneinfo/zone.tab ]; then
  cc=$(awk -v z="$HOME_TZ" '$1 !~ /^#/ && $3 == z { print $1; exit }' \
       /usr/share/zoneinfo/zone.tab 2>/dev/null)
  if [ -n "$cc" ] && [ -r /usr/share/zoneinfo/iso3166.tab ]; then
    HOME_COUNTRY=$(awk -F'\t' -v c="$cc" '$1 == c { print $2; exit }' \
                   /usr/share/zoneinfo/iso3166.tab 2>/dev/null)
  fi
  [ -n "$HOME_COUNTRY" ] && log "Home country from $HOME_TZ: $HOME_COUNTRY"
fi

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

# Bandwidth pass budget. Deliberately small: this runs before the install, on
# whatever connection the user has, and someone installing on old hardware over
# a thin link should not pay hundreds of megabytes to sort a list. 1 MB is
# already enough to tell 0.9 MiB/s from 3 MiB/s, and --max-time bounds the cost
# for the slow ones (a crawling mirror simply gets cut off and ranks last).
BW_PROBE_TOP=12
BW_PROBE_BYTES=1000000
BW_PROBE_SECS=5

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

  # SECOND PASS: measure BANDWIDTH, not handshake time.
  #
  # The pass above times a request for the repo database, which is small — so
  # what it really measures is how quickly a server says hello. That is not what
  # an install spends its time on. Proof from a real run: mirror.rabisu.com
  # answered FASTEST of every German mirror (0.135s) and then delivered
  # essentially nothing, while a mirror listed under the United States turned
  # out to be a CDN answering from a European node at 3.1 MiB/s. Ranking on the
  # first number puts the wrong server first in both directions.
  #
  # Measuring every mirror this way would cost hundreds of megabytes before the
  # install even starts, so only the head of the list is re-measured — the ones
  # that could actually win — plus every mirror in the user's own country, so a
  # good local server is never buried by a handshake that happened to be slow.
  head -n "$BW_PROBE_TOP" /tmp/mo_sorted > /tmp/mo_bwjobs
  if [ -n "$HOME_COUNTRY" ]; then
    awk -F"$TAB" -v c="$HOME_COUNTRY" '$2 == c' /tmp/mo_sorted >> /tmp/mo_bwjobs
    sort -u /tmp/mo_bwjobs -o /tmp/mo_bwjobs
  fi
  bw_n=$(wc -l < /tmp/mo_bwjobs)
  log "[$label] measuring real download speed on $bw_n mirror(s)..."
  : > /tmp/mo_bw
  n=0
  while IFS="$TAB" read -r t country srv; do
    base=$(printf '%s' "$srv" | sed "s|\$repo|$repo|g; s|\$arch|x86_64|g; s|/*$||")
    (
      # A ranged read of the same database: a server that ignores Range simply
      # sends more, and --max-time caps what that can cost. speed_download is
      # bytes per second either way.
      bps=$(curl -fsS --max-time "$BW_PROBE_SECS" -r "0-$BW_PROBE_BYTES" \
              -o /dev/null -w '%{speed_download}' "$base/$db" 2>/dev/null)
      case "$bps" in
        ''|0|0.*) : ;;   # measured nothing worth having: keep the latency figure
        *) printf '%s\t%s\t%s\n' "$bps" "$country" "$srv" >> /tmp/mo_bw ;;
      esac
    ) &
    n=$((n+1))
    [ $((n % 6)) -eq 0 ] && wait
  done < /tmp/mo_bwjobs
  wait

  # Merge into one ranking key, smaller = better, in two tiers: everything that
  # was speed-tested comes before everything that was not, because a measured
  # MiB/s is worth more than a guess from a handshake.
  awk -F"$TAB" -v OFS="$TAB" '
    FILENAME == ARGV[1] { bw[$3] = $1; next }
    { if ($3 in bw) print 0, 1000000000 / bw[$3], $2, $3
      else          print 1, $1,                  $2, $3 }
  ' /tmp/mo_bw /tmp/mo_sorted | sort -t"$TAB" -k1,1n -k2,2n > /tmp/mo_ranked
  cut -f2- /tmp/mo_ranked > /tmp/mo_sorted
  # Grouped by COUNTRY, countries ordered by their fastest mirror, and fastest
  # first inside each country. Speed still decides the order — but the blocks
  # survive it, so moving country is "comment out one heading, uncomment
  # another" instead of hand-sorting a flat list of URLs by hostname guesswork.
  {
    echo "# $label mirrorlist - rebuilt by the Artix installer (full health check)."
    echo "# $ok reachable mirrors, fastest first within each country;"
    echo "# $dead unreachable or too slow, commented out at the bottom."
    # Only claim the home country leads when it actually HAS a live mirror.
    # Artix has none in some countries, and a header announcing a block that is
    # not there (because every mirror in it failed) reads as a broken tool.
    if [ -n "$HOME_COUNTRY" ] \
       && awk -F"$TAB" -v c="$HOME_COUNTRY" '$2 == c { found = 1 }
                                             END { exit !found }' /tmp/mo_sorted; then
      echo "# $HOME_COUNTRY is first because it is yours; the rest follow by measured speed."
    elif [ -n "$HOME_COUNTRY" ]; then
      echo "# No reachable mirror in $HOME_COUNTRY, so these are ordered by measured speed."
    else
      echo "# Countries are ordered by their fastest mirror."
    fi
    echo "#"
    echo "# The head of the list was ranked by MEASURED DOWNLOAD SPEED, not by how"
    echo "# fast a server answers - those are different numbers, and the second one"
    echo "# happily puts a mirror that delivers nothing in first place."
    echo "#"
    echo "# A country heading is where a mirror is REGISTERED. Some of the largest"
    echo "# are CDNs and answer from a node near you, so one listed far away can"
    echo "# genuinely be your fastest - that is not a mistake in this list."
    echo "#"
    echo "# Moved to another country? Comment out the block you are leaving and"
    echo "# uncomment the one you arrived in - that is all this file needs."
    echo "# Original saved next to this file as *.bak-installer."
    echo ""
    awk -F"$TAB" -v home="$HOME_COUNTRY" '
      { if (!(($2) in seen)) { order[++n] = $2; seen[$2] = 1 }
        line[$2] = line[$2] sprintf("Server = %s\n", $3) }
      END {
        # Your own country leads, whatever it measured: it is the block you are
        # most likely to want, and the one you would otherwise go hunting for.
        if (home != "" && (home in seen)) printf "## %s\n%s\n", home, line[home]
        for (i = 1; i <= n; i++)
          if (order[i] != home) printf "## %s\n%s\n", order[i], line[order[i]]
      }
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

  # Report the fastest in the unit it was actually measured in: MiB/s for the
  # ones that got the bandwidth pass, seconds for the rest. Printing a merged
  # ranking key as if it were seconds would be a number that means nothing.
  fastest=$(head -n 1 /tmp/mo_ranked 2>/dev/null | awk -F"$TAB" '
    { if ($1 == 0) printf "%s, %s (%.1f MiB/s)", $4, $3, 1000000000 / $2 / 1048576
      else         printf "%s, %s (%ss)",        $4, $3, $2 }')
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
