#!/usr/bin/env bash
#
# Zip the Windows build.
#
# The binary comes from the native Windows build the packaging job runs on a
# Windows runner (mingw cross-compiles cannot work: gpui-pre-windows'
# build.rs compiles its HLSL shaders only when the build host itself is
# Windows). The MSVC toolchain links its runtime statically, so the
# executable is self-contained; any DLL the linker did leave beside it is
# shipped anyway rather than assumed absent.
#
# Usage: package-windows.sh --binary <path> [--out DIR] [--version X]
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

binary=
out=dist/windows
version=

while [ $# -gt 0 ]; do
	case $1 in
	--binary)
		binary=${2:?--binary needs a path}
		shift 2
		;;
	--out)
		out=${2:?--out needs a path}
		shift 2
		;;
	--version)
		version=${2:?--version needs a value}
		shift 2
		;;
	-h | --help)
		sed -n '/^# Usage:/,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' | sed '$d'
		exit 0
		;;
	*)
		echo "package-windows.sh: unknown argument $1" >&2
		exit 2
		;;
	esac
done

[ -n "$binary" ] && [ -f "$binary" ] || {
	echo "package-windows.sh: --binary must name a built taskstream-desktop.exe" >&2
	exit 2
}
# Version comes from Cargo.toml, so the archive is named after the build. Run
# the resolver through bash: a Windows checkout does not keep its exec bit.
version=${version:-$(bash "$repo/scripts/version.sh")}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
stage=$work/taskstream
mkdir -p "$stage"

# The crate is `taskstream-desktop`; the shipped program is the product's name.
cp -f "$binary" "$stage/taskstream.exe"
for dll in "$(dirname "$binary")"/*.dll; do
	[ -f "$dll" ] || continue
	cp -f "$dll" "$stage/"
done

mkdir -p "$out"
# Resolve to an absolute path now: the archive commands below run after
# `cd "$work"`, where a relative $out would land inside the temp dir and the
# caller's dist/ would stay empty.
out=$(cd "$out" && pwd)
# Git Bash on the Windows runner image has no `zip`; 7-Zip is on PATH there
# instead. Either makes the same .zip container.
if command -v zip >/dev/null 2>&1; then
	(cd "$work" && zip -qr "$out/taskstream-$version-windows-x86_64.zip" taskstream)
elif command -v 7z >/dev/null 2>&1; then
	(cd "$work" && 7z a -tzip "$out/taskstream-$version-windows-x86_64.zip" taskstream >/dev/null)
else
	echo "package-windows.sh: need zip or 7z to make the archive" >&2
	exit 1
fi
echo "package-windows.sh: $out/taskstream-$version-windows-x86_64.zip"
