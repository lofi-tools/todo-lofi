# Files and previews

Reviewing what an agent produced without leaving the window.

## Common to both

- See the repository's files and open them without switching to an editor.
- See which files changed, and view the change as a diff.
- A place to preview an app or page the agent just built.
- Multiple windows/panels can show different views of the same work at once.

## herdr-app only

- Files, changes, picker and peek are **shipped mods**, not app core, so their
  layout and behaviour are editable like any other panel.
- The `changes` mod and `files` mod are lists with actions; the `pane` and `peek`
  mods place surfaces. Diff rendering is basic compared with a review tool.
- `herdr-app shot [file.png]` captures the last rendered window frame, which is
  the app's own screenshot path for UI inspection rather than a preview of the
  project.
- Terminals inside a mod can show a live program (a server, a TUI) as a block,
  which is how Herdr previews running work.
- No embedded browser and no design or simulator surfaces.

## pandacode only

- **Files and search** tabs in the inspector, with file icons and `@` file
  mentions in the composer.
- **Changes** tab with unified and split diffs, one Tab stop per diff, plus
  per-turn saved changes so review is scoped to what a turn touched.
- **Browser** tab: an embedded browser for the running app, with captures
  available to the agent as image attachments.
- **Design canvas** and **moodboards** for visual work, and a built-in mockup
  sketcher that feeds sketches into a prompt.
- **Simulator** tab showing an iOS Simulator or Android emulator, so a mobile
  build can be tapped through beside the conversation.
- **VM** tab drawing a live remote/VM screen from raw frames.
- File previews from the transcript chips (documents, folders, images) and an
  embedded preview for run/preview projects.
- Attachments from the operating system: images, PDFs, documents, folders and
  sketches, rendered read-only under the user's message.

## Divergence worth knowing

Herdr-app's review surfaces are thin by design and replaceable through mods; its
answer to "show me the app" is a terminal running it. PandaCode has purpose-built
review surfaces (diff stages, browser, simulators, VMs) and connects them to the
task record. Shepherd deciding whether review is core or a mod is the same decision
as how much of the window to make programmable.
