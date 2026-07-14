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
| `src/system/install/mod.rs` | `build_plan` — the core: reads like a table of contents (25 steps, one line each), with the detail in eight `plan_*` functions (see below) | a new install step |
| `src/system/install/helpers.rs` | Action constructors (`act`, `chroot`, `write_target_file`…), LUKS/rootflags | — |
| `src/system/install/scripts.rs` | ALL embedded scripts/services/dotfiles/assets | editing a script's text happens here, and only here |
| `src/system/install/packages.rs` | DE/GPU/kernel → package lists | add a default package |
| `src/system/install/mirrors.rs` | health-checks **every** mirror (Artix + Arch + Chaotic) before installing: live ones fastest-first, dead ones commented out | — |
| `src/system/disk.rs` | lsblk parsing, partition plan | — |
| `src/system/runner.rs` | plan execution, log streaming, `capture()` | — |
| `src/rollback.rs` | btrfs rollback (snapshot picker) | — |
| `src/assets/` | waybar/wofi/fastfetch configs; `pinnacle/` holds the compositor config as plain files | Pinnacle config: just edit the file under `assets/pinnacle/` |
| `iso-profile/` | live-ISO profile for `buildiso` (packages, dinit services, overlay); but the profile `buildiso` actually reads is a separate `profile.yaml` (outside this repo), not `Packages-Live`/`live-overlay/` here | a live-ISO service → add it to `live-session.services:` in `profile.yaml` |

## How `build_plan` is laid out

It's the biggest file, so it's worth knowing its shape. `build_plan` does not
*contain* the install logic — it *lists* it:

```rust
pub fn build_plan(app: &App) -> Vec<Action> {
    // 0)  host tooling
    // 1)  disk: partition, format, mount
    // 2)  basestrap: base + chosen packages
    // 3)  fstab (+ extra disks)         → plan_fstab()
    // ...
    // 9)  accounts                      → plan_accounts()
    // 9b) GTK bookmarks + D-Bus session → plan_session_env()
    // 9c) initramfs + LUKS keyfiles     → plan_initramfs_luks()
    // 10) bootloader                    → plan_bootloader()
    // 11) firewall                      → plan_firewall()
    // 12) dinit services + AUR          → plan_services()
}
```

Each `plan_*` is a **pure function**: it reads `InstallConfig`, appends steps to
the plan, and does nothing else. That's what makes an install testable without
touching a disk:

```rust
let t = plan_text(&build_plan(&app));
assert!(t.contains("groupadd -f log"));
```

**Step order matters** (fstab before the bootloader, accounts before services)
and the compiler won't check it — don't reorder the calls without a reason.

## Types, not strings

`InstallConfig` doesn't store the user's choices as strings. `boot_mode` is a
`BootMode`, not `"uefi"`; `bootloader` is a `Bootloader`; `gpu` is a
`Vec<GpuDriver>`, not CSV. This isn't style:

- add a variant to an `enum` and the compiler **forces** you to handle it in
  every `match` — including the picker on screen;
- knowledge about dependencies lives **on the type**, not scattered across
  files: `SeatProvider::user_launcher()` knows elogind pairs with `userspawn`
  and seatd with `turnstiled`.

Don't add new string fields where the set of values is finite.

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

**Wi-Fi behaviour** → `src/screens/wifi.rs`; the NetworkManager daemon on the
live ISO is enabled via the `live-session.services:` list in `profile.yaml`
(the file `buildiso` actually reads), NOT a symlink in this repo's
`iso-profile/live-overlay/` — files placed there don't reach the built ISO.

## Building & checks

Everything CI does runs locally too — and should, before you commit:

```sh
cd installer
cargo fmt --check                          # one style
cargo build --release                      # rustc >= 1.90
cargo clippy --release -- -D warnings      # what the compiler lets through
cargo test --release                       # regression tests (see below)
```

**The tests are bugs that already happened.** Every `#[test]` in
`system/install/mod.rs` pins down a failure that reached a real user: the key
stick that got formatted; the `log` group without which logs were unreadable;
`useradd` before `groupadd`; the AUR check that lied about `-git` packages. The
plan is pure data, so an install can be inspected without touching a disk:

```rust
let t = plan_text(&build_plan(&app));
assert!(t.contains("groupadd -f log"));
```

Found a bug? **Write the test first**, then fix it — otherwise the next
refactor brings it back.

**Three levels of tests, each catching something the others can't:**

| Where | What it checks | A bug it caught |
|---|---|---|
| `system/install/mod.rs` | the **install plan** — pure data, no disk needed | the key stick got formatted; `useradd` ran before `groupadd` |
| `screens/mod.rs` | **rendering**, via `TestBackend` (draws to memory) | a panic in `draw()`; a cursor past the end of a list; a raw i18n key on screen |
| `event.rs` | **keys** — `handle_global` called directly | `q` killed the installer from a password field |

`TestBackend` is ratatui's built-in backend that renders into an in-memory
buffer instead of a terminal, so any screen can be drawn in a unit test:

```rust
let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
term.draw(|f| draw(f, &mut app, f.area())).unwrap();
let text = term.backend().to_string();   // the whole screen as text
```

Why this matters here specifically: the installer runs on a **physical console
from a live ISO**. A panic in `draw()` isn't a stack trace in a log — it's a
dead machine mid-install, with no way back. So every screen is checked at three
sizes (80x24 being the promised minimum) and in both languages.

```sh
# translation parity (CI runs the same):
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
wifi-test
```

(It ships on the live ISO as `/usr/bin/wifi-test`. Outside the ISO, run
`sh scripts/wifi-test.sh` from the repo root.)

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
