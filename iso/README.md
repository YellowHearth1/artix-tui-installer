# `iso/` — where built images land

`sh scripts/build-iso.sh` points `buildiso` at this folder, so a finished live
image appears here as:

```
iso/artix-tui-dinit-YYYYMMDD-x86_64.iso
```

**Nothing in here is committed.** A live image is well over a gigabyte and
GitHub refuses any file past 100 MB, so `.gitignore` keeps everything out except
this file. Images are published as **release assets** instead:

```sh
sh scripts/release.sh
```

which uploads the newest image here under a fixed name, so the download link in
the README never changes.

## Housekeeping

Nothing removes old images — that is deliberate, since the one that still boots
is worth more than the newest one. They add up fast, though:

```sh
ls -lh iso/*.iso          # what is here
rm iso/artix-tui-dinit-20260101-x86_64.iso
```

If this partition is tight, build somewhere else instead:

```sh
ISO_DIR=/mnt/big/iso sh scripts/build-iso.sh
```
