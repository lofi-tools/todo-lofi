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
  rail currently has placeholder icons for Workspace, Conversations, and Tools.
  Previous and Next are placeholders for navigation history. While the
  Freebuff terminal is visible, Previous is available as **Back to chat**.
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
  accessible descriptions. Settings can occupy the middle pane while the
  sidebar and inspector remain in place.
  In Terminals, keep the selected task's read-only Codex command output
  alongside the interactive terminal panes. Follow the latest command by
  default and let the user select an earlier command without shifting focus
  when new output arrives.
  Resize the inspector at its left edge by dragging or with the arrow keys
  while the separator is focused. Save the chosen width with the task.
- Place the status bar below the wrapper on the window background. Its
  transparent, borderless surface keeps it visually separate from the panes.

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
  groups controls under **Workspace**, **Projects**, and **Appearance**.
  **Settings → Projects** offers the same per-source import buttons even when
  the sidebar already contains projects.

### Add project picker

- Exclude `~/.claude`, `~/.codex`, and `~/ComfyUI-Installs` from the home scan
  before forming groups.
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

- In a project with no visible tasks, show the bold 30 px prompt **What should
  {agent} build in {project}?** centred both horizontally and vertically in
  the area above the composer. Keep the composer ready for input when the
  project opens. The first nonblank input creates a draft and sending the
  prompt starts the chat.
- The agent and project names are inline pickers. Keep the surrounding
  headline typography, underline each name, and add a small down caret. The
  triggers have transparent backgrounds and no borders. Use Shepherd's dark
  popover styling for the expanded menus. The project picker switches the
  selected project. Codex is the default agent. **Freebuff (terminal)** opens
  a standalone terminal that fills the conversation pane. Use **Back to chat**
  in the window header to close it. Other unavailable agents are disabled until
  their integrations are ready.

## Conversation lifecycle

- Show **Cancel turn** in the conversation header during an active turn. The
  shortcut is Command+. on macOS and Ctrl+. on Linux. Show **Cancelling…**
  until the adapter settles the turn, and pause approval actions during that
  interval. When the server withdraws an approval, retain its arguments and
  show **withdrawn** in place of the actions.
- Show **Resume** beside the state in the conversation header when the task is
  interrupted. Enable the composer when the core reports the task is ready.

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
- The safety picker shows a small shield containing `>_`, the current value
  **Ask me**, and a down caret immediately after the text. Reserve a 128 px
  slot wide enough for **Approve for me** so changes to the value leave sibling
  positions stable. Inside that slot, fit the button and its hover and click
  areas to the shield, label, and caret. **Ask me** is the default. **Approve for
  me** is a disabled upcoming option.
- Show the selected model name on the model and effort trigger. Shepherd's
  Codex default is GPT-6.1 Terra (`gpt-6.1-terra`). Before a session, display
  the model resolved from Shepherd's launch configuration. Once a session
  starts, use the model reported by that session. Use **Codex default** if
  the model name is unavailable.
- Send sits inside the card, at the end of the lower row. Use the compact
  right-pointing arrowhead icon and a bright accent when enabled. The current
  button size is 32 × 30 px. The disabled state uses a muted tint of the same
  accent. **Settings → Appearance** offers Blue, Violet, Teal, Amber, and
  Neutral. Blue is the default. The choice persists across regular `shep`
  launches and lasts for the window session in `shep --test`.

Use the [desktop launch modes](../../README.md#run) to review the UI with
disposable data. `shep` keeps a separate test database between launches and
`shep --test` starts a fresh database each launch.
