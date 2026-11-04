# Customization

How far a user (or their agent) can change the app.

## Common to both

- Themes and appearance: fonts, colors, terminal colors, spacing, sounds.
- Configurable keys and a documented set of commands or actions.
- Settings persisted to disk, with a visible error when a file is invalid.
- Per-repository and per-user configuration that survives updates.
- Appearance that respects reduced transparency and increase-contrast
  preferences.

## herdr-app only

- **The whole interface is files.** Layouts, keys, settings and mods live under
  the app's config folder and are redrawn live within a second; the agents list,
  spaces sidebar, tab bar, menus, dialogs and notifications are all mods built
  from the same API users get.
- **Mods** are JavaScript describing GPUI elements (no DOM, HTML, CSS, web view or
  Node APIs) that can run programs, read files, fetch URLs, keep per-mod or
  shared state, open dialogs, popups and floating blocks, embed live terminals and
  animate natively.
- A hard reset (`ctrl-alt-shift-escape`) restores the default layout and keys, so
  a broken customization cannot lock a user out; a broken edit keeps the last
  good version and writes `<name>.error`.
- `herdr-app check <mod folder>` validates a mod once without laying it out;
  shipped layouts and mods are replaced on start while personal copies stay.
- Themes ship as JSON (`atom-one-dark`, `win98`), and layouts can carry their own
  frame and chrome.
- Agent-facing documentation is generated into the config folder
  (`defaults/AGENTS.md`) and checked by tests against the command and settings
  surface, so the customization guide cannot drift from the app.

## pandacode only

- **Design system as a contract**: one set of `--ds-*` and `--accent*` tokens, one
  glass material for floating surfaces, no literal greys or accent colors in
  components, with contrast checked by tests across themes and wallpapers.
- **VS Code theme import** (colors only, nothing executed) that also sets surface
  tokens and holds still when wallpapers change.
- **Wallpapers and procedural scenes**: eight shader scenes with a sky that
  follows the clock, per-thread scene selection and crossfades, plus reduced-motion
  and low-power behaviour.
- Agent-level customization rather than app-level: **skills library and rules
  registry**, provider connections and credentials, bot channels and rules, named
  team profiles, approval modes, token-saver packs and schedules.
- Appearance follows platform accessibility settings (Reduce Transparency,
  Increase Contrast, forced colors) with solid fallbacks, and motion never gates
  input.

## Divergence worth knowing

Herdr-app makes the application programmable and keeps its core deliberately
small; PandaCode keeps the chrome fixed and makes the design system and the
agents configurable. This is the sharpest architectural split between the two:
one bets on users writing the UI, the other bets on a curated UI with deep agent
configuration.
