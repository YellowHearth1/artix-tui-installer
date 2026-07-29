#!/bin/bash
# Managed by the Artix installer.
build() {
    add_binary /usr/local/bin/artix-rollback
    add_binary btrfs
    add_binary mount
    add_binary umount
    add_binary /usr/bin/sync
    add_runscript
}
help() {
    echo "Artix snapshot rollback picker for early boot (kernel param: artix.rollback)."
}
