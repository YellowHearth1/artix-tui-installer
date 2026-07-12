# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A bilingual (English/Ukrainian) Rust TUI installer (ratatui) for a custom Artix
Linux spin running the `dinit` init system. It's a 15-step wizard (`installer/`)
plus a live-ISO profile (`iso-profile/`) built with artools `buildiso`.

**Read `ARCHITECTURE.en.md` first** — it has the data-flow diagram, file map,
and "making a typical change" recipes (add a package, add a translation key,
add a screen, add a bootloader, change Wi-Fi behavior). `ARCHITECTURE.md` is
the Ukrainian original; keep both in sync when editing. `README.en.md` /
`README.md` document every user-facing feature and the full wizard flow — check
them before changing installer behavior, and update them when behavior changes.

## Commands

```sh
cd installer && cargo build --release   # rustc >= 1.90; binary at target/release/artix-installer
rustfmt --edition 2021                  # before committing
dash -n scripts/*.sh                    # shell scripts are POSIX sh — verify
```

Translation parity check (every i18n key must exist in both `uk.toml` and `en.toml`):

```sh
cd installer && python3 - <<'EOF'
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

There is no automated test suite. Verification is manual:
- Read-only screens (timezone, keyboard, Wi-Fi, package search, disk listing)
  run fine outside a target and can be checked by just running the binary.
- Actual install steps (partitioning, basestrap, chroot) need root and a real
  target — test in a VM (QEMU + OVMF for UEFI).
- Wi-Fi without hardware: boot the ISO and pick the third mode-screen entry
  ("Wi-Fi test"). It loads `mac80211_hwsim`, runs hostapd broadcasting
  `ArtixTest` / `testtest123` on one virtual radio plus dnsmasq for DHCP, and
  leaves the other radio for the installer's Wi-Fi screen. The script is
  EMBEDDED in the binary (`scripts.rs::WIFI_TEST_SCRIPT`) precisely so it does
  not depend on the ISO overlay. Also test a wrong password (must stay on the
  screen with an error) and Enter on an empty scan list (must rescan, never sit
  silent).

## Architecture (see ARCHITECTURE.en.md for the full map)

```
main.rs (terminal, event loop)
  -> event.rs (key routing; any_modal_open() so Esc closes a modal, not the screen)
    -> screens/*.rs (one file per wizard step: draw() renders, handle_key() reacts)
      -> app.rs (App = all state; InstallConfig = the user's choices; defaults)
        -> system/install/ (build_plan: choices -> ordered Action steps)
          -> system/runner.rs (executes steps over a PTY, streams the log to the install screen)
```

Key files and where a change belongs:
- `src/app.rs` — app state, `InstallConfig`, the `Screen` enum. A new user
  choice starts here.
- `src/event.rs` — global key routing. A new modal must register its
  open/close flag here or Esc will exit the screen instead of closing it.
- `src/i18n.rs` + `i18n/*.toml` — all user-facing text; every key must exist in
  both `uk.toml` and `en.toml` (see parity check above). Accessed via
  `t(app.lang, "section.key")`.
- `src/system/install/mod.rs` — `build_plan`, the heart of the installer: 40+
  numbered install steps from user choices to an ordered action list.
- `src/system/install/helpers.rs` — Action constructors (`act`, `chroot`,
  `write_target_file`, ...), LUKS/rootflags handling.
- `src/system/install/scripts.rs` — every embedded script/service/dotfile/asset
  text lives here, and only here. Shell inside `format!` uses `@@PLACEHOLDER@@`
  + `.replace()`, never `{{` (avoids clashing with Rust's format braces).
- `src/system/install/packages.rs` — DE/GPU/kernel -> package list mapping.
- `src/system/install/mirrors.rs` — mirror ranking + timezone-to-countries table.
- `src/system/disk.rs` — `lsblk` parsing, partition planning.
- `src/system/runner.rs` — plan execution, PTY log streaming, `capture()`.
- `src/rollback.rs` — btrfs rollback (snapshot picker), independent of the
  installer's own runtime state (it also runs from the initramfs hook).
- `src/screens/wifi.rs` — `nmcli`-based Wi-Fi; NetworkManager start fallback,
  retry logic, background connect with a timeout.
- `src/assets/` — embedded configs (kitty, waybar, wofi, fastfetch); the
  Pinnacle desktop config ships as `pinnacle.tar.gz` (unpack, edit, repack with
  `tar --sort=name --owner=0 --group=0`, paths without `./`).
- `iso-profile/` — the artools `buildiso` profile. NOTE: the authoritative file
  is the user's **`profile.yaml`** (live-session services, rootfs packages),
  NOT `Packages-Live` or `live-overlay/`. Files dropped into `live-overlay/`
  did NOT reach a rebuilt ISO in practice. To enable a live-ISO service, add it
  to `live-session.services:` in `profile.yaml`; to ship a tool, prefer
  embedding it in the binary over relying on the overlay.

## Conventions

- Comments explain *why*, not *what*; a big decision gets a comment block
  above the code.
- Commits: one topic per commit; the message states the user-visible effect.
- No systemd assumptions anywhere — this is a dinit-native installer
  (turnstile for seatd, userspawn for elogind). Don't introduce systemd-unit
  patterns; use dinit service files and, where periodic execution is needed,
  cronie (dinit has no systemd timers).

## Hard-won rules (each of these cost a broken build)

- **Absolute paths, not relative.** Use `crate::system::runner::…`, never
  `super::runner::…`. `super` changes meaning when a file moves a level deeper
  — this broke the build right after the `install.rs` split (E0433).
- **Verify every package exists in Artix repos** (`pacman -Ss <name>`) before
  adding it to a package list. Names differ from Arch, and one missing package
  aborts the whole transaction.
- **Never bump Cargo dependencies** unless explicitly asked.
- **Enter must never be a silent no-op** — anywhere, not just Wi-Fi. If a list
  is empty, Enter retries the step and a status line explains what happened.
  A dead key with no feedback was a real, long-standing user-reported bug.
- **`goto_next()` honours `can_advance`.** Screens set `can_advance = false`
  every frame to make Enter mean "act", not "next". If you advance from such a
  screen after an async success, flip the gate yourself — otherwise
  `goto_next()` is a silent no-op and the user is stranded on the screen.
- **Blocking calls freeze the TUI.** Anything that can hang (`nmcli connect`,
  package downloads) runs in a background thread with a crossbeam channel,
  polled from `tick()`, with a timeout. Handle ALL three channel states —
  `Ok` / `Empty` / `Disconnected`. Ignoring `Disconnected` leaves the UI in a
  silent limbo (this happened).
- **No Russian mirrors, anywhere** — `strip_ru` in `mirrors.rs` excludes them by
  country, TLD, and known hostnames. This is a deliberate project stance.
