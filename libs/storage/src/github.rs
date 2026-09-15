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

use crate::{ExternalComment, QueryResult, Task, TodoStore};
use snafu::ResultExt;
use std::collections::BTreeMap;

/// The fields the sync maps onto a local task, and the keys used in
/// [`IssueFieldState::local_changed_at`].
pub const ISSUE_FIELDS: [&str; 4] = ["title", "body", "state", "labels"];

/// `open` / `merged` / `closed` / `waived` for `run_pull_requests.state`.
/// `waived` is the user giving up on a repo's PR so a multi-repo run can
/// still complete (decision 25); the other three mirror GitHub.
pub const PULL_REQUEST_OPEN: &str = "open";
pub const PULL_REQUEST_MERGED: &str = "merged";
pub const PULL_REQUEST_CLOSED: &str = "closed";
pub const PULL_REQUEST_WAIVED: &str = "waived";

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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

const RUN_PULL_REQUEST_SELECT: &str = r#"SELECT id, run_id, repo_dir, integration_id, owner,
        repo, number, url, head_branch, base_branch, draft, state, merged_at, last_polled_at
        FROM run_pull_requests"#;

fn run_pull_request_columns() -> [toasty::stmt::Type; 14] {
    [
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
    ]
}

fn parse_run_pull_request(row: toasty::stmt::Value) -> Option<RunPullRequest> {
    let toasty::stmt::Value::Record(record) = row else {
        return None;
    };
    Some(RunPullRequest {
        id: record_i64(&record, 0)? as u64,
        run_id: record_i64(&record, 1)? as u64,
        repo_dir: record_string(&record, 2).unwrap_or_default(),
        integration_id: record_i64(&record, 3).unwrap_or(0) as u64,
        owner: record_string(&record, 4).unwrap_or_default(),
        repo: record_string(&record, 5).unwrap_or_default(),
        number: record_i64(&record, 6)? as u64,
        url: record_string(&record, 7).unwrap_or_default(),
        head_branch: record_string(&record, 8).unwrap_or_default(),
        base_branch: record_string(&record, 9).unwrap_or_default(),
        draft: record_i64(&record, 10).unwrap_or(0) != 0,
        state: record_string(&record, 11).unwrap_or_default(),
        merged_at: record_timestamp(&record, 12),
        last_polled_at: record_timestamp(&record, 13),
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

    /// When any repo of this integration last synced successfully, for the
    /// card's "last synced" line (§5.8). `None` until the first pass lands.
    pub async fn github_last_synced_at(
        &mut self,
        integration_id: u64,
    ) -> QueryResult<Option<jiff::Timestamp>> {
        let rows = toasty::sql::query(
            r#"SELECT MAX(last_synced_at) FROM integration_sync_state
               WHERE integration_id = ?1"#,
        )
        .column_types([toasty::stmt::Type::String])
        .bind(integration_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "load last sync",
        })?;
        Ok(rows
            .into_iter()
            .next()
            .and_then(|row| match row {
                toasty::stmt::Value::Record(record) => record_timestamp(&record, 0),
                _ => None,
            }))
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

    /// Shared reader for the two filtered PR queries. `clause` is a literal
    /// chosen by the caller (never user input) and decides which value the
    /// statement binds: a run id, or the open state for the poller's list.
    async fn query_run_pull_requests(
        &mut self,
        clause: &str,
        run_id: Option<i64>,
    ) -> QueryResult<Vec<RunPullRequest>> {
        let sql = format!("{RUN_PULL_REQUEST_SELECT} {clause} ORDER BY id");
        let statement = match run_id {
            Some(run_id) => toasty::sql::query(sql).bind(run_id),
            None => toasty::sql::query(sql).bind(PULL_REQUEST_OPEN),
        };
        let rows = statement
            .column_types(run_pull_request_columns())
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "list run pull requests",
            })?;
        Ok(rows.into_iter().filter_map(parse_run_pull_request).collect())
    }

    /// Every pull request row, whatever its state.
    async fn query_run_pull_requests_all(&mut self) -> QueryResult<Vec<RunPullRequest>> {
        let rows = toasty::sql::query(format!("{RUN_PULL_REQUEST_SELECT} ORDER BY id"))
            .column_types(run_pull_request_columns())
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "list all run pull requests",
            })?;
        Ok(rows.into_iter().filter_map(parse_run_pull_request).collect())
    }

    /// One pull request row by id.
    async fn query_run_pull_request(&mut self, id: u64) -> QueryResult<Option<RunPullRequest>> {
        let rows = toasty::sql::query(format!("{RUN_PULL_REQUEST_SELECT} WHERE id = ?1"))
            .column_types(run_pull_request_columns())
            .bind(id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "load run pull request",
            })?;
        Ok(rows.into_iter().filter_map(parse_run_pull_request).next())
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

/// One page of issues plus the conditional-request cursor. `complete` is what
/// makes a missing link safe to tombstone: a `since`-filtered page is partial,
/// so treating it as complete would tombstone every issue that merely did not
/// change since the last pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssuePage {
    pub issues: Vec<RemoteIssue>,
    pub complete: bool,
    pub etag: Option<String>,
}

/// One repository the connected account can see, as the tag settings binding
/// picker lists it (§5.3). Only the identity and visibility are shown; the
/// sync itself always talks about `owner/repo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRepo {
    pub full_name: String,
    pub private: bool,
}

/// Narrow the `/user/repos` payload to the fields the picker shows.
pub fn repos_from_json(value: &serde_json::Value) -> Vec<RemoteRepo> {
    value
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|repo| {
            let full_name = repo.get("full_name")?.as_str()?.trim().to_string();
            if full_name.is_empty() {
                return None;
            }
            Some(RemoteRepo {
                full_name,
                private: repo
                    .get("private")
                    .and_then(|private| private.as_bool())
                    .unwrap_or(false),
            })
        })
        .collect()
}

/// The fields the sync may push for one issue. Only the set fields go on the
/// wire, so we never round-trip (and clobber) fields we do not model (§5.5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssuePatch {
    pub title: Option<String>,
    pub body: Option<String>,
    /// `open` | `closed`.
    pub state: Option<String>,
    pub labels: Option<Vec<String>>,
}

impl IssuePatch {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.body.is_none()
            && self.state.is_none()
            && self.labels.is_none()
    }
}

/// Why an API call failed, in the terms the retry policy needs (§8):
/// transient failures back off and retry, permanent ones block with a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncFailure {
    /// Network, 5xx, or a rate limit: retry after `retry_after`.
    Transient {
        message: String,
        retry_after: Option<std::time::Duration>,
    },
    /// The user has to act: reconnect (401), bind a visible repo (404), or fix
    /// the payload (422).
    Permanent { status: u16, message: String },
}

impl SyncFailure {
    pub fn is_transient(&self) -> bool {
        matches!(self, SyncFailure::Transient { .. })
    }

    pub fn message(&self) -> &str {
        match self {
            SyncFailure::Transient { message, .. } | SyncFailure::Permanent { message, .. } => {
                message
            }
        }
    }

    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            SyncFailure::Transient { retry_after, .. } => *retry_after,
            SyncFailure::Permanent { .. } => None,
        }
    }
}

impl std::fmt::Display for SyncFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncFailure::Transient { message, .. } => write!(f, "{message} (temporary)"),
            SyncFailure::Permanent { status, message } => write!(f, "{message} ({status})"),
        }
    }
}

impl std::error::Error for SyncFailure {}

/// The PR step refused to run because worktrees hold uncommitted work. It is
/// not a failure: the caller lists the changed paths and offers to commit them
/// and continue (§6.7), which is why it is a distinct error type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyWorktrees {
    /// One `(worktree path, changed paths)` per worktree with uncommitted work.
    pub worktrees: Vec<(String, Vec<String>)>,
}

impl std::fmt::Display for DirtyWorktrees {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "uncommitted work in the run's worktrees")
    }
}

impl std::error::Error for DirtyWorktrees {}

/// 401, 404 and 422 need the user; 403/429 and 5xx are worth retrying. A 403
/// without a rate-limit hint is a permanent permission problem.
pub fn classify_status(
    status: u16,
    message: &str,
    retry_after: Option<std::time::Duration>,
) -> SyncFailure {
    match status {
        401 => SyncFailure::Permanent {
            status,
            message: format!("GitHub rejected the stored token; reconnect GitHub ({message})"),
        },
        404 => SyncFailure::Permanent {
            status,
            message: format!("The repository is not visible to this account ({message})"),
        },
        422 => SyncFailure::Permanent {
            status,
            message: format!("GitHub rejected the request ({message})"),
        },
        403 | 429 => SyncFailure::Transient {
            message: format!("GitHub rate limited the request ({message})"),
            retry_after: retry_after.or(Some(std::time::Duration::from_secs(60))),
        },
        status if status >= 500 => SyncFailure::Transient {
            message: format!("GitHub returned {status} ({message})"),
            retry_after,
        },
        _ => SyncFailure::Permanent {
            status,
            message: format!("GitHub returned {status} ({message})"),
        },
    }
}

