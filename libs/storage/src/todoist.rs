//! Todoist project sync (import direction).
//!
//! For every project linked to `integration_id` (source_kind `project`,
//! created by picking a Todoist project in the UI):
//! 1. Fetch remote sections → child tags under the project tag (spec §3.1)
//!    plus `tag_sections` rows on the project tag.
//! 2. Fetch remote tasks → create or per-field-merge local tasks, tagged
//!    with the project tag and their section's child tag.
//! 3. Tombstone locally-linked tasks missing remotely (§4.3).
//!
//! Push (local → Todoist) is not implemented yet: remote state wins every
//! mapped field when the remote `updated_at` is newer than the link's
//! timestamp, so local completions/edits are overwritten until push lands.
//! Label/section removal is additive-only for the same reason.

use crate::{QueryResult, TodoStore};
use snafu::ResultExt;
use std::collections::{HashMap, HashSet};

const API_BASE: &str = "https://api.todoist.com/api/v1";
const SYNC_URL: &str = "https://api.todoist.com/api/v1/sync";

#[derive(Debug, Clone, serde::Deserialize)]
struct RemoteSection {
    id: String,
    name: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct RemoteDue {
    date: String,
    #[serde(default)]
    datetime: Option<String>,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default)]
    is_recurring: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct RemoteTask {
    id: String,
    content: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    section_id: Option<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    priority: i64,
    #[serde(default)]
    due: Option<RemoteDue>,
    #[serde(default)]
    updated_at: Option<String>,
}

/// Counts reported after an integration sync.
#[derive(Debug, Default)]
pub struct SyncSummary {
    pub projects: usize,
    pub sections: usize,
    pub tasks_upserted: usize,
    pub tasks_tombstoned: usize,
}

struct ProjectSync {
    sections: usize,
    tasks: usize,
    ids: Vec<String>,
}

/// Todoist priority (4 highest … 1 lowest) → `urgency_factor` (spec §3.3).
pub fn urgency_for_priority(priority: i64) -> f64 {
    match priority {
        4 => 2.0,
        3 => 1.5,
        2 => 1.25,
        _ => 0.5,
    }
}

/// Inverse of [`urgency_for_priority`] for push: nearest Todoist priority
/// at or below the local urgency.
pub fn priority_for_urgency(urgency: f64) -> i64 {
    if urgency >= 2.0 {
        4
    } else if urgency >= 1.5 {
        3
    } else if urgency >= 1.25 {
        2
    } else {
        1
    }
}

/// Local deadline (UTC epoch) → Todoist due `date` (`YYYY-MM-DD`).
pub fn due_date_for_deadline(deadline: u64) -> Option<String> {
    jiff::Timestamp::from_second(deadline as i64)
        .ok()
        .map(|stamp| stamp.to_string()[..10].to_string())
}

/// Due date → UTC epoch seconds. Prefers `datetime`, falls back to `date`
/// at midnight UTC. Returns `None` when nothing parses.
pub fn deadline_for_due(due: &RemoteDue) -> Option<u64> {
    if let Some(datetime) = due.datetime.as_deref()
        && let Ok(stamp) = datetime.parse::<jiff::Timestamp>()
    {
        return Some(stamp.as_second() as u64);
    }
    if let Ok(date) = due.date.parse::<jiff::civil::Date>() {
        if let Ok(zoned) = date.to_zoned(jiff::tz::TimeZone::UTC) {
            return Some(zoned.timestamp().as_second() as u64);
        }
    }
    None
}

