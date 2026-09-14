//! GitHub issue sync: identity, per-field merge state, and the run-side rows
//! the PR step needs (worktrees, pull requests).
//!
//! GitHub timestamps whole issues rather than individual fields, so the merge
//! cannot compare per-field remote timestamps the way the Todoist sync does.
//! Instead each link stores the last remote values we synced plus the time of
//! the last *local* write per field: a remote value that differs from the
//! snapshot means the issue changed, dated by `updated_at` (spec §5.4).
//!
//! One integration spans many repos, so an issue's external id is
//! `owner/repo#number` — the bare number is only unique within a repo.

use crate::{QueryResult, TodoStore};
use snafu::ResultExt;
use std::collections::BTreeMap;

/// The fields the sync maps onto a local task, and the keys used in
/// [`IssueFieldState::local_changed_at`].
pub const ISSUE_FIELDS: [&str; 4] = ["title", "body", "state", "labels"];

/// `open` / `merged` / `closed` for `run_pull_requests.state`.
pub const PULL_REQUEST_OPEN: &str = "open";
pub const PULL_REQUEST_MERGED: &str = "merged";
pub const PULL_REQUEST_CLOSED: &str = "closed";

/// A parsed `owner/repo#number` identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

/// The external id used in `external_task_links.external_id`.
pub fn issue_external_id(owner: &str, repo: &str, number: u64) -> String {
    format!("{owner}/{repo}#{number}")
}

/// Inverse of [`issue_external_id`]. `None` for anything that is not a
/// repo-qualified issue id, so a malformed link can never be mistaken for a
/// different issue.
pub fn parse_issue_external_id(external_id: &str) -> Option<IssueRef> {
    let (slug, number) = external_id.split_once('#')?;
    let (owner, repo) = slug.split_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(IssueRef {
        owner: owner.to_string(),
        repo: repo.to_string(),
        number: number.parse().ok()?,
    })
}

/// The remote values last written to a local task, used to tell a remote edit
/// apart from a value the app itself pushed.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemoteIssueFields {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// `open` | `closed`.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub labels: Vec<String>,
}

/// Merge state for one issue link.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IssueFieldState {
    #[serde(default)]
    pub remote: RemoteIssueFields,
    /// Epoch seconds of the last local write to each mapped field.
    #[serde(default)]
    pub local_changed_at: BTreeMap<String, i64>,
    /// Mirrored for display only; never pushed.
    #[serde(default)]
    pub assignees: Vec<String>,
    #[serde(default)]
    pub milestone: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// Set when the local task was deleted: the issue was closed and later
    /// pulls must not resurrect the row.
    #[serde(default)]
    pub tombstoned: bool,
    /// Why a link was tombstoned from the remote side (deleted, transferred,
    /// access revoked), so the UI can explain it instead of hiding the task.
    #[serde(default)]
    pub tombstone_reason: Option<String>,
}

impl IssueFieldState {
    /// Record a local write so a later pull keeps it (§5.4).
    pub fn stamp_local(&mut self, field: &str, at: jiff::Timestamp) {
        self.local_changed_at
            .insert(field.to_string(), at.as_second());
    }

    /// Adopt remote values as the synced snapshot after a successful pull or
    /// push, so the app's own writes never look like remote edits.
    pub fn adopt_remote(&mut self, remote: &RemoteIssueFields) {
        self.remote = remote.clone();
    }
}

/// One issue as the API returns it, narrowed to the fields we map.
#[derive(Debug, Clone, Default)]
pub struct RemoteIssue {
    pub number: u64,
    pub title: String,
    pub body: String,
    /// `open` | `closed`.
    pub state: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub milestone: Option<String>,
    pub author: Option<String>,
    pub url: Option<String>,
    pub updated_at: Option<jiff::Timestamp>,
}

impl RemoteIssue {
    pub fn field_values(&self) -> RemoteIssueFields {
        RemoteIssueFields {
            title: self.title.clone(),
            body: self.body.clone(),
            state: self.state.clone(),
            labels: self.labels.clone(),
        }
    }
}

