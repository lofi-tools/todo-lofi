use crate::agent::ClientTool;
use crate::config::TrackerConfig;
use crate::domain::*;
use crate::error::Result;
use crate::error::SymphonyError::*;
use log::debug;
use reqwest::Client;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Linear page size (Section 11.2).
pub const PAGE_SIZE: usize = 50;
/// Linear network timeout in milliseconds (Section 11.2).
pub const NETWORK_TIMEOUT_MS: u64 = 30_000;

/// GraphQL selection set for a normalized issue.
const ISSUE_FIELDS: &str = r#"
    id
    identifier
    title
    description
    priority
    state { name }
    branchName
    url
    labels { nodes { name } }
    createdAt
    updatedAt
    inverseRelations { nodes { type issue { id identifier state { name } } } }
"#;

/// Candidate issue query: filters the project by `slugId` (Section 11.2).
fn candidate_issues_query() -> String {
    format!(
        r#"query CandidateIssues($projectSlug: String!, $states: [String!]!, $first: Int!, $after: String) {{
  issues(first: $first, after: $after, filter: {{ project: {{ slugId: {{ eq: $projectSlug }} }}, state: {{ name: {{ in: $states }} }} }}) {{
    nodes {{{ISSUE_FIELDS}}}
    pageInfo {{ hasNextPage endCursor }}
  }}
}}"#
    )
}

/// Issues-by-state query, used for startup terminal cleanup (Section 11.1).
fn issues_by_states_query() -> String {
    format!(
        r#"query IssuesByStates($projectSlug: String!, $states: [String!]!, $first: Int!, $after: String) {{
  issues(first: $first, after: $after, filter: {{ project: {{ slugId: {{ eq: $projectSlug }} }}, state: {{ name: {{ in: $states }} }} }}) {{
    nodes {{{ISSUE_FIELDS}}}
    pageInfo {{ hasNextPage endCursor }}
  }}
}}"#
    )
}

/// Issue-state refresh query: GraphQL issue IDs typed as `[ID!]` (Section 11.2).
fn issue_states_by_ids_query() -> String {
    format!(
        r#"query IssueStatesByIds($ids: [ID!]!, $first: Int!) {{
  issues(first: $first, filter: {{ id: {{ in: $ids }} }}) {{
    nodes {{{ISSUE_FIELDS}}}
  }}
}}"#
    )
}

/// Trait defining the tracker interface Symphony expects (Section 11.1).
#[async_trait::async_trait]
pub trait IssueTracker: Send + Sync {
    /// Fetch candidate issues in active states for the configured project.
    async fn fetch_candidate_issues(&self) -> Result<Vec<Issue>>;

    /// Fetch issues in the given states (used for startup terminal cleanup).
    async fn fetch_issues_by_states(&self, state_names: Vec<String>) -> Result<Vec<Issue>>;

    /// Fetch current normalized issues for specific issue IDs (reconciliation).
    async fn fetch_issue_states_by_ids(&self, issue_ids: Vec<String>) -> Result<Vec<Issue>>;
}

/// Linear API client for fetching issues.
pub struct LinearTracker {
    client: Client,
    endpoint: String,
    api_key: String,
    project_slug: String,
    active_states: Vec<String>,
    terminal_states: Vec<String>,
}

impl LinearTracker {
    /// Build a tracker from a validated tracker configuration.
    pub fn from_config(config: &TrackerConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_millis(NETWORK_TIMEOUT_MS))
            .build()
            .map_err(|error| LinearApiRequest {
                source: Box::new(error),
            })?;

