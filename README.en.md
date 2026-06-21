<div align="center">

# 🐧 Artix TUI Installer

**A bilingual (English / Ukrainian) terminal installer for a custom
[Artix Linux](https://artixlinux.org) spin running the
[dinit](https://davmac.org/projects/dinit/) init system.**

Built in Rust with [ratatui](https://ratatui.rs) and styled to feel like a modern
graphical installer: a left step-rail, rounded panels, an Artix-blue accent,
segmented toggles, and a live scrollable install log.

[![Language: Rust](https://img.shields.io/badge/Rust-2021-orange?logo=rust)](https://www.rust-lang.org/)
[![TUI: ratatui](https://img.shields.io/badge/TUI-ratatui%200.30-blue)](https://ratatui.rs)
[![Init: dinit](https://img.shields.io/badge/init-dinit-green)](https://davmac.org/projects/dinit/)

🇺🇦 **[Українська версія → README.md](README.md)**

</div>

---

> ### 🤖 Code authorship
>
> **All of this project's code was written by [Claude](https://claude.ai), an AI
> model by [Anthropic](https://www.anthropic.com).** The architecture, iterative
> debugging, and implementation were generated entirely by Claude in
> conversation. The design vision, testing on real hardware and virtual machines,
> and the Artix/dinit-specific decisions belong to the project's author.

---

## ✨ Features

- **🌐 Bilingual UI** — English and Ukrainian, selectable on the first screen.
- **⚙️ dinit-native** — sets up a per-user dinit instance the dinit way, with no
  systemd assumptions anywhere:
  - **turnstile** for `seatd` (its own PAM module, no elogind needed);
  - **userspawn** for `elogind` (the stock Artix option);
  - the `seatd`/`elogind` seat manager and per-user PipeWire audio services.
- **📦 Interactive package install** — packages are installed through `pacman`
  running under a PTY, so *you* pick providers (GPU/Vulkan drivers, multimedia
  backends, …) instead of the first one being chosen silently. Same-package
  across repos auto-prefers Artix; `[Y/n]` prompts are auto-confirmed.
- **🔒 LUKS disk encryption** — root-only, **full-disk with an encrypted `/boot`**
  on UEFI, or a **USB key** (a keyfile means you enter the passphrase only once).
- **🥾 Bootloader choice** — GRUB, rEFInd or Limine; with GRUB, `os-prober` for
  detecting other operating systems. Configured before the extra-disk step.
- **🗄️ Additional disks** — mount other disks or partitions (under home, `/mnt`,
  or a custom path), optionally formatting or encrypting each. Encrypted extra
  disks unlock automatically at boot (key on the encrypted root, via a dinit
  service), regardless of how the root itself is unlocked.
- **💾 Filesystem choice** — ext4, btrfs, xfs, f2fs, jfs, ext3, ext2.
- **🖥️ Desktop choice** — KDE Plasma, LXQt, **Pinnacle** (an AwesomeWM-like
  Wayland compositor), XFCE, Cinnamon, MATE, LXDE, or none.
- **🎮 GPU drivers** — NVIDIA (open-dkms), NVIDIA 580xx (legacy), nouveau, AMD,
  Intel; nouveau is automatically blacklisted when a proprietary driver is chosen.
- **🛟 System recovery mode** — mounts an existing install (unlocking LUKS if
  needed), detects the bootloader, and opens a chroot shell to repair it.
- **🔁 AUR support** — `paru` is built from source (so it always matches the
  system's `libalpm`), then used to install the packages you selected.
- **🧩 Automatic dinit service enablement** — any installed `*-dinit` package has
  its service enabled automatically, whether from the repos or the AUR.
- **📜 System logging out of the box** — `syslog-ng` collects all logs to
  `/var/log`, and `logrotate` (via `cronie`) keeps **one week**, deletes older,
  and rotates immediately if a file exceeds **5 GB**. User services log to a
  buffer, so `dinitctl catlog` works for them right away.
- **🔥 Prebaked firewall** — an embedded nftables config opens the ports for KDE
  Connect, LocalSend, Sunshine, RustDesk, Steam Remote Play, Syncthing, and SSH.
- **🎨 Embedded configs** — kitty (Catppuccin Mocha), a starship prompt, fastfetch,
  plus waybar and wofi for Pinnacle; no external assets required.
- **🕹️ Gaming-ready** — raises the open-file limit (`nofile`) for Wine/Proton
  fsync, and optionally sets up `auto-cpufreq` automatically.
- **🧳 Self-contained** — the host tools it needs (artools, gptfdisk, cryptsetup,
  …) are installed automatically, so the installer works even from the **official
  Artix ISO**, not just its own image.
- **🏷️ Configurable** hostname and UEFI boot-entry label.
- 📀 Built as a live ISO with **artools** (`buildiso`).

---

## 👁️ Look

```
┌───────────┬──────────────────────────────────────────────┐
│  ◆  01    │  09 · Disk & partitions                       │
│  ●  02    │  ┌────────────────────────────────────────┐   │
│  ●  …     │  │  Mode       ● UEFI   ○ BIOS             │   │
│  ◆  09    │  │  Disk: /dev/sda  256G                   │   │
│  ○  10    │  │  Add SWAP?  [yes]  [ 4 GiB ]            │   │
│  ○  …     │  │  Filesystem  ‹ ext4 ›  btrfs  xfs       │   │
│           │  │              ◂ Back        Next ▸       │   │
│           │  │                                          │   │
│           │  └────────────────────────────────────────┘   │
│           ├──────────────────────────────────────────────┤
│           │  ↑/↓ move · ←/→ change · Enter next           │
└───────────┴──────────────────────────────────────────────┘
```

The left rail shows only step numbers (a small diamond spins on the active step);
the full step name is in the panel header.

---

## 🔨 Building

```sh
cd installer
cargo build --release
# → target/release/artix-installer
```

The read-only steps (timezone, keyboard, Wi-Fi, package search, disk listing)
degrade gracefully when their tools aren't available outside the target. The
install itself (partitioning, basestrap, chroot) needs root and a real target, so
**test it in a virtual machine**.

---

## 🧭 Wizard steps

The installer opens with a mode chooser: **Install** or **System recovery**.
Install runs 15 steps:

1. **Language** — Ukrainian / English; sets the UI language and the system locale.
2. **Timezone** — the full IANA list with a filter search.
3. **Wi-Fi** — skip (wired), scan, or connect via `nmcli`.
4. **Keyboard** — console layouts via `localectl`; the first checked is primary.
5. **Kernel** — linux / lts / zen / hardened.
6. **Desktop** — pick a desktop (or none) and the seat manager.
7. **Packages** — GPU driver + search and multi-select from the repos.
8. **AUR** — a curated recommended list and a live AUR search.
9. **Disk** — boot mode, target disk, SWAP, and root filesystem.
10. **Bootloader & encryption** — choose the bootloader (GRUB / rEFInd / Limine),
    other-OS detection (`os-prober`, GRUB only), the UEFI entry label, and disk
    encryption: root-only, full (encrypted `/boot`) or a USB key, with scope and
    passphrase. Comes **before** the extra disks so the key on an extra disk
    actually makes sense.
11. **Additional disks** — for each detected disk/partition: format (or keep the
    data), where to mount it (home / `/mnt` / a custom path with a folder name)
    and a separate encryption checkbox. Nothing changes until you choose.
12. **User** — hostname, account mode, username, and passwords (kept in memory
    only; never written to disk by the installer).
13. **Options** — passwordless sudo, the Chaotic-AUR repository, and mirror
    optimisation.
14. **Install** — a review, then a live log runs the plan step by step; it stops
    on error and lets you go **Back**.
15. **Finish** — a summary and reboot.

Navigation is the same everywhere: `↑`/`↓` moves focus (and Up on the topmost
item returns to the previous step), `←`/`→` changes a value, `Enter` advances,
`Esc` closes a popup or goes back.

---

## 🧱 How the install is organized

`src/system/install.rs` builds a single ordered list of actions; the install
screen runs each one, streaming output live. Roughly:

install host tools → partition → format (LUKS if asked) → mount → **phase 1**
`basestrap` a minimal bootable base (kernel, firmware, dinit + services, audio,
logging) → set up repos + keys → **phase 2** interactive `pacman` for the desktop,
drivers, and your extra packages → accounts → locale / timezone / keymap /
hostname + hosts → user-dinit wiring (turnstile or userspawn) → initramfs (with
the `encrypt` hook when encrypting) → bootloader → embedded nftables → log
rotation → enable all dinit services → **phase 3** AUR via `paru`.

---

## 📀 ISO profile (`iso-profile/`, for artools `buildiso`)

- `Packages-Root` / `Packages-Live` — packages for the live image (dinit only).
- `profile.conf` — autologin/display-manager settings for the live session.
- `live-overlay/usr/bin/installer-launch` — gives the TUI a real controlling
  terminal on tty1 (`setsid -c`), with a fallback shell on failure.
- `live-overlay/etc/dinit.d/installer.conf` — the autostart service that runs the
  installer instead of a getty on tty1.
- `grub-overrides/loopback.cfg` — boots straight into the installer.

Drop the compiled binary at `live-overlay/usr/bin/artix-installer`, then run
`sudo buildiso -p <profile>`.

---

## 🗂️ Project layout

```
installer/        Rust sources (ratatui TUI + install logic)
  src/app.rs      state model + config
  src/event.rs    global key handling / navigation
  src/main.rs     entry point + the "graphical installer" chrome
  src/screens/    one module per wizard step
  src/system/     disk, runner (PTY), install plan, packages, recovery
  src/assets/     embedded configs (kitty, fastfetch, waybar, wofi, pinnacle)
  i18n/           UI strings en.toml / uk.toml
iso-profile/      artools buildiso profile + live-image overlay
```

---

## 📄 License

Released under the **Apache 2.0** license — full text in [`LICENSE`](LICENSE).
