# Shepherd workspace UI decisions

This document records the agreed layout, controls, and interaction details for
the Shepherd desktop shell. Use it alongside the [master specification](./master-spec.md)
when implementing the [remaining tasks](../todo.md). Where an earlier sketch
differs on an element covered here, follow this document.

Keep these decisions when adding features. Ask the user before substantially
changing a covered element's placement, visual hierarchy, controls, or default
behaviour, unless the user already requested that change. Fixes and additions
that preserve these decisions can proceed. The measurements below describe the
current layout and give future changes a point of comparison.

## Window and workspace frame

- Group the Projects sidebar, conversation pane, and context multipane in one
  rounded wrapper. Use a slightly lighter surface than the surrounding window
  and a subtle border. The current wrapper radius is 10 px.
- Place a slim icon rail to the left, outside the wrapper. The rail and the
  Previous and Next control group share a 52 px width and 6 px padding. The
  rail currently has placeholder icons for Workspace and Conversations.
  Previous and Next are placeholders for navigation history. While the
  Freebuff terminal is visible, Previous is available as **Back to chat**.
- **Search tasks** sits immediately after Previous and Next in the window
  header. It has a magnifier inside the field and shows the current shortcut
  as individually bordered, muted keycaps with small text, tight spacing and
  minimal horizontal padding and
  a very dim background when collapsed. The field expands
  with an ease transition on focus. The default is Command-Shift-F on macOS
  or Ctrl-Alt-F on Linux. Ranked matches from every project appear underneath
  the field with the task, project and a highlighted snippet. Up and Down
  move between results, Enter opens one, and Escape collapses the field and
  restores focus. Opening a result scrolls to and focuses
  its transcript row and highlights the matching text. Archived tasks can open
  directly from search while their sidebar visibility preference stays intact.
- **Command palette** in the icon rail opens an app-styled search dialog.
  Command-K on macOS or Ctrl-Shift-K on Linux opens it from the keyboard.
  Results show the command, its category, the current shortcut, and any
  reason it is unavailable. Up and Down move between results, Enter runs the
  selected command, and Escape restores focus. The project and task switchers
  use the same dialog. **Send next key to terminal** restores the terminal
  input that opened the palette and gives it the next key before normal
  shortcut routing resumes.
- On macOS, show the native window controls on the app's surface, with the
  title hidden. Put Previous and Next directly to their right on the same
  horizontal line, centred vertically in a 42 px header. The free header space
  is draggable. Quit uses the system window or app controls.
- Use one window grid for the header, workspace, and footer. The header and
  workspace share columns through CSS subgrid. The navigation, conversation,
  and inspector sections of the header share the window background.
- Keep the inspector on the right. Left-align its five icon tabs in the header
  above it in this order: **Terminals**, **Preview**, **Review**, **Git**, and
  **Files**.
  Give each icon an accessible name and tooltip. Use Left and Right to switch
  between available tabs. Show terminal and review attention as dots with
  accessible descriptions. The tabs follow the conversation in the Tab
  sequence, with one tab stop for the selected tab and the named panel next.
  Settings can occupy the middle pane while the sidebar and inspector remain
  in place.
  In Terminals, keep the selected task's read-only Codex command output
  alongside the interactive terminal panes. Follow the latest command by
  default and let the user select an earlier command without shifting focus
  when new output arrives.
  Show ACP terminals for the selected task as interactive **agent: <title>**
  panes. Use the same terminal input, resize, and hide controls as task
  shells. Released agent panes disappear when the turn settles.
  Down from the Terminals tab focuses the first live pane's **Hide pane**
  button. Arrow keys on live pane header buttons move between adjacent live
  panes. Up from a pane with no live pane above it returns to the Terminals
  tab. Arrow keys in the terminal input go to the shell.
  Command-Shift-X on macOS or Ctrl-Shift-X on Linux opens copy mode with the
  current viewport selected. Escape returns to the shell. Tab or Shift-Tab
  from copy mode moves to another control. The off-screen Terminal output
  region continues to update while the copy selection stays fixed.
  On macOS, the current input source supplies printable Option characters,
  including Option+3 for `#` on the British layout. Option with a navigation
  key still sends a terminal chord.
  Resize the inspector at its left edge by dragging or with the arrow keys
  while the separator is focused. Save the chosen width with the task.
