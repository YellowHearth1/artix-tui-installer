#!/bin/bash
# ~/.config/mango/launcher.sh

PINNED="Steam\nFirefox\nCaja\nKitty"

echo -e "$PINNED" | wofi --show dmenu \
  --prompt "Apps" \
  --style ~/.config/wofi/style.css
