#!/usr/bin/env bash
set -euo pipefail

phases=(prepare restore)
duration=180s
unset SHEPHERD_DEV_CODEX_SMOKE
case "${1:-}" in
  "")
    command -v vim less python3 >/dev/null
    export SHEPHERD_DEV_SCRIPTED_HARNESS=1
    ;;
  --codex-approval)
    command -v python3 >/dev/null
    phases=(codex-approval)
    export SHEPHERD_DEV_CODEX_SMOKE=approval-replay
    ;;
  --codex-live)
    command -v python3 >/dev/null
    phases=(codex-live)
    duration=300s
    export SHEPHERD_DEV_CODEX_SMOKE=approval-cancel
    ;;
  *)
    printf '%s\n' "Usage: $0 [--codex-approval|--codex-live]" >&2
    exit 2
    ;;
esac

port=${SHEPHERD_DESKTOP_SMOKE_PORT:-1432}
if [[ ! $port =~ ^[1-9][0-9]{3,4}$ ]] || (( port < 1024 || port > 65535 )); then
  printf '%s\n' 'SHEPHERD_DESKTOP_SMOKE_PORT must be between 1024 and 65535' >&2
  exit 2
fi

repo=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$repo/.cache/test-db"
fixture=$(mktemp -d "$repo/.cache/test-db/smoke.XXXXXX")
trap 'rm -rf -- "$fixture"' EXIT
fixture=$(cd "$fixture" && pwd -P)
mkdir "$fixture/project"
git -C "$fixture/project" -c init.templateDir= init --quiet -b main
git -C "$fixture/project" -c core.hooksPath=/dev/null -c commit.gpgsign=false \
  -c user.name="Shepherd Terminal Smoke" -c user.email="smoke@example.invalid" \
  commit --quiet --allow-empty -m "Seed disposable repository"

export SHEPHERD_DEV_DATA_DIR="$fixture/data"
export SHEPHERD_DEV_PROJECT_ROOT="$fixture"
export SHEPHERD_DESKTOP_SMOKE_PROJECT="$fixture/project"
export SHEPHERD_DESKTOP_SMOKE_FISH=0
if fish_version=$(fish --version 2>/dev/null) && [[ $fish_version =~ ^fish,\ version\ ([0-9]+)\. ]] && (( BASH_REMATCH[1] >= 4 )); then
  export SHEPHERD_DESKTOP_SMOKE_FISH=1
fi
export SHELL=/bin/sh
export CARGO_NET_OFFLINE=true

cd "$repo"
config=$(cat <<EOF
{
  "build": {
    "beforeDevCommand": "pnpm --dir ui run dev --port $port",
    "devUrl": "http://localhost:$port"
  }
}
EOF
)
launch=(pnpm --dir apps/shepherd exec tauri dev --no-watch --config "$config")
if [[ -n ${SHEPHERD_DEV_CODEX_SMOKE:-} ]]; then
  launch+=(--features desktop-codex-smoke)
fi
for phase in "${phases[@]}"; do
  SHEPHERD_DESKTOP_SMOKE_PHASE="$phase" timeout --signal=INT --kill-after=10s "$duration" "${launch[@]}" 2>&1 | tee "$fixture/$phase.log"
  rg -q 'SHEPHERD_TERMINAL_SMOKE \{"ok":true' "$fixture/$phase.log"
done
