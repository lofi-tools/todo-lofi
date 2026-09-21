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
#                         [--version X]
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
plist=$repo/apps/todo-2/assets/Info.plist
icon_svg=$repo/apps/todo-2/assets/icons/do-list-app.svg

binary=
out=dist/linux
appimagetool=

while [ $# -gt 0 ]; do
  case $1 in
    --binary) binary=${2:?--binary needs a path}; shift 2 ;;
    --out) out=${2:?--out needs a path}; shift 2 ;;
    --appimagetool) appimagetool=${2:?--appimagetool needs a path}; shift 2 ;;
    --version) version=${2:?--version needs a value}; shift 2 ;;
    -h|--help)
      sed -n '/^# Usage:/,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' | sed '$d'
      exit 0 ;;
    *) echo "package-linux.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done

[ -n "$binary" ] && [ -f "$binary" ] || {
  echo "package-linux.sh: --binary must name a built todo-2 binary" >&2
  exit 2
}
version=${version:-$(sed -n 's/.*CFBundleShortVersionString<\/key><string>\([^<]*\)<\/string>.*/\1/p' "$plist")}
arch=$(uname -m)
case $arch in
  x86_64) deb_arch=amd64 ;;
  aarch64) deb_arch=arm64 ;;
  *) deb_arch=$arch ;;
esac

# The closure is what makes the bundle possible, so refuse a binary built
# outside the store rather than shipping something that cannot run.
if ! nix-store -qR "$binary" >/dev/null 2>&1; then
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
  soname=$(objdump -p "$lib" 2>/dev/null | awk '/SONAME/{print $2; exit}')
  [ -n "$soname" ] || continue
  [ -e "$appdir/usr/lib/todo-lofi/$soname" ] && continue
  ln -s "$(basename "$lib")" "$appdir/usr/lib/todo-lofi/$soname"
done

cat > "$appdir/usr/bin/todo-lofi" <<'WRAPPER'
#!/bin/sh
# Run todo-lofi against the libraries bundled beside it.
here=$(dirname "$(readlink -f "$0")")
LD_LIBRARY_PATH="$here/../lib/todo-lofi${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export LD_LIBRARY_PATH
exec "$here/todo-lofi.bin" "$@"
WRAPPER
chmod +x "$appdir/usr/bin/todo-lofi"

cat > "$appdir/usr/share/applications/todo-lofi.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=todo-lofi
Comment=A lofi, local-first task manager
Exec=todo-lofi
Terminal=false
Categories=Office;Utility;
DESKTOP

# The AppImage and the icon theme both want a plain PNG, which is the one thing
# the repo does not carry: the artwork is an SVG.
if command -v rsvg-convert >/dev/null 2>&1; then
  rsvg-convert -w 512 -h 512 "$icon_svg" -o "$appdir/usr/share/icons/hicolor/512x512/apps/todo-lofi.png" 2>/dev/null || true
fi
if [ -f "$appdir/usr/share/icons/hicolor/512x512/apps/todo-lofi.png" ]; then
  mkdir -p "$appdir/usr/share/icons/hicolor/512x512/apps"
  cp -f "$appdir/usr/share/icons/hicolor/512x512/apps/todo-lofi.png" "$appdir/todo-lofi.png"
else
  echo "package-linux.sh: rsvg-convert missing, shipping without an icon" >&2
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
cat > "$deb/DEBIAN/control" <<CONTROL
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
  [ -f "$appdir/todo-lofi.png" ] || { : > "$appdir/todo-lofi.png"; }
  arch_env=$arch
  case $arch in
    x86_64) arch_env=x86_64 ;;
    aarch64) arch_env=aarch64 ;;
  esac
  ARCH=$arch_env "$appimagetool" --appimage-extract-and-run "$appdir" \
    "$out/todo-lofi-$version-linux-$arch.AppImage" >/dev/null
  echo "package-linux.sh: $out/todo-lofi-$version-linux-$arch.AppImage"
elif [ -n "$appimagetool" ]; then
  echo "package-linux.sh: --appimagetool $appimagetool is not executable" >&2
  exit 1
fi
