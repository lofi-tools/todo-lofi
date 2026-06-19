use crate::domain::*;
use crate::error::Result;
use crate::error::SymphonyError::*;
use reqwest::Client;
use std::collections::HashMap;
use std::time::SystemTime;
use time::OffsetDateTime;
use url::Url;

/// Linear API client for fetching issues.
pub struct LinearTracker {
    client: Client,
    endpoint: Url,
    api_key: String,
    project_slug: String,
    active_states: Vec<String>,
    terminal_states: Vec<String>,
}

impl LinearTracker {
    /// Create a new Linear tracker instance.
    pub fn new(
        endpoint: String,
        api_key: String,
        project_slug: String,
        active_states: Vec<String>,
        terminal_states: Vec<String>,
    ) -> Result<Self> {
        let endpoint_url = Url::parse(&endpoint)
            .map_err(|e| ConfigValidation { message: format!("Invalid tracker endpoint URL: {}", e) })?;
        
        Ok(Self {
            client: Client::new(),
            endpoint: endpoint_url,
            api_key,
            project_slug,
            active_states: active_states.iter().map(|s| s.to_lowercase()).collect(),
            terminal_states: terminal_states.iter().map(|s| s.to_lowercase()).collect(),
        })
    }
}

/// Trait defining the tracker interface that Symphony expects.
#[async_trait::async_trait]
pub trait IssueTracker: Send + Sync {
    /// Fetch candidate issues in active states for the configured project.
    async fn fetch_candidate_issues(&self) -> Result<Vec<Issue>>;
    
    /// Fetch issues by their states (used for startup terminal cleanup).
    async fn fetch_issues_by_states(&self, state_names: Vec<String>) -> Result<Vec<Issue>>;
    
    /// Fetch current states for specific issue IDs (used for reconciliation).
    async fn fetch_issue_states_by_ids(&self, issue_ids: Vec<String>) -> Result<HashMap<String, String>>;
}

