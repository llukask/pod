#!/bin/sh
# Installs pod-desktop for the current user: the binary to ~/.local/bin, the
# launcher and icon to ~/.local/share. Pass a prefix to install elsewhere,
# e.g. `sudo ./install.sh /usr/local`.
set -eu
prefix=${1:-"$HOME/.local"}
here=$(dirname "$0")
install -Dm755 "$here/pod-desktop" "$prefix/bin/pod-desktop"
install -Dm644 "$here/pod.desktop" "$prefix/share/applications/pod.desktop"
install -Dm644 "$here/pod.svg" "$prefix/share/icons/hicolor/scalable/apps/pod.svg"
echo "Installed pod-desktop to $prefix/bin"