/// What the sync should do with one mapped field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldDecision {
    /// Neither side changed it since the last sync.
    Unchanged,
    /// The issue changed it (or the tie goes to GitHub).
    TakeRemote,
    /// The local edit is newer and stays.
    KeepLocal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldMerge {
    pub field: &'static str,
    pub decision: FieldDecision,
}

/// Resolve one field. `remote_changed` means the remote value differs from the
/// snapshot; a tie between the two timestamps goes to GitHub, and a remote
/// change with no dated local write also goes to GitHub.
pub fn resolve_field(
    remote_changed: bool,
    local_changed_at: Option<i64>,
    remote_updated_at: Option<jiff::Timestamp>,
) -> FieldDecision {
    match (remote_changed, local_changed_at) {
        (false, None) => FieldDecision::Unchanged,
        (false, Some(_)) => FieldDecision::KeepLocal,
        (true, None) => FieldDecision::TakeRemote,
        (true, Some(local_at)) => match remote_updated_at {
            Some(remote_at) if remote_at.as_second() >= local_at => FieldDecision::TakeRemote,
            Some(_) => FieldDecision::KeepLocal,
            // An undated remote change cannot be compared, so GitHub wins.
            None => FieldDecision::TakeRemote,
        },
    }
}

/// The per-field plan for one issue, in `ISSUE_FIELDS` order.
pub fn plan_issue_merge(state: &IssueFieldState, remote: &RemoteIssue) -> Vec<FieldMerge> {
    let changed: [bool; 4] = [
        state.remote.title != remote.title,
        state.remote.body != remote.body,
        state.remote.state != remote.state,
        state.remote.labels != remote.labels,
    ];
    ISSUE_FIELDS
        .iter()
        .zip(changed)
        .map(|(field, remote_changed)| FieldMerge {
            field,
            decision: resolve_field(
                remote_changed,
                state.local_changed_at.get(*field).copied(),
                remote.updated_at,
            ),
        })
        .collect()
}

/// One issue link with its merge state.
#[derive(Debug, Clone)]
pub struct IssueLink {
    pub integration_id: u64,
    pub external_id: String,
    pub task_id: u64,
    pub external_updated_at: Option<jiff::Timestamp>,
    pub state: IssueFieldState,
}

/// Incremental-pull cursor for one repo (§5.7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncCursor {
    pub etag: Option<String>,
    pub since: Option<String>,
    pub last_synced_at: Option<jiff::Timestamp>,
}

/// A worktree row: one run's checkout in one repo. Multi-repo projects get
/// several, which is why the base branch and remote live here rather than on
/// the run (§6.2).
#[derive(Debug, Clone)]
pub struct RunWorktree {
    pub id: u64,
    pub run_id: u64,
    pub repo_dir: String,
    pub worktree_path: String,
    pub branch: String,
    pub base_branch: String,
    pub remote: String,
    pub created_at: jiff::Timestamp,
    pub removed_at: Option<jiff::Timestamp>,
}

/// The parts of a worktree row the caller knows at creation time.
#[derive(Debug, Clone)]
pub struct NewRunWorktree {
    pub run_id: u64,
    pub repo_dir: String,
    pub worktree_path: String,
    pub branch: String,
    pub base_branch: String,
    pub remote: String,
}

/// A pull request opened by the PR step.
#[derive(Debug, Clone)]
pub struct RunPullRequest {
    pub id: u64,
    pub run_id: u64,
    pub repo_dir: String,
    pub integration_id: u64,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub draft: bool,
    pub state: String,
    pub merged_at: Option<jiff::Timestamp>,
    pub last_polled_at: Option<jiff::Timestamp>,
}

#[derive(Debug, Clone)]
pub struct NewRunPullRequest {
    pub run_id: u64,
    pub repo_dir: String,
    pub integration_id: u64,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub draft: bool,
    pub state: String,
}

