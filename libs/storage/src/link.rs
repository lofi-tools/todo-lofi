//! Task-to-task relationships.
//!
//! Two link kinds exist today:
//! - [`LinkKind::BlockedBy`]: hard dependency. A task is blocked while any
//!   of its blockers is unfinished; finishing or reopening a blocker
//!   unblocks or re-blocks dependants automatically because blocked state
//!   is computed, never stored.
//! - [`LinkKind::After`]: soft ordering hint ("do A after B"). Managed
//!   from the task details UI; the ordering function that consumes it
//!   comes later.
//!
//! Links form a DAG per kind: adding a link that would close a cycle is
//! rejected with [`crate::error::QueryErr::LinkCycle`].

use crate::{QueryResult, TodoStore};
use snafu::ResultExt;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkKind {
    BlockedBy,
    After,
}

impl LinkKind {
    fn as_str(self) -> &'static str {
        match self {
            LinkKind::BlockedBy => "blocked_by",
            LinkKind::After => "after",
        }
    }
}

impl TodoStore {
    async fn link_graph(&mut self, kind: LinkKind) -> QueryResult<HashMap<u64, Vec<u64>>> {
        let rows = toasty::sql::query(
            r#"SELECT task_id, other_id FROM task_links WHERE kind = ?1"#,
        )
        .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::I64])
        .bind(kind.as_str())
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "load task link graph",
        })?;

        let mut graph: HashMap<u64, Vec<u64>> = HashMap::new();
        for row in rows {
            if let toasty::stmt::Value::Record(record) = row {
                let from = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                let to = record.get(1).and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                graph.entry(from).or_default().push(to);
            }
        }
        Ok(graph)
    }

    /// True if adding `task_id -> other_id` of `kind` would close a cycle,
    /// i.e. `task_id` is already reachable from `other_id`.
    pub async fn link_would_create_cycle(
        &mut self,
        task_id: u64,
        other_id: u64,
        kind: LinkKind,
    ) -> QueryResult<bool> {
        if task_id == other_id {
            return Ok(true);
        }
        let graph = self.link_graph(kind).await?;
        let mut seen: HashSet<u64> = HashSet::new();
        let mut queue: VecDeque<u64> = VecDeque::from([other_id]);
        while let Some(current) = queue.pop_front() {
            if current == task_id {
                return Ok(true);
            }
            if !seen.insert(current) {
                continue;
            }
            if let Some(next) = graph.get(&current) {
                queue.extend(next.iter().copied());
            }
        }
        Ok(false)
    }

    async fn add_link(&mut self, task_id: u64, other_id: u64, kind: LinkKind) -> QueryResult<()> {
        if self.link_would_create_cycle(task_id, other_id, kind).await? {
            return crate::error::LinkCycleSnafu {
                task_id,
                other_id,
                kind: kind.as_str().to_string(),
            }
            .fail();
        }
        toasty::sql::statement(
            r#"INSERT INTO task_links (task_id, other_id, kind) VALUES (?1, ?2, ?3)
               ON CONFLICT (task_id, other_id, kind) DO NOTHING"#,
        )
        .bind(task_id as i64)
        .bind(other_id as i64)
        .bind(kind.as_str())
        .exec(&mut self.db)
        .await
        .context(crate::error::AddTaskLinkSnafu {
            task_id,
            other_id,
        })?;
        Ok(())
    }

    async fn remove_link(&mut self, task_id: u64, other_id: u64, kind: LinkKind) -> QueryResult<()> {
        toasty::sql::statement(
            r#"DELETE FROM task_links WHERE task_id = ?1 AND other_id = ?2 AND kind = ?3"#,
        )
        .bind(task_id as i64)
        .bind(other_id as i64)
        .bind(kind.as_str())
        .exec(&mut self.db)
        .await
        .context(crate::error::RemoveTaskLinkSnafu {
            task_id,
            other_id,
        })?;
        Ok(())
    }

    async fn linked_ids(&mut self, task_id: u64, kind: LinkKind) -> QueryResult<Vec<u64>> {
        let rows = toasty::sql::query(
            r#"SELECT other_id FROM task_links WHERE task_id = ?1 AND kind = ?2 ORDER BY other_id"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(task_id as i64)
        .bind(kind.as_str())
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list task links",
        })?;

        let mut ids = Vec::with_capacity(rows.len());
        for row in rows {
            if let toasty::stmt::Value::Record(record) = row {
                if let Some(id) = record.first().and_then(|v| v.to_i64()) {
                    ids.push(id as u64);
                }
            }
        }
        Ok(ids)
    }

    pub async fn add_blocker(&mut self, task_id: u64, blocker_id: u64) -> QueryResult<()> {
        self.add_link(task_id, blocker_id, LinkKind::BlockedBy)
            .await
    }

    pub async fn remove_blocker(&mut self, task_id: u64, blocker_id: u64) -> QueryResult<()> {
        self.remove_link(task_id, blocker_id, LinkKind::BlockedBy)
            .await
    }

    pub async fn blocker_ids(&mut self, task_id: u64) -> QueryResult<Vec<u64>> {
        self.linked_ids(task_id, LinkKind::BlockedBy).await
    }

    /// Blocker tasks with full details, ordered by id.
    pub async fn list_blockers(&mut self, task_id: u64) -> QueryResult<Vec<crate::Task>> {
        let ids = self.blocker_ids(task_id).await?;
        let mut tasks = Vec::with_capacity(ids.len());
        for id in ids {
            tasks.push(self.get_task(id).await?);
        }
        Ok(tasks)
    }

    pub async fn add_after_link(&mut self, task_id: u64, other_id: u64) -> QueryResult<()> {
        self.add_link(task_id, other_id, LinkKind::After).await
    }

    pub async fn remove_after_link(&mut self, task_id: u64, other_id: u64) -> QueryResult<()> {
        self.remove_link(task_id, other_id, LinkKind::After).await
    }

    pub async fn after_ids(&mut self, task_id: u64) -> QueryResult<Vec<u64>> {
        self.linked_ids(task_id, LinkKind::After).await
    }

    /// Tasks that must be done before `task_id` ("do this after those"),
    /// with full details, ordered by id.
    pub async fn list_after_tasks(&mut self, task_id: u64) -> QueryResult<Vec<crate::Task>> {
        let ids = self.after_ids(task_id).await?;
        let mut tasks = Vec::with_capacity(ids.len());
        for id in ids {
            tasks.push(self.get_task(id).await?);
        }
        Ok(tasks)
    }

    /// Candidates that may become blockers of `task_id`: every task except
    /// itself, its current blockers, and tasks that would close a cycle.
    pub async fn blocker_candidates(
        &mut self,
        task_id: u64,
    ) -> QueryResult<Vec<crate::Task>> {
        self.link_candidates(task_id, LinkKind::BlockedBy).await
    }

    /// Candidates that may be linked "after" `task_id`: every task except
    /// itself, its current after links, and tasks that would close a cycle.
    pub async fn after_candidates(
        &mut self,
        task_id: u64,
    ) -> QueryResult<Vec<crate::Task>> {
        self.link_candidates(task_id, LinkKind::After).await
    }

    /// Candidates that may be linked from `task_id` with `kind`: every task
    /// except itself, its current links, and tasks that would close a cycle.
    async fn link_candidates(&mut self, task_id: u64, kind: LinkKind) -> QueryResult<Vec<crate::Task>> {
        let current: HashSet<u64> = self.linked_ids(task_id, kind).await?.into_iter().collect();
        let tasks = self.list_tasks().await?;
        let graph = self.link_graph(kind).await?;
        let mut out = Vec::new();
        for task in tasks {
            if task.id == task_id || current.contains(&task.id) {
                continue;
            }
            // Reachable from candidate back to task_id means adding the
            // edge would close a cycle.
            let mut seen: HashSet<u64> = HashSet::new();
            let mut queue: VecDeque<u64> = VecDeque::from([task.id]);
            let mut closes_cycle = false;
            while let Some(current_id) = queue.pop_front() {
                if current_id == task_id {
                    closes_cycle = true;
                    break;
                }
                if !seen.insert(current_id) {
                    continue;
                }
                if let Some(next) = graph.get(&current_id) {
                    queue.extend(next.iter().copied());
                }
            }
            if !closes_cycle {
                out.push(task);
            }
        }
        Ok(out)
    }

    /// Whether `task_id` is currently blocked: `blocked_until` lies in the
    /// future or any blocker is unfinished. Pass unix seconds for `now_secs`
    /// so callers (and tests) control time.
    pub async fn is_task_blocked(&mut self, task_id: u64, now_secs: u64) -> QueryResult<bool> {
        let task = self.get_task(task_id).await?;
        if task.blocked_until.is_some_and(|until| until > now_secs) {
            return Ok(true);
        }
        for blocker in self.list_blockers(task_id).await? {
            if !blocker.done {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Refresh the computed [`crate::TaskWithMeta::blocked`] flag. Called
    /// from the tag loaders so every listed task carries fresh state.
    pub async fn load_blocked_flag(
        &mut self,
        task: &mut crate::TaskWithMeta,
        now_secs: u64,
    ) -> QueryResult<()> {
        task.blocked = self.is_task_blocked(task.id, now_secs).await?;
        Ok(())
    }

    pub async fn update_blocked_until(
        &mut self,
        id: u64,
        blocked_until: Option<u64>,
    ) -> QueryResult<()> {
        crate::Task::update_by_id(id)
            .blocked_until(blocked_until)
            .exec(&mut self.db)
            .await
            .context(crate::error::UpdateTaskSnafu { id })?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use crate::TodoStore;

    async fn make_task(storage: &mut TodoStore, title: &str) -> crate::Task {
        storage
            .create_task(crate::Task::create().title(title.to_string()))
            .await
            .expect("create task")
    }

    async fn set_done(storage: &mut TodoStore, id: u64, done: bool) {
        storage.update_task_done(id, done).await.expect("set done");
    }

    #[tokio::test]
    async fn test_blocker_blocks_until_done() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let first = make_task(&mut storage, "first").await;
        let second = make_task(&mut storage, "second").await;

        storage.add_blocker(second.id, first.id).await?;
        assert!(storage.is_task_blocked(second.id, 1000).await?);
        assert!(!storage.is_task_blocked(first.id, 1000).await?);

        set_done(&mut storage, first.id, true).await;
        assert!(!storage.is_task_blocked(second.id, 1000).await?);

        // Reopening the blocker re-blocks the dependant.
        set_done(&mut storage, first.id, false).await;
        assert!(storage.is_task_blocked(second.id, 1000).await?);
        Ok(())
    }

    #[tokio::test]
    async fn test_multiple_blockers_need_all_done() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let first = make_task(&mut storage, "first").await;
        let second = make_task(&mut storage, "second").await;
        let third = make_task(&mut storage, "third").await;

        storage.add_blocker(third.id, first.id).await?;
        storage.add_blocker(third.id, second.id).await?;
        assert!(storage.is_task_blocked(third.id, 1000).await?);

        set_done(&mut storage, first.id, true).await;
        assert!(storage.is_task_blocked(third.id, 1000).await?);

        set_done(&mut storage, second.id, true).await;
        assert!(!storage.is_task_blocked(third.id, 1000).await?);
        Ok(())
    }

    #[tokio::test]
    async fn test_blocker_cycles_rejected() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let first = make_task(&mut storage, "first").await;
        let second = make_task(&mut storage, "second").await;
        let third = make_task(&mut storage, "third").await;

        storage.add_blocker(second.id, first.id).await?;
        storage.add_blocker(third.id, second.id).await?;

        // Direct and transitive cycles, plus self-links.
        assert!(storage.add_blocker(first.id, second.id).await.is_err());
        assert!(storage.add_blocker(first.id, third.id).await.is_err());
        assert!(storage.add_blocker(first.id, first.id).await.is_err());

        // Unrelated edges still work.
        let fourth = make_task(&mut storage, "fourth").await;
        storage.add_blocker(fourth.id, first.id).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_blocked_until_time() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let task = make_task(&mut storage, "timed").await;

        storage.update_blocked_until(task.id, Some(2000)).await?;
        assert!(storage.is_task_blocked(task.id, 1000).await?);
        assert!(!storage.is_task_blocked(task.id, 2000).await?);
        assert!(!storage.is_task_blocked(task.id, 3000).await?);

        storage.update_blocked_until(task.id, None).await?;
        assert!(!storage.is_task_blocked(task.id, 1000).await?);
        Ok(())
    }

    #[tokio::test]
    async fn test_blocker_candidates_exclude_cycles_and_current() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let first = make_task(&mut storage, "first").await;
        let second = make_task(&mut storage, "second").await;
        let third = make_task(&mut storage, "third").await;

        storage.add_blocker(second.id, first.id).await?;
        // For `first`: `second` would close a cycle, `first` is itself.
        let candidates = storage.blocker_candidates(first.id).await?;
        let ids: Vec<u64> = candidates.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![third.id]);

        // Current blockers are excluded too.
        storage.add_blocker(third.id, first.id).await?;
        let candidates = storage.blocker_candidates(third.id).await?;
        let ids: Vec<u64> = candidates.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![second.id]);
        Ok(())
    }

    #[tokio::test]
    async fn test_after_links_multiple_and_cycles() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let first = make_task(&mut storage, "first").await;
        let second = make_task(&mut storage, "second").await;
        let third = make_task(&mut storage, "third").await;

        // Multiple "after" links for one task are allowed.
        storage.add_after_link(first.id, second.id).await?;
        storage.add_after_link(first.id, third.id).await?;
        let after = storage.after_ids(first.id).await?;
        assert_eq!(after, vec![second.id, third.id]);
        let after_tasks = storage.list_after_tasks(first.id).await?;
        assert_eq!(
            after_tasks.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![second.id, third.id]
        );

        // Direct and transitive cycles, plus self-links, are rejected.
        assert!(storage.add_after_link(second.id, first.id).await.is_err());
        assert!(storage.add_after_link(third.id, first.id).await.is_err());
        assert!(storage.add_after_link(first.id, first.id).await.is_err());

        // Removing one link keeps the other.
        storage.remove_after_link(first.id, second.id).await?;
        let after = storage.after_ids(first.id).await?;
        assert_eq!(after, vec![third.id]);
        Ok(())
    }

    #[tokio::test]
    async fn test_after_candidates_exclude_cycles_and_current() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let first = make_task(&mut storage, "first").await;
        let second = make_task(&mut storage, "second").await;
        let third = make_task(&mut storage, "third").await;

        storage.add_after_link(second.id, first.id).await?;
        // For `first`: `second` would close a cycle, `first` is itself.
        let candidates = storage.after_candidates(first.id).await?;
        let ids: Vec<u64> = candidates.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![third.id]);

        // Current after links are excluded too.
        storage.add_after_link(third.id, first.id).await?;
        let candidates = storage.after_candidates(third.id).await?;
        let ids: Vec<u64> = candidates.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![second.id]);
        Ok(())
    }
}