- In Files, name the active root above the tree and let the user choose the
  project root or the selected task's worktree using a compact, themed picker;
  label the project root `/`. Put it, the filename filter, and the settings gear
  on one row, with the gear at the right. Match the filter height to the root
  picker, with minimal vertical padding. Keep the gear transparent and
  borderless, with a subtle background only on hover.
  Show a small folder icon on
  each directory row without an expand caret. Expand folders in place on
  demand, keeping one open path and its ancestors while closing sibling
  branches with a short eased height transition. Show dotfiles by default; a
  gear opens a compact card beside the button with a global, persistent
  **Show dotfiles** toggle. Open text
  in a read-only, line-numbered viewer inside the inspector. **Find files**
  uses the app's themed input and filters filenames as the user types.
  Filtering starts as the user types.
  Use muted default text and Warm Charmed file type icons. Colour changed paths
  and their ancestor folders using the Aura Dark or One Light Git status colours
  for the active appearance. Let the tree use the available inspector height,
  with scrolling for longer lists. Show Git change badges beside visible paths and list deleted paths below the
  tree. Show Git-ignored files and folders in the active Zed theme's dim ignored
  colour, including monochrome icons; continue hiding `.shepherdignore` entries.
  Preload nearby folder levels with bounded concurrency so opening a folder does
  not briefly show an empty list. Refresh Git status and cached folders after a
  debounced workspace change.
  Put **Open in external editor** and **Reveal in file manager** above the file
  preview. **Open in Files** from a Git or Review diff selects and focuses the
  Files tab and opens the matching worktree file.
  Git and Review file rows expose their change status in words. Diff text is
  a named, keyboard-focusable region.
- In Preview, offer **Project settings** when the selected project has no
  preview URL. **Settings → Projects** saves or clears that project's loopback
  URL. When it is configured, show the address, **Reload**, **Open in browser**,
  and **Project settings** above the sandboxed page. The preview uses a separate
  origin from the shell so the project can load its own modules and assets.
  **Annotate** pauses interaction with the page so a click selects an element.
  Write a note and queue it in Preview. **Add to prompt** appends the queued
  annotations as editable text in the active composer and clears the queue.
  If element selection is unavailable, show that status and offer a screenshot
  picker. **Screenshot note** also opens the picker directly. Choose an image
  captured in the browser and mark a point, then use the same note queue.
- Place the status bar below the wrapper on the window background. Its
  transparent, borderless surface keeps it visually separate from the panes.
  Show the running and review counts across projects. The review count toggles
  the sidebar filter and the macOS Dock badge mirrors the count.
  For a task worktree, show its current branch and changed file count as Git
  reports them. Show **Detached HEAD** when detached. The native title names
  the active project and adds a working-count suffix while any task is live.
- Show runtime errors in a temporary floating toast above the workspace. Provide
  **Error history** in the status bar to inspect earlier messages and their
  operation, timestamp, identifier, and diagnostic details. A damaged history
  record is identified in the list while readable records remain available;
  new records remain separate from an interrupted final record. Distinct
  operations and error chains remain separate even when their messages match,
  and nested aggregate causes retain their messages and codes.
- After restart, show a dismissible notice for previous process records whose
  identity matches or cannot be verified. Show the PID and inspection status,
  allow a recheck, and explain that a match may belong to another instance.
  If an agent leader has exited but its recorded group may still have members,
  identify that uncertainty in the notice rather than claiming a match.
- List archived worktrees that could not be collected at startup with their
  task, path, and reason in a dismissible notice.

## Projects sidebar and settings

- Use the left-aligned **Projects** heading in sentence case. Keep its compact,
  muted typography at 11.5 px, based on Herdr's **Spaces** heading. Place a
  borderless folder-plus icon in the same muted colour at the right end.
  It gains a background on hover. Its tooltip is **Add project** and it opens
  the project picker. The icon uses Herdr App's
  `defaults/mods/workspace-chrome/folder-plus.svg` from checkout `5b37363`.
- When the project list is empty, show a plus icon with **New project**, then
  **or import all from**, followed by one button per source. Left-align the
  prompt and buttons. **ChatGPT app** comes first and imports the local Codex
  workspace folders shown in the desktop app. Dim and disable sources that
  are unavailable or unimplemented. The current placeholders are OpenCode,
  Pi, Claude Code, and Freebuff. Keep the empty state free of explanatory copy
  above these controls.
- Anchor **Settings** at the bottom of the Projects sidebar. The gear and label
  are muted, with a transparent, borderless resting state and a hover highlight.
  Opening Settings replaces the conversation pane. The settings subnavigation
  groups controls under **Workspace**, **Projects**, **Appearance**, and
  **Keyboard shortcuts**.
  **Settings → Projects** offers the same per-source import buttons even when
  the sidebar already contains projects.
- **Settings → Workspace** has an unchecked **Notify when tasks need input**
  option. Enabling it allows system notifications for new waits outside the
  focused task workspace. Review changes only affect the in-app indicators
  and Dock count.
- **Settings → Workspace** edits the default attachment limit and terminal
  scrollback. **Settings → Projects** shows the workspace values and lets
  the selected project override them. Reset removes the project override.
  **Reload settings** reads the settings file and lets users review pending
  changes if a value changed on disk.
