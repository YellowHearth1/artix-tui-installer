#!/bin/bash
cliphist list | wofi --dmenu --prompt Clipboard --width 600 --height 400 --sort-order=default | cliphist decode | wl-copy
