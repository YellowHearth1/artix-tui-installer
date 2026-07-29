#!/bin/sh
# Managed by the Artix installer. pinnacle.toml runs THIS instead of a bare
# `cargo run`: same effect, but the release profile is used and failures are
# logged. `cargo build` is incremental - after the installer's prebuild it
# returns in milliseconds, and it transparently rebuilds after you edit
# src/main.rs (then just reload the config from inside Pinnacle).
cd "$(dirname "$0")" || exit 1
if ! cargo build --release >run-config.log 2>&1; then
    command -v notify-send >/dev/null 2>&1 && \
        notify-send "Pinnacle config" "Build failed - see ~/.config/pinnacle/run-config.log"
    exit 1
fi
exec ./target/release/pinnacle-config
