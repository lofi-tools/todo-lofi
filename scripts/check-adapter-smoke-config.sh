#!/usr/bin/env bash
set -euo pipefail

agent=${1:-}
case "$agent" in
  opencode|claude-acp)
    variable=SHEPHERD_ADAPTER_TEST_SETTINGS
    ;;
  pi)
    variable=SHEPHERD_PI_TEST_EXECUTABLE
    ;;
  *)
    printf '::error::unknown adapter smoke %q; expected opencode, pi or claude-acp\n' "$agent" >&2
    exit 2
    ;;
esac

value=${!variable:-}
if [[ -z $value ]]; then
  printf '::error::%s is required for the %s smoke; configure the repository variable before dispatching\n' \
    "$variable" "$agent" >&2
  exit 1
fi

if [[ $variable == SHEPHERD_ADAPTER_TEST_SETTINGS ]]; then
  if [[ ! -r $value ]]; then
    printf '::error::%s must point to a readable settings.json on the runner: %s\n' \
      "$variable" "$value" >&2
    exit 1
  fi
else
  if [[ ! -x $value ]]; then
    printf '::error::%s must point to an executable on the runner: %s\n' \
      "$variable" "$value" >&2
    exit 1
  fi
fi

printf 'adapter smoke configuration is present for %s\n' "$agent"