fn parse_updated(value: Option<&str>) -> Option<jiff::Timestamp> {
    value.and_then(|raw| raw.parse().ok())
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

fn url_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

async fn get_json(
    token: &str,
    path: &str,
    params: &[(&str, &str)],
) -> anyhow::Result<serde_json::Value> {
    let body: serde_json::Value = client()
        .get(format!("{API_BASE}{path}"))
        .query(params)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Todoist request failed: {e}"))?
        .error_for_status()
        .map_err(|e| anyhow::anyhow!("Todoist request failed: {e}"))?
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Todoist response was not JSON: {e}"))?;
    Ok(body)
}

fn results(body: &serde_json::Value) -> Vec<serde_json::Value> {
    body
        .get("results")
        .unwrap_or(body)
        .as_array()
        .cloned()
        .unwrap_or_default()
}

async fn fetch_sections(token: &str, project_id: &str) -> anyhow::Result<Vec<RemoteSection>> {
    let body = get_json(token, "/sections", &[("project_id", project_id)]).await?;
    Ok(results(&body)
        .iter()
        .filter_map(|item| {
            Some(RemoteSection {
                id: item.get("id")?.as_str()?.to_string(),
                name: item.get("name")?.as_str()?.to_string(),
            })
        })
        .collect())
}

async fn fetch_tasks(token: &str, project_id: &str) -> anyhow::Result<Vec<RemoteTask>> {
    let mut tasks = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let mut params = vec![("project_id", project_id), ("limit", "200")];
        if let Some(next) = cursor.as_deref() {
            params.push(("cursor", next));
        }
        let body = get_json(token, "/tasks", &params).await?;
        for item in results(&body) {
            let Ok(task) = serde_json::from_value::<RemoteTask>(item) else {
                continue;
            };
            tasks.push(task);
        }
        cursor = body
            .get("next_cursor")
            .and_then(|c| c.as_str())
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    Ok(tasks)
}

impl TodoStore {
    /// Sync every linked Todoist project of one integration. Runs on the
    /// Tokio runtime (network + blocking file-free DB work).
    pub async fn sync_todoist_integration(
        &mut self,
        token: &str,
        integration_id: u64,
    ) -> QueryResult<SyncSummary> {
        let projects: Vec<(String, u64)> = self
            .tag_links_for_integration(integration_id)
            .await?
            .into_iter()
            .filter(|link| link.source_kind == "project")
            .map(|link| (link.external_id, link.tag_id))
            .collect();

        let mut summary = SyncSummary::default();
        let mut remote_ids: HashSet<(String, u64)> = HashSet::new();
        for (project_id, tag_id) in &projects {
            let project_ids = self
                .sync_todoist_project(token, integration_id, project_id, *tag_id)
                .await
                .map_err(|e| crate::QueryErr::UnexpectedValue {
                    message: format!("Todoist sync failed for project {project_id}: {e}"),
                })?;
            summary.projects += 1;
            summary.sections += project_ids.sections;
            summary.tasks_upserted += project_ids.tasks;
            remote_ids.extend(project_ids.ids.into_iter().map(|id| (id, integration_id)));
            self.record_watermark(
                integration_id,
                project_id,
                &jiff::Timestamp::now().to_string(),
            )
            .await?;
        }

        for link in self.task_links_for_integration(integration_id).await? {
            if !remote_ids.contains(&(link.external_id.clone(), link.integration_id))
                && self.get_task(link.task_id).await?.deleted_at.is_none()
            {
                self.tombstone_task(link.task_id).await?;
                summary.tasks_tombstoned += 1;
            }
        }
        Ok(summary)
    }

    /// Sync one linked project: sections as child tags + `tag_sections`
    /// rows, tasks upserted with per-field merge. Returns remote task ids
    /// for tombstone comparison by the caller.
    async fn sync_todoist_project(
        &mut self,
        token: &str,
        integration_id: u64,
        project_id: &str,
        project_tag_id: u64,
    ) -> anyhow::Result<ProjectSync> {
        // The project tag itself carries the sync target from here on, so
        // re-syncs and future push code find it without the link table.
        self.set_tag_sync_target(
            project_tag_id,
            Some(crate::SyncTarget {
                integration_id,
                external_id: project_id.to_string(),
            }),
        )
        .await?;

        let sections = fetch_sections(token, project_id).await?;
        let mut section_tags: HashMap<String, u64> = HashMap::new();
        for section in &sections {
            let tag_id = self
                .todoist_section_tag(integration_id, project_tag_id, section)
                .await?;
            self.add_tag_section(project_tag_id, section.name.clone())
                .await?;
            section_tags.insert(section.id.clone(), tag_id);
        }

        let tasks = fetch_tasks(token, project_id).await?;
        let mut local_ids: HashMap<String, u64> = HashMap::new();
        for task in &tasks {
            let id = self
                .upsert_todoist_task(
                    token,
                    integration_id,
                    project_tag_id,
                    &section_tags,
                    task,
                )
                .await?;
            local_ids.insert(task.id.clone(), id);
        }
        // Second pass: parents must exist before linking.
        for task in &tasks {
            if let (Some(remote_parent), Some(&local_id)) =
                (task.parent_id.as_deref(), local_ids.get(&task.id))
                && let Some(&local_parent) = local_ids.get(remote_parent)
            {
                crate::Task::update_by_id(local_id)
                    .parent_id(Some(local_parent))
                    .exec(&mut self.db)
                    .await
                    .context(crate::error::UpdateTaskSnafu { id: local_id })?;
            }
        }

        Ok(ProjectSync {
            sections: sections.len(),
            tasks: tasks.len(),
            ids: tasks.into_iter().map(|t| t.id).collect(),
        })
    }

    /// Get-or-create the child tag for a remote section, linked for
    /// idempotent re-syncs and implied by the project tag.
    async fn todoist_section_tag(
        &mut self,
        integration_id: u64,
        project_tag_id: u64,
        section: &RemoteSection,
    ) -> QueryResult<u64> {
        if let Some(link) = self.tag_link(integration_id, &section.id).await?
            && self.get_tag(link.tag_id).await.is_ok()
        {
            return Ok(link.tag_id);
        }
        let tag = match self.get_tag_by_name(&section.name).await? {
            Some(tag) => tag,
            None => {
                let scoped = format!("todoist/{}", section.name);
                match self.get_tag_by_name(&scoped).await? {
                    Some(tag) => tag,
                    None => {
                        self.create_tag_with_display_name(
                            scoped,
                            Some(section.name.clone()),
                        )
                        .await?
                    }
                }
            }
        };
        let namespaced = tag.display_name.is_some();
        // Idempotent re-sync: the link may already imply the project tag,
        // which would report a cycle.
        let _ = self.add_tag_implication(tag.id, project_tag_id).await;
        self.link_tag(integration_id, &section.id, tag.id, "section", namespaced)
            .await?;
        Ok(tag.id)
    }

    /// Create a new local task for a remote one, or merge remote fields
    /// when the remote `updated_at` is newer than the link timestamp.
    async fn upsert_todoist_task(
        &mut self,
        _token: &str,
        integration_id: u64,
        project_tag_id: u64,
        section_tags: &HashMap<String, u64>,
        remote: &RemoteTask,
    ) -> QueryResult<u64> {
        let deadline = remote.due.as_ref().and_then(deadline_for_due);
        let timezone = remote
            .due
            .as_ref()
            .and_then(|due| due.timezone.clone());
        let urgency = urgency_for_priority(remote.priority);
        let remote_updated = parse_updated(remote.updated_at.as_deref());

        if let Some(link) = self.task_link(integration_id, &remote.id).await? {
            let newer = match (remote_updated, link.external_updated_at) {
                (Some(remote), Some(known)) => remote > known,
                (Some(_), None) => true,
                (None, _) => true,
            };
            if newer {
                crate::Task::update_by_id(link.task_id)
                    .title(remote.content.clone())
                    .description(if remote.description.is_empty() {
                        None
                    } else {
                        Some(remote.description.clone())
                    })
                    .deadline(deadline)
                    .urgency_factor(urgency)
                    .timezone(timezone)
                    .deleted_at(None)
                    .done(false)
                    .exec(&mut self.db)
                    .await
                    .context(crate::error::UpdateTaskSnafu {
                        id: link.task_id,
                    })?;
                self.link_task(
                    integration_id,
                    &remote.id,
                    link.task_id,
                    remote_updated,
                )
                .await?;
            }
            self.assign_todoist_tags(
                link.task_id,
                project_tag_id,
                section_tags,
                remote,
            )
            .await?;
            return Ok(link.task_id);
        }

        let created = self
            .create_task(
                crate::Task::create()
                    .title(remote.content.clone())
                    .description(if remote.description.is_empty() {
                        None
                    } else {
                        Some(remote.description.clone())
                    })
                    .deadline(deadline)
                    .urgency_factor(urgency)
                    .timezone(timezone),
            )
            .await?;
        self.link_task(integration_id, &remote.id, created.id, remote_updated)
            .await?;
        self.assign_todoist_tags(created.id, project_tag_id, section_tags, remote)
            .await?;
        Ok(created.id)
    }

    /// Project tag, section child tag and flat label tags. Additive-only:
    /// tags removed remotely stay until push/merge handles them.
    async fn assign_todoist_tags(
        &mut self,
        task_id: u64,
        project_tag_id: u64,
        section_tags: &HashMap<String, u64>,
        remote: &RemoteTask,
    ) -> QueryResult<()> {
        let project = self.get_tag(project_tag_id).await?;
        self.assign_tag_to_task(task_id, &project.name).await?;
        if let Some(section_id) = remote.section_id.as_deref()
            && let Some(&section_tag_id) = section_tags.get(section_id)
        {
            let section = self.get_tag(section_tag_id).await?;
            self.assign_tag_to_task(task_id, &section.name).await?;
        }
        for label in &remote.labels {
            self.assign_tag_to_task(task_id, label).await?;
        }
        Ok(())
    }
}

/// Delta of mapped fields to push for one task. `None` = leave untouched;
/// `Some(None)` (where applicable) = clear on the remote side.
#[derive(Debug, Default)]
pub struct TaskPatch {
    pub content: Option<String>,
    pub description: Option<Option<String>>,
    pub due_date: Option<Option<String>>,
    pub priority: Option<i64>,
    pub done: Option<bool>,
}

fn sync_command(command_type: &str, args: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type": command_type,
        "uuid": uuid::Uuid::new_v4().to_string(),
        "args": args,
    })
}

