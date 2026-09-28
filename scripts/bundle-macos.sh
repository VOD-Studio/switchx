#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cargo build --manifest-path "$repo_root/Cargo.toml"
bundle="$repo_root/target/debug/SwitchX.app"
mkdir -p "$bundle/Contents/MacOS"
mkdir -p "$bundle/Contents/Resources"
cp "$repo_root/packaging/macos/Info.plist" "$bundle/Contents/Info.plist"
cp "$repo_root/target/debug/switchx" "$bundle/Contents/MacOS/switchx"
cp "$repo_root/assets/providers/NOTICE.md" "$bundle/Contents/Resources/Provider-Icons-NOTICE.md"
echo "$bundle"
