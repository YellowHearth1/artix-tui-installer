# `iso-profile/` — build your own live ISO

Everything needed to build an Artix live image that boots straight into this
installer. The image is based on Artix **base** — a text console, no desktop, no
graphics drivers — which is the environment the installer is written for.

The whole thing is one command:

```sh
sh scripts/build-iso.sh
```

That compiles the installer, checks it, copies this profile out to the artools
workspace, and runs `buildiso`. Nothing else needs to be in place first.

## What you need

```sh
sudo pacman -S artools
```

`artools` provides `buildiso`. Its workspace defaults to
`~/artools-workspace/`; profiles live in `~/artools-workspace/iso-profiles/`
and finished images land in `~/artools-workspace/iso/`.

You do **not** need to create the profile there by hand — the build script
copies this directory out on every run. That direction is deliberate: this repo
is the source of truth, so the profile cannot quietly drift from the code that
depends on it.

## Doing it by hand

```sh
cd installer && cargo build --release && cd ..
mkdir -p ~/artools-workspace/iso-profiles/tui
cp -a iso-profile/profile.yaml iso-profile/live-overlay iso-profile/grub-overrides \
      ~/artools-workspace/iso-profiles/tui/
install -m755 installer/target/release/artix-installer \
      ~/artools-workspace/iso-profiles/tui/live-overlay/usr/bin/artix-tui-installer
sudo buildiso -p tui
```

**The binary goes in `live-overlay`, not `root-overlay`.** This profile builds a
live session (`profile.yaml` has a `live-session:` block), and `live-overlay` is
what reaches the running image. A copy into `root-overlay` fails silently: the
build succeeds and ships whatever binary was there before.

To rebuild, just run `buildiso` again — it cleans its work directory on every
run (`-c` disables that). There is nothing to reset by hand.

## What is in here

- **`profile.yaml`** — packages and live-session settings for `buildiso`.
  Autologin as `artix`, no display manager, no desktop.
- **`live-overlay/`** — files copied verbatim into the live filesystem:
  - `home/artix/.bash_profile` starts the installer on tty1. That is the whole
    autostart mechanism: no dinit service, no getty replacement.
  - `usr/bin/installer-start` sets the console font, then execs the installer.
  - `usr/share/kbd/consolefonts/` — the console fonts the installer offers that
    **no package provides**. See `usr/share/licenses/artix-tui-consolefonts/`
    for what each one is, where it came from and what was changed.
  - `usr/share/artix-installer/nftables.conf` — firewall rules the installer
    can write into the installed system.
  - `etc/ssh/sshd_config.d/10-live-root.conf` — root login over SSH, **live
    image only**, for diagnosing a machine that will not show its own screen.
- **`grub-overrides/loopback.cfg`** — boots straight into the installer when the
  ISO is chain-loaded from a GRUB menu.

## Adding a package to the image

Put it under `rootfs.packages` in `profile.yaml`. Check it exists in the live
Artix repositories first — `pacman -T <name>` answers that; `pacman -Qq` only
tells you what is installed here.

## Changing the console font

The default is set in `usr/bin/installer-start`. Anything you name there must
exist on the image: either from `kbd` / `terminus-font`, or carried in
`usr/share/kbd/consolefonts/`. `scripts/build-iso.sh` refuses to build if the
installer offers a font the image cannot load — a missing font is not an error
anywhere at runtime, it just leaves the console on the previous font, which is
very hard to attribute when you meet it.
