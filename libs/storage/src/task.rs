use crate::TodoStore;
use derive_entity_id::EntityId;
use snafu::{OptionExt, ResultExt};
use std::collections::{HashMap, HashSet, VecDeque};
use toasty::Deferred;
use toasty::Embed;
use toasty::Model;
use toasty::schema::Model;

// TODO after https://github.com/tokio-rs/toasty/issues/1040: use as key once embed keys work with parent/subtasks relationship
#[derive(EntityId, Embed, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
#[entity_id(prefix = "task")]
pub struct TaskId(u64);

#[derive(Debug, Clone, Model)]
pub struct Task {
    #[key]
    #[auto]
    pub id: u64,
    pub title: String,
    pub description: Option<String>,
    pub branch_name: Option<String>,
    pub labels: Option<toasty::Json<Vec<String>>>,
    pub deadline: Option<u64>,
    /// Epoch seconds until which the task is blocked (time-based block).
    /// `None` means no time block.
    pub blocked_until: Option<u64>,
    #[default(1.0)]
    pub importance_factor: f64,
    #[default(1.0)]
    pub urgency_factor: f64,
    #[default(false)]
    pub done: bool,
    /// Epoch seconds when the task was completed (set when `done` flips to
    /// true, cleared when reopened). Used to sort and eventually hide
    /// completed tasks.
    pub completed_at: Option<u64>,
    #[default(jiff::Timestamp::now())]
    pub created_at: jiff::Timestamp,
    #[update(jiff::Timestamp::now())]
    pub updated_at: jiff::Timestamp,
    #[index]
    pub parent_id: Option<u64>,
    /// The task this one was created as a follow-up of. Only set for
    /// follow-up tasks; plain tasks and subtasks leave it empty.
    pub source_task_id: Option<u64>,
    /// Tombstone for mirrored deletions; rows with this set are hidden.
    pub deleted_at: Option<jiff::Timestamp>,
    /// Original remote timezone string for recurrence/display.
    pub timezone: Option<String>,
    /// One-way imported remote comments as JSON.
    pub comments: Option<toasty::Json<Vec<crate::external::ExternalComment>>>,
    /// The workflow run this task is a step of, if it was created by the
    /// workflow engine. `NULL` for ordinary user tasks. Workflow steps
    /// never sync to any integration.
    pub workflow_run_id: Option<u64>,
    /// The recipe node this task materializes (engine bookkeeping for edge
    /// evaluation and event lookup). Only set on workflow steps.
    pub node_id: Option<String>,
    /// Coding runs only: `'step'` on an engine-materialized run step, so every
    /// subtask surface can tell scaffolding from work. `NULL` for plain tasks,
    /// user- and model-added subtasks, and nested-run roots.
    pub role: Option<String>,
    /// Coding runs only: when the feature spec was accepted as covering this
    /// subtask. An own `spec` always wins over this mark.
    pub spec_covered_at: Option<jiff::Timestamp>,
    #[has_many(pair = parent)]
    pub subtasks: Deferred<Vec<Task>>,
    #[belongs_to(key = parent_id, references = id)]
    pub parent: Deferred<Option<Task>>,
}
impl Task {
    /// Compute priority score matching the SQL formula in `list_tasks_by_priority`.
    pub fn compute_priority_score(&self, now_secs: u64) -> f64 {
        self.importance_factor
            * self.urgency_factor
            * Self::deadline_pressure(self.deadline, now_secs)
    }

    /// Deadline pressure: 86400 / seconds-remaining while approaching,
    /// then keeps growing past the deadline (86400 at the deadline, +1
    /// per overdue second) instead of plateauing.
    fn deadline_pressure(deadline: Option<u64>, now_secs: u64) -> f64 {
        match deadline {
            None => 1.0,
            Some(dl) => {
                let diff = dl as f64 - now_secs as f64;
                if diff >= 1.0 {
                    86400.0_f64 / diff
                } else {
                    86400.0_f64 + (1.0 - diff)
                }
            }
        }
    }
}

/// Compute `(importance, urgency)` for a task inserted between `above`
/// and `below` (list order: higher score first) so it sorts between
/// them. The new task carries no deadline (pressure 1), so its score is
/// just the factor product; the target is the geometric mean of the
/// neighbours' live scores, split across the two factors following the
/// neighbours' balance. Edges double/halve the single neighbour's score
/// (`None`/`None` yields the defaults). Scores drift as neighbours'
/// deadlines approach, so this places the task, it doesn't pin it.
pub fn factors_between(above: Option<&Task>, below: Option<&Task>) -> (f64, f64) {
    const MIN: f64 = 1e-3;
    const MAX: f64 = 1e9;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let score = |task: &Task| {
        (task.importance_factor
            * task.urgency_factor
            * Task::deadline_pressure(task.deadline, now_secs))
        .clamp(MIN, MAX)
    };
    let target = match (above, below) {
        (Some(a), Some(b)) => (score(a) * score(b)).sqrt(),
        (None, Some(b)) => score(b) * 2.0,
        (Some(a), None) => score(a) / 2.0,
        (None, None) => 1.0,
    }
    .clamp(MIN, MAX);
    // Neighbours' per-factor balance (geometric mean; a missing side
    // reuses the present one), scaled so the product hits the target.
    let geo = |pick: fn(&Task) -> f64| match (above, below) {
        (Some(a), Some(b)) => (pick(a).clamp(MIN, MAX) * pick(b).clamp(MIN, MAX)).sqrt(),
        (Some(a), None) | (None, Some(a)) => pick(a).clamp(MIN, MAX),
        (None, None) => 1.0,
    };
    let (base_imp, base_urg) = (
        geo(|task| task.importance_factor),
        geo(|task| task.urgency_factor),
    );
    let scale = (target / (base_imp * base_urg).clamp(MIN, MAX)).sqrt();
    (
        (base_imp * scale).clamp(MIN, MAX),
        (base_urg * scale).clamp(MIN, MAX),
    )
}

