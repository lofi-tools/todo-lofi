# todo-2 agent pane — gpui UI spec

## 0. Scope

Companion to **`docs/spec/acp-client-and-agent-panel-spec.md`**, which owns the behaviour rules
(activation, lifecycle, session continuity, permissions policy, capability scope). This file owns
the **implementation-facing UI**: component inventory, element trees, styling, interaction,
streaming and performance rules.

Where the two disagree about *what* happens, the main spec wins; this file always wins on *how it
is built*. Decisions here that resolve items listed as open in the main spec are marked
**[resolves §X.Y]**.

Pinned versions in play: `gpui = gpui-pre 0.3.4`, `gpui-component = 0.6.1`,
`gpui-component-assets = gpui-kit-assets 0.6.1`. Everything below was verified against those
sources, not from memory.

---

## 1. Verified toolkit (reuse these, don't rebuild)

### 1.1 Components that already exist and are the correct fit

| Need | Use | Notes (verified API) |
| --- | --- | --- |
| Task list ⟷ right pane split | `gpui_component::{h_resizable, resizable_panel, ResizableState}` | Re-exported from `gpui-base`. `h_resizable(id)` → `ResizablePanelGroup`; `.with_state(&Entity<ResizableState>)`; `resizable_panel().size(px(n)).size_range(px(a)..px(b)).visible(bool)`; `.on_resize(cb)` |
| Transcript scroller | `gpui_component::message_scroller::{MessageScroller, MessageScrollerState}` | Stick-to-bottom chat scroller. `MessageScrollerState::new(item_count, cx)`, `.append(n, cx) -> bool`, `.splice(..)`, `.remeasure_items(range, cx) -> bool`, `.scroll_to_end(cx)`, `.is_following_tail()`, `.is_scrolled_up()`. Built-in "Jump to latest" button |
| Message frames | `gpui_component::message::{Message, MessageGroup, MessageAlignment, MessageAvatar, MessageHeader, MessageContent, MessageFooter}` | Row layout with header/footer/avatar slots |
| User prompt bubble | `gpui_component::bubble::{Bubble, BubbleVariant}` | Optional; `Message` + `MessageAlignment` is sufficient |
| Markdown agent text | `gpui_component::text::{TextView, markdown, TextViewState, TextViewStyle}` | `TextView` with the `markdown` extension; also `html` |
| Multi-line prompt box | `gpui_component::input::{Textarea, TextareaState}` | `TextareaState = InputBaseState<TextareaMode>`; same surface as the app's existing `InputState`: `set_placeholder`, `set_value`, `value`, `focus` |
| Prompt submit key | `gpui_component::input::InputEvent::PressEnter { secondary, shift }` | The base state emits Enter **without inserting a newline**; Shift+Enter inserts one. Exactly the "Enter sends / Shift+Enter newline" contract, no custom key handling |
| Expandable tool calls, plans | `gpui_component::collapsible::Collapsible`, `gpui_component::accordion` | Use per tool call; default collapsed |
| Running indicator | `gpui_component::spinner::Spinner`, `shimmer`, `skeleton` | Spinner for tool-call/turn status; skeleton only for the launch phase |
| Controls | `gpui_component::button::{Button, ButtonVariants}`, `Disableable`, `Sizable`, `Size`, `Tooltip`, `Switch` (or the app's `components/Checkbox`) | Match existing usage in `navbar.rs` / `integrations.rs` |
| Slash-command dropdown | Hand-rolled absolute child (TravelPanel precedent) | See §7.5; `Popover` exists (`anchor`, `open`, `on_open_change`, `content`) as a fallback |
| Toasts | `gpui_component::WindowExt::push_notification` + `Notification` / `NotificationType` | Already used style: `WindowExt` is imported in `main.rs` |
| Icons | `gpui_component_assets::IconName`, or vendored SVG bytes | See §1.2 |

### 1.2 Icon inventory (from `gpui-kit-assets` `default-icons.txt`)

Available and relevant: `bot`, `square-terminal`, `panel-right`, `panel-right-open`,
`panel-right-close`, `panel-bottom`, `play`, `pause`, `loader-circle`, `circle-check`,
`circle-x`, `circle-user`, `file-text`, `file`, `folder`, `copy`, `plus`, `close`, `check`,
`chevron-down/up/left/right`, `chevrons-up-down`, `arrow-up/down/left/right`, `eye`, `eye-off`,
`layout-dashboard`, `settings`, `window-close`.

Assignments: Details switcher → `PanelRight`; Agent switcher → `Bot` (the navbar already uses
`Bot` for Automations); transcript tool-call terminal → `SquareTerminal`; file edits →
`FileText`; running → `LoaderCircle`/`Spinner`; done → `CircleCheck`; failed → `CircleX`;
cancelled → `Close`; copy → `Copy`; send → `ArrowUp`; stop → `Pause`.

Anything outside this list must be vendored as SVG bytes and rendered with
`Icon::default().data(include_bytes!("..."))`, the pattern already used for
`assets/icons/blocks.svg` and `todoist.svg`.

### 1.3 Feature gap that constrains rendering

`gpui-component` 0.6.1 declares **no `default` feature**, and the app depends on it with no
features, so **tree-sitter syntax highlighting is off** (`tree-sitter`, `tree-sitter-<lang>` are
opt-in).

Consequence: code blocks, tool-call arguments and diffs render as **monospace plain text** with
our own diff line tints. Do **not** design around syntax colours. Enabling `tree-sitter` plus
language features and using `highlighter::SyntaxHighlighter` is a deliberate later decision
(§14) — it is a real build-time cost.

---

## 2. Window and layout tree

### 2.1 Root layout

`Layout::render` today is:

```
div().relative().size_full().v_flex()
  .child(TitleBar…)                      // unchanged
  .child(main row: navbar | panel)       // unchanged
  .children(dialog_layer)                // unchanged
```

Add the footer strip between the main row and the dialog layer:

```
div().relative().size_full().v_flex()
  .child(TitleBar…)
  .child(main row…)
  .child(self.render_pane_footer(window, cx))   // NEW — §5
  .children(dialog_layer)
```

The strip is rendered **only in `NavPanel::Tasks`**; outside it, nothing is added (no empty
28px band). `dialog_layer` must stay the last child so dialogs still paint above everything.

### 2.2 Tasks panel: the resizable split

`NavPanel::Tasks` currently renders a `right-column` row with the task list and, conditionally,
the details pane as two `flex_1` siblings (a hard 50/50). Replace that arm with:

```
h_resizable(ElementId::Name("tasks-split".into()))
    .with_state(&self.split_state)
    .child(
        resizable_panel()
            .size(px(640.))
            .size_range(px(320.)..px(1200.))
            .child(div().flex_1().min_h_0().flex().flex_col().child(self.task_list.clone())),
    )
    .child(
        resizable_panel()
            .visible(self.right_pane_open())
            .size(px(420.))
            .size_range(px(320.)..px(760.))
            .child(self.render_right_pane(window, cx)),
    )
    .into_any_element()
```

Rules:

- **Always two panels.** The right panel is collapsed with `.visible(false)`, never removed, so
  the group's keyed state stays coherent across selection changes.
- `self.right_pane_open()` = details has a selection **or** the agent pane is the active pane
  (`RightPane::Agent`).
- Keep the existing `on_click` that clears the task selection on the right column, minus the
  parts that conflicted with pane switching.
- Both panels keep `min_h_0()` children so the row cannot grow past the window (the reason the
  current layout has `min_h_0` there).

**[resolves §9.4]** No hand-rolled drag handle: `ResizablePanelGroup` already provides the
divider, the hit area, the cursor and the drag. Persistence needs no code either: when
`with_state` is not supplied the group stores its state via
`window.use_keyed_state(self.id.clone(), cx, …)`, i.e. keyed state that lives as long as the
window — which is exactly "remembered for the app run". We do pass an explicit
`Entity<ResizableState>` held on `Layout` so the type stays nameable and `on_resize` can be
subscribed later; either way, **no width is written to `acp-sessions.json`.**

---

## 3. Right pane contents

`RightPane` is a small enum on `Layout`: `Details | Agent`, defaulting to `Details`.

```
fn render_right_pane(&self, window, cx) -> AnyElement {
    match self.right_pane {
        RightPane::Details => self.details.clone().into_any_element(),   // unchanged view
        RightPane::Agent   => self.agent_pane.clone().into_any_element(), // §4
    }
}
```

Switching panes must not tear down a session: the agent pane entity is created once with the
layout and merely stops being rendered. Its tasks live in fields, so nothing is cancelled by
being hidden (AGENTS.md: store the `Task` to avoid cancellation).

---

## 4. The agent pane

### 4.1 Structure

```
div().size_full().min_h_0().flex().flex_col()
    .bg(rgb(APP_BG))
    .border_l_1().border_color(rgb(HAIRLINE))
    .child(self.render_header(window, cx))       // 40px
    .child(div().flex_1().min_h_0().child(self.render_transcript(window, cx)))
    .child(self.render_prompt_box(window, cx))
```

### 4.2 Header (40px, `px_3`, `h_flex`, bottom hairline)

- Left: session title from `SessionUpdate::SessionInfoUpdate` (fallback: the project's display
  label), `text_sm` + `font_semibold`, truncated with ellipsis.
- Left, muted next to the title: the model id currently selected (e.g.
  `opencode/minimax-m2.5-free`) at `text_xs`/`TEXT_FAINT`. Ids are opaque — display verbatim.
- Right: **"New session"** button (ghost, compact, `IconName::Plus`, tooltip "Start a new
  session") — starts a fresh `session/new`, replaces the stored id, resets the transcript
  (main spec §5.6). Disabled while a turn is running (queue/Stop must be resolved first), with
  a tooltip saying so.

### 4.3 Transcript

```
MessageScroller::new(
    ElementId::Name("agent-transcript".into()),
    self.transcript_scroller.clone(),
    move |index, window, cx| self.render_entry(index, window, cx),
)
    .scrollbar(true)
    .jump_button(true)
    .with_jump_button_label("Jump to latest")
    .into_any_element()
```

Wiring rules:

- `MessageScrollerState::new(entry_count, cx)` is created with the pane; one **row per
  transcript entry**, indexed by position in the transcript model.
- New entries → `state.append(added, cx)` where `added` is every row the batch just added. One
  batch can add several rows (thought, tool call, message), and a row the list is never told
  about is never rendered — which is what freezes the pane behind the agent's stream.
- Streaming growth of an existing entry → `state.remeasure_items(ix..ix + 1, cx)`; do not
  append a row per chunk.
- **Auto-follow only when `is_following_tail()`** — if the user scrolled up, new output must not
  yank the viewport. The built-in jump button is the way back.
- Row content is pulled by index at render time; the model must therefore be stable and cheap to
  index (a `Vec<Entry>` with an id → index map for tool-call updates).

### 4.4 Entry rendering

| Entry | Rendering |
| --- | --- |
| `UserMessage` | `Message` + `MessageAlignment::End`, `TextView` (markdown) of the prompt text, max width ~80%, `CARD_BG` |
| `AgentText` | `Message` + `MessageAlignment::Start`, `MessageAvatar` = `IconName::Bot`; header (`opencode` + model id, muted, first agent message of a turn only); content = `TextView` markdown |
| `Thought` | Indented, `TEXT_MUTED`, label "Thinking", inside a `Collapsible` (expanded while streaming, collapsed once the turn ends) |
| `ToolCall` | Card: `[status icon] [title/kind] [summary] [chevron]`. Collapsed by default (always collapsed for read-only tools). Expanded body: arguments as a mono block (`raw_input`, pretty-printed) + result. Status drive icon + colour: running `Spinner`/`LoaderCircle`, done `CircleCheck` (success), failed `CircleX` (danger), cancelled `Close` (faint) |
| `Diff` | File header (`FileText` + relative path) then hunks as mono lines with add/del background tints (§9). No syntax colours (§1.3) |
| `Terminal` | Header `[SquareTerminal] [command] [status/exit code]`, body = live mono output block, `Copy` button on hover. Cap the visible height and show the **last 200 lines** with a "Show all" expander so a chatty command cannot balloon a row |
| `Plan` | Compact list; per `PlanEntry` a status marker (pending / in-progress / completed) + text |
| `Permission` | Card: tool-call summary + **one `Button` per offered `PermissionOption`**, label = the option's `name`, variant by kind (AllowOnce/AllowAlways `primary`, RejectOnce/RejectAlways `ghost` + danger text). After a decision the card collapses to a one-line record ("Allowed once · `rm -rf build`") |
| `Usage` | Right-aligned, `text_xs`, `TEXT_FAINT`; updated in place rather than appended |
| `Notice` | Centred, `text_xs`, `TEXT_MUTED`; `danger` colour for error-level (`Level::Error`). Used for resume failure, queue dropped, mode/config changes |
| `Error` | Centred card: title, last ~20 stderr lines in a mono block, **Retry** button. Used for launch failure and mid-session process exit (main spec §5.10) |
| `AuthRequired` | Card listing advertised auth methods; `AuthMethod::Agent` → "Sign in" button; `AuthMethod::Terminal` → embedded terminal widget running the advertised command |

Tool-call status must not rely on colour alone: icon **and** text label.

### 4.5 Empty / non-ready states

In the transcript area when there is nothing to show:

- **Disabled** (tag is not a directory project): centred muted text — "Select a project with a
  directory to use the agent." The pane is normally unreachable in this state because the footer
  switcher is disabled; this state covers the tag switching out from under an open pane.
- **Launching**: `Spinner` + "Starting opencode…" + the resolved command and cwd, muted.
- **No directories resolve**: `Error` entry — no process is spawned (main spec §5.4).
- **Ready, empty transcript**: centred hint "Ask the agent about ⟨project label⟩" plus the
  resolved directory list, muted, at `text_xs`.

---

## 5. Pane-switching footer (window-wide)

```
div()
    .flex_none()
    .h(px(28.))
    .h_flex().items_center().justify_end().gap_1().px_2()
    .border_t_1().border_color(rgb(HAIRLINE))
    .bg(rgb(PANEL_BG))
    .child(switch_button("pane-switch-details", IconName::PanelRight, "Details", …))
    .child(switch_button("pane-switch-agent",   IconName::Bot,         "Agent",   …))
```

Rules:

- **Only these two buttons.** No agent status, no model chip, no Stop here — those live inside
  the pane (§7.4). The navbar keeps its own footer rows untouched.
- Icon-only ghost buttons, `.compact()`, fixed 22×22 hit box (`w(px(22.)) h(px(22.))`,
  `justify_center`), each with a tooltip ("Show details pane" / "Show agent pane") and an
  `aria_label`.
- Selected pane: `bg(rgb(PANEL_HOVER))` and `text_color(rgb(0xe5e5e5))`; unselected:
  `text_color(rgb(TEXT_MUTED))` with `hover(|s| s.bg(rgb(PANEL_HOVER)))` — the same treatment
  as `nav_footer_row`.
- **Agent button disabled** when the selected tag is not a directory project: `.disabled(true)`
  plus a tooltip explaining it needs a directory-backed project (main spec §5.3). Disabled still
  means visible — never hidden.
- Clicking either switcher is what opens a collapsed right pane: selecting Agent sets
  `right_pane = Agent`, which makes `right_pane_open()` true.

---

## 6. Streaming into the UI

Data path: ACP notifications arrive on the Tokio runtime → the session task hops to the GPUI
foreground → the pane's model is updated → the view is notified.

Rules:

- Never mutate GPUI state from the Tokio thread. Use `gpui_tokio::Tokio::spawn_result` for the
  I/O side and `this.update(cx, …)` on the foreground side (AGENTS.md core async pattern).
- **Coalesce chunks per frame.** Push incoming deltas onto a `pending: Vec<Chunk>` buffer; flush
  and `cx.notify()` at most once per frame using `window.on_next_frame` (precedent: the
  project picker's deferred autofocus in `main.rs`). Do not notify once per token.
- On flush: append/merge text into the current entry, `remeasure_items` that row if it grew, and
  `append` (for the count of new entries) only when the batch created one or more.
- Turn start/end must notify the **navbar** so the busy dot goes on and off — emit an event from
  the pane and have `Layout` subscribe, mirroring `IntegrationsEvent::Changed` →
  `refresh_tags` (main spec §8).
- Turn end (`stop_reason`), permission decisions and queue drains all flush before updating
  status, so the transcript never lags the state.

---

## 7. Prompt box

### 7.1 Element tree

```
div().flex_none().p_2().gap_2().v_flex()      // prompt area
    .child(self.render_controls_row(window, cx))   // §7.4
    .child(
        div().relative()                            // anchor for the slash dropdown
            .rounded_lg().border_1().border_color(rgb(HAIRLINE)).bg(rgb(CARD_BG))
            .child(Textarea::new(&self.prompt_input).h(px(88.)).appearance(false))
            .when(self.slash_open, |this| this.child(self.render_slash_dropdown(window, cx))),
    )
    .child(self.render_hints_row(window, cx))       // queue chip, cwd, errors
```

The slash dropdown is the **last child** of a `relative()` container so GPUI paints it above the
input — the same technique `main.rs` already documents for the travel popover.

### 7.2 Input behaviour

- `TextareaState::new(window, cx)` with `set_placeholder("Ask the agent to do something…", window, cx)`.
- Subscribe to `InputEvent`:
  - `PressEnter { shift: false, .. }` → when the slash dropdown is open, accept the highlighted
    command; otherwise **send**.
  - `PressEnter { shift: true, .. }` → let the textarea insert the newline (built-in behaviour).
  - `Change` → recompute `slash_open` (text starts with `/` and no space yet) and the filtered
    command list.
- Height: start at `h(px(88.))`. Auto-growing is **not confirmed** for `Textarea`; if
  `DefiniteLength::Auto`/growth is unsupported, keep the fixed height and let the textarea
  scroll internally (§14).
- Empty/whitespace input disables Send.
- While the agent pane is not ready (launching/error/auth), the textarea stays disabled with a
  status-appropriate placeholder.

### 7.3 Send / Stop / queue

- **Send** (`IconName::ArrowUp`, primary) — sends immediately when idle; while a turn runs it
  appends to the queue.
- **Stop** (`IconName::Pause`, ghost) replaces Send while streaming and issues `session/cancel`;
  it also **clears the queue** and posts a `Notice` reporting how many messages were dropped
  (main spec §9.6).
- **Queue indicator**: a chip next to the controls reading `Queued · n`, with an X to clear.
  At the cap (10) Send is disabled with a "Queue full" tooltip.
- Queue state is in-memory and session-scoped; closing the pane must not clear it.

### 7.4 Controls row

`[model chip] [mode chip] [auto-approve toggle] … [attach task] [send|stop]`

- **Model chip**: `Button` ghost/compact + `IconName::ChevronsUpDown`, label = current model id
  (truncated), opens a list of the values of the agent's model-shaped `SessionConfigOption`
  (§9.2 of the main spec). Selection sends `SetSessionConfigOptionRequest`.
- **Mode chip**: same affordance for the modes the agent reports, from whichever source it
  reports them: session modes (`SessionModeState`, selection sends `SetSessionModeRequest`) or,
  as opencode does, a `mode`-category `SessionConfigOption` (selection sends
  `SetSessionConfigOptionRequest`, and the option's response is what names the mode taken).
  Hidden when the agent advertises no modes.
- **Mode chip label**: the *name* of the mode in force, from that same source (a mode the source
  does not name falls back to its id). A pick moves the label at the click: `session/set_mode`
  answers with an empty result and no announcement is promised, so waiting for one would name
  the mode the user just left. A `CurrentModeUpdate` sent afterwards still replaces it, and a
  refused change puts the previous mode back — unless a later pick owns the label by then.
- **Auto-approve toggle**: a `Switch` (or the app's `components/Checkbox` for visual
  consistency) with the tooltip "Automatically approve tool calls for this project". Default
  off, persisted per project (main spec §5.8 / §9.5).
- **Attach task** (`IconName::FileText`, ghost, tooltip "Insert the selected task"): appends the
  current task's context to the textarea without sending (main spec §5.12). Disabled when no
  task is selected.
- Other advertised select-valued config options render as additional compact chips, in
  advertisement order, after the mode chip.

### 7.5 Slash-command dropdown

- Opens when the textarea content starts with `/`; filters `AvailableCommand`s on the text after
  the slash (matching name first, then description).
- Each row: `/name` (mono) + description (muted, `text_xs`), keyboard-highlighted row uses
  `bg(rgb(PANEL_HOVER))`.
- Keys, bound in a dedicated context named `AgentPane` registered once at startup (the
  `project_picker::init` precedent):
  - `up` / `down` move the highlight,
  - `enter` inserts the command into the textarea (as `/name `, ready for arguments) rather than
    sending,
  - `escape` closes the dropdown.
- Empty filter result: the dropdown closes and the text stays literal.
- Because Enter is intercepted, the pane must check "dropdown open" **before** the send path in
  its `PressEnter` handler.

---

## 8. Styling rules

- Add the missing tokens to `theme.rs` instead of scattering new literals; several existing
  views already hardcode the same hexes (`navbar.rs` uses `0x1e1e1e`, `0x2a2a2a`, `0xa3a3a3`,
  `0x737373`):
  - `PANEL_BG = 0x1e1e1e`, `PANEL_HOVER = 0x2a2a2a`
  - `TEXT_MUTED = 0xa3a3a3`, `TEXT_FAINT = 0x737373`, `TEXT_STRONG = 0xe5e5e5`
  - `SUCCESS = 0x4ade80`, `DANGER = 0xef4444`
  - `DIFF_ADD_BG`, `DIFF_DEL_BG` (subtle translucent tints, not saturated)
- Existing tokens stay authoritative: `APP_BG` for the pane surface, `CARD_BG` for cards and the
  prompt box, `HAIRLINE` for every 1px border.
- Spacing: pane padding `p_3`; transcript row gap `12px`; tool-call card padding `p_2`;
  radius `rounded_md` for rows, `rounded_lg` for cards and the prompt box.
- Markdown inside a message renders at **one size**: `message_markdown_style(body)` resolves
  headings from the row's own body size (H1 = body + 2px, every other level = body + 1px) instead
  of the component default's 14px base, which puts an H1 at 28px next to 12px body text, and the
  `code_block` refinement pins fenced blocks to the body too (the theme's mono step is 13px, a
  third size next to a 12px message). Bullets, emphasis and links inherit the body size already.
- Inline code is the one size the style API cannot reach — the renderer draws it at 0.875× the
  body in a mono family with its own background. **Prompt bubbles** therefore render
  `prompt_bubble_text(text)`: the code-span markers are dropped so the spans read as the sentence
  around them (the interview template is a page of headings, lists and `./docs/spec/<slug>-spec.md`
  spans, and that third size is what made the bubble uneven). Fenced blocks keep their markers —
  a whole block of code is not a mid-sentence size change — and the agent's own replies keep their
  code styling, where a mono pill on a path earns its keep. The prompt *sent* to the agent is
  unchanged: only the bubble's rendering is flattened.
- Typography: markdown/agent text at the gpui-component default size; the theme's resolved mono
  family for arguments, diffs and terminal output at a size one step smaller; notices `text_xs`.
  The token is the CSS generic `ui-monospace`, but GPUI takes one concrete family and has no
  stack, and naming one the machine lacks is not a silent fallback: the text system caches the
  failed lookup and rebuilds an `anyhow` error on every per-run font resolution, so the pane reads
  the theme's mono token instead — gpui-component probes that for a family the machine has. The
  pane caches the answer for its element helpers (`resolve_mono_font`).
- Long content: every code/diff/terminal block scrolls horizontally inside its own container
  rather than widening the pane.
- Markdown code blocks get the panel surface (`PANEL_BG`). Copying is per **message**, not per
  code block: the message row's own `Copy` control (§9) is the affordance, and it takes the
  Markdown source — fences and all — so a code block comes out with the rest of the reply.
- A message row reveals that control on hover (`MESSAGE_GROUP`), like the task row's ownership
  marker: `opacity(0.0)` + `.group_hover(MESSAGE_GROUP, |s| s.opacity(1.0))` over the row's own
  surface (`APP_BG`), so it hides the text it floats over instead of stacking a new shade on it.
  In the prompt row it sits in the free space to the left of the right-aligned bubble; in the
  reply row it is pinned to the row's top-right corner.

---

## 9. Interaction model

- Focus order inside the pane: header buttons → transcript (scrollable, `MessageScroller` handles
  its own focus) → model/mode chips → auto-approve toggle → attach → textarea → Send/Stop.
- Opening the pane puts focus in the textarea (deferred one frame if the element is not in the
  focus tree yet — the project picker needed `window.on_next_frame` for exactly this).
- `escape`: closes a dropdown first. While a turn is running **and the pane holds the focus**, the
  next Escape arms the cancellation — the activity row shows an `Esc` keycap chip followed by
  "again to cancel the conversation" for one second, and is clickable for the same cancel — and a
  second Escape inside that window calls Stop. An idle pane,
  or one that does not hold the focus, returns the key to the window-wide deselect observer in
  `main.rs`. Escape is not bound in `AGENT_PANE_CONTEXT`: the observer owns it, so every focus
  position inside the pane behaves the same.
- The Stop button's tooltip names both shortcuts: "Stop the turn · Ctrl-C or Esc twice".
- `ctrl-c`: interrupts the running turn, from anywhere inside the pane (terminal habit). Taken by
  the pane's keystroke interceptor, ahead of the prompt box's own Ctrl-C → copy. It falls through
  while no turn is running, and also while the prompt box holds the focus *with a selection in it*
  — a selection is there to be copied, so the platform meaning of the key wins.
- A message row's text is **not selectable** and carries a `Copy` control that copies the whole
  message as the Markdown it was sent as (`Copy the prompt` / `Copy the reply`). See §10 for why
  the trade is worth it: registering selection quads is the single most expensive thing the
  transcript does per frame. Bounded rows — notices, tool cards, terminal output, error text —
  keep their normal selection.
- Every icon-only control needs a tooltip **and** an accessible label (`.aria_label(…)`, which
  `Textarea` and `Button` both support).
- Hover states: `bg(rgb(PANEL_HOVER))` on interactive rows, matching `nav_footer_row`; cursor
  changes on the split divider are handled by `ResizablePanelGroup`.
- Disabled controls still render with a tooltip explaining *why* (the navbar's disabled history
  buttons use `.tooltip(…)` for this).
- Scrolling a transcript must never move the task list or the split; each pane scrolls
  independently.

---

## 10. Performance

- One row per entry, virtualization by `MessageScroller`; entries are only rendered when visible.
- **A message row's text is not selectable.** The markdown renderer registers selection quads by
  walking every character of every inline on **every paint** (`Inline::text_line_bounds`), two
  glyph lookups per character, so a long message costs its length on each frame — measured at
  ~390ms per frame for a 6.7KB message against ~1.8ms with selection off (release is smaller by
  the usual factor, but it is the same shape). Every visible row is re-laid out on every frame,
  scroll frames included, so with selection on, scrolling a conversation of real messages runs at
  a few frames per second. Selectable bounded rows (notices, tool cards, error text) cost the
  same per frame but stay short, which is why the rule is drawn at the message rows.
- The `Copy` control on a message row (§8/§9) is what selection used to provide: the whole
  message, as Markdown. Adding a *partial* copy affordance must not reintroduce selection on the
  message rows without addressing the per-frame walk.
- Chunk coalescing per frame (§6) — the single most important rule for streaming smoothness.
- Terminal output and tool-call results are capped (200 lines visible, expandable) so a single
  row can never hold unbounded content.
- The transcript model keeps a `tool_call_id → index` map so `ToolCallUpdate` is O(1) and never
  rescans the vector.
- Bounded memory: the pane drops nothing on its own (the session is the unit of lifetime); the
  model is dropped with the pane when the session is torn down (main spec §9.1).

---

## 11. Accessibility

- Status is never colour-only: icon + label text for tool calls, diff lines carry `+`/`-`
  prefixes in addition to tints, errors carry a title.
- Muted text stays legible on `APP_BG`: `TEXT_MUTED` (≈7:1) for readable content,
  `TEXT_FAINT` (≈4:1) only for incidental metadata such as the model id.
- Focus is always visible; rely on gpui-component's focus ring rather than removing outlines.
- Avoid flashing or rapidly animating elements: a spinner plus a "Running" label conveys
  progress without motion being the only signal. Prefer static status glyphs for finished work.
- The prompt box carries an `aria_label` ("Message the agent"), the transcript an
  `aria_label` ("Agent transcript").

---

## 12. Testing the UI

Use `gpui::TestAppContext` / `VisualTestContext` with gpui timers
(`cx.background_executor().timer(…)`), never `smol::Timer` (AGENTS.md).

Cases worth covering, driven through the pane's model rather than through a real agent:

- A batch of `AgentMessageChunk`s grows one row and does **not** add rows per chunk.
- A `ToolCall` followed by `ToolCallUpdate` mutates the existing row (map lookup) and the row
  count is unchanged.
- `PressEnter { shift: false }` sends; `PressEnter { shift: true }` does not send.
- With the slash dropdown open, `PressEnter` inserts the command instead of sending.
- Footer: the Agent switcher renders disabled with a tooltip for a plain tag, enabled for a
  `project:{path}` tag; clicking it opens the pane.
- Switching panes and switching back preserves the transcript and the `MessageScrollerState`.
- The split keeps two panels when the right pane is hidden (`.visible(false)`), so returning to
  it does not reset the width.
- A queue of 10 disables Send; Stop clears the queue and appends the drop notice.
- Permission cards render exactly the options the agent sent (including a two-option and a
  four-option case).

---

## 13. File layout in the app

Per AGENTS.md, prefer existing files and avoid a sprawl of small modules. Suggested:

- `apps/todo-2/src/ui_parts/agent_pane.rs` — the pane entity: header, transcript rows, prompt
  box, states (§4, §7, §8).
- `apps/todo-2/src/ui_parts/agent_transcript.rs` — the transcript model + entry types and the
  `session/update` → entry reduction (§4.4), if `agent_pane.rs` grows past comfortable size.
- `apps/todo-2/src/theme.rs` — new tokens (§8).
- `apps/todo-2/src/main.rs` — `RightPane` state, split wiring, footer strip, navbar subscription
  for the busy dot.
- `apps/todo-2/src/ui_parts/mod.rs` — module declarations.

---

## 14. To verify while implementing

- `Textarea` auto-grow: whether `.h()` accepts `Auto`/a growth mode, or whether the fixed 88px +
  internal scroll is the end state.
- Whether `MessageScroller`'s `with_content_style`/`with_list_style` are needed to make row
  spacing match the app's other views.
- The exact `ResizablePanelGroup` distribution when only one panel declares a `size` (the group
  documents an average-size fallback for unsized panels); confirm the left pane absorbs the
  remainder, and otherwise set an explicit left `size`.
- Whether `gpui_component::Popover` can be hover/typing-triggered and keyboard-driven well enough
  to replace the hand-rolled slash dropdown; the hand-rolled version is the default plan.
- `Switch` vs the app's own `components::Checkbox` for the auto-approve toggle (visual
  consistency with the task list's checkboxes argues for `Checkbox`).
- Whether to enable gpui-component's `tree-sitter` feature (plus a language list) for code and
  diff highlighting — off today, and a build-time cost worth measuring before adopting.
- Exact icon variant names on `IconName` for the assignments in §1.2 (`IconName::SquareTerminal`,
  `IconName::PanelRight`, `IconName::Copy`, …) — the SVG filenames exist in the asset bundle;
  confirm the generated enum variants.
