use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

type TaskMap = HashMap<String, String>;

/// An Operation defines a single change to the task database, as stored locally in the replica.
///
/// Operations are the means by which changes are made to the database, typically batched together
/// into [`Operations`] and committed to the replica.
#[derive(PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub enum Operation {
    /// Create a new task.
    ///
    /// On undo, the task is deleted.
    Create { uuid: Uuid },

    /// Delete an existing task.
    ///
    /// On undo, the task's data is restored from old_task.
    Delete { uuid: Uuid, old_task: TaskMap },

    /// Update an existing task, setting the given property to the given value.  If the value is
    /// None, then the corresponding property is deleted.
    ///
    /// On undo, the property is set back to its previous value.
    Update {
        uuid: Uuid,
        property: String,
        old_value: Option<String>,
        value: Option<String>,
        timestamp: DateTime<Utc>,
    },

    /// Mark a point in the operations history to which the user might like to undo.  Users
    /// typically want to undo more than one operation at a time (for example, most changes update
    /// both the `modified` property and some other task property -- the user would like to "undo"
    /// both updates at the same time).  Applying an UndoPoint does nothing.
    UndoPoint,
}