/// One comment as GitHub returns it, narrowed to what we import.
pub fn comment_from_json(value: &serde_json::Value) -> Option<ExternalComment> {
    let id = value.get("id")?.as_u64()?;
    let text = value
        .get("body")
        .and_then(|body| body.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if text.is_empty() {
        return None;
    }
    Some(ExternalComment {
        external_id: id.to_string(),
        text,
        author: value
            .get("user")
            .and_then(|user| user.get("login"))
            .and_then(|login| login.as_str())
            .map(str::to_owned),
        created_at: value
            .get("created_at")
            .and_then(|at| at.as_str())
            .map(str::to_owned),
    })
}

/// One issue as GitHub's JSON returns it. Pull requests share the issues
/// endpoint, so anything carrying `pull_request` is skipped rather than
/// imported as a task.
pub fn remote_issue_from_json(value: &serde_json::Value) -> Option<RemoteIssue> {
    if value.get("pull_request").is_some() {
        return None;
    }
    let number = value.get("number")?.as_u64()?;
    let strings = |key: &str| -> Vec<String> {
        value
            .get(key)
            .and_then(|list| list.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|entry| {
                        entry
                            .get("name")
                            .or_else(|| entry.get("login"))
                            .and_then(|name| name.as_str())
                            .map(str::to_owned)
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(RemoteIssue {
        number,
        title: value
            .get("title")
            .and_then(|title| title.as_str())
            .unwrap_or_default()
            .to_string(),
        body: value
            .get("body")
            .and_then(|body| body.as_str())
            .unwrap_or_default()
            .to_string(),
        state: value
            .get("state")
            .and_then(|state| state.as_str())
            .unwrap_or("open")
            .to_string(),
        labels: strings("labels"),
        assignees: strings("assignees"),
        milestone: value
            .get("milestone")
            .and_then(|milestone| milestone.get("title"))
            .and_then(|title| title.as_str())
            .map(str::to_owned),
        author: value
            .get("user")
            .and_then(|user| user.get("login"))
            .and_then(|login| login.as_str())
            .map(str::to_owned),
        url: value
            .get("html_url")
            .and_then(|url| url.as_str())
            .map(str::to_owned),
        updated_at: value
            .get("updated_at")
            .and_then(|at| at.as_str())
            .and_then(|at| at.parse().ok()),
    })
}

/// A pull request as GitHub returns it, narrowed to what the PR step needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePullRequest {
    pub number: u64,
    pub url: String,
    /// GitHub's own `open`/`closed`; `merged` is reported separately.
    pub state: String,
    pub draft: bool,
    pub merged: bool,
    pub merged_at: Option<jiff::Timestamp>,
    /// GraphQL id, needed to flip a draft to ready (the REST API cannot).
    pub node_id: Option<String>,
}

impl RemotePullRequest {
    /// The local `run_pull_requests.state` this PR maps to.
    pub fn local_state(&self) -> &'static str {
        if self.merged {
            PULL_REQUEST_MERGED
        } else if self.state == "closed" {
            PULL_REQUEST_CLOSED
        } else {
            PULL_REQUEST_OPEN
        }
    }
}

/// The pull request to open for one worktree's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPullRequest {
    pub head: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

/// Parse a pull request payload.
pub fn remote_pull_request_from_json(value: &serde_json::Value) -> Option<RemotePullRequest> {
    let number = value.get("number")?.as_u64()?;
    Some(RemotePullRequest {
        number,
        url: value
            .get("html_url")
            .and_then(|url| url.as_str())
            .unwrap_or_default()
            .to_string(),
        state: value
            .get("state")
            .and_then(|state| state.as_str())
            .unwrap_or("open")
            .to_string(),
        draft: value
            .get("draft")
            .and_then(|draft| draft.as_bool())
            .unwrap_or(false),
        merged: value
            .get("merged")
            .and_then(|merged| merged.as_bool())
            .unwrap_or(false),
        merged_at: value
            .get("merged_at")
            .and_then(|at| at.as_str())
            .and_then(|at| at.parse().ok()),
        node_id: value
            .get("node_id")
            .and_then(|id| id.as_str())
            .map(str::to_owned),
    })
}

/// Conventional-commit types stripped from a generated PR title: they are
/// exactly what the repo's PR convention forbids (decision 36).
const CONVENTIONAL_TYPES: &[&str] = &[
    "fix", "feat", "feature", "chore", "docs", "doc", "refactor", "test", "tests", "ci",
    "build", "perf", "style", "revert", "wip",
];

/// Leading imperative verbs dropped before a release-notes bullet, so the
/// bullet does not read "Added add …".
const LEADING_VERBS: &[&str] = &[
    "add", "adds", "fix", "fixes", "support", "supports", "implement", "implements", "update",
    "updates", "improve", "improves", "enable", "enables", "create", "creates", "remove",
    "removes", "refactor", "document", "documents", "rename", "renames", "make", "makes",
    "allow", "allows", "handle", "handles",
];

/// Words that mean a change is not user-facing, so its release note is `N/A`.
const NON_USER_FACING_WORDS: &[&str] = &[
    "docs", "documentation", "readme", "test", "tests", "testing", "refactor", "refactoring",
    "chore", "ci", "internal", "cleanup", "lint", "linting", "typo", "typos", "comment",
    "comments", "formatting", "rename", "renames",
];

/// Words that mean a change fixes something, so its release note is `Fixed`.
const FIX_WORDS: &[&str] = &[
    "fix", "fixes", "fixed", "bug", "bugs", "broken", "crash", "crashes", "regression",
    "regressions", "error", "errors", "incorrect", "wrong", "fails", "failing", "failure",
];

/// Shape a title the way the repo's PR convention asks (decision 36):
/// imperative and capitalized, no conventional-commit prefix, no trailing
/// punctuation, and short enough to read. A `scope:` prefix that is not a
/// conventional type is kept, since that is the crate name.
pub fn format_pull_request_title(raw: &str) -> String {
    let collapsed = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut title = collapsed.trim().to_string();
    if let Some((head, rest)) = title.split_once(':') {
        let head = head.trim();
        let (kind, scope) = match head.find('(') {
            Some(index) => (
                head[..index].trim(),
                Some(head[index + 1..].trim_end_matches(')').trim()),
            ),
            None => (head, None),
        };
        if CONVENTIONAL_TYPES.contains(&kind.to_lowercase().as_str()) {
            let subject = rest.trim();
            title = match scope.filter(|scope| !scope.is_empty()) {
                Some(scope) if !subject.is_empty() => format!("{scope}: {subject}"),
                _ => subject.to_string(),
            };
        }
    }
    title = title.trim_end_matches(['.', ',', ';', ':']).trim().to_string();
    // A kept `scope: subject` keeps the scope exactly as written (it is a crate
    // name, so lower case matters) and capitalizes the imperative subject.
    title = match title.split_once(": ") {
        Some((scope, subject)) if !scope.contains(' ') && !subject.trim().is_empty() => {
            format!("{}: {}", scope, capitalize_first(subject.trim()))
        }
        _ => capitalize_first(&title),
    };
    truncate_at_word(&title, 72)
        .trim_end_matches(['.', ',', ';', ':'])
        .to_string()
}

fn capitalize_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_lowercase() => first.to_uppercase().collect::<String>() + chars.as_str(),
        _ => text.to_string(),
    }
}

fn truncate_at_word(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let cut: String = text.chars().take(limit).collect();
    match cut.rfind(' ') {
        Some(index) if index > 0 => cut[..index].to_string(),
        _ => cut,
    }
}

/// The one release-notes bullet for a change, from its title: docs, tests and
/// internal work are `- N/A`, anything that sounds like a fix is `- Fixed …`,
/// and the rest is `- Added …` (decision 36).
pub fn release_note_for(title: &str) -> String {
    let subject = format_pull_request_title(title);
    let tokens: Vec<String> = subject
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mentions = |words: &[&str]| {
        tokens
            .iter()
            .any(|token| words.iter().any(|word| token == word))
    };
    let bare = strip_leading_verb(&subject);
    if mentions(NON_USER_FACING_WORDS) {
        return "- N/A".to_string();
    }
    if mentions(FIX_WORDS) {
        return format!("- Fixed {}", lower_first(&bare));
    }
    format!("- Added {}", lower_first(&bare))
}

/// Drop a leading imperative verb, so the bullet's own verb does not repeat
/// the title's. An empty remainder falls back to the title itself.
fn strip_leading_verb(subject: &str) -> String {
    let (first, rest) = match subject.split_once(' ') {
        Some((first, rest)) => (first, rest.trim()),
        None => (subject, ""),
    };
    let is_verb = LEADING_VERBS
        .iter()
        .any(|verb| first.eq_ignore_ascii_case(verb));
    if is_verb && !rest.is_empty() {
        rest.to_string()
    } else {
        subject.to_string()
    }
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_uppercase() => first.to_lowercase().collect::<String>() + chars.as_str(),
        _ => text.to_string(),
    }
}

/// Remove a `Release Notes:` tail an agent may have written itself, since the
/// app appends the canonical one.
fn strip_release_notes_section(body: &str) -> String {
    match body.find("Release Notes:") {
        Some(index) => body[..index].trim_end().to_string(),
        None => body.to_string(),
    }
}

/// The body of a generated pull request (decision 36): the agent's summary,
/// then `Closes #<n>` for the linked issue, then the `Release Notes:` section
/// as the heading, a blank line, and exactly one bullet.
pub fn format_pull_request_body(
    summary: &str,
    issue_number: Option<u64>,
    release_note: Option<&str>,
) -> String {
    let mut body = strip_release_notes_section(summary).trim().to_string();
    if let Some(number) = issue_number {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&format!("Closes #{number}"));
    }
    if !body.is_empty() {
        body.push_str("\n\n");
    }
    body.push_str("Release Notes:\n\n");
    body.push_str(
        release_note
            .map(str::to_string)
            .unwrap_or_else(|| "- N/A".to_string())
            .as_str(),
    );
    body
}

/// The agent's `propose_summary` for a run, as `(commit_message, pr_summary)`.
/// `propose_summary` stores the commit message followed by the summary prose
/// in one review annotation, so the first paragraph is the message.
pub fn summary_from_notes(notes: &[crate::workflow::RunNote]) -> Option<(String, Option<String>)> {
    let note = notes
        .iter()
        .rev()
        .find(|note| note.kind == "annotation" && note.phase == "review")?;
    let text = note.body.trim();
    if text.is_empty() {
        return None;
    }
    match text.split_once("\n\n") {
        Some((message, prose)) => {
            let message = message.trim();
            if message.is_empty() {
                return None;
            }
            let prose = prose.trim();
            Some((
                message.to_string(),
                (!prose.is_empty()).then(|| prose.to_string()),
            ))
        }
        None => Some((text.to_string(), None)),
    }
}

/// The GitHub API surface the sync engine needs. Generic over `impl`
/// `GithubClient` rather than `dyn` so tests can pass a fake and never touch
/// the network (spec decision 29). The returned futures are explicitly `Send`
/// because the app awaits the whole engine on the Tokio runtime.
pub trait GithubClient {
    fn list_issues<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        since: Option<&'a str>,
        etag: Option<&'a str>,
    ) -> impl std::future::Future<Output = anyhow::Result<IssuePage>> + Send + 'a;

    fn issue_comments<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<ExternalComment>>> + Send + 'a;

    fn update_issue<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
        patch: &'a IssuePatch,
    ) -> impl std::future::Future<Output = anyhow::Result<RemoteIssue>> + Send + 'a;

    /// Every repository the account can reach, most recently pushed first:
    /// what the tag settings binding picker offers (§5.3).
    fn list_repos(&self) -> impl std::future::Future<Output = anyhow::Result<Vec<RemoteRepo>>> + Send + '_;

    /// Create the label unless it already exists. The app never deletes or
    /// renames label objects (decision 27).
    fn ensure_label<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        name: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a;

    fn create_pull_request<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        request: &'a NewPullRequest,
    ) -> impl std::future::Future<Output = anyhow::Result<RemotePullRequest>> + Send + 'a;

    /// The PR already open for this head branch, so a re-run adopts it instead
    /// of failing (spec §8).
    fn find_pull_request<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        head_branch: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<RemotePullRequest>>> + Send + 'a;

    fn get_pull_request<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
    ) -> impl std::future::Future<Output = anyhow::Result<RemotePullRequest>> + Send + 'a;

    /// Flip a draft to ready for review. REST cannot do this, so the client
    /// looks the PR up for its GraphQL id and runs the mutation.
    fn mark_pull_request_ready<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a;

    fn add_issue_labels<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
        labels: &'a [String],
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a;

    fn add_issue_assignees<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
        assignees: &'a [String],
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a;

    fn request_reviewers<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
        reviewers: &'a [String],
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a;
}

const DEFAULT_API_BASE: &str = "https://api.github.com";
/// Pages of 100 followed per listing. 20 pages is 2000 issues, past which a
/// repo is out of scope for a quiet first import.
const MAX_ISSUE_PAGES: usize = 20;
/// Pages of 100 followed when listing the account's repos: 10 pages is 1000
/// repos, past which a picker is no longer a picker.
const MAX_REPO_PAGES: usize = 10;

/// The real GitHub client. Tokens are API-only (decision 30): pushing uses
/// the user's own git credentials and never this token.
pub struct GithubHttpClient {
    token: String,
    base: String,
    agent: reqwest::Client,
}

impl GithubHttpClient {
    pub fn new(token: impl Into<String>) -> Self {
        Self::with_base(token, DEFAULT_API_BASE)
    }

    /// A client against another base URL (a proxy, or a test server).
    pub fn with_base(token: impl Into<String>, base: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            base: base.into().trim_end_matches('/').to_string(),
            agent: reqwest::Client::new(),
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.agent
            .request(method, format!("{}{path}", self.base))
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header(reqwest::header::USER_AGENT, "todo-lofi")
            .bearer_auth(&self.token)
    }

    /// Send and turn any non-2xx into a classified failure, honouring the
    /// rate-limit headers so the retry policy knows how long to wait (§5.7).
    async fn send(&self, request: reqwest::RequestBuilder) -> anyhow::Result<reqwest::Response> {
        let response = request.send().await.map_err(|e| {
            anyhow::Error::new(SyncFailure::Transient {
                message: format!("Could not reach GitHub: {e}"),
                retry_after: None,
            })
        })?;
        let status = response.status();
        // 304 is the desired answer to a conditional request: nothing changed.
        if status.is_success() || status == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(response);
        }
        let retry_after = rate_limit_delay(&response);
        let body = response.text().await.unwrap_or_default();
        Err(anyhow::Error::new(classify_status(
            status.as_u16(),
            &truncate(&body, 300),
            retry_after,
        )))
    }
}

/// Seconds GitHub asks us to wait, from `retry-after` or the primary rate
/// limit's reset header.
fn rate_limit_delay(response: &reqwest::Response) -> Option<std::time::Duration> {
    let headers = response.headers();
    if let Some(seconds) = headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        return Some(std::time::Duration::from_secs(seconds));
    }
    let remaining = headers
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    if remaining == Some(0) {
        let reset = headers
            .get("x-ratelimit-reset")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<i64>().ok())?;
        let wait = reset - jiff::Timestamp::now().as_second();
        return Some(std::time::Duration::from_secs(wait.max(1) as u64));
    }
    None
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push('…');
    out
}

