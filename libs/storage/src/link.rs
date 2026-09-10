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

    /// Direct subtasks of any of `task_ids`, as a map from parent id to
    /// its subtasks (with metadata), ordered by subtask id. Used by the
    /// task list to collapse subtask rows under their parent, show the
    /// first subtask inline, and expand the rest behind the N/M counter.
    pub async fn subtasks_map(
        &mut self,
        task_ids: &[u64],
    ) -> QueryResult<HashMap<u64, Vec<crate::TaskWithMeta>>> {
        if task_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let id_list: Vec<String> = task_ids.iter().map(|id| id.to_string()).collect();
        let placeholders: Vec<&str> = id_list.iter().map(|s| s.as_str()).collect();
        let rows = toasty::sql::query(format!(
            "SELECT id, parent_id FROM tasks WHERE parent_id IN ({}) AND deleted_at IS NULL AND (blocked_until IS NULL OR blocked_until <= CAST(strftime('%s', 'now') AS INTEGER)) ORDER BY id",
            placeholders.join(",")
        ))
        .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::I64])
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "subtasks map",
        })?;
        let mut grouped: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut all_subtasks: Vec<u64> = Vec::new();
        for row in rows {
            if let toasty::stmt::Value::Record(record) = row {
                let id = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                let parent = record.get(1).and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                grouped.entry(parent).or_default().push(id);
                all_subtasks.push(id);
            }
        }
        let meta = self.load_meta_map(&all_subtasks).await?;
        let mut map = HashMap::with_capacity(grouped.len());
        for (parent, subtask_ids) in grouped {
            let tasks: Vec<crate::TaskWithMeta> = subtask_ids
                .into_iter()
                .filter_map(|id| meta.get(&id).cloned())
                .collect();
            map.insert(parent, tasks);
        }
        Ok(map)
    }

    /// Tasks whose parent is `parent_id` (subtasks), with full details,
    /// ordered by id.
    pub async fn list_subtasks(&mut self, parent_id: u64) -> QueryResult<Vec<crate::Task>> {
        let rows = toasty::sql::query(
            r#"SELECT id FROM tasks WHERE parent_id = ?1 AND deleted_at IS NULL AND (blocked_until IS NULL OR blocked_until <= CAST(strftime('%s', 'now') AS INTEGER)) ORDER BY id"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(parent_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list subtasks",
        })?;
        let mut tasks = Vec::with_capacity(rows.len());
        for row in rows {
            if let toasty::stmt::Value::Record(record) = row {
                if let Some(id) = record.first().and_then(|v| v.to_i64()) {
                    tasks.push(self.get_task(id as u64).await?);
                }
            }
        }
        Ok(tasks)
    }

    /// Tasks that are blocked by `task_id` (the reverse of its blockers),
    /// with full details, ordered by id. Used for the "linked to" list.
    pub async fn list_blocking_tasks(&mut self, task_id: u64) -> QueryResult<Vec<crate::Task>> {
        let rows = toasty::sql::query(
            r#"SELECT task_id FROM task_links WHERE other_id = ?1 AND kind = 'blocked_by' ORDER BY task_id"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(task_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list tasks blocked by task",
        })?;
        let mut tasks = Vec::with_capacity(rows.len());
        for row in rows {
            if let toasty::stmt::Value::Record(record) = row {
                if let Some(id) = record.first().and_then(|v| v.to_i64()) {
                    tasks.push(self.get_task(id as u64).await?);
                }
            }
        }
        Ok(tasks)
    }

    /// Load task metadata for every id in `ids`, one query per id but
    /// shared by the two map helpers below.
    async fn load_meta_map(&mut self, ids: &[u64]) -> QueryResult<HashMap<u64, crate::TaskWithMeta>> {
        let mut map = HashMap::with_capacity(ids.len());
        for &id in ids {
            map.insert(id, self.get_task_with_meta(id).await?);
        }
        Ok(map)
    }

    /// All tasks blocked by any of `task_ids`, as a map from blocker id to
    /// its blocked tasks (with metadata). One query over `task_links`;
    /// used by the task list to nest blocked tasks under their blocker.
    pub async fn blocking_map(
        &mut self,
        task_ids: &[u64],
    ) -> QueryResult<HashMap<u64, Vec<crate::TaskWithMeta>>> {
        if task_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let id_list: Vec<String> = task_ids.iter().map(|id| id.to_string()).collect();
        let placeholders: Vec<&str> = id_list.iter().map(|s| s.as_str()).collect();
        let rows = toasty::sql::query(format!(
            "SELECT task_id, other_id FROM task_links WHERE kind = 'blocked_by' AND other_id IN ({})",
            placeholders.join(",")
        ))
        .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::I64])
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "blocking map",
        })?;
        let mut grouped: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut all_blocked: Vec<u64> = Vec::new();
        for row in rows {
            if let toasty::stmt::Value::Record(record) = row {
                let blocked = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                let blocker = record.get(1).and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                grouped.entry(blocker).or_default().push(blocked);
                all_blocked.push(blocked);
            }
        }
        let meta = self.load_meta_map(&all_blocked).await?;
        let mut map = HashMap::with_capacity(grouped.len());
        for (blocker, blocked_ids) in grouped {
            let tasks: Vec<crate::TaskWithMeta> = blocked_ids
                .into_iter()
                .filter_map(|id| meta.get(&id).cloned())
                .collect();
            map.insert(blocker, tasks);
        }
        Ok(map)
    }

    /// All blockers of any of `task_ids`, as a map from blocked task id to
    /// its blocker tasks (with metadata). Used to decide which tasks nest
    /// under their blocker and to unblock dependants locally when a
    /// blocker completes.
    pub async fn blockers_map(
        &mut self,
        task_ids: &[u64],
    ) -> QueryResult<HashMap<u64, Vec<crate::TaskWithMeta>>> {
        if task_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let id_list: Vec<String> = task_ids.iter().map(|id| id.to_string()).collect();
        let placeholders: Vec<&str> = id_list.iter().map(|s| s.as_str()).collect();
        let rows = toasty::sql::query(format!(
            "SELECT task_id, other_id FROM task_links WHERE kind = 'blocked_by' AND task_id IN ({})",
            placeholders.join(",")
        ))
        .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::I64])
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "blockers map",
        })?;
        let mut grouped: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut all_blockers: Vec<u64> = Vec::new();
        for row in rows {
            if let toasty::stmt::Value::Record(record) = row {
                let blocked = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                let blocker = record.get(1).and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                grouped.entry(blocked).or_default().push(blocker);
                all_blockers.push(blocker);
            }
        }
        let meta = self.load_meta_map(&all_blockers).await?;
        let mut map = HashMap::with_capacity(grouped.len());
        for (blocked, blocker_ids) in grouped {
            let tasks: Vec<crate::TaskWithMeta> = blocker_ids
                .into_iter()
                .filter_map(|id| meta.get(&id).cloned())
                .collect();
            map.insert(blocked, tasks);
        }
        Ok(map)
    }

    /// Create a follow-up task for `source_task`: the new task cannot be
    /// worked on until the source is done, records the source in
    /// `source_task_id`, and copies the source's native (direct) tags as
    /// native tags of its own. Inferred and inherited tags are not copied.
    pub async fn create_follow_up(
        &mut self,
        source_task: u64,
        title: String,
    ) -> QueryResult<crate::Task> {
        let task = self
            .create_task(
                crate::Task::create()
                    .title(title)
                    .source_task_id(Some(source_task)),
            )
            .await?;
        self.add_blocker(task.id, source_task).await?;
        let source_tags = self.get_direct_task_tags(source_task).await?;
        for tag in source_tags {
            self.assign_tag_to_task(task.id, &tag.name).await?;
        }
        Ok(task)
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

    #[tokio::test]
    async fn test_blocking_and_blockers_maps() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let first = make_task(&mut storage, "first").await;
        let second = make_task(&mut storage, "second").await;
        let third = make_task(&mut storage, "third").await;

        storage.add_blocker(second.id, first.id).await?;
        storage.add_blocker(third.id, first.id).await?;

        // blocking_map: first blocks second and third.
        let blocking = storage.blocking_map(&[first.id]).await?;
        let blocked = blocking.get(&first.id).unwrap();
        let titles: Vec<&str> = blocked.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["second", "third"]);

        // blockers_map: second's blocker is first.
        let blockers = storage.blockers_map(&[second.id, third.id]).await?;
        assert_eq!(blockers[&second.id][0].title, "first");
        assert_eq!(blockers[&third.id][0].title, "first");

        // Completing the blocker unblocks dependants (meta flags refresh).
        set_done(&mut storage, first.id, true).await;
        let blocking = storage.blocking_map(&[first.id]).await?;
        assert!(
            !blocking[&first.id][0].blocked,
            "dependant should be unblocked once its blocker is done"
        );
        assert!(
            !blocking[&first.id][1].blocked,
            "second dependant should also be unblocked"
        );
        Ok(())
    }

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

    #[tokio::test]
    async fn test_subtasks_map_by_parent() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let parent = make_task(&mut storage, "parent").await;
        let other = make_task(&mut storage, "other").await;

        let first = storage
            .create_task(
                crate::Task::create()
                    .title("first child".to_string())
                    .parent_id(Some(parent.id)),
            )
            .await?;
        storage
            .create_task(
                crate::Task::create()
                    .title("second child".to_string())
                    .parent_id(Some(parent.id)),
            )
            .await?;

        let map = storage.subtasks_map(&[parent.id, other.id]).await?;
        let children = map.get(&parent.id).unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].title, "first child");
        assert_eq!(children[1].title, "second child");
        assert!(!map.contains_key(&other.id), "no subtasks -> absent");

        // Completing a subtask is reflected in its metadata.
        storage.update_task_done(first.id, true).await?;
        let map = storage.subtasks_map(&[parent.id]).await?;
        let children = map.get(&parent.id).unwrap();
        assert!(children.iter().any(|t| t.id == first.id && t.done));

        assert!(storage.subtasks_map(&[]).await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_list_subtasks_by_parent() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let parent = make_task(&mut storage, "parent").await;
        let other = make_task(&mut storage, "other").await;

        storage
            .create_task(
                crate::Task::create()
                    .title("first child".to_string())
                    .parent_id(Some(parent.id)),
            )
            .await?;
        storage
            .create_task(
                crate::Task::create()
                    .title("second child".to_string())
                    .parent_id(Some(parent.id)),
            )
            .await?;

        let subtasks = storage.list_subtasks(parent.id).await?;
        assert_eq!(subtasks.len(), 2);
        assert!(subtasks.iter().all(|t| t.parent_id == Some(parent.id)));
        assert_eq!(subtasks[0].title, "first child");
        assert_eq!(subtasks[1].title, "second child");

        // A task without children lists none.
        assert!(storage.list_subtasks(other.id).await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_follow_up_blocked_by_parent_and_listed() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let parent = make_task(&mut storage, "parent").await;

        let follow_up = storage
            .create_follow_up(parent.id, "follow up".to_string())
            .await?;

        // The follow-up is blocked by the parent.
        assert!(storage.is_task_blocked(follow_up.id, 1000).await?);
        assert!(!storage.is_task_blocked(parent.id, 1000).await?);

        // The parent's "linked to" list contains the follow-up.
        let blocking = storage.list_blocking_tasks(parent.id).await?;
        assert_eq!(
            blocking.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![follow_up.id]
        );

        // Completing the parent unblocks the follow-up.
        set_done(&mut storage, parent.id, true).await;
        assert!(!storage.is_task_blocked(follow_up.id, 1000).await?);
        Ok(())
    }

    #[tokio::test]
    async fn test_follow_up_copies_source_tags_and_records_source() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let tag_a = storage.create_tag("TagA").await?;
        let tag_b = storage.create_tag("TagB").await?;
        let source = make_task(&mut storage, "source").await;
        storage.assign_tag_to_task(source.id, &tag_a.name).await?;
        storage.assign_tag_to_task(source.id, &tag_b.name).await?;

        let follow_up = storage
            .create_follow_up(source.id, "follow up".to_string())
            .await?;

        // The source is recorded on the follow-up (and survives reloads
        // through both the ORM and the raw list queries).
        assert_eq!(follow_up.source_task_id, Some(source.id));
        let fetched = storage.get_task(follow_up.id).await?;
        assert_eq!(fetched.source_task_id, Some(source.id));
        let listed = storage.list_tasks_by_priority().await?;
        let listed_follow_up = listed.iter().find(|t| t.id == follow_up.id).unwrap();
        assert_eq!(listed_follow_up.task.source_task_id, Some(source.id));

        // The source's native tags are copied as native tags, not inherited.
        let copied = storage.get_direct_task_tags(follow_up.id).await?;
        let names: Vec<String> = copied.iter().map(|t| t.name.clone()).collect();
        assert!(names.contains(&tag_a.name));
        assert!(names.contains(&tag_b.name));
        let meta = storage.get_task_with_meta(follow_up.id).await?;
        assert!(meta.direct_tags.contains(&tag_a.label()));
        assert!(meta.inherited_tags.is_empty(), "copied tags must be native");

        // The source's own direct tags are unchanged.
        let source_tags = storage.get_direct_task_tags(source.id).await?;
        assert_eq!(source_tags.len(), 2);
        Ok(())
    }
}
