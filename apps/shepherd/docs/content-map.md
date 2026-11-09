# Shepherd content map

Status: current audit of the public site and the feature references. This is an
internal planning document, not a public page.

The public split now lives at `/docs/users` and `/docs/contributors` in
`apps/shep-docs-web`. This map classifies each capability the references expose,
and each shipped Shepherd capability, so a feature has one intended home:

- **Landing argument** — a promise on `/`. It must be understandable without
  setup and must distinguish available from next or planned.
- **User guide** — a page under `/docs/users`. It needs installation,
  configuration, a decision, a workflow or a recovery step.
- **Contributor note** — a page under `/docs/contributors`. It describes an
  internal contract, protocol, security boundary, release gate or test setup.
- **Not public** — not shipped, not Shepherd's scope, or useful only as research
  input. It must not appear as a product promise.

## Rules

1. The landing page leads with the individual-developer promise, not the stack.
2. A planned capability is labelled **next** or **planned** wherever it appears,
   and never described as available.
3. A user page may name an agent the user installs (for example Codex, OpenCode
   or Claude Code) but not an internal reference project.
4. No public comparison table, feature count or competing-app name. The
   reference projects are research inputs only.
5. An internal protocol, token, ledger or fixture detail belongs in the
   contributor guide unless the user must act on it (for example, do not share a
   preview wrapper URL).
6. A capability that Shepherd does not have gets no public page merely because a
   reference project has it.

## Reference feature audit

The source is `apps/shepherd/docs/references/by-feature`. The table names feature
areas, not reference products, and keeps the conclusion that matters for public
copy.

| Reference area | Capability observed | Placement | Why it is not a landing promise |
| --- | --- | --- | --- |
| Layout and navigation | One window with a project sidebar, active work area, inspector and restorable layout | **L** one workspace; **U** mental model | The layout needs the task/worktree model to make sense. |
| Layout and navigation | Keyboard-first commands, palette, task search | **U** mental model and troubleshooting | An efficiency detail, not the first promise. |
| Layout and navigation | Split panes, peek, movable panes, board columns | **U** mental model (terminal splits only); **X** board/peek | Shepherd has fixed regions and a flat task list; it does not ship a board, detachable panes or peek. |
| Terminals | Real PTYs, scrollback, copy/paste, search, resize | **L** terminals; **U** mental model | Table stakes for the workspace; the promise is continuity. |
| Terminals | Shared and task-specific terminals survive task switches; quit ends them | **L** terminals; **U** mental model and troubleshooting | This is a differentiator, but the honest quit behavior is part of the promise. |
| Terminals | Terminal graphics, image paste, keep/park/pin, remote attach | **X** not shipped or not in v1 | No public promise until the behavior exists; remote terminals are out of scope. |
| Agents and status | Several agents in parallel with derived working/waiting/review states | **L** several agents; **U** mental model | The core promise, but the state names need context. |
| Agents and status | Approvals that name the tool, arguments and affected diff | **L** approvals; **U** mental model | The safety model is a promise; the decision rules are user-guide detail. |
| Agents and status | Adapter tiers and structured protocol events | **C** adapter contracts; **U** install/configuration only at the executable level | Users need to know an agent is supported, not how the event loop is assembled. |
| Agents and status | Agent teams, automatic model routing, subagent tracking | **X** not shipped | Shepherd does not track subagents as tasks and has no agent-team orchestration. |
| Agents and status | Usage and cost display | **X** not shipped; **C** quota ledger is a planned contract | The pool is next, not current; cost display is not a v1 claim. |
| Work management | Durable task records with project, branch, worktree and transcript | **L** worktree per task; **U** mental model | The task is the product's unit, but a landing page should lead with what the user gets. |
| Work management | Queueing, steering, cancel, review, archive, resume | **U** mental model and troubleshooting; **L** review promise | Useful after the first task; too much detail for the hero. |
| Work management | Board, schedules, goals, rewind, side threads | **X** not shipped | No public page for an unstarted workflow. |
| Files and previews | Read-only file tree, changed-file marks, diffs | **U** mental model; **L** review promise | The diff is a promise; the file-tree controls are workflow detail. |
| Files and previews | Browser preview with element annotation and screenshot fallback | **L** preview; **U** preview; **C** annotation API | This is a headline capability; its protocol and bounds are contributor-only. |
| Files and previews | Design canvas, moodboards, simulators, VMs, PDF/image preview | **X** not shipped | Shepherd is not a design tool, simulator host or VM surface. |
| Git, machines and cloud | Worktree per task, status, stage, commit, squash-merge | **L** worktrees and review; **U** mental model | The Git workflow is central; the step-by-step controls are user-guide detail. |
| Git, machines and cloud | Branch display and checkout; no push/pull/rebase/PR | **U** mental model | Scope belongs in the guide, not the promise. |
| Git, machines and cloud | SSH projects, cloud sandboxes, GitHub PRs, deploys | **X** not Shepherd v1 | Local directories only; no hosted sandbox, deploy pipeline or PR surface. |
| Customization | Themes, density, appearance, rebindable keys, settings files | **U** configuration; **L** local-first/quiet only | Preferences are not a reason to choose the app. |
| Customization | Programmable UI, mods, wallpapers, theme import | **X** not shipped or not Shepherd | No plugin API, no programmable UI and no competing theme catalog. |
| Lifecycle and automation | Honest quit, interrupted tasks, resume, ended terminals, crash PID check | **L** honest lifecycle; **U** mental model and troubleshooting | The promise is that the app does not pretend work survived; the recovery steps are guide detail. |
| Lifecycle and automation | CLI, headless server, self-update, schedules | **X** not shipped | No CLI in v1, one window only, no updater. The release gate is contributor-only. |
| Lifecycle and automation | No telemetry, no account, local logs | **L** local-first; **U** configuration and troubleshooting | The promise is local-first; the log path is recovery detail. |
| Stack | Tauri 2, SolidJS, Rust core, Turso, terminal engine, GPU shell plan | **C** contributor overview and test fixtures | The stack is not the user's reason to choose Shepherd; v2 is unscheduled. |

