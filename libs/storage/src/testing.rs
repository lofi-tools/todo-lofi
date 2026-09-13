use crate::prelude::*;
use snafu::ResultExt;

struct SeedTask {
    title: String,
    description: Option<String>,
    branch_name: Option<String>,
    labels: Vec<String>,
    deadline: Option<u64>,
    blocked_until: Option<u64>,
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

    /// Next date on a biweekly grid (`anchor` + 14k days) at or after
    /// today, in the system timezone, as a UTC epoch for `minutes` past
    /// midnight. Anchors the "green bin" (Mon Sep 7) and "gray bin"
    /// (Mon Sep 14) cadences.
    fn biweekly_at(anchor: (i16, i8, i8), minutes: u64, now_secs: u64) -> u64 {
        use jiff::ToSpan;
        let zone = jiff::tz::TimeZone::system();
        let today = jiff::Timestamp::from_second(now_secs as i64)
            .unwrap()
            .to_zoned(zone.clone())
            .date();
        let mut date =
            jiff::civil::Date::new(anchor.0, anchor.1, anchor.2).unwrap();
        while date < today {
            date = date.checked_add(14.days()).unwrap();
        }
        date.at((minutes / 60) as i8, (minutes % 60) as i8, 0, 0)
            .to_zoned(zone)
            .map(|zoned| zoned.timestamp().as_second() as u64)
            .unwrap_or(now_secs)
    }