        Ok(Self {
            client,
            endpoint: config.endpoint.clone(),
            api_key: config.api_key.clone(),
            project_slug: config.project_slug.clone(),
            active_states: config.active_states.clone(),
            terminal_states: config.terminal_states.clone(),
        })
    }

    /// The configured endpoint.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Execute a raw GraphQL document against the configured Linear endpoint.
    ///
    /// Used both by the tracker queries and the `linear_graphql` client tool.
    pub async fn execute_graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let response = self
            .client
            .post(&self.endpoint)
            .header("Authorization", self.api_key.as_str())
            .header("Content-Type", "application/json")
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await
            .map_err(|error| LinearApiRequest {
                source: Box::new(error),
            })?;

        let status = response.status();
        let body = response.text().await.map_err(|error| LinearApiRequest {
            source: Box::new(error),
        })?;

        if !status.is_success() {
            return Err(LinearApiStatus {
                status: status.as_u16(),
                body: truncate(&body, 500),
            });
        }

        let payload: Value = serde_json::from_str(&body).map_err(|_| LinearUnknownPayload {
            payload: truncate(&body, 500),
        })?;

        Ok(payload)
    }

    async fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let payload = self.execute_graphql(query, variables).await?;

        if let Some(errors) = payload.get("errors") {
            return Err(LinearGraphqlErrors {
                errors: graphql_error_messages(errors),
            });
        }

        let data = payload.get("data").cloned().ok_or_else(|| LinearUnknownPayload {
            payload: truncate(&payload.to_string(), 500),
        })?;

        Ok(data)
    }

    /// Page through an `issues` connection built from the supplied query.
    async fn fetch_issues_paginated(&self, query: &str, state_names: Vec<String>) -> Result<Vec<Issue>> {
        let mut issues = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let variables = json!({
                "projectSlug": self.project_slug,
                "states": state_names,
                "first": PAGE_SIZE,
                "after": cursor,
            });

            let data = self.graphql(query, variables).await?;
            let connection = data.get("issues").ok_or_else(|| LinearUnknownPayload {
                payload: truncate(&data.to_string(), 500),
            })?;

            issues.extend(parse_issue_nodes(connection.get("nodes"))?);

            let page_info = connection
                .get("pageInfo")
                .ok_or_else(|| LinearUnknownPayload {
                    payload: truncate(&data.to_string(), 500),
                })?;
            let has_next_page = page_info
                .get("hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            if !has_next_page {
                break;
            }

            // A page that claims more results without a cursor would loop forever.
            cursor = page_info
                .get("endCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                return Err(LinearMissingEndCursor);
            }
        }

        Ok(issues)
    }

    fn is_candidate(&self, issue: &Issue) -> bool {
        let state = issue.normalized_state();
        self.active_states
            .iter()
            .any(|active| normalize_state(active) == state)
            && !self
                .terminal_states
                .iter()
                .any(|terminal| normalize_state(terminal) == state)
    }
}

#[async_trait::async_trait]
impl IssueTracker for LinearTracker {
    async fn fetch_candidate_issues(&self) -> Result<Vec<Issue>> {
        let mut issues = self
            .fetch_issues_paginated(&candidate_issues_query(), self.active_states.clone())
            .await?;

        // The tracker filter is authoritative, but terminal states still win locally.
        issues.retain(|issue| self.is_candidate(issue));
        issues.sort_by(|left, right| left.identifier.cmp(&right.identifier));
        Ok(issues)
    }

    async fn fetch_issues_by_states(&self, state_names: Vec<String>) -> Result<Vec<Issue>> {
        if state_names.is_empty() {
            return Ok(Vec::new());
        }
        self.fetch_issues_paginated(&issues_by_states_query(), state_names)
            .await
    }

    async fn fetch_issue_states_by_ids(&self, issue_ids: Vec<String>) -> Result<Vec<Issue>> {
        if issue_ids.is_empty() {
            return Ok(Vec::new());
        }

        let first = issue_ids.len().clamp(1, 250);
        let variables = json!({ "ids": issue_ids, "first": first });
        let data = self
            .graphql(&issue_states_by_ids_query(), variables)
            .await?;

        let connection = data.get("issues").ok_or_else(|| LinearUnknownPayload {
            payload: truncate(&data.to_string(), 500),
        })?;

        parse_issue_nodes(connection.get("nodes"))
    }
}

fn graphql_error_messages(errors: &Value) -> Vec<String> {
    match errors.as_array() {
        Some(errors) => errors
            .iter()
            .map(|error| {
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown GraphQL error")
                    .to_string()
            })
            .collect(),
        None => vec![errors.to_string()],
    }
}

