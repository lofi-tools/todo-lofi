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
            name: "ops".to_string(),
            implies: Some("work".to_string()),
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
        SeedTask {
            title: "Deploy to production".to_string(),
            description: Some("Ship the reviewed changes to the production environment".to_string()),
            branch_name: Some("release/prod".to_string()),
            labels: vec!["release".to_string()],
            deadline: Some(now_secs + 2 * 86400),
            importance_factor: 1.5,
            urgency_factor: 1.5,
            parent_title: None,
        },
        SeedTask {
            title: "feed dorito".to_string(),
            description: Some("Feed the cat every evening".to_string()),
            branch_name: None,
            labels: vec!["chore".to_string()],
            deadline: None,
            importance_factor: 1.0,
            urgency_factor: 1.0,
            parent_title: None,
        },
        // Parent with three subtasks, demoing the collapsed subtask row:
        // the first subtask renders inline right of the title and the
        // "0/3" counter expands the full list.
        SeedTask {
            title: "Rewrite the details panel".to_string(),
            description: Some("Redesign the task details panel for the new task view".to_string()),
            branch_name: Some("feat/details-redesign".to_string()),
            labels: vec!["feature".to_string()],
            deadline: Some(now_secs + 10 * 86400),
            importance_factor: 1.5,
            urgency_factor: 0.5,
            parent_title: None,
        },
        SeedTask {
            title: "Sketch the new layout".to_string(),
            description: Some("Wireframe the sidebar and content area".to_string()),
            branch_name: None,
            labels: vec!["design".to_string()],
            deadline: Some(now_secs + 10 * 86400),
            importance_factor: 1.0,
            urgency_factor: 0.5,
            parent_title: Some("Rewrite the details panel".to_string()),
        },
        SeedTask {
            title: "Implement the sidebar".to_string(),
            description: Some("Build the attribute sidebar with edit controls".to_string()),
            branch_name: Some("feat/details-sidebar".to_string()),
            labels: vec!["frontend".to_string()],
            deadline: Some(now_secs + 11 * 86400),
            importance_factor: 1.5,
            urgency_factor: 0.5,
            parent_title: Some("Rewrite the details panel".to_string()),
        },
        SeedTask {
            title: "Migrate existing actions".to_string(),
            description: Some("Move repeat, blocker and follow-up actions into the new panel".to_string()),
            branch_name: None,
            labels: vec!["refactor".to_string()],
            deadline: Some(now_secs + 12 * 86400),
            importance_factor: 1.0,
            urgency_factor: 0.5,
            parent_title: Some("Rewrite the details panel".to_string()),
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
        SeedAssignment {
            task_title: "Deploy to production".to_string(),
            tag_name: "ops".to_string(),
        },
        SeedAssignment {
            task_title: "feed dorito".to_string(),
            tag_name: "personal".to_string(),
        },
        SeedAssignment {
            task_title: "Rewrite the details panel".to_string(),
            tag_name: "frontend".to_string(),
        },
        SeedAssignment {
            task_title: "Sketch the new layout".to_string(),
            tag_name: "frontend".to_string(),
        },
        SeedAssignment {
            task_title: "Implement the sidebar".to_string(),
            tag_name: "frontend".to_string(),
        },
        SeedAssignment {
            task_title: "Migrate existing actions".to_string(),
            tag_name: "frontend".to_string(),
        },
    ]
}

impl TodoStore {
    pub async fn seed(&mut self) -> crate::QueryResult<()> {
        if !self.list_tasks().await?.is_empty() {
            tracing::info!("Database already seeded, skipping");
            return Ok(());
        }
        if !self.list_tags().await?.is_empty() {
            tracing::info!("Tags already seeded, skipping");
            return Ok(());
        }

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
            if let Some(&task_id) = task_map.get(&assignment.task_title) {
                self.assign_tag_to_task(task_id, &assignment.tag_name)
                    .await?;
            }
        }

