//! Focus and input mode owned by the core, so shells do not resolve shortcuts.

use crate::tasks::TaskId;

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
