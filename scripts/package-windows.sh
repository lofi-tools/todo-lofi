#!/usr/bin/env bash
#
# Zip the cross-compiled Windows build.
#
# The binary comes from the mingw cross build the packaging job runs in the
# `todo-2-windows` devshell (see apps/todo-2/part.p.nix). That toolchain links
# its runtime statically, so the executable is self-contained; any DLL the
# linker did leave beside it is shipped anyway rather than assumed absent.
#
# Usage: package-windows.sh --binary <path> [--out DIR] [--version X]
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
plist=$repo/apps/todo-2/assets/Info.plist

binary=
out=dist/windows
version=

while [ $# -gt 0 ]; do
  case $1 in
    --binary) binary=${2:?--binary needs a path}; shift 2 ;;
    --out) out=${2:?--out needs a path}; shift 2 ;;
    --version) version=${2:?--version needs a value}; shift 2 ;;
    -h|--help)
      sed -n '/^# Usage:/,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' | sed '$d'
      exit 0 ;;
    *) echo "package-windows.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done

[ -n "$binary" ] && [ -f "$binary" ] || {
  echo "package-windows.sh: --binary must name a built todo-2.exe" >&2
  exit 2
}
version=${version:-$(sed -n 's/.*CFBundleShortVersionString<\/key><string>\([^<]*\)<\/string>.*/\1/p' "$plist")}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
stage=$work/todo-lofi
mkdir -p "$stage"

# The crate is `todo-2`; the shipped program is the product's name.
cp -f "$binary" "$stage/todo-lofi.exe"
for dll in "$(dirname "$binary")"/*.dll; do
  [ -f "$dll" ] || continue
  cp -f "$dll" "$stage/"
done

mkdir -p "$out"
(cd "$work" && zip -qr "$out/todo-lofi-$version-windows-x86_64.zip" todo-lofi)
echo "package-windows.sh: $out/todo-lofi-$version-windows-x86_64.zip"
