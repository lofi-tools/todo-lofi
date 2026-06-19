use dson::{
    CausalDotStore, Identifier, OrMap,
    crdts::{NoExtensionTypes, TypeVariantValue, mvreg::MvRegValue},
};
use uuid::Uuid;

/// A unique identifier for a task within the CRDT
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub Uuid);
impl TaskId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}
impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}
impl From<TaskId> for String {
    fn from(value: TaskId) -> Self {
        value.0.to_string()
    }
}

/// Represents the state of a single task
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    pub done: bool,
}
impl Task {
    pub fn new(title: &str) -> Self {
        Self {
            id: TaskId::new(),
            title: title.to_string(),
            done: false,
        }
    }

    pub fn id(&self) -> TaskId {
        self.id
    }
}

pub struct UntypedTask(pub TypeVariantValue<NoExtensionTypes>);
impl UntypedTask {
    // pub fn new(title: &str) -> Self {
    //     Self(TypeVariantValue {
    //         // reg: MvRegValue::String(title.to_string()),
    //         reg: MvReg(dson::DotFun{}),
    //         ..Default::default(),
    //     })
    // }
}

/// The todo-list CRDT using DSON
#[derive(Debug)]
pub struct TodoList {
    store: CausalDotStore<OrMap<String>>,
    // store2: CausalDotStore<OrMap<TaskId, Task>>,
    my_actor_id: Identifier,
}

impl TodoList {
    /// Create a new empty todo list
    pub fn new(my_actor_id: Identifier) -> Self {
        Self {
            store: CausalDotStore::<OrMap<String>>::default(),
            // next_id: 0,
            my_actor_id,
        }
    }

    /// Add a new task to the list
    pub fn add_task(&mut self, title: String) -> Task {
        // let id = Identifier::new(0, self.next_id);
        // let task_idx = self.next_id as usize;
        // self.next_id += 1;
        let task = Task::new(&title);

        let _delta = {
            let mut tx = self.store.transact(self.my_actor_id);
            tx.in_map("tasks", |task_tx| {
                task_tx.in_map(task.id(), |task_tx| {
                    task_tx.write_register("title", MvRegValue::String(title));
                    task_tx.write_register("done", MvRegValue::Bool(false));
                });
            });
            tx.commit()
        };

        task
    }

    pub fn get_task(&self, _task_id: TaskId) -> anyhow::Result<Task> {
        // let got = self.store.store.get(&task_id)
        let _tasks = match self.store.store.get("tasks") {
            Some(v) => &v.map,
            None => return Err(anyhow::anyhow!("No tasks found")),
        };

        todo!()
    }

    pub fn list_tasks(&self) -> Vec<Task> {
        // for (key, value) in self.store.store.inner().iter() {
        //     println!("Key: {}, Value: {:?}", key, value);
        // }

        // let tasks = self.store.store.get("tasks").unwrap();
        let tasks = match self.store.store.get("tasks") {
            Some(v) => &v.map,
            None => return Vec::new(),
        };

        for (key, value) in tasks.inner().iter() {
            println!("Key: {}, Task: {:?}", key, value);
        }

        // match self.store.store.get("tasks") {
        //     Some(tasks) => {
        //         dbg!(&tasks);
        //         // for (key, value) in tasks {
        //         //     println!("Key: {}, Value: {:?}", key, value);
        //         // }
        //         serde_json::from_value(tasks).expect("Failed to deserialize tasks")
        //     }
        //     None => Vec::new(),
        // }
        todo!()
    }
    pub fn tasks(&self) -> Vec<Task> {
        todo!()
        // serde_json::from_value::<Vec<MyItem>>(json_val).expect("Failed to deserialize")
    }

    //     /// Mark a task as done or not done
    //     pub fn set_done(&mut self, task_id: TaskId, done: bool) {
    //         let id = Identifier::new(0, task_id.0);
    //         let task_idx = task_id.0 as usize;

    //         let mut tx = self.store.transact(id);

    //         tx.in_map("tasks", |tasks_tx| {
    //             tasks_tx.in_array("tasks", |tasks_array_tx| {
    //                 tasks_array_tx.insert_map(task_idx, |task_tx| {
    //                     task_tx.write_register("done", MvRegValue::Bool(done));
    //                 });
    //             });
    //         });

    //         let _delta = tx.commit();
    //     }

    //     /// Delete a task from the list
    //     pub fn delete_task(&mut self, task_id: TaskId) {
    //         let id = Identifier::new(0, task_id.0);
    //         let task_idx = task_id.0 as usize;

    //         let mut tx = self.store.transact(id);

    //         tx.in_map("tasks", |tasks_tx| {
    //             tasks_tx.in_array("tasks", |tasks_array_tx| {
    //                 tasks_array_tx.remove(task_idx);
    //             });
    //         });

    //         let _delta = tx.commit();
    //     }

    // /// Get a single task by ID
    // pub fn get_task(&self, task_id: TaskId) -> Option<Task> {
    //     use dson::transaction::CrdtValue;

    //     let id = Identifier::new(0, 0);
    //     let tx = self.store.transact(id);

