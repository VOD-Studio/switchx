#!/bin/sh
set -eu

profile=debug
target=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --release) profile=release; shift ;;
        --target)
            [ "$#" -ge 2 ] || { echo '--target requires a Rust target triple' >&2; exit 2; }
            target=$2
            shift 2
            ;;
        *) echo "Usage: $0 [--release] [--target TRIPLE]" >&2; exit 2 ;;
    esac
done

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
bundle_tmp=$(mktemp -d)
trap 'rm -rf "$bundle_tmp"' 0
set -- --locked --manifest-path "$repo_root/Cargo.toml" --bin switchx
[ "$profile" != release ] || set -- "$@" --release
[ -z "$target" ] || set -- "$@" --target "$target"
cargo build "$@"
cargo metadata --locked --no-deps --format-version 1 --manifest-path "$repo_root/Cargo.toml" > "$bundle_tmp/metadata.json"
python=${PYTHON:-python3}
target_dir=$("$python" -c 'import json, sys; print(json.load(open(sys.argv[1]))["target_directory"])' "$bundle_tmp/metadata.json")
version=$("$python" -c 'import json, sys; print(next(p["version"] for p in json.load(open(sys.argv[1]))["packages"] if p["name"] == "switchx"))' "$bundle_tmp/metadata.json")
[ -z "$target" ] || target_dir="$target_dir/$target"
bundle="$target_dir/$profile/SwitchX.app"
mkdir -p "$bundle/Contents/MacOS"
mkdir -p "$bundle/Contents/Resources"
"$python" - "$repo_root/packaging/macos/Info.plist" "$bundle/Contents/Info.plist" "$version" <<'PY'
import plistlib
import sys

with open(sys.argv[1], 'rb') as source:
    info = plistlib.load(source)
info['CFBundleShortVersionString'] = sys.argv[3].split('-')[0]
info['CFBundleVersion'] = sys.argv[3].split('-')[0]
info['SwitchXVersion'] = sys.argv[3]
with open(sys.argv[2], 'wb') as destination:
    plistlib.dump(info, destination, sort_keys=False)
PY
cp "$target_dir/$profile/switchx" "$bundle/Contents/MacOS/switchx"
cp "$repo_root/assets/providers/NOTICE.md" "$bundle/Contents/Resources/Provider-Icons-NOTICE.md"

icon_tmp="$bundle_tmp"
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
# Ad-hoc signing makes the bundle internally consistent on Apple Silicon.
# Developer ID signing and notarization require an Apple developer account.
codesign --force --deep --sign - "$bundle"
codesign --verify --deep --strict "$bundle"
# Let Finder notice updated resources when rebuilding an existing app bundle.
touch "$bundle"
echo "$bundle"