#[async_trait::async_trait]
impl IssueTracker for LinearTracker {
    async fn fetch_candidate_issues(&self) -> Result<Vec<Issue>> {
        // Linear GraphQL query for issues in a project with pagination
        let query = r#"
            query($projectId: String!, $cursor: String) {
                project(id: $projectId) {
                    issues(first: 50, after: $cursor) {
                        nodes {
                            id
                            identifier
                            title
                            description
                            priority
                            state {
                                name
                            }
                            branchName
                            url
                            labels {
                                nodes {
                                    name
                                }
                            }
                            createdAt
                            updatedAt
                            blockedBy {
                                nodes {
                                    id
                                    identifier
                                    state {
                                        name
                                    }
                                }
                            }
                        }
                        pageInfo {
                            endCursor
                            hasNextPage
                        }
                    }
                }
            }
        "#;
        
        // First we need to get the project ID from the slug
        let project_query = r#"
            query($slug: String!) {
                projectSlugLookup(slug: $slug) {
                    id
                }
            }
        "#;
        
        let project_lookup: serde_json::Value = self
            .client
            .post(self.endpoint.as_str())
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "query": project_query,
                "variables": {
                    "slug": self.project_slug
                }
            }))
            .send()
            .await
            .map_err(|e| LinearApiRequest { source: Box::new(e) })?
            .json()
            .await
            .map_err(|e| LinearApiRequest { source: Box::new(e) })?;
            
        // Check for GraphQL errors
        if let Some(errors) = project_lookup.get("errors") {
            return Err(LinearGraphqlErrors {
                errors: errors.as_array().unwrap_or(&vec![])
                    .iter()
                    .map(|e| e.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown error").to_string())
                    .collect()
            });
        }
        
        let project_id = project_lookup
            .get("data")
            .and_then(|d| d.get("projectSlugLookup"))
            .and_then(|p| p.get("id"))
            .and_then(|id| id.as_str())
            .ok_or_else(|| LinearUnknownPayload {
                payload: serde_json::to_string(&project_lookup).unwrap_or_default()
            })?;
        
        // Now fetch issues with pagination
        let mut all_issues = Vec::new();
        let mut cursor: Option<String> = None;
        let mut has_next_page = true;
        
        while has_next_page {
            let variables = serde_json::json!({
                "projectId": project_id,
                "cursor": cursor
            });
            
            let response: serde_json::Value = self
                .client
                .post(self.endpoint.as_str())
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&serde_json::json!({
                    "query": query,
                    "variables": variables
                }))
                .send()
                .await
                .map_err(|e| LinearApiRequest { source: Box::new(e) })?
                .json()
                .await
                .map_err(|e| LinearApiRequest { source: Box::new(e) })?;
                
            // Check for GraphQL errors
            if let Some(errors) = response.get("errors") {
                return Err(LinearGraphqlErrors {
                    errors: errors.as_array().unwrap_or(&vec![])
                        .iter()
                        .map(|e| e.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown error").to_string())
                        .collect()
                });
            }
            
            let data = response
                .get("data")
                .and_then(|d| d.get("project"))
                .ok_or_else(|| LinearUnknownPayload {
                    payload: serde_json::to_string(&response).unwrap_or_default()
                })?;
                
            let issues_data = data
                .get("issues")
                .ok_or_else(|| LinearUnknownPayload {
                    payload: serde_json::to_string(&response).unwrap_or_default()
                })?;
                
            let nodes = issues_data
                .get("nodes")
                .ok_or_else(|| LinearUnknownPayload {
                    payload: serde_json::to_string(&response).unwrap_or_default()
                })?
                .as_array()
                .ok_or_else(|| LinearUnknownPayload {
                    payload: serde_json::to_string(&response).unwrap_or_default()
                })?;
                
            for node in nodes {
                let issue = parse_issue_node(node)?;
                // Filter by active states (case-insensitive)
                let state_lower = issue.state.to_lowercase();
                if self.active_states.contains(&state_lower) 
                    && !self.terminal_states.contains(&state_lower) {
                    all_issues.push(issue);
                }
            }
            
            let page_info = issues_data
                .get("pageInfo")
                .ok_or_else(|| LinearUnknownPayload {
                    payload: serde_json::to_string(&response).unwrap_or_default()
                })?;
                
            has_next_page = page_info
                .get("hasNextPage")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
                
            cursor = page_info
                .get("endCursor")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
        
        Ok(all_issues)
    }
    
    async fn fetch_issues_by_states(&self, state_names: Vec<String>) -> Result<Vec<Issue>> {
        // Similar to fetch_candidate_issues but filter by specific states
        let mut issues = self.fetch_candidate_issues().await?;
        
        // Filter by the requested states
        let state_set: std::collections::HashSet<String> = state_names
            .into_iter()
            .map(|s| s.to_lowercase())
            .collect();
            
        issues.retain(|issue| {
            state_set.contains(&issue.state.to_lowercase())
        });
        
        Ok(issues)
    }
    
    async fn fetch_issue_states_by_ids(&self, issue_ids: Vec<String>) -> Result<HashMap<String, String>> {
        if issue_ids.is_empty() {
            return Ok(HashMap::new());
        }
        
        // Linear doesn't have a direct batch lookup by IDs, so we'll fetch issues for the project
        // and filter
        // In a production implementation, you might want to optimize this
        let all_issues = self.fetch_candidate_issues().await?;
        
        let mut state_map = HashMap::new();
        for issue in all_issues {
            if issue_ids.contains(&issue.id) {
                state_map.insert(issue.id, issue.state);
            }
        }
        
        Ok(state_map)
    }
}

