#!/usr/bin/env bash
#
# Print the version of the todo-2 package, resolved the way cargo resolves it:
# the package's own `version` when it has one, and the workspace's when it
# inherits it with `version.workspace = true`.
#
# The manifests are parsed as text rather than through `cargo metadata` because
# the Linux artifacts are assembled inside the nix sandbox, where no cargo is on
# PATH. Cargo.toml stays the single source of truth for the version; this only
# reads it.
#
# Usage: version.sh
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
member=$repo/apps/todo-2/Cargo.toml
workspace=$repo/Cargo.toml

[ -f "$member" ] || { echo "version.sh: no $member" >&2; exit 1; }

version=$(sed -n 's/^[[:space:]]*version[[:space:]]*=[[:space:]]*"\([^"]*\)"[[:space:]]*$/\1/p' "$member" | head -n1)

if [ -z "$version" ] && grep -qE '^[[:space:]]*version\.workspace[[:space:]]*=[[:space:]]*true' "$member"; then
  # Both shapes the workspace version can take: a `[workspace.package]` table,
  # and the inline `package = { version = "..." }` field.
  version=$(awk '
    /^\[workspace\.package\]/ { in_table = 1; next }
    /^\[/ { in_table = 0 }
    in_table && /^[[:space:]]*version[[:space:]]*=/ {
      match($0, /"[^"]*"/); print substr($0, RSTART + 1, RLENGTH - 2); exit
    }
    /^[[:space:]]*package[[:space:]]*=[[:space:]]*\{/ {
      if (match($0, /version[[:space:]]*=[[:space:]]*"[^"]*"/)) {
        field = substr($0, RSTART, RLENGTH)
        match(field, /"[^"]*"/); print substr(field, RSTART + 1, RLENGTH - 2); exit
      }
    }
  ' "$workspace")
fi

[ -n "$version" ] || { echo "version.sh: could not resolve todo-2's version from $member" >&2; exit 1; }
printf '%s\n' "$version"
