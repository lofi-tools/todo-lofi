# Rust coding guidelines

* Prioritize code correctness and clarity. Speed and efficiency are secondary priorities unless otherwise specified.
* Do not write organizational or comments that summarize the code. Comments should only be written in order to explain "why" the code is written in some way in the case there is a reason that is tricky / non-obvious.
* Prefer implementing functionality in existing files unless it is a new logical component. Avoid creating many small files.
* Avoid using functions that panic like `unwrap()`, instead use mechanisms like `?` to propagate errors.
* Be careful with operations like indexing which may panic if the indexes are out of bounds.
* Never silently discard errors with `let _ =` on fallible operations. Always handle errors appropriately:
  - Propagate errors with `?` when the calling function should handle them
  - Use `.log_err()` or similar when you need to ignore errors but want visibility
  - Use explicit error handling with `match` or `if let Err(...)` when you need custom logic
  - Example: avoid `let _ = client.request(...).await?;` - use `client.request(...).await?;` instead
* When implementing async operations that may fail, ensure errors propagate to the UI layer so users get meaningful feedback.
* Never create files with `mod.rs` paths - prefer `src/some_module.rs` instead of `src/some_module/mod.rs`.
* When creating new crates, prefer specifying the library root path in `Cargo.toml` using `[lib] path = "...rs"` instead of the default `lib.rs`, to maintain consistent and descriptive naming (e.g., `gpui.rs` or `main.rs`).
* Avoid creative additions unless explicitly requested
* Use full words for variable names (no abbreviations like "q" for "queue")
* Use variable shadowing to scope clones in async contexts for clarity, minimizing the lifetime of borrowed references.
  Example:
  ```rust
  executor.spawn({
      let task_ran = task_ran.clone();
      async move {
          *task_ran.borrow_mut() = true;
      }
  });
  ```

# Build guidelines

Tests that call a real agent must cache successful responses and external
effects that the assertions depend on, such as worktree edits. Replay the
capture on subsequent runs. Keep process or provider probes that require the
real implementation behind a per-agent feature that is disabled by default.
Live Codex tests using a ChatGPT plan must use `gpt-6.1-terra` at `low` effort.
Use `LiveCodexRun` to record under `.cache/shepherd-live/`. A cache miss
requires the `live-codex` feature to record.

Before each live Freebuff submission, ask the user to confirm the exact test
message, project/worktree, and files or other context that may be sent. Use
only a small number of useful tasks related to one of their projects because
Freebuff is metered. Cache every response, including partial responses, raw PTY
bytes, terminal-size changes, rendered terminal-state transitions in order, and
assertion-relevant effects under the repository `.cache/` for replay. Keep live
Freebuff probes behind the non-default
`live-freebuff` feature; default tests must not make live agent calls.

Run workspace commands through the repository's Nix direnv environment, not
the ambient shell:

```sh
direnv exec . cargo fmt --check
direnv exec . cargo test -p agent-harness
direnv exec . cargo clippy --workspace -- -D warnings
```

The bare shell may expose a different Rust toolchain and does not have
`cargo fmt`. If direnv reports that `.envrc` is blocked, run `direnv allow .`
once from the repository root. In a non-interactive runner without direnv, use
`nix develop -c <command>` instead.

<!--# Pull request hygiene

When an agent opens or updates a pull request, it must:

- Use a clear, correctly capitalized, imperative PR title (for example, `Fix crash in project panel`).
- Avoid conventional commit prefixes in PR titles (`fix:`, `feat:`, `docs:`, etc.).
- Avoid trailing punctuation in PR titles.
- Optionally prefix the title with a crate name when one crate is the clear scope (for example, `git_ui: Add history view`).
- Include a `Release Notes:` section as the final section in the PR body.
- Use one bullet under `Release Notes:`:
  - `- Added ...`, `- Fixed ...`, or `- Improved ...` for user-facing changes, or
  - `- N/A` for docs-only and other non-user-facing changes.
- Format release notes exactly with a blank line after the heading, for example:

```
Release Notes:

- N/A
```-->



# Crash Investigation

 <!--TODO-->

# Reference repositories

For named reference repositories, use
[reference-repos](docs/agent_skills/reference-repos.md). Run `skills` in the Nix
dev shell to install repository skills under `.agents/skills`.

# Shepherd workspace UI

Before changing Shepherd's desktop UI, read the agreed
[workspace UI decisions](apps/shepherd/docs/spec/workspace-ui.md). Preserve
them while completing the implementation task list. If a task requires a
substantial change to a documented element's layout, visual hierarchy, controls,
or default behaviour, describe the proposed change and ask the user before
implementing it, unless the user has already requested that specific change.
Update the spec when the agreed behaviour changes.

Use the background macOS smoke for routine native checks. Run a visible
macOS smoke only when the current change affects the behavior that check
exercises or the user requests it. Keep test windows at the
normal desktop level. The [Shepherd README](apps/shepherd/README.md) lists
the flags for window, visibility and IME checks.

## Routine verification

Verify only the affected area. For a change confined to one module, run that
module's tests with a filter, the single web or browser test that covers it, or
a typecheck of the touched package. A whole-crate `cargo test -p <crate>`, a
chain of several crates' suites, or a whole browser suite is not routine
verification: it is slow, it reports failures unrelated to the change, and it
buries the result that matters.

Widen the check when the change genuinely spans those areas, when a focused
check fails and the surrounding suites are needed to explain it, when a commit
or push gate requires it, or when the user asks.

<!-- my-nix:agents-fragment:start commit-messages -->
## Commit messages

Use the `commit-messages` skill to write commit messages: a `<short affected area>: <extremely terse description>` subject, with any needed rationale in the body.
<!-- my-nix:agents-fragment:end commit-messages -->

<!-- my-nix:agents-fragment:start test-sharding -->
## Routine test selection

For routine tests, use only `direnv exec . shepherd-test`. It runs
default unit tests for directly changed crates and web packages. Shared or
unknown paths select all supported workspace unit suites.
For Git-backed Shepherd changes, pass `--git` to the same command to
include the selected crates' opt-in Git tests.
Before every commit, run `direnv exec . shepherd-test --pre-commit`;
it runs one pseudo-randomly rotating shard of the workspace's default-enabled
unit tests, without doctests or browser suites.
Run `direnv exec . python3 scripts/test-shards.py --all` every 16 commits,
before a push, and in CI; this includes doctests. Browser suites are opt-in
with `--all --browser`.
If a pre-commit or comprehensive run fails, bisect only the failing tests from a
known-good revision in an isolated checkout, then apply or propose a fix.
If no reliable good revision is available, report that limit.
Follow explicit requests for different checks and preserve live-agent
and native-window gates. See `apps/shepherd/README.md` for details.
<!-- my-nix:agents-fragment:end test-sharding -->
