#
# ~/.bash_profile
#
[[ -f ~/.bashrc ]] && . ~/.bashrc

# Auto-launch the installer on tty1 (Cyrillic font is set inside the wrapper).
if [ "$(tty)" = "/dev/tty1" ]; then
    sudo /usr/bin/installer-start
fi
