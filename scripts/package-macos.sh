#!/bin/sh
set -eu

project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_root"

cargo bundle --release --format osx

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
dist_dir="$project_root/dist/$version"
app_source="$project_root/target/release/bundle/osx/LUMIX LUT Maker.app"
app_destination="$dist_dir/LUMIX LUT Maker.app"
dmg_destination="$dist_dir/LUMIX-LUT-Maker-$version-macos-arm64.dmg"

mkdir -p "$dist_dir"
if [ -e "$app_destination" ]; then
    rm -rf "$app_destination"
fi
ditto "$app_source" "$app_destination"
if [ -e "$dmg_destination" ]; then
    rm -f "$dmg_destination"
fi
hdiutil create -volname "LUMIX LUT Maker $version" -srcfolder "$app_destination" -ov -format UDZO "$dmg_destination"

printf '%s\n' "$app_destination"
printf '%s\n' "$dmg_destination"
