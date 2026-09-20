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

use crate::{ExternalComment, QueryResult, Tag, Task, TodoStore};
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
    /// When the issue was opened, as epoch seconds, so the details pane can
    /// read `opened 3h ago` the way GitHub's header does. Mirrored for display
    /// only, like `author`: the app does not own it on GitHub.
    #[serde(default)]
    pub created_at: Option<i64>,
    /// When the issue was closed, as epoch seconds. GitHub's header shows the
    /// latest transition (`closed 3h ago`), which is this date when the issue
    /// is closed.
    #[serde(default)]
    pub closed_at: Option<i64>,
    #[serde(default)]
    pub url: Option<String>,
    /// The issue's own database id, recorded whenever a payload carries it so
    /// a linked task can be attached as a sub-issue without another lookup.
    #[serde(default)]
    pub issue_id: Option<u64>,
    /// `owner/repo#number` of the issue this one hangs under on GitHub, so an
    /// attachment is not pushed twice and a remote un-parenting is noticed
    /// (§5.5). GitHub owns the relationship.
    #[serde(default)]
    pub parent_issue: Option<String>,
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
    /// GitHub's own row id, which the sub-issue endpoints address an issue
    /// by. `0` when the payload did not carry one.
    pub id: u64,
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
    /// `owner/repo` the issue lives in, from the payload. A sub-issue may live
    /// in another repo of the same owner, so this is what keeps its number from
    /// being paired with the parent's repo (§5.9).
    pub repository: Option<String>,
    pub updated_at: Option<jiff::Timestamp>,
    /// When the issue was opened, so the details pane can read the panel the
    /// way GitHub's header does (`#3 · nmrshll opened 3h ago`).
    pub created_at: Option<jiff::Timestamp>,
    /// When the issue was closed; `None` while it is open and whenever the
    /// payload omits it.
    pub closed_at: Option<jiff::Timestamp>,
    /// How many sub-issues the issue has, from the listing's summary: the
    /// trigger for reading a parent's children (§5.5).
    pub sub_issue_total: u64,
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
        id: value.get("id").and_then(|id| id.as_u64()).unwrap_or_default(),
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
        created_at: value
            .get("created_at")
            .and_then(|at| at.as_str())
            .and_then(|at| at.parse().ok()),
        closed_at: value
            .get("closed_at")
            .and_then(|at| at.as_str())
            .and_then(|at| at.parse().ok()),
        sub_issue_total: value
            .get("sub_issues_summary")
            .and_then(|summary| summary.get("total"))
            .and_then(|total| total.as_u64())
            .unwrap_or_default(),
        repository: value
            .get("repository_url")
            .and_then(|url| url.as_str())
            .and_then(repository_from_api_url),
    })
}

/// The `owner/repo` at the end of a repository API URL, for the sub-issue
/// check above.
fn repository_from_api_url(url: &str) -> Option<String> {
    let mut parts = url.trim_end_matches('/').rsplit('/');
    let repo = parts.next()?;
    let owner = parts.next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
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

    /// Open a new issue. Returns the remote row, which the caller records as
    /// the task's snapshot so the app's own creation never reads back as a
    /// remote edit (§5.4).
    fn create_issue<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        title: &'a str,
        body: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<RemoteIssue>> + Send + 'a;

    /// One issue by number, for the fields a link made before ids were stored
    /// never recorded.
    fn get_issue<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
    ) -> impl std::future::Future<Output = anyhow::Result<RemoteIssue>> + Send + 'a;

    /// Hang `sub_issue_id` under the issue `number`. `replace_parent` moves a
    /// sub-issue GitHub still has under another parent, which is what mirroring
    /// the local tree wants (§5.5).
    fn add_sub_issue<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
        sub_issue_id: u64,
        replace_parent: bool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a;

    /// The sub-issues of one issue, in the parent's order (§5.5). One page of
    /// 100 covers GitHub's own limit, so this is the complete set.
    fn list_sub_issues<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<RemoteIssue>>> + Send + 'a;

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
/// How long a request may take before it fails as transient. The default
/// (no timeout) lets a half-open connection stall for as long as the OS
/// retransmits, and callers here can be holding the app's store lock.
const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// How long to wait for the connection itself, so an unreachable host fails
/// fast instead of hanging a sync pass.
const HTTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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
        // A stalled request must fail (and be retried) rather than hang: the
        // sync path holds the store lock only around database work, but a
        // request that never returns still stalls the pass it belongs to.
        let agent = reqwest::Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            token: token.into(),
            base: base.into().trim_end_matches('/').to_string(),
            agent,
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

    fn create_issue<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        title: &'a str,
        body: &'a str,
    ) -> impl std::future::Future<Output = anyhow::Result<RemoteIssue>> + Send + 'a {
        async move {
            let path = format!("/repos/{owner}/{repo}/issues");
            let response = self
                .send(
                    self.request(reqwest::Method::POST, &path)
                        .json(&serde_json::json!({ "title": title, "body": body })),
                )
                .await?;
            let created = self.json(response, "issue").await?;
            remote_issue_from_json(&created).ok_or_else(|| {
                anyhow::anyhow!("GitHub did not describe the issue it created: {created}")
            })
        }
    }

    fn get_issue<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
    ) -> impl std::future::Future<Output = anyhow::Result<RemoteIssue>> + Send + 'a {
        async move {
            let path = format!("/repos/{owner}/{repo}/issues/{number}");
            let response = self.send(self.request(reqwest::Method::GET, &path)).await?;
            let body = self.json(response, "issue").await?;
            remote_issue_from_json(&body).ok_or_else(|| {
                anyhow::anyhow!("GitHub did not describe issue #{number}: {body}")
            })
        }
    }

    fn add_sub_issue<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
        sub_issue_id: u64,
        replace_parent: bool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
        async move {
            let path = format!("/repos/{owner}/{repo}/issues/{number}/sub_issues");
            self.post_json(
                &path,
                serde_json::json!({
                    "sub_issue_id": sub_issue_id,
                    "replace_parent": replace_parent,
                }),
            )
            .await
        }
    }

    fn list_sub_issues<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        number: u64,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<RemoteIssue>>> + Send + 'a {
        async move {
            let path = format!("/repos/{owner}/{repo}/issues/{number}/sub_issues?per_page=100");
            let response = self.send(self.request(reqwest::Method::GET, &path)).await?;
            let body = self.json(response, "sub-issue list").await?;
            Ok(body
                .as_array()
                .map(|items| items.iter().filter_map(remote_issue_from_json).collect())
                .unwrap_or_default())
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

/// Asked before each unit of sync work. `true` stops the pass at the next safe
/// boundary: every issue applied so far stays committed (each one records its
/// own snapshot), the repo's cursor is left unrecorded, and the next pass
/// resumes the same window. Used by callers that hold a lock a user action may
/// be queued behind, so that lock is never held for a whole pass.
pub type SyncYield<'a> = &'a (dyn Fn() -> bool + Send + Sync);

/// A pass that never stops early, for callers with no one to defer to.
fn never_yield() -> bool {
    false
}

/// What one pass did, and whether it stopped early because it was asked to.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GithubSyncOutcome {
    pub summary: GithubSyncSummary,
    /// The pass yielded: the rest of the window is still to sync, so the
    /// caller should not treat the pass as failed and should come back.
    pub aborted: bool,
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
    /// Sub-issue relationships newly mirrored onto the local tree.
    pub nested: usize,
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
        self.nested += other.nested;
    }

    pub fn is_empty(&self) -> bool {
        self.imported == 0
            && self.updated == 0
            && self.pushed == 0
            && self.tombstoned == 0
            && self.nested == 0
    }

    /// One line for the card's status row.
    pub fn describe(&self) -> String {
        format!(
            "{} repo(s): {} imported, {} updated, {} pushed, {} comments, {} sub-issues, {} removed.",
            self.repos,
            self.imported,
            self.updated,
            self.pushed,
            self.comments,
            self.nested,
            self.tombstoned
        )
    }
}

