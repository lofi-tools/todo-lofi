#!/usr/bin/env bash
set -euo pipefail

command -v vim less python3 >/dev/null

repo=$(cd "$(dirname "$0")/.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
fixture=$(cd "$fixture" && pwd -P)
mkdir "$fixture/project"

export SHEPHERD_DEV_DATA_DIR="$fixture/data"
export SHEPHERD_DEV_PROJECT_ROOT="$fixture"
export SHEPHERD_DESKTOP_SMOKE_PROJECT="$fixture/project"
export SHELL=/bin/sh
export CARGO_NET_OFFLINE=true

cd "$repo"
timeout --signal=INT --kill-after=10s 180s pnpm --dir apps/shepherd exec tauri dev --no-watch --config '{
  "build": {
    "beforeDevCommand": "pnpm --dir ui run dev --port 1432",
    "devUrl": "http://localhost:1432"
  },
  "app": {
    "windows": [{
      "label": "main",
      "title": "Shepherd Terminal Smoke",
      "width": 1280,
      "height": 820
    }]
  }
}' 2>&1 | tee "$fixture/run.log"
rg -q 'SHEPHERD_TERMINAL_SMOKE \{"ok":true' "$fixture/run.log"
