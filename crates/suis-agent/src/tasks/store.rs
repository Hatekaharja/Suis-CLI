//! In-memory, session-scoped task storage.

use super::types::{Task, TaskStatus};

/// A session's task list. Not persisted: a fresh store accompanies each
/// session and is discarded when it ends.
#[derive(Debug, Clone, Default)]
pub struct TaskStore {
    tasks: Vec<Task>,
    next: u64,
}

impl TaskStore {
    /// An empty store.
    pub fn new() -> Self {
        TaskStore::default()
    }

    /// Create a new `Todo` task, returning its generated id.
    pub fn create(&mut self, title: impl Into<String>) -> String {
        self.next += 1;
        let id = format!("t{}", self.next);
        self.tasks.push(Task {
            id: id.clone(),
            title: title.into(),
            status: TaskStatus::Todo,
        });
        id
    }

    /// Update the status of an existing task. Returns `false` if no task has
    /// the given id.
    pub fn update(&mut self, id: &str, status: TaskStatus) -> bool {
        match self.tasks.iter_mut().find(|t| t.id == id) {
            Some(task) => {
                task.status = status;
                true
            }
            None => false,
        }
    }

    /// The first task currently `Doing`, if any — the task injected into
    /// context as the agent's focus.
    pub fn active(&self) -> Option<&Task> {
        self.tasks.iter().find(|t| t.status == TaskStatus::Doing)
    }

    /// Look up a task by id.
    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// All tasks in creation order, for UI display.
    pub fn all(&self) -> &[Task] {
        &self.tasks
    }

    /// Whether there are any tasks.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_appears_as_todo() {
        let mut store = TaskStore::new();
        let id = store.create("analyze code");
        assert_eq!(store.all().len(), 1);
        assert_eq!(store.get(&id).unwrap().status, TaskStatus::Todo);
    }

    #[test]
    fn update_to_doing_becomes_active() {
        let mut store = TaskStore::new();
        let id = store.create("write tests");
        assert!(store.active().is_none());
        assert!(store.update(&id, TaskStatus::Doing));
        assert_eq!(store.active().unwrap().id, id);
    }

    #[test]
    fn update_to_done_clears_active() {
        let mut store = TaskStore::new();
        let id = store.create("fix bug");
        store.update(&id, TaskStatus::Doing);
        store.update(&id, TaskStatus::Done);
        assert!(store.active().is_none());
    }

    #[test]
    fn only_first_doing_is_active() {
        let mut store = TaskStore::new();
        let a = store.create("a");
        let b = store.create("b");
        store.update(&a, TaskStatus::Doing);
        store.update(&b, TaskStatus::Doing);
        assert_eq!(store.active().unwrap().id, a);
    }

    #[test]
    fn update_unknown_id_returns_false() {
        let mut store = TaskStore::new();
        assert!(!store.update("nope", TaskStatus::Done));
    }

    #[test]
    fn ids_are_unique() {
        let mut store = TaskStore::new();
        let a = store.create("a");
        let b = store.create("b");
        assert_ne!(a, b);
    }
}
