use crate::prelude::*;

struct SeedTask {
    title: String,
    description: Option<String>,
    branch_name: Option<String>,
    labels: Vec<String>,
    deadline: Option<u64>,
    importance_factor: f64,
    urgency_factor: f64,
    parent_title: Option<String>,
}

struct SeedTag {
    name: String,
    implies: Option<String>,
}

struct SeedAssignment {
    task_title: String,
    tag_name: String,
}

fn seed_tags() -> Vec<SeedTag> {
    vec![
        SeedTag {
            name: "work".to_string(),
            implies: None,
        },
        SeedTag {
            name: "programming".to_string(),
            implies: Some("work".to_string()),
        },
        SeedTag {
            name: "backend".to_string(),
            implies: Some("programming".to_string()),
        },
        SeedTag {
            name: "frontend".to_string(),
            implies: Some("programming".to_string()),
        },
        SeedTag {
            name: "rust".to_string(),
            implies: Some("backend".to_string()),
        },
        SeedTag {
            name: "personal".to_string(),
            implies: None,
        },
        SeedTag {
            name: "health".to_string(),
            implies: Some("personal".to_string()),
        },
        SeedTag {
            name: "life-admin".to_string(),
            implies: Some("personal".to_string()),
        },
    ]
}

fn seed_tasks() -> Vec<SeedTask> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    vec![
        SeedTask {
            title: "Implement task CRUD".to_string(),
            description: Some("Add create, read, update, delete operations for tasks".to_string()),
            branch_name: Some("feat/task-crud".to_string()),
            labels: vec!["feature".to_string(), "core".to_string()],
            deadline: Some(now_secs + 3 * 86400),
            importance_factor: 2.0,
            urgency_factor: 1.5,
            parent_title: None,
        },
        SeedTask {
            title: "Add tag filtering".to_string(),
            description: Some("Filter tasks by tags in the list view".to_string()),
            branch_name: Some("feat/tag-filter".to_string()),
            labels: vec!["feature".to_string()],
            deadline: Some(now_secs + 7 * 86400),
            importance_factor: 1.5,
            urgency_factor: 1.0,
            parent_title: None,
        },
        SeedTask {
            title: "Fix date display".to_string(),
            description: Some("Dates show as timestamps instead of formatted strings".to_string()),
            branch_name: Some("fix/date-display".to_string()),
            labels: vec!["bug".to_string()],
            deadline: Some(now_secs + 86400),
            importance_factor: 1.0,
            urgency_factor: 2.0,
            parent_title: None,
        },
        SeedTask {
            title: "Write migration tests".to_string(),
            description: None,
            branch_name: None,
            labels: vec!["testing".to_string()],
            deadline: None,
            importance_factor: 1.0,
            urgency_factor: 0.5,
            parent_title: None,
        },
        SeedTask {
            title: "Plan weekend trip".to_string(),
            description: Some("Research hotels and activities for the mountain trip".to_string()),
            branch_name: None,
            labels: vec!["vacation".to_string()],
            deadline: Some(now_secs + 14 * 86400),
            importance_factor: 0.5,
            urgency_factor: 0.5,
            parent_title: None,
        },
        SeedTask {
            title: "Grocery shopping".to_string(),
            description: Some("Weekly groceries and household items".to_string()),
            branch_name: None,
            labels: vec!["errands".to_string()],
            deadline: Some(now_secs + 2 * 86400),
            importance_factor: 1.0,
            urgency_factor: 1.0,
            parent_title: None,
        },
        SeedTask {
            title: "Migrate database schema".to_string(),
            description: Some("Add indexes for query performance".to_string()),
            branch_name: Some("chore/db-indexes".to_string()),
            labels: vec!["infrastructure".to_string()],
            deadline: Some(now_secs + 5 * 86400),
            importance_factor: 1.5,
            urgency_factor: 1.0,
            parent_title: Some("Implement task CRUD".to_string()),
        },
        SeedTask {
            title: "Code review PRs".to_string(),
            description: Some("Review open pull requests from the team".to_string()),
            branch_name: None,
            labels: vec!["chore".to_string()],
            deadline: Some(now_secs + 86400),
            importance_factor: 1.0,
            urgency_factor: 1.5,
            parent_title: None,
        },
    ]
}

fn seed_assignments() -> Vec<SeedAssignment> {
    vec![
        SeedAssignment {
            task_title: "Implement task CRUD".to_string(),
            tag_name: "backend".to_string(),
        },
        SeedAssignment {
            task_title: "Add tag filtering".to_string(),
            tag_name: "frontend".to_string(),
        },
        SeedAssignment {
            task_title: "Fix date display".to_string(),
            tag_name: "frontend".to_string(),
        },
        SeedAssignment {
            task_title: "Write migration tests".to_string(),
            tag_name: "rust".to_string(),
        },
        SeedAssignment {
            task_title: "Plan weekend trip".to_string(),
            tag_name: "personal".to_string(),
        },
        SeedAssignment {
            task_title: "Grocery shopping".to_string(),
            tag_name: "health".to_string(),
        },
        SeedAssignment {
            task_title: "Migrate database schema".to_string(),
            tag_name: "rust".to_string(),
        },
        SeedAssignment {
            task_title: "Code review PRs".to_string(),
            tag_name: "work".to_string(),
        },
    ]
}

impl TodoStore {
    pub async fn seed(&mut self) -> crate::QueryResult<()> {
        let tags = seed_tags();
        let mut tag_map = std::collections::HashMap::new();

        for seed in &tags {
            let tag = self.create_tag(&seed.name).await?;
            tag_map.insert(seed.name.clone(), tag.id);
        }

        for seed in &tags {
            if let Some(ref implies_name) = seed.implies
                && let Some(&implies_id) = tag_map.get(implies_name)
                && let Some(&tag_id) = tag_map.get(&seed.name)
            {
                self.add_tag_implication(tag_id, implies_id).await?;
            }
        }

        let tasks = seed_tasks();
        let mut task_map = std::collections::HashMap::new();

        for seed in &tasks {
            let parent_id = if let Some(ref parent_title) = seed.parent_title {
                task_map.get(parent_title).copied()
            } else {
                None
            };

            let task = self
                .create_task(
                    Task::create()
                        .title(seed.title.clone())
                        .description(seed.description.clone())
                        .branch_name(seed.branch_name.clone())
                        .labels(Some(toasty::Json(seed.labels.clone())))
                        .deadline(seed.deadline)
                        .importance_factor(seed.importance_factor)
                        .urgency_factor(seed.urgency_factor)
                        .parent_id(parent_id),
                )
                .await?;

            task_map.insert(seed.title.clone(), task.id);
        }

        let assignments = seed_assignments();
        for assignment in &assignments {
            if let Some(&task_id) = task_map.get(&assignment.task_title)
                && let Some(&tag_id) = tag_map.get(&assignment.tag_name)
            {
                self.assign_tag_to_task(task_id, tag_id).await?;
            }
        }

        tracing::info!(
            tasks = task_map.len(),
            tags = tag_map.len(),
            "Seed data inserted"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_seed_data() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        store.seed().await?;

        let tasks = store.list_tasks().await?;
        assert_eq!(tasks.len(), 8);

        let tags = store.list_tags().await?;
        assert_eq!(tags.len(), 8);

        let task_with_parent = tasks.iter().find(|t| t.title == "Migrate database schema");
        assert!(task_with_parent.is_some());
        assert!(task_with_parent.unwrap().parent_id.is_some());

        Ok(())
    }
}