    // Today's `minutes` (since midnight) in the system timezone, as a UTC
    // epoch. Used for the "feed dorito" start (5:50pm) and deadline
    // (6:30pm).
    let today_at = |minutes: u64| {
        let zone = jiff::tz::TimeZone::system();
        let date = jiff::Timestamp::from_second(now_secs as i64)
            .unwrap()
            .to_zoned(zone.clone())
            .date();
        date.at((minutes / 60) as i8, (minutes % 60) as i8, 0, 0)
            .to_zoned(zone)
            .map(|zoned| zoned.timestamp().as_second() as u64)
            .unwrap_or(now_secs)
    };

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
            blocked_until: None,
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
            blocked_until: None,
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
            blocked_until: None,
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
            blocked_until: None,
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
            blocked_until: None,
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
            blocked_until: None,
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
            blocked_until: None,
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
            blocked_until: None,
        },
        SeedTask {
            title: "Deploy to production".to_string(),
            description: Some(
                "Ship the reviewed changes to the production environment".to_string(),
            ),
            branch_name: Some("release/prod".to_string()),
            labels: vec!["release".to_string()],
            deadline: Some(now_secs + 2 * 86400),
            importance_factor: 1.5,
            urgency_factor: 1.5,
            parent_title: None,
            blocked_until: None,
        },
        SeedTask {
            title: "feed dorito".to_string(),
            description: Some("Feed Dexter every evening".to_string()),
            branch_name: None,
            labels: vec!["chore".to_string()],
            deadline: Some(today_at(18 * 60 + 30)),
            importance_factor: 3.0,
            urgency_factor: 1.0,
            parent_title: None,
            blocked_until: Some(today_at(17 * 60 + 50)),
        },
        SeedTask {
            title: "gym".to_string(),
            description: Some("Morning workout".to_string()),
            branch_name: None,
            labels: vec!["health".to_string()],
            deadline: Some(today_at(11 * 60 + 30)),
            importance_factor: 2.0,
            urgency_factor: 1.0,
            parent_title: None,
            blocked_until: Some(today_at(10 * 60)),
        },
        SeedTask {
            title: "shower".to_string(),
            description: Some("Shower after the workout".to_string()),
            branch_name: None,
            labels: vec!["health".to_string()],
            deadline: Some(today_at(12 * 60)),
            importance_factor: 1.0,
            urgency_factor: 1.0,
            parent_title: None,
            blocked_until: Some(today_at(11 * 60 + 30)),
        },
        SeedTask {
            title: "green bin".to_string(),
            description: Some("Take out the green bin every 2 weeks".to_string()),
            branch_name: None,
            labels: vec!["chore".to_string()],
            deadline: Some(biweekly_at((2026, 9, 7), 21 * 60, now_secs)),
            importance_factor: 1.5,
            urgency_factor: 1.0,
            parent_title: None,
            blocked_until: Some(biweekly_at((2026, 9, 7), 16 * 60, now_secs)),
        },
        SeedTask {
            title: "recycling bin (gray)".to_string(),
            description: Some("Take out the gray bin every 2 weeks".to_string()),
            branch_name: None,
            labels: vec!["chore".to_string()],
            deadline: Some(biweekly_at((2026, 9, 14), 21 * 60, now_secs)),
            importance_factor: 1.5,
            urgency_factor: 1.0,
            parent_title: None,
            blocked_until: Some(biweekly_at((2026, 9, 14), 16 * 60, now_secs)),
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
            blocked_until: None,
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
            blocked_until: None,
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
            blocked_until: None,
        },
        SeedTask {
            title: "Migrate existing actions".to_string(),
            description: Some(
                "Move repeat, blocker and follow-up actions into the new panel".to_string(),
            ),
            branch_name: None,
            labels: vec!["refactor".to_string()],
            deadline: Some(now_secs + 12 * 86400),
            importance_factor: 1.0,
            urgency_factor: 0.5,
            parent_title: Some("Rewrite the details panel".to_string()),
            blocked_until: None,
        },
        // Tally (the accounting project) backlog.
        SeedTask {
            title: "File the corporation tax return".to_string(),
            description: Some(
                "Prepare the CT600 and the iXBRL accounts, then submit to HMRC".to_string(),
            ),
            branch_name: Some("feat/ct600-filing".to_string()),
            labels: vec!["tax".to_string(), "feature".to_string()],
            deadline: Some(now_secs + 6 * 86400),
            importance_factor: 2.0,
            urgency_factor: 2.0,
            parent_title: None,
            blocked_until: None,
        },
        SeedTask {
            title: "Classify the latest batch of transactions".to_string(),
            description: Some(
                "Pull the new bank feed and classify the transactions into the account tree"
                    .to_string(),
            ),
            branch_name: None,
            labels: vec!["chore".to_string()],
            deadline: Some(now_secs + 2 * 86400),
            importance_factor: 1.0,
            urgency_factor: 1.5,
            parent_title: None,
            blocked_until: None,
        },
        SeedTask {
            title: "Document the FRS 105 mappings".to_string(),
            description: Some(
                "Record how the account tree maps onto the iXBRL taxonomy for micro-entities"
                    .to_string(),
            ),
            branch_name: None,
            labels: vec!["docs".to_string()],
            deadline: Some(now_secs + 9 * 86400),
            importance_factor: 1.0,
            urgency_factor: 0.5,
            parent_title: None,
            blocked_until: None,
        },
        // The about-me site.
        SeedTask {
            title: "Add the 2026 accounts to the site".to_string(),
            description: Some("Publish the last accounts and a short write-up on the site".to_string()),
            branch_name: Some("content/2026-accounts".to_string()),
            labels: vec!["content".to_string()],
            deadline: Some(now_secs + 4 * 86400),
            importance_factor: 1.0,
            urgency_factor: 1.0,
            parent_title: None,
            blocked_until: None,
        },
        SeedTask {
            title: "Write a post about the Tally build".to_string(),
            description: Some(
                "Draft a blog entry covering how the accounts pipeline works".to_string(),
            ),
            branch_name: Some("content/tally-build".to_string()),
            labels: vec!["blog".to_string()],
            deadline: Some(now_secs + 5 * 86400),
            importance_factor: 1.0,
            urgency_factor: 0.5,
            parent_title: None,
            blocked_until: None,
        },
        SeedTask {
            title: "Update the projects page".to_string(),
            description: Some("Refresh project blurbs and links on the home page".to_string()),
            branch_name: None,
            labels: vec!["frontend".to_string()],
            deadline: Some(now_secs + 3 * 86400),
            importance_factor: 1.0,
            urgency_factor: 1.0,
            parent_title: None,
            blocked_until: None,
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
            task_title: "gym".to_string(),
            tag_name: "health".to_string(),
        },
        SeedAssignment {
            task_title: "shower".to_string(),
            tag_name: "health".to_string(),
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
            let tag = self.create_seed_tag(&seed.name).await?;
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

        // Seed a couple of project folders by default so the agent pane has
        // directory-backed projects available immediately. Created before
        // the tasks so the assignments below reuse the same display-named
        // tags instead of `assign_tag_to_task` auto-creating plain ones.
        for (path, label) in [
            (
                std::path::Path::new("/Users/me/src/me/accounting"),
                "accounting",
            ),
            (
                std::path::Path::new("/Users/me/src/me/about-me"),
                "about-me",
            ),
        ] {
            self.create_seed_project_tag(path, label).await?;
        }

        let tasks = seed_tasks();
        let mut task_map = std::collections::HashMap::new();
        let demo = self.demo_app().await?;

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
                        .blocked_until(seed.blocked_until)
                        .importance_factor(seed.importance_factor)
                        .urgency_factor(seed.urgency_factor)
                        .parent_id(parent_id),
                )
                .await?;
            // Shipped content belongs to the builtin app in `captured` mode:
            // fully editable, never regenerated, never pushed.
            self.set_task_managed(
                task.id,
                demo.id,
                crate::managed::ManagedMode::Captured,
                true,
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

        // Tasks belonging to the seeded project folders, keyed by the same
        // `project:{path}` tag names created above.
        let project_tasks: &[(&str, &[&str])] = &[
            (
                "project:/Users/me/src/me/accounting",
                &[
                    "File the corporation tax return",
                    "Classify the latest batch of transactions",
                    "Document the FRS 105 mappings",
                ],
            ),
            (
                "project:/Users/me/src/me/about-me",
                &[
                    "Add the 2026 accounts to the site",
                    "Write a post about the Tally build",
                    "Update the projects page",
                ],
            ),
        ];
        for (tag_name, titles) in project_tasks {
            for title in *titles {
                if let Some(&task_id) = task_map.get(*title) {
                    self.assign_tag_to_task(task_id, tag_name).await?;
                }
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
        if let (Some(&review_id), Some(&deploy_id)) = (
            task_map.get("Code review PRs"),
            task_map.get("Deploy to production"),
        ) {
            self.add_blocker(deploy_id, review_id).await?;
        }

        // "feed dorito" repeats daily: starts 5:50pm, due 6:30pm, in the
        // system timezone. The occurrence hides until its start time via
        // `blocked_until`.
        if let Some(&task_id) = task_map.get("feed dorito") {
            let template = self
                .set_repeat(
                    task_id,
                    "feed dorito".to_string(),
                    1,
                    Some(18 * 60 + 30),
                    Some(17 * 60 + 50),
                )
                .await?;
            let zone = jiff::tz::TimeZone::system();
            let timezone = zone.iana_name().unwrap_or("UTC").to_string();
            crate::RepeatTaskTemplate::update_by_id(template.id)
                .timezone(Some(timezone))
                .exec(&mut self.db)
                .await
                .context(crate::error::QueryTagsSnafu {
                    context: "seed repeat timezone",
                })?;
        }

        // "gym" repeats daily (starts 10am, due 11:30am) and "shower"
        // (starts 11:30am, due 12pm) waits for the same day's gym session
        // via template dependence.
        if let Some(&gym_id) = task_map.get("gym") {
            let gym = self
                .set_repeat(
                    gym_id,
                    "gym".to_string(),
                    1,
                    Some(11 * 60 + 30),
                    Some(10 * 60),
                )
                .await?;
            if let Some(&shower_id) = task_map.get("shower") {
                self.set_repeat(
                    shower_id,
                    "shower".to_string(),
                    1,
                    Some(12 * 60),
                    Some(11 * 60 + 30),
                )
                .await?;
                self.set_repeat_blocked_by(shower_id, Some(gym.id)).await?;
            }
        }

        // "green bin" (from Mon Sep 7) and "recycling bin (gray)" (from Mon
        // Sep 14) repeat every 2 weeks: enabled 4pm, due 9pm. The first
        // occurrences below anchor the 14-day grid for the materializer.
        for title in ["green bin", "recycling bin (gray)"] {
            if let Some(&task_id) = task_map.get(title) {
                let template = self
                    .set_repeat(task_id, title.to_string(), 14, Some(21 * 60), Some(16 * 60))
                    .await?;
                let zone = jiff::tz::TimeZone::system();
                let timezone = zone.iana_name().unwrap_or("UTC").to_string();
                crate::RepeatTaskTemplate::update_by_id(template.id)
                    .timezone(Some(timezone))
                    .exec(&mut self.db)
                    .await
                    .context(crate::error::QueryTagsSnafu {
                        context: "seed repeat timezone",
                    })?;
            }
        }

        self.seed_workflows().await?;

        tracing::info!(
            tasks = task_map.len(),
            tags = tag_map.len(),
            "Seed data inserted"
        );
        Ok(())
    }

    /// Seed the eight workflow recipes from the spec's acceptance cases
    /// (v2 form: timer waits live on the edge, no `time` nodes). No runs
    /// are started. The travel recipe is enabled by default — its managed
    /// tag and checklist sections are created here and marked as seed
    /// data — while every other automation stays disabled until the user
    /// enables it from the Automations panel. The Birthday recipe also
    /// gets a yearly schedule template.
    pub async fn seed_workflows(&mut self) -> crate::QueryResult<()> {
        use serde_json::json;
        if !self.list_recipes().await?.is_empty() {
            return Ok(());
        }
        let recipes: [(&str, serde_json::Value); 8] = [
            // Case 1: travel checklists. Enabled via its managed tag; each
            // trip is a workflow run whose items the builder generates into
            // the Pack / Before leaving sections (nodes are a formality).
            (
                "packing-list",
                json!({
                    "name": "Travel checklists",
                    "description": "Build a packing checklist per trip: pack and before-leaving sections.",
                    "managed_tag": "managed:packing-list",
                    "params": {
                        "name": { "type": "string", "default": "" },
                        "days": { "type": "string", "default": "5" },
                        "activities": { "type": "string", "default": "" }
                    },
                    "nodes": [
                        { "id": "trip", "kind": "action", "title": "Trip checklist" }
                    ],
                    "edges": []
                }),
            ),
            // Case 2: relative timer — the follow-up appears 4 days after
            // the application is submitted, not at run creation.
            (
                "follow-up",
                json!({
                    "name": "Job application tracker",
                    "description": "Submit an application, then get a nudge to follow up 4 days later.",
                    "nodes": [
                        { "id": "submit", "kind": "action", "title": "Submit application" },
                        { "id": "followup", "kind": "action", "title": "Follow up" }
                    ],
                    "edges": [
                        { "from": "submit", "to": "followup", "condition_type": "timer", "condition_value": "4 days" }
                    ]
                }),
            ),
            // Case 3: annual birthday workflow, isolated runs per year.
            (
                "birthday",
                json!({
                    "name": "Birthday reminders",
                    "description": "Send greetings and call once a year, on schedule.",
                    "missed_policy": "skip",
                    "nodes": [
                        { "id": "greet", "kind": "action", "title": "Send birthday greeting" },
                        { "id": "call", "kind": "action", "title": "Call for birthday" }
                    ],
                    "edges": [
                        { "from": "greet", "to": "call", "condition_type": "on_complete" }
                    ]
                }),
            ),
            // Case 4: AI draft, human approval as a subtask, rejection
            // spawns Edit and re-opens the draft for another pass.
            (
                "gatekeeper",
                json!({
                    "name": "Gatekeeper",
                    "description": "AI drafts, you approve; rejection opens an edit loop.",
                    "nodes": [
                        { "id": "draft", "kind": "action", "title": "Draft the email", "ai": true, "description": "An AI or script can pick this up and write a draft." },
                        { "id": "approve", "kind": "action", "title": "Approve draft", "approval": true, "retrigger_on_reject": true },
                        { "id": "send", "kind": "action", "title": "Send message" },
                        { "id": "edit", "kind": "action", "title": "Edit draft" }
                    ],
                    "edges": [
                        { "from": "draft", "to": "approve", "condition_type": "on_result", "condition_value": {} },
                        { "from": "approve", "to": "send", "condition_type": "on_result", "condition_value": { "approved": true } },
                        { "from": "approve", "to": "edit", "condition_type": "on_result", "condition_value": { "approved": false } }
                    ]
                }),
            ),
            // Case 5: parallel research, summary only when all three are
            // done, in any order.
            (
                "parallel-research",
                json!({
                    "name": "Parallel research",
                    "description": "Three research tasks in parallel; the summary waits for all of them.",
                    "nodes": [
                        { "id": "research_a", "kind": "action", "title": "Research topic A" },
                        { "id": "research_b", "kind": "action", "title": "Research topic B" },
                        { "id": "research_c", "kind": "action", "title": "Research topic C" },
                        { "id": "final_summary", "kind": "action", "title": "Write final summary" }
                    ],
                    "edges": [
                        { "from": "research_a", "to": "final_summary", "condition_type": "on_complete" },
                        { "from": "research_b", "to": "final_summary", "condition_type": "on_complete" },
                        { "from": "research_c", "to": "final_summary", "condition_type": "on_complete" }
                    ]
                }),
            ),
            // Case 6: runtime param picks an immediate or delayed call.
            (
                "conditional-urgency",
                json!({
                    "name": "Conditional urgency",
                    "description": "Call now when marked urgent, otherwise wait 3 days.",
                    "params": { "urgent": { "type": "boolean", "default": false, "description": "Call immediately when true" } },
                    "nodes": [
                        { "id": "review", "kind": "action", "title": "Review the lead" },
                        { "id": "call_client", "kind": "action", "title": "Call client" }
                    ],
                    "edges": [
                        { "from": "review", "to": "call_client", "condition_type": "timer", "condition_value": { "if": "param:urgent", "then": "0 days", "else": "3 days" } }
                    ]
                }),
            ),
            // Case 7: external event — an "Await reply" task is visible and
            // tickable; the webhook/button fires the same signal.
            (
                "contract-follow-up",
                json!({
                    "name": "Contract follow-up",
                    "description": "Send an email, then wait for the reply before drafting the contract.",
                    "nodes": [
                        { "id": "send_email", "kind": "action", "title": "Send email" },
                        { "id": "await_reply", "kind": "event", "title": "Await reply from client" },
                        { "id": "draft_contract", "kind": "action", "title": "Draft contract" }
                    ],
                    "edges": [
                        { "from": "send_email", "to": "await_reply", "condition_type": "event", "condition_value": "client_replied" },
                        { "from": "await_reply", "to": "draft_contract", "condition_type": "on_complete" }
                    ]
                }),
            ),
            // Case 8: manual real-world await — mark received appears after
            // a 3-day wait and is ticked by hand when the package arrives.
            (
                "order-delivery",
                json!({
                    "name": "Order delivery",
                    "description": "Place an order, then mark the package received once it arrives.",
                    "nodes": [
                        { "id": "place_order", "kind": "action", "title": "Place order" },
                        { "id": "mark_received", "kind": "action", "title": "Mark package as received" }
                    ],
                    "edges": [
                        { "from": "place_order", "to": "mark_received", "condition_type": "timer", "condition_value": "3 days" }
                    ]
                }),
            ),
        ];
        let mut ids = std::collections::HashMap::new();
        for (slug, recipe_json) in recipes {
            let recipe = self.create_recipe(slug, recipe_json).await?;
            ids.insert(slug, recipe.id);
        }
        // No runs are created: every automation starts disabled, except
        // travel checklists, which is enabled by default: its tag and
        // checklist sections are created here so the travel panel shows up
        // without a trip to the Automations panel. The tag belongs to the
        // travel app, not the builtin demo app.
        let travel_id = ids["packing-list"];
        self.enable_managed_recipe(travel_id).await?;

        // Yearly schedule for the Birthday recipe: fires March 1 each year
        // via the repeat materializer, creating a brand new isolated run.
        let birthday_id = ids["birthday"];
        let zone = jiff::tz::TimeZone::system();
        let timezone = zone.iana_name().unwrap_or("UTC").to_string();
        crate::RepeatTaskTemplate::create()
            .name("Birthday".to_string())
            .interval_days(365)
            .time_of_day(None)
            .start_time_of_day(None)
            .weekdays(None)
            .month_day(Some(1))
            .strict(false)
            .timezone(Some(timezone))
            .recipe_id(Some(birthday_id))
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "seed birthday workflow schedule",
            })?;
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
        assert_eq!(tasks.len(), 24);

        let tags = store.list_tags().await?;
        // 9 seed tags, 2 project tags, plus the travel managed tag and
        // its 8 checklist sections, enabled by default.
        assert_eq!(tags.len(), 20);

        // The builtin demo app owns the shipped rows so sync never touches
        // them; workflow steps are covered by the workflow_run_id guard
        // instead. Directory-backed project tags stay user-owned, and the
        // enabled travel app owns the sections of its own tag.
        let demo = store.demo_app().await?;
        for task in &tasks {
            let ownership = store.task_ownership(task.id).await?;
            assert_eq!(
                ownership.managed_by,
                Some(demo.id),
                "{} must belong to the demo app",
                task.title
            );
            assert_eq!(
                ownership.managed_mode,
                Some(crate::managed::ManagedMode::Captured),
                "demo content is captured, never regenerated"
            );
        }
        let mut demo_tags = 0;
        for tag in &tags {
            if store.tag_owner(tag.id).await? == Some(demo.id) {
                demo_tags += 1;
            }
        }
        assert_eq!(demo_tags, 9, "the nine seeded tags belong to the demo app");

        // Automations start disabled: no runs exist until enabled — except
        // travel checklists, which is enabled by default: its managed tag
        // exists straight after seeding.
        assert!(store.list_workflow_runs().await?.is_empty());
        // The travel automation is a managed-tag recipe whose tag is
        // created (and marked as seed data) during seeding.
        let travel_meta = store
            .list_recipe_metas()
            .await?
            .into_iter()
            .find(|m| m.slug == "packing-list")
            .expect("travel recipe should be seeded");
        assert!(travel_meta.managed_tag.is_some());
        assert!(travel_meta.managed_enabled, "travel should be enabled by default");
        assert!(
            store
                .get_tag_by_name("managed:packing-list")
                .await?
                .is_some()
        );

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
            rewrite_subtasks
                .iter()
                .all(|t| t.parent_id == Some(rewrite.id)),
            "all three subtasks must point back at the parent"
        );

        // "feed dorito" repeats daily: starts 5:50pm, due 6:30pm, high
        // importance, hidden until its start time.
        let feed = tasks.iter().find(|t| t.title == "feed dorito").unwrap();
        assert_eq!(feed.importance_factor, 3.0);
        let template = store.repeat_template_for_task(feed.id).await?.unwrap();
        assert_eq!(template.interval_days, 1);
        assert_eq!(template.time_of_day, Some(18 * 60 + 30));
        assert_eq!(template.start_time_of_day, Some(17 * 60 + 50));
        assert!(template.timezone.is_some());

        // "gym" repeats daily (starts 10am, due 11:30am); "shower"
        // (starts 11:30am, due 12pm) depends on the same day's gym session.
        let gym = tasks.iter().find(|t| t.title == "gym").unwrap();
        let gym_template = store.repeat_template_for_task(gym.id).await?.unwrap();
        assert_eq!(gym_template.interval_days, 1);
        assert_eq!(gym_template.time_of_day, Some(11 * 60 + 30));
        assert_eq!(gym_template.start_time_of_day, Some(10 * 60));
        let shower = tasks.iter().find(|t| t.title == "shower").unwrap();
        let shower_template = store.repeat_template_for_task(shower.id).await?.unwrap();
        assert_eq!(shower_template.time_of_day, Some(12 * 60));
        assert_eq!(shower_template.start_time_of_day, Some(11 * 60 + 30));
        assert_eq!(
            shower_template.blocked_by_template_id,
            Some(gym_template.id)
        );

        // Bin collections repeat every 2 weeks from their anchor Mondays:
        // green from Sep 7, gray from Sep 14; enabled 4pm, due 9pm.
        for title in ["green bin", "recycling bin (gray)"] {
            let bin = tasks.iter().find(|t| t.title == title).unwrap();
            let template = store.repeat_template_for_task(bin.id).await?.unwrap();
            assert_eq!(template.interval_days, 14);
            assert_eq!(template.time_of_day, Some(21 * 60));
            assert_eq!(template.start_time_of_day, Some(16 * 60));
        }

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
        assert_eq!(
            blocked_titles,
            vec!["Write migration tests", "Add tag filtering"]
        );

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
