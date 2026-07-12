# 🧭 Architecture & Contributor Guide

> A map of the project for anyone who wants to fix or add something,
> even if Rust is new to you. Every common change is traced to its file.

## Data flow in 30 seconds

```
main.rs (terminal, event loop)
   └─ event.rs (key routing; any_modal_open — so Esc closes the modal, not the screen)
       └─ screens/*.rs (15 screens: draw() renders, handle_key() reacts)
           └─ app.rs (App — all state; InstallConfig — the user's choices; defaults)
               └─ system/install/ (build_plan: choices → ordered Action steps)
                   └─ system/runner.rs (executes steps, streams the log to screen 14)
```

## File map

| File | Responsibility | Typical change |
|---|---|---|
| `src/main.rs` | terminal setup, main loop | rarely touched |
| `src/app.rs` | app state, `InstallConfig`, defaults, the `Screen` enum | a new user choice → a new field here |
| `src/event.rs` | global key routing, `any_modal_open()` | new modal → register its flag here or Esc will exit the screen |
| `src/i18n.rs` + `i18n/*.toml` | all user-facing text, uk/en | **every** key goes into BOTH tomls (parity is checked) |
| `src/theme.rs` | colours/styles | — |
| `src/screens/*.rs` | one file per screen (language, disk, wifi, options, summary…) | that screen's behaviour |
| `src/screens/wifi.rs` | Wi-Fi: nmcli, NetworkManager start fallback, retry logic | rule: Enter is never a silent no-op |
| `src/system/install/mod.rs` | `build_plan` — the heart: 40+ numbered install steps | a new install step |
| `src/system/install/helpers.rs` | Action constructors (`act`, `chroot`, `write_target_file`…), LUKS/rootflags | — |
| `src/system/install/scripts.rs` | ALL embedded scripts/services/dotfiles/assets | editing a script's text happens here, and only here |
| `src/system/install/packages.rs` | DE/GPU/kernel → package lists | add a default package |
| `src/system/install/mirrors.rs` | mirror ranking + timezone→countries table | — |
| `src/system/disk.rs` | lsblk parsing, partition plan | — |
| `src/system/runner.rs` | plan execution, log streaming, `capture()` | — |
| `src/rollback.rs` | btrfs rollback (snapshot picker) | — |
| `src/assets/` | waybar/wofi/fastfetch configs, the Pinnacle config tarball | Pinnacle config: unpack `pinnacle.tar.gz`, edit, repack |
| `iso-profile/` | live-ISO profile for `buildiso` (packages, dinit services, overlay) | a live-ISO service → symlink in `live-overlay/etc/dinit.d/boot.d/` |

## Making a typical change

**Add a default package** → `src/system/install/packages.rs`, `base_packages`
(or a DE set inside it). Verify the package exists: `pacman -Ss name`.

**Add text/translation** → the same key in `i18n/uk.toml` AND `i18n/en.toml`;
use it via `t(app.lang, "section.key")`. Parity check below.

**Change an embedded script** (rollback, mirrors, the Secure Boot guide) →
`src/system/install/scripts.rs`. Scripts are POSIX sh: check with `dash -n`.

**Add a screen** → a new `src/screens/file.rs` (copy the simplest one as a
template), a variant in `enum Screen` (`app.rs`), branches in `event.rs` and
the draw router. Modals — don't forget `any_modal_open()`.

**Add a bootloader** → `ORDER` in `src/screens/options.rs`, a branch in
`match c.bootloader` in `install/mod.rs`, an i18n hint, README.

**Wi-Fi behaviour** → `src/screens/wifi.rs`; the daemon on the ISO is enabled
by the `iso-profile/live-overlay/etc/dinit.d/boot.d/NetworkManager` symlink.

## Building & checks

```sh
cd installer && cargo build --release        # rustc ≥ 1.90
# translation parity:
python3 - <<'EOF'
import tomllib
def f(d,p=""):
    s=set()
    for k,v in d.items():
        s|=f(v,p+k+".") if isinstance(v,dict) else {p+k}
    return s
a=f(tomllib.load(open("i18n/uk.toml","rb"))); b=f(tomllib.load(open("i18n/en.toml","rb")))
print("OK" if a==b else a^b)
EOF
```

**Testing the TUI without hardware:** the installer runs great in QEMU (UEFI
via OVMF).

**Wi-Fi in a VM with no adapter — one command.** In the live ISO (as root):

```sh
sh scripts/wifi-test.sh
```

It loads `mac80211_hwsim` (two virtual radios), runs hostapd on one broadcasting
**ArtixTest** / **testtest123**, and leaves the other for the installer. Then walk
the Wi-Fi screen normally. Worth testing a **wrong** password too (must stay on
the screen with an error) and Enter on an empty list (must rescan, never sit
silent).

Alternatively, boot the ISO from a USB stick on a laptop — the Wi-Fi screen is
~20 seconds in, no install needed.

## Style

- Comments explain *why*, not *what*; big decisions get a block above the code.
- `rustfmt --edition 2021` before committing.
- Shell inside `format!` uses `@@PLACEHOLDER@@` + `.replace()`, never `{{`.
- Commits: one topic per commit; the message states the user-visible effect.