fn parse_issue_nodes(nodes: Option<&Value>) -> Result<Vec<Issue>> {
    let nodes = nodes.ok_or_else(|| LinearUnknownPayload {
        payload: "issues connection without nodes".to_string(),
    })?;
    let nodes = nodes.as_array().ok_or_else(|| LinearUnknownPayload {
        payload: truncate(&nodes.to_string(), 500),
    })?;

    let mut issues = Vec::with_capacity(nodes.len());
    for node in nodes {
        issues.push(normalize_issue(node)?);
    }
    Ok(issues)
}

/// Normalize a Linear issue node into the stable domain model (Section 11.3).
pub fn normalize_issue(node: &Value) -> Result<Issue> {
    let malformed = || LinearUnknownPayload {
        payload: truncate(&node.to_string(), 500),
    };

    let id = node
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(malformed)?
        .to_string();
    let identifier = node
        .get("identifier")
        .and_then(Value::as_str)
        .ok_or_else(malformed)?
        .to_string();
    let title = node
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(malformed)?
        .to_string();
    let state = node
        .get("state")
        .and_then(|state| state.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(malformed)?
        .to_string();

    let description = node
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);

    // Non-integer priorities normalize to null.
    let priority = node.get("priority").and_then(Value::as_i64).map(|value| value as i32);

    let branch_name = node
        .get("branchName")
        .and_then(Value::as_str)
        .map(str::to_string);
    let url = node.get("url").and_then(Value::as_str).map(str::to_string);

    // Labels are normalized to lowercase.
    let labels = node
        .get("labels")
        .and_then(|labels| labels.get("nodes"))
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| label.get("name").and_then(Value::as_str))
                .map(|name| name.to_lowercase())
                .collect()
        })
        .unwrap_or_default();

    // Blockers come from inverse relations of type `blocks`.
    let blocked_by = node
        .get("inverseRelations")
        .and_then(|relations| relations.get("nodes"))
        .and_then(Value::as_array)
        .map(|relations| {
            relations
                .iter()
                .filter(|relation| {
                    relation
                        .get("type")
                        .and_then(Value::as_str)
                        .map(|kind| kind.eq_ignore_ascii_case("blocks"))
                        .unwrap_or(false)
                })
                .filter_map(|relation| relation.get("issue"))
                .map(|issue| BlockerRef {
                    id: issue.get("id").and_then(Value::as_str).map(str::to_string),
                    identifier: issue
                        .get("identifier")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    state: issue
                        .get("state")
                        .and_then(|state| state.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(Issue {
        id,
        identifier,
        title,
        description,
        priority,
        state,
        branch_name,
        url,
        labels,
        blocked_by,
        created_at: parse_timestamp(node.get("createdAt")),
        updated_at: parse_timestamp(node.get("updatedAt")),
    })
}

fn parse_timestamp(value: Option<&Value>) -> Option<SystemTime> {
    let value = value?.as_str()?;
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .map(SystemTime::from)
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars().take(limit).collect::<String>() + "…"
}

/// The OPTIONAL `linear_graphql` client-side tool extension (Section 10.5).
pub struct LinearGraphqlTool {
    tracker: LinearTracker,
}

impl LinearGraphqlTool {
    /// Build the tool from validated tracker configuration.
    pub fn from_config(config: &TrackerConfig) -> Result<Self> {
        Ok(Self {
            tracker: LinearTracker::from_config(config)?,
        })
    }
}

#[async_trait::async_trait]
impl ClientTool for LinearGraphqlTool {
    fn name(&self) -> &str {
        "linear_graphql"
    }

    fn description(&self) -> &str {
        "Execute one raw GraphQL operation against the configured Linear workspace using Symphony's tracker auth."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "A single GraphQL query or mutation document.",
                    "minLength": 1,
                },
                "variables": {
                    "type": "object",
                    "description": "Optional GraphQL variables object.",
                },
            },
            "required": ["query"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, arguments: Value) -> std::result::Result<Value, String> {
        let query = match &arguments {
            // A raw GraphQL string is accepted as shorthand input.
            Value::String(query) => query.clone(),
            Value::Object(map) => match map.get("query") {
                Some(Value::String(query)) => query.clone(),
                _ => {
                    return Ok(tool_failure(
                        "invalid_arguments",
                        "`query` must be a non-empty string",
                    ));
                }
            },
            _ => {
                return Ok(tool_failure(
                    "invalid_arguments",
                    "expected an object with a `query` string or a raw GraphQL string",
                ));
            }
        };

        let variables = match &arguments {
            Value::Object(map) => match map.get("variables") {
                Some(Value::Object(variables)) => Value::Object(variables.clone()),
                Some(Value::Null) | None => json!({}),
                Some(_) => {
                    return Ok(tool_failure(
                        "invalid_arguments",
                        "`variables` must be a JSON object",
                    ));
                }
            },
            _ => json!({}),
        };

        if query.trim().is_empty() {
            return Ok(tool_failure("invalid_arguments", "`query` must not be empty"));
        }

        if count_graphql_operations(&query) != 1 {
            return Ok(tool_failure(
                "invalid_arguments",
                "the document must contain exactly one GraphQL operation",
            ));
        }

        let payload = match self.tracker.execute_graphql(&query, variables).await {
            Ok(payload) => payload,
            Err(error) => {
                return Ok(tool_failure("request_failed", &error.to_string()));
            }
        };

        match payload.get("errors") {
            Some(errors) => Ok(json!({
                "success": false,
                "errors": errors,
                "data": payload.get("data").cloned().unwrap_or(Value::Null),
            })),
            None => Ok(json!({
                "success": true,
                "data": payload.get("data").cloned().unwrap_or(Value::Null),
            })),
        }
    }
}

fn tool_failure(code: &str, message: &str) -> Value {
    json!({ "success": false, "error": { "code": code, "message": message } })
}

/// Count top-level GraphQL operations in a document.
///
/// `operationName` selection is intentionally out of scope, so a document with
/// more than one operation is rejected as invalid input.
fn count_graphql_operations(document: &str) -> usize {
    let stripped = strip_graphql_noise(document);
    let characters: Vec<char> = stripped.chars().collect();
    let mut operations = 0usize;
    let mut depth = 0i32;
    let mut expects_operation = true;
    let mut index = 0usize;

    while index < characters.len() {
        let character = characters[index];
        if character == '{' {
            // A selection set at the top level is an anonymous shorthand operation.
            if depth == 0 && expects_operation {
                operations += 1;
                expects_operation = false;
            }
            depth += 1;
            index += 1;
            continue;
        }
        if character == '}' {
            depth = (depth - 1).max(0);
            expects_operation = depth == 0;
            index += 1;
            continue;
        }
        if character.is_ascii_alphanumeric() || character == '_' {
            let start = index;
            while index < characters.len()
                && (characters[index].is_ascii_alphanumeric() || characters[index] == '_')
            {
                index += 1;
            }
            let word: String = characters[start..index].iter().collect();
            if depth == 0
                && expects_operation
                && matches!(
                    word.to_ascii_lowercase().as_str(),
                    "query" | "mutation" | "subscription"
                )
            {
                operations += 1;
                expects_operation = false;
            }
            continue;
        }
        index += 1;
    }

    operations
}

/// Remove comments and string literals so brace/keyword scanning is not confused by them.
fn strip_graphql_noise(document: &str) -> String {
    let mut output = String::with_capacity(document.len());
    let mut characters = document.chars().peekable();
    let mut inside_string = false;

    while let Some(character) = characters.next() {
        if inside_string {
            if character == '\\' {
                // Skip the escaped character.
                characters.next();
            } else if character == '"' {
                inside_string = false;
            }
            continue;
        }
        match character {
            '"' => inside_string = true,
            '#' => {
                for next in characters.by_ref() {
                    if next == '\n' {
                        output.push('\n');
                        break;
                    }
                }
            }
            other => output.push(other),
        }
    }

    output
}

/// A tracker that returns a fixed issue set; used by tests and offline runs.
#[derive(Debug, Clone, Default)]
pub struct StaticTracker {
    pub candidates: Vec<Issue>,
    pub states: HashMap<String, String>,
    pub by_states: Vec<Issue>,
}

#[async_trait::async_trait]
impl IssueTracker for StaticTracker {
    async fn fetch_candidate_issues(&self) -> Result<Vec<Issue>> {
        Ok(self.candidates.clone())
    }

    async fn fetch_issues_by_states(&self, state_names: Vec<String>) -> Result<Vec<Issue>> {
        if state_names.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self
            .by_states
            .iter()
            .filter(|issue| state_names.iter().any(|state| state == &issue.state))
            .cloned()
            .collect())
    }

    async fn fetch_issue_states_by_ids(&self, issue_ids: Vec<String>) -> Result<Vec<Issue>> {
        let mut issues: Vec<Issue> = self
            .candidates
            .iter()
            .filter(|issue| issue_ids.contains(&issue.id))
            .cloned()
            .collect();

        for issue in &mut issues {
            if let Some(state) = self.states.get(&issue.id) {
                issue.state = state.clone();
            }
        }

        debug!(
            target: "symphony",
            "requested={} returned={}",
            issue_ids.len(),
            issues.len()
        );

        Ok(issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn tracker_config(endpoint: String) -> TrackerConfig {
        TrackerConfig {
            kind: "linear".to_string(),
            endpoint,
            api_key: "test-key".to_string(),
            project_slug: "proj-slug".to_string(),
            active_states: vec!["Todo".to_string(), "In Progress".to_string()],
            terminal_states: vec!["Done".to_string()],
        }
    }

    fn issue_node() -> Value {
        json!({
            "id": "issue-1",
            "identifier": "MT-1",
            "title": "Fix the thing",
            "description": "Details",
            "priority": 2,
            "state": { "name": "In Progress" },
            "branchName": "mt-1-fix",
            "url": "https://linear.app/x/issue/MT-1",
            "labels": { "nodes": [ { "name": "Bug" }, { "name": "Frontend" } ] },
            "createdAt": "2026-01-02T03:04:05Z",
            "updatedAt": "2026-01-03T03:04:05Z",
            "inverseRelations": {
                "nodes": [
                    { "type": "blocks", "issue": { "id": "issue-2", "identifier": "MT-2", "state": { "name": "Todo" } } },
                    { "type": "related", "issue": { "id": "issue-3", "identifier": "MT-3", "state": { "name": "Done" } } }
                ]
            }
        })
    }

    /// Spawn a one-shot HTTP server that replies with a canned response.
    async fn spawn_stub(response: String) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("stub server binds");
        let address = listener.local_addr().expect("address");
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            let mut request = vec![0u8; 8192];
            let read = socket.read(&mut request).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..read]).into_owned();
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
            socket.flush().await.expect("flush");
            request
        });
        (format!("http://{address}"), handle)
    }

    fn http_ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn candidate_query_filters_project_by_slug_id() {
        let query = candidate_issues_query();
        assert!(query.contains("project: { slugId: { eq: $projectSlug } }"));
        assert!(query.contains("state: { name: { in: $states } }"));
        assert!(query.contains("pageInfo { hasNextPage endCursor }"));
        assert!(query.contains("inverseRelations"));
    }

    #[test]
    fn state_refresh_query_uses_id_list_typing() {
        let query = issue_states_by_ids_query();
        assert!(query.contains("$ids: [ID!]!"));
        assert!(query.contains("id: { in: $ids }"));
    }

    #[test]
    fn normalizes_issue_fields() {
        let issue = normalize_issue(&issue_node()).expect("issue normalizes");

        assert_eq!(issue.id, "issue-1");
        assert_eq!(issue.identifier, "MT-1");
        assert_eq!(issue.state, "In Progress");
        assert_eq!(issue.priority, Some(2));
        assert_eq!(issue.labels, vec!["bug", "frontend"]);
        assert_eq!(issue.branch_name.as_deref(), Some("mt-1-fix"));
        assert!(issue.created_at.is_some());
        assert!(issue.updated_at.is_some());

        // Only inverse relations of type `blocks` become blockers.
        assert_eq!(issue.blocked_by.len(), 1);
        let blocker = &issue.blocked_by[0];
        assert_eq!(blocker.id.as_deref(), Some("issue-2"));
        assert_eq!(blocker.identifier.as_deref(), Some("MT-2"));
        assert_eq!(blocker.state.as_deref(), Some("Todo"));
    }

    #[test]
    fn non_integer_priority_becomes_null() {
        let mut node = issue_node();
        node["priority"] = json!("urgent");
        let issue = normalize_issue(&node).expect("issue normalizes");
        assert_eq!(issue.priority, None);
    }

    #[test]
    fn malformed_issue_payload_is_an_error() {
        let node = json!({ "id": "issue-1" });
        assert!(matches!(
            normalize_issue(&node),
            Err(LinearUnknownPayload { .. })
        ));
    }

    #[tokio::test]
    async fn empty_state_list_skips_the_api_call() {
        // An unroutable endpoint proves no request is attempted.
        let tracker = LinearTracker::from_config(&tracker_config(
            "http://127.0.0.1:1/graphql".to_string(),
        ))
        .expect("tracker builds");

        let issues = tracker
            .fetch_issues_by_states(Vec::new())
            .await
            .expect("empty state list short-circuits");
        assert!(issues.is_empty());

        let issues = tracker
            .fetch_issue_states_by_ids(Vec::new())
            .await
            .expect("empty id list short-circuits");
        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn fetches_candidates_with_auth_and_pagination() {
        // First page reports a next cursor, second page ends the pagination.
        let first_page = json!({
            "data": { "issues": {
                "nodes": [issue_node()],
                "pageInfo": { "hasNextPage": true, "endCursor": "cursor-1" }
            }}
        })
        .to_string();

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for page in 0..2 {
                let (mut socket, _) = listener.accept().await.expect("connection");
                let mut buffer = vec![0u8; 16384];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                requests.push(request);

                let body = if page == 0 {
                    first_page.clone()
                } else {
                    json!({
                        "data": { "issues": {
                            "nodes": [issue_node()],
                            "pageInfo": { "hasNextPage": false, "endCursor": null }
                        }}
                    })
                    .to_string()
                };

                socket
                    .write_all(http_ok(&body).as_bytes())
                    .await
                    .expect("write");
                socket.flush().await.expect("flush");
            }
            requests
        });

        let tracker = LinearTracker::from_config(&tracker_config(format!("http://{address}")))
            .expect("tracker builds");
        let issues = tracker.fetch_candidate_issues().await.expect("fetch succeeds");

        assert_eq!(issues.len(), 2);
        let requests = server.await.expect("server task");
        // Header names are case-insensitive on the wire.
        let first = requests[0].to_lowercase();
        assert!(first.contains("authorization: test-key"), "got: {first}");
        assert!(requests[0].contains("slugId"));
        // The second request continues from the first page's cursor.
        assert!(requests[1].contains("cursor-1"));
    }

    #[tokio::test]
    async fn maps_non_200_status() {
        let (endpoint, server) = spawn_stub(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\nConnection: close\r\n\r\nboom!"
                .to_string(),
        )
        .await;

        let tracker = LinearTracker::from_config(&tracker_config(endpoint)).expect("tracker builds");
        let error = tracker
            .fetch_candidate_issues()
            .await
            .expect_err("status error");
        match error {
            LinearApiStatus { status, .. } => assert_eq!(status, 500),
            other => panic!("unexpected error: {other}"),
        }
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn maps_graphql_errors() {
        let body = json!({ "errors": [ { "message": "bad query" } ] }).to_string();
        let (endpoint, server) = spawn_stub(http_ok(&body)).await;

        let tracker = LinearTracker::from_config(&tracker_config(endpoint)).expect("tracker builds");
        let error = tracker
            .fetch_candidate_issues()
            .await
            .expect_err("graphql error");
        match error {
            LinearGraphqlErrors { errors } => assert_eq!(errors, vec!["bad query".to_string()]),
            other => panic!("unexpected error: {other}"),
        }
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn maps_unknown_payloads() {
        let (endpoint, server) = spawn_stub(http_ok("not json")).await;

        let tracker = LinearTracker::from_config(&tracker_config(endpoint)).expect("tracker builds");
        assert!(matches!(
            tracker.fetch_candidate_issues().await,
            Err(LinearUnknownPayload { .. })
        ));
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn maps_pagination_without_cursor() {
        let body = json!({
            "data": { "issues": {
                "nodes": [],
                "pageInfo": { "hasNextPage": true, "endCursor": null }
            }}
        })
        .to_string();
        let (endpoint, server) = spawn_stub(http_ok(&body)).await;

        let tracker = LinearTracker::from_config(&tracker_config(endpoint)).expect("tracker builds");
        assert!(matches!(
            tracker.fetch_candidate_issues().await,
            Err(LinearMissingEndCursor)
        ));
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn state_refresh_uses_the_configured_tracker() {
        let body = json!({
            "data": { "issues": { "nodes": [ issue_node() ] } }
        })
        .to_string();
        let (endpoint, server) = spawn_stub(http_ok(&body)).await;

        let tracker = LinearTracker::from_config(&tracker_config(endpoint)).expect("tracker builds");
        let issues = tracker
            .fetch_issue_states_by_ids(vec!["issue-1".to_string()])
            .await
            .expect("refresh succeeds");

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].state, "In Progress");

        let request = server.await.expect("server task");
        assert!(request.contains("[ID!]"));
    }

    #[test]
    fn counts_graphql_operations() {
        assert_eq!(count_graphql_operations("query A { a }"), 1);
        assert_eq!(count_graphql_operations("{ a }"), 1);
        assert_eq!(count_graphql_operations("query A { a }\nmutation B { b }"), 2);
        assert_eq!(
            count_graphql_operations("# comment with query braces {}\nquery A { viewer { id } }"),
            1
        );
        assert_eq!(count_graphql_operations("query { a(text: \"mutation X {}\") }"), 1);
    }

    #[tokio::test]
    async fn linear_graphql_tool_executes_and_reports_errors() {
        let body = json!({ "data": { "viewer": { "id": "u1" } } }).to_string();
        let (endpoint, server) = spawn_stub(http_ok(&body)).await;

        let tool = LinearGraphqlTool::from_config(&tracker_config(endpoint)).expect("tool builds");
        assert_eq!(tool.name(), "linear_graphql");
        assert_eq!(tool.input_schema()["required"][0], "query");

        let result = tool
            .call(json!({ "query": "query Viewer { viewer { id } }" }))
            .await
            .expect("tool result");
        assert_eq!(result["success"], true);
        assert_eq!(result["data"]["viewer"]["id"], "u1");
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn linear_graphql_tool_rejects_invalid_input() {
        let tool = LinearGraphqlTool::from_config(&tracker_config(
            "http://127.0.0.1:1/graphql".to_string(),
        ))
        .expect("tool builds");

        let result = tool.call(json!({ "query": "" })).await.expect("result");
        assert_eq!(result["success"], false);
        assert_eq!(result["error"]["code"], "invalid_arguments");

        let result = tool
            .call(json!({ "query": "query A { a } mutation B { b }" }))
            .await
            .expect("result");
        assert_eq!(result["error"]["code"], "invalid_arguments");

        let result = tool.call(json!({ "nope": 1 })).await.expect("result");
        assert_eq!(result["error"]["code"], "invalid_arguments");
    }

    #[tokio::test]
    async fn linear_graphql_tool_preserves_graphql_error_body() {
        let body = json!({
            "data": null,
            "errors": [ { "message": "unknown field" } ]
        })
        .to_string();
        let (endpoint, server) = spawn_stub(http_ok(&body)).await;

        let tool = LinearGraphqlTool::from_config(&tracker_config(endpoint)).expect("tool builds");
        let result = tool
            .call(json!({ "query": "query Viewer { viewer { nope } }" }))
            .await
            .expect("tool result");

        assert_eq!(result["success"], false);
        assert_eq!(result["errors"][0]["message"], "unknown field");
        server.await.expect("server task");
    }
}