- **Settings → Appearance** offers System, Light, and Dark, with System as the
  default. Compact density keeps the default spacing. Comfortable adds space
  between task rows and transcript entries and around inspector content.
  The terminal follows the active theme without a restart.
- **Settings → Keyboard shortcuts** shows the current binding for each
  command. **Edit** focuses a recorder, **Save shortcut** applies the chord
  immediately, and **Reset** restores the default. Escape cancels recording
  and returns focus to the Edit button. A conflict leaves the recorded chord
  visible. **Reload configuration** reads external keybinding edits.

### Add project picker

- Exclude `~/.claude`, `~/.codex`, and `~/ComfyUI-Installs` from the home scan
  before forming groups.
- Discover Git working trees with either a `.git` directory or a `.git` file
  containing a Git `gitdir:` marker, and stop the scan at each discovered
  repository.
- Report an unavailable scan root as an error, while skipping individual
  subdirectories that cannot be read.
- Use actual directory branches to group repositories. Enumerate siblings
  directly when they share a parent and there is no useful grouping. Collapse
  single-child chains and skip a sole top-level group so the next meaningful
  level is visible immediately. For example, if `src` is the only group after
  exclusions, show its children as the first rows.
- Expand the first group level by default. Open and close groups in place in
  the same list. Filtering matches repository names and paths across the tree
  and shows the matches as a flat list.

### Project rows and context cards

- Show a small line folder icon, project name, and an icon-only Compose action.
  Add a small branch badge to the folder for Git working trees. Use compact
  icons with light strokes. The text and icons share a colour. Use muted text
  for unselected projects and slightly brighter text for the selected project.
  Unselected rows rest on the sidebar's background with a transparent fill.
  The selected row has a raised fill and no visible border. On devices with
  hover, reveal Compose when the pointer is over the row or a row button has
  keyboard focus. Compose opens a draft in that project.
- Show task rows beneath the selected project only when more than one task is
  visible after applying the archive filter. A sole visible task opens directly
  in the conversation pane. Reserve the expanded area for task rows. Compose
  stays on the project row and the archive switch stays in the context card.
- While the status-bar review filter is active, show waiting-for-review tasks
  under each matching project, including a sole review task. Checkboxes and
  modifier-clicks select rows across projects. **Archive selected** shows the
  number of selected tasks in a confirmation dialog.
  If a selected task leaves review while the dialog is open, close the dialog
  and ask for a fresh confirmation of the remaining selection. A failed
  archive keeps the eligible tasks selected for retry.
- Highlight a row on hover and open a compact context card directly to its
  right. The pointer can move onto the card. Right-click and Shift+F10 open the
  same card. The row uses hover and these context actions as its menu entry
  points.
- Order the card rows as the read-only local path, Git remote when available,
  **Show archived**, and **Project settings**. Use matching row heights, text
  sizes, and padding, with icons and text vertically centred. Show a folder
  beside the path and abbreviate the home directory as `~/`.
- Show GitHub, GitLab, and Hugging Face remotes with monochrome line icons.
  Omit the recognised forge host and trailing `.git` from the displayed remote
  label. Keep it on one line and let the card widen to fit within the window.
- **Show archived** uses a switch the user can toggle in the card for that
  project. **Project settings** has a small gear and opens that project's
  settings in the middle pane.

## Empty conversation

- With an empty Projects list, show **Add a project to start** in the main
  pane. An unavailable folder keeps an existing transcript readable and
  disables the composer. If the remembered task was deleted, open a draft.
- In a project with no visible tasks, show the bold 30 px prompt **What should
  {agent} build in {project}?** centred both horizontally and vertically in
  the area above the composer. Keep the composer ready for input when the
  project opens. The first nonblank input creates a draft and sending the
  prompt starts the chat.
- The agent and project names are inline pickers. Keep the surrounding
  headline typography, underline each name, and add a small down caret. The
  triggers have transparent backgrounds and no borders. Use Shepherd's themed
  popover styling for the expanded menus. The project picker switches the
  selected project. Codex is the default agent. **Freebuff (terminal)** opens
  a standalone terminal that fills the conversation pane. Use **Back to chat**
  in the window header to close it. Other unavailable agents are disabled until
  their integrations are ready.
- OpenCode and Claude Code are available when their executables are configured
  in `settings.json`.
  The selection applies to the new draft. The composer can change a draft's
  agent until its first prompt. The task then keeps its agent across restarts.

## Conversation lifecycle

- Saved tasks offer **Delete task** in the conversation header. Confirm deletion
  in a dialog; keep the worktree by default, and offer removal only when it is
  clean. The task branch is retained. Active turns cannot be deleted.