## Shepherd-only surfaces

| Capability | Classification | Public treatment |
| --- | --- | --- |
| Quota-aware agent pool | **L** next; **U** agent pool; **C** quota ledger | The landing page says next. The user page says management exists but task selection does not. The contributor page documents slots, the ledger, availability, selection, idle release, the Settings status/reorder/enable/correction/cooldown/force UI, catalog snapshot limit seeding and provider-owned reset windows (Codex 5-hour and weekly, rolled from the last observed use); dual-window enforcement and task-start wiring are pending. |
| Signed macOS/Linux artifacts | **L** next; **U** releases; **C** release pipeline | No signed release yet. The release gate has not been run; no downloads route. |
| Windows support and installer | **L** planned; **U** releases | Planned with no date; no setup page. |
| Preview annotation wrapper | **U** preview (user warning); **C** annotation API (token, claim, origin, validation) | The user page explains the capability and the sharing warning; the protocol is contributor-only. |
| Release and test pipeline | **C** release pipeline and test fixtures | Never a landing argument. |
| Docs search and roadmap data | **X** not yet built; **U/C** later site items | Do not claim search or a generated roadmap until those items ship. |

## Landing-page arguments

The current `/` page carries these claims:

1. **Several coding agents, one workspace.** Parallel tasks across projects, with
   one task list and a shared inspector.
2. **A worktree per task.** Agents do not edit the same checkout; review and
   commit stay attached to the task.
3. **Approvals in context.** The tool, arguments, paths and affected diff are in
   the decision.
4. **Preview and annotation.** Select the element, write the change, send
   structured context; screenshot fallback when injection is blocked.
5. **Terminals that survive a task switch.** Project-shared and task-specific
   terminals; quit ends them honestly.
6. **Local-first and quiet.** Projects, worktrees, transcripts and settings stay
   on the machine; no account or telemetry.
7. **Honest lifecycle.** Quitting stops the work and the next launch offers
   resume rather than claiming it survived.