        // "Write migration tests" and "Add tag filtering" can't start
        // until the task CRUD work is done, so both are blocked by
        // "Implement task CRUD" (demoing the "blocks 2" chip in the task
        // list).
        if let Some(&crud_id) = task_map.get("Implement task CRUD") {
            for dependent in ["Write migration tests", "Add tag filtering"] {
                if let Some(&dependent_id) = task_map.get(dependent) {
                    self.add_blocker(dependent_id, crud_id).await?;
                }
            }
        }

        // "Deploy to production" must wait for the code review, so it is
        // blocked by exactly one task ("Code review PRs") — demoing the
        // inline arrow chain (single blocker) in the task list.
        if let (Some(&review_id), Some(&deploy_id)) =
            (task_map.get("Code review PRs"), task_map.get("Deploy to production"))
        {
            self.add_blocker(deploy_id, review_id).await?;
        }

        // "feed dorito" repeats every day at 6pm.
        if let Some(&task_id) = task_map.get("feed dorito") {
            self.set_repeat(task_id, "feed dorito".to_string(), 1, Some(18 * 60))
                .await?;
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
        assert_eq!(tasks.len(), 14);

        let tags = store.list_tags().await?;
        assert_eq!(tags.len(), 9);

        let task_with_parent = tasks.iter().find(|t| t.title == "Migrate database schema");
        assert!(task_with_parent.is_some());
        assert!(task_with_parent.unwrap().parent_id.is_some());

        // "Rewrite the details panel" has three subtasks, demoing the
        // collapsed subtask row and the N/M counter.
        let rewrite = tasks
            .iter()
            .find(|t| t.title == "Rewrite the details panel")
            .unwrap();
        let subtask_map = store.subtasks_map(&[rewrite.id]).await?;
        let rewrite_subtasks = &subtask_map[&rewrite.id];
        assert_eq!(rewrite_subtasks.len(), 3);
        assert!(
            rewrite_subtasks.iter().all(|t| t.parent_id == Some(rewrite.id)),
            "all three subtasks must point back at the parent"
        );

        // "feed dorito" repeats every day at 6pm.
        let feed = tasks.iter().find(|t| t.title == "feed dorito").unwrap();
        let template = store.repeat_template_for_task(feed.id).await?.unwrap();
        assert_eq!(template.interval_days, 1);
        assert_eq!(template.time_of_day, Some(18 * 60));

        // "Write migration tests" and "Add tag filtering" are blocked by
        // "Implement task CRUD" (the blocked flag is computed by the meta
        // loader).
        let with_meta = store.list_tasks_by_priority().await?;
        let migration_tests = with_meta
            .iter()
            .find(|t| t.title == "Write migration tests")
            .unwrap();
        assert!(migration_tests.blocked, "migration tests should be blocked");
        let tag_filtering = with_meta
            .iter()
            .find(|t| t.title == "Add tag filtering")
            .unwrap();
        assert!(tag_filtering.blocked, "tag filtering should be blocked");
        let blockers = store.list_blockers(migration_tests.id).await?;
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].title, "Implement task CRUD");

        // "Implement task CRUD" blocks exactly those two tasks.
        let blocking = store.blocking_map(&[blockers[0].id]).await?;
        let blocked_titles: Vec<&str> = blocking[&blockers[0].id]
            .iter()
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(blocked_titles, vec!["Write migration tests", "Add tag filtering"]);

        // "Deploy to production" is blocked by exactly one task ("Code
        // review PRs"), so it demos the inline arrow chain.
        let deploy = with_meta
            .iter()
            .find(|t| t.title == "Deploy to production")
            .unwrap();
        assert!(deploy.blocked, "deploy should be blocked by the review");
        let deploy_blockers = store.list_blockers(deploy.id).await?;
        assert_eq!(deploy_blockers.len(), 1);
        assert_eq!(deploy_blockers[0].title, "Code review PRs");

        // "Code review PRs" blocks exactly that one task.
        let blocking = store.blocking_map(&[deploy_blockers[0].id]).await?;
        let blocked_titles: Vec<&str> = blocking[&deploy_blockers[0].id]
            .iter()
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(blocked_titles, vec!["Deploy to production"]);

        Ok(())
    }
}
