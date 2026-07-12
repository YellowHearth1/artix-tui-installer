//! Pacman mirror optimization.
//!
//! `MIRROR_OPTIMIZE_SCRIPT` ranks the nearest countries' mirrors by real
//! speed and keeps EVERY other world mirror active below as fallback
//! (nearest-first, Russian mirrors excluded entirely — see strip_ru).
//! `mirror_region_countries` maps the chosen timezone to that country list;
//! only countries actually present in the Artix mirrorlist matter.


/// Build the entire install plan in order.
/// Maps the chosen timezone to mirror-list country NAMES (as they appear in the
/// `## Country` headers of the Artix/Arch mirrorlists), used to find and
/// uncomment the regional mirrors. The user's own country comes first, then
/// nearby neighbors. rankmirrors then sorts these by real speed, so the set
/// only needs to be "the right part of the world". Falls back to a continent
/// set, then a safe default. Names must match the generators' headers exactly.
/// Shell script that rewrites a pacman mirrorlist so the regional mirrors are
/// active and ranked by speed at the TOP, the full list is kept (commented)
/// below for easy relocation, and unwanted mirrors are filtered out. It's a script
/// (not inline) because the logic is substantial and reused for Artix and Arch.
/// Positional args ($@) are the regional country names to surface. Everything
/// is best-effort: missing rankmirrors, no network, or an unreachable generator
/// all fall back gracefully, never aborting the install.
pub(crate) const MIRROR_OPTIMIZE_SCRIPT: &str = r###"#!/bin/sh
set -u
log() { echo ">>> $*"; }

MODE=region
case "${1:-}" in
  --arch) MODE=arch; shift;;
  --chaotic) MODE=chaotic; shift;;
esac

HAVE_RANK=0
command -v rankmirrors >/dev/null 2>&1 && HAVE_RANK=1

# country names -> /tmp/mo_args (region/arch modes)
: > /tmp/mo_args
for a in "$@"; do printf '%s\n' "$a" >> /tmp/mo_args; done

# Silently filter out excluded mirrors: their section headers, their
# server lines, and the matching CDN country entries. Nothing about them
# is left anywhere in the generated list.
strip_ru() {
  awk '
    /[Rr]ussia/ { next }
    /[Ss]erver[[:space:]]*=.*\.ru\//                       { next }
    /[Ss]erver[[:space:]]*=.*\/\/ru-?[0-9]*-?mirror\.chaotic/ { next }
    { print }
  '
}

# Active block for Artix/Arch: per target country, a "## Country" header then
# its uncommented Server lines. Ranked-fastest first for speed, but — crucially
# — we DO NOT drop the rest: every remaining mirror of the country is appended
# below the ranked ones as fallbacks. If the top mirrors die mid-download (slow
# to a crawl, or time out), pacman simply walks down the list to the next one
# instead of aborting the whole transaction. Countries are already ordered
# nearest-first by the caller, so the overall list runs closest→farthest, with
# the always-up (if distant) main Artix mirror as the final safety net.
build_regional() {
  full="$1"; : > /tmp/mo_active
  while IFS= read -r country; do
    [ -n "$country" ] || continue
    servers=$(awk -v C="$country" '$0==("## " C){f=1;next} /^## /{f=0} f' "$full" \
              | sed -E 's/^#*[[:space:]]*Server/Server/' | grep '^Server' || true)
    [ -n "$servers" ] || continue
    # Rank only a bounded head of the candidates, so rankmirrors (which probes
    # each mirror over the network) stays fast even for countries with 100+
    # mirrors. The ranked ones go on top…
    head_set=$(printf '%s\n' "$servers" | head -n 10)
    ranked=""
    if [ "$HAVE_RANK" = 1 ]; then
      ranked=$(printf '%s\n' "$head_set" | rankmirrors - 2>/dev/null | grep '^Server' || true)
    fi
    [ -n "$ranked" ] || ranked="$head_set"
    # …and EVERY other mirror of this country is appended below as a fallback
    # (order as listed upstream), deduplicated against the ranked head. Nothing
    # is discarded — a distant-but-alive mirror is always better than a dead
    # "fast" one when the transaction would otherwise fail. POSIX sh: dedup via
    # temp files (no process substitution).
    printf '%s\n' "$ranked" > /tmp/mo_ranked
    rest=$(printf '%s\n' "$servers" | awk 'NR==FNR{seen[$0]=1;next} !seen[$0]' /tmp/mo_ranked -)
    {
      printf '## %s\n' "$country"
      printf '%s\n' "$ranked"
      [ -n "$rest" ] && printf '%s\n' "$rest"
      printf '\n'
    } >> /tmp/mo_active
  done < /tmp/mo_args
}