fn parse_issue_node(node: &serde_json::Value) -> Result<Issue> {
    let id = node.get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LinearUnknownPayload {
            payload: serde_json::to_string(node).unwrap_or_default()
        })?
        .to_string();
    
    let identifier = node.get("identifier")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LinearUnknownPayload {
            payload: serde_json::to_string(node).unwrap_or_default()
        })?
        .to_string();
    
    let title = node.get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LinearUnknownPayload {
            payload: serde_json::to_string(node).unwrap_or_default()
        })?
        .to_string();
    
    let description = node.get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    
    let priority = node.get("priority")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    
    let state = node.get("state")
        .and_then(|s| s.get("name"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| LinearUnknownPayload {
            payload: serde_json::to_string(node).unwrap_or_default()
        })?
        .to_string();
    
    let branch_name = node.get("branchName")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    
    let url = node.get("url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    
    // Parse labels
    let mut labels = Vec::new();
    if let Some(label_nodes) = node.get("labels")
        .and_then(|l| l.get("nodes"))
        .and_then(|n| n.as_array()) {
        for label in label_nodes {
            if let Some(name) = label.get("name").and_then(|v| v.as_str()) {
                labels.push(name.to_lowercase());
            }
        }
    }
    
    // Parse blockedBy relationships
    let mut blocked_by = Vec::new();
    if let Some(blocked_nodes) = node.get("blockedBy")
        .and_then(|b| b.get("nodes"))
        .and_then(|n| n.as_array()) {
        for blocker in blocked_nodes {
            let blocker_id = blocker.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
            let blocker_identifier = blocker.get("identifier").and_then(|v| v.as_str()).map(|s| s.to_string());
            let blocker_state = blocker.get("state")
                .and_then(|s| s.get("name"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            
            blocked_by.push(BlockerRef {
                id: blocker_id,
                identifier: blocker_identifier,
                state: blocker_state,
            });
        }
    }
    
    // Parse timestamps
    let created_at = node.get("createdAt")
        .and_then(|v| v.as_str())
        .and_then(|s| OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok())
        .map(SystemTime::from);
    
    let updated_at = node.get("updatedAt")
        .and_then(|v| v.as_str())
        .and_then(|s| OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok())
        .map(SystemTime::from);
    
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
        created_at,
        updated_at,
    })
}

// We need to add the date_time dependency to Cargo.toml
// For now, we'll use a simple placeholder that always returns None
// In a real implementation, you'd use the chrono or time crate

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_issue_node() {
        let json = serde_json::json!({
            "id": "issue-123",
            "identifier": "TEST-123",
            "title": "Test Issue",
            "description": "This is a test issue",
            "priority": 2,
            "state": { "name": "In Progress" },
            "branchName": "feature/test",
            "url": "https://linear.app/test/issue/TEST-123",
            "labels": {
                "nodes": [
                    { "name": "frontend" },
                    { "name": "bug" }
                ]
            },
            "createdAt": "2023-01-01T10:00:00Z",
            "updatedAt": "2023-01-02T10:00:00Z",
            "blockedBy": {
                "nodes": [
                    {
                        "id": "issue-456",
                        "identifier": "TEST-456",
                        "state": { "name": "Todo" }
                    }
                ]
            }
        });
        
        let issue = parse_issue_node(&json).unwrap();
        
        assert_eq!(issue.id, "issue-123");
        assert_eq!(issue.identifier, "TEST-123");
        assert_eq!(issue.title, "Test Issue");
        assert_eq!(issue.description, Some("This is a test issue".to_string()));
        assert_eq!(issue.priority, Some(2));
        assert_eq!(issue.state, "In Progress");
        assert_eq!(issue.branch_name, Some("feature/test".to_string()));
        assert_eq!(issue.url, Some("https://linear.app/test/issue/TEST-123".to_string()));
        assert_eq!(issue.labels, vec!["frontend", "bug"]);
        assert_eq!(issue.blocked_by.len(), 1);
        assert_eq!(issue.blocked_by[0].id.as_ref().unwrap(), "issue-456");
        assert_eq!(issue.blocked_by[0].identifier.as_ref().unwrap(), "TEST-456");
        assert_eq!(issue.blocked_by[0].state.as_ref().unwrap(), "Todo");
        // Timestamps would be parsed if we had a proper date parser
    }
}