- Show **Needs input** on waiting task rows and an attention dot on the
  selected task's composer. Review rows have a ring and **Needs review** badge.
- Show the negotiated adapter tier in the conversation header after a session
  opens. When a fallback is used, show its reason and keep the command,
  protocol, capabilities and stderr in **Diagnostics**.
- Show **Cancel turn** in the conversation header during an active turn when
  the adapter supports cancellation. The shortcut is Command+. on macOS and
  Ctrl+. on Linux. Show **Cancelling…**
  until the adapter settles the turn, and pause approval actions during that
  interval. When the server withdraws an approval, retain its arguments and
  show **withdrawn** in place of the actions.
- For eligible ACP requests, offer **Allow once**, **Allow for session** and
  **Always allow for project** on the inline card. Identify reused grants on
  the resolved card. In Settings → Projects, show the selected project's
  saved approvals and live session approvals in separate lists. Label each
  session grant with its task, remove it when its session ends, and show a
  reset action for each grant.
- For interrupted or failed tasks with a worktree, show **Resume** when the
  saved session can resume, **Reconnect** for legacy records with a session
  handle, or **Start new session** when a fresh session is needed. Enable the
  composer when the core reports the task is ready.

## Conversation composer

- The text area and its controls share one card at the bottom of the chat.
  Use the raised surface, slightly rounded corners, and a broad inset shadow
  across the whole card. The blue focus border encloses both rows. The current
  radius is 12 px and the card has 8 px of padding on every side. Use a muted
  15 px placeholder and open space between the transcript and the card. The
  transparent text area shares the plus button's 4 px horizontal inset and
  uses the same inset at the top. The lower row uses the card's padding directly.
- Place the controls in this order. Start with **+** for attachments and
  commands, then the shell safety picker, then the agent picker. Put the
  model and effort picker immediately before Send at the trailing edge. Keep
  the model picker and Send together when the toolbar wraps in a narrow window.
- Use compact, app-styled menus that open above the toolbar when there is room.
  The trigger carets point down regardless of opening direction. Keep
  unavailable attachment, command, agent, model, and effort options visible
  and disabled until their integrations are ready.
- **Attach files** opens a local file chooser. Show removable filename chips
  inside the composer while files are queued. On Send, Shepherd copies files
  into the selected task's attachment cache. The size limit is 50 MB per file
  by default, with app and project overrides in `settings.json`. The prompt can
  be sent with files alone. It includes the paths in the transcript and
  passes supported images to the agent as image input.
- Typing `@` at a word boundary opens a file picker above the composer and
  keeps focus in the text area. Type part of a workspace path to narrow the
  results. Use the arrow keys and Enter to insert a reference, or Escape to
  dismiss the picker. Drafts search the project directory until they have a
  worktree. A sent mention resolves against the task's worktree.
- The safety picker shows a small shield containing `>_`, the current value
  **Ask me**, and a down caret immediately after the text. Reserve a 128 px
  slot wide enough for **Approve for me** so changes to the value leave sibling
  positions stable. Inside that slot, fit the button and its hover and click
  areas to the shield, label, and caret. **Ask me** is the default. **Approve for
  me** is a disabled upcoming option. Show the picker in an empty chat or
  draft. For an existing session, show it when the adapter reports approval
  support. A legacy session can report its capabilities after reconnecting.
- Show the selected model name on the model and effort trigger. Shepherd's
  Codex default is GPT-6.1 Terra (`gpt-6.1-terra`). Before a session, display
  the model resolved from Shepherd's launch configuration. Once a session
  starts, use the model reported by that session. List the visible models
  advertised by Codex and the reasoning efforts available for the chosen
  model. Choosing a model also selects its advertised default effort. A
  choice in the composer applies to the next turn. Use **Codex default** if
  the model name is unavailable. OpenCode and Claude Code use their own
  model configuration and do not show this picker.
- Show a Mode picker beside Send when the connected session advertises
  choices. Native OpenCode drafts can also pick from the project's catalog.
  Apply a draft choice before the first prompt and display the session's
  current mode once connected. Keep choices disabled during a turn.
  Shepherd handles approval requests through its shell approval controls.
- Send sits inside the card, at the end of the lower row. Use the compact
  right-pointing arrowhead icon and a bright accent when enabled. The current
  button size is 32 × 30 px. The disabled state uses a muted tint of the same
  accent. **Settings → Appearance** offers Blue, Violet, Teal, Amber, and
  Neutral. Blue is the default. The choice persists across regular `shep`
  launches and lasts for the window session in `shep --test`.

Use the [desktop launch modes](../../README.md#run) to review the UI with
disposable data. `shep` keeps a separate test database between launches and
`shep --test` starts a fresh database each launch.