fn record_string(record: &[toasty::stmt::Value], index: usize) -> Option<String> {
    record
        .get(index)
        .filter(|value| !matches!(value, toasty::stmt::Value::Null))
        .and_then(|value| value.as_str())
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn record_i64(record: &[toasty::stmt::Value], index: usize) -> Option<i64> {
    record.get(index).and_then(|value| value.to_i64())
}

fn record_timestamp(record: &[toasty::stmt::Value], index: usize) -> Option<jiff::Timestamp> {
    record_string(record, index).and_then(|text| text.parse().ok())
}

fn parse_field_state(value: Option<&toasty::stmt::Value>) -> IssueFieldState {
    match value.and_then(|value| value.as_str()) {
        Some(raw) if !raw.is_empty() => serde_json::from_str(raw).unwrap_or_default(),
        _ => IssueFieldState::default(),
    }
}

fn issue_link_columns() -> [toasty::stmt::Type; 5] {
    [
        toasty::stmt::Type::I64,
        toasty::stmt::Type::String,
        toasty::stmt::Type::I64,
        toasty::stmt::Type::String,
        toasty::stmt::Type::String,
    ]
}

fn parse_issue_link(row: toasty::stmt::Value) -> Option<IssueLink> {
    let toasty::stmt::Value::Record(record) = row else {
        return None;
    };
    Some(IssueLink {
        integration_id: record_i64(&record, 0)? as u64,
        external_id: record_string(&record, 1)?,
        task_id: record_i64(&record, 2)? as u64,
        external_updated_at: record_timestamp(&record, 3),
        state: parse_field_state(record.get(4)),
    })
}

impl TodoStore {
    /// Link a local task to an issue and persist its merge state in one write,
    /// so a half-written link (task linked, no snapshot) cannot exist.
    pub async fn link_issue(
        &mut self,
        integration_id: u64,
        external_id: &str,
        task_id: u64,
        state: &IssueFieldState,
        external_updated_at: Option<jiff::Timestamp>,
    ) -> QueryResult<()> {
        let state = serde_json::to_string(state).unwrap_or_else(|_| "{}".to_string());
        toasty::sql::statement(
            r#"INSERT INTO external_task_links
               (integration_id, external_id, task_id, external_updated_at, field_state)
               VALUES (?1, ?2, ?3, ?4, ?5)
               ON CONFLICT (integration_id, external_id)
               DO UPDATE SET task_id = excluded.task_id,
                             external_updated_at = excluded.external_updated_at,
                             field_state = excluded.field_state"#,
        )
        .bind(integration_id as i64)
        .bind(external_id)
        .bind(task_id as i64)
        .bind(external_updated_at.map(|t| t.to_string()).unwrap_or_default())
        .bind(state)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "link external issue",
        })?;
        Ok(())
    }

    pub async fn issue_link(
        &mut self,
        integration_id: u64,
        external_id: &str,
    ) -> QueryResult<Option<IssueLink>> {
        let rows = toasty::sql::query(
            r#"SELECT integration_id, external_id, task_id, external_updated_at, field_state
               FROM external_task_links WHERE integration_id = ?1 AND external_id = ?2"#,
        )
        .column_types(issue_link_columns())
        .bind(integration_id as i64)
        .bind(external_id)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "lookup issue link",
        })?;
        Ok(rows.into_iter().next().and_then(parse_issue_link))
    }

    /// Every issue link of one integration. Tombstoned links are included so
    /// the caller can skip them explicitly rather than silently.
    pub async fn issue_links_for_integration(
        &mut self,
        integration_id: u64,
    ) -> QueryResult<Vec<IssueLink>> {
        let rows = toasty::sql::query(
            r#"SELECT integration_id, external_id, task_id, external_updated_at, field_state
               FROM external_task_links WHERE integration_id = ?1"#,
        )
        .column_types(issue_link_columns())
        .bind(integration_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list issue links",
        })?;
        Ok(rows.into_iter().filter_map(parse_issue_link).collect())
    }

    /// Persist merge state without touching `external_updated_at`, for the
    /// per-field local stamps recorded between syncs.
    pub async fn save_issue_field_state(
        &mut self,
        integration_id: u64,
        external_id: &str,
        state: &IssueFieldState,
    ) -> QueryResult<()> {
        let state = serde_json::to_string(state).unwrap_or_else(|_| "{}".to_string());
        toasty::sql::statement(
            r#"UPDATE external_task_links SET field_state = ?1
               WHERE integration_id = ?2 AND external_id = ?3"#,
        )
        .bind(state)
        .bind(integration_id as i64)
        .bind(external_id)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "save issue field state",
        })?;
        Ok(())
    }

    pub async fn sync_cursor(
        &mut self,
        integration_id: u64,
        external_id: &str,
    ) -> QueryResult<Option<SyncCursor>> {
        let rows = toasty::sql::query(
            r#"SELECT etag, since, last_synced_at FROM integration_sync_state
               WHERE integration_id = ?1 AND external_id = ?2"#,
        )
        .column_types([
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
        .bind(integration_id as i64)
        .bind(external_id)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "load sync cursor",
        })?;
        Ok(rows.into_iter().next().and_then(|row| match row {
            toasty::stmt::Value::Record(record) => Some(SyncCursor {
                etag: record_string(&record, 0),
                since: record_string(&record, 1),
                last_synced_at: record_timestamp(&record, 2),
            }),
            _ => None,
        }))
    }

    /// Record the cursor only after a successful pass, so a failed sync
    /// retries the same window (§5.7).
    pub async fn record_sync_cursor(
        &mut self,
        integration_id: u64,
        external_id: &str,
        cursor: &SyncCursor,
    ) -> QueryResult<()> {
        toasty::sql::statement(
            r#"INSERT INTO integration_sync_state
               (integration_id, external_id, etag, since, last_synced_at)
               VALUES (?1, ?2, ?3, ?4, ?5)
               ON CONFLICT (integration_id, external_id)
               DO UPDATE SET etag = excluded.etag,
                             since = excluded.since,
                             last_synced_at = excluded.last_synced_at"#,
        )
        .bind(integration_id as i64)
        .bind(external_id)
        .bind(cursor.etag.clone().unwrap_or_default())
        .bind(cursor.since.clone().unwrap_or_default())
        .bind(
            cursor
                .last_synced_at
                .unwrap_or_else(jiff::Timestamp::now)
                .to_string(),
        )
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "record sync cursor",
        })?;
        Ok(())
    }

    pub async fn insert_run_worktree(
        &mut self,
        worktree: &NewRunWorktree,
    ) -> QueryResult<u64> {
        toasty::sql::statement(
            r#"INSERT INTO run_worktrees
               (run_id, repo_dir, worktree_path, branch, base_branch, remote, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
        )
        .bind(worktree.run_id as i64)
        .bind(&worktree.repo_dir)
        .bind(&worktree.worktree_path)
        .bind(&worktree.branch)
        .bind(&worktree.base_branch)
        .bind(&worktree.remote)
        .bind(jiff::Timestamp::now().to_string())
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "insert run worktree",
        })?;
        self.last_insert_id().await
    }

    /// A run's worktrees; `removed_at` is set once the checkout is gone but
    /// the row is kept so the run still shows where it worked.
    pub async fn run_worktrees(&mut self, run_id: u64) -> QueryResult<Vec<RunWorktree>> {
        let rows = toasty::sql::query(
            r#"SELECT id, run_id, repo_dir, worktree_path, branch, base_branch, remote,
                      created_at, removed_at
               FROM run_worktrees WHERE run_id = ?1 ORDER BY id"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
        .bind(run_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list run worktrees",
        })?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let toasty::stmt::Value::Record(record) = row else {
                continue;
            };
            let Some(id) = record_i64(&record, 0) else {
                continue;
            };
            let Some(run_id) = record_i64(&record, 1) else {
                continue;
            };
            out.push(RunWorktree {
                id: id as u64,
                run_id: run_id as u64,
                repo_dir: record_string(&record, 2).unwrap_or_default(),
                worktree_path: record_string(&record, 3).unwrap_or_default(),
                branch: record_string(&record, 4).unwrap_or_default(),
                base_branch: record_string(&record, 5).unwrap_or_default(),
                remote: record_string(&record, 6).unwrap_or_default(),
                created_at: record_timestamp(&record, 7).unwrap_or_else(jiff::Timestamp::now),
                removed_at: record_timestamp(&record, 8),
            });
        }
        Ok(out)
    }

    pub async fn mark_run_worktree_removed(&mut self, id: u64) -> QueryResult<()> {
        toasty::sql::statement(
            r#"UPDATE run_worktrees SET removed_at = ?1 WHERE id = ?2"#,
        )
        .bind(jiff::Timestamp::now().to_string())
        .bind(id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "mark run worktree removed",
        })?;
        Ok(())
    }

    /// Insert or adopt a pull request. Adopting matters when a branch already
    /// has a PR (a re-run, or one opened by hand): the row is refreshed
    /// instead of failing the step (§8).
    pub async fn upsert_run_pull_request(
        &mut self,
        pull_request: &NewRunPullRequest,
    ) -> QueryResult<u64> {
        toasty::sql::statement(
            r#"INSERT INTO run_pull_requests
               (run_id, repo_dir, integration_id, owner, repo, number, url, head_branch,
                base_branch, draft, state, merged_at, last_polled_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, NULL)
               ON CONFLICT (integration_id, owner, repo, number)
               DO UPDATE SET run_id = excluded.run_id,
                             repo_dir = excluded.repo_dir,
                             url = excluded.url,
                             head_branch = excluded.head_branch,
                             base_branch = excluded.base_branch,
                             draft = excluded.draft,
                             state = excluded.state"#,
        )
        .bind(pull_request.run_id as i64)
        .bind(&pull_request.repo_dir)
        .bind(pull_request.integration_id as i64)
        .bind(&pull_request.owner)
        .bind(&pull_request.repo)
        .bind(pull_request.number as i64)
        .bind(&pull_request.url)
        .bind(&pull_request.head_branch)
        .bind(&pull_request.base_branch)
        .bind(i64::from(pull_request.draft))
        .bind(&pull_request.state)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "upsert run pull request",
        })?;
        // The conflict path has no new rowid, so look the row up by identity.
        let rows = toasty::sql::query(
            r#"SELECT id FROM run_pull_requests
               WHERE integration_id = ?1 AND owner = ?2 AND repo = ?3 AND number = ?4"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(pull_request.integration_id as i64)
        .bind(&pull_request.owner)
        .bind(&pull_request.repo)
        .bind(pull_request.number as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "lookup run pull request",
        })?;
        Ok(rows
            .into_iter()
            .next()
            .and_then(|row| match row {
                toasty::stmt::Value::Record(record) => record_i64(&record, 0),
                _ => None,
            })
            .unwrap_or(0) as u64)
    }

    pub async fn run_pull_requests(&mut self, run_id: u64) -> QueryResult<Vec<RunPullRequest>> {
        self.query_run_pull_requests("WHERE run_id = ?1", Some(run_id as i64))
            .await
    }

    /// The poller's work list (§5.7): every PR that still needs watching.
    pub async fn open_run_pull_requests(&mut self) -> QueryResult<Vec<RunPullRequest>> {
        self.query_run_pull_requests("WHERE state = ?1", None).await
    }

    /// Shared reader for the two PR queries. `filter` is a literal chosen by
    /// the caller (never user input) and decides which value `bound` carries:
    /// a run id, or the open state for the poller's work list.
    async fn query_run_pull_requests(
        &mut self,
        clause: &str,
        run_id: Option<i64>,
    ) -> QueryResult<Vec<RunPullRequest>> {
        let sql = format!(
            r#"SELECT id, run_id, repo_dir, integration_id, owner, repo, number, url,
                      head_branch, base_branch, draft, state, merged_at, last_polled_at
               FROM run_pull_requests {clause} ORDER BY id"#
        );
        let mut statement = toasty::sql::query(sql).column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ]);
        match run_id {
            Some(run_id) => statement = statement.bind(run_id),
            None => statement = statement.bind(PULL_REQUEST_OPEN),
        }
        let rows = statement
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "list run pull requests",
            })?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let toasty::stmt::Value::Record(record) = row else {
                continue;
            };
            let Some(id) = record_i64(&record, 0) else {
                continue;
            };
            let Some(run_id) = record_i64(&record, 1) else {
                continue;
            };
            let Some(number) = record_i64(&record, 6) else {
                continue;
            };
            out.push(RunPullRequest {
                id: id as u64,
                run_id: run_id as u64,
                repo_dir: record_string(&record, 2).unwrap_or_default(),
                integration_id: record_i64(&record, 3).unwrap_or(0) as u64,
                owner: record_string(&record, 4).unwrap_or_default(),
                repo: record_string(&record, 5).unwrap_or_default(),
                number: number as u64,
                url: record_string(&record, 7).unwrap_or_default(),
                head_branch: record_string(&record, 8).unwrap_or_default(),
                base_branch: record_string(&record, 9).unwrap_or_default(),
                draft: record_i64(&record, 10).unwrap_or(0) != 0,
                state: record_string(&record, 11).unwrap_or_default(),
                merged_at: record_timestamp(&record, 12),
                last_polled_at: record_timestamp(&record, 13),
            });
        }
        Ok(out)
    }

    /// Record what a poll learned about one PR.
    pub async fn update_run_pull_request(
        &mut self,
        id: u64,
        state: &str,
        draft: bool,
        merged_at: Option<jiff::Timestamp>,
    ) -> QueryResult<()> {
        toasty::sql::statement(
            r#"UPDATE run_pull_requests
               SET state = ?1, draft = ?2, merged_at = ?3, last_polled_at = ?4
               WHERE id = ?5"#,
        )
        .bind(state)
        .bind(i64::from(draft))
        .bind(merged_at.map(|t| t.to_string()).unwrap_or_default())
        .bind(jiff::Timestamp::now().to_string())
        .bind(id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "update run pull request",
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;

    fn remote(number: u64, title: &str, state: &str, at: i64) -> RemoteIssue {
        RemoteIssue {
            number,
            title: title.to_string(),
            body: "body".to_string(),
            state: state.to_string(),
            labels: vec!["bug".to_string()],
            assignees: vec!["me".to_string()],
            milestone: Some("v1".to_string()),
            author: Some("someone".to_string()),
            url: Some(format!("https://github.com/o/r/issues/{number}")),
            updated_at: jiff::Timestamp::from_second(at).ok(),
        }
    }

    #[test]
    fn issue_ids_round_trip_and_reject_malformed_ones() {
        let id = issue_external_id("lofi-tools", "todo-lofi", 42);
        assert_eq!(id, "lofi-tools/todo-lofi#42");
        assert_eq!(
            parse_issue_external_id(&id),
            Some(IssueRef {
                owner: "lofi-tools".to_string(),
                repo: "todo-lofi".to_string(),
                number: 42,
            })
        );
        // A bare number is not repo-qualified, so it can never collide across
        // repos of the same account.
        assert!(parse_issue_external_id("42").is_none());
        assert!(parse_issue_external_id("todo-lofi#42").is_none());
        assert!(parse_issue_external_id("/repo#42").is_none());
        assert!(parse_issue_external_id("owner/repo#not-a-number").is_none());
    }

    #[test]
    fn unresolved_fields_report_unchanged() {
        let state = IssueFieldState::default();
        let plan = plan_issue_merge(&state, &remote(1, "Title", "open", 100));
        assert!(
            plan.iter().all(|f| f.decision != FieldDecision::Unchanged),
            "a snapshot taken with default values must not swallow real values"
        );
    }

    #[test]
    fn remote_change_wins_when_the_local_side_is_untouched() {
        let mut state = IssueFieldState::default();
        let mut issue = remote(1, "New title", "open", 100);
        issue.body = "same".to_string();
        state.remote = issue.field_values();
        // Only the title differs from the snapshot now.
        issue.title = "Newer title".to_string();
        issue.updated_at = jiff::Timestamp::from_second(200).ok();

        let plan = plan_issue_merge(&state, &issue);
        let title = plan.iter().find(|f| f.field == "title").unwrap();
        assert_eq!(title.decision, FieldDecision::TakeRemote);
        for field in ["body", "state", "labels"] {
            let merge = plan.iter().find(|f| f.field == field).unwrap();
            assert_eq!(merge.decision, FieldDecision::Unchanged, "{field}");
        }
    }

    #[test]
    fn local_edit_keeps_the_newer_value_and_loses_ties_to_github() {
        let mut state = IssueFieldState::default();
        let issue = remote(1, "Remote title", "open", 100);
        state.remote = issue.field_values();
        // The issue did not change, but the user rewrote the title locally.
        state.stamp_local("title", jiff::Timestamp::from_second(150).unwrap());
        let plan = plan_issue_merge(&state, &issue);
        assert_eq!(
            plan.iter().find(|f| f.field == "title").unwrap().decision,
            FieldDecision::KeepLocal
        );

        // Now the issue changed too: the newer stamp wins.
        let mut newer = issue.clone();
        newer.title = "Changed remotely".to_string();
        newer.updated_at = jiff::Timestamp::from_second(160).ok();
        assert_eq!(
            plan_issue_merge(&state, &newer)
                .iter()
                .find(|f| f.field == "title")
                .unwrap()
                .decision,
            FieldDecision::TakeRemote
        );

        // A tie goes to GitHub.
        let mut tied = issue.clone();
        tied.title = "Changed remotely".to_string();
        tied.updated_at = jiff::Timestamp::from_second(150).ok();
        assert_eq!(
            plan_issue_merge(&state, &tied)
                .iter()
                .find(|f| f.field == "title")
                .unwrap()
                .decision,
            FieldDecision::TakeRemote
        );

        // An undated remote change cannot be compared, so GitHub still wins.
        let mut undated = issue.clone();
        undated.title = "Changed remotely".to_string();
        undated.updated_at = None;
        assert_eq!(
            plan_issue_merge(&state, &undated)
                .iter()
                .find(|f| f.field == "title")
                .unwrap()
                .decision,
            FieldDecision::TakeRemote
        );
    }

    #[test]
    fn adopting_a_remote_snapshot_stops_the_apps_own_push_looking_like_an_edit() {
        let mut state = IssueFieldState::default();
        let pushed = remote(1, "Title I just wrote", "open", 100);
        state.adopt_remote(&pushed.field_values());
        let plan = plan_issue_merge(&state, &pushed);
        assert!(plan.iter().all(|f| f.decision == FieldDecision::Unchanged));
    }

    #[test]
    fn field_state_round_trips_through_json() {
        let mut state = IssueFieldState {
            assignees: vec!["me".to_string()],
            milestone: Some("v1".to_string()),
            ..Default::default()
        };
        state.stamp_local("body", jiff::Timestamp::from_second(7).unwrap());
        let raw = serde_json::to_string(&state).unwrap();
        let parsed: IssueFieldState = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed, state);
        // Unknown/absent keys from an older row still parse.
        let legacy: IssueFieldState =
            serde_json::from_str(r#"{"remote":{"title":"t"}}"#).unwrap();
        assert_eq!(legacy.remote.title, "t");
        assert!(legacy.local_changed_at.is_empty());
    }

    #[tokio::test]
    async fn issue_links_carry_their_merge_state() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let integration = storage.create_integration("github", Some("me".to_string())).await?;
        let task = storage.create_task(Task::create().title("Issue")).await?;
        let external_id = issue_external_id("lofi-tools", "todo-lofi", 7);

        let mut state = IssueFieldState::default();
        state.adopt_remote(&remote(7, "Fix login", "open", 100).field_values());
        state.stamp_local("body", jiff::Timestamp::from_second(120).unwrap());
        storage
            .link_issue(
                integration.id,
                &external_id,
                task.id,
                &state,
                jiff::Timestamp::from_second(100).ok(),
            )
            .await?;

        let link = storage.issue_link(integration.id, &external_id).await?.unwrap();
        assert_eq!(link.task_id, task.id);
        assert_eq!(link.state.remote.title, "Fix login");
        assert_eq!(link.state.local_changed_at.get("body"), Some(&120));
        assert_eq!(link.external_updated_at.and_then(|t| Some(t.as_second())), Some(100));

        // Re-linking updates in place instead of duplicating.
        let mut updated = state.clone();
        updated.tombstoned = true;
        updated.tombstone_reason = Some("transferred".to_string());
        storage
            .link_issue(integration.id, &external_id, task.id, &updated, None)
            .await?;
        let links = storage.issue_links_for_integration(integration.id).await?;
        assert_eq!(links.len(), 1);
        assert!(links[0].state.tombstoned);
        assert_eq!(
            links[0].state.tombstone_reason.as_deref(),
            Some("transferred")
        );
        Ok(())
    }

    #[tokio::test]
    async fn sync_cursors_are_per_repo_and_only_move_on_success() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let integration = storage.create_integration("github", None).await?;
        assert!(
            storage
                .sync_cursor(integration.id, "lofi-tools/todo-lofi")
                .await?
                .is_none()
        );

        storage
            .record_sync_cursor(
                integration.id,
                "lofi-tools/todo-lofi",
                &SyncCursor {
                    etag: Some("W/\"abc\"".to_string()),
                    since: Some("2026-09-14T00:00:00Z".to_string()),
                    last_synced_at: jiff::Timestamp::from_second(1000).ok(),
                },
            )
            .await?;
        let cursor = storage
            .sync_cursor(integration.id, "lofi-tools/todo-lofi")
            .await?
            .unwrap();
        assert_eq!(cursor.etag.as_deref(), Some("W/\"abc\""));
        assert_eq!(cursor.last_synced_at.map(|t| t.as_second()), Some(1000));

        // A second repo keeps its own cursor.
        assert!(
            storage
                .sync_cursor(integration.id, "lofi-tools/other")
                .await?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_run_can_hold_one_worktree_per_repo() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let first = storage
            .insert_run_worktree(&NewRunWorktree {
                run_id: 5,
                repo_dir: "/repos/api".to_string(),
                worktree_path: "/repos/api/worktrees/123-add-login".to_string(),
                branch: "feature/123-add-login".to_string(),
                base_branch: "main".to_string(),
                remote: "github".to_string(),
            })
            .await?;
        storage
            .insert_run_worktree(&NewRunWorktree {
                run_id: 5,
                repo_dir: "/repos/web".to_string(),
                worktree_path: "/repos/web/worktrees/123-add-login".to_string(),
                branch: "feature/123-add-login".to_string(),
                base_branch: "develop".to_string(),
                remote: "origin".to_string(),
            })
            .await?;

        let worktrees = storage.run_worktrees(5).await?;
        assert_eq!(worktrees.len(), 2);
        // Each repo keeps its own base branch and remote.
        assert_ne!(worktrees[0].base_branch, worktrees[1].base_branch);
        assert_eq!(worktrees[1].remote, "origin");
        assert!(worktrees.iter().all(|w| w.removed_at.is_none()));

        storage.mark_run_worktree_removed(first).await?;
        let worktrees = storage.run_worktrees(5).await?;
        assert!(worktrees[0].removed_at.is_some());
        assert!(worktrees[1].removed_at.is_none());
        // Other runs are untouched.
        assert!(storage.run_worktrees(6).await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn pull_requests_are_adopted_and_polled() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let integration = storage.create_integration("github", None).await?;
        let pr = NewRunPullRequest {
            run_id: 9,
            repo_dir: "/repos/api".to_string(),
            integration_id: integration.id,
            owner: "lofi-tools".to_string(),
            repo: "todo-lofi".to_string(),
            number: 12,
            url: "https://github.com/lofi-tools/todo-lofi/pull/12".to_string(),
            head_branch: "feature/123-add-login".to_string(),
            base_branch: "main".to_string(),
            draft: true,
            state: PULL_REQUEST_OPEN.to_string(),
        };
        let id = storage.upsert_run_pull_request(&pr).await?;
        assert!(id > 0);

        // Re-opening the PR step adopts the existing PR instead of failing.
        let mut opened = pr.clone();
        opened.draft = false;
        opened.url = "https://github.com/lofi-tools/todo-lofi/pull/12".to_string();
        let again = storage.upsert_run_pull_request(&opened).await?;
        assert_eq!(again, id);
        let all = storage.run_pull_requests(9).await?;
        assert_eq!(all.len(), 1);
        assert!(!all[0].draft);

        // Only open PRs are polled.
        assert_eq!(storage.open_run_pull_requests().await?.len(), 1);
        storage
            .update_run_pull_request(id, PULL_REQUEST_MERGED, false, jiff::Timestamp::from_second(2000).ok())
            .await?;
        assert!(storage.open_run_pull_requests().await?.is_empty());
        let merged = &storage.run_pull_requests(9).await?[0];
        assert_eq!(merged.state, PULL_REQUEST_MERGED);
        assert_eq!(merged.merged_at.map(|t| t.as_second()), Some(2000));
        assert!(merged.last_polled_at.is_some());
        Ok(())
    }
}
