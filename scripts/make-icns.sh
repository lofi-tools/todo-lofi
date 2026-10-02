#!/usr/bin/env bash
#
# Render the app's .icns from the checked-in SVG artwork.
#
# Two toolchains can rasterize the SVG and pack the result, and a host only
# ever has one of them:
#
#   - macOS' own sips + iconutil, on a developer machine and on the CI runners;
#   - nixpkgs' rsvg-convert + png2icns, which is what a `nix build` sandbox has,
#     since a sandbox cannot see /usr/bin at all (this is the toolchain
#     packages.taskstream-desktop-app relies on).
#
# Either way the artwork is painted on a 1024px canvas — sips rasterizes an SVG
# at its intrinsic size, which is not the size we want — and the grain is baked
# into the raster (scripts/icon-grain.py) rather than the SVG, so the artwork
# stays clean for its other consumers (web, Finder previews).
#
# Usage: make-icns.sh <icon.svg> <out.icns>
set -euo pipefail

svg=${1:?make-icns.sh needs the source SVG}
out=${2:?make-icns.sh needs the .icns to write}
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

sizes=(16 32 128 256 512)

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

iconset=$scratch/icon.iconset
mkdir -p "$iconset"
sed 's|<svg |<svg width="1024" height="1024" |' "$svg" > "$scratch/icon-1024.svg"

if command -v sips >/dev/null 2>&1 && command -v iconutil >/dev/null 2>&1; then
  sips -s format png "$scratch/icon-1024.svg" --out "$iconset/master.png" >/dev/null
  python3 "$repo/scripts/icon-grain.py" "$iconset/master.png" 20 5 11
  for size in "${sizes[@]}"; do
    sips -z "$size" "$size" "$iconset/master.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size * 2))" "$((size * 2))" "$iconset/master.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
  done
  # The master already is the 1024px rep; copying it keeps the grained pixels
  # instead of letting sips re-encode noise (which grows it ~20%).
  cp "$iconset/master.png" "$iconset/icon_512x512@2x.png"
  rm -f "$iconset/master.png"
  iconutil -c icns "$iconset" -o "$out"
elif command -v rsvg-convert >/dev/null 2>&1 && command -v png2icns >/dev/null 2>&1; then
  rsvg-convert -o "$iconset/master.png" "$scratch/icon-1024.svg"
  python3 "$repo/scripts/icon-grain.py" "$iconset/master.png" 20 5 11
  # Every rep is rasterized from the artwork rather than downsampled from the
  # grained master: rsvg-convert only reads SVG. The grain is a ~5px mottle, so
  # it only reads at icon sizes anyway, and the 1024 rep below still carries it
  # (which is the one Finder draws large).
  for size in "${sizes[@]}"; do
    rsvg-convert -w "$size" -h "$size" -o "$iconset/icon_${size}x${size}.png" "$scratch/icon-1024.svg"
    rsvg-convert -w "$((size * 2))" -h "$((size * 2))" -o "$iconset/icon_${size}x${size}@2x.png" "$scratch/icon-1024.svg"
  done
  cp "$iconset/master.png" "$iconset/icon_512x512@2x.png"
  rm -f "$iconset/master.png"
  # png2icns picks each icns type from the PNG's pixels, not its name, so the
  # @2x reps simply land as separate (larger) types.
  png2icns "$out" \
    "$iconset/icon_16x16.png" "$iconset/icon_32x32.png" \
    "$iconset/icon_128x128.png" "$iconset/icon_256x256.png" \
    "$iconset/icon_512x512.png" "$iconset/icon_512x512@2x.png"
else
  echo "make-icns.sh: need sips + iconutil, or rsvg-convert + png2icns" >&2
  exit 1
fi

[ -f "$out" ] || { echo "make-icns.sh: nothing was written to $out" >&2; exit 1; }
