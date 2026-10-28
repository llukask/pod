#!/usr/bin/env bash
# Run pod-desktop on a headless X server and screenshot it.
#
#   scripts/screenshot.sh [out.png] [-- xdotool args...]
#
# Without xdotool args it waits for the first frame and captures it. With
# them, it runs `xdotool <args>` against the app window first, e.g.
#
#   scripts/screenshot.sh tmp/shots/signup.png -- \
#     mousemove 701 624 click 1 sleep 1
#
# The app keeps its data in tmp/xdg-data, never in your real data dir.
# Delete that directory to get back to the sign-in screen.
#
# Xvfb listens on TCP 127.0.0.1:6099 instead of /tmp/.X11-unix because the
# nono sandbox refuses unix socket binds. A per-run cookie in tmp/xauth
# keeps other local clients out.
#
# Rendering goes through Mesa's lavapipe (software Vulkan). Set
# VK_ICD_FILENAMES to override the NixOS default below.
set -euo pipefail

cd "$(dirname "$0")/.."
out=${1:-tmp/shots/screen.png}
shift || true
[[ ${1:-} == -- ]] && shift

mkdir -p tmp/xdg-data "$(dirname "$out")"
cargo build -p pod-desktop --quiet

export XAUTHORITY=$PWD/tmp/xauth
rm -f "$XAUTHORITY"
# FamilyWild (ffff) so the cookie matches TCP connections. `xauth add
# 127.0.0.1:99` would store it as <hostname>/unix:99 and TCP clients
# would be refused.
echo "ffff 0000 0002 3939 0012 4d49542d4d414749432d434f4f4b49452d31 0010 $(mcookie)" |
  xauth -f "$XAUTHORITY" nmerge -

Xvfb :99 -auth "$XAUTHORITY" -nolisten unix -nolisten local -listen tcp \
  -screen 0 1400x900x24 >tmp/xvfb.log 2>&1 &
xvfb=$!
trap 'kill "${app:-}" "$xvfb" 2>/dev/null || true' EXIT

export DISPLAY=127.0.0.1:99
for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.2; done
xdpyinfo >/dev/null

XDG_DATA_HOME=$PWD/tmp/xdg-data \
  VK_ICD_FILENAMES=${VK_ICD_FILENAMES:-/run/opengl-driver/share/vulkan/icd.d/lvp_icd.x86_64.json} \
  ./target/debug/pod-desktop >tmp/app.log 2>&1 &
app=$!

# The window maps before lavapipe has drawn anything, so give the first
# frame a moment after it appears.
xdotool search --sync --pid "$app" --onlyvisible >/dev/null
sleep 2

if (($# > 0)); then
  xdotool "$@"
fi
import -window root "$out"
echo "$out"
