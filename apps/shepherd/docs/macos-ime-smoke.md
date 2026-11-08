# macOS Pinyin candidate-input smoke

Status: **not yet run**. This is the human-gated check for system candidate
input in Shepherd's terminal. It does not replace the automated suite; it
covers what only a live macOS desktop can show: the macOS candidate window
following the terminal caret while Pinyin composes text.

Run it at the commit being released on a macOS 13+ desktop.

## Prerequisites

- A macOS 13+ desktop session, not SSH, with the repository checked out in the
  Nix dev shell (`direnv allow .` once).
- `python3` on `PATH`; the smoke uses it for the disposable fixture and log
  checks.
- The system input source **Pinyin - Simplified**:
  **System Settings → Keyboard → Text Input → Input Sources → Edit → + →
  Chinese, Simplified → Pinyin - Simplified**. Keep it enabled and switch to it
  with Control+Space or the Globe key. The menu bar must show the Pinyin input
  source before starting.
- A terminal window large enough to see the caret and a multi-row candidate
  panel. Do not resize the window during the check.

## Build and launch

From the repository root:

```sh
direnv exec . ./scripts/smoke-shepherd-terminal.sh --macos-ime
```

This builds and launches the **debug Tauri dev app** with
`pnpm --dir apps/shepherd exec tauri dev --no-watch`, not the release app
bundle. It uses the scripted harness, a disposable project and database under
`.cache/test-db/`, a 300-second timeout, and writes the log to
`<fixture>/macos-ime.log`. The script prints the two prompt instructions before
the window appears; wait for each prompt instead of typing ahead.

## Procedure

1. Switch to **Pinyin - Simplified** and confirm the menu-bar input indicator.
2. Wait for `IME1>` in the Shepherd terminal, then type `ni`.
3. Check that the candidate panel opens **next to the current terminal caret**,
   inside the terminal window, and not at the screen corner or a stale caret
   position.
4. Press Space to commit the first candidate (often `你`), then press Return to
   send it to the shell.
5. Verify the shell prompt shows exactly one committed string. There must be no
   literal `ni`, no duplicated character, and the canvas and the accessibility
   mirror must show the same committed text.
6. Wait for `IME2>` after the script moves the caret, then type `ni` again.
7. Check that the panel follows the new caret position.
8. Use Down (or the number key) to select a different candidate, for example
   `尼`, press Space to commit it, then Return to submit.
9. Repeat the single-string, no-duplicate and canvas/mirror checks.

The smoke itself checks that a trusted composition event and the PTY's received
text match on both attempts. The operator checks candidate-panel placement
visually.

## Pass condition

All of the following must hold:

- the candidate panel is anchored next to the terminal caret at both positions;
- each prompt receives exactly one committed string and no stray Latin input;
- the committed text appears once on the rendered canvas and once in the
  accessibility mirror;
- the log contains `SHEPHERD_TERMINAL_SMOKE {"ok":true`.

## Record

| Commit | Date | Machine | Result | Operator |
| --- | --- | --- | --- | --- |
| | | | Pending — human-gated | |

Keep the `macos-ime.log` and a screenshot of each candidate position with the
evidence. Until this row is filled, the macOS candidate-input criterion is not
claimed as passed.

## If it fails

Keep the log and a screenshot, and record whether the candidate panel was
misplaced, the committed string was duplicated or dropped, or the composition
never reached the shell. Do not mark the criterion as passed. If the failure is
input-related rather than placement-related, reproduce it in the headless
composition regression before changing the terminal engine.