pub type TaskCreate = <Task as toasty::schema::Model>::Create;

#[derive(Debug, Clone)]
pub struct TaskWithMeta {
    pub task: Task,
    /// The task's spec markdown, read from the namespaced `task_extra` store
    /// (`coding` / `spec`). `None` in list queries, which do not load extra
    /// data; the spec surfaces all go through `get_task_with_meta`.
    pub spec: Option<String>,
    pub direct_tags: Vec<String>,
    /// Direct tags of the task's ancestor chain, so a subtask is
    /// automatically tagged like its parents. Computed on load, never
    /// stored, and not directly modifiable.
    pub inherited_tags: Vec<String>,
    pub inferred_tags: Vec<String>,
    /// Most specific tags only: ancestors implied by another tag on the
    /// same task are omitted. Used for display in the task list.
    pub leaf_tags: Vec<String>,
    /// True when the task currently cannot be worked on: an unfinished
    /// blocker exists or `blocked_until` lies in the future. Computed on
    /// load, so reopening a blocker re-blocks dependants automatically.
    pub blocked: bool,
    /// The app that owns this task, if any (see [`crate::managed`]).
    pub managed_by: Option<u64>,
    /// Owning app's display label, for "Managed by …" tooltips.
    pub managed_label: Option<String>,
    /// Whether the app *owns* the task or merely captures (propagates) it.
    pub managed_mode: Option<crate::managed::ManagedMode>,
    /// The owning app's editability setting; remote edits also set it.
    pub managed_editable: bool,
    /// A local content edit made the task the user's: regeneration and app
    /// removal spare it.
    pub user_modified: bool,
}

impl TaskWithMeta {
    /// True when the task's content fields are locked to the user because an
    /// app owns it and has not made it editable.
    pub fn is_managed_read_only(&self) -> bool {
        self.managed_by.is_some()
            && self.managed_mode == Some(crate::managed::ManagedMode::Managed)
            && !self.managed_editable
    }
}
impl std::ops::Deref for TaskWithMeta {
    type Target = Task;
    fn deref(&self) -> &Self::Target {
        &self.task
    }
}
impl TaskWithMeta {
    pub fn priority_score(&self, now_secs: u64) -> f64 {
        self.importance_factor * self.urgency_factor * self.deadline_factor(now_secs)
    }

    pub fn deadline_factor(&self, now_secs: u64) -> f64 {
        Task::deadline_pressure(self.deadline, now_secs)
    }
}

fn parse_task_from_row(record: &toasty::stmt::Value) -> crate::QueryResult<TaskWithMeta> {
    let toasty::stmt::Value::Record(record) = record else {
        unreachable!("raw SQL queries return record rows");
    };

    let id =
        record
            .first()
            .and_then(|v| v.to_i64())
            .context(crate::error::UnexpectedValueSnafu {
                message: "expected i64 for id",
            })? as u64;
    let title = record
        .get(1)
        .and_then(|v| v.as_str())
        .context(crate::error::UnexpectedValueSnafu {
            message: "expected string for title",
        })?
        .to_owned();
    let description = record.get(2).and_then(|v| v.as_str()).map(str::to_owned);
    let branch_name = record.get(3).and_then(|v| v.as_str()).map(str::to_owned);
    let labels = record
        .get(4)
        .and_then(|v| v.as_str())
        .map(|s| toasty::Json(serde_json::from_str(s).unwrap_or_default()));
    let deadline = record.get(6).and_then(|v| v.to_u64());
    let importance_factor = record.get(7).and_then(|v| v.to_f64()).unwrap_or(1.0);
    let urgency_factor = record.get(8).and_then(|v| v.to_f64()).unwrap_or(1.0);
    let done_raw = record.get(9);
    let done = match done_raw {
        Some(toasty::stmt::Value::Bool(b)) => *b,
        Some(toasty::stmt::Value::I64(n)) => *n != 0,
        _ => false,
    };
    let created_at = record
        .get(10)
        .and_then(|v| v.as_str())
        .context(crate::error::UnexpectedValueSnafu {
            message: "expected string for created_at",
        })?
        .parse::<jiff::Timestamp>()?;
    let updated_at = record
        .get(11)
        .and_then(|v| v.as_str())
        .context(crate::error::UnexpectedValueSnafu {
            message: "expected string for updated_at",
        })?
        .parse::<jiff::Timestamp>()?;
    let parent_id = record.get(12).and_then(|v| v.to_i64()).map(|id| id as u64);
    let blocked_until = record.get(13).and_then(|v| v.to_u64());
    let source_task_id = record.get(14).and_then(|v| v.to_i64()).map(|id| id as u64);
    let completed_at = record.get(15).and_then(|v| v.to_u64());
    let workflow_run_id = record.get(16).and_then(|v| v.to_i64()).map(|id| id as u64);
    let node_id = record.get(17).and_then(|v| v.as_str()).map(str::to_owned);
    // The role and coverage columns are only selected by queries that need
    // them, so these reads are optional rather than positional requirements.
    let role = record.get(20).and_then(|v| v.as_str()).map(str::to_owned);
    let spec_covered_at = record
        .get(21)
        .and_then(|v| v.as_str())
        .and_then(|raw| raw.parse::<jiff::Timestamp>().ok());

    let task = Task {
        id,
        title,
        description,
        branch_name,
        labels,
        deadline,
        blocked_until,
        importance_factor,
        urgency_factor,
        done,
        completed_at,
        created_at,
        updated_at,
        parent_id,
        source_task_id,
        deleted_at: None,
        timezone: None,
        comments: None,
        workflow_run_id,
        node_id,
        role,
        spec_covered_at,
        subtasks: Deferred::default(),
        parent: Deferred::default(),
    };

    Ok(TaskWithMeta {
        task,
        // Only `get_task_with_meta` loads the namespaced spec; list queries
        // stay a single round trip.
        spec: None,
        direct_tags: Vec::new(),
        inherited_tags: Vec::new(),
        inferred_tags: Vec::new(),
        leaf_tags: Vec::new(),
        blocked: false,
        managed_by: None,
        managed_label: None,
        managed_mode: None,
        managed_editable: false,
        user_modified: false,
    })
}

