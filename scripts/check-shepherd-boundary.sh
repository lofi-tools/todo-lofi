#!/usr/bin/env bash
#
# Keep Shepherd's shared core headless. The shell owns Tauri and the future
# GPUI app; shepherd-core must never pull either one, or a webview stack, into
# its dependency closure.
#
# Usage:
#   scripts/check-shepherd-boundary.sh [cargo-tree-output.txt]
#
# With no argument, runs `cargo tree -p shepherd-core --edges normal,build,dev`.
# The file form exists so this check itself can be tested without a full build.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo"

if [[ ${1:-} == "--help" ]]; then
  cat <<'EOF'
Keep Shepherd's shared core headless. The shell owns Tauri and the future GPUI
app; shepherd-core must never pull either one, or a webview stack, into its
dependency closure.

Usage:
  scripts/check-shepherd-boundary.sh [cargo-tree-output.txt]

With no argument, runs `cargo tree -p shepherd-core --edges normal,build,dev`.
The file form exists so this check itself can be tested without a full build.
EOF
  exit 0
fi
if (($# > 1)); then
  echo "usage: $0 [cargo-tree-output.txt]" >&2
  exit 2
fi

if (($# == 1)); then
  tree_file=${1:?}
  if [[ ! -f $tree_file ]]; then
    echo "boundary check: no tree file at $tree_file" >&2
    exit 2
  fi
  tree=$(<"$tree_file")
else
  if ! command -v cargo >/dev/null 2>&1; then
    echo "boundary check: cargo is not on PATH; run this inside the Nix dev shell" >&2
    exit 2
  fi
  tree=$(cargo tree -p shepherd-core --edges normal,build,dev)
fi

forbidden='(^|[^[:alnum:]_-])((tauri|gpui)(-[[:alnum:]_-]+)?|wry|webkit2gtk(-[[:alnum:]_-]+)?|webview2-com(-[[:alnum:]_-]+)?|webview(-[[:alnum:]_-]+)?)([^[:alnum:]_-]|$)'

if violations=$(printf '%s\n' "$tree" | grep -Ei "$forbidden"); then
  printf 'shepherd-core dependency boundary violated:\n%s\n' "$violations" >&2
  exit 1
fi

echo "shepherd-core dependency boundary OK"