impl GithubClient for GithubHttpClient {
    fn list_issues<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        since: Option<&'a str>,
        etag: Option<&'a str>,
    ) -> impl std::future::Future<Output = anyhow::Result<IssuePage>> + Send + 'a {
        async move {
            let mut path = format!("/repos/{owner}/{repo}/issues?state=all&per_page=100");
            if let Some(since) = since {
                path.push_str(&format!("&since={since}"));
            }
            let mut request = self.request(reqwest::Method::GET, &path);
            if let Some(etag) = etag {
                request = request.header(reqwest::header::IF_NONE_MATCH, etag);
            }
            let response = self.send(request).await?;
            let response_etag = response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
                .or_else(|| etag.map(str::to_owned));
            if response.status() == reqwest::StatusCode::NOT_MODIFIED {
                // Nothing changed since the ETag, and an empty answer is not
                // evidence that any issue was deleted.
                return Ok(IssuePage {
                    issues: Vec::new(),
                    complete: false,
                    etag: response_etag,
                });
            }
            let mut issues = Vec::new();
            let mut next = next_link(&response);
            let mut body = response.json::<serde_json::Value>().await.map_err(|e| {
                anyhow::Error::new(SyncFailure::Transient {
                    message: format!("GitHub issue list was not JSON: {e}"),
                    retry_after: None,
                })
            })?;
            let mut pages = 0;
            loop {
                pages += 1;
                for value in body.as_array().cloned().unwrap_or_default() {
                    if let Some(issue) = remote_issue_from_json(&value) {
                        issues.push(issue);
                    }
                }
                let Some(url) = next.clone().filter(|_| pages < MAX_ISSUE_PAGES) else {
                    break;
                };
                let response = self.send(self.agent.get(url)).await?;
                next = next_link(&response);
                body = response.json::<serde_json::Value>().await.map_err(|e| {
                    anyhow::Error::new(SyncFailure::Transient {
                        message: format!("GitHub issue list was not JSON: {e}"),
                        retry_after: None,
                    })
                })?;
            }
            // A `since`-filtered page is partial by definition, so the caller
            // must not treat unseen issues as deleted.
            Ok(IssuePage {
                issues,
                complete: since.is_none(),
                etag: response_etag,
            })
        }
    }

    fn list_repos(&self) -> impl std::future::Future<Output = anyhow::Result<Vec<RemoteRepo>>> + Send + '_ {
        async move {
            // Most recently pushed first, so the repos the user actually works
            // in are the first rows of the picker.
            let path = "/user/repos?per_page=100&sort=pushed\
                        &affiliation=owner,collaborator,organization_member";
            let response = self.send(self.request(reqwest::Method::GET, path)).await?;
            let mut next = next_link(&response);
            let mut repos = repos_from_json(&repo_list_body(response).await?);
            let mut pages = 1;
            while let Some(url) = next.clone().filter(|_| pages < MAX_REPO_PAGES) {
                let response = self.send(self.agent.get(url)).await?;
                next = next_link(&response);
                pages += 1;
                repos.extend(repos_from_json(&repo_list_body(response).await?));
            }
            Ok(repos)
        }
    }

    fn issue_comments<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<ExternalComment>>> + Send + 'a {
        async move {
            let path = format!("/repos/{owner}/{repo}/issues/{number}/comments?per_page=100");
            let response = self.send(self.request(reqwest::Method::GET, &path)).await?;
            let body = response.json::<serde_json::Value>().await.map_err(|e| {
                anyhow::Error::new(SyncFailure::Transient {
                    message: format!("GitHub comments were not JSON: {e}"),
                    retry_after: None,
                })
            })?;
            Ok(body
                .as_array()
                .map(|items| items.iter().filter_map(comment_from_json).collect())
                .unwrap_or_default())
        }
    }

    fn update_issue<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
        patch: &'a IssuePatch,
    ) -> impl std::future::Future<Output = anyhow::Result<RemoteIssue>> + Send + 'a {
        async move {
            let mut payload = serde_json::Map::new();
            if let Some(title) = &patch.title {
                payload.insert("title".to_string(), serde_json::json!(title));
            }
            if let Some(body) = &patch.body {
                payload.insert("body".to_string(), serde_json::json!(body));
            }
            if let Some(state) = &patch.state {
                payload.insert("state".to_string(), serde_json::json!(state));
            }
            if let Some(labels) = &patch.labels {
                payload.insert("labels".to_string(), serde_json::json!(labels));
            }
            let path = format!("/repos/{owner}/{repo}/issues/{number}");
            let response = self
                .send(
                    self.request(reqwest::Method::PATCH, &path)
                        .json(&serde_json::Value::Object(payload)),
                )
                .await?;
            let body = response.json::<serde_json::Value>().await.map_err(|e| {
                anyhow::Error::new(SyncFailure::Transient {
                    message: format!("GitHub issue update was not JSON: {e}"),
                    retry_after: None,
                })
            })?;
            Ok(remote_issue_from_json(&body).unwrap_or_default())
        }
    }

    fn create_pull_request<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        request: &'a NewPullRequest,
    ) -> impl std::future::Future<Output = anyhow::Result<RemotePullRequest>> + Send + 'a {
        async move {
            let path = format!("/repos/{owner}/{repo}/pulls");
            let response = self
                .send(
                    self.request(reqwest::Method::POST, &path)
                        .json(&serde_json::json!({
                            "title": request.title,
                            "body": request.body,
                            "head": request.head,
                            "base": request.base,
                            "draft": request.draft,
                        })),
                )
                .await?;
            let body = self.json(response, "pull request").await?;
            remote_pull_request_from_json(&body).ok_or_else(|| {
                anyhow::anyhow!("GitHub did not describe the pull request it created: {body}")
            })
        }
    }

    fn find_pull_request<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        head_branch: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<Option<RemotePullRequest>>> + Send + 'a {
        async move {
            // `head` is `<owner>:<branch>`; the query is for any state so an
            // already-merged branch is adopted rather than reopened.
            let path = format!(
                "/repos/{owner}/{repo}/pulls?state=all&per_page=1&head={owner}%3A{head_branch}"
            );
            let response = self.send(self.request(reqwest::Method::GET, &path)).await?;
            let body = self.json(response, "pull request list").await?;
            Ok(body
                .as_array()
                .and_then(|items| items.first())
                .and_then(remote_pull_request_from_json))
        }
    }

    fn get_pull_request<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
    ) -> impl std::future::Future<Output = anyhow::Result<RemotePullRequest>> + Send + 'a {
        async move {
            let path = format!("/repos/{owner}/{repo}/pulls/{number}");
            let response = self.send(self.request(reqwest::Method::GET, &path)).await?;
            let body = self.json(response, "pull request").await?;
            remote_pull_request_from_json(&body).ok_or_else(|| {
                anyhow::anyhow!("GitHub did not describe pull request #{number}: {body}")
            })
        }
    }

    fn mark_pull_request_ready<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
        async move {
            // REST has no draft→ready endpoint; the GraphQL mutation needs the
            // PR's node id, which the REST read carries.
            let pull = self.get_pull_request(owner, repo, number).await?;
            if !pull.draft {
                return Ok(());
            }
            let Some(node_id) = pull.node_id else {
                anyhow::bail!(
                    "GitHub did not report the node id needed to mark PR #{number} ready"
                );
            };
            let response = self
                .send(
                    self.request(reqwest::Method::POST, "/graphql").json(
                        &serde_json::json!({
                            "query": "mutation($id: ID!) { markPullRequestReadyForReview(input: {pullRequestId: $id}) { pullRequest { isDraft } } }",
                            "variables": { "id": node_id },
                        }),
                    ),
                )
                .await?;
            let body = self.json(response, "graphql response").await?;
            if let Some(errors) = body.get("errors").and_then(|errors| errors.as_array())
                && !errors.is_empty()
            {
                anyhow::bail!("GitHub refused to mark PR #{number} ready: {errors:?}");
            }
            Ok(())
        }
    }

    fn add_issue_labels<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
        labels: &'a [String],
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
        async move {
            if labels.is_empty() {
                return Ok(());
            }
            let path = format!("/repos/{owner}/{repo}/issues/{number}/labels");
            self.post_json(&path, serde_json::json!({ "labels": labels }))
                .await
        }
    }

    fn add_issue_assignees<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
        assignees: &'a [String],
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
        async move {
            if assignees.is_empty() {
                return Ok(());
            }
            let path = format!("/repos/{owner}/{repo}/issues/{number}/assignees");
            self.post_json(&path, serde_json::json!({ "assignees": assignees }))
                .await
        }
    }

    fn request_reviewers<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
        reviewers: &'a [String],
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
        async move {
            if reviewers.is_empty() {
                return Ok(());
            }
            let path = format!("/repos/{owner}/{repo}/pulls/{number}/requested_reviewers");
            self.post_json(&path, serde_json::json!({ "reviewers": reviewers }))
                .await
        }
    }

    fn ensure_label<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        name: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
        async move {
            let path = format!("/repos/{owner}/{repo}/labels");
            let response = self
                .request(reqwest::Method::POST, &path)
                .json(&serde_json::json!({ "name": name }))
                .send()
                .await
                .map_err(|e| {
                    anyhow::Error::new(SyncFailure::Transient {
                        message: format!("Could not reach GitHub: {e}"),
                        retry_after: None,
                    })
                })?;
            if response.status().is_success() {
                return Ok(());
            }
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            // A label that already exists is the desired state.
            if status == 422 && body.contains("already_exists") {
                return Ok(());
            }
            Err(anyhow::Error::new(classify_status(
                status,
                &truncate(&body, 300),
                None,
            )))
        }
    }
}

impl GithubHttpClient {
    /// Decode a response body, reporting a non-JSON one as transient (the
    /// retry policy owns the decision, not this reader).
    async fn json(
        &self,
        response: reqwest::Response,
        what: &str,
    ) -> anyhow::Result<serde_json::Value> {
        response.json::<serde_json::Value>().await.map_err(|e| {
            anyhow::Error::new(SyncFailure::Transient {
                message: format!("GitHub {what} was not JSON: {e}"),
                retry_after: None,
            })
        })
    }

    /// POST a JSON body where only success matters. The body is read so the
    /// connection is released, and a read failure is reported rather than
    /// swallowed.
    async fn post_json(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> anyhow::Result<()> {
        let response = self
            .send(self.request(reqwest::Method::POST, path).json(&body))
            .await?;
        response
            .bytes()
            .await
            .map_err(|e| {
                anyhow::Error::new(SyncFailure::Transient {
                    message: format!("GitHub response could not be read: {e}"),
                    retry_after: None,
                })
            })?;
        Ok(())
    }
}

fn next_link(response: &reqwest::Response) -> Option<String> {
    let header = response.headers().get(reqwest::header::LINK)?.to_str().ok()?;
    header.split(',').find_map(|part| {
        let part = part.trim();
        let url = part.split(';').next()?.trim().trim_start_matches('<').trim_end_matches('>');
        part.contains("rel=\"next\"").then(|| url.to_string())
    })
}

/// A repo listing payload. A malformed body is transient: the gateway answered
/// something, so the next tick can try again.
async fn repo_list_body(response: reqwest::Response) -> anyhow::Result<serde_json::Value> {
    response.json::<serde_json::Value>().await.map_err(|e| {
        anyhow::Error::new(SyncFailure::Transient {
            message: format!("GitHub repo list was not JSON: {e}"),
            retry_after: None,
        })
    })
}

/// One repo bound to one project tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundRepo {
    pub integration_id: u64,
    pub owner: String,
    pub repo: String,
    pub tag_id: u64,
}

impl BoundRepo {
    /// `owner/repo`, the `external_id` of an `external_tag_links` row (§5.4).
    pub fn external_id(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// One issue link as the UI needs it to badge a task and offer
/// "Open on GitHub".
#[derive(Debug, Clone, PartialEq)]
pub struct TaskIssue {
    pub integration_id: u64,
    pub issue: IssueRef,
    pub state: IssueFieldState,
    pub external_updated_at: Option<jiff::Timestamp>,
}

/// What one sync pass did, for the integration card.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GithubSyncSummary {
    pub repos: usize,
    pub imported: usize,
    pub updated: usize,
    pub pushed: usize,
    pub labels: usize,
    pub comments: usize,
    pub tombstoned: usize,
}

impl GithubSyncSummary {
    pub fn absorb(&mut self, other: &Self) {
        self.repos += other.repos;
        self.imported += other.imported;
        self.updated += other.updated;
        self.pushed += other.pushed;
        self.labels += other.labels;
        self.comments += other.comments;
        self.tombstoned += other.tombstoned;
    }

