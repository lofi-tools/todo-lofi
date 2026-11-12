#!/usr/bin/env bash
# Keep local Cargo test-binary launches off the Gatekeeper assessment path.
#
# Rust builds an unsigned Mach-O test binary and drops it in target/. macOS can
# make every launch wait on assessment, which turns a fast test run into a
# stall. This removes quarantine recursively and ad-hoc signs every executable
# Mach-O file in the target directory, like the taskstream dev-shell `tt`
# helper does for the binaries produced by `cargo test --no-run`.
#
# Usage: scripts/disable-gatekeeper.sh [target-directory]
# Defaults to $CARGO_TARGET_DIR, then ./target.
set -euo pipefail

root=${1:-${CARGO_TARGET_DIR:-target}}
if [[ ! -d "$root" ]]; then
  echo "disable-gatekeeper: target directory not found: $root" >&2
  exit 1
fi

identity=${SHEPHERD_CODESIGN_IDENTITY:--}

# Only Cargo's build outputs belong to this developer. Nix's package-check and
# kache scratch trees under target/ are root-owned and can make a recursive
# xattr sweep fail or wait on files this user cannot inspect.
signed=0
for directory in "$root/debug" "$root"/*/debug; do
  [[ -d "$directory" ]] || continue
  case "$directory" in
    */package-check/*) continue ;;
  esac
  # Quarantine can be attached to bundles and dylibs as well as executables.
  xattr -dr com.apple.quarantine "$directory" 2>/dev/null || true
  while IFS= read -r -d '' path; do
    if ! file -b "$path" | grep -q "Mach-O"; then
      continue
    fi
    if codesign --force --sign "$identity" "$path" >/dev/null 2>&1; then
      signed=$((signed + 1))
    fi
  done < <(find -P "$directory" -type f -perm -u+x -print0)
done

echo "disable-gatekeeper: cleared quarantine in Cargo debug directories under $root; signed $signed executable(s) with $identity"
