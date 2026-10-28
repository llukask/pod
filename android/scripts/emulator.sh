#!/usr/bin/env bash
# Boots an API 36 emulator for the Android client. Run it inside `nix develop
# .#android-emulator`; the AVD is created on first use in $ANDROID_AVD_HOME,
# since the SDK itself sits in the read-only Nix store. Extra arguments go to
# the emulator, e.g. `-no-window -no-audio` for a headless run.
#
# The emulator reaches the host's loopback at 10.0.2.2, so a pod-server on
# the host's port 3000 is http://10.0.2.2:3000 in the sign-in form.
set -euo pipefail

avd_name="${AVD_NAME:-pod}"
system_image="system-images;android-36;google_apis;x86_64"

if [ -z "${ANDROID_HOME:-}" ]; then
  echo "enter the emulator dev shell first: nix develop .#android-emulator" >&2
  exit 1
fi

if ! avdmanager list avd -c | grep -qx "$avd_name"; then
  echo no | avdmanager create avd --name "$avd_name" --package "$system_image" --device pixel_7 --force
fi

exec emulator -avd "$avd_name" -no-snapshot-save -gpu swiftshader_indirect "$@"
