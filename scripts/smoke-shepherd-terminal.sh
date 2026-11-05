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
  --codex-edit)
    command -v python3 >/dev/null
    phases=(codex-edit)
    export SHEPHERD_DEV_CODEX_SMOKE=worktree-edit
    ;;
  --codex-restart)
    command -v python3 >/dev/null
    phases=(codex-before-restart codex-after-restart)
    export SHEPHERD_DEV_CODEX_SMOKE=restart
    ;;
  --codex-exit)
    command -v python3 >/dev/null
    phases=(codex-exit)
    export SHEPHERD_DEV_CODEX_SMOKE=mid-turn-exit
    ;;
  --codex-review-merge)
    command -v python3 >/dev/null
    phases=(codex-review-merge reopen-archived)
    duration=240s
    export SHEPHERD_DEV_CODEX_SMOKE=review-merge
    ;;
  *)
    printf '%s\n' "Usage: $0 [--codex-approval|--codex-live|--codex-edit|--codex-restart|--codex-exit|--codex-review-merge]" >&2
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
if [[ ${SHEPHERD_DEV_CODEX_SMOKE:-} == worktree-edit || ${SHEPHERD_DEV_CODEX_SMOKE:-} == review-merge ]]; then
  printf '%s\n' 'first line' > "$fixture/project/note.txt"
  git -C "$fixture/project" add -- note.txt
fi
if [[ ${SHEPHERD_DEV_CODEX_SMOKE:-} == review-merge ]]; then
  printf '%s\n' 'outline start' > "$fixture/project/outline.txt"
  git -C "$fixture/project" add -- outline.txt
  git -C "$fixture/project" config --local user.name "Shepherd Review Smoke"
  git -C "$fixture/project" config --local user.email "smoke@example.invalid"
  git -C "$fixture/project" config --local commit.gpgsign false
  git -C "$fixture/project" config --local core.hooksPath /dev/null
fi
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
if [[ -z ${SHEPHERD_DEV_CODEX_SMOKE:-} ]]; then
  mkdir "$fixture/bin"
  cat > "$fixture/bin/open" <<'EOF'
#!/bin/sh
printf '%s\n' "$1" >> "$SHEPHERD_SMOKE_OPEN_LOG"
EOF
  chmod +x "$fixture/bin/open"
  ln -s open "$fixture/bin/xdg-open"
  export SHEPHERD_SMOKE_OPEN_LOG="$fixture/open.log"
  export PATH="$fixture/bin:$PATH"
fi

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
  if [[ $phase == codex-review-merge ]]; then
    worktrees=("$fixture/project/.git/shepherd/worktrees/"*)
    [[ ${#worktrees[@]} == 1 && -d ${worktrees[0]} ]]
    task_worktree=${worktrees[0]}
    task_branch=$(git -C "$task_worktree" symbolic-ref HEAD)
    task_tip=$(git -C "$task_worktree" rev-parse HEAD)
    git -C "$fixture/project" worktree remove -- "$task_worktree"
    [[ ! -e $task_worktree && $(git -C "$fixture/project" rev-parse "$task_branch") == "$task_tip" ]]
  fi
done
if [[ -z ${SHEPHERD_DEV_CODEX_SMOKE:-} ]]; then
  [[ $(cat "$SHEPHERD_SMOKE_OPEN_LOG") == 'https://example.invalid/shepherd-terminal' ]]
fi
if [[ ${SHEPHERD_DEV_CODEX_SMOKE:-} == review-merge ]]; then
  [[ $(git -C "$fixture/project" rev-list --count main) == 2 ]]
  [[ $(git -C "$fixture/project" show -s --format=%P main) == "$(git -C "$fixture/project" rev-parse main~1)" ]]
  [[ $(git -C "$fixture/project" show main:note.txt) == $'first line\nreviewed by shepherd' ]]
  [[ $(git -C "$fixture/project" show main:outline.txt) == $'outline start\noutline from shepherd' ]]
  branch=$(git -C "$fixture/project" for-each-ref --format='%(refname)' refs/heads/shepherd)
  [[ $branch == "$task_branch" && $(git -C "$fixture/project" show -s --format=%s "$branch") == 'Apply the review feedback' ]]
  [[ $(git -C "$task_worktree" symbolic-ref HEAD) == "$task_branch" ]]
  [[ $(git -C "$task_worktree" rev-parse HEAD) == "$task_tip" ]]
fi
