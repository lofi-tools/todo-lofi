#!/usr/bin/env bash
#
# Assemble the Linux artifacts: a portable tarball, a .deb, and (when an
# appimagetool is supplied) an AppImage.
#
# The binary comes from `nix build .#todo-2`, so it is dynamically linked
# against /nix/store paths that no other machine has. Every artifact therefore
# bundles the runtime libraries it was linked against and ships a wrapper that
# points the loader at them:
#
#   usr/bin/todo-lofi      wrapper: puts usr/lib/todo-lofi on LD_LIBRARY_PATH
#   usr/bin/todo-lofi.bin  the built binary
#   usr/lib/todo-lofi/*    the shared libraries it needs, named by SONAME
#
# Libraries are collected from the binary's ldd closure rather than from the
# whole nix closure: the latter would drag in rustc's and gcc's dylibs. Anything
# the GPU stack dlopens (the Vulkan/EGL drivers, the ICD loader) stays on the
# system, as it does for any packaged GPU application.
#
# Usage: package-linux.sh --binary <path> [--out DIR] [--appimagetool PATH]
#                         [--appimage-runtime FILE] [--version X]
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
icon_svg=$repo/apps/todo-2/assets/icons/do-list-app.svg

binary=
out=dist/linux
appimagetool=
appimage_runtime=

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
	--appimagetool)
		appimagetool=${2:?--appimagetool needs a path}
		shift 2
		;;
	--appimage-runtime)
		appimage_runtime=${2:?--appimage-runtime needs a path}
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
		echo "package-linux.sh: unknown argument $1" >&2
		exit 2
		;;
	esac
done

[ -n "$binary" ] && [ -f "$binary" ] || {
	echo "package-linux.sh: --binary must name a built todo-2 binary" >&2
	exit 2
}
# Version comes from Cargo.toml; flake.nix passes it explicitly because the
# sandbox has no checkout of the workspace root to read it from.
version=${version:-$("$repo/scripts/version.sh")}
arch=$(uname -m)
case $arch in
x86_64) deb_arch=amd64 ;;
aarch64) deb_arch=arm64 ;;
*) deb_arch=$arch ;;
esac

# The closure is what makes the bundle possible, so refuse a binary built
# outside the store rather than shipping something that cannot run.
# Skipped when PACKAGE_LINUX_SKIP_STORE_CHECK is set: the nix derivation
# (flake.nix `packages.todo-lofi-linux-dist`) only ever passes store paths,
# and `nix-store` cannot reach a daemon inside the sandbox anyway.
if [ -z "${PACKAGE_LINUX_SKIP_STORE_CHECK:-}" ] && ! nix-store -qR "$binary" >/dev/null 2>&1; then
	echo "package-linux.sh: $binary is not a nix store path; build it with 'nix build .#todo-2'" >&2
	exit 2
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
appdir=$work/todo-lofi

mkdir -p "$appdir/usr/bin" "$appdir/usr/lib/todo-lofi" "$appdir/usr/share/applications"
cp -f "$binary" "$appdir/usr/bin/todo-lofi.bin"
chmod +x "$appdir/usr/bin/todo-lofi.bin"

# Every shared library the loader will reach, following each one's own deps.
declare -A seen=()
collect() {
	local dep
	while read -r dep; do
		[ -n "$dep" ] || continue
		[ -f "$dep" ] || continue
		[ -n "${seen[$dep]:-}" ] && continue
		seen[$dep]=1
		collect "$dep"
	done < <(ldd "$1" 2>/dev/null | awk '/=> \//{print $3} /^[[:space:]]*\//{print $1}')
}
collect "$appdir/usr/bin/todo-lofi.bin"

for lib in "${!seen[@]}"; do
	cp -Lf "$lib" "$appdir/usr/lib/todo-lofi/$(basename "$lib")"
done

