---
name: shepherd-debug
description: Investigate Shepherd desktop runtime errors using its local diagnostic history and live log stream.
---

Shepherd writes structured JSON lines to `logs/errors.jsonl` in its active data
directory. For the usual development launch, inspect
`.cache/test-db/logs/errors.jsonl` from the repository root. `shep --test` uses a
fresh launch directory under `.cache/test-db/`; use the active launch's directory.
Production uses the platform application data directory. `SHEPHERD_DEV_DATA_DIR`
overrides the development data directory.

Read recent records with `tail -n 100 <data-directory>/logs/errors.jsonl` and
follow new ones with `tail -F <data-directory>/logs/errors.jsonl`. Each record
contains an ID, timestamp, operation, message and error details. Correlate by ID
with the in-app Error history, then inspect the named bridge command and its
underlying error chain. Preserve the original log while investigating. Error
messages can contain local paths and other user data; avoid sharing them outside
the workspace without permission. A missing log means no diagnostic has yet
been recorded in that data directory, or the application could not write it.
