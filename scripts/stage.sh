#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
case "${1:-armhf}" in
    armhf|armv7-unknown-linux-musleabihf) target=armv7-unknown-linux-musleabihf ;;
    *) echo "Unsupported package architecture: $1" >&2; exit 1 ;;
esac

stage=${2:-build/stage}
target_dir=${CARGO_TARGET_DIR:-target}
binary=$target_dir/$target/release/files-kindle
[ -x "$binary" ] || { echo "Build $binary before staging." >&2; exit 1; }
file "$binary" | grep -q 'ELF.*ARM' || { echo "Expected an ARM binary." >&2; exit 1; }
[ -r assets/files-icon.png ] || { echo "Missing the Files icon." >&2; exit 1; }
icon="# Icon: data:image/png;base64,$(base64 < assets/files-icon.png | tr -d '\n')"

install -d "$stage/files/bin" "$stage/documents"
install -m 755 "$binary" "$stage/files/bin/files-kindle"
awk -v icon="$icon" '{ print } /^# Author:/ { print icon }' deploy/Files.sh > "$stage/documents/Files.sh"
chmod 755 "$stage/documents/Files.sh"