**Next** on the landing page is limited to the agent pool and signed release
artifacts. **Planned** is Windows support. Everything else in this map is either
a guide page, a contributor note, or not public.

## User guide map

| Page | Covers | Does not cover |
| --- | --- | --- |
| `/docs/users` | Promise, mental model summary, available/next/planned split, reading order | Protocol details, release internals |
| `/docs/users/install` | Platforms, dev shell, test mode, bundle build, agent install, first project | Adapter internals, signing |
| `/docs/users/configuration` | Settings files, agent executables, launch environment, limits, preview URL, appearance, logs | Token protocol, ledger schema |
| `/docs/users/mental-model` | Project/task/worktree, states, inspector, terminals, approvals, review, quit | Adapter event contract |
| `/docs/users/preview` | Configure, annotate, add to prompt, wrapper warning, screenshot fallback, bounds | Wrapper HTTP validation and tests |
| `/docs/users/agent-pool` | Next status, current management UI including corrections/cooldown, selection promise, visibility, limits | Ledger persistence, correction and selection tests |
| `/docs/users/roadmap` | Now/Next/Later/Exploring, definitions, links and the shipped boundary | Individual delivery dates or unpromised support |
| `/docs/users/releases` | No signed release, source build, platforms, clean-machine gate, signing | Workflow file details |
| `/docs/users/troubleshooting` | Agent discovery, auth, fallback, preview, Git, terminals, settings and logs | Implementation fixes |

## Contributor guide map

| Page | Covers | Why it is not user-facing |
| --- | --- | --- |
| `/docs/contributors` | Crate boundaries, runtime shape, status, reading order, ground rules | It is a map of the code, not a product promise. |
| `/docs/contributors/adapter-contracts` | Tiers, trait, events, turn ownership, capabilities, approvals, failures, contract tests | Adapter authors need the event and capability contract. |
| `/docs/contributors/annotation-api` | Shared payload, bounds, wrapper security, prompt assembly, tests | Token, claim and origin rules are an implementation boundary. |
| `/docs/contributors/quota-ledger` | Slot, ledger, availability, selection, idle release, Settings status/reorder/enable/correction/cooldown/force, catalog snapshot limit seeding and provider reset windows implemented; dual-window enforcement, snapshot refresh and task-start wiring are planned | Core code, UI code and tests exist; the remaining parts are a design contract. |
| `/docs/contributors/release-pipeline` | Build inputs, smoke workflows, clean-machine gate, artifact checks, signing | Packaging and release evidence are not a user workflow. |
| `/docs/contributors/test-fixtures` | Test layers, recorded fixtures, fake harness, UI, live smokes, docs tests, rules | Fixture strategy is only useful to contributors. |

## Exclusions

- **Not shipped:** board/peek/detachable panes, agent teams, schedules, goals,
  rewind, design canvas, simulators, VMs, SSH projects, cloud sandboxes, PRs,
  deploys, updater, CLI, Windows installer.
- **Not Shepherd's scope:** model provider, hosted sandbox, account system,
  telemetry, code editor, LSP, debugger, plugin API, background daemon.
- **Research-only:** reference-project names, comparison tables, feature counts,
  screenshots and implementation details copied from another product.
- **Later site work:** docs search, the generated roadmap page, the downloads
  page, and tests for every public claim.

## Open gaps

- The agent-pool user page and quota-ledger contributor page are intentionally
  marked planned; the split and map do not make them shipped.
- The release page says no signed installer and the release gate is pending. Do
  not add a download link until the release item supplies one.
- The roadmap page renders from `data/roadmap.ts`; that file is the only public
  item list. Keep the landing availability columns derived from it. Site tests
  also derive restricted claim terms and non-shipped markers from that file and
  check every public route, so do not add a page-specific availability list.
- Docs search is not implemented. When it is, it must index the same
  `src/data/docs.ts` navigation and not become a second page inventory.

## Review triggers

Update this map when any of these changes:

- A task moves from next/planned to shipped.
- A public page is added, removed or changes audience.
- The reference feature audit finds a new capability worth adopting or rejecting.
- The landing page's available/next/planned split changes.
- A release artifact or the agent pool becomes available.
