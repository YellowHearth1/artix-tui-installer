# Build the live ISO on ANY Linux that can run a container.
#
# The point: nothing here is needed on the host. Not Artix, not `artools`, not
# even a Rust toolchain — the installer is compiled inside too. A machine with
# Docker or Podman and this checkout can produce the image, which is what
# "independent" has to mean for a project people are asked to build themselves.
#
# Run it through the script rather than by hand:
#
#     sh scripts/build-iso.sh --container   # whichever engine is installed
#     sh scripts/build-iso.sh --podman      # or name the one you want
#     sh scripts/build-iso.sh --docker
#
# Why it needs --privileged: `buildiso` builds a root filesystem, mounts an
# overlay over it, loop-mounts a FAT image for the EFI payload and runs
# `mksquashfs`. Those are kernel operations, not tricks a normal container is
# allowed to perform. The container is doing exactly what the same build does
# on the host — the isolation being given up is against this build, not against
# the host's other work.
# The registry is NAMED. Docker silently prepends docker.io to a bare image
# name; podman deliberately does not, and refuses with
#
#     short-name "artixlinux/artixlinux:latest" did not resolve to an alias
#     and no containers-registries.conf(5) was found
#
# which stops the build on its very first line. Qualifying it works identically
# under both engines, and says where the image actually comes from.
FROM docker.io/artixlinux/artixlinux:latest

# One transaction, and `-Syu` never bare `-Sy`: a partial upgrade here mixes a
# fresh package against an older base and breaks in ways that look like our bug.
# NOT `artools`: the tooling is carried in vendor/artools and used from there,
# so the container and a host build run the SAME buildiso. What is installed
# here is only what that tooling calls out to. basestrap, artix-chroot and
# fstabgen come from vendor/artools as well, so no package provides them.
RUN pacman -Syu --noconfirm --needed \
        rust cargo \
        git \
        squashfs-tools \
        dosfstools \
        libisoburn \
        grub \
        mkinitcpio \
        gptfdisk parted \
        go-yq \
        base-devel \
    && pacman -Scc --noconfirm

# artools reads its workspace location from here. Kept inside /build so the
# whole thing is one directory that can be thrown away.
ENV WORKSPACE_DIR=/build/workspace
RUN mkdir -p /build/workspace/iso-profiles

WORKDIR /repo
ENTRYPOINT ["/bin/sh", "/repo/scripts/container-build.sh"]
