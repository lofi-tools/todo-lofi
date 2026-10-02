#!/usr/bin/env bash
#
# Assemble taskstream.app and, unless told otherwise, the archives that ship.
#
# Two callers share this. `t2-bundle` (flake.nix) runs it over the debug binary
# it just built, with --link --no-archives --register, so the local bundle
# tracks rebuilds and the Dock picks up a new icon. The packaging job in
# .github/workflows/bundle.yml runs it over the release binary `nix build .#taskstream-desktop`
# produced, copying that binary in and writing the .zip and .dmg.
#
# The icon is generated rather than committed. scripts/make-icns.sh owns the
# recipe, and picks the one toolchain the host has: macOS' sips + iconutil
# here, or nixpkgs' rsvg-convert + png2icns when a nix build runs this.
#
# Usage: package-macos.sh --binary <path> [--out DIR] [--version X] [--link]
#                         [--register] [--no-archives]
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
icon_svg=$repo/apps/taskstream-desktop/assets/icons/do-list-app.svg
plist=$repo/apps/taskstream-desktop/assets/Info.plist
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

binary=
out=dist/macos
version=
link=0
register=0
archives=1

while [ $# -gt 0 ]; do
  case $1 in
    --binary) binary=${2:?--binary needs a path}; shift 2 ;;
    --out) out=${2:?--out needs a path}; shift 2 ;;
    --version) version=${2:?--version needs a value}; shift 2 ;;
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
  echo "package-macos.sh: --binary must name a built taskstream-desktop binary" >&2
  exit 2
}

# Version comes from Cargo.toml rather than the checked-in plist, so the
# binary, the app bundle and the archive names all agree on one number.
version=${version:-$("$repo/scripts/version.sh")}
arch=$(uname -m)

app=$out/taskstream.app
exe=$app/Contents/MacOS/taskstream-desktop
icns=$app/Contents/Resources/taskstream.icns

mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
# The bundle reports the version the binary was built with: write it over the
# plist's own CFBundleShortVersionString instead of shipping the checked-in one.
sed "s|\\(<key>CFBundleShortVersionString</key><string>\\)[^<]*\\(</string>\\)|\\1$version\\2|" \
  "$plist" > "$app/Contents/Info.plist"
grep -q "<key>CFBundleShortVersionString</key><string>$version</string>" "$app/Contents/Info.plist" \
  || { echo "package-macos.sh: could not write version $version into the bundle's Info.plist" >&2; exit 1; }

# Regenerate whenever the artwork or the recipe changed, so an icon baked
# earlier (qlmanage composites the SVG on a white matte) can't stick.
if [ ! -f "$icns" ] || [ "$icon_svg" -nt "$icns" ] || [ "$repo/scripts/make-icns.sh" -nt "$icns" ]; then
  ico=$(cksum "$icns" 2>/dev/null || echo none)
  "$repo/scripts/make-icns.sh" "$icon_svg" "$icns"

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

# Sign with the self-signed taskstream-dev identity when the machine has one so
# Gatekeeper doesn't throttle every launch; the ad-hoc identity otherwise, which
# is what a release artifact gets (nothing here is notarized).
identity=-
if security find-identity -v -p codesigning 2>/dev/null | grep -q "taskstream-dev"; then
  identity=taskstream-dev
fi
codesign --force --sign "$identity" "$exe" >/dev/null 2>&1 || true
codesign --force --deep --sign "$identity" "$app" >/dev/null 2>&1 || true
xattr -dr com.apple.quarantine "$app" 2>/dev/null || true

echo "package-macos.sh: $app ($arch, $version, signed $identity)"

if [ "$archives" = 1 ]; then
  # ditto rather than zip: it keeps the bundle's symlinks and extended
  # attributes, which is what keeps the signature valid after extraction.
  ditto -c -k --sequesterRsrc --keepParent "$app" "$out/taskstream-$version-macos-$arch.zip"
  rm -f "$out/taskstream-$version-macos-$arch.dmg"
  hdiutil create \
    -volname "taskstream $version" \
    -srcfolder "$app" \
    -ov -format UDZO \
    "$out/taskstream-$version-macos-$arch.dmg" >/dev/null
  echo "package-macos.sh: $out/taskstream-$version-macos-$arch.zip"
  echo "package-macos.sh: $out/taskstream-$version-macos-$arch.dmg"
fi