# Map a country NAME to its Chaotic-AUR 2-letter code (only those Chaotic
# actually hosts a country mirror for); empty when there is none.
chaotic_code() {
  case "$1" in
    Poland) echo pl;; Germany) echo de;; France) echo fr;; Italy) echo it;;
    Spain) echo es;; Sweden) echo se;; Greece) echo gr;; Switzerland) echo ch;;
    "United Kingdom") echo gb;; Netherlands) echo nl;; "United States") echo us;;
    Canada) echo ca;; Brazil) echo br;; Japan) echo jp;; "South Korea") echo kr;;
    Taiwan) echo tw;; Singapore) echo sg;; "Hong Kong") echo hk;; India) echo in;;
    Australia) echo au;; "New Zealand") echo nz;; Indonesia) echo id;;
    Israel) echo il;; Mexico) echo mx;; Chile) echo cl;; Colombia) echo co;;
    Peru) echo pe;; "Saudi Arabia") echo sa;; "South Africa") echo za;;
    Thailand) echo th;; "United Arab Emirates") echo ae;; Argentina) echo ar;;
    Vietnam) echo vn;; Nigeria) echo ng;; *) echo "";;
  esac
}

# Active block for chaotic: geo-mirror (auto-routes to closest) + cdn-mirror
# (reliable fallback), then the per-country virtual mirror for each nearby
# region Chaotic hosts (e.g. for Ukraine's neighbours -> pl, de). Each of those
# auto-routes within its country, making solid close fallbacks behind geo.
build_chaotic_active() {
  full="$1"; : > /tmp/mo_active
  geo=$(grep -E '^#?[[:space:]]*Server[[:space:]]*=.*geo-mirror\.chaotic' "$full" | head -1 | sed -E 's/^#*[[:space:]]*//')
  cdn=$(grep -E '^#?[[:space:]]*Server[[:space:]]*=.*cdn-mirror\.chaotic' "$full" | head -1 | sed -E 's/^#*[[:space:]]*//')
  { [ -n "$geo" ] && printf '## Geo mirror (auto-routes to the closest up-to-date mirror)\n%s\n\n' "$geo"
    [ -n "$cdn" ] && printf '## CDN mirror (reliable fallback)\n%s\n\n' "$cdn"; } >> /tmp/mo_active
  while IFS= read -r country; do
    [ -n "$country" ] || continue
    code=$(chaotic_code "$country")
    [ -n "$code" ] || continue
    line=$(grep -E "^#?[[:space:]]*Server[[:space:]]*=.*//${code}-mirror\.chaotic" "$full" | head -1 | sed -E 's/^#*[[:space:]]*//')
    [ -n "$line" ] && printf '## %s (closest country mirror)\n%s\n\n' "$country" "$line" >> /tmp/mo_active
  done < /tmp/mo_args
}

assemble() {
  ml="$1"; label="$2"; full="$3"; out=/tmp/mo_out
  # Collect the Server lines already placed in the active (ranked, regional)
  # block, so we don't list them twice below.
  grep '^Server' /tmp/mo_active 2>/dev/null | sed -E 's/^[[:space:]]*//' > /tmp/mo_active_srv || : > /tmp/mo_active_srv
  {
    echo "## $label mirrors"
    echo "## Nearest mirrors (ranked by real speed) are on top; ALL other"
    echo "## mirrors follow, active too, ordered as upstream lists them, so a"
    echo "## download never stalls for lack of a fallback. Russian mirrors are"
    echo "## excluded entirely."
    echo "##"
    echo ""
    if [ -s /tmp/mo_active ]; then cat /tmp/mo_active
    else echo "## (no regional match — the full list below is active)"; echo ""; fi
    echo "## ------------------------------------------------------------------"
    echo "## All remaining mirrors (active, farther away). Reorder freely."
    echo "## ------------------------------------------------------------------"
    # In the upstream mirrorlist EVERY Server line is COMMENTED (#Server) except
    # the few we activated regionally. This lower section must therefore UNcomment
    # each remaining mirror (strip the leading #), skipping any already ranked on
    # top, so the whole world stays active as fallback. A country header is
    # emitted only when it still has a kept mirror below it (no empty duplicates).
    # Handles both "#Server = …" and bare "Server = …" forms.
    awk '
      NR==FNR { active[$0]=1; next }
      /^##/ {
        pending=$0; have_pending=1; next
      }
      /^[[:space:]]*#?[[:space:]]*Server/ {
        line=$0
        sub(/^[[:space:]]*#?[[:space:]]*/, "", line)   # drop leading # and spaces
        if (!(line in active)) {
          if (have_pending) { print ""; print pending; have_pending=0 }
          print line                                    # emit ACTIVE (uncommented)
        }
        next
      }
      { if (have_pending) { print pending; have_pending=0 } print }
    ' /tmp/mo_active_srv "$full"
  } > "$out"
  mv "$out" "$ml"
}