fn non_empty(text: &str) -> Option<String> {
    (!text.trim().is_empty()).then(|| text.to_string())
}

fn completed_at_for(issue: &RemoteIssue) -> Option<u64> {
    (issue.state == "closed").then(|| jiff::Timestamp::now().as_second() as u64)
}

/// The link state a freshly seen issue starts from: its mapped values as the
/// snapshot, the read-only metadata, and its own identity for the sub-issue
/// endpoints.
fn issue_state_for(issue: &RemoteIssue) -> IssueFieldState {
    IssueFieldState {
        remote: issue.field_values(),
        assignees: issue.assignees.clone(),
        milestone: issue.milestone.clone(),
        author: issue.author.clone(),
        created_at: issue.created_at.map(|at| at.as_second()),
        closed_at: issue.closed_at.map(|at| at.as_second()),
        url: issue.url.clone(),
        issue_id: (issue.id != 0).then_some(issue.id),
        ..Default::default()
    }
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

    /// Unbind one repo from a tag: the link row goes, and when the tag's sync
    /// target pointed at it, the target moves to another of the tag's repos
    /// or clears. Synced tasks keep their local copies; they just stop
    /// syncing. A repo the directory still resolves to is re-bound by the
    /// next detection pass.
    pub async fn unbind_repo_tag(
        &mut self,
        tag_id: u64,
        integration_id: u64,
        repo: &str,
    ) -> QueryResult<()> {
        self.unlink_tag(integration_id, repo).await?;
        let pointed_here = self.tag_settings(tag_id).await?.sync_target.is_some_and(
            |target| target.integration_id == integration_id && target.external_id == repo,
        );
        if !pointed_here {
            return Ok(());
        }
        let fallback = self
            .bound_repos(integration_id)
            .await?
            .into_iter()
            .filter(|bound| bound.tag_id == tag_id)
            .map(|bound| bound.external_id())
            .find(|external_id| external_id != repo)
            .map(|external_id| crate::SyncTarget {
                integration_id,
                external_id,
            });
        self.set_tag_sync_target(tag_id, fallback).await
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

    /// The repo a task's project tag is bound to, so a task added locally can
    /// be opened as an issue there. Resolves through a section's parent the
    /// way the Todoist destination lookup does (a task in a section of a
    /// bound project still belongs to that project). `None` leaves the task
    /// purely local.
    async fn bound_repo_for_task(&mut self, task_id: u64) -> QueryResult<Option<BoundRepo>> {
        let integration_ids: Vec<u64> = self
            .list_integrations()
            .await?
            .into_iter()
            .filter(|integration| integration.provider == "github")
            .map(|integration| integration.id)
            .collect();
        if integration_ids.is_empty() {
            return Ok(None);
        }
        for tag in self.get_direct_task_tags(task_id).await? {
            let mut chain = vec![tag.id];
            chain.extend(self.get_parents(tag.id).await?.into_iter().map(|tag| tag.id));
            for tag_id in chain {
                // The tag's own binding first: detection persists it there
                // (§5.3), so it is the common case and needs no link scan.
                if let Some(target) = self.tag_settings(tag_id).await?.sync_target
                    && integration_ids.contains(&target.integration_id)
                    && let Some((owner, repo)) = target.external_id.split_once('/')
                {
                    return Ok(Some(BoundRepo {
                        integration_id: target.integration_id,
                        owner: owner.to_string(),
                        repo: repo.to_string(),
                        tag_id,
                    }));
                }
                for integration_id in &integration_ids {
                    let link = self
                        .tag_links_for_integration(*integration_id)
                        .await?
                        .into_iter()
                        .find(|link| link.tag_id == tag_id && link.source_kind == "repo");
                    if let Some(link) = link
                        && let Some((owner, repo)) = link.external_id.split_once('/')
                    {
                        return Ok(Some(BoundRepo {
                            integration_id: *integration_id,
                            owner: owner.to_string(),
                            repo: repo.to_string(),
                            tag_id,
                        }));
                    }
                }
            }
        }
        Ok(None)
    }

    /// The issue of the nearest ancestor that has one, with the integration
    /// that owns it. A deleted ancestor is stepped over, and a tombstoned link
    /// names an issue that is gone or inaccessible, so neither can host a
    /// sub-issue. Walks with a visited set, so a corrupted parent chain cannot
    /// loop.
    async fn nearest_ancestor_issue(
        &mut self,
        task_id: u64,
    ) -> QueryResult<Option<(u64, IssueRef)>> {
        let mut current = Some(task_id);
        let mut visited = std::collections::HashSet::new();
        while let Some(id) = current {
            if !visited.insert(id) {
                return Ok(None);
            }
            let task = self.get_task(id).await?;
            if task.deleted_at.is_none()
                && let Some(link) = self.issue_link_for_task(id).await?
                && !link.state.tombstoned
            {
                return Ok(Some((link.integration_id, link.issue)));
            }
            current = task.parent_id;
        }
        Ok(None)
    }

    /// Whether `task_id`'s ancestry says it has to stay local: it is a workflow
    /// step, or sits under one (a step is run scaffolding, not a subtask), or
    /// the chain loops, which would otherwise walk forever. Walks up with a
    /// visited set.
    ///
    /// The test is the explicit `role` (decision #4), not "a run is involved":
    /// a run root and a promoted subtask both carry a run of their own and keep
    /// syncing their own issue.
    async fn ancestry_blocks_sync(&mut self, task_id: u64) -> QueryResult<bool> {
        let mut current = Some(task_id);
        let mut visited = std::collections::HashSet::new();
        while let Some(id) = current {
            if !visited.insert(id) {
                return Ok(true);
            }
            let task = self.get_task(id).await?;
            if crate::workflow::is_step(&task) {
                return Ok(true);
            }
            current = task.parent_id;
        }
        Ok(false)
    }

    /// The issue's own database id, which the sub-issue endpoints address a
    /// child by. Recorded at every write; a link made before ids were stored
    /// pays one lookup and keeps the answer.
    async fn issue_database_id<C: GithubClient>(
        &mut self,
        client: &C,
        link: &TaskIssue,
    ) -> anyhow::Result<Option<u64>> {
        if let Some(id) = link.state.issue_id {
            return Ok(Some(id));
        }
        let issue = client
            .get_issue(&link.issue.owner, &link.issue.repo, link.issue.number)
            .await?;
        if issue.id == 0 {
            return Ok(None);
        }
        let mut state = link.state.clone();
        state.issue_id = Some(issue.id);
        let external_id = issue_external_id(&link.issue.owner, &link.issue.repo, link.issue.number);
        self.save_issue_field_state(link.integration_id, &external_id, &state)
            .await?;
        Ok(Some(issue.id))
    }

    /// Hang a task's issue under its parent's issue, once (§5.5). The recorded
    /// relationship short-circuits a repeat; otherwise `replace_parent` moves
    /// an issue GitHub still has under an older parent. Nothing happens for a
    /// child whose issue lives in another repo, because the link's external id
    /// names the child inside its own repo.
    async fn attach_sub_issue<C: GithubClient>(
        &mut self,
        client: &C,
        parent: &IssueRef,
        child_task_id: u64,
    ) -> anyhow::Result<()> {
        let Some(link) = self.issue_link_for_task(child_task_id).await? else {
            return Ok(());
        };
        if link.issue.owner != parent.owner || link.issue.repo != parent.repo {
            return Ok(());
        }
        let parent_external_id = issue_external_id(&parent.owner, &parent.repo, parent.number);
        if link.state.parent_issue.as_deref() == Some(parent_external_id.as_str()) {
            return Ok(());
        }
        let Some(sub_issue_id) = self.issue_database_id(client, &link).await? else {
            return Ok(());
        };
        client
            .add_sub_issue(
                &parent.owner,
                &parent.repo,
                parent.number,
                sub_issue_id,
                true,
            )
            .await?;
        let external_id = issue_external_id(&link.issue.owner, &link.issue.repo, link.issue.number);
        let mut state = link.state.clone();
        // The lookup may have just recorded the id, so it is written back from
        // here rather than relying on the stale clone.
        state.issue_id = Some(sub_issue_id);
        state.parent_issue = Some(parent_external_id);
        self.save_issue_field_state(link.integration_id, &external_id, &state)
            .await?;
        Ok(())
    }

    /// The issue for one task, opening it when the task has none. Its subtasks
    /// are not walked here; `push_github_new_task` owns that, so callers of the
    /// chain below never recurse into a sibling's tree.
    async fn ensure_github_issue<C: GithubClient>(
        &mut self,
        client: &C,
        task_id: u64,
    ) -> anyhow::Result<Option<(u64, IssueRef)>> {
        let task = self.get_task(task_id).await?;
        if task.deleted_at.is_some()
            || crate::workflow::is_step(&task)
            || self.is_builtin_owned(task_id).await?
            || self.ancestry_blocks_sync(task_id).await?
        {
            return Ok(None);
        }
        // A subtask hangs under the nearest ancestor that is on GitHub, so a
        // chain of local-only tasks in between is no obstacle. When nothing
        // above it has an issue yet, the parent's own chain is opened first
        // and the child hangs under the issue that appears.
        let parent = match task.parent_id {
            Some(parent_id) => match self.nearest_ancestor_issue(parent_id).await? {
                Some(found) => Some(found),
                // Boxed, because this is the recursion the future's size would
                // otherwise be defined in terms of.
                None => Box::pin(self.ensure_github_issue(client, parent_id)).await?,
            },
            None => None,
        };
        if let Some(link) = self.issue_link_for_task(task_id).await? {
            if let Some((_, parent_issue)) = &parent {
                self.attach_sub_issue(client, parent_issue, task_id).await?;
            }
            return Ok(Some((link.integration_id, link.issue)));
        }
        // A sub-issue lives in its parent's repo; a top-level task goes where
        // its own project tag is bound.
        let destination = match &parent {
            Some((integration_id, issue)) => {
                Some((*integration_id, issue.owner.clone(), issue.repo.clone()))
            }
            None => self
                .bound_repo_for_task(task_id)
                .await?
                .map(|bound| (bound.integration_id, bound.owner, bound.repo)),
        };
        let Some((integration_id, owner, repo)) = destination else {
            return Ok(None);
        };
        let issue = client
            .create_issue(
                &owner,
                &repo,
                &task.title,
                task.description.as_deref().unwrap_or_default(),
            )
            .await?;
        let external_id = issue_external_id(&owner, &repo, issue.number);
        let state = issue_state_for(&issue);
        self.link_issue(integration_id, &external_id, task_id, &state, issue.updated_at)
            .await?;
        if let Some((_, parent_issue)) = &parent {
            self.attach_sub_issue(client, parent_issue, task_id).await?;
        }
        // Reflect the task's local tags (a project's subtags included) as
        // labels straight away, so a captured task does not wait for the next
        // sync's merge (§5.5). The issue and its link are already recorded, so
        // a label failure is logged and left to the next merge rather than
        // stranding the capture.
        if let Err(error) = self
            .push_local_issue_labels(
                client,
                integration_id,
                &external_id,
                task_id,
                &owner,
                &repo,
                issue.number,
            )
            .await
        {
            tracing::warn!(
                task_id,
                %error,
                "could not copy local tags onto the new issue"
            );
        }
        Ok(Some((
            integration_id,
            IssueRef {
                owner,
                repo,
                number: issue.number,
            },
        )))
    }

    /// Add the task's local tags to an issue it is linked to, and fold them
    /// into the link's snapshot so the next pull does not read the app's own
    /// push as a remote edit. Best-effort by design: a failure is logged by
    /// the caller and retried by the next merge, because the issue already
    /// exists by the time this runs.
    async fn push_local_issue_labels<C: GithubClient>(
        &mut self,
        client: &C,
        integration_id: u64,
        external_id: &str,
        task_id: u64,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> anyhow::Result<()> {
        let labels = self.local_issue_labels(task_id, None).await?;
        if labels.is_empty() {
            return Ok(());
        }
        for label in &labels {
            client.ensure_label(owner, repo, label).await?;
        }
        client
            .add_issue_labels(owner, repo, number, &labels)
            .await?;
        let Some(link) = self.issue_link(integration_id, external_id).await? else {
            return Ok(());
        };
        let mut state = link.state;
        state.remote.labels = labels;
        self.save_issue_field_state(integration_id, external_id, &state)
            .await?;
        Ok(())
    }

    /// Open the issue for a task captured by a repo-bound project tag, so a
    /// task typed into that project also exists on GitHub (§5.2), and mirror
    /// the task's subtask tree under it (§5.5). The link is recorded with the
    /// opened issue as its snapshot, so the first pull after this merges
    /// instead of importing a duplicate. Returns the issue the task ended up
    /// with, and `None` only when the task must never sync or sits in no
    /// repo-bound project.
    pub async fn push_github_new_task<C: GithubClient>(
        &mut self,
        client: &C,
        task_id: u64,
    ) -> anyhow::Result<Option<IssueRef>> {
        let mut seen = std::collections::HashSet::new();
        self.mirror_github_task(client, task_id, &mut seen).await
    }

    /// The issue for one task, plus the same for its subtask tree: a parent
    /// that gains an issue later still picks up the children it already has.
    /// Each task is visited once, so a corrupted parent chain cannot recurse
    /// forever, and a subtask that fails is reported without stopping its
    /// siblings.
    async fn mirror_github_task<C: GithubClient>(
        &mut self,
        client: &C,
        task_id: u64,
        seen: &mut std::collections::HashSet<u64>,
    ) -> anyhow::Result<Option<IssueRef>> {
        if !seen.insert(task_id) {
            return Ok(None);
        }
        let issue = self.ensure_github_issue(client, task_id).await?;
        let mut failed = None;
        for subtask in self.list_subtasks(task_id).await? {
            // Boxed, because this is the recursion the future's size would
            // otherwise be defined in terms of.
            if let Err(error) = Box::pin(self.mirror_github_task(client, subtask.id, seen)).await
                && failed.is_none()
            {
                failed = Some(error);
            }
        }
        match failed {
            Some(error) => Err(error),
            None => Ok(issue.map(|(_, issue)| issue)),
        }
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
        Ok(self
            .sync_github_integration_yielding(client, integration_id, full, &never_yield)
            .await?
            .summary)
    }

    /// The same pass, stopping at a safe boundary when `should_yield` asks.
    pub async fn sync_github_integration_yielding<C: GithubClient>(
        &mut self,
        client: &C,
        integration_id: u64,
        full: bool,
        should_yield: SyncYield<'_>,
    ) -> anyhow::Result<GithubSyncOutcome> {
        let repos = self.bound_repos(integration_id).await?;
        let mut summary = GithubSyncSummary::default();
        let mut aborted = false;
        for bound in &repos {
            let mut repo_summary = GithubSyncSummary::default();
            let completed = self
                .sync_github_repo(client, bound, full, &mut repo_summary, should_yield)
                .await
                .map_err(|e| match e.downcast::<SyncFailure>() {
                    Ok(failure) => anyhow::Error::new(failure),
                    Err(other) => anyhow::anyhow!(
                        "GitHub sync failed for {}: {other}",
                        bound.external_id()
                    ),
                })?;
            summary.absorb(&repo_summary);
            if !completed {
                aborted = true;
                break;
            }
            summary.repos += 1;
        }
        Ok(GithubSyncOutcome { summary, aborted })
    }

    /// Sync one repo. Returns whether it ran to the end: `false` means the
    /// caller asked for the lock back and this pass stopped between units of
    /// work, with the cursor left for the next pass to resume from.
    async fn sync_github_repo<C: GithubClient>(
        &mut self,
        client: &C,
        bound: &BoundRepo,
        full: bool,
        summary: &mut GithubSyncSummary,
        should_yield: SyncYield<'_>,
    ) -> anyhow::Result<bool> {
        if should_yield() {
            return Ok(false);
        }
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
        // Issues that have sub-issues of their own: their children are read in
        // a second pass, one call per parent rather than one per issue (§5.5).
        let mut parents: Vec<(String, u64)> = Vec::new();
        for issue in &page.issues {
            let external_id = issue_external_id(&bound.owner, &bound.repo, issue.number);
            seen.insert(external_id.clone());
            if issue.sub_issue_total > 0 {
                parents.push((external_id, issue.number));
            }
        }
        for issue in page.issues {
            // Between issues is a safe stopping point: an applied issue has
            // already recorded its own snapshot, so the next pass re-reads the
            // page, finds nothing to do for it, and carries on from here.
            if should_yield() {
                return Ok(false);
            }
            self.sync_github_issue(client, bound, &issue, summary).await?;
        }

        // Parents we recorded children under are read too: a parent whose last
        // sub-issue was removed on GitHub still has to notice.
        for link in self.issue_links_for_integration(bound.integration_id).await? {
            if link.state.tombstoned {
                continue;
            }
            let Some(parent_external_id) = link.state.parent_issue else {
                continue;
            };
            if parents.iter().any(|(known, _)| known == &parent_external_id) {
                continue;
            }
            if let Some(parent) = parse_issue_external_id(&parent_external_id)
                && parent.owner == bound.owner
                && parent.repo == bound.repo
            {
                parents.push((parent_external_id, parent.number));
            }
        }
        for (parent_external_id, parent_number) in parents {
            if should_yield() {
                return Ok(false);
            }
            self.sync_github_sub_issues(
                client,
                bound,
                &parent_external_id,
                parent_number,
                &seen,
                summary,
            )
            .await?;
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
        Ok(true)
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
        // user deleted locally. A step — or anything under one — never syncs
        // at all: steps are run scaffolding the issue tracker has no place for
        // (§5.5). A promoted subtask keeps syncing its own issue.
        if link.state.tombstoned {
            return Ok(());
        }
        let task = self.get_task(link.task_id).await?;
        if task.deleted_at.is_some()
            || crate::workflow::is_step(&task)
            || self.ancestry_blocks_sync(link.task_id).await?
        {
            return Ok(());
        }
        self.merge_github_issue(client, bound, &external_id, &link, issue, summary)
            .await
    }

    /// Mirror one parent's sub-issues locally. GitHub owns the relationship:
    /// a child that moved parents is re-parented, and one the parent no longer
    /// lists is un-nested. Children of this repo sync like any other issue; a
    /// child whose issue the page never carried is only re-nested when its own
    /// link already exists, because a sub-issue may live in another repo of
    /// the same owner and this repo's number would name a different issue.
    async fn sync_github_sub_issues<C: GithubClient>(
        &mut self,
        client: &C,
        bound: &BoundRepo,
        parent_external_id: &str,
        parent_number: u64,
        in_page: &std::collections::HashSet<String>,
        summary: &mut GithubSyncSummary,
    ) -> anyhow::Result<()> {
        let Some(parent) = self.issue_link(bound.integration_id, parent_external_id).await? else {
            return Ok(());
        };
        if parent.state.tombstoned {
            return Ok(());
        }
        let children = client
            .list_sub_issues(&bound.owner, &bound.repo, parent_number)
            .await?;
        let mut listed = std::collections::HashSet::new();
        for child in &children {
            // A sub-issue can live in another repo of the same owner, where its
            // number is not this repo's: pairing the two would name a
            // different issue, so such a child is left alone (§9).
            if child
                .repository
                .as_deref()
                .is_some_and(|slug| !slug.eq_ignore_ascii_case(&bound.external_id()))
            {
                continue;
            }
            let child_external_id = issue_external_id(&bound.owner, &bound.repo, child.number);
            listed.insert(child_external_id.clone());
            if in_page.contains(&child_external_id) {
                self.sync_github_issue(client, bound, child, summary).await?;
            } else if self
                .issue_link(bound.integration_id, &child_external_id)
                .await?
                .is_none()
            {
                continue;
            }
            if self
                .nest_issue(
                    bound.integration_id,
                    &child_external_id,
                    parent.task_id,
                    parent_external_id,
                )
                .await?
            {
                summary.nested += 1;
            }
        }
        self.detach_unlisted_sub_issues(
            bound.integration_id,
            parent_external_id,
            parent.task_id,
            &listed,
        )
        .await?;
        Ok(())
    }

    /// Put a linked child task under the task of its parent issue, and record
    /// the relationship so a later push does not repeat it. A cycle is refused
    /// rather than allowed to corrupt the local tree. Returns whether anything
    /// changed.
    async fn nest_issue(
        &mut self,
        integration_id: u64,
        child_external_id: &str,
        parent_task_id: u64,
        parent_external_id: &str,
    ) -> QueryResult<bool> {
        let Some(link) = self.issue_link(integration_id, child_external_id).await? else {
            return Ok(false);
        };
        if link.state.tombstoned {
            return Ok(false);
        }
        let child = self.get_task(link.task_id).await?;
        if child.deleted_at.is_some() {
            return Ok(false);
        }
        if child.parent_id == Some(parent_task_id)
            && link.state.parent_issue.as_deref() == Some(parent_external_id)
        {
            return Ok(false);
        }
        let mut current = Some(parent_task_id);
        let mut visited = std::collections::HashSet::new();
        while let Some(id) = current {
            if id == child.id {
                return Ok(false);
            }
            if !visited.insert(id) {
                break;
            }
            current = self.get_task(id).await?.parent_id;
        }
        Task::update_by_id(child.id)
            .parent_id(Some(parent_task_id))
            .exec(&mut self.db)
            .await
            .context(crate::error::UpdateTaskSnafu { id: child.id })?;
        let mut state = link.state.clone();
        state.parent_issue = Some(parent_external_id.to_string());
        self.save_issue_field_state(integration_id, child_external_id, &state)
            .await?;
        Ok(true)
    }

    /// A child the parent no longer lists was un-parented on GitHub: undo the
    /// local nesting without touching the issue itself.
    async fn detach_unlisted_sub_issues(
        &mut self,
        integration_id: u64,
        parent_external_id: &str,
        parent_task_id: u64,
        listed: &std::collections::HashSet<String>,
    ) -> QueryResult<()> {
        for link in self.issue_links_for_integration(integration_id).await? {
            if link.state.tombstoned
                || listed.contains(&link.external_id)
                || link.state.parent_issue.as_deref() != Some(parent_external_id)
            {
                continue;
            }
            let task = self.get_task(link.task_id).await?;
            if task.deleted_at.is_some() || task.parent_id != Some(parent_task_id) {
                continue;
            }
            Task::update_by_id(link.task_id)
                .parent_id(None)
                .exec(&mut self.db)
                .await
                .context(crate::error::UpdateTaskSnafu { id: link.task_id })?;
            let mut state = link.state.clone();
            state.parent_issue = None;
            self.save_issue_field_state(integration_id, &link.external_id, &state)
                .await?;
        }
        Ok(())
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
        let state = issue_state_for(issue);
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
        let local_labels = self
            .local_issue_labels(link.task_id, Some(bound.tag_id))
            .await?;

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
        // A payload that omits the date (a partial patch response) leaves the
        // known one in place rather than blanking the row.
        if let Some(at) = issue.created_at {
            state.created_at = Some(at.as_second());
        }
        // A reopened issue has no close date, so a `null` here clears the old
        // one: the row must not keep saying `closed …` for an open issue.
        state.closed_at = issue.closed_at.map(|at| at.as_second());
        state.url = issue.url.clone();
        // The relationship is GitHub's, so it is never touched here; only the
        // id is refreshed from the payload.
        if issue.id != 0 {
            state.issue_id = Some(issue.id);
        }
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
    /// repo binding itself and other project tags. `exclude_tag_id` is the
    /// bound repo tag when the caller has it; tags with a sync target are left
    /// out either way.
    async fn local_issue_labels(
        &mut self,
        task_id: u64,
        exclude_tag_id: Option<u64>,
    ) -> QueryResult<Vec<String>> {
        let mut out = Vec::new();
        for tag in self.get_direct_task_tags(task_id).await? {
            if Some(tag.id) == exclude_tag_id {
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

    /// Get-or-create a local tag for a remote label, placed under the repo's
    /// project tag so an imported label reads as a subtag of the project it
    /// came from. The remote label is the display name, and a tag the user
    /// already made with that name is reused rather than duplicated (§5.2).
    async fn assign_issue_labels(
        &mut self,
        task_id: u64,
        bound: &BoundRepo,
        labels: &[String],
    ) -> QueryResult<()> {
        for label in labels {
            let tag = self.github_label_tag(bound, label).await?;
            if tag.id == bound.tag_id {
                continue;
            }
            self.assign_tag_to_task(task_id, &tag.name).await?;
        }
        Ok(())
    }

    /// The local tag standing in for one remote label: the user's own tag when
    /// one is named after the label, otherwise a per-repo
    /// `github/<owner>/<repo>/<label>` tag. Scoping the name to the
    /// repo keeps two projects from sharing one label tag — which, now that
    /// labels hang under their project, would also leak each project's tasks
    /// into the other's aggregated view.
    async fn github_label_tag(&mut self, bound: &BoundRepo, label: &str) -> QueryResult<Tag> {
        let tag = match self.get_tag_by_name(label).await? {
            Some(tag) => tag,
            None => {
                let scoped = format!("github/{}/{}/{}", bound.owner, bound.repo, label);
                match self.get_tag_by_name(&scoped).await? {
                    Some(tag) => tag,
                    None => {
                        self.create_tag_with_display_name(scoped, Some(label.to_string()))
                            .await?
                    }
                }
            }
        };
        self.place_label_tag(tag.id, bound.tag_id).await?;
        Ok(tag)
    }

    /// Nest an imported label under its project tag. Idempotent, and
    /// non-fatal when the label already sits above the project (that placement
    /// would cycle): the label still syncs as a direct tag, it just cannot be
    /// nested here, which is logged rather than allowed to fail the pass.
    async fn place_label_tag(&mut self, tag_id: u64, project_tag_id: u64) -> QueryResult<()> {
        if tag_id == project_tag_id {
            return Ok(());
        }
        match self.add_tag_implication(tag_id, project_tag_id).await {
            Ok(()) => Ok(()),
            Err(crate::QueryErr::UnexpectedValue { message }) => {
                tracing::warn!(
                    tag_id,
                    project_tag_id,
                    %message,
                    "could not nest a GitHub label under its project"
                );
                Ok(())
            }
            Err(error) => Err(error),
        }
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
            id: number,
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
            ..Default::default()
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
        /// `(repo, title, body)` per issue opened through the API, so a test
        /// can assert that a capture opened exactly one.
        created_issues: std::sync::Mutex<Vec<(String, String, String)>>,
        /// `(parent repo, parent number, child repo, child id)` per accepted
        /// sub-issue call.
        sub_issues: std::sync::Mutex<Vec<(String, u64, String, u64)>>,
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

        /// Record a sub-issue relationship both ways GitHub shows it: the
        /// relationship itself, and the parent's own summary count (the sync's
        /// trigger for reading a parent's children).
        fn with_sub_issue(self, repo: &str, parent: u64, child: u64) -> Self {
            self.with_sub_issue_from(repo, parent, repo, child)
        }

        /// The same, for a sub-issue that lives in another repo of the same
        /// owner: GitHub allows it, and its number means nothing in the
        /// parent's repo.
        fn with_sub_issue_from(self, repo: &str, parent: u64, child_repo: &str, child: u64) -> Self {
            if let Some(issues) = self.issues.lock().expect("issues lock").get_mut(repo)
                && let Some(issue) = issues.iter_mut().find(|issue| issue.number == parent)
            {
                issue.sub_issue_total += 1;
            }
            self.sub_issues.lock().expect("sub-issues lock").push((
                repo.to_string(),
                parent,
                child_repo.to_string(),
                child,
            ));
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

        /// Un-parent a sub-issue the way the GitHub UI does: the relationship
        /// goes and the parent's summary drops with it.
        fn remove_sub_issue(&self, repo: &str, parent: u64, child: u64) {
            self.sub_issues
                .lock()
                .expect("sub-issues lock")
                .retain(|(slug, number, _, sub_issue)| {
                    !(slug == repo && *number == parent && *sub_issue == child)
                });
            if let Some(issues) = self.issues.lock().expect("issues lock").get_mut(repo)
                && let Some(issue) = issues.iter_mut().find(|issue| issue.number == parent)
            {
                issue.sub_issue_total = issue.sub_issue_total.saturating_sub(1);
            }
        }

        fn push_count(&self) -> usize {
            self.updates.lock().expect("updates lock").len()
        }

        fn created_issues(&self) -> Vec<(String, String, String)> {
            self.created_issues
                .lock()
                .expect("created issues lock")
                .clone()
        }

        fn sub_issue_calls(&self) -> Vec<(String, u64, u64)> {
            self.sub_issues
                .lock()
                .expect("sub-issues lock")
                .iter()
                .map(|(repo, parent, _, child)| (repo.clone(), *parent, *child))
                .collect()
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

        fn create_issue<'a>(
            &'a self,
            owner: &'a str,
            repo: &'a str,
            title: &'a str,
            body: &'a str,
        ) -> impl std::future::Future<Output = anyhow::Result<RemoteIssue>> + Send + 'a {
            async move {
                let key = format!("{owner}/{repo}");
                self.created_issues
                    .lock()
                    .expect("created issues lock")
                    .push((key.clone(), title.to_string(), body.to_string()));
                let mut issues = self.issues.lock().expect("issues lock");
                let list = issues.entry(key).or_default();
                // Continue the repo's numbering, so a test that seeded issues
                // by hand cannot have one overwritten.
                let number = list.iter().map(|issue| issue.number).max().unwrap_or(0) + 1;
                let created = RemoteIssue {
                    // The fake numbers its issues per repo, so an id equal to
                    // the number is enough for the sub-issue calls.
                    id: number,
                    number,
                    title: title.to_string(),
                    body: body.to_string(),
                    state: "open".to_string(),
                    updated_at: jiff::Timestamp::from_second(2_000_000_000).ok(),
                    ..Default::default()
                };
                list.push(created.clone());
                Ok(created)
            }
        }

        fn get_issue<'a>(
            &'a self,
            owner: &'a str,
            repo: &'a str,
            number: u64,
        ) -> impl std::future::Future<Output = anyhow::Result<RemoteIssue>> + Send + 'a {
            async move {
                self.issues
                    .lock()
                    .expect("issues lock")
                    .get(&format!("{owner}/{repo}"))
                    .and_then(|issues| issues.iter().find(|issue| issue.number == number))
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("the fake has no issue {number}"))
            }
        }

        fn add_sub_issue<'a>(
            &'a self,
            owner: &'a str,
            repo: &'a str,
            number: u64,
            sub_issue_id: u64,
            replace_parent: bool,
        ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
            async move {
                let key = format!("{owner}/{repo}");
                let exists = self
                    .issues
                    .lock()
                    .expect("issues lock")
                    .get(&key)
                    .is_some_and(|issues| issues.iter().any(|issue| issue.number == number));
                if !exists {
                    anyhow::bail!("the fake has no issue {number} in {key}");
                }
                let mut sub_issues = self.sub_issues.lock().expect("sub-issues lock");
                if replace_parent {
                    sub_issues.retain(|(_, _, _, child)| *child != sub_issue_id);
                }
                sub_issues.push((key.clone(), number, key, sub_issue_id));
                Ok(())
            }
        }

        fn list_sub_issues<'a>(
            &'a self,
            owner: &'a str,
            repo: &'a str,
            number: u64,
        ) -> impl std::future::Future<Output = anyhow::Result<Vec<RemoteIssue>>> + Send + 'a {
            async move {
                let key = format!("{owner}/{repo}");
                let children = self
                    .sub_issues
                    .lock()
                    .expect("sub-issues lock")
                    .iter()
                    .filter(|(slug, parent, _, _)| slug == &key && *parent == number)
                    .map(|(_, _, child_repo, child)| (child_repo.clone(), *child))
                    .collect::<Vec<(String, u64)>>();
                let issues = self.issues.lock().expect("issues lock");
                Ok(children
                    .into_iter()
                    .filter_map(|(child_repo, child)| {
                        let issue = issues
                            .get(&child_repo)?
                            .iter()
                            .find(|issue| issue.id == child)?
                            .clone();
                        // GitHub stamps every payload with the repo it came
                        // from, which is how a cross-repo child is spotted.
                        Some(RemoteIssue {
                            repository: Some(child_repo),
                            ..issue
                        })
                    })
                    .collect())
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
            owner: &'a str,
            repo: &'a str,
            number: u64,
            labels: &'a [String],
        ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a {
            async move {
                self.pull_request_labels
                    .lock()
                    .expect("pr labels")
                    .push((number, labels.to_vec()));
                // Mirror GitHub: the labels land on the issue itself, so a
                // pull that follows sees them. A number with no issue in the
                // fake (the PR step's metadata copy) is left alone.
                if let Some(issues) = self
                    .issues
                    .lock()
                    .expect("issues lock")
                    .get_mut(&format!("{owner}/{repo}"))
                    && let Some(issue) = issues.iter_mut().find(|issue| issue.number == number)
                {
                    for label in labels {
                        if !issue.labels.contains(label) {
                            issue.labels.push(label.clone());
                        }
                    }
                }
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
    async fn a_yielded_pass_stops_between_issues_and_leaves_the_rest_for_next_time()
    -> anyhow::Result<()> {
        let (mut storage, integration, _) = bound_store("o", "r").await?;
        let fake = FakeGithub::default()
            .with_issue("o/r", remote(1, "First", "open", 100))
            .with_issue("o/r", remote(2, "Second", "open", 110));

        // Asked to stop once the first issue has been applied: the pass ends
        // there rather than sync the second, which the next pass picks up.
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let pull_back = || asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 2;
        let outcome = storage
            .sync_github_integration_yielding(&fake, integration.id, false, &pull_back)
            .await?;
        assert!(outcome.aborted, "the pass stopped early");
        assert_eq!(outcome.summary.imported, 1);
        assert!(storage.issue_link(integration.id, "o/r#1").await?.is_some());
        assert!(
            storage.issue_link(integration.id, "o/r#2").await?.is_none(),
            "the yield stopped the pass before the second issue"
        );
        // No cursor: resuming re-reads this window instead of skipping it.
        assert!(storage.sync_cursor(integration.id, "o/r").await?.is_none());

        let uninterrupted = || false;
        let outcome = storage
            .sync_github_integration_yielding(&fake, integration.id, false, &uninterrupted)
            .await?;
        assert!(!outcome.aborted);
        assert_eq!(outcome.summary.imported, 1);
        assert!(storage.issue_link(integration.id, "o/r#2").await?.is_some());
        assert!(storage.sync_cursor(integration.id, "o/r").await?.is_some());
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
        // stays local instead of being pushed as an issue edit (§5.5). The
        // exclusion reads the explicit step role, so a run root or a promoted
        // subtask (which also carries a run) keeps syncing.
        Task::update_by_id(task_id)
            .workflow_run_id(Some(4))
            .role(Some("step".to_string()))
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

    #[tokio::test]
    async fn a_new_task_in_a_bound_project_opens_an_issue() -> anyhow::Result<()> {
        let (mut storage, integration, tag) = bound_store("lofi-tools", "todo-lofi").await?;
        let task = storage
            .create_task(
                Task::create()
                    .title("Add login".to_string())
                    .description(Some("With OAuth".to_string())),
            )
            .await?;
        storage.assign_tag_to_task(task.id, &tag.name).await?;

        let fake = FakeGithub::default();
        let opened = storage.push_github_new_task(&fake, task.id).await?;
        assert_eq!(opened.as_ref().map(|issue| issue.number), Some(1));
        assert_eq!(
            fake.created_issues(),
            vec![(
                "lofi-tools/todo-lofi".to_string(),
                "Add login".to_string(),
                "With OAuth".to_string()
            )]
        );

        // The link carries the fresh issue as its snapshot, so the pass that
        // follows merges instead of importing a second task for it.
        let link = storage
            .issue_link(integration.id, "lofi-tools/todo-lofi#1")
            .await?
            .expect("the issue was linked");
        assert_eq!(link.task_id, task.id);
        assert_eq!(link.state.remote.title, "Add login");
        let summary = storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;
        assert_eq!(summary.imported, 0);
        assert_eq!(storage.list_tasks().await?.len(), 1);

        // A task that is already issue-backed never opens a second issue; the
        // push reports the issue it has.
        assert_eq!(
            storage
                .push_github_new_task(&fake, task.id)
                .await?
                .map(|issue| issue.number),
            Some(1)
        );
        assert_eq!(fake.created_issues().len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn a_section_task_resolves_its_projects_repo() -> anyhow::Result<()> {
        let (mut storage, _, tag) = bound_store("lofi-tools", "todo-lofi").await?;
        let section = storage.create_tag("In progress").await?;
        storage.add_tag_implication(section.id, tag.id).await?;
        let task = storage.create_task(Task::create().title("Add login".to_string())).await?;
        storage.assign_tag_to_task(task.id, "In progress").await?;

        let fake = FakeGithub::default();
        assert!(storage.push_github_new_task(&fake, task.id).await?.is_some());
        assert_eq!(fake.created_issues().len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn tasks_outside_a_bound_project_open_nothing() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        storage.create_integration("github", None).await?;
        let task = storage.create_task(Task::create().title("Local only".to_string())).await?;
        storage.assign_tag_to_task(task.id, "errands").await?;

        let fake = FakeGithub::default();
        assert!(storage.push_github_new_task(&fake, task.id).await?.is_none());
        assert!(fake.created_issues().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn a_run_step_never_opens_an_issue() -> anyhow::Result<()> {
        let (mut storage, _, tag) = bound_store("lofi-tools", "todo-lofi").await?;
        let step = storage
            .create_task(
                Task::create()
                    .title("Interview".to_string())
                    .workflow_run_id(Some(4))
                    .role(Some("step".to_string())),
            )
            .await?;
        storage.assign_tag_to_task(step.id, &tag.name).await?;

        let fake = FakeGithub::default();
        assert!(storage.push_github_new_task(&fake, step.id).await?.is_none());
        assert!(fake.created_issues().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn a_subtask_opens_an_issue_under_its_parents_issue() -> anyhow::Result<()> {
        let (mut storage, integration, tag) = bound_store("lofi-tools", "todo-lofi").await?;
        let parent = storage
            .create_task(Task::create().title("Add login".to_string()))
            .await?;
        storage.assign_tag_to_task(parent.id, &tag.name).await?;
        let subtask = storage
            .create_task(
                Task::create()
                    .title("Sketch the form".to_string())
                    .parent_id(Some(parent.id)),
            )
            .await?;

        // Nothing above the subtask is on GitHub yet, so the push opens the
        // parent's issue first and then hangs the child under it.
        let fake = FakeGithub::default();
        let opened = storage.push_github_new_task(&fake, subtask.id).await?;
        assert_eq!(opened.as_ref().map(|issue| issue.number), Some(2));
        assert_eq!(fake.created_issues().len(), 2);
        assert_eq!(
            fake.sub_issue_calls(),
            vec![("lofi-tools/todo-lofi".to_string(), 1, 2)]
        );
        let child = storage
            .issue_link(integration.id, "lofi-tools/todo-lofi#2")
            .await?
            .expect("the child issue was linked");
        assert_eq!(child.task_id, subtask.id);
        assert_eq!(child.state.issue_id, Some(2));
        assert_eq!(
            child.state.parent_issue.as_deref(),
            Some("lofi-tools/todo-lofi#1")
        );

        // A second push is quiet: both links exist and the relationship is
        // recorded, so neither an issue nor a sub-issue call is repeated.
        assert!(
            storage
                .push_github_new_task(&fake, subtask.id)
                .await?
                .is_some()
        );
        assert_eq!(fake.created_issues().len(), 2);
        assert_eq!(fake.sub_issue_calls().len(), 1);

        // The pull that follows merges both and keeps the tree: no duplicate
        // task, and the child stays a subtask.
        let summary = storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;
        assert_eq!(summary.imported, 0);
        assert_eq!(summary.nested, 0);
        assert_eq!(storage.list_tasks().await?.len(), 2);
        assert_eq!(storage.list_subtasks(parent.id).await?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn a_nested_subtask_chain_mirrors_under_each_level() -> anyhow::Result<()> {
        let (mut storage, integration, tag) = bound_store("o", "r").await?;
        let grandparent = storage
            .create_task(Task::create().title("Add login".to_string()))
            .await?;
        storage.assign_tag_to_task(grandparent.id, &tag.name).await?;
        let parent = storage
            .create_task(
                Task::create()
                    .title("The form".to_string())
                    .parent_id(Some(grandparent.id)),
            )
            .await?;
        let child = storage
            .create_task(
                Task::create()
                    .title("Sketch it".to_string())
                    .parent_id(Some(parent.id)),
            )
            .await?;

        // Pushing the leaf opens the whole chain, top down, and hangs each
        // issue under the one above it.
        let fake = FakeGithub::default();
        assert!(
            storage
                .push_github_new_task(&fake, child.id)
                .await?
                .is_some()
        );
        assert_eq!(fake.created_issues().len(), 3);
        assert_eq!(
            fake.sub_issue_calls(),
            vec![("o/r".to_string(), 1, 2), ("o/r".to_string(), 2, 3)]
        );
        assert_eq!(
            storage
                .issue_link(integration.id, "o/r#3")
                .await?
                .unwrap()
                .state
                .parent_issue
                .as_deref(),
            Some("o/r#2")
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_child_link_without_a_recorded_id_is_looked_up_before_attaching()
    -> anyhow::Result<()> {
        let (mut storage, integration, tag) = bound_store("o", "r").await?;
        let parent = storage
            .create_task(Task::create().title("Add login".to_string()))
            .await?;
        storage.assign_tag_to_task(parent.id, &tag.name).await?;
        let subtask = storage
            .create_task(
                Task::create()
                    .title("Sketch the form".to_string())
                    .parent_id(Some(parent.id)),
            )
            .await?;
        // The child as an older build linked it: which issue it is, known; its
        // row id, not.
        storage
            .link_issue(
                integration.id,
                "o/r#1",
                subtask.id,
                &IssueFieldState::default(),
                None,
            )
            .await?;

        // Only the parent's issue is opened (#2, since #1 exists), and the
        // attachment uses the id the lookup returned for the child.
        let fake = FakeGithub::default().with_issue("o/r", remote(1, "Sketch the form", "open", 100));
        assert!(
            storage
                .push_github_new_task(&fake, subtask.id)
                .await?
                .is_some()
        );
        assert_eq!(fake.created_issues().len(), 1);
        assert_eq!(fake.sub_issue_calls(), vec![("o/r".to_string(), 2, 1)]);
        let child = storage.issue_link(integration.id, "o/r#1").await?.unwrap();
        assert_eq!(child.state.issue_id, Some(1));
        assert_eq!(child.state.parent_issue.as_deref(), Some("o/r#2"));
        Ok(())
    }

    #[tokio::test]
    async fn nothing_under_a_workflow_step_is_pushed() -> anyhow::Result<()> {
        let (mut storage, _, tag) = bound_store("lofi-tools", "todo-lofi").await?;
        let run = storage
            .create_task(Task::create().title("Add login".to_string()))
            .await?;
        storage.assign_tag_to_task(run.id, &tag.name).await?;
        let step = storage
            .create_task(
                Task::create()
                    .title("Interview".to_string())
                    .workflow_run_id(Some(4))
                    .role(Some("step".to_string()))
                    .parent_id(Some(run.id)),
            )
            .await?;
        let note = storage
            .create_task(
                Task::create()
                    .title("Answer the questions".to_string())
                    .parent_id(Some(step.id)),
            )
            .await?;

        // Run content is the app's, not the issue's: a step is not pushed, and
        // neither is anything under it.
        let fake = FakeGithub::default();
        assert!(storage.push_github_new_task(&fake, note.id).await?.is_none());
        assert!(fake.created_issues().is_empty());
        // The run's own task still syncs; only its step subtask is left out.
        assert!(storage.push_github_new_task(&fake, run.id).await?.is_some());
        assert_eq!(fake.created_issues().len(), 1);
        assert!(fake.sub_issue_calls().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn a_pulled_sub_issue_becomes_a_subtask_and_un_nests_when_removed() -> anyhow::Result<()> {
        let (mut storage, integration, _) = bound_store("o", "r").await?;
        let fake = FakeGithub::default()
            .with_issue("o/r", remote(1, "Add login", "open", 100))
            .with_issue("o/r", remote(2, "Sketch the form", "open", 100))
            .with_sub_issue("o/r", 1, 2);

        let summary = storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;
        assert_eq!(summary.imported, 2);
        assert_eq!(summary.nested, 1);
        let parent_id = storage
            .issue_link(integration.id, "o/r#1")
            .await?
            .unwrap()
            .task_id;
        let child_id = storage
            .issue_link(integration.id, "o/r#2")
            .await?
            .unwrap()
            .task_id;
        assert_eq!(storage.get_task(child_id).await?.parent_id, Some(parent_id));
        assert_eq!(
            storage
                .issue_link(integration.id, "o/r#2")
                .await?
                .unwrap()
                .state
                .parent_issue
                .as_deref(),
            Some("o/r#1")
        );

        // Un-parenting it on GitHub un-nests it locally even though the parent
        // no longer reports children in its summary: the recorded
        // relationship is what finds it.
        fake.remove_sub_issue("o/r", 1, 2);
        storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;
        assert_eq!(storage.get_task(child_id).await?.parent_id, None);
        assert_eq!(
            storage
                .issue_link(integration.id, "o/r#2")
                .await?
                .unwrap()
                .state
                .parent_issue,
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_sub_issue_in_another_repo_is_left_alone() -> anyhow::Result<()> {
        let (mut storage, integration, _) = bound_store("o", "r").await?;
        let fake = FakeGithub::default()
            .with_issue("o/r", remote(1, "Add login", "open", 100))
            .with_issue("o/r", remote(9, "A local issue", "open", 100))
            .with_issue("o/other", remote(9, "Elsewhere", "open", 100))
            .with_sub_issue_from("o/r", 1, "o/other", 9);

        let summary = storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;
        // This repo's #9 is a task of its own; the other repo's #9 shares its
        // number, so it is not paired with it and nothing is nested.
        assert_eq!(summary.imported, 2);
        assert_eq!(summary.nested, 0);
        let child_id = storage
            .issue_link(integration.id, "o/r#9")
            .await?
            .expect("this repo's #9 imported")
            .task_id;
        assert_eq!(storage.get_task(child_id).await?.parent_id, None);
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
    async fn an_imported_label_becomes_a_subtag_of_the_project() -> anyhow::Result<()> {
        let (mut storage, integration, project) = bound_store("o", "r").await?;
        // The user already has a tag named after one of the labels, so that
        // one is reused and nested instead of being copied as `github/o/r/…`.
        let mine = storage.create_tag("bugs").await?;
        let mut issue = remote(1, "Buggy", "open", 100);
        issue.labels = vec!["bug".to_string()];
        let mut other = remote(2, "Also buggy", "open", 110);
        other.labels = vec!["Bugs".to_string()];
        let fake = FakeGithub::default()
            .with_issue("o/r", issue)
            .with_issue("o/r", other);
        storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;

        // A new label hangs under the project tag, named per repo and
        // displayed as the label, so the navbar reads `project > bug`.
        let children = storage.get_children(project.id).await?;
        let bug = children
            .iter()
            .find(|tag| tag.label() == "bug")
            .expect("the label is a subtag of the project");
        assert_eq!(bug.name, "github/o/r/bug");
        let task_id = storage
            .issue_link(integration.id, "o/r#1")
            .await?
            .unwrap()
            .task_id;
        assert!(
            storage
                .get_direct_task_tags(task_id)
                .await?
                .iter()
                .any(|tag| tag.id == bug.id)
        );

        // The user's own tag is reused (and nested), not duplicated.
        let bugs = children
            .iter()
            .find(|tag| tag.label() == "bugs")
            .expect("the user's tag is nested, not copied");
        assert_eq!(bugs.id, mine.id);
        assert!(storage.get_tag_by_name("github/o/r/Bugs").await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn a_captured_task_carries_its_subtags_as_labels() -> anyhow::Result<()> {
        let (mut storage, integration, project) = bound_store("o", "r").await?;
        // A subtag placed under the project, as tag settings does.
        let subtag = storage.create_tag("bugs").await?;
        storage.add_tag_implication(subtag.id, project.id).await?;
        let task = storage
            .create_task(Task::create().title("Add login".to_string()))
            .await?;
        storage.assign_tag_to_task(task.id, &project.name).await?;
        storage.assign_tag_to_task(task.id, &subtag.name).await?;

        // Opening the issue reflects the local tags at once, so the label does
        // not wait for the next sync.
        let fake = FakeGithub::default();
        assert!(
            storage
                .push_github_new_task(&fake, task.id)
                .await?
                .is_some()
        );
        assert_eq!(
            *fake.labels.lock().expect("labels lock"),
            vec!["bugs".to_string()]
        );
        assert_eq!(
            *fake.pull_request_labels.lock().expect("labels"),
            vec![(1u64, vec!["bugs".to_string()])]
        );
        // The snapshot took them, so the pull that follows is quiet rather
        // than reading the app's own push as a remote edit.
        let summary = storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;
        assert_eq!(summary.pushed, 0);
        assert_eq!(storage.list_tasks().await?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn the_same_label_in_two_projects_stays_two_tags() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let integration = storage.create_integration("github", None).await?;
        let first = storage.create_tag("first").await?;
        storage
            .bind_repo_tag(first.id, integration.id, "o", "first")
            .await?;
        let second = storage.create_tag("second").await?;
        storage
            .bind_repo_tag(second.id, integration.id, "o", "second")
            .await?;

        let mut issue = remote(1, "Buggy", "open", 100);
        issue.labels = vec!["bug".to_string()];
        let fake = FakeGithub::default()
            .with_issue("o/first", issue.clone())
            .with_issue("o/second", issue);
        storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;

        let first_bug = storage
            .get_children(first.id)
            .await?
            .into_iter()
            .find(|tag| tag.label() == "bug")
            .expect("first project nests the label");
        let second_bug = storage
            .get_children(second.id)
            .await?
            .into_iter()
            .find(|tag| tag.label() == "bug")
            .expect("second project nests the label");
        assert_ne!(first_bug.id, second_bug.id);
        // Each project's view holds only its own task: the two label tags are
        // not shared, so neither project's aggregation reaches the other.
        let first_tasks = storage.list_tasks_by_tag(first.id).await?;
        assert_eq!(first_tasks.len(), 1);
        assert_eq!(first_tasks[0].title, "Buggy");
        let second_tasks = storage.list_tasks_by_tag(second.id).await?;
        assert_eq!(second_tasks.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn a_local_subtag_of_a_project_is_pushed_as_a_label() -> anyhow::Result<()> {
        let (mut storage, integration, project) = bound_store("o", "r").await?;
        // A subtag placed under the project, as tag settings does.
        let subtag = storage.create_tag("bugs").await?;
        storage.add_tag_implication(subtag.id, project.id).await?;

        let mut issue = remote(1, "Buggy", "open", 100);
        issue.labels = Vec::new();
        let fake = FakeGithub::default().with_issue("o/r", issue);
        storage
            .sync_github_integration(&fake, integration.id, true)
            .await?;
        let task_id = storage
            .issue_link(integration.id, "o/r#1")
            .await?
            .unwrap()
            .task_id;

        storage.assign_tag_to_task(task_id, &subtag.name).await?;
        let summary = storage
            .sync_github_integration(&fake, integration.id, false)
            .await?;
        assert_eq!(summary.labels, 1, "the subtag becomes a remote label");
        assert_eq!(
            *fake.labels.lock().expect("labels lock"),
            vec!["bugs".to_string()]
        );
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
    async fn unbinding_a_repo_moves_the_target_or_clears_it() -> anyhow::Result<()> {
        let mut storage = TodoStore::for_test().await?;
        let integration = storage.create_integration("github", None).await?;
        let tag = storage.create_tag("project").await?;
        storage
            .bind_repo_tag(tag.id, integration.id, "lofi-tools", "todo-lofi")
            .await?;
        storage
            .bind_repo_tag(tag.id, integration.id, "lofi-tools", "api")
            .await?;
        // Two repos bound; the target follows the last binding.
        assert_eq!(
            storage
                .tag_settings(tag.id)
                .await?
                .sync_target
                .unwrap()
                .external_id,
            "lofi-tools/api"
        );
        // Removing the targeted repo retargets to the remaining one.
        storage
            .unbind_repo_tag(tag.id, integration.id, "lofi-tools/api")
            .await?;
        let bound: Vec<String> = storage
            .bound_repos(integration.id)
            .await?
            .into_iter()
            .filter(|bound| bound.tag_id == tag.id)
            .map(|bound| bound.external_id())
            .collect();
        assert_eq!(bound, vec!["lofi-tools/todo-lofi"]);
        assert_eq!(
            storage
                .tag_settings(tag.id)
                .await?
                .sync_target
                .unwrap()
                .external_id,
            "lofi-tools/todo-lofi"
        );
        // Removing the last one clears the target entirely.
        storage
            .unbind_repo_tag(tag.id, integration.id, "lofi-tools/todo-lofi")
            .await?;
        assert!(
            storage
                .tag_settings(tag.id)
                .await?
                .sync_target
                .is_none()
        );
        assert!(storage.bound_repos(integration.id).await?.is_empty());
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
            "created_at": "2026-09-01T08:30:00Z",
            "closed_at": "2026-09-14T09:00:00Z",
        }))
        .expect("an issue");
        assert_eq!(issue.number, 4);
        assert_eq!(issue.labels, vec!["bug".to_string(), "p1".to_string()]);
        assert_eq!(issue.assignees, vec!["me".to_string()]);
        assert_eq!(issue.state, "closed");
        assert!(issue.updated_at.is_some());
        // When it was opened, so the pane can show `opened 3h ago` the way
        // GitHub's header does.
        assert_eq!(issue.author.as_deref(), Some("author"));
        assert_eq!(
            issue.created_at.map(|at| at.to_string()),
            Some("2026-09-01T08:30:00Z".to_string())
        );
        assert_eq!(
            issue.closed_at.map(|at| at.to_string()),
            Some("2026-09-14T09:00:00Z".to_string())
        );
        // The link state mirrors the dates for display; they are never pushed.
        let state = issue_state_for(&issue);
        assert_eq!(state.created_at, issue.created_at.map(|at| at.as_second()));
        assert_eq!(state.closed_at, issue.closed_at.map(|at| at.as_second()));
        assert_eq!(state.author.as_deref(), Some("author"));

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
