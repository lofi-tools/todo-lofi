//! Core library for the todo application using DSON CRDT

use dson::{
    CausalContext, CausalDotStore, Identifier, OrMap,
    crdts::{mvreg::MvRegValue, snapshot::ToValue},
    dot_fun::DotFun,
    sentinel::{DummySentinel, Sentinel, ValueSentinel},
    traits::{DotStore, DotStoreJoin},
    transaction::CrdtValue,
    types::{DotChange, DryJoinOutput},
};

pub mod operation;

/// A unique task identifier
pub type TaskId = String;

/// Task properties stored in the CRDT
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    pub completed: bool,
    pub created_at: u64,
    pub updated_at: u64,
}

/// AppState uses DSON CRDT for distributed state management
pub struct AppState {
    store: CausalDotStore<OrMap<String>>,
    local_id: u8,
    version: u16,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            store: CausalDotStore::default(),
            local_id: 0,
            version: 0,
        }
    }

    /// Get all tasks from the CRDT store
    pub fn get_all(&self) -> Vec<Task> {
        let id = Identifier::new(self.local_id, self.version);
        let tx = self.store.transact(id);
        let mut tasks = Vec::new();

        // Iterate through all task entries in the store
        if let Some(root) = tx.get(&"tasks".to_string()) {
            if let CrdtValue::Map(task_map) = root {
                for (task_id, task_entry) in task_map.iter() {
                    if let Some(task_data) = Self::parse_task(task_id, task_entry) {
                        tasks.push(task_data);
                    }
                }
            }
        }

        tasks
    }

    /// Parse a task entry from the CRDT map
    fn parse_task(task_id: &str, task_entry: &dson::crdts::map::MapEntry<String>) -> Option<Task> {
        let map = &task_entry.map;

        let title = map
            .get(&"title".to_string())?
            .reg
            .value()
            .and_then(|v| match v {
                MvRegValue::String(s) => Some(s.clone()),
                _ => None,
            })?;

        let completed = map
            .get(&"completed".to_string())?
            .reg
            .value()
            .and_then(|v| match v {
                MvRegValue::Bool(b) => Some(b),
                _ => None,
            })?;

        let created_at = map
            .get(&"created_at".to_string())?
            .reg
            .value()
            .and_then(|v| match v {
                MvRegValue::U64(n) => Some(*n),
                _ => None,
            })?;

        let updated_at = map
            .get(&"updated_at".to_string())?
            .reg
            .value()
            .and_then(|v| match v {
                MvRegValue::U64(n) => Some(*n),
                _ => None,
            })?;

        Some(Task {
            id: task_id.to_string(),
            title,
            completed: completed?,
            created_at: created_at?,
            updated_at: updated_at?,
        })
    }

    /// Add a new task to the CRDT store
    pub fn add(&mut self, task: Task) {
        let id = Identifier::new(self.local_id, self.version);
        self.version = self.version.wrapping_add(1);
        let mut tx = self.store.transact(id);

        tx.in_map("tasks", |tasks_tx| {
            tasks_tx.in_map(&task.id, |task_tx| {
                task_tx.write_register("title", MvRegValue::String(task.title));
                task_tx.write_register("completed", MvRegValue::Bool(task.completed));
                task_tx.write_register("created_at", MvRegValue::U64(task.created_at));
                task_tx.write_register("updated_at", MvRegValue::U64(task.updated_at));
            });
        });

        let _delta = tx.commit();
    }

    /// Remove a task from the CRDT store
    pub fn remove(&mut self, id: &str) -> Option<Task> {
        let task = self.get_all().into_iter().find(|t| t.id == id)?;

        let id = Identifier::new(self.local_id, self.version);
        self.version = self.version.wrapping_add(1);
        let mut tx = self.store.transact(id);

        tx.in_map("tasks", |tasks_tx| {
            tasks_tx.remove(id);
        });

        let _delta = tx.commit();

        Some(task)
    }

    /// Toggle a task's completed status
    pub fn toggle(&mut self, id: &str) -> bool {
        let task = match self.get_all().into_iter().find(|t| t.id == id) {
            Some(t) => t,
            None => return false,
        };

        let id = Identifier::new(self.local_id, self.version);
        self.version = self.version.wrapping_add(1);
        let mut tx = self.store.transact(id);

        tx.in_map("tasks", |tasks_tx| {
            tasks_tx.in_map(&task.id, |task_tx| {
                task_tx.write_register("completed", MvRegValue::Bool(!task.completed));
                task_tx.write_register(
                    "updated_at",
                    MvRegValue::U64(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                    ),
                );
            });
        });

        let _delta = tx.commit();
        true
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_state_add_task() {
        let mut store = AppState::new();

        let task = Task {
            id: "task-1".to_string(),
            title: "Test task".to_string(),
            completed: false,
            created_at: 1000,
            updated_at: 1000,
        };

        store.add(task.clone());
        let tasks = store.get_all();

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "task-1");
        assert_eq!(tasks[0].title, "Test task");
        assert!(!tasks[0].completed);
    }

    #[test]
    fn test_app_state_toggle_task() {
        let mut store = AppState::new();

        let task = Task {
            id: "task-1".to_string(),
            title: "Test task".to_string(),
            completed: false,
            created_at: 1000,
            updated_at: 1000,
        };

        store.add(task);

        // Toggle to completed
        store.toggle("task-1");
        let tasks = store.get_all();
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].completed);

        // Toggle back to not completed
        store.toggle("task-1");
        let tasks = store.get_all();
        assert!(!tasks[0].completed);
    }

    #[test]
    fn test_app_state_remove_task() {
        let mut store = AppState::new();

        let task1 = Task {
            id: "task-1".to_string(),
            title: "Task 1".to_string(),
            completed: false,
            created_at: 1000,
            updated_at: 1000,
        };

        let task2 = Task {
            id: "task-2".to_string(),
            title: "Task 2".to_string(),
            completed: false,
            created_at: 2000,
            updated_at: 2000,
        };

        store.add(task1);
        store.add(task2);

        assert_eq!(store.get_all().len(), 2);

        let removed = store.remove("task-1");
        assert!(removed.is_some());
        assert_eq!(store.get_all().len(), 1);

        let remaining = store.get_all()[0].clone();
        assert_eq!(remaining.id, "task-2");
    }
}
