#!/usr/bin/env bash
#
# Assemble todo-lofi.app and, unless told otherwise, the archives that ship.
#
# Two callers share this. `t2-bundle` (flake.nix) runs it over the debug binary
# it just built, with --link --no-archives --register, so the local bundle
# tracks rebuilds and the Dock picks up a new icon. The packaging job in
# .github/workflows/bundle.yml runs it over the release binary `nix build .#todo-2`
# produced, copying that binary in and writing the .zip and .dmg.
#
# The icon is generated rather than committed: sips rasterizes an SVG at its
# intrinsic size, so the artwork is painted on a 1024px canvas instead, and the
# grain is baked into that raster (scripts/icon-grain.py) so the SVG stays clean
# for its web consumers.
#
# Usage: package-macos.sh --binary <path> [--out DIR] [--link] [--register]
#                         [--no-archives]
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
icon_svg=$repo/apps/todo-2/assets/icons/do-list-app.svg
plist=$repo/apps/todo-2/assets/Info.plist
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

binary=
out=dist/macos
link=0
register=0
archives=1

while [ $# -gt 0 ]; do
  case $1 in
    --binary) binary=${2:?--binary needs a path}; shift 2 ;;
    --out) out=${2:?--out needs a path}; shift 2 ;;
    --link) link=1; shift ;;
    --register) register=1; shift ;;
    --no-archives) archives=0; shift ;;
    -h|--help)
      sed -n '/^# Usage:/,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' | sed '$d'
      exit 0 ;;
    *) echo "package-macos.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done

[ -n "$binary" ] && [ -f "$binary" ] || {
  echo "package-macos.sh: --binary must name a built todo-2 binary" >&2
  exit 2
}

version=$(sed -n 's/.*CFBundleShortVersionString<\/key><string>\([^<]*\)<\/string>.*/\1/p' "$plist")
[ -n "$version" ] || { echo "package-macos.sh: no CFBundleShortVersionString in $plist" >&2; exit 1; }
arch=$(uname -m)

app=$out/todo-lofi.app
exe=$app/Contents/MacOS/todo-2
icns=$app/Contents/Resources/todo-lofi.icns

mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp -f "$plist" "$app/Contents/Info.plist"

# Regenerate whenever the artwork or this recipe changed, so an icon baked
# earlier (qlmanage composites the SVG on a white matte) can't stick.
if [ ! -f "$icns" ] || [ "$icon_svg" -nt "$icns" ] || [ "${BASH_SOURCE[0]}" -nt "$icns" ]; then
  scratch=$(mktemp -d)
  trap 'rm -rf "$scratch"' EXIT
  iconset=$scratch/icon.iconset
  mkdir -p "$iconset"
  ico=$(cksum "$icns" 2>/dev/null || echo none)

  sed 's|<svg |<svg width="1024" height="1024" |' "$icon_svg" > "$scratch/icon-1024.svg"
  sips -s format png "$scratch/icon-1024.svg" --out "$iconset/master.png" >/dev/null
  # Grain is composited into the raster rather than committed in the SVG, so the
  # artwork stays clean for other consumers and only the built icon pays for the
  # texture (cell px at 1024, amplitude, seed).
  python3 "$repo/scripts/icon-grain.py" "$iconset/master.png" 20 5 11
  for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$iconset/master.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size * 2))" "$((size * 2))" "$iconset/master.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
  done
  # The master already is the 1024px rep; copying it keeps the grained pixels
  # instead of letting sips re-encode noise (which grows it ~20%).
  cp "$iconset/master.png" "$iconset/icon_512x512@2x.png"
  rm -f "$iconset/master.png"
  iconutil -c icns "$iconset" -o "$icns"
  rm -rf "$scratch"
  trap - EXIT

  # IconServices caches rendered tiles, so the Dock goes on painting the previous
  # icon until the bundle is re-registered and the Dock restarts. Only the dev
  # bundle pays for that: a CI artifact is never launched from here.
  if [ "$register" = 1 ] && [ "$ico" != "$(cksum "$icns" | cut -d' ' -f1,2)" ]; then
    touch "$app" "$icns"
    "$lsregister" -f "$app" 2>/dev/null || true
    killall Dock 2>/dev/null || true
  fi
fi

if [ "$link" = 1 ]; then
  ln -sf "$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")" "$exe"
else
  # install rather than cp: a binary taken from the nix store is read-only, and
  # signing has to write to it.
  install -m 755 "$binary" "$exe"
fi

# Sign with the self-signed todo-lofi-dev identity when the machine has one so
# Gatekeeper doesn't throttle every launch; the ad-hoc identity otherwise, which
# is what a release artifact gets (nothing here is notarized).
identity=-
if security find-identity -v -p codesigning 2>/dev/null | grep -q "todo-lofi-dev"; then
  identity=todo-lofi-dev
fi
codesign --force --sign "$identity" "$exe" >/dev/null 2>&1 || true
codesign --force --deep --sign "$identity" "$app" >/dev/null 2>&1 || true
xattr -dr com.apple.quarantine "$app" 2>/dev/null || true

echo "package-macos.sh: $app ($arch, $version, signed $identity)"

if [ "$archives" = 1 ]; then
  # ditto rather than zip: it keeps the bundle's symlinks and extended
  # attributes, which is what keeps the signature valid after extraction.
  ditto -c -k --sequesterRsrc --keepParent "$app" "$out/todo-lofi-$version-macos-$arch.zip"
  rm -f "$out/todo-lofi-$version-macos-$arch.dmg"
  hdiutil create \
    -volname "todo-lofi $version" \
    -srcfolder "$app" \
    -ov -format UDZO \
    "$out/todo-lofi-$version-macos-$arch.dmg" >/dev/null
  echo "package-macos.sh: $out/todo-lofi-$version-macos-$arch.zip"
  echo "package-macos.sh: $out/todo-lofi-$version-macos-$arch.dmg"
fi