impl TodoStore {
    #[fastrace::trace]
    pub async fn create_task(
        &mut self,
        create: <Task as Model>::Create,
    ) -> crate::QueryResult<Task> {
        let created = create
            .exec(&mut self.db)
            .await
            .context(crate::error::CreateTaskSnafu)?;
        Ok(created)
    }

    #[fastrace::trace]
    pub async fn update_task_done(&mut self, id: u64, done: bool) -> crate::QueryResult<()> {
        tracing::info!(id, done, "update_task_done: executing");
        let completed_at = if done { Some(Self::now_secs()) } else { None };
        Task::update_by_id(id)
            .done(done)
            .completed_at(completed_at)
            .exec(&mut self.db)
            .await
            .context(crate::error::UpdateTaskSnafu { id })?;
        tracing::info!(id, done, "update_task_done: done");
        // Workflow steps evaluate their outgoing edges on completion
        // (spawning downstream steps, recording results, auto-completing
        // the run). Reopening just flips the flag back; the engine does
        // not un-spawn.
        if done {
            if let Ok(task) = self.get_task(id).await
                && task.workflow_run_id.is_some()
            {
                self.evaluate_workflow_completion(&task).await?;
            }
        }
        Ok(())
    }

    #[fastrace::trace]
    pub async fn update_task_title(&mut self, id: u64, title: &str) -> crate::QueryResult<()> {
        self.guard_and_mark_modified(id).await?;
        Task::update_by_id(id)
            .title(title)
            .exec(&mut self.db)
            .await
            .context(crate::error::UpdateTaskSnafu { id })?;
        Ok(())
    }

    #[fastrace::trace]
    pub async fn update_task_description(
        &mut self,
        id: u64,
        description: Option<String>,
    ) -> crate::QueryResult<()> {
        self.guard_and_mark_modified(id).await?;
        Task::update_by_id(id)
            .description(description)
            .exec(&mut self.db)
            .await
            .context(crate::error::UpdateTaskSnafu { id })?;
        Ok(())
    }

    /// Store a coding feature task's spec artifact in the namespaced
    /// `task_extra` store. `None` leaves the stored value alone. A step row
    /// never carries a spec (decision #31), so it is refused.
    #[fastrace::trace]
    pub async fn save_task_spec(
        &mut self,
        id: u64,
        spec: Option<String>,
    ) -> crate::QueryResult<()> {
        self.set_task_spec(id, spec).await
    }

    /// Reject writes to a run step: a step is scaffolding, never a subtask, so
    /// it has no spec, no coverage mark and no place in a subtask surface.
    pub(crate) async fn ensure_not_a_step(
        &mut self,
        id: u64,
        what: &str,
    ) -> crate::QueryResult<()> {
        let task = self.get_task(id).await?;
        if crate::workflow::is_step(&task) {
            return Err(crate::QueryErr::UnexpectedValue {
                message: format!("task {id} is a workflow step and cannot hold a {what}"),
            });
        }
        Ok(())
    }

    /// Mark `id` as covered by its run's umbrella spec (decision #10). The
    /// mark is set, never cleared by a later own spec: an own spec wins in
    /// `subtask_coverage` and the mark stays as history.
    #[fastrace::trace]
    pub async fn cover_subtask_at(
        &mut self,
        id: u64,
        covered_at: jiff::Timestamp,
    ) -> crate::QueryResult<()> {
        self.ensure_not_a_step(id, "coverage mark").await?;
        Task::update_by_id(id)
            .spec_covered_at(Some(covered_at))
            .exec(&mut self.db)
            .await
            .context(crate::error::UpdateTaskSnafu { id })?;
        Ok(())
    }

    #[fastrace::trace]
    pub async fn get_task(&mut self, id: u64) -> crate::QueryResult<Task> {
        let task = Task::get_by_id(&mut self.db, id)
            .await
            .context(crate::error::GetTaskSnafu { id })?;
        Ok(task)
    }

    /// Full task with tags, leaf tags and computed blocked flag.
    pub async fn get_task_with_meta(&mut self, id: u64) -> crate::QueryResult<TaskWithMeta> {
        let task = self.get_task(id).await?;
        let mut meta = TaskWithMeta {
            task,
            spec: self.get_task_spec(id).await?,
            direct_tags: Vec::new(),
            inherited_tags: Vec::new(),
            inferred_tags: Vec::new(),
            leaf_tags: Vec::new(),
            blocked: false,
            managed_by: None,
            managed_label: None,
            managed_mode: None,
            managed_editable: false,
            user_modified: false,
        };
        self.load_all_meta(&mut meta).await?;
        Ok(meta)
    }

    #[fastrace::trace]
    pub async fn list_tasks(&mut self) -> crate::QueryResult<Vec<Task>> {
        let tasks = Task::all()
            .exec(&mut self.db)
            .await
            .context(crate::error::ListTasksByPrioritySnafu)?;
        Ok(tasks)
    }

    #[fastrace::trace]
    pub async fn delete_task(&mut self, id: u64) -> crate::QueryResult<()> {
        self.assert_task_editable(id).await?;
        Task::delete_by_id(&mut self.db, id)
            .await
            .context(crate::error::DeleteTaskSnafu { id })?;
        // The namespaced extension data goes with the row; nothing enforces
        // the reference in SQL.
        self.delete_task_extra(id).await?;
        Ok(())
    }

