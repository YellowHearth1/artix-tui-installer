# `vendor/artools/` — the ISO build tooling, carried in the repo

These files are **not ours**. They are the parts of Artix's `artools` needed to
build a live image, carried here so that building this project does not depend
on the `artools` package being installed — or on the host being Artix at all.

    artools-iso   0.39.1-1   buildiso, lib/iso/*.sh
    artools-base  0.39.1-1   basestrap, artix-chroot, fstabgen, lib/base/*.sh,
                             pacman.conf.d/, makepkg.conf.d/, setarch-aliases.d/
    iso-profiles  2025.09-2  iso-profiles/common/common.yaml

Upstream: https://gitea.artixlinux.org/artix/artools
Licence: **GPL-3.0-or-later** (`SPDX-License-Identifier` in each file); the full
text is in `LICENSE.GPL-3.0`. The installer itself is Apache-2.0 — these files
keep their own licence, and modifying them puts the GPL on those modifications.

## Copied from the maintainer's Artix machine — and verified

These come off a real Artix install, which is the point: they are then certainly
Artix's tools and not Arch's. Two repositories are mixed on such a machine
(`system`/`world`/`galaxy` are Artix, `extra` is Arch), and picking by name alone
is how the wrong distro's package ends up in the build.

Copying a machine has its own trap, so the copy is CHECKED rather than trusted:

    pacman -Qkk artools-base artools-iso     # must report nothing

That check earns its place. An earlier attempt copied a machine whose
`lib/iso/services.sh` had been edited by hand (`ln -s` → `ln -sf`), with a stale
`.bak` beside it — a private patch that would have shipped as if it were
upstream. `-Qkk` catches exactly that: it compares every file against the
package's own checksums.

Pinning to a version instead does NOT work here, and that was measured: the
installed `glibc` was `2.44+r3+g0b05bc142249-1` while the mirror already carried
`+r5+g7cba77790f32`. A list of URLs built from what is installed 404s within
days. The files themselves do not go stale.

Checked for anything personal before committing — usernames, e-mail addresses,
GPG key IDs, hostnames, tokens. There is none: `makepkg.conf.d` ships
`#PACKAGER="John Doe <john@doe.com>"` commented out, and that is the whole of it.

## The two changes we DO make

Both are in `etc/artools-iso.conf`, which is a config file and meant to be
edited. The tools themselves are byte-identical to the packages.

1. `WORKSPACE_DIR="${WORKSPACE_DIR:-...}"` — upstream assigns it outright, so
   exporting the variable had no effect: the container set one workspace and
   buildiso looked in another, then reported the profile as missing.
2. `INITSYS="dinit"` — upstream's default is openrc, and that default produced
   `artix-tui-openrc-*.iso`: an image running the wrong init system, from a
   project whose entire reason for existing is the other one. It showed up only
   in the FILENAME, which is exactly the kind of thing that ships.

## How they are used

`buildiso` finds everything through three environment variables, so nothing had
to be patched to make it run from here:

    LIBDIR=vendor/artools/lib
    DATADIR=vendor/artools
    SYSCONFDIR=vendor/artools/etc

`scripts/build-iso.sh` sets those and prefers this copy, falling back to a system
`buildiso` if one exists and this directory does not.

## What is still needed on the host

Carrying artools removes the artools dependency, not every dependency. The build
still calls out to `pacman`, `mksquashfs`, `xorriso`, `mkfs.fat`, `grub-mkimage`,
`mkinitcpio` and `yq` — and `pacman` in particular is not something a Debian or
Fedora machine has.

**That is what the container is for:**

    sh scripts/build-iso.sh --container

It runs the whole build inside an Artix image, so the host needs only Docker or
Podman — no Artix, no artools, not even a Rust toolchain. And because the
container uses THIS copy too, a host build and a container build run the same
tools rather than two versions that might quietly disagree.

## Updating

When `artools` is upgraded on the maintainer's machine:

    pacman -Qkk artools-base artools-iso        # must be silent first
    cp /usr/bin/{buildiso,basestrap,artix-chroot,fstabgen}  vendor/artools/bin/
    cp -r /usr/share/artools/lib/{base,iso}                 vendor/artools/lib/
    cp -r /usr/share/artools/{pacman,makepkg}.conf.d \
          /usr/share/artools/setarch-aliases.d              vendor/artools/
    cp /usr/share/artools/iso-profiles/common/common.yaml \
          vendor/artools/iso-profiles/common/

Then re-apply the two config changes above (`pacman` replaces
`etc/artools-iso.conf` on upgrade) and **build once, checking that the ISO's
filename says `dinit`**. That one word is what caught the init-system default
last time.
