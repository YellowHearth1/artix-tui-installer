# upstream/

Pristine copies of files this project ships a MODIFIED version of.

They are here to be diffed against, and for nothing else: nothing in this
directory reaches the image.

`kernels.cfg` is what `artix-grub-live` installs. Our copy lives in
`root-overlay/usr/share/grub/cfg/kernels.cfg` and differs from it in two ways —
three appended lines that skip the boot menu, and an `--id=artix-tui` plus a
rename on two menu entries.

That second kind of change is why the check that used to live in
`scripts/build-iso.sh` was wrong. It assumed our file was a verbatim copy with a
block appended, stripped the block, and compared the rest to the package — so
the entries we had deliberately edited made it warn "upstream changed" on every
single build, and the fix it suggested would have copied our own file over
itself. A warning that is always wrong is a warning nobody reads.

The answerable question is narrower: **has the package changed since we copied
it?** That is what this file is for. When the build says it has, diff this
against the new package, carry any real upstream change into our copy by hand,
and refresh this file.
