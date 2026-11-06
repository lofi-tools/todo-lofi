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
        .map(|command| CommandBinding {
            id: command.id,
            label: command.label,
            category: command.category,
            chord: match platform {
                KeyPlatform::Mac => command.mac,
                KeyPlatform::Linux => command.linux,
            },
            scope: command.scope,
            requirements: command
                .requires
                .into_iter()
                .map(|condition| CommandRequirement {
                    condition,
                    reason: condition.reason(),
                })
                .collect(),
        })
        .collect())
}

#[derive(Serialize)]
pub struct CommandRegistry {
    pub commands: Vec<CommandBinding>,
    pub chords: BTreeMap<ShortcutScope, BTreeMap<String, String>>,
}

pub fn command_registry(platform: KeyPlatform) -> serde_json::Result<CommandRegistry> {
    use ShortcutScope::{CommitForm, Composer, Window, WorkspaceTerminal};

    let commands = command_bindings(platform)?;
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
    -> serde_json::Result<()> {
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
}
