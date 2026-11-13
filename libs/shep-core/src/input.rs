//! Focus and input mode owned by the core, so shells do not resolve shortcuts.

use std::collections::BTreeMap;

use crate::tasks::TaskId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    Chat,
    Composer,
    Inspector,
    Terminal,
    Palette,
    Modal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Focus {
    pub mode: InputMode,
    pub task_id: Option<TaskId>,
}

impl Default for Focus {
    fn default() -> Self {
        Self {
            mode: InputMode::Chat,
            task_id: None,
        }
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyPlatform {
    Mac,
    Linux,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum ShortcutScope {
    #[default]
    Window,
    Composer,
    WorkspaceTerminal,
    CommitForm,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandCondition {
    Idle,
    Projects,
    Project,
    Tasks,
    Task,
    Prompt,
    Cancel,
    Worktree,
    TerminalReady,
    TerminalPane,
    TerminalMultiple,
    TerminalFour,
    FocusedTerminal,
    GitWritable,
    Diagnostics,
}

impl CommandCondition {
    fn reason(self) -> &'static str {
        match self {
            Self::Idle => "Wait for the current operation to finish.",
            Self::Projects => "Add a project first.",
            Self::Project => "Select an available project.",
            Self::Tasks => "This project has no visible tasks.",
            Self::Task => "Select a task.",
            Self::Prompt => "Enter a prompt or attach a file to a ready task.",
            Self::Cancel => "No cancellable turn is active.",
            Self::Worktree => "Select a task with a worktree.",
            Self::TerminalReady => "Wait for the terminal layout to load.",
            Self::TerminalPane => "Open a workspace terminal pane.",
            Self::TerminalMultiple => "Open at least two terminal panes.",
            Self::TerminalFour => "Open four visible terminal panes.",
            Self::FocusedTerminal => "Focus a terminal input before opening the palette.",
            Self::GitWritable => "Select a task with a worktree and no active turn.",
            Self::Diagnostics => "Open a task with agent diagnostics.",
        }
    }
}

#[derive(Deserialize)]
struct WorkspaceCommand {
    id: String,
    label: String,
    category: String,
    mac: Option<String>,
    linux: Option<String>,
    #[serde(default)]
    scope: ShortcutScope,
    #[serde(default)]
    requires: Vec<CommandCondition>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandBinding {
    pub id: String,
    pub label: String,
    pub category: String,
    pub chord: Option<String>,
    pub default_chord: Option<String>,
    pub overridden: bool,
    pub scope: ShortcutScope,
    pub requirements: Vec<CommandRequirement>,
}

#[derive(Serialize)]
pub struct CommandRequirement {
    pub condition: CommandCondition,
    pub reason: &'static str,
}

fn command_bindings(platform: KeyPlatform) -> serde_json::Result<Vec<CommandBinding>> {
    let commands: Vec<WorkspaceCommand> =
        serde_json::from_str(include_str!("workspace-commands.json"))?;
    Ok(commands
        .into_iter()
        .map(|command| {
            let chord = match platform {
                KeyPlatform::Mac => command.mac,
                KeyPlatform::Linux => command.linux,
            };
            CommandBinding {
                id: command.id,
                label: command.label,
                category: command.category,
                default_chord: chord.clone(),
                chord,
                overridden: false,
                scope: command.scope,
                requirements: command
                    .requires
                    .into_iter()
                    .map(|condition| CommandRequirement {
                        condition,
                        reason: condition.reason(),
                    })
                    .collect(),
            }
        })
        .collect())
}

#[derive(Serialize)]
pub struct CommandRegistry {
    pub commands: Vec<CommandBinding>,
    pub chords: BTreeMap<ShortcutScope, BTreeMap<String, String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum KeybindingError {
    #[error(transparent)]
    Registry(#[from] serde_json::Error),
    #[error("Use one shortcut chord, such as Control+Shift+K.")]
    Chord,
    #[error("Use Control, Alt, or Meta with a key, or use a function key.")]
    Modifier,
    #[error("{0} is reserved by the macOS menu.")]
    Reserved(String),
    #[error("{chord} conflicts between {first} and {second}.")]
    Conflict {
        chord: String,
        first: String,
        second: String,
    },
}

pub fn normalize_binding(platform: KeyPlatform, chord: &str) -> Result<String, KeybindingError> {
    let (modifiers, key) = chord.rsplit_once('+').unwrap_or(("", chord));
    let key = key.trim();
    let function = key
        .strip_prefix('F')
        .and_then(|number| number.parse::<u8>().ok())
        .is_some_and(|number| (1..=24).contains(&number) && key == format!("F{number}"));
    let printable = key.len() == 1 && key.as_bytes().first().is_some_and(u8::is_ascii_graphic);
    let named = matches!(
        key,
        "ArrowDown"
            | "ArrowLeft"
            | "ArrowRight"
            | "ArrowUp"
            | "Backspace"
            | "Delete"
            | "End"
            | "Enter"
            | "Escape"
            | "Home"
            | "Insert"
            | "PageDown"
            | "PageUp"
            | "Plus"
            | "Space"
            | "Tab"
    );
    if !function && !printable && !named {
        return Err(KeybindingError::Chord);
    }

    let mut pressed = [false; 4];
    if chord.contains('+') {
        for modifier in modifiers.split('+').map(str::trim) {
            let pressed = match modifier.to_ascii_lowercase().as_str() {
                "control" | "ctrl" => &mut pressed[0],
                "alt" | "option" => &mut pressed[1],
                "shift" => &mut pressed[2],
                "meta" | "cmd" | "command" => &mut pressed[3],
                _ => return Err(KeybindingError::Chord),
            };
            if *pressed {
                return Err(KeybindingError::Chord);
            }
            *pressed = true;
        }
    }
    if !function && !pressed[0] && !pressed[1] && !pressed[3] {
        return Err(KeybindingError::Modifier);
    }
    let key = if printable {
        key.to_ascii_uppercase()
    } else {
        key.to_owned()
    };
    let mut parts: Vec<_> = ["Control", "Alt", "Shift", "Meta"]
        .into_iter()
        .zip(pressed)
        .filter_map(|(name, pressed)| pressed.then_some(name))
        .collect();
    parts.push(&key);
    let chord = parts.join("+");
    if matches!(platform, KeyPlatform::Mac)
        && matches!(
            chord.as_str(),
            "Meta+A"
                | "Meta+C"
                | "Meta+H"
                | "Meta+M"
                | "Meta+Q"
                | "Meta+V"
                | "Meta+X"
                | "Meta+Z"
                | "Shift+Meta+Z"
                | "Alt+Meta+H"
        )
    {
        return Err(KeybindingError::Reserved(chord));
    }
    Ok(chord)
}

pub fn command_registry(platform: KeyPlatform) -> Result<CommandRegistry, KeybindingError> {
    command_registry_with_overrides(platform, &BTreeMap::new())
}

pub fn command_registry_with_overrides(
    platform: KeyPlatform,
    overrides: &BTreeMap<String, String>,
) -> Result<CommandRegistry, KeybindingError> {
    use ShortcutScope::{CommitForm, Composer, Window, WorkspaceTerminal};

    let mut commands = command_bindings(platform)?;
    for binding in &mut commands {
        if let Some(chord) = overrides.get(&binding.id) {
            binding.chord = Some(normalize_binding(platform, chord)?);
            binding.overridden = true;
        }
    }
    for (index, first) in commands.iter().enumerate() {
        let Some(chord) = &first.chord else { continue };
        if let Some(second) = commands.iter().skip(index + 1).find(|second| {
            second.chord.as_ref() == Some(chord)
                && (first.scope == Window || second.scope == Window || first.scope == second.scope)
        }) {
            return Err(KeybindingError::Conflict {
                chord: chord.clone(),
                first: first.label.clone(),
                second: second.label.clone(),
            });
        }
    }
    let chords = [Window, Composer, WorkspaceTerminal, CommitForm]
        .into_iter()
        .map(|scope| {
            let keys = commands
                .iter()
                .filter(|binding| binding.scope == Window || binding.scope == scope)
                .filter_map(|binding| Some((binding.chord.clone()?, binding.id.clone())))
                .collect();
            (scope, keys)
        })
        .collect();
    Ok(CommandRegistry { commands, chords })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn command_ids_and_chords_are_unique_within_each_scope() -> serde_json::Result<()> {
        for platform in [KeyPlatform::Mac, KeyPlatform::Linux] {
            let bindings = command_bindings(platform)?;
            let mut ids = BTreeSet::new();
            let mut chords = BTreeSet::new();
            for binding in &bindings {
                assert!(ids.insert(&binding.id));
                if let Some(chord) = &binding.chord {
                    assert!(chords.insert((binding.scope, chord)));
                }
            }
            for (scope, chord) in &chords {
                if *scope != ShortcutScope::Window {
                    assert!(!chords.contains(&(ShortcutScope::Window, *chord)));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn command_map_preserves_readline_and_resolves_the_shared_commit_and_send_chord()
    -> Result<(), KeybindingError> {
        for (platform, palette_chord, enter) in [
            (KeyPlatform::Mac, "Meta+K", "Meta+Enter"),
            (KeyPlatform::Linux, "Control+Shift+K", "Control+Enter"),
        ] {
            let registry = command_registry(platform)?;
            let resolve =
                |scope, chord: &str| registry.chords.get(&scope)?.get(chord).map(String::as_str);
            assert_eq!(
                resolve(ShortcutScope::WorkspaceTerminal, palette_chord),
                Some("commandPalette")
            );
            assert_eq!(resolve(ShortcutScope::WorkspaceTerminal, "Control+K"), None);
            assert_eq!(resolve(ShortcutScope::WorkspaceTerminal, enter), None);
            assert_eq!(resolve(ShortcutScope::Composer, enter), Some("sendPrompt"));
            assert_eq!(resolve(ShortcutScope::CommitForm, enter), Some("commit"));
        }
        Ok(())
    }

    #[test]
    fn shortcut_strings_are_single_normalized_chords() -> Result<(), KeybindingError> {
        let platform = KeyPlatform::Mac;
        assert_eq!(
            normalize_binding(platform, "Shift + Ctrl + k")?,
            "Control+Shift+K"
        );
        assert_eq!(normalize_binding(platform, "F9")?, "F9");
        assert_eq!(
            normalize_binding(platform, "Ctrl+Shift+Plus")?,
            "Control+Shift+Plus"
        );
        assert!(matches!(
            normalize_binding(platform, "Shift+T"),
            Err(KeybindingError::Modifier)
        ));
        assert!(matches!(
            normalize_binding(platform, "Cmd+C"),
            Err(KeybindingError::Reserved(_))
        ));
        for invalid in [
            "Control+K Control+P",
            "Control+Control+F",
            "Control++",
            "F25",
            "Alt+Unknown",
        ] {
            assert!(
                matches!(
                    normalize_binding(platform, invalid),
                    Err(KeybindingError::Chord)
                ),
                "{invalid}"
            );
        }
        Ok(())
    }

    #[test]
    fn overrides_replace_default_chords_and_check_conflicts_in_effective_scopes()
    -> Result<(), KeybindingError> {
        let platform = KeyPlatform::Linux;
        let mut overrides = BTreeMap::from([
            ("commandPalette".into(), "F9".into()),
            ("sendPrompt".into(), "Ctrl+Alt+Enter".into()),
            ("commit".into(), "Alt+Ctrl+Enter".into()),
        ]);
        let registry = command_registry_with_overrides(platform, &overrides)?;
        let palette = registry
            .commands
            .iter()
            .find(|binding| binding.id == "commandPalette");
        assert!(palette.is_some_and(|binding| binding.overridden
            && binding.default_chord.as_deref() == Some("Control+Shift+K")));
        let window = &registry.chords[&ShortcutScope::Window];
        assert_eq!(window.get("F9").map(String::as_str), Some("commandPalette"));
        assert!(!window.contains_key("Control+Shift+K"));
        assert_eq!(
            registry.chords[&ShortcutScope::CommitForm]
                .get("Control+Alt+Enter")
                .map(String::as_str),
            Some("commit")
        );
        overrides.insert("toggleSidebar".into(), "Ctrl+Alt+Enter".into());
        assert!(matches!(
            command_registry_with_overrides(platform, &overrides),
            Err(KeybindingError::Conflict { .. })
        ));
        Ok(())
    }
}
