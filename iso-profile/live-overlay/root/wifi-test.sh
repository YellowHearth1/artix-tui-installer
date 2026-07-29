#!/bin/sh
# ─────────────────────────────────────────────────────────────────────────────
# wifi-test.sh — set up a FAKE Wi-Fi network inside a VM, so the installer's
# Wi-Fi screen can be tested without any real wireless hardware.
#
# Usage (inside the live ISO, as root):
#     sh wifi-test.sh
#
# What it does:
#   1. loads mac80211_hwsim → two simulated radios appear (wlan0, wlan1)
#   2. hands wlan0 to hostapd as an access point broadcasting SSID "ArtixTest"
#   3. leaves wlan1 for NetworkManager → that's what the installer will see
#
# Then, in the installer: pick "configure Wi-Fi" → adapter wlan1 → network
# ArtixTest → password: testtest123
# ─────────────────────────────────────────────────────────────────────────────
set -e

SSID="ArtixTest"
PASS="testtest123"

say() { printf '\n\033[1;36m==>\033[0m %s\n' "$1"; }
die() { printf '\n\033[1;31m!! %s\033[0m\n' "$1"; exit 1; }

# Needs root (modprobe, hostapd, nmcli device set). If launched as a normal
# user, re-exec through sudo rather than just failing — the copy in the live
# user's home is meant to be double-clickable-simple.
if [ "$(id -u)" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1; then
        say "Needs root — re-running through sudo…"
        exec sudo sh "$0" "$@"
    fi
    die "Run as root:  sudo sh $0"
fi

# ── 1. simulated radios ──────────────────────────────────────────────────────
say "Loading mac80211_hwsim (2 virtual radios)…"
modprobe mac80211_hwsim radios=2 2>/dev/null || die "mac80211_hwsim not available in this kernel"
sleep 1

# The kernel names them wlan0/wlan1 by default, but don't assume — ask.
radios=$(iw dev 2>/dev/null | awk '/Interface/ {print $2}')
ap=$(printf '%s\n' "$radios" | sed -n 1p)
sta=$(printf '%s\n' "$radios" | sed -n 2p)
[ -n "$ap" ] && [ -n "$sta" ] || die "Expected 2 radios, got: $radios"
printf '    AP radio:      %s\n    Client radio:  %s\n' "$ap" "$sta"

# ── 2. keep NetworkManager off the AP radio ──────────────────────────────────
# hostapd owns it; if NM also grabs it, they fight and the AP never comes up.
say "Telling NetworkManager to ignore $ap (hostapd owns it)…"
nmcli device set "$ap" managed no 2>/dev/null || true
rfkill unblock wifi 2>/dev/null || true

# ── 3. bring up the access point ─────────────────────────────────────────────
command -v hostapd >/dev/null 2>&1 || die "hostapd is not installed on this ISO — add it to iso-profile/Packages-Live"

say "Starting access point \"$SSID\" on $ap…"
cat > /tmp/hostapd.conf <<EOF
interface=$ap
driver=nl80211
ssid=$SSID
hw_mode=g
channel=6
wpa=2
wpa_passphrase=$PASS
wpa_key_mgmt=WPA-PSK
rsn_pairwise=CCMP
EOF

pkill hostapd 2>/dev/null || true
hostapd -B /tmp/hostapd.conf >/tmp/hostapd.log 2>&1 || die "hostapd failed — see /tmp/hostapd.log"
sleep 2

# ── 3b. hand out IP addresses ────────────────────────────────────────────────
# hostapd only does the 802.11 half: radio, encryption, authentication. Once a
# client associates it asks for an IP over DHCP — and with nobody answering,
# NetworkManager waits out the DHCP timeout and then fails the connection with
# "IP configuration could not be completed". So the AP needs an address itself
# and a DHCP server behind it. dnsmasq is the smallest thing that does both.
say "Giving $ap an address and starting DHCP…"
ip addr flush dev "$ap" 2>/dev/null || true
ip addr add 10.42.0.1/24 dev "$ap" 2>/dev/null || true
ip link set "$ap" up 2>/dev/null || true

if command -v dnsmasq >/dev/null 2>&1; then
    pkill -f "dnsmasq.*$ap" 2>/dev/null || true
    dnsmasq \
        --interface="$ap" \
        --bind-interfaces \
        --dhcp-range=10.42.0.10,10.42.0.100,12h \
        --dhcp-option=3,10.42.0.1 \
        --dhcp-option=6,10.42.0.1 \
        --pid-file=/tmp/dnsmasq-wifitest.pid \
        >/tmp/dnsmasq.log 2>&1 \
        || die "dnsmasq failed — see /tmp/dnsmasq.log"
    sleep 1
else
    printf '\n\033[1;33m~~ dnsmasq is not installed — the client will associate but get no IP,\n'
    printf '   and NetworkManager will fail with "IP configuration could not be\n'
    printf '   completed". Add dnsmasq to the ISO to test the full flow.\033[0m\n'
fi

# ── 4. report ────────────────────────────────────────────────────────────────
if nmcli -t -f SSID dev wifi list --rescan yes 2>/dev/null | grep -q "^$SSID$"; then
    say "SUCCESS — NetworkManager can see the network."
else
    printf '\n\033[1;33m~~ AP is up, but NM does not list it yet. Give it a few seconds,\n'
    printf '   or press Enter/r on the installer'\''s network screen to rescan.\033[0m\n'
fi

cat <<EOF

  ┌───────────────────────────────────────────────┐
  │  Now go to the installer's Wi-Fi screen:      │
  │                                               │
  │     adapter   →  $sta$(printf '%*s' $((28 - ${#sta})) '')│
  │     network   →  $SSID$(printf '%*s' $((28 - ${#SSID})) '')│
  │     password  →  $PASS$(printf '%*s' $((28 - ${#PASS})) '')│
  └───────────────────────────────────────────────┘

  Worth testing while you're there:
    • a WRONG password  → must stay on the screen with a red error
    • Enter on an empty list → must rescan, never sit silent

EOF