    /// How long a completed task stays visible in the task list (sorted to
    /// the bottom) before being hidden from list queries.
    pub const COMPLETED_TASK_VISIBLE_SECS: u64 = 24 * 60 * 60;

    /// ORDER BY clause shared by the list queries: open doable tasks by
    /// priority score (`importance × urgency × deadline pressure`, still
    /// growing past the deadline), then not-yet-doable tasks (future
    /// `blocked_until`, e.g. a recurring start time), then completed
    /// tasks at the bottom (most recently completed first).
    fn priority_order_sql() -> &'static str {
        r#"
            ORDER BY
                done ASC,
                CASE WHEN blocked_until IS NOT NULL
                          AND blocked_until > CAST(strftime('%s', 'now') AS INTEGER)
                     THEN 1 ELSE 0 END ASC,
                CASE WHEN done THEN completed_at ELSE 0 END DESC,
                importance_factor * urgency_factor * CASE
                    WHEN deadline IS NULL THEN 1.0
                    WHEN CAST(deadline AS REAL) - CAST(strftime('%s', 'now') AS REAL) >= 1.0
                        THEN 86400.0 / MAX(1.0,
                            CAST(deadline AS REAL) - CAST(strftime('%s', 'now') AS REAL)
                        )
                    ELSE 86400.0 + (1.0 -
                        (CAST(deadline AS REAL) - CAST(strftime('%s', 'now') AS REAL))
                    )
                END DESC
        "#
    }

    /// WHERE clause shared by the list queries: hide completed tasks that
    /// were finished more than `COMPLETED_TASK_VISIBLE_SECS` ago.
    fn completed_visible_where_sql() -> &'static str {
        "
            AND NOT (done = 1 AND completed_at IS NOT NULL
                AND completed_at < CAST(strftime('%s', 'now') AS INTEGER) - 86400)
        "
    }

    /// WHERE fragment hiding tombstoned rows (mirrored deletions, §4.3).
    /// Takes the table qualifier used by the query (`""` or `"t."`).
    fn not_deleted_where_sql(table: &str) -> String {
        format!("AND {table}deleted_at IS NULL")
    }

    /// WHERE fragment hiding tasks that start more than 2 days out. Near
    /// upcoming tasks stay listed (bottom group); the full list is
    /// available through the `including_distant` variants.
    fn near_only_where_sql(table: &str) -> String {
        format!(
            "AND ({table}blocked_until IS NULL \
             OR {table}blocked_until <= CAST(strftime('%s', 'now') AS INTEGER) + 172800)"
        )
    }

    #[fastrace::trace]
    pub async fn list_tasks_by_priority(&mut self) -> crate::QueryResult<Vec<TaskWithMeta>> {
        self.list_tasks_by_priority_impl(false).await
    }

    /// Full list including tasks that start more than 2 days out (for the
    /// "show all" toggle).
    pub async fn list_tasks_by_priority_including_distant(
        &mut self,
    ) -> crate::QueryResult<Vec<TaskWithMeta>> {
        self.list_tasks_by_priority_impl(true).await
    }

    /// WHERE fragment excluding engine run steps: they belong to a run's
    /// Workflow section, never to a task list.
    fn not_a_step_where_sql(table: &str) -> String {
        format!("AND ({table}role IS NULL OR {table}role <> 'step')")
    }

    async fn list_tasks_by_priority_impl(
        &mut self,
        include_distant: bool,
    ) -> crate::QueryResult<Vec<TaskWithMeta>> {
        let near = if include_distant {
            String::new()
        } else {
            Self::near_only_where_sql("")
        };
        let rows = toasty::sql::query(format!(
            r#"
            SELECT
                id, title, description, branch_name, labels, blocked_by,
                deadline, importance_factor, urgency_factor, done, created_at, updated_at,
                parent_id, blocked_until, source_task_id, completed_at,
                workflow_run_id, node_id
            FROM tasks
            WHERE 1 = 1
            {}
            {}
            {}
            {}
            {}
            "#,
            Self::completed_visible_where_sql(),
            Self::not_deleted_where_sql(""),
            Self::not_a_step_where_sql(""),
            near,
            Self::priority_order_sql(),
        ))
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::F64,
            toasty::stmt::Type::F64,
            toasty::stmt::Type::Bool,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
        ])
        .exec(&mut self.db)
        .await
        .context(crate::error::ListTasksByPrioritySnafu)?;

        let mut tasks = Vec::with_capacity(rows.len());
        for row in rows {
            let mut task = parse_task_from_row(&row)?;
            self.load_all_meta(&mut task).await?;
            tasks.push(task);
        }

        Ok(tasks)
    }

    #[fastrace::trace]
    pub async fn list_tasks_by_tag(
        &mut self,
        tag_id: u64,
    ) -> crate::QueryResult<Vec<TaskWithMeta>> {
        self.list_tasks_by_tag_impl(tag_id, false).await
    }

    /// Full tag list including tasks that start more than 2 days out (for
    /// the "show all" toggle).
    pub async fn list_tasks_by_tag_including_distant(
        &mut self,
        tag_id: u64,
    ) -> crate::QueryResult<Vec<TaskWithMeta>> {
        self.list_tasks_by_tag_impl(tag_id, true).await
    }

    async fn list_tasks_by_tag_impl(
        &mut self,
        tag_id: u64,
        include_distant: bool,
    ) -> crate::QueryResult<Vec<TaskWithMeta>> {
        let mut tag_ids = vec![tag_id];
        let descendants = self.get_all_descendants(tag_id).await?;
        tag_ids.extend(descendants.into_iter().map(|t| t.id));

        let id_list: Vec<String> = tag_ids.iter().map(|id| id.to_string()).collect();
        let placeholders: Vec<&str> = id_list.iter().map(|s| s.as_str()).collect();

        // Tasks carrying the tag (or a descendant tag), plus every task in
        // their subtask tree: subtasks inherit their parent's tags. The
        // closure is walked in Rust because the driver rejects recursive
        // CTEs.
        let seed_rows = toasty::sql::query(format!(
            "SELECT DISTINCT dtt.task_id FROM direct_task_tags dtt WHERE dtt.tag_id IN ({})",
            placeholders.join(",")
        ))
        .column_types([toasty::stmt::Type::I64])
        .exec(&mut self.db)
        .await
        .context(crate::error::ListTasksByTagSnafu { tag_id })?;

        let mut all_ids: HashSet<u64> = seed_rows
            .iter()
            .filter_map(|row| {
                if let toasty::stmt::Value::Record(record) = row {
                    record.first().and_then(|v| v.to_i64()).map(|id| id as u64)
                } else {
                    None
                }
            })
            .collect();

        let link_rows = toasty::sql::query(r#"SELECT id, parent_id FROM tasks"#)
            .column_types([toasty::stmt::Type::I64, toasty::stmt::Type::I64])
            .exec(&mut self.db)
            .await
            .context(crate::error::ListTasksByTagSnafu { tag_id })?;
        let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
        for row in link_rows {
            if let toasty::stmt::Value::Record(record) = row {
                let id = record.first().and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                let parent = record.get(1).and_then(|v| v.to_i64()).unwrap_or(0) as u64;
                children.entry(parent).or_default().push(id);
            }
        }
        let mut queue: VecDeque<u64> = all_ids.iter().copied().collect();
        while let Some(current) = queue.pop_front() {
            if let Some(subtasks) = children.get(&current) {
                for &subtask in subtasks {
                    if all_ids.insert(subtask) {
                        queue.push_back(subtask);
                    }
                }
            }
        }

        if all_ids.is_empty() {
            return Ok(Vec::new());
        }
        let task_id_list: Vec<String> = all_ids.iter().map(|id| id.to_string()).collect();
        let task_placeholders: Vec<&str> = task_id_list.iter().map(|s| s.as_str()).collect();
        let query = format!(
            r#"
            SELECT
                t.id, t.title, t.description, t.branch_name, t.labels, t.blocked_by,
                t.deadline, t.importance_factor, t.urgency_factor, t.done, t.created_at, t.updated_at,
                t.parent_id, t.blocked_until, t.source_task_id, t.completed_at,
                t.workflow_run_id, t.node_id
            FROM tasks t
            WHERE t.id IN ({})
            {}
            {}
            {}
            {}
            {}
            "#,
            task_placeholders.join(","),
            Self::completed_visible_where_sql(),
            Self::not_deleted_where_sql("t."),
            Self::not_a_step_where_sql("t."),
            if include_distant {
                String::new()
            } else {
                Self::near_only_where_sql("t.")
            },
            Self::priority_order_sql()
                .replace("importance_factor", "t.importance_factor")
                .replace("urgency_factor", "t.urgency_factor"),
        );

        let rows = toasty::sql::query(&query)
            .column_types([
                toasty::stmt::Type::I64,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::I64,
                toasty::stmt::Type::F64,
                toasty::stmt::Type::F64,
                toasty::stmt::Type::Bool,
                toasty::stmt::Type::String,
                toasty::stmt::Type::String,
                toasty::stmt::Type::I64,
                toasty::stmt::Type::I64,
                toasty::stmt::Type::I64,
                toasty::stmt::Type::I64,
                toasty::stmt::Type::I64,
                toasty::stmt::Type::String,
            ])
            .exec(&mut self.db)
            .await
            .context(crate::error::ListTasksByTagSnafu { tag_id })?;

        let mut tasks = Vec::with_capacity(rows.len());
        for row in rows {
            let mut task = parse_task_from_row(&row)?;
            self.load_all_meta(&mut task).await?;
            tasks.push(task);
        }

        Ok(tasks)
    }

    pub async fn load_direct_tags(&mut self, task: &mut TaskWithMeta) -> crate::QueryResult<()> {
        let tags = self.get_direct_task_tags(task.id).await?;
        task.direct_tags = tags.iter().map(|t| t.label()).collect();
        Ok(())
    }

    pub async fn load_inherited_tags(&mut self, task: &mut TaskWithMeta) -> crate::QueryResult<()> {
        let tags = self.inherited_task_tags(task.id).await?;
        task.inherited_tags = tags.iter().map(|t| t.label()).collect();
        Ok(())
    }

    /// Inferred and leaf tags over the task's own direct tags plus the
    /// inherited tags of its ancestors, so a subtask carries the same
    /// effective tag set (and leaf display) as its parents.
    pub async fn load_inferred_tags(&mut self, task: &mut TaskWithMeta) -> crate::QueryResult<()> {
        let mut seed: HashSet<u64> = self
            .get_direct_task_tags(task.id)
            .await?
            .into_iter()
            .map(|t| t.id)
            .collect();
        seed.extend(
            self.inherited_task_tags(task.id)
                .await?
                .into_iter()
                .map(|t| t.id),
        );
        let tags = self.inferred_tags_from_seed(&seed).await?;
        task.inferred_tags = tags.iter().map(|t| t.label()).collect();
        let leaves = self.leaf_tags_from_all(&tags).await?;
        task.leaf_tags = leaves.iter().map(|t| t.label()).collect();
        Ok(())
    }

    pub async fn load_all_tags(&mut self, task: &mut TaskWithMeta) -> crate::QueryResult<()> {
        self.load_direct_tags(task).await?;
        self.load_inherited_tags(task).await?;
        self.load_inferred_tags(task).await?;
        Ok(())
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    pub async fn load_blocked(&mut self, task: &mut TaskWithMeta) -> crate::QueryResult<()> {
        self.load_blocked_flag(task, Self::now_secs()).await
    }

    /// Tags plus computed blocked flag: everything list views need.
    pub async fn load_all_meta(&mut self, task: &mut TaskWithMeta) -> crate::QueryResult<()> {
        self.load_all_tags(task).await?;
        self.load_blocked(task).await?;
        self.load_managed(task).await?;
        Ok(())
    }

    /// Who owns this task, and whether it is locked or spared.
    pub async fn load_managed(&mut self, task: &mut TaskWithMeta) -> crate::QueryResult<()> {
        let ownership = self.task_ownership(task.id).await?;
        task.managed_by = ownership.managed_by;
        task.managed_mode = ownership.managed_mode;
        task.managed_editable = ownership.managed_editable;
        task.user_modified = ownership.user_modified;
        task.managed_label = match ownership.managed_by {
            Some(app_id) => self.app_by_id(app_id).await?.map(|app| app.label),
            None => None,
        };
        Ok(())
    }

    /// Guard for local content edits: fails with `TaskLocked` when an app
    /// owns the task and has not made it editable. Completion is exempt.
    pub async fn ensure_task_editable(&mut self, id: u64) -> crate::QueryResult<()> {
        self.assert_task_editable(id).await
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use jiff::Timestamp;

    use crate::prelude::*;
    use std::time::Duration;

    async fn task_with_factors(store: &mut TodoStore, importance: f64, urgency: f64) -> Task {
        store
            .create_task(
                Task::create()
                    .title("t".to_string())
                    .importance_factor(importance)
                    .urgency_factor(urgency),
            )
            .await
            .expect("in-memory task")
    }

    fn score_of(task: &Task) -> f64 {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        task.compute_priority_score(now_secs)
    }

    #[tokio::test]
    async fn test_factors_between_lands_strictly_between() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let above = task_with_factors(&mut store, 2.0, 1.5).await;
        let below = task_with_factors(&mut store, 1.0, 2.0).await;
        let (imp, urg) = crate::factors_between(Some(&above), Some(&below));
        let inserted = task_with_factors(&mut store, imp, urg).await;
        let new_score = score_of(&inserted);
        assert!(new_score < 3.0, "new score {new_score} below above (3.0)");
        assert!(new_score > 2.0, "new score {new_score} above below (2.0)");
        // Per-factor geometric split of the neighbours' balance.
        assert!((imp - 2.0f64.sqrt()).abs() < 1e-9);
        assert!((urg - 3.0f64.sqrt()).abs() < 1e-9);

        // End to end: the inserted row sorts between its neighbours.
        let listed = store.list_tasks_by_priority().await?;
        let positions: std::collections::HashMap<u64, usize> =
            listed.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        assert!(positions[&above.id] < positions[&inserted.id]);
        assert!(positions[&inserted.id] < positions[&below.id]);
        Ok(())
    }

    #[tokio::test]
    async fn test_factors_between_edges() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let only = task_with_factors(&mut store, 2.0, 2.0).await;
        let (imp_top, urg_top) = crate::factors_between(None, Some(&only));
        let top = task_with_factors(&mut store, imp_top, urg_top).await;
        assert!(score_of(&top) > 4.0);
        let (imp_bottom, urg_bottom) = crate::factors_between(Some(&only), None);
        let bottom = task_with_factors(&mut store, imp_bottom, urg_bottom).await;
        let bottom_score = score_of(&bottom);
        assert!(bottom_score < 4.0);
        assert!(bottom_score > 0.0);
        let (imp_default, urg_default) = crate::factors_between(None, None);
        assert_eq!((imp_default, urg_default), (1.0, 1.0));
        Ok(())
    }

    #[tokio::test]
    async fn test_factors_between_equal_neighbours() -> anyhow::Result<()> {
        // No value sorts strictly between equal scores; the midpoint
        // keeps the task adjacent instead of flinging it elsewhere.
        let mut store = TodoStore::for_test().await?;
        let task = task_with_factors(&mut store, 1.5, 1.5).await;
        let (imp, urg) = crate::factors_between(Some(&task), Some(&task));
        let inserted = task_with_factors(&mut store, imp, urg).await;
        assert!((score_of(&inserted) - 2.25).abs() < 1e-9);
        Ok(())
    }

    #[tokio::test]
    async fn test_create_get_task() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let task = storage
            .create_task(
                Task::create()
                    .title("Task 1".to_string())
                    .description(Some("Do Task 1".to_string()))
                    .branch_name(Some("fix/task-1".to_string()))
                    .labels(toasty::Json(vec!["bug".to_string(), "backend".to_string()]))
                    // .blocked_by(toasty::Json(vec![BlockerRef {
                    //     id: Some("task_00".to_string()),
                    // }]))
                    .importance_factor(1.0)
                    .urgency_factor(1.0),
            )
            .await
            .unwrap();

        let retrieved = storage.get_task(task.id).await?;
        assert_eq!(retrieved.id, task.id);
        assert_eq!(retrieved.title, "Task 1");

        Ok(())
    }

    #[tokio::test]
    async fn test_update_task() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let task = storage.create_task(Task::create().title("Task 1")).await?;

        let query = Task::update_by_id(task.id)
            .title("Updated Task 1")
            .description(Some("Updated description".to_string()))
            .importance_factor(2.0);
        query.exec(&mut storage.db).await?;

        let reloaded = storage.get_task(task.id).await?;
        assert_eq!(reloaded.title, "Updated Task 1");
        assert_eq!(reloaded.importance_factor, 2.0);
        Ok(())
    }

    #[tokio::test]
    async fn test_list_tasks() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        for i in 1..=5 {
            storage
                .create_task(
                    Task::create()
                        .title(format!("Task {}", i))
                        .description(None)
                        .branch_name(None)
                        .labels(Some(toasty::Json(vec![])))
                        .importance_factor(1.0)
                        .urgency_factor(1.0),
                )
                .await?;
        }

        let tasks = storage.list_tasks().await?;
        assert_eq!(tasks.len(), 5);
        Ok(())
    }

    #[tokio::test]
    async fn test_list_tasks_by_priority() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();

        storage
            .create_task(
                Task::create()
                    .title("Low priority".to_string())
                    .importance_factor(1.0)
                    .deadline(Some(now_unix + 7 * 86400)),
            )
            .await?;

        storage
            .create_task(
                Task::create()
                    .title("High priority".to_string())
                    .importance_factor(2.0)
                    .deadline(Some(now_unix + 3600)),
            )
            .await?;

        storage
            .create_task(
                Task::create()
                    .title("Urgent no deadline".to_string())
                    .importance_factor(5.0),
            )
            .await?;

        let tasks = storage.list_tasks_by_priority().await?;
        assert_eq!(tasks.len(), 3);

        assert_eq!(tasks[0].title, "High priority");
        assert_eq!(tasks[1].title, "Urgent no deadline");
        assert_eq!(tasks[2].title, "Low priority");

        Ok(())
    }

    #[tokio::test]
    #[ignore = "triggers still experimental on turso, unsupported by toasty driver"]
    async fn test_db_schema__update_created_at_should_fail() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let task = storage.create_task(Task::create().title("Task 1")).await?;

        let original_created_at = task.created_at;

        let mut updated = task.clone();
        updated.created_at = Timestamp::now().checked_add(Duration::from_secs(1))?;
        updated.title = "Updated".to_string();

        let result = Task::update_by_id(task.id)
            .created_at(updated.created_at)
            .exec(&mut storage.db)
            .await;

        dbg!(&result);
        assert!(result.is_err());

        let reloaded = storage.get_task(task.id).await?;
        assert_eq!(
            reloaded.created_at, original_created_at,
            "created_at should not be mutable"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_create_task_with_parent() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let parent = storage
            .create_task(Task::create().title("Parent task".to_string()))
            .await?;

        let child = storage
            .create_task(
                Task::create()
                    .title("Child task".to_string())
                    .parent_id(Some(parent.id)),
            )
            .await?;

        assert_eq!(child.parent_id, Some(parent.id));

        let fetched = storage.get_task(child.id).await?;
        assert_eq!(fetched.parent_id, Some(parent.id));

        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn test_priority_score_over_time() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let start = tokio::time::Instant::now();
        let start_timestamp = start.elapsed().as_secs();

        let task = storage
            .create_task(
                Task::create()
                    .title("Deadline task".to_string())
                    .importance_factor(2.0)
                    .deadline(Some(start_timestamp + 3600)),
            )
            .await?;

        let score_early = task.compute_priority_score(start_timestamp);

        tokio::time::advance(Duration::from_secs(59 * 60)).await;
        let score_closer = task.compute_priority_score(start.elapsed().as_secs());
        assert!(score_closer > score_early);

        tokio::time::advance(Duration::from_secs(2 * 60)).await;
        let prio_score_after = task.compute_priority_score(start.elapsed().as_secs());
        // Past the deadline the score keeps growing instead of plateauing.
        assert!(
            prio_score_after > score_closer,
            "overdue score {prio_score_after} should keep growing past {score_closer}"
        );

        let no_deadline = storage
            .create_task(
                Task::create()
                    .title("No deadline".to_string())
                    .importance_factor(3.0),
            )
            .await?;
        assert_eq!(no_deadline.compute_priority_score(start_timestamp), 3.0);

        Ok(())
    }

    #[tokio::test]
    async fn test_urgency_factor_moves_ordering() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        storage
            .create_task(
                Task::create()
                    .title("Normal no deadline".to_string())
                    .importance_factor(1.0)
                    .urgency_factor(1.0),
            )
            .await?;
        // Same weight, higher urgency (Todoist P1 → 2.0): floats above.
        storage
            .create_task(
                Task::create()
                    .title("Urgent no deadline".to_string())
                    .importance_factor(1.0)
                    .urgency_factor(2.0),
            )
            .await?;

        let tasks = storage.list_tasks_by_priority().await?;
        let titles: Vec<&str> = tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["Urgent no deadline", "Normal no deadline"]);

        // Rust-side score agrees with the SQL order.
        let now = TodoStore::now_secs();
        assert!(
            tasks[0].priority_score(now) > tasks[1].priority_score(now),
            "urgent score should exceed normal score"
        );

        Ok(())
    }

    fn real_now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[tokio::test]
    async fn test_distant_upcoming_hidden_by_default() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let now = real_now_secs();

        let near = storage
            .create_task(Task::create().title("Starts tomorrow"))
            .await?;
        storage
            .update_blocked_until(near.id, Some(now + 86400))
            .await?;
        let far = storage
            .create_task(Task::create().title("Starts in a week"))
            .await?;
        storage
            .update_blocked_until(far.id, Some(now + 7 * 86400))
            .await?;

        let listed = storage.list_tasks_by_priority().await?;
        assert!(listed.iter().any(|t| t.id == near.id));
        assert!(!listed.iter().any(|t| t.id == far.id));

        let full = storage.list_tasks_by_priority_including_distant().await?;
        assert!(full.iter().any(|t| t.id == near.id));
        assert!(full.iter().any(|t| t.id == far.id));

        // Same cap inside tag views.
        let tag = storage.create_tag("Trip").await?;
        storage.assign_tag_to_task(near.id, &tag.name).await?;
        storage.assign_tag_to_task(far.id, &tag.name).await?;
        let tagged = storage.list_tasks_by_tag(tag.id).await?;
        assert!(!tagged.iter().any(|t| t.id == far.id));
        let tagged_full = storage.list_tasks_by_tag_including_distant(tag.id).await?;
        assert!(tagged_full.iter().any(|t| t.id == far.id));
        Ok(())
    }

    #[tokio::test]
    async fn test_overdue_score_keeps_increasing() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let now = real_now_secs();

        // Same weight, different overdue depths: the longer overdue sorts
        // first, in SQL order and in Rust-side scores alike.
        storage
            .create_task(
                Task::create()
                    .title("Overdue an hour")
                    .importance_factor(1.0)
                    .deadline(Some(now - 3600)),
            )
            .await?;
        storage
            .create_task(
                Task::create()
                    .title("Overdue two hours")
                    .importance_factor(1.0)
                    .deadline(Some(now - 7200)),
            )
            .await?;

        let tasks = storage.list_tasks_by_priority().await?;
        let titles: Vec<&str> = tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["Overdue two hours", "Overdue an hour"]);
        assert!(tasks[0].priority_score(now) > tasks[1].priority_score(now));
        Ok(())
    }

    #[tokio::test]
    async fn test_not_doable_tasks_sort_to_bottom() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let now = real_now_secs();

        let waiting = storage
            .create_task(
                Task::create()
                    .title("Not yet doable")
                    .importance_factor(2.0),
            )
            .await?;
        storage
            .update_blocked_until(waiting.id, Some(now + 3600))
            .await?;
        let ready = storage.create_task(Task::create().title("Doable")).await?;

        // Future blocked_until: still listed, but below doable tasks …
        let listed = storage.list_tasks_by_priority().await?;
        let titles: Vec<&str> = listed.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["Doable", "Not yet doable"]);

        // … including in the subtask tree …
        let child = storage
            .create_task(
                Task::create()
                    .title("Waiting child")
                    .parent_id(Some(ready.id)),
            )
            .await?;
        storage
            .update_blocked_until(child.id, Some(now + 3600))
            .await?;
        assert_eq!(storage.list_subtasks(ready.id).await?.len(), 1);

        // … and first again once the start time passes.
        storage
            .update_blocked_until(waiting.id, Some(now - 10))
            .await?;
        let listed = storage.list_tasks_by_priority().await?;
        assert_eq!(listed[0].title, "Not yet doable");
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn test_dorito_sorts_to_top_as_deadline_nears() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let now = real_now_secs();

        // Evening chores far out; dorito due in two hours at high
        // importance — still behind the one-hour errand.
        storage
            .create_task(
                Task::create()
                    .title("Take out trash")
                    .importance_factor(2.0)
                    .deadline(Some(now + 3600)),
            )
            .await?;
        let dorito = storage
            .create_task(
                Task::create()
                    .title("feed dorito")
                    .importance_factor(3.0)
                    .deadline(Some(now + 7200)),
            )
            .await?;

        let listed = storage.list_tasks_by_priority().await?;
        let titles: Vec<&str> = listed.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["Take out trash", "feed dorito"]);

        // 30 minutes to the 6:30pm deadline: dorito's priority_score jumps
        // past the errand and it sorts to the top.
        crate::Task::update_by_id(dorito.id)
            .deadline(Some(now + 1800))
            .exec(&mut storage.db)
            .await?;
        let listed = storage.list_tasks_by_priority().await?;
        let titles: Vec<&str> = listed.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["feed dorito", "Take out trash"]);
        Ok(())
    }

    #[tokio::test]
    async fn test_completed_tasks_sorted_to_bottom_and_hidden_after_24h() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        storage
            .create_task(Task::create().title("Open task"))
            .await?;
        let done_recent = storage
            .create_task(Task::create().title("Done recently"))
            .await?;
        let done_old = storage
            .create_task(Task::create().title("Done long ago"))
            .await?;

        storage.update_task_done(done_recent.id, true).await?;
        storage.update_task_done(done_old.id, true).await?;

        // Freshly completed tasks have a completed_at set.
        let recent = storage.get_task(done_recent.id).await?;
        assert!(recent.completed_at.is_some());

        // Backdate the older one slightly (still within the window) and
        // verify it sorts below the more recently completed task.
        let now = TodoStore::now_secs();
        toasty::sql::query("UPDATE tasks SET completed_at = ?1 WHERE id = ?2")
            .bind(now - 3600)
            .bind(done_old.id as i64)
            .exec(&mut storage.db)
            .await?;

        let tasks = storage.list_tasks_by_priority().await?;
        let titles: Vec<&str> = tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["Open task", "Done recently", "Done long ago"]);

        // Backdate beyond the visibility window: hidden from the list.
        toasty::sql::query("UPDATE tasks SET completed_at = ?1 WHERE id = ?2")
            .bind(now - 25 * 60 * 60)
            .bind(done_old.id as i64)
            .exec(&mut storage.db)
            .await?;

        let tasks = storage.list_tasks_by_priority().await?;
        let titles: Vec<&str> = tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["Open task", "Done recently"]);

        // Reopening clears completed_at.
        storage.update_task_done(done_old.id, false).await?;
        let reopened = storage.get_task(done_old.id).await?;
        assert!(reopened.completed_at.is_none());

        Ok(())
    }

    #[tokio::test]
    async fn test_done_roundtrip_via_list_queries() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;

        let task = storage
            .create_task(Task::create().title("Roundtrip task"))
            .await?;

        // Initially not done
        let tasks = storage.list_tasks_by_priority().await?;
        let t = tasks.iter().find(|t| t.id == task.id).unwrap();
        assert!(!t.done, "task should start as not done");

        // Mark done
        storage.update_task_done(task.id, true).await?;

        // Verify via list_tasks_by_priority
        let tasks = storage.list_tasks_by_priority().await?;
        let t = tasks.iter().find(|t| t.id == task.id).unwrap();
        assert!(
            t.done,
            "task should be done after update (list_tasks_by_priority)"
        );

        // Verify via list_tasks_by_tag
        let tag = storage.create_tag("roundtrip-tag").await?;
        storage.assign_tag_to_task(task.id, &tag.name).await?;
        let tasks = storage.list_tasks_by_tag(tag.id).await?;
        let t = tasks.iter().find(|t| t.id == task.id).unwrap();
        assert!(
            t.done,
            "task should be done after update (list_tasks_by_tag)"
        );

        Ok(())
    }
}