process() {
  ml="$1"; gen="$2"; label="$3"; mode="$4"
  [ -e "$ml" ] || { log "$label: $ml absent, skipping."; return 0; }
  cp "$ml" "$ml.bak" 2>/dev/null || true
  full=/tmp/mo_full; : > "$full"
  if [ "$gen" != "-" ]; then
    curl -fsS --connect-timeout 10 --max-time 45 "$gen" 2>/dev/null > "$full" || : > "$full"
  fi
  [ -s "$full" ] || cp "$ml.bak" "$full" 2>/dev/null || : > "$full"
  [ -s "$full" ] || { log "$label: no mirror data, skipping."; return 0; }
  strip_ru < "$full" > "$full.x" && mv "$full.x" "$full"
  if [ "$mode" = chaotic ]; then build_chaotic_active "$full"; else build_regional "$full"; fi
  assemble "$ml" "$label" "$full"
  na=$(grep -c '^Server' "$ml" 2>/dev/null || echo 0)
  log "$label: $na active mirror(s) on top; full list below."
}

case "$MODE" in
  chaotic) process /etc/pacman.d/chaotic-mirrorlist "-" "Chaotic-AUR" chaotic;;
  arch)    process /etc/pacman.d/mirrorlist-arch "https://archlinux.org/mirrorlist/all/" "Arch" region;;
  *)       process /etc/pacman.d/mirrorlist "https://packages.artixlinux.org/mirrorlist/?country=all&protocol=https" "Artix" region;;
esac
log "Mirror optimization complete.""###;

pub(crate) fn mirror_region_countries(timezone: &str) -> &'static [&'static str] {
    match timezone {
        "Europe/Kyiv" | "Europe/Kiev" => &[
            "Ukraine",
            "Poland",
            "Germany",
            "Czechia",
            "Netherlands",
            "France",
            "Slovakia",
            "Hungary",
            "Romania",
        ],
        "Europe/Warsaw" => &[
            "Poland",
            "Germany",
            "Czechia",
            "Ukraine",
            "Slovakia",
            "Lithuania",
        ],
        "Europe/Berlin" | "Europe/Vienna" | "Europe/Zurich" => &[
            "Germany",
            "Netherlands",
            "Austria",
            "Czechia",
            "Poland",
            "France",
            "Switzerland",
        ],
        "Europe/Paris" | "Europe/Brussels" | "Europe/Amsterdam" => &[
            "France",
            "Netherlands",
            "Germany",
            "Belgium",
            "United Kingdom",
        ],
        "Europe/London" | "Europe/Dublin" => &[
            "United Kingdom",
            "Ireland",
            "Netherlands",
            "France",
            "Germany",
        ],
        "Europe/Moscow" => &["Finland", "Ukraine", "Germany", "Kazakhstan"],
        "Europe/Madrid" | "Europe/Lisbon" => &["Spain", "Portugal", "France", "Germany"],
        "Europe/Rome" => &["Italy", "France", "Germany", "Austria", "Switzerland"],
        "Europe/Stockholm" | "Europe/Helsinki" | "Europe/Oslo" | "Europe/Copenhagen" => {
            &["Sweden", "Finland", "Norway", "Denmark", "Germany"]
        }
        "America/New_York" | "America/Toronto" | "America/Chicago" => &["United States", "Canada"],
        "America/Los_Angeles" | "America/Vancouver" | "America/Denver" => {
            &["United States", "Canada"]
        }
        "America/Sao_Paulo" | "America/Argentina/Buenos_Aires" => {
            &["Brazil", "Chile", "United States"]
        }
        "Asia/Tokyo" => &["Japan", "South Korea", "Taiwan", "Singapore", "Hong Kong"],
        "Asia/Singapore" | "Asia/Kuala_Lumpur" | "Asia/Jakarta" => {
            &["Singapore", "Hong Kong", "Japan", "India"]
        }
        "Asia/Kolkata" | "Asia/Calcutta" => &["India", "Singapore", "Hong Kong"],
        "Asia/Shanghai" | "Asia/Hong_Kong" | "Asia/Taipei" => {
            &["Hong Kong", "Taiwan", "Singapore", "Japan"]
        }
        "Australia/Sydney" | "Australia/Melbourne" | "Australia/Perth" => {
            &["Australia", "New Zealand", "Singapore"]
        }
        _ => match timezone.split('/').next().unwrap_or("") {
            "Europe" => &[
                "Germany",
                "France",
                "Netherlands",
                "Poland",
                "United Kingdom",
                "Sweden",
                "Czechia",
                "Austria",
                "Finland",
            ],
            "America" => &["United States", "Canada", "Brazil"],
            "Asia" => &[
                "Japan",
                "Singapore",
                "India",
                "South Korea",
                "Hong Kong",
                "Taiwan",
            ],
            "Africa" => &["South Africa", "Germany", "France"],
            "Australia" | "Pacific" => &["Australia", "New Zealand", "Singapore"],
            "Indian" => &["India", "Singapore", "South Africa"],
            "Atlantic" => &["United Kingdom", "United States", "Germany"],
            _ => &["Germany", "United States", "Netherlands"],
        },
    }
}
