#!/usr/bin/env bash
#
# Record one target's Shepherd release artifacts.
#
# A release job runs this after packaging one target, so the published release
# carries the artifact names, sizes, SHA-256 checksums, version and source
# commit. The publish job runs `--check` over every target's metadata before it
# creates the release; a missing target, a stale version or commit, or a
# checksum that no longer matches fails the run instead of publishing it.
#
# Usage:
#   shepherd-release-metadata.sh --dist DIR --version V --commit SHA \
#     --platform macos|linux|windows --arch arm64|x86_64 \
#     --expect EXT [--expect EXT ...] --out FILE
#   shepherd-release-metadata.sh --check DIR --version V --commit SHA
set -euo pipefail

usage() {
  sed -n '/^# Usage:/,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' | sed '$d'
}

fail() {
  echo "shepherd-release-metadata.sh: $*" >&2
  exit 1
}

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

dist=
version=
commit=
platform=
arch=
out=
check=
expects=()

while [ $# -gt 0 ]; do
  case "$1" in
    --dist) dist=${2:?--dist needs a directory}; shift 2 ;;
    --version) version=${2:?--version needs a value}; shift 2 ;;
    --commit) commit=${2:?--commit needs a value}; shift 2 ;;
    --platform) platform=${2:?--platform needs a value}; shift 2 ;;
    --arch) arch=${2:?--arch needs a value}; shift 2 ;;
    --expect) expects+=("${2:?--expect needs an extension}"); shift 2 ;;
    --out) out=${2:?--out needs a path}; shift 2 ;;
    --check) check=${2:?--check needs a directory}; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$version" ] || fail "--version is required"
[ -n "$commit" ] || fail "--commit is required"

if [ -n "$check" ]; then
  [ -d "$check" ] || fail "no metadata directory: $check"
  metadata=()
  while IFS= read -r file; do
    metadata+=("$file")
  done < <(find "$check" -maxdepth 1 -name 'release-metadata-*.json' -print | LC_ALL=C sort)
  [ "${#metadata[@]}" -eq 5 ] || fail "expected 5 target metadata files, found ${#metadata[@]}"
  for file in "${metadata[@]}"; do
    grep -Fq "\"version\": \"$version\"" "$file" || fail "$file has another version"
    grep -Fq "\"commit\": \"$commit\"" "$file" || fail "$file has another commit"
    sums="${file%.json}.sha256"
    [ -f "$sums" ] || fail "missing checksums beside $file"
    while IFS= read -r line; do
      [ -n "$line" ] || continue
      expected=${line%%  *}
      name=${line#*  }
      [ -f "$check/$name" ] || fail "missing artifact $name"
      actual=$(hash_file "$check/$name")
      [ "$actual" = "$expected" ] || fail "checksum mismatch for $name"
    done < "$sums"
  done
  echo "verified ${#metadata[@]} targets at $commit"
  exit 0
fi

[ -n "$dist" ] || fail "--dist is required"
[ -d "$dist" ] || fail "no artifact directory: $dist"
[ -n "$platform" ] || fail "--platform is required"
[ -n "$arch" ] || fail "--arch is required"
[ -n "$out" ] || fail "--out is required"
[ "${#expects[@]}" -gt 0 ] || fail "at least one --expect is required"

artifacts=()
while IFS= read -r artifact; do
  artifacts+=("$artifact")
  done < <(
  find "$dist" -maxdepth 1 -type f \
    ! -name 'release-metadata-*' ! -name '*.sha256' \
    -print | LC_ALL=C sort
)
[ "${#artifacts[@]}" -gt 0 ] || fail "no artifacts in $dist"

for extension in "${expects[@]}"; do
  found=0
  for artifact in "${artifacts[@]}"; do
    case "$artifact" in
      *."$extension") found=1 ;;
    esac
  done
  [ "$found" -eq 1 ] || fail "no .$extension artifact in $dist"
done

sums="${out%.json}.sha256"
: > "$sums"
{
  printf '{\n'
  printf '  "version": "%s",\n' "$version"
  printf '  "commit": "%s",\n' "$commit"
  printf '  "platform": "%s",\n' "$platform"
  printf '  "arch": "%s",\n' "$arch"
  printf '  "artifacts": [\n'
  first=1
  for artifact in "${artifacts[@]}"; do
    name=$(basename "$artifact")
    hash=$(hash_file "$artifact")
    bytes=$(wc -c < "$artifact" | tr -d '[:space:]')
    printf '%s  %s\n' "$hash" "$name" >> "$sums"
    if [ "$first" -eq 0 ]; then
      printf ',\n'
    fi
    first=0
    printf '    {"name": "%s", "sha256": "%s", "bytes": %s}' "$name" "$hash" "$bytes"
  done
  printf '\n  ]\n'
  printf '}\n'
} > "$out"

echo "recorded ${#artifacts[@]} artifacts for $platform-$arch in $out"
