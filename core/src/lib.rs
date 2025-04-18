//! Core library for the todo application

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub id: String,
    pub title: String,
    pub completed: bool,
    pub created_at: i64,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoList {
    pub todos: Vec<Todo>,
}

impl TodoList {
    pub fn new() -> Self {
        Self { todos: Vec::new() }
    }

    pub fn add(&mut self, todo: Todo) {
        self.todos.push(todo);
    }

    pub fn remove(&mut self, id: &str) -> Option<Todo> {
        if let Some(pos) = self.todos.iter().position(|t| t.id == id) {
            Some(self.todos.remove(pos))
        } else {
            None
        }
    }

    pub fn toggle(&mut self, id: &str) -> bool {
        if let Some(todo) = self.todos.iter_mut().find(|t| t.id == id) {
            todo.completed = !todo.completed;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_todo_list_operations() {
        let mut list = TodoList::new();
        let todo = Todo {
            id: "1".to_string(),
            title: "Test todo".to_string(),
            completed: false,
            created_at: 0,
            updated_at: None,
        };

        list.add(todo.clone());
        assert_eq!(list.todos.len(), 1);

        assert!(list.toggle("1"));
        assert!(list.todos[0].completed);

        let removed = list.remove("1").unwrap();
        assert_eq!(removed.id, "1");
        assert!(list.todos.is_empty());
    }
}
