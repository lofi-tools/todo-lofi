#!/usr/bin/env bash
set -euo pipefail

phases=(prepare restore)
duration=180s
window_config=''
platform=$(uname -s)
unset SHEPHERD_DEV_CODEX_SMOKE SHEPHERD_DEV_BACKGROUND_SMOKE
case "${1:-}" in
  ""|--macos-window)
    if [[ ${1:-} == --macos-window && $platform != Darwin ]]; then
      printf '%s\n' 'The macOS window smoke requires a macOS desktop session.' >&2
      exit 2
    fi
    required_tools=(python3)
    if [[ $platform != Darwin || ${1:-} == --macos-window ]]; then
      required_tools+=(vim less fzf)
    fi
    for tool in "${required_tools[@]}"; do
      if ! command -v "$tool" >/dev/null; then
        printf 'Shepherd terminal smoke requires %s on PATH\n' "$tool" >&2
        exit 1
      fi
    done
    export SHEPHERD_DEV_SCRIPTED_HARNESS=1
    ;;
  --linux-ime)
    if [[ $platform != Linux || ! -r ${SHEPHERD_SMOKE_IBUS_MODULE:-} ]]; then
      printf '%s\n' 'The native IME smoke requires Linux and SHEPHERD_SMOKE_IBUS_MODULE pointing to the GTK 3 IBus module.' >&2
      exit 2
    fi
    phases=(linux-ime)
    export SHEPHERD_DEV_SCRIPTED_HARNESS=1
    ;;
  --macos-ime)
    if [[ $platform != Darwin ]]; then
      printf '%s\n' 'The macOS IME smoke requires a macOS desktop session.' >&2
      exit 2
    fi
    command -v python3 >/dev/null
    duration=300s
    export SHEPHERD_DEV_SCRIPTED_HARNESS=1
    case "${2:-pinyin}" in
      pinyin)
        phases=(macos-ime)
        printf '%s\n' 'Use Pinyin - Simplified. Wait for each IME prompt before typing ni.' \
          'At IME1>, check the candidate panel position, then press Space and Return.' \
          'At IME2>, check the new position, select a different candidate, then press Return to submit it.'
        ;;
      british)
        phases=(macos-dead-key)
        printf '%s\n' 'Use the British layout. Wait for each IME prompt before typing.' \
          'At IME1>, press Option+E, then E and Return.' \
          'At IME2>, press Option+U, then U and Return.' \
          'At IME3>, press Option+3 and Return.'
        ;;
      *)
        printf '%s\n' 'Usage: --macos-ime [pinyin|british]' >&2
        exit 2
        ;;
    esac
    ;;
  --macos-visibility)
    if [[ $platform != Darwin ]]; then
      printf '%s\n' 'The window visibility smoke requires a macOS desktop session.' >&2
      exit 2
    fi
    phases=(macos-visibility)
    window_config=$(cat <<'EOF'
  "app": {
    "security": {
      "capabilities": ["default", {
        "identifier": "terminal-visibility-smoke",
        "windows": ["main"],
        "permissions": ["core:window:allow-minimize", "core:window:allow-unminimize"]
      }]
    }
  },
EOF
    )
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
    printf '%s\n' "Usage: $0 [--macos-window|--linux-ime|--macos-ime [pinyin|british]|--macos-visibility|--codex-approval|--codex-live|--codex-edit|--codex-restart|--codex-exit|--codex-review-merge] [--record for Codex scenarios]" >&2
    exit 2
    ;;
esac

codex_feature=desktop-codex-smoke
if [[ -n ${SHEPHERD_DEV_CODEX_SMOKE:-} && -n ${2:-} ]]; then
  if [[ $2 != --record || $SHEPHERD_DEV_CODEX_SMOKE == approval-replay || $# != 2 ]]; then
    printf '%s\n' 'Codex smoke accepts --record only for scenarios other than --codex-approval.' >&2
    exit 2
  fi
  codex_feature=live-codex
fi

if [[ $platform == Darwin && ( -z ${1:-} || ${1:-} == --codex-* ) ]]; then
  export SHEPHERD_DEV_BACKGROUND_SMOKE=1
  window_config=$(cat <<'EOF'
  "app": {
    "windows": [{
      "label": "main",
      "visible": false,
      "focus": false,
      "focusable": false,
      "backgroundThrottling": "disabled"
    }]
  },
EOF
  )
fi

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
if [[ ${phases[0]} == prepare ]]; then
  printf '%s\n' 'Shepherd workspace search fixture' > "$fixture/project/workspace-guide.md"
  git -C "$fixture/project" add -- workspace-guide.md
fi
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
if [[ ${phases[0]} == prepare ]]; then
  mkdir "$fixture/second-project"
fi
if [[ ${phases[0]} == prepare && ${SHEPHERD_DEV_BACKGROUND_SMOKE:-} != 1 ]]; then
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
$window_config
  "build": {
    "beforeDevCommand": "pnpm --dir ui run dev --port $port",
    "devUrl": "http://localhost:$port"
  }
}
EOF
)
launch=(pnpm --dir apps/shepherd exec tauri dev --no-watch --config "$config")
if [[ -n ${SHEPHERD_DEV_CODEX_SMOKE:-} ]]; then
  launch+=(--features "$codex_feature")
fi
for phase in "${phases[@]}"; do
  if [[ $phase == linux-ime ]]; then
    SHEPHERD_DESKTOP_SMOKE_PHASE="$phase" XDG_DATA_DIRS=/usr/share:/usr/local/share \
      /usr/bin/dbus-run-session --dbus-daemon=/usr/bin/dbus-daemon -- \
      xvfb-run -a -s '-screen 0 1600x1000x24' /usr/bin/python3 "$repo/scripts/smoke-shepherd-ime.py" "$fixture" "${launch[@]}"
    continue
  fi
  SHEPHERD_DESKTOP_SMOKE_PHASE="$phase" timeout --signal=INT --kill-after=10s "$duration" "${launch[@]}" 2>&1 | tee "$fixture/$phase.log"
  rg -q 'SHEPHERD_TERMINAL_SMOKE \{"ok":true' "$fixture/$phase.log"
  if [[ $phase == prepare ]]; then
    for job in stop-foreground stop-background quit-foreground quit-background; do
      job_pid=$(cat "$fixture/$job.pid")
      [[ $job_pid =~ ^[1-9][0-9]+$ ]]
      for ((attempt = 0; attempt < 20; attempt++)); do
        job_state=$(ps -A -o pid=,stat= | awk -v pid="$job_pid" '$1 == pid { print $2 }')
        if [[ -z $job_state || $job_state == Z* ]]; then
          break
        fi
        sleep 0.1
      done
      if [[ -n $job_state && $job_state != Z* ]]; then
        printf 'PTY fixture job %s (%s) survived shutdown\n' "$job_pid" "$job" >&2
        exit 1
      fi
    done
  fi
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
if [[ ${phases[0]} == prepare && ${SHEPHERD_DEV_BACKGROUND_SMOKE:-} != 1 ]]; then
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