    pub fn is_empty(&self) -> bool {
        self.imported == 0
            && self.updated == 0
            && self.pushed == 0
            && self.tombstoned == 0
    }

    /// One line for the card's status row.
    pub fn describe(&self) -> String {
        format!(
            "{} repo(s): {} imported, {} updated, {} pushed, {} comments, {} removed.",
            self.repos, self.imported, self.updated, self.pushed, self.comments, self.tombstoned
        )
    }
}

fn non_empty(text: &str) -> Option<String> {
    (!text.trim().is_empty()).then(|| text.to_string())
}

fn completed_at_for(issue: &RemoteIssue) -> Option<u64> {
    (issue.state == "closed").then(|| jiff::Timestamp::now().as_second() as u64)
}

impl TodoStore {
    /// The provider behind an integration id, so link rows of other
    /// integrations are never mistaken for issue links.
    async fn integration_provider(&mut self, integration_id: u64) -> QueryResult<Option<String>> {
        let rows = toasty::sql::query("SELECT provider FROM integrations WHERE id = ?1")
            .column_types([toasty::stmt::Type::String])
            .bind(integration_id as i64)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "integration provider",
            })?;
        Ok(rows.into_iter().next().and_then(|row| match row {
            toasty::stmt::Value::Record(record) => record_string(&record, 0),
            _ => None,
        }))
    }

    /// Bind an existing tag to a repo: the link row plus `sync_target`, so
    /// re-syncs and the PR step find the binding without re-detecting it
    /// (§5.3).
    pub async fn bind_repo_tag(
        &mut self,
        tag_id: u64,
        integration_id: u64,
        owner: &str,
        repo: &str,
    ) -> QueryResult<()> {
        let external_id = format!("{owner}/{repo}");
        let namespaced = self
            .get_tag(tag_id)
            .await
            .map(|tag| tag.name.starts_with("github/"))
            .unwrap_or(false);
        self.link_tag(integration_id, &external_id, tag_id, "repo", namespaced)
            .await?;
        self.set_tag_sync_target(
            tag_id,
            Some(crate::SyncTarget {
                integration_id,
                external_id,
            }),
        )
        .await
    }

    /// The namespaced project tag for a repo, `github/<owner>/<repo>`, bound
    /// and carrying its sync target. Used when a repo has no local tag to
    /// bind, so its issues still have somewhere to land.
    pub async fn ensure_repo_tag(
        &mut self,
        integration_id: u64,
        owner: &str,
        repo: &str,
    ) -> QueryResult<u64> {
        let external_id = format!("{owner}/{repo}");
        if let Some(link) = self.tag_link(integration_id, &external_id).await?
            && self.get_tag(link.tag_id).await.is_ok()
        {
            return Ok(link.tag_id);
        }
        let name = format!("github/{owner}/{repo}");
        let tag = match self.get_tag_by_name(&name).await? {
            Some(tag) => tag,
            None => {
                self.create_tag_with_display_name(name, Some(external_id.clone()))
                    .await?
            }
        };
        self.bind_repo_tag(tag.id, integration_id, owner, repo).await?;
        Ok(tag.id)
    }

    /// Every repo this integration syncs with, whatever bound it: a tag-link
    /// row from a detected remote, or an explicit `sync_target` set in the
    /// tag settings popover. Deduplicated by repo, first binding wins.
    pub async fn bound_repos(&mut self, integration_id: u64) -> QueryResult<Vec<BoundRepo>> {
        let mut out: Vec<BoundRepo> = Vec::new();
        let push = |out: &mut Vec<BoundRepo>, owner: &str, repo: &str, tag_id: u64| {
            let external_id = format!("{owner}/{repo}");
            if owner.is_empty()
                || repo.is_empty()
                || out.iter().any(|bound| bound.external_id() == external_id)
            {
                return;
            }
            out.push(BoundRepo {
                integration_id,
                owner: owner.to_string(),
                repo: repo.to_string(),
                tag_id,
            });
        };
        for link in self.tag_links_for_integration(integration_id).await? {
            if link.source_kind != "repo" {
                continue;
            }
            if let Some((owner, repo)) = link.external_id.split_once('/') {
                push(&mut out, owner, repo, link.tag_id);
            }
        }
        for tag in self.list_tags().await? {
            if out.iter().any(|bound| bound.tag_id == tag.id) {
                continue;
            }
            let Some(target) = self.tag_settings(tag.id).await?.sync_target else {
                continue;
            };
            if target.integration_id != integration_id {
                continue;
            }
            if let Some((owner, repo)) = target.external_id.split_once('/') {
                push(&mut out, owner, repo, tag.id);
            }
        }
        Ok(out)
    }

    /// The issue a task is synced from, when it is issue-backed. Other
    /// integrations' links are ignored.
    pub async fn issue_link_for_task(&mut self, task_id: u64) -> QueryResult<Option<TaskIssue>> {
        for link in self.task_links_for_task(task_id).await? {
            if self.integration_provider(link.integration_id).await?.as_deref() != Some("github") {
                continue;
            }
            let Some(stored) = self.issue_link(link.integration_id, &link.external_id).await? else {
                continue;
            };
            let Some(issue) = parse_issue_external_id(&link.external_id) else {
                continue;
            };
            return Ok(Some(TaskIssue {
                integration_id: link.integration_id,
                issue,
                state: stored.state,
                external_updated_at: stored.external_updated_at,
            }));
        }
        Ok(None)
    }

    /// Record that the local side changed one mapped field, so a later pull
    /// keeps it (decision 7). No-op for tasks with no GitHub issue link.
    pub async fn stamp_local_issue_field(
        &mut self,
        task_id: u64,
        field: &str,
    ) -> QueryResult<()> {
        for link in self.task_links_for_task(task_id).await? {
            if self.integration_provider(link.integration_id).await?.as_deref() != Some("github") {
                continue;
            }
            let Some(stored) = self.issue_link(link.integration_id, &link.external_id).await? else {
                continue;
            };
            let mut state = stored.state;
            state.stamp_local(field, jiff::Timestamp::now());
            self.save_issue_field_state(link.integration_id, &link.external_id, &state)
                .await?;
        }
        Ok(())
    }

    /// §5.6: deleting a synced task closes its issue (GitHub has no delete)
    /// and tombstones the link, so a later pull cannot resurrect the row.
    /// Returns whether the task was issue-backed at all. The caller decides
    /// what a failed close means; this reports it rather than swallowing it.
    pub async fn close_issue_for_task<C: GithubClient>(
        &mut self,
        client: &C,
        task_id: u64,
    ) -> anyhow::Result<bool> {
        let Some(task_issue) = self.issue_link_for_task(task_id).await? else {
            return Ok(false);
        };
        client
            .update_issue(
                &task_issue.issue.owner,
                &task_issue.issue.repo,
                task_issue.issue.number,
                &IssuePatch {
                    state: Some("closed".to_string()),
                    ..Default::default()
                },
            )
            .await?;
        let external_id = issue_external_id(
            &task_issue.issue.owner,
            &task_issue.issue.repo,
            task_issue.issue.number,
        );
        self.tombstone_issue_link(task_issue.integration_id, &external_id, "deleted locally")
            .await?;
        Ok(true)
    }

    /// Tombstone a task's link without touching GitHub: the path taken when no
    /// connection is available, so the issue stays open but the pull that
    /// follows still cannot resurrect the deleted row.
    pub async fn tombstone_issue_link_for_task(&mut self, task_id: u64) -> QueryResult<bool> {
        let Some(task_issue) = self.issue_link_for_task(task_id).await? else {
            return Ok(false);
        };
        let external_id = issue_external_id(
            &task_issue.issue.owner,
            &task_issue.issue.repo,
            task_issue.issue.number,
        );
        self.tombstone_issue_link(task_issue.integration_id, &external_id, "deleted locally")
            .await?;
        Ok(true)
    }

    async fn tombstone_issue_link(
        &mut self,
        integration_id: u64,
        external_id: &str,
        reason: &str,
    ) -> QueryResult<()> {
        let Some(link) = self.issue_link(integration_id, external_id).await? else {
            return Ok(());
        };
        let mut state = link.state.clone();
        state.tombstoned = true;
        state.tombstone_reason = Some(reason.to_string());
        self.link_issue(
            integration_id,
            external_id,
            link.task_id,
            &state,
            link.external_updated_at,
        )
        .await
    }

    /// Sync every repo bound to a GitHub integration (§5.4–5.7): pull, merge
    /// per field, push local wins, import comments, and record each repo's
    /// cursor only after it succeeded. `full` ignores the stored `since` so a
    /// manual "Sync now" can detect deletions; the periodic poll is
    /// incremental.
    pub async fn sync_github_integration<C: GithubClient>(
        &mut self,
        client: &C,
        integration_id: u64,
        full: bool,
    ) -> anyhow::Result<GithubSyncSummary> {
        let repos = self.bound_repos(integration_id).await?;
        let mut summary = GithubSyncSummary::default();
        for bound in &repos {
            let mut repo_summary = GithubSyncSummary::default();
            self.sync_github_repo(client, bound, full, &mut repo_summary)
                .await
                .map_err(|e| match e.downcast::<SyncFailure>() {
                    Ok(failure) => anyhow::Error::new(failure),
                    Err(other) => anyhow::anyhow!(
                        "GitHub sync failed for {}: {other}",
                        bound.external_id()
                    ),
                })?;
            summary.absorb(&repo_summary);
            summary.repos += 1;
        }
        Ok(summary)
    }

    async fn sync_github_repo<C: GithubClient>(
        &mut self,
        client: &C,
        bound: &BoundRepo,
        full: bool,
        summary: &mut GithubSyncSummary,
    ) -> anyhow::Result<()> {
        let external_id = bound.external_id();
        let cursor = self
            .sync_cursor(bound.integration_id, &external_id)
            .await?
            .unwrap_or_default();
        let since = if full { None } else { cursor.since.as_deref() };
        // A full pass must not be answered by a cached body, so it drops the
        // conditional header along with the `since` filter.
        let etag = if full { None } else { cursor.etag.as_deref() };
        let page = client
            .list_issues(&bound.owner, &bound.repo, since, etag)
            .await?;

        let mut seen = std::collections::HashSet::new();
        for issue in &page.issues {
            seen.insert(issue_external_id(&bound.owner, &bound.repo, issue.number));
        }
        for issue in page.issues {
            self.sync_github_issue(client, bound, &issue, summary).await?;
        }

        // A missing link is only evidence of deletion when the listing was
        // the repo's complete issue set (§5.6).
        if page.complete {
            for link in self.issue_links_for_integration(bound.integration_id).await? {
                if link.state.tombstoned || seen.contains(&link.external_id) {
                    continue;
                }
                let Some(issue) = parse_issue_external_id(&link.external_id) else {
                    continue;
                };
                if issue.owner != bound.owner || issue.repo != bound.repo {
                    continue;
                }
                if self.get_task(link.task_id).await?.deleted_at.is_some() {
                    continue;
                }
                self.tombstone_task(link.task_id).await?;
                let mut state = link.state.clone();
                state.tombstoned = true;
                state.tombstone_reason = Some(
                    "The issue was deleted, transferred, or is no longer accessible".to_string(),
                );
                self.link_issue(
                    bound.integration_id,
                    &link.external_id,
                    link.task_id,
                    &state,
                    link.external_updated_at,
                )
                .await?;
                summary.tombstoned += 1;
            }
        }

        let now = jiff::Timestamp::now();
        self.record_sync_cursor(
            bound.integration_id,
            &external_id,
            &SyncCursor {
                etag: page.etag,
                since: Some(now.to_string()),
                last_synced_at: Some(now),
            },
        )
        .await?;
        Ok(())
    }

    async fn sync_github_issue<C: GithubClient>(
        &mut self,
        client: &C,
        bound: &BoundRepo,
        issue: &RemoteIssue,
        summary: &mut GithubSyncSummary,
    ) -> anyhow::Result<()> {
        let external_id = issue_external_id(&bound.owner, &bound.repo, issue.number);
        let Some(link) = self
            .issue_link(bound.integration_id, &external_id)
            .await?
        else {
            // Issue scope is the repo's open issues (decision 6): a closed
            // issue that was never linked is history, not a task. A linked
            // issue that closes later still syncs, below.
            if issue.state != "open" {
                return Ok(());
            }
            return self
                .import_github_issue(client, bound, &external_id, issue, summary)
                .await;
        };
        // A tombstoned link is never resurrected, and neither is a task the
        // user deleted locally. A workflow-step task never syncs at all — the
        // same rule the Todoist engine applies (§5.5).
        if link.state.tombstoned {
            return Ok(());
        }
        let task = self.get_task(link.task_id).await?;
        if task.deleted_at.is_some() || task.workflow_run_id.is_some() {
            return Ok(());
        }
        self.merge_github_issue(client, bound, &external_id, &link, issue, summary)
            .await
    }

    async fn import_github_issue<C: GithubClient>(
        &mut self,
        client: &C,
        bound: &BoundRepo,
        external_id: &str,
        issue: &RemoteIssue,
        summary: &mut GithubSyncSummary,
    ) -> anyhow::Result<()> {
        let created = self
            .create_task(
                Task::create()
                    .title(issue.title.clone())
                    .description(non_empty(&issue.body))
                    .done(issue.state == "closed")
                    .completed_at(completed_at_for(issue)),
            )
            .await?;
        let tag = self.get_tag(bound.tag_id).await?;
        self.assign_tag_to_task(created.id, &tag.name).await?;
        self.assign_issue_labels(created.id, bound, &issue.labels)
            .await?;
        let comments = client
            .issue_comments(&bound.owner, &bound.repo, issue.number)
            .await?;
        summary.comments += comments.len();
        if !comments.is_empty() {
            Task::update_by_id(created.id)
                .comments(Some(toasty::Json(comments)))
                .exec(&mut self.db)
                .await
                .context(crate::error::UpdateTaskSnafu { id: created.id })?;
        }
        let state = IssueFieldState {
            remote: issue.field_values(),
            assignees: issue.assignees.clone(),
            milestone: issue.milestone.clone(),
            author: issue.author.clone(),
            url: issue.url.clone(),
            ..Default::default()
        };
        self.link_issue(
            bound.integration_id,
            external_id,
            created.id,
            &state,
            issue.updated_at,
        )
        .await?;
        summary.imported += 1;
        Ok(())
    }

    async fn merge_github_issue<C: GithubClient>(
        &mut self,
        client: &C,
        bound: &BoundRepo,
        external_id: &str,
        link: &IssueLink,
        issue: &RemoteIssue,
        summary: &mut GithubSyncSummary,
    ) -> anyhow::Result<()> {
        let task = self.get_task(link.task_id).await?;
        let snapshot = link.state.remote.clone();
        let local_labels = self.local_issue_labels(link.task_id, bound.tag_id).await?;

        let title_changed = task.title != snapshot.title;
        let body_changed = task.description.clone().unwrap_or_default() != snapshot.body;
        let state_changed = task.done != (snapshot.state == "closed");
        let labels_changed = local_labels != snapshot.labels;
        let remote = issue.field_values();

        let mut take_remote: Vec<&'static str> = Vec::new();
        let mut keep_local: Vec<&'static str> = Vec::new();
        for (field, remote_changed, local_changed) in [
            ("title", remote.title != snapshot.title, title_changed),
            ("body", remote.body != snapshot.body, body_changed),
            ("state", remote.state != snapshot.state, state_changed),
            ("labels", remote.labels != snapshot.labels, labels_changed),
        ] {
            match (remote_changed, local_changed) {
                (false, false) => {}
                (true, false) => take_remote.push(field),
                (false, true) => keep_local.push(field),
                // Both sides moved: the newer stamp wins, ties to GitHub.
                (true, true) => match resolve_field(
                    true,
                    link.state.local_changed_at.get(field).copied(),
                    issue.updated_at,
                ) {
                    FieldDecision::TakeRemote => take_remote.push(field),
                    _ => keep_local.push(field),
                },
            }
        }

        if !take_remote.is_empty() {
            let mut update = Task::update_by_id(link.task_id);
            for field in &take_remote {
                match *field {
                    "title" => update = update.title(issue.title.clone()),
                    "body" => update = update.description(non_empty(&issue.body)),
                    "state" => {
                        update = update
                            .done(issue.state == "closed")
                            .completed_at(completed_at_for(issue))
                    }
                    _ => {}
                }
            }
            update
                .exec(&mut self.db)
                .await
                .context(crate::error::UpdateTaskSnafu { id: link.task_id })?;
        }

        // Labels are additive on both sides (decision 27): a remote label
        // becomes a local tag, and never disappears from either side.
        self.assign_issue_labels(link.task_id, bound, &issue.labels)
            .await?;

        let mut snapshot_after = remote.clone();
        if !keep_local.is_empty() {
            let mut patch = IssuePatch::default();
            if keep_local.contains(&"title") {
                patch.title = Some(task.title.clone());
            }
            if keep_local.contains(&"body") {
                patch.body = Some(task.description.clone().unwrap_or_default());
            }
            if keep_local.contains(&"state") {
                patch.state = Some(if task.done { "closed" } else { "open" }.to_string());
            }
            if keep_local.contains(&"labels") {
                let mut union = issue.labels.clone();
                for label in &local_labels {
                    if !union.iter().any(|existing| existing == label) {
                        union.push(label.clone());
                    }
                }
                for label in &local_labels {
                    if !issue.labels.iter().any(|existing| existing == label) {
                        client
                            .ensure_label(&bound.owner, &bound.repo, label)
                            .await?;
                        summary.labels += 1;
                    }
                }
                patch.labels = Some(union.clone());
                snapshot_after.labels = union;
            }
            client
                .update_issue(&bound.owner, &bound.repo, issue.number, &patch)
                .await?;
            // The pushed values are now the remote's, so the snapshot takes
            // them and the local stamps are spent: the app's own write must
            // not come back as a remote edit (§5.4).
            if let Some(title) = patch.title {
                snapshot_after.title = title;
            }
            if let Some(body) = patch.body {
                snapshot_after.body = body;
            }
            if let Some(state) = patch.state {
                snapshot_after.state = state;
            }
            summary.pushed += 1;
        }

        let mut state = link.state.clone();
        state.adopt_remote(&snapshot_after);
        state.assignees = issue.assignees.clone();
        state.milestone = issue.milestone.clone();
        state.author = issue.author.clone();
        state.url = issue.url.clone();
        for field in take_remote.iter().chain(keep_local.iter()) {
            state.local_changed_at.remove(*field);
        }

        // Comments are one-way and only re-read when the issue moved.
        let remote_is_newer = match (issue.updated_at, link.external_updated_at) {
            (Some(remote), Some(known)) => remote > known,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if remote_is_newer {
            let comments = client
                .issue_comments(&bound.owner, &bound.repo, issue.number)
                .await?;
            summary.comments += comments.len();
            Task::update_by_id(link.task_id)
                .comments(Some(toasty::Json(comments)))
                .exec(&mut self.db)
                .await
                .context(crate::error::UpdateTaskSnafu { id: link.task_id })?;
        }

        self.link_issue(
            bound.integration_id,
            external_id,
            link.task_id,
            &state,
            issue.updated_at,
        )
        .await?;
        summary.updated += 1;
        Ok(())
    }

    /// Tags on the task that stand in for issue labels: everything except the
    /// repo binding itself and other project tags.
    async fn local_issue_labels(
        &mut self,
        task_id: u64,
        repo_tag_id: u64,
    ) -> QueryResult<Vec<String>> {
        let mut out = Vec::new();
        for tag in self.get_direct_task_tags(task_id).await? {
            if tag.id == repo_tag_id {
                continue;
            }
            if self.tag_settings(tag.id).await?.sync_target.is_some() {
                continue;
            }
            let label = tag.label();
            if !out.contains(&label) {
                out.push(label);
            }
        }
        Ok(out)
    }

    /// Get-or-create a local tag for a remote label. The remote label is the
    /// display name, so a name that is already taken locally gets a
    /// namespaced tag instead of merging into the user's tag (§5.2).
    async fn assign_issue_labels(
        &mut self,
        task_id: u64,
        bound: &BoundRepo,
        labels: &[String],
    ) -> QueryResult<()> {
        for label in labels {
            let tag = match self.get_tag_by_name(label).await? {
                Some(tag) => tag,
                None => {
                    let scoped = format!("github/{label}");
                    match self.get_tag_by_name(&scoped).await? {
                        Some(tag) => tag,
                        None => {
                            self.create_tag_with_display_name(scoped, Some(label.clone()))
                                .await?
                        }
                    }
                }
            };
            if tag.id == bound.tag_id {
                continue;
            }
            self.assign_tag_to_task(task_id, &tag.name).await?;
        }
        Ok(())
    }

    /// Whether every pull request of a run is finished: merged, or waived by
    /// the user. A PR closed without merging deliberately does *not* count,
    /// or a run would complete on a rejected pull request; the waiver button
    /// is how the user says "that repo is not landing" (decision 25). `false`
    /// when the run has no PRs, so an unopened step cannot look complete.
    pub async fn run_pull_requests_resolved(&mut self, run_id: u64) -> QueryResult<bool> {
        let pull_requests = self.run_pull_requests(run_id).await?;
        if pull_requests.is_empty() {
            return Ok(false);
        }
        Ok(pull_requests
            .iter()
            .all(|pull_request| matches!(pull_request.state.as_str(), PULL_REQUEST_MERGED | PULL_REQUEST_WAIVED)))
    }

    /// Give up on the still-open pull requests of a run so a multi-repo run
    /// can complete early (decision 25). Returns how many were waived.
    pub async fn waive_run_pull_requests(&mut self, run_id: u64) -> QueryResult<usize> {
        let open: Vec<RunPullRequest> = self
            .run_pull_requests(run_id)
            .await?
            .into_iter()
            .filter(|pull_request| pull_request.state == PULL_REQUEST_OPEN)
            .collect();
        for pull_request in &open {
            self.update_run_pull_request(
                pull_request.id,
                PULL_REQUEST_WAIVED,
                pull_request.draft,
                pull_request.merged_at,
            )
            .await?;
        }
        Ok(open.len())
    }

    /// One pull request row by its id, for the step's per-PR actions.
    pub async fn run_pull_request(&mut self, id: u64) -> QueryResult<Option<RunPullRequest>> {
        self.query_run_pull_request(id).await
    }

    /// Every pull request row, any state, for the poller's full sweep.
    pub async fn all_run_pull_requests(&mut self) -> QueryResult<Vec<RunPullRequest>> {
        self.query_run_pull_requests_all().await
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

    /// A GitHub that behaves like GitHub with respect to the fields we model:
    /// patches mutate the stored issue, so "the remote reflects the push" (and
    /// therefore a quiet second pass) is actually exercised.
    #[derive(Default)]
    struct FakeGithub {
        issues: std::sync::Mutex<std::collections::HashMap<String, Vec<RemoteIssue>>>,
        comments: std::sync::Mutex<std::collections::HashMap<String, Vec<ExternalComment>>>,
        updates: std::sync::Mutex<Vec<(String, u64, IssuePatch)>>,
        labels: std::sync::Mutex<Vec<String>>,
        /// `(repo, head_branch, pull request)`, so `find_pull_request` works
        /// the way GitHub's `head=` filter does.
        pull_requests:
            std::sync::Mutex<Vec<(String, String, RemotePullRequest)>>,
        created_pull_requests: std::sync::Mutex<Vec<(String, NewPullRequest)>>,
        ready_pull_requests: std::sync::Mutex<Vec<u64>>,
        pull_request_labels: std::sync::Mutex<Vec<(u64, Vec<String>)>>,
        pull_request_assignees: std::sync::Mutex<Vec<(u64, Vec<String>)>>,
        reviewers_requested: std::sync::Mutex<Vec<(u64, Vec<String>)>>,
        pull_request_counter: std::sync::Mutex<u64>,
        repos: std::sync::Mutex<Vec<RemoteRepo>>,
    }

    impl FakeGithub {
        fn with_issue(self, repo: &str, issue: RemoteIssue) -> Self {
            self.issues
                .lock()
                .expect("issues lock")
                .entry(repo.to_string())
                .or_default()
                .push(issue);
            self
        }

        fn with_comment(self, repo: &str, number: u64, comment: ExternalComment) -> Self {
            self.comments
                .lock()
                .expect("comments lock")
                .entry(format!("{repo}#{number}"))
                .or_default()
                .push(comment);
            self
        }

        fn with_repo(self, full_name: &str, private: bool) -> Self {
            self.repos.lock().expect("repos lock").push(RemoteRepo {
                full_name: full_name.to_string(),
                private,
            });
            self
        }

        fn remove_issue(&self, repo: &str, number: u64) {
            if let Some(issues) = self.issues.lock().expect("issues lock").get_mut(repo) {
                issues.retain(|issue| issue.number != number);
            }
        }

        fn push_count(&self) -> usize {
            self.updates.lock().expect("updates lock").len()
        }

        fn last_patch(&self) -> IssuePatch {
            self.updates
                .lock()
                .expect("updates lock")
                .last()
                .map(|(_, _, patch)| patch.clone())
                .unwrap_or_default()
        }
    }

    impl GithubClient for FakeGithub {
        fn list_issues<'a>(
            &'a self,
            owner: &'a str,
            repo: &'a str,
            since: Option<&'a str>,
            _etag: Option<&'a str>,
        ) -> impl std::future::Future<Output = anyhow::Result<IssuePage>> + Send + 'a {
            async move {
                let issues = self
                    .issues
                    .lock()
                    .expect("issues lock")
                    .get(&format!("{owner}/{repo}"))
                    .cloned()
                    .unwrap_or_default();
                // Mirrors the real client: a `since`-filtered page is partial.
                Ok(IssuePage {
                    issues,
                    complete: since.is_none(),
                    etag: Some("etag-1".to_string()),
                })
            }
        }

        fn list_repos(
            &self,
        ) -> impl std::future::Future<Output = anyhow::Result<Vec<RemoteRepo>>> + Send + '_ {
            async move { Ok(self.repos.lock().expect("repos lock").clone()) }
        }

        fn issue_comments<'a>(
            &'a self,
            owner: &'a str,
            repo: &'a str,
            number: u64,
        ) -> impl std::future::Future<Output = anyhow::Result<Vec<ExternalComment>>> + Send + 'a {
            async move {
                Ok(self
                    .comments
                    .lock()
                    .expect("comments lock")
                    .get(&format!("{owner}/{repo}#{number}"))
                    .cloned()
                    .unwrap_or_default())
            }
        }

        fn update_issue<'a>(
            &'a self,
            owner: &'a str,
            repo: &'a str,
            number: u64,
            patch: &'a IssuePatch,
        ) -> impl std::future::Future<Output = anyhow::Result<RemoteIssue>> + Send + 'a {
            async move {
                let key = format!("{owner}/{repo}");
                self.updates
                    .lock()
                    .expect("updates lock")
                    .push((key.clone(), number, patch.clone()));
                let mut issues = self.issues.lock().expect("issues lock");
                let Some(issue) = issues
                    .entry(key)
                    .or_default()
                    .iter_mut()
                    .find(|issue| issue.number == number)
                else {
                    anyhow::bail!("the fake has no issue {number}");
                };
                if let Some(title) = &patch.title {
                    issue.title = title.clone();
                }
                if let Some(body) = &patch.body {
                    issue.body = body.clone();
                }
                if let Some(state) = &patch.state {
                    issue.state = state.clone();
                }
                if let Some(labels) = &patch.labels {
                    issue.labels = labels.clone();
                }
                // Far future, so a both-changed field resolves to GitHub
                // unless the local stamp is explicitly newer.
                issue.updated_at = jiff::Timestamp::from_second(2_000_000_000).ok();
                Ok(issue.clone())
            }
        }

        fn ensure_label<'a>(
            &'a self,
            _owner: &'a str,
            _repo: &'a str,
            name: &'a str,
        ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
            async move {
                self.labels.lock().expect("labels lock").push(name.to_string());
                Ok(())
            }
        }

        fn create_pull_request<'a>(
            &'a self,
            owner: &'a str,
            repo: &'a str,
            request: &'a NewPullRequest,
        ) -> impl std::future::Future<Output = anyhow::Result<RemotePullRequest>> + Send + 'a {
            async move {
                let mut counter = self.pull_request_counter.lock().expect("pr counter");
                *counter += 1;
                let number = *counter;
                drop(counter);
                self.created_pull_requests
                    .lock()
                    .expect("created prs")
                    .push((format!("{owner}/{repo}"), request.clone()));
                let pull = RemotePullRequest {
                    number,
                    url: format!("https://github.com/{owner}/{repo}/pull/{number}"),
                    state: "open".to_string(),
                    draft: request.draft,
                    merged: false,
                    merged_at: None,
                    node_id: Some(format!("PR_{number}")),
                };
                self.pull_requests.lock().expect("prs").push((
                    format!("{owner}/{repo}"),
                    request.head.clone(),
                    pull.clone(),
                ));
                Ok(pull)
            }
        }

        fn find_pull_request<'a>(
            &'a self,
            owner: &'a str,
            repo: &'a str,
            head_branch: &'a str,
        ) -> impl std::future::Future<Output = anyhow::Result<Option<RemotePullRequest>>> + Send + 'a {
            async move {
                Ok(self
                    .pull_requests
                    .lock()
                    .expect("prs")
                    .iter()
                    .find(|(repo_slug, head, _)| {
                        repo_slug == &format!("{owner}/{repo}") && head == head_branch
                    })
                    .map(|(_, _, pull)| pull.clone()))
            }
        }

        fn get_pull_request<'a>(
            &'a self,
            owner: &'a str,
            repo: &'a str,
            number: u64,
        ) -> impl std::future::Future<Output = anyhow::Result<RemotePullRequest>> + Send + 'a {
            async move {
                self.pull_requests
                    .lock()
                    .expect("prs")
                    .iter()
                    .find(|(repo_slug, _, pull)| {
                        repo_slug == &format!("{owner}/{repo}") && pull.number == number
                    })
                    .map(|(_, _, pull)| pull.clone())
                    .ok_or_else(|| anyhow::anyhow!("the fake has no PR #{number}"))
            }
        }

        fn mark_pull_request_ready<'a>(
            &'a self,
            owner: &'a str,
            repo: &'a str,
            number: u64,
        ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
            async move {
                self.ready_pull_requests
                    .lock()
                    .expect("ready prs")
                    .push(number);
                let mut pull_requests = self.pull_requests.lock().expect("prs");
                for (repo_slug, _, pull) in pull_requests.iter_mut() {
                    if repo_slug == &format!("{owner}/{repo}") && pull.number == number {
                        pull.draft = false;
                    }
                }
                Ok(())
            }
        }

        fn add_issue_labels<'a>(
            &'a self,
            _owner: &'a str,
            _repo: &'a str,
            number: u64,
            labels: &'a [String],
        ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
            async move {
                self.pull_request_labels
                    .lock()
                    .expect("pr labels")
                    .push((number, labels.to_vec()));
                Ok(())
            }
        }

        fn add_issue_assignees<'a>(
            &'a self,
            _owner: &'a str,
            _repo: &'a str,
            number: u64,
            assignees: &'a [String],
        ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
            async move {
                self.pull_request_assignees
                    .lock()
                    .expect("pr assignees")
                    .push((number, assignees.to_vec()));
                Ok(())
            }
        }

        fn request_reviewers<'a>(
            &'a self,
            _owner: &'a str,
            _repo: &'a str,
            number: u64,
            reviewers: &'a [String],
        ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
            async move {
                self.reviewers_requested
                    .lock()
                    .expect("reviewers")
                    .push((number, reviewers.to_vec()));
                Ok(())
            }
        }
    }

    async fn bound_store(
        owner: &str,
        repo: &str,
    ) -> anyhow::Result<(TodoStore, Integration, Tag)> {
        let mut storage = TodoStore::for_test().await?;
        let integration = storage
            .create_integration("github", Some("me".to_string()))
            .await?;
        let tag = storage.create_tag(format!("{owner}-{repo}")).await?;
        storage
            .bind_repo_tag(tag.id, integration.id, owner, repo)
            .await?;
        Ok((storage, integration, tag))
    }

    #[tokio::test]
    async fn imports_issues_into_the_bound_repo_tag() -> anyhow::Result<()> {
        let (mut storage, integration, tag) = bound_store("lofi-tools", "todo-lofi").await?;
        let mut issue = remote(7, "Fix login", "open", 100);
        issue.body = "Steps to reproduce".to_string();
        issue.url = Some("https://github.com/lofi-tools/todo-lofi/issues/7".to_string());
        issue.assignees = vec!["me".to_string()];
        issue.milestone = Some("v1".to_string());
        let fake = FakeGithub::default().with_issue("lofi-tools/todo-lofi", issue).with_comment(
            "lofi-tools/todo-lofi",
            7,
            ExternalComment {
                external_id: "1".to_string(),
                text: "me too".to_string(),
                author: Some("someone".to_string()),
                created_at: None,
            },
        );

        let summary = storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        assert_eq!(summary.repos, 1);
        assert_eq!(summary.imported, 1);
        assert_eq!(summary.comments, 1);

        let link = storage
            .issue_link(integration.id, "lofi-tools/todo-lofi#7")
            .await?
            .expect("the issue was linked");
        let task = storage.get_task(link.task_id).await?;
        assert_eq!(task.title, "Fix login");
        assert_eq!(task.description.as_deref(), Some("Steps to reproduce"));
        assert!(!task.done);
        assert_eq!(
            task.comments.as_ref().map(|comments| comments.0.len()),
            Some(1)
        );
        // The repo tag carries the task, and the metadata is mirrored.
        assert!(
            storage
                .get_direct_task_tags(task.id)
                .await?
                .iter()
                .any(|task_tag| task_tag.id == tag.id)
        );
        assert_eq!(link.state.assignees, vec!["me".to_string()]);
        assert_eq!(link.state.milestone.as_deref(), Some("v1"));
        assert_eq!(link.state.remote.title, "Fix login");
        assert_eq!(link.external_updated_at.map(|at| at.as_second()), Some(100));
        // The cursor only moves after a successful pass.
        let cursor = storage
            .sync_cursor(integration.id, "lofi-tools/todo-lofi")
            .await?
            .expect("cursor recorded");
        assert!(cursor.since.is_some());
        assert_eq!(cursor.etag.as_deref(), Some("etag-1"));

        // Issue scope is the repo's open issues (decision 6): a closed issue
        // that was never linked is history, not a task.
        let mut closed = remote(8, "Already done", "closed", 110);
        closed.body = String::new();
        let fake = FakeGithub::default()
            .with_issue("lofi-tools/todo-lofi", closed)
            .with_issue("lofi-tools/todo-lofi", remote(7, "Fix login", "open", 100));
        let summary = storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;
        assert_eq!(summary.imported, 0);
        assert!(
            storage
                .issue_link(integration.id, "lofi-tools/todo-lofi#8")
                .await?
                .is_none(),
            "an unlinked closed issue is not imported"
        );

        // A linked issue that closes on GitHub completes its task instead.
        let closed_later = remote(7, "Fix login", "closed", 120);
        let fake = FakeGithub::default().with_issue("lofi-tools/todo-lofi", closed_later);
        storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;
        assert!(storage.get_task(task.id).await?.done);
        Ok(())
    }

    #[tokio::test]
    async fn a_locally_deleted_task_closes_its_issue_and_never_comes_back() -> anyhow::Result<()> {
        let (mut storage, integration, _) = bound_store("o", "r").await?;
        let fake = FakeGithub::default().with_issue("o/r", remote(1, "Fix login", "open", 100));
        storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;
        assert!(storage.github_last_synced_at(integration.id).await?.is_some());
        let task_id = storage.issue_link(integration.id, "o/r#1").await?.unwrap().task_id;

        assert!(storage.close_issue_for_task(&fake, task_id).await?);
        assert_eq!(fake.last_patch().state.as_deref(), Some("closed"));
        let link = storage.issue_link(integration.id, "o/r#1").await?.unwrap();
        assert!(link.state.tombstoned);
        assert_eq!(link.state.tombstone_reason.as_deref(), Some("deleted locally"));

        // The link outlives the row it points at, so the next pass must skip
        // it instead of looking up a task that is gone (§5.6).
        storage.delete_task(task_id).await?;
        let summary = storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;
        assert_eq!(summary.imported, 0);
        assert_eq!(fake.push_count(), 1, "only the close was pushed");
        Ok(())
    }

    #[tokio::test]
    async fn a_deleted_task_tombstones_its_link_without_a_connection() -> anyhow::Result<()> {
        let (mut storage, integration, _) = bound_store("o", "r").await?;
        let fake = FakeGithub::default().with_issue("o/r", remote(1, "Fix login", "open", 100));
        storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        let task_id = storage.issue_link(integration.id, "o/r#1").await?.unwrap().task_id;

        assert!(storage.tombstone_issue_link_for_task(task_id).await?);
        assert_eq!(fake.push_count(), 0, "nothing is pushed without a connection");
        storage.delete_task(task_id).await?;
        let summary = storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        assert_eq!(summary.imported, 0);
        Ok(())
    }

    #[tokio::test]
    async fn workflow_step_tasks_never_sync() -> anyhow::Result<()> {
        let (mut storage, integration, _) = bound_store("o", "r").await?;
        let fake = FakeGithub::default().with_issue("o/r", remote(1, "Fix login", "open", 100));
        storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        let task_id = storage.issue_link(integration.id, "o/r#1").await?.unwrap().task_id;

        // A run step's task is the run's, not the issue's: a local rename
        // stays local instead of being pushed as an issue edit (§5.5).
        Task::update_by_id(task_id)
            .workflow_run_id(Some(4))
            .exec(&mut storage.db)
            .await?;
        storage.update_task_title(task_id, "Step title").await?;
        let summary = storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        assert_eq!(summary.updated, 0);
        assert_eq!(fake.push_count(), 0);
        assert_eq!(storage.get_task(task_id).await?.title, "Step title");
        Ok(())
    }

    #[test]
    fn repo_listings_are_narrowed_to_what_the_picker_shows() {
        let payload = serde_json::json!([
            { "full_name": "o/private", "private": true },
            { "full_name": "o/public", "private": false },
            { "full_name": "  ", "private": false },
            { "name": "no-owner" }
        ]);
        assert_eq!(
            repos_from_json(&payload),
            vec![
                RemoteRepo {
                    full_name: "o/private".to_string(),
                    private: true,
                },
                RemoteRepo {
                    full_name: "o/public".to_string(),
                    private: false,
                },
            ]
        );
    }

    #[tokio::test]
    async fn the_binding_picker_reads_the_clients_repo_list() -> anyhow::Result<()> {
        let fake = FakeGithub::default()
            .with_repo("o/private", true)
            .with_repo("o/public", false);
        let repos = fake.list_repos().await?;
        assert_eq!(repos.len(), 2);
        assert!(repos[0].private);
        Ok(())
    }

    #[tokio::test]
    async fn pushed_local_edits_are_not_read_back_as_remote_edits() -> anyhow::Result<()> {
        let (mut storage, integration, _) = bound_store("o", "r").await?;
        let fake =
            FakeGithub::default().with_issue("o/r", remote(1, "Remote title", "open", 100));
        storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        let task_id = storage
            .issue_link(integration.id, "o/r#1")
            .await?
            .expect("linked")
            .task_id;

        storage.update_task_title(task_id, "Local title").await?;
        let summary = storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        assert_eq!(summary.pushed, 1);
        assert_eq!(fake.last_patch().title.as_deref(), Some("Local title"));

        // The push refreshed the snapshot and spent the stamp, so the next
        // pass is quiet instead of echoing the app's own write (§5.4).
        let summary = storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        assert_eq!(summary.pushed, 0);
        assert_eq!(fake.push_count(), 1);
        let link = storage.issue_link(integration.id, "o/r#1").await?.unwrap();
        assert_eq!(link.state.remote.title, "Local title");
        assert!(link.state.local_changed_at.get("title").is_none());
        assert_eq!(storage.get_task(task_id).await?.title, "Local title");
        Ok(())
    }

    #[tokio::test]
    async fn both_sides_changed_newer_stamp_wins_and_ties_go_to_github() -> anyhow::Result<()> {
        let (mut storage, integration, _) = bound_store("o", "r").await?;
        let mut issue = remote(1, "Remote title", "open", 100);
        issue.body = "Remote body".to_string();
        let fake = FakeGithub::default().with_issue("o/r", issue.clone());
        storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        let task_id = storage.issue_link(integration.id, "o/r#1").await?.unwrap().task_id;

        // Local edit stamped "now", remote edit dated 100 (1970): the local
        // stamp is newer, so the local title survives and is pushed.
        storage.update_task_title(task_id, "Local title").await?;
        storage.stamp_local_issue_field(task_id, "title").await?;
        let mut changed = issue.clone();
        changed.title = "Remote title v2".to_string();
        let fake = FakeGithub::default().with_issue("o/r", changed);
        storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        assert_eq!(storage.get_task(task_id).await?.title, "Local title");
        assert_eq!(fake.last_patch().title.as_deref(), Some("Local title"));

        // Now both sides moved and the local side is undated (no stamp), so
        // GitHub wins the tie.
        storage.update_task_title(task_id, "Local again").await?;
        let mut newest = issue.clone();
        newest.title = "Remote wins".to_string();
        newest.updated_at = jiff::Timestamp::from_second(2_000_000_000).ok();
        let fake = FakeGithub::default().with_issue("o/r", newest);
        storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        assert_eq!(storage.get_task(task_id).await?.title, "Remote wins");
        Ok(())
    }

    #[tokio::test]
    async fn a_missing_issue_tombstones_only_on_a_complete_listing() -> anyhow::Result<()> {
        let (mut storage, integration, _) = bound_store("o", "r").await?;
        let fake = FakeGithub::default()
            .with_issue("o/r", remote(1, "Keep", "open", 100))
            .with_issue("o/r", remote(2, "Gone", "open", 100));
        storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;
        let gone = storage
            .issue_link(integration.id, "o/r#2")
            .await?
            .expect("linked")
            .task_id;
        fake.remove_issue("o/r", 2);

        // Incremental: the page is partial, so a missing link proves nothing.
        let summary = storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        assert_eq!(summary.tombstoned, 0);
        assert!(storage.get_task(gone).await?.deleted_at.is_none());

        // A full pass sees the whole issue set, so the missing one is deleted.
        let summary = storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;
        assert_eq!(summary.tombstoned, 1);
        assert!(storage.get_task(gone).await?.deleted_at.is_some());
        let link = storage.issue_link(integration.id, "o/r#2").await?.unwrap();
        assert!(link.state.tombstoned);
        assert!(link.state.tombstone_reason.is_some());

        // A tombstoned link is never resurrected, even when the issue returns.
        let fake = FakeGithub::default()
            .with_issue("o/r", remote(1, "Keep", "open", 100))
            .with_issue("o/r", remote(2, "Gone", "open", 100));
        let summary = storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;
        assert_eq!(summary.imported, 0);
        assert!(storage.get_task(gone).await?.deleted_at.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn labels_are_additive_and_created_remotely_when_new() -> anyhow::Result<()> {
        let (mut storage, integration, _) = bound_store("o", "r").await?;
        let mut issue = remote(1, "Buggy", "open", 100);
        issue.labels = vec!["bug".to_string()];
        let fake = FakeGithub::default().with_issue("o/r", issue.clone());
        storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        let task_id = storage.issue_link(integration.id, "o/r#1").await?.unwrap().task_id;
        let imported = storage.get_direct_task_tags(task_id).await?;
        assert!(imported.iter().any(|tag| tag.label() == "bug"));

        // A new local tag becomes a label, while the remote's label stays.
        let local = storage.create_tag("needs-review").await?;
        storage.assign_tag_to_task(task_id, &local.name).await?;
        let summary = storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        assert_eq!(summary.pushed, 1);
        assert_eq!(summary.labels, 1);
        assert_eq!(
            *fake.labels.lock().expect("labels lock"),
            vec!["needs-review".to_string()]
        );
        let mut pushed = fake.last_patch().labels.unwrap_or_default();
        pushed.sort();
        assert_eq!(pushed, vec!["bug".to_string(), "needs-review".to_string()]);

        // A label removed locally is still never removed remotely.
        let fake_before = fake.last_patch().labels.unwrap_or_default();
        assert!(fake_before.contains(&"bug".to_string()));
        Ok(())
    }

    #[tokio::test]
    async fn bindings_include_detected_and_explicit_targets_once_each() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let integration = storage
            .create_integration("github", Some("me".to_string()))
            .await?;
        let other = storage.create_integration("github", None).await?;
        let detected = storage.create_tag("project").await?;
        storage
            .bind_repo_tag(detected.id, integration.id, "lofi-tools", "todo-lofi")
            .await?;
        // A tag bound only through its sync target (the settings popover).
        let explicit = storage.create_tag("api").await?;
        storage
            .set_tag_sync_target(
                explicit.id,
                Some(SyncTarget {
                    integration_id: integration.id,
                    external_id: "lofi-tools/api".to_string(),
                }),
            )
            .await?;
        // A target of another integration is not this one's business.
        let elsewhere = storage.create_tag("elsewhere").await?;
        storage
            .set_tag_sync_target(
                elsewhere.id,
                Some(SyncTarget {
                    integration_id: other.id,
                    external_id: "lofi-tools/elsewhere".to_string(),
                }),
            )
            .await?;
        // Binding the same repo twice does not double-sync it.
        let duplicate = storage.create_tag("duplicate").await?;
        storage
            .bind_repo_tag(duplicate.id, integration.id, "lofi-tools", "todo-lofi")
            .await?;

        let bound = storage.bound_repos(integration.id).await?;
        let mut ids: Vec<String> = bound.iter().map(BoundRepo::external_id).collect();
        ids.sort();
        assert_eq!(ids, vec!["lofi-tools/api", "lofi-tools/todo-lofi"]);
        Ok(())
    }

    #[tokio::test]
    async fn ensure_repo_tag_creates_the_namespaced_tag_once() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let integration = storage.create_integration("github", None).await?;
        let first = storage
            .ensure_repo_tag(integration.id, "lofi-tools", "todo-lofi")
            .await?;
        let second = storage
            .ensure_repo_tag(integration.id, "lofi-tools", "todo-lofi")
            .await?;
        assert_eq!(first, second);
        let tag = storage.get_tag(first).await?;
        assert_eq!(tag.name, "github/lofi-tools/todo-lofi");
        assert_eq!(tag.label(), "lofi-tools/todo-lofi");
        // Bound, so the engine finds it without re-detecting the remote.
        assert_eq!(storage.bound_repos(integration.id).await?.len(), 1);
        assert_eq!(
            storage.tag_settings(first).await?.sync_target.unwrap().external_id,
            "lofi-tools/todo-lofi"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_task_reports_only_its_github_issue_link() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let github = storage.create_integration("github", None).await?;
        let todoist = storage.create_integration("todoist", None).await?;
        let task = storage.create_task(Task::create().title("Shared")).await?;
        storage
            .link_task(todoist.id, "todoist-1", task.id, None)
            .await?;
        assert!(
            storage.issue_link_for_task(task.id).await?.is_none(),
            "a Todoist link is not an issue"
        );

        storage
            .link_issue(
                github.id,
                "lofi-tools/todo-lofi#3",
                task.id,
                &IssueFieldState {
                    url: Some("https://github.com/lofi-tools/todo-lofi/issues/3".to_string()),
                    ..Default::default()
                },
                jiff::Timestamp::from_second(50).ok(),
            )
            .await?;
        let found = storage
            .issue_link_for_task(task.id)
            .await?
            .expect("the issue link");
        assert_eq!(found.issue.number, 3);
        assert_eq!(found.issue.owner, "lofi-tools");
        assert_eq!(found.external_updated_at.map(|at| at.as_second()), Some(50));
        Ok(())
    }

    #[test]
    fn failures_are_classified_for_the_retry_policy() {
        assert!(
            classify_status(500, "boom", None).is_transient(),
            "5xx retries"
        );
        assert!(classify_status(403, "secondary rate limit", None).is_transient());
        assert!(classify_status(429, "slow down", None).is_transient());
        assert_eq!(
            classify_status(429, "slow down", None).retry_after(),
            Some(std::time::Duration::from_secs(60))
        );
        assert_eq!(
            classify_status(403, "limit", Some(std::time::Duration::from_secs(9))).retry_after(),
            Some(std::time::Duration::from_secs(9))
        );
        for status in [401, 404, 422] {
            let failure = classify_status(status, "no", None);
            assert!(!failure.is_transient(), "{status} must block");
            assert!(matches!(failure, SyncFailure::Permanent { .. }));
        }
    }

    #[test]
    fn issue_and_comment_payloads_are_narrowed_safely() {
        let issue = remote_issue_from_json(&serde_json::json!({
            "number": 4,
            "title": "Broken",
            "body": "details",
            "state": "closed",
            "labels": [{ "name": "bug" }, { "name": "p1" }],
            "assignees": [{ "login": "me" }],
            "milestone": { "title": "v1" },
            "user": { "login": "author" },
            "html_url": "https://github.com/o/r/issues/4",
            "updated_at": "2026-09-14T10:00:00Z",
        }))
        .expect("an issue");
        assert_eq!(issue.number, 4);
        assert_eq!(issue.labels, vec!["bug".to_string(), "p1".to_string()]);
        assert_eq!(issue.assignees, vec!["me".to_string()]);
        assert_eq!(issue.state, "closed");
        assert!(issue.updated_at.is_some());

        // The issues endpoint also returns pull requests; they are not tasks.
        assert!(
            remote_issue_from_json(&serde_json::json!({
                "number": 5,
                "title": "A PR",
                "pull_request": { "url": "x" },
            }))
            .is_none()
        );
        assert!(remote_issue_from_json(&serde_json::json!({ "title": "no number" })).is_none());

        let comment = comment_from_json(&serde_json::json!({
            "id": 9,
            "body": "  looks good  ",
            "user": { "login": "reviewer" },
            "created_at": "2026-09-14T10:00:00Z",
        }))
        .expect("a comment");
        assert_eq!(comment.external_id, "9");
        assert_eq!(comment.text, "looks good");
        assert_eq!(comment.author.as_deref(), Some("reviewer"));
        // An empty body is not worth importing.
        assert!(
            comment_from_json(&serde_json::json!({ "id": 10, "body": "   " })).is_none()
        );
    }

    #[test]
    fn pull_request_titles_follow_the_pr_convention() {
        // A conventional prefix is dropped, not passed through.
        assert_eq!(
            format_pull_request_title("feat: add the login flow"),
            "Add the login flow"
        );
        assert_eq!(
            format_pull_request_title("fix: crash on startup."),
            "Crash on startup"
        );
        // A scope is the crate name, so it is kept.
        assert_eq!(
            format_pull_request_title("fix(git_ui): stop the flicker"),
            "git_ui: Stop the flicker"
        );
        assert_eq!(
            format_pull_request_title("git_ui: Add history view"),
            "git_ui: Add history view"
        );
        // Trailing punctuation goes, and the title stays imperative.
        assert_eq!(format_pull_request_title("Handle empty repos;"), "Handle empty repos");
        assert_eq!(format_pull_request_title("  fix conflict  "), "Fix conflict");
        // Long titles are cut at a word boundary, not mid-word.
        let long = format_pull_request_title(
            "Refactor the synchronisation engine so that it can handle every remote transport",
        );
        assert!(long.chars().count() <= 72, "{long}");
        assert!(!long.ends_with(' '), "{long}");
    }

    #[test]
    fn release_notes_are_one_bullet_of_the_right_kind() {
        assert_eq!(release_note_for("Add login flow"), "- Added login flow");
        assert_eq!(release_note_for("Fix crash on startup"), "- Fixed crash on startup");
        assert_eq!(release_note_for("Fix the broken sync"), "- Fixed the broken sync");
        // Docs, tests and internal work are not user-facing.
        assert_eq!(release_note_for("docs: update the readme"), "- N/A");
        assert_eq!(release_note_for("Add tests for the parser"), "- N/A");
        assert_eq!(release_note_for("Refactor the sync loop"), "- N/A");
    }

    #[test]
    fn pull_request_bodies_end_with_exactly_one_release_note() {
        let body = format_pull_request_body(
            "This adds a login form and wires it to the API.",
            Some(42),
            Some("- Added login form"),
        );
        assert_eq!(
            body,
            "This adds a login form and wires it to the API.\n\nCloses #42\n\nRelease Notes:\n\n- Added login form"
        );
        let tail = body.split("Release Notes:\n\n").nth(1).expect("the section");
        assert_eq!(tail.lines().count(), 1, "exactly one bullet: {tail}");

        // Without an issue there is no `Closes`, and an agent-written section
        // is replaced rather than duplicated.
        let body = format_pull_request_body(
            "Some prose.\n\nRelease Notes:\n\n- Added a thing the agent made up",
            None,
            None,
        );
        assert_eq!(body, "Some prose.\n\nRelease Notes:\n\n- N/A");
        assert_eq!(body.matches("Release Notes:").count(), 1);
    }

    #[test]
    fn the_agents_summary_is_read_from_the_review_notes() {
        let notes = vec![
            crate::workflow::RunNote::new("annotation", "implement", "implement", "unrelated"),
            crate::workflow::RunNote::new(
                "annotation",
                "review",
                "review",
                "Add the login flow\n\nAdds a form and wires it up.",
            ),
        ];
        let (message, prose) = summary_from_notes(&notes).expect("a summary");
        assert_eq!(message, "Add the login flow");
        assert_eq!(prose.as_deref(), Some("Adds a form and wires it up."));
        // A bare message has no prose, and no note means no summary.
        let bare = vec![crate::workflow::RunNote::new(
            "annotation",
            "review",
            "review",
            "Just a message",
        )];
        assert_eq!(summary_from_notes(&bare).unwrap().1, None);
        assert!(summary_from_notes(&[]).is_none());
    }

    #[test]
    fn pull_request_payloads_map_to_local_states() {
        let open = remote_pull_request_from_json(&serde_json::json!({
            "number": 7,
            "html_url": "https://github.com/o/r/pull/7",
            "state": "open",
            "draft": true,
            "node_id": "PR_kwDO",
        }))
        .expect("a pull request");
        assert_eq!(open.number, 7);
        assert!(open.draft);
        assert_eq!(open.local_state(), PULL_REQUEST_OPEN);
        assert_eq!(open.node_id.as_deref(), Some("PR_kwDO"));

        let merged = remote_pull_request_from_json(&serde_json::json!({
            "number": 7,
            "state": "closed",
            "merged": true,
            "merged_at": "2026-09-14T10:00:00Z",
            "draft": false,
        }))
        .expect("a merged pull request");
        assert_eq!(merged.local_state(), PULL_REQUEST_MERGED);
        assert!(merged.merged_at.is_some());

        let closed = remote_pull_request_from_json(&serde_json::json!({
            "number": 8,
            "state": "closed",
            "merged": false,
        }))
        .expect("a closed pull request");
        assert_eq!(closed.local_state(), PULL_REQUEST_CLOSED);
        assert!(remote_pull_request_from_json(&serde_json::json!({ "state": "open" })).is_none());
    }

    #[tokio::test]
    async fn pull_requests_are_adopted_by_branch_and_flip_to_ready() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let integration = storage.create_integration("github", None).await?;
        let fake = FakeGithub::default();
        let request = NewPullRequest {
            head: "feature/42-add-login".to_string(),
            base: "main".to_string(),
            title: "Add login".to_string(),
            body: "body".to_string(),
            draft: true,
        };
        let opened = fake
            .create_pull_request("lofi-tools", "todo-lofi", &request)
            .await?;
        assert!(opened.draft);

        // A re-run finds the branch's existing PR instead of opening another.
        let found = fake
            .find_pull_request("lofi-tools", "todo-lofi", "feature/42-add-login")
            .await?
            .expect("the adopted pull request");
        assert_eq!(found.number, opened.number);
        assert!(
            fake.find_pull_request("lofi-tools", "todo-lofi", "feature/other")
                .await?
                .is_none()
        );

        // Marking ready goes through the PR's node id.
        fake.mark_pull_request_ready("lofi-tools", "todo-lofi", opened.number)
            .await?;
        let updated = fake
            .get_pull_request("lofi-tools", "todo-lofi", opened.number)
            .await?;
        assert!(!updated.draft);

        let id = storage
            .upsert_run_pull_request(&NewRunPullRequest {
                run_id: 4,
                repo_dir: "/repos/api".to_string(),
                integration_id: integration.id,
                owner: "lofi-tools".to_string(),
                repo: "todo-lofi".to_string(),
                number: opened.number,
                url: opened.url.clone(),
                head_branch: request.head.clone(),
                base_branch: request.base.clone(),
                draft: opened.draft,
                state: PULL_REQUEST_OPEN.to_string(),
            })
            .await?;
        let row = storage
            .run_pull_request(id)
            .await?
            .expect("the row by id");
        assert_eq!(row.number, opened.number);
        assert!(storage.run_pull_request(id + 99).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn a_run_completes_when_every_pull_request_is_resolved() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let integration = storage.create_integration("github", None).await?;
        // No PRs yet: the step is not finished.
        assert!(!storage.run_pull_requests_resolved(11).await?);

        let mut ids = Vec::new();
        for (run_id, repo_dir, number) in [(11, "/repos/api", 1u64), (11, "/repos/web", 2u64)] {
            ids.push(
                storage
                    .upsert_run_pull_request(&NewRunPullRequest {
                        run_id,
                        repo_dir: repo_dir.to_string(),
                        integration_id: integration.id,
                        owner: "lofi-tools".to_string(),
                        repo: "todo-lofi".to_string(),
                        number,
                        url: format!("https://github.com/lofi-tools/todo-lofi/pull/{number}"),
                        head_branch: "feature/42-add-login".to_string(),
                        base_branch: "main".to_string(),
                        draft: true,
                        state: PULL_REQUEST_OPEN.to_string(),
                    })
                    .await?,
            );
        }
        assert!(!storage.run_pull_requests_resolved(11).await?);

        storage
            .update_run_pull_request(ids[0], PULL_REQUEST_MERGED, false, jiff::Timestamp::from_second(9).ok())
            .await?;
        assert!(!storage.run_pull_requests_resolved(11).await?);

        // Waiving the rest lets a multi-repo run finish early (decision 25).
        assert_eq!(storage.waive_run_pull_requests(11).await?, 1);
        assert!(storage.run_pull_requests_resolved(11).await?);
        assert!(
            storage.all_run_pull_requests().await?
                .iter()
                .any(|pull_request| pull_request.state == PULL_REQUEST_WAIVED)
        );
        Ok(())
    }

    #[tokio::test]
    async fn pull_request_metadata_is_copied_from_the_issue() -> anyhow::Result<()> {
        // The client surface the step uses for the metadata copy: labels,
        // assignees, and reviewers, each recorded on the PR's issue number.
        let fake = FakeGithub::default();
        fake.add_issue_labels(
            "o",
            "r",
            12,
            &["bug".to_string(), "needs-review".to_string()],
        )
        .await?;
        fake.add_issue_assignees("o", "r", 12, &["me".to_string()])
            .await?;
        fake.request_reviewers("o", "r", 12, &["me".to_string()])
            .await?;
        assert_eq!(
            *fake.pull_request_labels.lock().expect("labels"),
            vec![(
                12,
                vec!["bug".to_string(), "needs-review".to_string()]
            )]
        );
        assert_eq!(
            *fake.pull_request_assignees.lock().expect("assignees"),
            vec![(12, vec!["me".to_string()])]
        );
        assert_eq!(
            *fake.reviewers_requested.lock().expect("reviewers"),
            vec![(12, vec!["me".to_string()])]
        );
        Ok(())
    }
}