/// Send Sync commands and fail unless every `sync_status[uuid]` is `"ok"`.
/// UUIDs make retries idempotent: the server never re-executes a UUID.
async fn send_commands(
    token: &str,
    commands: Vec<serde_json::Value>,
) -> anyhow::Result<()> {
    if commands.is_empty() {
        return Ok(());
    }
    let body: serde_json::Value = client()
        .post(SYNC_URL)
        .bearer_auth(token)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(format!(
            "commands={}",
            url_encode(&serde_json::to_string(&commands).unwrap_or_else(|_| "[]".to_string()))
        ))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Todoist sync request failed: {e}"))?
        .error_for_status()
        .map_err(|e| anyhow::anyhow!("Todoist sync request failed: {e}"))?
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Todoist sync response was not JSON: {e}"))?;
    let status = body.get("sync_status").cloned().unwrap_or_default();
    let failures: Vec<String> = commands
        .iter()
        .filter_map(|command| command.get("uuid")?.as_str())
        .filter(|uuid| status.get(*uuid).and_then(|s| s.as_str()) != Some("ok"))
        .map(|uuid| {
            format!(
                "{uuid}: {}",
                status.get(uuid).cloned().unwrap_or_default()
            )
        })
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "Todoist rejected sync commands: {}",
            failures.join("; ")
        ))
    }
}

