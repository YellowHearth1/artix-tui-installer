# Skipping the GRUB welcome

The stock artools profile ships a GRUB/efiboot menu with a multi-second
timeout (the "welcome" screen). To boot straight to the installer:

1. Set `timeout=0` and `timeout_style=hidden` in the profile's grub config
   (this directory's `loopback.cfg` is a drop-in example — adapt the kernel
   `root=` / label to match your profile's output).
2. Keep a single default `menuentry` so there is nothing to choose.
3. For the EFI path, mirror the same in the profile's `efiboot` grub config.

After this, power-on → kernel → dinit → `installer` service on tty1, with no
menu and no login prompt.
