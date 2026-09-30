#!/bin/sh
set -eu

profile=debug
if [ "$#" -eq 1 ] && [ "$1" = "--release" ]; then
    profile=release
elif [ "$#" -ne 0 ]; then
    echo "Usage: $0 [--release]" >&2
    exit 2
fi

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cargo build --manifest-path "$repo_root/Cargo.toml" "$@"
bundle="$repo_root/target/$profile/SwitchX.app"
mkdir -p "$bundle/Contents/MacOS"
mkdir -p "$bundle/Contents/Resources"
cp "$repo_root/packaging/macos/Info.plist" "$bundle/Contents/Info.plist"
cp "$repo_root/target/$profile/switchx" "$bundle/Contents/MacOS/switchx"
cp "$repo_root/assets/providers/NOTICE.md" "$bundle/Contents/Resources/Provider-Icons-NOTICE.md"

icon_tmp=$(mktemp -d)
trap 'rm -rf "$icon_tmp"' 0
icon_source="$icon_tmp/AppIcon.png"
swift "$repo_root/scripts/prepare-macos-icon.swift" "$repo_root/assets/app-icon.png" "$icon_source"
iconset="$icon_tmp/AppIcon.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$icon_source" \
        --out "$iconset/icon_${size}x${size}.png" >/dev/null
    retina_size=$((size * 2))
    sips -z "$retina_size" "$retina_size" "$icon_source" \
        --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$bundle/Contents/Resources/AppIcon.icns"
# Let Finder notice updated resources when rebuilding an existing app bundle.
touch "$bundle"
echo "$bundle"