impl TodoStore {
    /// Push a field delta for one linked task via Sync commands
    /// (`item_update` for partial fields, `item_close`/`item_uncomplete`
    /// for completion — `item_update` explicitly does not support those).
    /// Seed rows and workflow steps never sync: silently skips them.
    /// Refreshes the link timestamp so the next import sees remote state
    /// as current.
    pub async fn push_todoist_patch(
        &mut self,
        token: &str,
        integration_id: u64,
        external_id: &str,
        task_id: u64,
        patch: &TaskPatch,
    ) -> QueryResult<()> {
        let task = self.get_task(task_id).await?;
        if task.is_seed || task.workflow_run_id.is_some() || task.trip_id.is_some() {
            return Ok(());
        }
        let mut update_args = serde_json::Map::new();
        update_args.insert(
            "id".to_string(),
            serde_json::Value::String(external_id.to_string()),
        );
        if let Some(content) = &patch.content {
            update_args.insert(
                "content".to_string(),
                serde_json::Value::String(content.clone()),
            );
        }
        if let Some(description) = &patch.description {
            update_args.insert(
                "description".to_string(),
                description
                    .clone()
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        if let Some(due) = &patch.due_date {
            update_args.insert(
                "due".to_string(),
                due.clone()
                    .map(|date| serde_json::json!({ "date": date }))
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        if let Some(priority) = patch.priority {
            update_args.insert(
                "priority".to_string(),
                serde_json::Value::from(priority),
            );
        }
        let mut commands = Vec::new();
        if update_args.len() > 1 {
            commands.push(sync_command(
                "item_update",
                serde_json::Value::Object(update_args),
            ));
        }
        if let Some(done) = patch.done {
            commands.push(sync_command(
                if done { "item_close" } else { "item_uncomplete" },
                serde_json::json!({ "id": external_id }),
            ));
        }
        send_commands(token, commands)
            .await
            .map_err(|e| crate::QueryErr::UnexpectedValue {
                message: format!("Todoist push failed for task {task_id}: {e}"),
            })?;
        self.link_task(
            integration_id,
            external_id,
            task_id,
            Some(jiff::Timestamp::now()),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_roundtrip() {
        assert_eq!(priority_for_urgency(2.0), 4);
        assert_eq!(priority_for_urgency(1.5), 3);
        assert_eq!(priority_for_urgency(1.25), 2);
        assert_eq!(priority_for_urgency(1.0), 1);
        assert_eq!(priority_for_urgency(0.5), 1);
        // Round-trips through the import mapping.
        for priority in 1..=4 {
            assert_eq!(priority_for_urgency(urgency_for_priority(priority)), priority);
        }
    }

    #[test]
    fn test_due_date_format() {
        let stamp: jiff::Timestamp = "2026-09-12T00:00:00Z".parse().unwrap();
        assert_eq!(
            due_date_for_deadline(stamp.as_second() as u64).as_deref(),
            Some("2026-09-12")
        );
    }

    #[test]
    fn test_sync_command_shape() {
        let command = sync_command("item_update", serde_json::json!({"id": "x"}));
        assert_eq!(command.get("type").and_then(|t| t.as_str()), Some("item_update"));
        assert!(command.get("uuid").and_then(|u| u.as_str()).is_some_and(|u| !u.is_empty()));
        // UUIDs must differ per command for idempotent retries.
        let other = sync_command("item_update", serde_json::json!({"id": "x"}));
        assert_ne!(command.get("uuid"), other.get("uuid"));
    }

    #[test]
    fn test_urgency_mapping() {
        assert_eq!(urgency_for_priority(4), 2.0);
        assert_eq!(urgency_for_priority(3), 1.5);
        assert_eq!(urgency_for_priority(2), 1.25);
        assert_eq!(urgency_for_priority(1), 0.5);
        assert_eq!(urgency_for_priority(0), 0.5);
    }

    #[test]
    fn test_deadline_parsing() {
        let day = RemoteDue {
            date: "2026-09-12".to_string(),
            datetime: None,
            timezone: None,
            is_recurring: false,
        };
        let stamp = deadline_for_due(&day).unwrap();
        let back = jiff::Timestamp::from_second(stamp as i64).unwrap();
        assert_eq!(back.to_string(), "2026-09-12T00:00:00Z");

        let timed = RemoteDue {
            date: "2026-09-12".to_string(),
            datetime: Some("2026-09-12T15:30:00Z".to_string()),
            timezone: None,
            is_recurring: false,
        };
        assert!(deadline_for_due(&timed).unwrap() > stamp);

        let broken = RemoteDue {
            date: "not-a-date".to_string(),
            datetime: Some("also-broken".to_string()),
            timezone: None,
            is_recurring: false,
        };
        assert!(deadline_for_due(&broken).is_none());
    }

    #[tokio::test]
    async fn test_push_skips_seed_tasks() -> anyhow::Result<()> {
        use crate::TodoStore;
        let mut storage = TodoStore::for_test().await?;
        // Bogus token: a seed task must return before any network happens.
        let seed = storage
            .create_task(crate::Task::create().title("seed").is_seed(true))
            .await?;
        storage
            .push_todoist_patch("bogus", 1, "ext-1", seed.id, &TaskPatch {
                content: Some("changed".to_string()),
                ..Default::default()
            })
            .await?;
        // A real task with the same bogus token fails at the network —
        // proving the guard above is what skipped the seed row.
        let live = storage
            .create_task(crate::Task::create().title("live"))
            .await?;
        assert!(
            storage
                .push_todoist_patch("bogus", 1, "ext-2", live.id, &TaskPatch {
                    content: Some("changed".to_string()),
                    ..Default::default()
                })
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_section_tag_link_roundtrip() -> anyhow::Result<()> {
        use crate::TodoStore;
        let mut storage = TodoStore::for_test().await?;
        let integration = storage.create_integration("todoist", None).await?;
        let project = storage.create_tag("Work").await?;
        let section = RemoteSection {
            id: "sec-1".to_string(),
            name: "Backlog".to_string(),
        };
        let first = storage
            .todoist_section_tag(integration.id, project.id, &section)
            .await?;
        // Re-sync reuses the linked tag instead of duplicating.
        let second = storage
            .todoist_section_tag(integration.id, project.id, &section)
            .await?;
        assert_eq!(first, second);
        let link = storage.tag_link(integration.id, "sec-1").await?.unwrap();
        assert_eq!(link.source_kind, "section");
        // Section tag implies the project tag.
        let parents = storage.get_parents(first).await?;
        assert!(parents.iter().any(|t| t.id == project.id));
        Ok(())
    }
}