# The loader resolves a versioned library by its SONAME, which is not the file
# name it happens to be stored under (libfoo.so.1 vs libfoo.so.1.2.3).
for lib in "$appdir"/usr/lib/todo-lofi/*; do
	# No `exit` in the awk: quitting early closes the pipe while objdump is
	# still writing, killing it with SIGPIPE (exit 141 under pipefail). Read
	# it all and keep the first SONAME line instead.
	soname=$(objdump -p "$lib" 2>/dev/null | awk '/SONAME/{print $2}')
	soname=${soname%%$'\n'*}
	[ -n "$soname" ] || continue
	[ -e "$appdir/usr/lib/todo-lofi/$soname" ] && continue
	ln -s "$(basename "$lib")" "$appdir/usr/lib/todo-lofi/$soname"
done

cat >"$appdir/usr/bin/todo-lofi" <<'WRAPPER'
#!/bin/sh
# Run todo-lofi against the libraries bundled beside it.
here=$(dirname "$(readlink -f "$0")")
LD_LIBRARY_PATH="$here/../lib/todo-lofi${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export LD_LIBRARY_PATH
exec "$here/todo-lofi.bin" "$@"
WRAPPER
chmod +x "$appdir/usr/bin/todo-lofi"

cat >"$appdir/usr/share/applications/todo-lofi.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=todo-lofi
Comment=A lofi, local-first task manager
Exec=todo-lofi
Icon=todo-lofi
Terminal=false
Categories=Office;Utility;
DESKTOP

# The AppImage and the icon theme both want a plain PNG, which is the one thing
# the repo does not carry: the artwork is an SVG. The output directory must
# exist before rsvg-convert runs; without it the conversion fails and the
# `|| true` below would silently ship no icon.
mkdir -p "$appdir/usr/share/icons/hicolor/512x512/apps"
if command -v rsvg-convert >/dev/null 2>&1; then
	rsvg-convert -w 512 -h 512 "$icon_svg" -o "$appdir/usr/share/icons/hicolor/512x512/apps/todo-lofi.png" 2>/dev/null || true
fi
if [ -f "$appdir/usr/share/icons/hicolor/512x512/apps/todo-lofi.png" ]; then
	cp -f "$appdir/usr/share/icons/hicolor/512x512/apps/todo-lofi.png" "$appdir/todo-lofi.png"
else
	echo "package-linux.sh: no icon generated (rsvg-convert missing or failed), shipping without one" >&2
fi

mkdir -p "$out"
echo "package-linux.sh: bundled $(find "$appdir/usr/lib/todo-lofi" | wc -l) libraries"

# Portable tarball: extract anywhere and run usr/bin/todo-lofi.
tar -C "$work" -czf "$out/todo-lofi-$version-linux-$arch.tar.gz" todo-lofi
echo "package-linux.sh: $out/todo-lofi-$version-linux-$arch.tar.gz"

# The .deb installs the same tree under /usr, so its wrapper and library layout
# are the ones already built above.
deb=$work/deb
mkdir -p "$deb/DEBIAN"
cp -a "$appdir/usr" "$deb/usr"
cat >"$deb/DEBIAN/control" <<CONTROL
Package: todo-lofi
Version: $version
Section: utils
Priority: optional
Architecture: $deb_arch
Maintainer: todo-lofi <noreply@github.com>
Description: A lofi, local-first task manager
 Nested tags, an embedded AI agent pane, Todoist sync and a keyboard-first
 workflow, running offline.
CONTROL
dpkg-deb --build --root-owner-group "$deb" "$out/todo-lofi-$version-linux-$arch.deb" >/dev/null
echo "package-linux.sh: $out/todo-lofi-$version-linux-$arch.deb"

if [ -n "$appimagetool" ] && [ -x "$appimagetool" ]; then
	# AppRun is what the AppImage runtime executes; the AppDir's own copy of the
	# wrapper already sets LD_LIBRARY_PATH against its siblings.
	ln -sf usr/bin/todo-lofi "$appdir/AppRun"
	# appimagetool only looks for the .desktop file at the AppDir top level
	# (the copy under usr/share/applications is for the .deb).
	cp -f "$appdir/usr/share/applications/todo-lofi.desktop" "$appdir/todo-lofi.desktop"
	[ -f "$appdir/todo-lofi.png" ] || { : >"$appdir/todo-lofi.png"; }
	arch_env=$arch
	case $arch in
	x86_64) arch_env=x86_64 ;;
	aarch64) arch_env=aarch64 ;;
	esac
	# appimagetool downloads the runtime it prepends to the AppImage from
	# type2-runtime's GitHub releases unless it is handed one. That download is
	# the only network access in this script, and it is fatal in a sandbox, so
	# callers with no network pass a pinned runtime and it is used instead.
	runtime_args=()
	if [ -n "$appimage_runtime" ]; then
		[ -f "$appimage_runtime" ] || {
			echo "package-linux.sh: --appimage-runtime $appimage_runtime does not exist" >&2
			exit 2
		}
		runtime_args=(--runtime-file "$appimage_runtime")
	fi
	ARCH=$arch_env "$appimagetool" --appimage-extract-and-run "${runtime_args[@]}" "$appdir" \
		"$out/todo-lofi-$version-linux-$arch.AppImage" >/dev/null
	echo "package-linux.sh: $out/todo-lofi-$version-linux-$arch.AppImage"
elif [ -n "$appimagetool" ]; then
	echo "package-linux.sh: --appimagetool $appimagetool is not executable" >&2
	exit 1
fi
