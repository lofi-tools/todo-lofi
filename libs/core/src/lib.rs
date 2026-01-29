//! Core library for the todo application

pub mod operation;

pub struct AppState {
    // replica: taskchampion::Replica,
}

pub struct Task {}

impl AppState {
    pub fn new() -> Self {
        // For now, create a simple empty state
        // TODO: Integrate with taskchampion when ready
        //     let replica = Replica::new(StorageConfig::InMemory.into_storage().unwrap());
        //     Self { replica }
        Self {}
    }

    // Method to get all todos (read-only)
    pub fn get_all(&self) -> Vec<Task> {
        // self.replica.all_tasks()
        //     .into_iter()
        //     .map(|task| Todo {
        //         id: task.get_uuid().to_string(),
        //         title: task.get_description().to_string(),
        //         completed: task.is_completed(),
        //         created_at: task.get_created().timestamp(),
        //         updated_at: task.get_modified().map(|t| t.timestamp())
        //     })
        //     .collect()
        todo!()
    }

    pub fn add(&mut self, task: Task) {
        // let mut task = self.replica.new_task(todo.title);
        // if todo.completed {
        //     task.set_completed();
        // }
        // self.replica.add_task(task);
        todo!()
    }

    pub fn remove(&mut self, id: &str) -> Option<Task> {
        // if let Ok(uuid) = uuid::Uuid::parse_str(id) {
        //     if let Some(task) = self.replica.get_task(uuid) {
        //         let todo = Todo {
        //             id: task.get_uuid().to_string(),
        //             title: task.get_description().to_string(),
        //             completed: task.is_completed(),
        //             created_at: task.get_created().timestamp(),
        //             updated_at: task.get_modified().map(|t| t.timestamp())
        //         };
        //         self.replica.delete_task(uuid);
        //         return Some(todo);
        //     }
        // }
        // None
        todo!()
    }

    pub fn toggle(&mut self, id: &str) -> bool {
        // if let Ok(uuid) = uuid::Uuid::parse_str(id) {
        //     if let Some(mut task) = self.replica.get_task(uuid) {
        //         if task.is_completed() {
        //             task.set_pending();
        //         } else {
        //             task.set_completed();
        //         }
        //         self.replica.update_task(task);
        //         return true;
        //     }
        // }
        // false
        todo!()
    }
}

#[cfg(test)]
mod tests {
    // use super::*;

    // #[test]
    // fn test_todo_list_operations_yrs() {
    //     let mut list = AppState::new();
    //     let todo1 = Todo {
    //         id: "1".to_string(),
    //         title: "Test todo 1".to_string(),
    //         completed: false,
    //         created_at: 100,
    //         updated_at: None,
    //     };
    //     let todo2 = Todo {
    //         id: "2".to_string(),
    //         title: "Test todo 2".to_string(),
    //         completed: true,
    //         created_at: 200,
    //         updated_at: Some(250),
    //     };

    //     // Add todos
    //     list.add(todo1.clone());
    //     list.add(todo2.clone());

    //     // Verify initial state
    //     let current_todos = list.get_all();
    //     assert_eq!(current_todos.len(), 2);
    //     assert!(current_todos.contains(&todo1));
    //     assert!(current_todos.contains(&todo2));

    //     // Toggle todo 1
    //     assert!(list.toggle("1"));
    //     let todos_after_toggle = list.get_all();
    //     let toggled_todo = todos_after_toggle.iter().find(|t| t.id == "1").unwrap();
    //     assert!(toggled_todo.completed);
    //     assert_eq!(toggled_todo.title, "Test todo 1"); // Ensure other fields are intact

    //     // Remove todo 2
    //     let removed = list.remove("2").unwrap();
    //     assert_eq!(removed, todo2);
    //     let todos_after_remove = list.get_all();
    //     assert_eq!(todos_after_remove.len(), 1);
    //     assert_eq!(todos_after_remove[0].id, "1");
    //     assert!(todos_after_remove[0].completed); // State from toggle should persist

    //     // Try removing non-existent todo
    //     assert!(list.remove("3").is_none());
    //     assert_eq!(list.get_all().len(), 1); // Count should remain 1

    //     // Try toggling non-existent todo
    //     assert!(!list.toggle("3"));
    // }
}
