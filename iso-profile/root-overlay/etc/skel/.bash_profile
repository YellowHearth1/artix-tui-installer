#
# ~/.bash_profile
#
[[ -f ~/.bashrc ]] && . ~/.bashrc

# Auto-launch the installer on tty1 (sets Cyrillic font inside the wrapper).
if [ "$(tty)" = "/dev/tty1" ]; then
    sudo /usr/bin/installer-start
fi