    //     let tasks_val = tx.get(&"tasks".to_string())?;
    //     let CrdtValue::Map(tasks_map) = tasks_val else {
    //         return None;
    //     };

    //     let tasks_array_val = tasks_map.get(&"tasks".to_string())?;
    //     let TypeVariantValue::Array(tasks_array) = tasks_array_val.clone() else {
    //         return None;
    //     };

    //     let task_entry = tasks_array.get(task_id.0 as usize)?;
    //     let task_data = &task_entry.map;

    //     let title_val = task_data.get(&"title".to_string())?;
    //     let done_val = task_data.get(&"done".to_string())?;

    //     let title = title_val.reg.value().ok()?.to_string();
    //     let done = done_val.reg.value().ok()?.as_bool()?;

    //     Some(Task {
    //         id: task_id,
    //         title,
    //         done,
    //     })
    // }

    //     /// Get all tasks in the list
    //     pub fn get_tasks(&self) -> Vec<Task> {
    //         let id = Identifier::new(0, 0);
    //         let tx = self.store.transact(id);

    //         let tasks = tx.get(&"tasks".to_string());
    //         if tasks.is_none() {
    //             return Vec::new();
    //         }

    //         let tasks_map = tasks.unwrap().as_map()?;
    //         let tasks_array_val = tasks_map.get(&"tasks".to_string());
    //         if tasks_array_val.is_none() {
    //             return Vec::new();
    //         }

    //         let tasks_array = tasks_array_val.unwrap().as_array()?;
    //         let mut result = Vec::new();
    //         for (idx, task_entry) in tasks_array.iter().enumerate() {
    //             let task_data = &task_entry.map;

    //             let title = task_data
    //                 .get(&"title".to_string())
    //                 .and_then(|v| v.reg.value().ok().map(|v| v.as_ref().to_string()));

    //             let done = task_data
    //                 .get(&"done".to_string())
    //                 .and_then(|v| v.reg.value().ok().and_then(|v| v.as_ref().as_bool().ok()));

    //             if let (Some(title), Some(done)) = (title, done) {
    //                 result.push(Task {
    //                     id: TaskId(idx as u16),
    //                     title,
    //                     done,
    //                 });
    //             }
    //         }

    //         result
    //     }
}

#[cfg(test)]
mod tests {
    use dson::sentinel::DummySentinel;

    use super::*;

    #[test]
    fn try_dson() {
        // Create a new CausalDotStore containing an OrMap.
        let mut doc: CausalDotStore<OrMap<String>> = CausalDotStore::new();
        let id = Identifier::new(0, 0);

        // Create a delta to insert a value.
        let delta = doc.store.apply_to_register(
            |reg, cc, id| reg.write(MvRegValue::U64(42), cc, id),
            "key".into(),
            &doc.context,
            id,
        );

        // Merge the delta into the document.
        doc = doc.join(delta, &mut DummySentinel).unwrap();

        // The value can now be read from the map.
        let val = doc.store.get("key").unwrap();
        use dson::crdts::snapshot::ToValue;
        assert_eq!(val.reg.value().unwrap(), &MvRegValue::U64(42));
    }

    #[test]
    fn test_add_task() {
        let id = Identifier::new(0, 0);
        let mut list = TodoList::new(id);
        let _ = list.add_task("Test task".to_string());
        let _ = list.add_task("Test task".to_string());

        list.list_tasks();

        // assert_eq!(task., 0);

        // let task = list.get_task(task_id).expect("Task should exist");
        // assert_eq!(task.title, "Test task");
        // assert!(!task.done);
    }

    // #[test]
    // fn test_set_done() {
    //     let mut list = TodoList::new();
    //     let task_id = list.add_task("Test task".to_string());

    //     list.set_done(task_id, true);

    //     let task = list.get_task(task_id).expect("Task should exist");
    //     assert!(task.done);
    // }

    // #[test]
    // fn test_delete_task() {
    //     let mut list = TodoList::new();
    //     let task_id = list.add_task("Test task".to_string());

    //     list.delete_task(task_id);

    //     assert!(list.get_task(task_id).is_none());
    // }

    // #[test]
    // fn test_get_tasks() {
    //     let mut list = TodoList::new();
    //     list.add_task("First task".to_string());
    //     list.add_task("Second task".to_string());

    //     let tasks = list.get_tasks();
    //     assert_eq!(tasks.len(), 2);
    //     assert_eq!(tasks[0].title, "First task");
    //     assert_eq!(tasks[1].title, "Second task");
    // }

    // #[test]
    // fn test_multiple_operations() {
    //     let mut list = TodoList::new();

    //     let task1_id = list.add_task("Task 1".to_string());
    //     let task2_id = list.add_task("Task 2".to_string());

    //     list.set_done(task1_id, true);

    //     let tasks = list.get_tasks();
    //     assert_eq!(tasks.len(), 2);
    //     assert!(tasks[0].done);
    //     assert!(!tasks[1].done);

    //     list.delete_task(task2_id);

    //     let tasks = list.get_tasks();
    //     assert_eq!(tasks.len(), 1);
    //     assert_eq!(tasks[0].title, "Task 1");
    // }
}
