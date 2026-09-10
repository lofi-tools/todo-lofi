//! Workflow engine (v2 of `docs/spec/workflow-engine-spec.md`, per
//! `docs/spec/workflow-simplification-spec.md`).
//!
//! Recipes are immutable, versioned JSON documents (`workflow_recipes`).
//! A run (`workflow_runs`) materializes the recipe's start nodes as
//! ordinary `tasks` rows carrying `workflow_run_id` + `node_id`; steps are
//! created lazily as their prerequisites complete. Time waits pre-create
//! the downstream step with `blocked_until` (existing list queries hide
//! far-future steps and prevent early ticking). Event waits create a
//! tickable waiter task. Results live on the run row (`step_results`).

use crate::{QueryResult, TodoStore};
use serde_json::Value;
use snafu::ResultExt;
use std::collections::{HashMap, HashSet, VecDeque};
use toasty::Model;

#[derive(Debug, Clone, Model)]
pub struct WorkflowRecipe {
    #[key]
    #[auto]
    pub id: u64,
    pub slug: String,
    /// Monotonic per slug; recipes are immutable, runs pin the exact row.
    pub version: u64,
    pub recipe_json: toasty::Json<Value>,
    #[default(jiff::Timestamp::now())]
    pub created_at: jiff::Timestamp,
}

#[derive(Debug, Clone, Model)]
pub struct WorkflowRun {
    #[key]
    #[auto]
    pub id: u64,
    pub recipe_id: u64,
    /// Set when the run was created by a recipe schedule
    /// (`repeat_task_templates.recipe_id`).
    pub schedule_id: Option<u64>,
    /// `active` | `completed` | `cancelled`.
    pub status: String,
    /// Runtime params merged with the recipe schema defaults.
    pub params: toasty::Json<Value>,
    /// `node_id -> result` recorded by completed steps; consumed by
    /// `on_result` edges.
    pub step_results: toasty::Json<Value>,
    #[default(jiff::Timestamp::now())]
    pub created_at: jiff::Timestamp,
    pub completed_at: Option<jiff::Timestamp>,
}

#[derive(Debug, Clone)]
pub struct RecipeNode {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub description: Option<String>,
    pub ai: bool,
    pub approval: bool,
    pub retrigger_on_reject: bool,
}

#[derive(Debug, Clone)]
pub struct RecipeEdge {
    pub from: String,
    pub to: String,
    pub condition_type: String,
    pub condition_value: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct ParamSchema {
    pub r#type: String,
    pub default: Option<Value>,
    pub required: bool,
}

#[derive(Debug, Clone)]
pub struct Recipe {
    pub name: String,
    pub description: Option<String>,
    pub params: HashMap<String, ParamSchema>,
    pub missed_policy: String,
    pub nodes: Vec<RecipeNode>,
    pub edges: Vec<RecipeEdge>,
}

/// Recipe summary for UI recipe pickers (the full graph lives in
/// `recipe_json`). `active_runs` is the number of runs currently in
/// `active` status, so the automations panel can show enable/disable
/// state without fetching the runs themselves.
#[derive(Debug, Clone)]
pub struct RecipeMeta {
    pub id: u64,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub active_runs: usize,
}

/// One step of a run, as the UI renders it: the task plus the recipe node
/// it materializes and the edges that completing it would evaluate.
#[derive(Debug, Clone)]
pub struct RunStepView {
    pub task: crate::TaskWithMeta,
    pub node: RecipeNode,
    pub outgoing: Vec<RecipeEdge>,
    /// The event edge that spawned this step, when the step is an event
    /// waiter (its trigger button resolves it).
    pub incoming_event: Option<RecipeEdge>,
}

/// A run plus its steps, for the run header/banner in the UI.
#[derive(Debug, Clone)]
pub struct RunView {
    pub run: WorkflowRun,
    pub recipe_name: String,
    pub steps: Vec<RunStepView>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// First column of a raw SQL row as an id, if the row is a record.
fn row_id(row: &toasty::stmt::Value) -> Option<u64> {
    if let toasty::stmt::Value::Record(record) = row {
        record.first().and_then(|v| v.to_i64()).map(|id| id as u64)
    } else {
        None
    }
}

fn invalid(message: impl Into<String>) -> crate::QueryErr {
    crate::QueryErr::UnexpectedValue {
        message: message.into(),
    }
}

/// Parse and validate a recipe document. Errors are user-facing strings.
pub fn parse_recipe(json: &Value) -> Result<Recipe, String> {
    let root = json.as_object().ok_or("recipe must be a JSON object")?;
    let name = root
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("recipe `name` must be a non-empty string")?;

    let mut params: HashMap<String, ParamSchema> = HashMap::new();
    if let Some(raw_params) = root.get("params") {
        let map = raw_params
            .as_object()
            .ok_or("recipe `params` must be an object")?;
        for (param_name, schema) in map {
            let obj = schema.as_object().ok_or("param schema must be an object")?;
            let r#type = obj
                .get("type")
                .and_then(|v| v.as_str())
                .ok_or("param schema requires `type`")?
                .to_string();
            if !["boolean", "string", "number"].contains(&r#type.as_str()) {
                return Err(format!("param `{param_name}` has invalid type `{type}`"));
            }
            let default = obj.get("default").cloned();
            if let Some(default) = &default
                && !type_matches(default, &r#type)
            {
                return Err(format!("param `{param_name}` default does not match type `{type}`"));
            }
            let required = obj
                .get("required")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            params.insert(param_name.clone(), ParamSchema {
                r#type,
                default,
                required,
            });
        }
    }

    let description = root
        .get("description")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let missed_policy = root
        .get("missed_policy")
        .and_then(|v| v.as_str())
        .unwrap_or("skip")
        .to_string();
    if !["skip", "catch_up"].contains(&missed_policy.as_str()) {
        return Err("recipe `missed_policy` must be \"skip\" or \"catch_up\"".to_string());
    }

    let raw_nodes = root
        .get("nodes")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or("recipe `nodes` must be a non-empty array")?;
    let mut nodes: Vec<RecipeNode> = Vec::with_capacity(raw_nodes.len());
    let mut node_ids: HashSet<String> = HashSet::new();
    for raw in raw_nodes {
        let obj = raw.as_object().ok_or("recipe node must be an object")?;
        let id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty() && valid_id(s))
            .ok_or("recipe node `id` must match [A-Za-z0-9_-]+")?
            .to_string();
        if !node_ids.insert(id.clone()) {
            return Err(format!("duplicate node id `{id}`"));
        }
        let kind = obj
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or("recipe node requires `kind`")?
            .to_string();
        if !["action", "event"].contains(&kind.as_str()) {
            return Err(format!("node `{id}` kind must be \"action\" or \"event\""));
        }
        let title = obj
            .get("title")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or(format!("node `{id}` requires a non-empty `title`"))?;
        let ai = obj.get("ai").and_then(|v| v.as_bool()).unwrap_or(false);
        let approval = obj
            .get("approval")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if approval && kind != "action" {
            return Err(format!("node `{id}`: `approval` is only valid on action nodes"));
        }
        let retrigger_on_reject = obj
            .get("retrigger_on_reject")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if retrigger_on_reject && !approval {
            return Err(format!(
                "node `{id}`: `retrigger_on_reject` requires `approval: true`"
            ));
        }
        nodes.push(RecipeNode {
            id,
            kind,
            title: title.to_string(),
            description: obj.get("description").and_then(|v| v.as_str()).map(str::to_string),
            ai,
            approval,
            retrigger_on_reject,
        });
    }

    let raw_edges = root
        .get("edges")
        .and_then(|v| v.as_array())
        .ok_or("recipe `edges` must be an array")?;
    let mut edges: Vec<RecipeEdge> = Vec::with_capacity(raw_edges.len());
    for raw in raw_edges {
        let obj = raw.as_object().ok_or("recipe edge must be an object")?;
        let from = obj
            .get("from")
            .and_then(|v| v.as_str())
            .ok_or("recipe edge requires `from`")?
            .to_string();
        let to = obj
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or("recipe edge requires `to`")?
            .to_string();
        if !node_ids.contains(from.as_str()) {
            return Err(format!("edge `{from}->{to}` references unknown node `{from}`"));
        }
        if !node_ids.contains(to.as_str()) {
            return Err(format!("edge `{from}->{to}` references unknown node `{to}`"));
        }
        if from == to {
            return Err(format!("edge `{from}->{to}` must not be a self-loop"));
        }
        let condition_type = obj
            .get("condition_type")
            .and_then(|v| v.as_str())
            .ok_or("recipe edge requires `condition_type`")?
            .to_string();
        if !["on_complete", "on_result", "timer", "event"].contains(&condition_type.as_str()) {
            return Err(format!(
                "edge `{from}->{to}` has invalid condition_type `{condition_type}`"
            ));
        }
        let condition_value = obj.get("condition_value").cloned();
        let to_kind = nodes
            .iter()
            .find(|n| n.id == to)
            .map(|n| n.kind.as_str())
            .unwrap_or("");
        match condition_type.as_str() {
            "event" => {
                if to_kind != "event" {
                    return Err(format!(
                        "edge `{from}->{to}`: event edges must target an event node"
                    ));
                }
                if !condition_value
                    .as_ref()
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty())
                {
                    return Err(format!(
                        "edge `{from}->{to}`: event edges require a non-empty `condition_value` (event name)"
                    ));
                }
            }
            "timer" => {
                if to_kind != "action" {
                    return Err(format!(
                        "edge `{from}->{to}`: timer edges must target an action node"
                    ));
                }
                let value = condition_value
                    .as_ref()
                    .ok_or(format!("edge `{from}->{to}`: timer edge requires a duration"))?;
                validate_duration(value, &params)
                    .map_err(|e| format!("edge `{from}->{to}`: {e}"))?;
            }
            "on_result" => {
                if condition_value.is_none() {
                    return Err(format!(
                        "edge `{from}->{to}`: on_result edges require `condition_value`"
                    ));
                }
                if to_kind != "action" {
                    return Err(format!(
                        "edge `{from}->{to}`: on_result edges must target an action node"
                    ));
                }
            }
            "on_complete" => {
                if to_kind != "action" {
                    return Err(format!(
                        "edge `{from}->{to}`: on_complete edges must target an action node"
                    ));
                }
            }
            _ => unreachable!("condition_type validated above"),
        }
        edges.push(RecipeEdge {
            from,
            to,
            condition_type,
            condition_value,
        });
    }

    // Event nodes: all incoming edges are `event`, all outgoing are
    // `on_complete`; an approval node is fed by an `on_result` edge from an
    // `ai: true` action node.
    for node in &nodes {
        let incoming: Vec<&RecipeEdge> = edges.iter().filter(|e| e.to == node.id).collect();
        let outgoing: Vec<&RecipeEdge> = edges.iter().filter(|e| e.from == node.id).collect();
        if node.kind == "event" {
            if incoming.is_empty() || incoming.iter().any(|e| e.condition_type != "event") {
                return Err(format!(
                    "event node `{}` must have exactly event-typed incoming edges",
                    node.id
                ));
            }
            if outgoing.is_empty() || outgoing.iter().any(|e| e.condition_type != "on_complete") {
                return Err(format!(
                    "event node `{}` must have on_complete outgoing edges",
                    node.id
                ));
            }
        }
        if node.approval {
            let fed_by_ai = incoming
                .iter()
                .any(|e| {
                    e.condition_type == "on_result"
                        && nodes
                            .iter()
                            .find(|n| n.id == e.from)
                            .is_some_and(|n| n.ai)
                });
            if !fed_by_ai {
                return Err(format!(
                    "approval node `{}` must be fed by an on_result edge from an ai: true node",
                    node.id
                ));
            }
        }
    }

    // DAG check (Kahn): lazy spawn and fan-in need an acyclic graph.
    let mut indegree: HashMap<String, usize> = node_ids.iter().map(|id| (id.clone(), 0)).collect();
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    for edge in &edges {
        indegree.entry(edge.to.clone()).and_modify(|d| *d += 1);
        adjacency
            .entry(edge.from.clone())
            .or_default()
            .push(edge.to.clone());
    }
    let mut queue: VecDeque<String> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(id, _)| id.clone())
        .collect();
    let mut seen = 0;
    while let Some(current) = queue.pop_front() {
        seen += 1;
        if let Some(next) = adjacency.get(&current) {
            for n in next {
                if let Some(degree) = indegree.get_mut(n) {
                    *degree -= 1;
                    if *degree == 0 {
                        queue.push_back(n.clone());
                    }
                }
            }
        }
    }
    if seen != node_ids.len() {
        return Err("recipe graph contains a cycle".to_string());
    }

    Ok(Recipe {
        name: name.to_string(),
        description,
        params,
        missed_policy,
        nodes,
        edges,
    })
}

fn valid_id(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn type_matches(value: &Value, r#type: &str) -> bool {
    match r#type {
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        _ => false,
    }
}

/// "4 days", "0 seconds", … -> seconds.
fn parse_duration(s: &str) -> Option<u64> {
    let (number, unit) = s.trim().split_once(' ')?;
    let n: u64 = number.parse().ok()?;
    let secs = match unit {
        "second" | "seconds" => 1,
        "minute" | "minutes" => 60,
        "hour" | "hours" => 3600,
        "day" | "days" => 86400,
        _ => return None,
    };
    Some(n * secs)
}

fn validate_duration(value: &Value, params: &HashMap<String, ParamSchema>) -> Result<u64, String> {
    match value {
        Value::String(s) => parse_duration(s).ok_or_else(|| {
            format!("invalid duration `{s}` (expected e.g. \"4 days\")")
        }),
        Value::Object(map) => {
            let param = map
                .get("if")
                .and_then(|v| v.as_str())
                .and_then(|s| s.strip_prefix("param:"))
                .ok_or("duration expression `if` must be \"param:<name>\"")?;
            if !params.contains_key(param) {
                return Err(format!("duration expression references unknown param `{param}`"));
            }
            let then = map
                .get("then")
                .and_then(|v| v.as_str())
                .ok_or("duration expression requires `then`")?;
            let otherwise = map
                .get("else")
                .and_then(|v| v.as_str())
                .ok_or("duration expression requires `else`")?;
            parse_duration(then)
                .ok_or_else(|| format!("invalid duration `{then}`"))?;
            parse_duration(otherwise)
                .ok_or_else(|| format!("invalid duration `{otherwise}`"))?;
            Ok(0) // resolved at run time; shape is what matters here
        }
        _ => Err("duration must be a string or an {\"if\"…} expression".to_string()),
    }
}

/// Resolve a duration or duration expression against the run's params.
fn resolve_duration(
    _recipe: &Recipe,
    run: &WorkflowRun,
    value: Option<&Value>,
) -> Result<u64, String> {
    let value = value.ok_or("timer edge is missing a duration")?;
    match value {
        Value::String(s) => parse_duration(s).ok_or_else(|| format!("invalid duration `{s}`")),
        Value::Object(map) => {
            let param = map
                .get("if")
                .and_then(|v| v.as_str())
                .and_then(|s| s.strip_prefix("param:"))
                .ok_or("duration expression `if` must be \"param:<name>\"")?;
            let value = run.params.0.get(param);
            let truthy = value
                .map(|v| !v.is_null() && v.as_bool().unwrap_or(true))
                .unwrap_or(false);
            let branch = if truthy { "then" } else { "else" };
            let raw = map
                .get(branch)
                .and_then(|v| v.as_str())
                .ok_or("duration expression is missing a branch")?;
            parse_duration(raw).ok_or_else(|| format!("invalid duration `{raw}`"))
        }
        _ => Err("duration must be a string or an {\"if\"…} expression".to_string()),
    }
}

/// Key-based matching (§6.4 of the v1 spec): object conditions match when
/// every key present in the condition exists in the result (extra result
/// keys ignored); scalars match by exact equality; `{}` matches anything.
pub fn key_match(result: &Value, condition: &Value) -> bool {
    match condition {
        // `{}` matches any result, including null: "complete and continue".
        Value::Object(map) if map.is_empty() => true,
        Value::Object(map) => {
            let Value::Object(result_map) = result else {
                return false;
            };
            map.iter()
                .all(|(key, value)| result_map.get(key).is_some_and(|v| key_match(v, value)))
        }
        scalar => result == scalar,
    }
}

fn is_rejection(result: &Value) -> bool {
    key_match(result, &serde_json::json!({ "approved": false }))
}

impl TodoStore {
    pub async fn get_recipe(&mut self, id: u64) -> QueryResult<WorkflowRecipe> {
        WorkflowRecipe::get_by_id(&mut self.db, id)
            .await
            .context(crate::error::GetTaskSnafu { id })
    }

    pub async fn list_recipes(&mut self) -> QueryResult<Vec<WorkflowRecipe>> {
        let recipes = WorkflowRecipe::all()
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "list workflow recipes",
            })?;
        Ok(recipes)
    }

    pub async fn list_recipe_metas(&mut self) -> QueryResult<Vec<RecipeMeta>> {
        let active = self
            .list_workflow_runs()
            .await?
            .into_iter()
            .filter(|run| run.status == "active")
            .fold(HashMap::<u64, usize>::new(), |mut counts, run| {
                *counts.entry(run.recipe_id).or_default() += 1;
                counts
            });
        let mut metas = Vec::new();
        for recipe in self.list_recipes().await? {
            let parsed = parse_recipe(&recipe.recipe_json.0)
                .unwrap_or_else(|_| Recipe {
                    name: recipe.slug.clone(),
                    description: None,
                    params: HashMap::new(),
                    missed_policy: "skip".to_string(),
                    nodes: Vec::new(),
                    edges: Vec::new(),
                });
            metas.push(RecipeMeta {
                id: recipe.id,
                slug: recipe.slug,
                name: parsed.name,
                description: parsed.description,
                active_runs: active.get(&recipe.id).copied().unwrap_or(0),
            });
        }
        Ok(metas)
    }

    /// Create the next immutable version of a recipe. Validation failures
    /// reject the recipe.
    pub async fn create_recipe(
        &mut self,
        slug: &str,
        recipe_json: Value,
    ) -> QueryResult<WorkflowRecipe> {
        parse_recipe(&recipe_json).map_err(|message| invalid(message))?;
        let rows = toasty::sql::query(
            r#"SELECT id FROM workflow_recipes WHERE slug = ?1"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(slug)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "count workflow recipe versions",
        })?;
        let version = rows.len() as u64 + 1;
        let recipe = WorkflowRecipe::create()
            .slug(slug.to_string())
            .version(version)
            .recipe_json(toasty::Json(recipe_json))
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "create workflow recipe",
            })?;
        Ok(recipe)
    }

    pub async fn find_run(&mut self, run_id: u64) -> QueryResult<Option<WorkflowRun>> {
        let rows = toasty::sql::query(
            r#"SELECT id, recipe_id, schedule_id, status, params, step_results, created_at, completed_at
               FROM workflow_runs WHERE id = ?1"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
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
            context: "find workflow run",
        })?;
        Ok(rows.into_iter().next().and_then(|row| parse_run_row(&row)))
    }

    pub async fn get_run(&mut self, run_id: u64) -> QueryResult<WorkflowRun> {
        self.find_run(run_id)
            .await?
            .ok_or_else(|| invalid(format!("workflow run {run_id} not found")))
    }

    /// All runs, newest first (drives the run banner and history).
    pub async fn list_workflow_runs(&mut self) -> QueryResult<Vec<WorkflowRun>> {
        let rows = toasty::sql::query(
            r#"SELECT id, recipe_id, schedule_id, status, params, step_results, created_at, completed_at
               FROM workflow_runs ORDER BY id DESC"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list workflow runs",
        })?;
        Ok(rows.iter().filter_map(parse_run_row).collect())
    }

    /// Runs created by a recipe schedule (for the materializer's
    /// last-fired watermark).
    pub async fn runs_for_schedule(
        &mut self,
        template_id: u64,
    ) -> QueryResult<Vec<WorkflowRun>> {
        let rows = toasty::sql::query(
            r#"SELECT id, recipe_id, schedule_id, status, params, step_results, created_at, completed_at
               FROM workflow_runs WHERE schedule_id = ?1 ORDER BY created_at"#,
        )
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
        .bind(template_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list runs for schedule",
        })?;
        Ok(rows.iter().filter_map(parse_run_row).collect())
    }

    /// Instantiate a recipe: insert the run row, validate/merge params,
    /// and materialize every start node as a task.
    pub async fn create_run(
        &mut self,
        recipe_id: u64,
        params: Value,
        schedule_id: Option<u64>,
    ) -> QueryResult<WorkflowRun> {
        let recipe_row = self.get_recipe(recipe_id).await?;
        let recipe = parse_recipe(&recipe_row.recipe_json.0)
            .map_err(|message| invalid(format!("recipe {recipe_id}: {message}")))?;
        let effective = resolve_params(&recipe, params).map_err(|message| invalid(message))?;
        let run = WorkflowRun::create()
            .recipe_id(recipe_id)
            .schedule_id(schedule_id)
            .status("active".to_string())
            .params(toasty::Json(effective))
            .step_results(toasty::Json(serde_json::json!({})))
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "create workflow run",
            })?;

        let start_ids: HashSet<&str> = recipe
            .nodes
            .iter()
            .filter(|node| recipe.edges.iter().all(|edge| edge.to != node.id))
            .map(|node| node.id.as_str())
            .collect();
        for node in &recipe.nodes {
            if start_ids.contains(node.id.as_str()) {
                self.spawn_node_task(&recipe, &run, &node.id, None, &[], None)
                    .await?;
            }
        }
        Ok(run)
    }

    /// Complete a workflow step with an explicit result (approve/reject,
    /// AI output, …). Marks the task done, records the result, evaluates
    /// outgoing edges, and auto-completes the run when nothing is pending.
    /// Plain ticks (no result) go through `update_task_done` instead.
    pub async fn complete_workflow_step(
        &mut self,
        task_id: u64,
        result: Value,
    ) -> QueryResult<()> {
        let task = self.get_task(task_id).await?;
        let Some(run_id) = task.workflow_run_id else {
            return Ok(());
        };
        crate::Task::update_by_id(task_id)
            .done(true)
            .completed_at(Some(now_secs()))
            .exec(&mut self.db)
            .await
            .context(crate::error::UpdateTaskSnafu { id: task_id })?;
        let run = self.get_run(run_id).await?;
        self.evaluate_step_completion(&task, &run, result).await?;
        self.check_run_complete(run_id).await?;
        Ok(())
    }

    /// Engine hook called by `update_task_done` when a workflow step is
    /// ticked plain (no result supplied).
    pub async fn evaluate_workflow_completion(&mut self, task: &crate::Task) -> QueryResult<()> {
        let Some(run_id) = task.workflow_run_id else {
            return Ok(());
        };
        let run = self.get_run(run_id).await?;
        self.evaluate_step_completion(task, &run, Value::Null).await?;
        self.check_run_complete(run_id).await?;
        Ok(())
    }

    /// Mark every pending event waiter of `run_id` whose event name
    /// matches as done and evaluate its outgoing edges. Returns the number
    /// of waiters fired. Manual ticking of a waiter goes through
    /// `complete_task`/`update_task_done` instead.
    pub async fn trigger_event(&mut self, run_id: u64, event_name: &str) -> QueryResult<usize> {
        let Some(run) = self.find_run(run_id).await? else {
            return Ok(0);
        };
        let recipe_row = self.get_recipe(run.recipe_id).await?;
        let recipe = parse_recipe(&recipe_row.recipe_json.0)
            .map_err(|message| invalid(format!("recipe {}: {message}", run.recipe_id)))?;
        let mut fired = 0;
        for node in recipe.nodes.iter().filter(|n| n.kind == "event") {
            let matches = recipe.edges.iter().any(|edge| {
                edge.to == node.id
                    && edge.condition_type == "event"
                    && edge.condition_value.as_ref().and_then(|v| v.as_str()) == Some(event_name)
            });
            if !matches {
                continue;
            }
            let rows = toasty::sql::query(
                r#"SELECT id FROM tasks WHERE workflow_run_id = ?1 AND node_id = ?2
                   AND done = 0 AND deleted_at IS NULL ORDER BY id"#,
            )
            .column_types([toasty::stmt::Type::I64])
            .bind(run_id as i64)
            .bind(node.id.as_str())
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "find event waiters",
            })?;
            for row in rows {
                let Some(id) = row_id(&row) else {
                    continue;
                };
                let task = self.get_task(id).await?;
                crate::Task::update_by_id(id)
                    .done(true)
                    .completed_at(Some(now_secs()))
                    .exec(&mut self.db)
                    .await
                    .context(crate::error::UpdateTaskSnafu { id })?;
                self.evaluate_step_completion(&task, &run, Value::Null).await?;
                fired += 1;
            }
        }
        if fired > 0 {
            self.check_run_complete(run_id).await?;
        }
        Ok(fired)
    }

    /// Cancel a run: tombstone every step and mark the run cancelled.
    pub async fn cancel_run(&mut self, run_id: u64) -> QueryResult<()> {
        let rows = toasty::sql::query(
            r#"SELECT id FROM tasks WHERE workflow_run_id = ?1 AND deleted_at IS NULL"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(run_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list run tasks for cancel",
        })?;
        for row in rows {
            if let Some(id) = row_id(&row) {
                self.tombstone_task(id).await?;
            }
        }
        WorkflowRun::update_by_id(run_id)
            .status("cancelled".to_string())
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "cancel workflow run",
            })?;
        Ok(())
    }

    /// Cancel every active run of a recipe (the automations panel's
    /// "Disable"). Returns how many runs were cancelled.
    pub async fn cancel_active_runs(&mut self, recipe_id: u64) -> QueryResult<usize> {
        let run_ids: Vec<u64> = self
            .list_workflow_runs()
            .await?
            .into_iter()
            .filter(|run| run.recipe_id == recipe_id && run.status == "active")
            .map(|run| run.id)
            .collect();
        let cancelled = run_ids.len();
        for run_id in run_ids {
            self.cancel_run(run_id).await?;
        }
        Ok(cancelled)
    }

    /// Active runs with their steps, for the run banner in the UI.
    pub async fn list_active_run_views(&mut self) -> QueryResult<Vec<RunView>> {
        let runs = self.list_workflow_runs().await?;
        let mut views = Vec::new();
        for run in runs {
            if run.status != "active" {
                continue;
            }
            if let Some(view) = self.workflow_run_view(run.id).await? {
                views.push(view);
            }
        }
        Ok(views)
    }

    /// A run plus its steps, ordered by spawn order. Steps for nodes the
    /// engine has not materialized yet simply do not appear.
    pub async fn workflow_run_view(&mut self, run_id: u64) -> QueryResult<Option<RunView>> {
        let Some(run) = self.find_run(run_id).await? else {
            return Ok(None);
        };
        // Cancelled runs have no view: every step is tombstoned, so there
        // is nothing left to show.
        if run.status == "cancelled" {
            return Ok(None);
        }
        let recipe_row = self.get_recipe(run.recipe_id).await?;
        let recipe = parse_recipe(&recipe_row.recipe_json.0).unwrap_or_else(|_| Recipe {
            name: recipe_row.slug.clone(),
            description: None,
            params: HashMap::new(),
            missed_policy: "skip".to_string(),
            nodes: Vec::new(),
            edges: Vec::new(),
        });
        let rows = toasty::sql::query(
            r#"SELECT id FROM tasks WHERE workflow_run_id = ?1 AND deleted_at IS NULL ORDER BY id"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(run_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list run step tasks",
        })?;
        let mut steps = Vec::new();
        for row in rows {
            let Some(id) = row_id(&row) else {
                continue;
            };
            let task = self.get_task_with_meta(id).await?;
            let Some(node_id) = task.node_id.clone() else {
                continue;
            };
            let Some(node) = recipe.nodes.iter().find(|n| n.id == node_id).cloned() else {
                continue;
            };
            let outgoing: Vec<RecipeEdge> = recipe
                .edges
                .iter()
                .filter(|edge| edge.from == node_id)
                .cloned()
                .collect();
            let incoming_event = recipe
                .edges
                .iter()
                .find(|edge| edge.to == node_id && edge.condition_type == "event")
                .cloned();
            steps.push(RunStepView {
                task,
                node,
                outgoing,
                incoming_event,
            });
        }
        Ok(Some(RunView {
            run,
            recipe_name: recipe.name,
            steps,
        }))
    }

    /// Core evaluation: record the result, evaluate outgoing edges, spawn
    /// downstream steps, and handle approval re-triggering.
    async fn evaluate_step_completion(
        &mut self,
        task: &crate::Task,
        run: &WorkflowRun,
        result: Value,
    ) -> QueryResult<()> {
        let Some(node_id) = task.node_id.clone() else {
            return Ok(());
        };
        let recipe_row = self.get_recipe(run.recipe_id).await?;
        let recipe = parse_recipe(&recipe_row.recipe_json.0)
            .map_err(|message| invalid(format!("recipe {}: {message}", run.recipe_id)))?;

        let mut results = run.step_results.0.clone();
        let results_obj = results.as_object_mut().ok_or_else(|| {
            invalid("workflow run step_results is corrupt (expected an object)")
        })?;
        results_obj.insert(node_id.clone(), result.clone());
        WorkflowRun::update_by_id(run.id)
            .step_results(toasty::Json(results.clone()))
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "record workflow step result",
            })?;
        let run = WorkflowRun {
            step_results: toasty::Json(results),
            ..run.clone()
        };

        // Approval rejection with `retrigger_on_reject`: re-open the
        // automated parent so the AI/script can produce a revised result.
        if let Some(node) = recipe.nodes.iter().find(|n| n.id == node_id)
            && node.approval
            && node.retrigger_on_reject
            && is_rejection(&result)
            && let Some(parent_id) = task.parent_id
        {
            self.retry_task(parent_id, &recipe, &run).await?;
        }

        let outgoing: Vec<&RecipeEdge> = recipe
            .edges
            .iter()
            .filter(|edge| edge.from == node_id)
            .collect();
        for edge in outgoing {
            match edge.condition_type.as_str() {
                "on_complete" => {
                    if self.target_spawnable(&recipe, &run, &edge.to).await? {
                        let sources = self.satisfied_sources(&recipe, &run, &edge.to).await?;
                        self.spawn_node_task(&recipe, &run, &edge.to, None, &sources, None)
                            .await?;
                    }
                }
                "on_result" => {
                    let matched = edge
                        .condition_value
                        .as_ref()
                        .is_some_and(|condition| key_match(&result, condition));
                    if matched && self.target_spawnable(&recipe, &run, &edge.to).await? {
                        // Approval nodes materialize as subtasks of the
                        // automated task they review.
                        let parent_id = recipe
                            .nodes
                            .iter()
                            .find(|n| n.id == edge.to)
                            .filter(|n| n.approval)
                            .map(|_| task.id);
                        let sources = self.satisfied_sources(&recipe, &run, &edge.to).await?;
                        self.spawn_node_task(&recipe, &run, &edge.to, parent_id, &sources, None)
                            .await?;
                    }
                }
                "timer" => {
                    let duration = resolve_duration(&recipe, &run, edge.condition_value.as_ref())
                        .map_err(|message| invalid(message))?;
                    let scheduled_at = now_secs() + duration;
                    self.spawn_node_task(
                        &recipe,
                        &run,
                        &edge.to,
                        None,
                        &[task.id],
                        Some(scheduled_at),
                    )
                    .await?;
                }
                "event" => {
                    self.spawn_node_task(&recipe, &run, &edge.to, None, &[task.id], None)
                        .await?;
                }
                other => {
                    tracing::warn!(node = %node_id, condition = other, "unknown edge condition");
                }
            }
        }
        Ok(())
    }

    /// A target is spawnable when every incoming edge is satisfied:
    /// `on_complete` sources have a done task row; `on_result` sources have
    /// a recorded result matching the condition. Timer/event incoming edges
    /// are driven directly and never count toward fan-in.
    async fn target_spawnable(
        &mut self,
        recipe: &Recipe,
        run: &WorkflowRun,
        target: &str,
    ) -> QueryResult<bool> {
        let incoming: Vec<&RecipeEdge> = recipe.edges.iter().filter(|e| e.to == target).collect();
        if incoming.is_empty() {
            return Ok(false);
        }
        for edge in incoming {
            match edge.condition_type.as_str() {
                "on_complete" => {
                    if !self.node_has_done_task(run, &edge.from).await? {
                        return Ok(false);
                    }
                }
                "on_result" => {
                    let Some(result) = self.node_result(run, &edge.from) else {
                        return Ok(false);
                    };
                    let matched = edge
                        .condition_value
                        .as_ref()
                        .is_some_and(|condition| key_match(&result, condition));
                    if !matched {
                        return Ok(false);
                    }
                }
                "timer" | "event" => return Ok(false),
                other => {
                    tracing::warn!(condition = other, "unknown incoming condition");
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Task ids of the satisfied source nodes of `target` (for audit links
    /// and tag copying on the spawned step).
    async fn satisfied_sources(
        &mut self,
        recipe: &Recipe,
        run: &WorkflowRun,
        target: &str,
    ) -> QueryResult<Vec<u64>> {
        let mut ids = Vec::new();
        for edge in recipe.edges.iter().filter(|e| e.to == target) {
            if let Some(id) = self.node_done_task_id(run, &edge.from).await? {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    async fn node_has_done_task(&mut self, run: &WorkflowRun, node_id: &str) -> QueryResult<bool> {
        Ok(self.node_done_task_id(run, node_id).await?.is_some())
    }

    async fn node_done_task_id(
        &mut self,
        run: &WorkflowRun,
        node_id: &str,
    ) -> QueryResult<Option<u64>> {
        let rows = toasty::sql::query(
            r#"SELECT id FROM tasks WHERE workflow_run_id = ?1 AND node_id = ?2
               AND done = 1 AND deleted_at IS NULL ORDER BY id LIMIT 1"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(run.id as i64)
        .bind(node_id)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "find done node task",
        })?;
        Ok(rows
            .first()
            .and_then(|row| row.as_record())
            .and_then(|record| record.first().and_then(|v| v.to_i64()))
            .map(|id| id as u64))
    }

    fn node_result(&self, run: &WorkflowRun, node_id: &str) -> Option<Value> {
        run.step_results.0.get(node_id).cloned()
    }

    /// Create a task for a recipe node, linked to the run. Sources become
    /// `blocked_by` audit links (already done, so never user-blocking), the
    /// first source is recorded as `source_task_id`, and the source's
    /// direct tags are copied like a follow-up.
    async fn spawn_node_task(
        &mut self,
        recipe: &Recipe,
        run: &WorkflowRun,
        node_id: &str,
        parent_id: Option<u64>,
        sources: &[u64],
        blocked_until: Option<u64>,
    ) -> QueryResult<crate::Task> {
        let Some(node) = recipe.nodes.iter().find(|n| n.id == node_id) else {
            return Err(invalid(format!("recipe node `{node_id}` not found")));
        };
        let task = self
            .create_task(
                crate::Task::create()
                    .title(node.title.clone())
                    .description(node.description.clone())
                    .workflow_run_id(Some(run.id))
                    .node_id(Some(node.id.clone()))
                    .parent_id(parent_id)
                    .blocked_until(blocked_until)
                    .source_task_id(sources.first().copied()),
            )
            .await?;
        for source in sources {
            if let Err(e) = self.add_blocker(task.id, *source).await {
                // Cycle guard is defensive only: audit links point from a
                // fresh task to a done prerequisite, so a cycle cannot form.
                tracing::warn!(task = task.id, source = *source, "audit link skipped: {e}");
            }
        }
        let mut seen: HashSet<u64> = HashSet::new();
        for source in sources {
            for tag in self.get_direct_task_tags(*source).await? {
                if seen.insert(tag.id) {
                    self.assign_tag_to_task(task.id, &tag.name).await?;
                }
            }
        }
        Ok(task)
    }

    /// Re-open an automated task (done=false, result cleared) and tombstone
    /// its pending approval subtasks so a revised pass can spawn fresh ones.
    async fn retry_task(
        &mut self,
        task_id: u64,
        recipe: &Recipe,
        run: &WorkflowRun,
    ) -> QueryResult<()> {
        let task = self.get_task(task_id).await?;
        if task.workflow_run_id != Some(run.id) {
            return Ok(());
        }
        let Some(node_id) = task.node_id.clone() else {
            return Ok(());
        };
        if !recipe.nodes.iter().any(|n| n.id == node_id && n.ai) {
            return Ok(());
        }
        crate::Task::update_by_id(task_id)
            .done(false)
            .completed_at(None)
            .exec(&mut self.db)
            .await
            .context(crate::error::UpdateTaskSnafu { id: task_id })?;
        let mut results = run.step_results.0.clone();
        if let Some(obj) = results.as_object_mut() {
            obj.remove(&node_id);
        }
        WorkflowRun::update_by_id(run.id)
            .step_results(toasty::Json(results))
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "clear workflow result on retry",
            })?;
        // Tombstone pending approval subtasks; the completed approval that
        // triggered the retry stays visible as done.
        let rows = toasty::sql::query(
            r#"SELECT id FROM tasks WHERE parent_id = ?1 AND done = 0 AND deleted_at IS NULL"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(task_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list pending approval subtasks",
        })?;
        for row in rows {
            if let Some(id) = row_id(&row) {
                self.tombstone_task(id).await?;
            }
        }
        Ok(())
    }

    /// A run completes when every step task is done (targets are created
    /// lazily the moment their prerequisites are satisfied, so no pending
    /// task implies no pending work).
    async fn check_run_complete(&mut self, run_id: u64) -> QueryResult<()> {
        let Some(run) = self.find_run(run_id).await? else {
            return Ok(());
        };
        if run.status != "active" {
            return Ok(());
        }
        let rows = toasty::sql::query(
            r#"SELECT id FROM tasks WHERE workflow_run_id = ?1 AND deleted_at IS NULL AND done = 0"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(run_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "count pending run tasks",
        })?;
        if rows.is_empty() {
            WorkflowRun::update_by_id(run_id)
                .status("completed".to_string())
                .completed_at(Some(jiff::Timestamp::now()))
                .exec(&mut self.db)
                .await
                .context(crate::error::QueryTagsSnafu {
                    context: "complete workflow run",
                })?;
        }
        Ok(())
    }
}

fn parse_run_row(record: &toasty::stmt::Value) -> Option<WorkflowRun> {
    let toasty::stmt::Value::Record(record) = record else {
        return None;
    };
    let id = record.first().and_then(|v| v.to_i64())? as u64;
    let recipe_id = record.get(1).and_then(|v| v.to_i64())? as u64;
    let schedule_id = record.get(2).and_then(|v| v.to_i64()).map(|id| id as u64);
    let status = record.get(3).and_then(|v| v.as_str())?.to_string();
    let params = record
        .get(4)
        .and_then(|v| v.as_str())
        .and_then(|raw| serde_json::from_str(raw).ok())
        .map(toasty::Json)?;
    let step_results = record
        .get(5)
        .and_then(|v| v.as_str())
        .and_then(|raw| serde_json::from_str(raw).ok())
        .map(toasty::Json)?;
    let created_at = record
        .get(6)
        .and_then(|v| v.as_str())
        .and_then(|raw| raw.parse::<jiff::Timestamp>().ok())?;
    let completed_at = record
        .get(7)
        .and_then(|v| v.as_str())
        .and_then(|raw| raw.parse::<jiff::Timestamp>().ok());
    Some(WorkflowRun {
        id,
        recipe_id,
        schedule_id,
        status,
        params,
        step_results,
        created_at,
        completed_at,
    })
}

/// Merge provided params with the recipe schema: defaults fill in missing
/// values, unknown names and type mismatches are rejected, `required`
/// params without a default must be provided.
fn resolve_params(recipe: &Recipe, provided: Value) -> Result<Value, String> {
    let provided = provided.as_object().ok_or("run params must be a JSON object")?;
    let mut out = serde_json::Map::new();
    for (name, schema) in &recipe.params {
        match provided.get(name) {
            Some(value) => {
                if !type_matches(value, &schema.r#type) {
                    return Err(format!(
                        "param `{name}` must be of type `{}`",
                        schema.r#type
                    ));
                }
                out.insert(name.clone(), value.clone());
            }
            None => {
                if let Some(default) = &schema.default {
                    out.insert(name.clone(), default.clone());
                } else if schema.required {
                    return Err(format!("required param `{name}` is missing"));
                }
            }
        }
    }
    for name in provided.keys() {
        if !recipe.params.contains_key(name) {
            return Err(format!("unknown param `{name}`"));
        }
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_recipe(store: &mut TodoStore, json: Value) -> u64 {
        store
            .create_recipe("test", json)
            .await
            .expect("recipe should validate")
            .id
    }

    #[test]
    fn test_key_match() {
        // Extra result keys are ignored; {} matches anything.
        assert!(key_match(
            &serde_json::json!({ "approved": true, "notes": "x" }),
            &serde_json::json!({ "approved": true })
        ));
        assert!(!key_match(
            &serde_json::json!({ "approved": false }),
            &serde_json::json!({ "approved": true })
        ));
        assert!(key_match(&serde_json::json!({ "anything": 1 }), &serde_json::json!({})));
        // Scalars match exactly.
        assert!(key_match(&serde_json::json!("ok"), &serde_json::json!("ok")));
        assert!(!key_match(&serde_json::json!("ok"), &serde_json::json!("no")));
    }

    #[tokio::test]
    async fn test_case1_packing_list_spawns_all_start_nodes() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let id = make_recipe(
            &mut store,
            serde_json::json!({
                "name": "Packing List",
                "nodes": [
                    { "id": "swimsuit", "kind": "action", "title": "Pack swimsuit" },
                    { "id": "sunscreen", "kind": "action", "title": "Pack sunscreen" },
                    { "id": "towel", "kind": "action", "title": "Pack towel" }
                ],
                "edges": []
            }),
        )
        .await;
        let run = store.create_run(id, serde_json::json!({}), None).await?;
        let view = store.workflow_run_view(run.id).await?.unwrap();
        assert_eq!(view.steps.len(), 3);
        Ok(())
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn test_case2_relative_timer_blocks_until_scheduled() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let id = make_recipe(
            &mut store,
            serde_json::json!({
                "name": "Follow-up",
                "nodes": [
                    { "id": "submit", "kind": "action", "title": "Submit application" },
                    { "id": "followup", "kind": "action", "title": "Follow up" }
                ],
                "edges": [
                    { "from": "submit", "to": "followup", "condition_type": "timer", "condition_value": "4 days" }
                ]
            }),
        )
        .await;
        let run = store.create_run(id, serde_json::json!({}), None).await?;
        // Only the start node exists; the follow-up is not materialized yet.
        let before = store.workflow_run_view(run.id).await?.unwrap();
        assert_eq!(before.steps.len(), 1);
        assert_eq!(before.steps[0].node.id, "submit");

        let submit = before.steps[0].task.id;
        store.update_task_done(submit, true).await?;
        let after = store.workflow_run_view(run.id).await?.unwrap();
        assert_eq!(after.steps.len(), 2);
        let followup = after.steps.iter().find(|s| s.node.id == "followup").unwrap();
        assert!(followup.task.blocked_until.is_some());
        assert!(followup.task.blocked_until.unwrap() > now());
        // The run is still active while the step waits.
        assert_eq!(store.find_run(run.id).await?.unwrap().status, "active");
        Ok(())
    }

    #[tokio::test]
    async fn test_case4_on_result_branches_and_approval_subtask() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let id = make_recipe(
            &mut store,
            serde_json::json!({
                "name": "Gatekeeper",
                "nodes": [
                    { "id": "draft", "kind": "action", "title": "Draft the email", "ai": true },
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
        )
        .await;
        let run = store.create_run(id, serde_json::json!({}), None).await?;
        let view = store.workflow_run_view(run.id).await?.unwrap();
        let draft = view.steps.iter().find(|s| s.node.id == "draft").unwrap().task.id;
        store.complete_workflow_step(draft, serde_json::json!({})).await?;
        let view = store.workflow_run_view(run.id).await?.unwrap();
        let approval = view.steps.iter().find(|s| s.node.id == "approve").unwrap();
        // Approval materializes as a subtask of the automated draft.
        assert_eq!(approval.task.parent_id, Some(draft));
        assert!(view.steps.iter().all(|s| s.node.id != "send"));

        // Approve: send spawns, edit never exists.
        store
            .complete_workflow_step(approval.task.id, serde_json::json!({ "approved": true }))
            .await?;
        let view = store.workflow_run_view(run.id).await?.unwrap();
        assert!(view.steps.iter().any(|s| s.node.id == "send"));
        assert!(view.steps.iter().all(|s| s.node.id != "edit"));
        Ok(())
    }

    #[tokio::test]
    async fn test_case4_rejection_retriggers_automated_task() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let id = make_recipe(
            &mut store,
            serde_json::json!({
                "name": "Gatekeeper",
                "nodes": [
                    { "id": "draft", "kind": "action", "title": "Draft the email", "ai": true },
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
        )
        .await;
        let run = store.create_run(id, serde_json::json!({}), None).await?;
        let view = store.workflow_run_view(run.id).await?.unwrap();
        let draft = view.steps.iter().find(|s| s.node.id == "draft").unwrap().task.id;
        store.complete_workflow_step(draft, serde_json::json!({})).await?;
        let view = store.workflow_run_view(run.id).await?.unwrap();
        let approval = view.steps.iter().find(|s| s.node.id == "approve").unwrap().task.id;
        store
            .complete_workflow_step(approval, serde_json::json!({ "approved": false }))
            .await?;
        let view = store.workflow_run_view(run.id).await?.unwrap();
        // Edit spawned and the draft was re-opened for another pass.
        assert!(view.steps.iter().any(|s| s.node.id == "edit"));
        let draft_step = view.steps.iter().find(|s| s.node.id == "draft").unwrap();
        assert!(!draft_step.task.done);
        Ok(())
    }

    #[tokio::test]
    async fn test_case5_fan_in_spawns_only_when_all_done() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let id = make_recipe(
            &mut store,
            serde_json::json!({
                "name": "Parallel Research",
                "nodes": [
                    { "id": "a", "kind": "action", "title": "Research A" },
                    { "id": "b", "kind": "action", "title": "Research B" },
                    { "id": "c", "kind": "action", "title": "Research C" },
                    { "id": "final", "kind": "action", "title": "Write final summary" }
                ],
                "edges": [
                    { "from": "a", "to": "final", "condition_type": "on_complete" },
                    { "from": "b", "to": "final", "condition_type": "on_complete" },
                    { "from": "c", "to": "final", "condition_type": "on_complete" }
                ]
            }),
        )
        .await;
        let run = store.create_run(id, serde_json::json!({}), None).await?;
        let view = store.workflow_run_view(run.id).await?.unwrap();
        let ids: HashMap<String, u64> = view
            .steps
            .iter()
            .map(|s| (s.node.id.clone(), s.task.id))
            .collect();
        assert!(!ids.contains_key("final"));

        store.update_task_done(ids["a"], true).await?;
        store.update_task_done(ids["b"], true).await?;
        let view = store.workflow_run_view(run.id).await?.unwrap();
        assert!(view.steps.iter().all(|s| s.node.id != "final"));

        store.update_task_done(ids["c"], true).await?;
        let view = store.workflow_run_view(run.id).await?.unwrap();
        assert!(view.steps.iter().any(|s| s.node.id == "final"));
        // Completing the summary finishes the run.
        let final_id = view
            .steps
            .iter()
            .find(|s| s.node.id == "final")
            .map(|s| s.task.id)
            .unwrap();
        store.update_task_done(final_id, true).await?;
        assert_eq!(store.find_run(run.id).await?.unwrap().status, "completed");
        Ok(())
    }

    #[tokio::test]
    async fn test_case6_conditional_duration_from_params() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let id = make_recipe(
            &mut store,
            serde_json::json!({
                "name": "Conditional Urgency",
                "params": { "urgent": { "type": "boolean", "default": false } },
                "nodes": [
                    { "id": "review", "kind": "action", "title": "Review the lead" },
                    { "id": "call", "kind": "action", "title": "Call client" }
                ],
                "edges": [
                    { "from": "review", "to": "call", "condition_type": "timer", "condition_value": { "if": "param:urgent", "then": "0 days", "else": "3 days" } }
                ]
            }),
        )
        .await;
        // Urgent run: the call appears immediately (blocked_until = now).
        let urgent = store
            .create_run(id, serde_json::json!({ "urgent": true }), None)
            .await?;
        let view = store.workflow_run_view(urgent.id).await?.unwrap();
        let review = view.steps.iter().find(|s| s.node.id == "review").unwrap().task.id;
        store.update_task_done(review, true).await?;
        let view = store.workflow_run_view(urgent.id).await?.unwrap();
        let call = view.steps.iter().find(|s| s.node.id == "call").unwrap();
        assert!(call.task.blocked_until.is_some());
        assert!(call.task.blocked_until.unwrap() <= now() + 60);

        // Non-urgent run: three-day wait.
        let calm = store
            .create_run(id, serde_json::json!({ "urgent": false }), None)
            .await?;
        let view = store.workflow_run_view(calm.id).await?.unwrap();
        let review = view.steps.iter().find(|s| s.node.id == "review").unwrap().task.id;
        store.update_task_done(review, true).await?;
        let view = store.workflow_run_view(calm.id).await?.unwrap();
        let call = view.steps.iter().find(|s| s.node.id == "call").unwrap();
        assert!(call.task.blocked_until.unwrap() > now() + 2 * 86400);
        Ok(())
    }

    #[tokio::test]
    async fn test_case7_event_waiter_and_trigger() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let id = make_recipe(
            &mut store,
            serde_json::json!({
                "name": "Contract Follow-up",
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
        )
        .await;
        let run = store.create_run(id, serde_json::json!({}), None).await?;
        let view = store.workflow_run_view(run.id).await?.unwrap();
        let send = view.steps.iter().find(|s| s.node.id == "send_email").unwrap().task.id;
        store.update_task_done(send, true).await?;
        let view = store.workflow_run_view(run.id).await?.unwrap();
        let waiter = view.steps.iter().find(|s| s.node.id == "await_reply").unwrap();
        assert!(waiter.incoming_event.is_some());
        assert!(view.steps.iter().all(|s| s.node.id != "draft_contract"));

        // Wrong event name fires nothing; the right one spawns the contract.
        assert_eq!(store.trigger_event(run.id, "nope").await?, 0);
        assert_eq!(store.trigger_event(run.id, "client_replied").await?, 1);
        let view = store.workflow_run_view(run.id).await?.unwrap();
        assert!(view.steps.iter().any(|s| s.node.id == "draft_contract"));
        Ok(())
    }

    #[tokio::test]
    async fn test_case8_timer_task_tickable_after_wait() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let id = make_recipe(
            &mut store,
            serde_json::json!({
                "name": "Order Delivery",
                "nodes": [
                    { "id": "place_order", "kind": "action", "title": "Place order" },
                    { "id": "mark_received", "kind": "action", "title": "Mark package as received" }
                ],
                "edges": [
                    { "from": "place_order", "to": "mark_received", "condition_type": "timer", "condition_value": "3 days" }
                ]
            }),
        )
        .await;
        let run = store.create_run(id, serde_json::json!({}), None).await?;
        let view = store.workflow_run_view(run.id).await?.unwrap();
        let order = view.steps.iter().find(|s| s.node.id == "place_order").unwrap().task.id;
        store.update_task_done(order, true).await?;
        let view = store.workflow_run_view(run.id).await?.unwrap();
        let received = view.steps.iter().find(|s| s.node.id == "mark_received").unwrap();
        assert!(received.task.blocked_until.is_some());
        assert!(received.task.blocked_until.unwrap() > now());
        Ok(())
    }

    #[tokio::test]
    async fn test_invalid_recipes_rejected() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        // Cycle.
        assert!(
            store
                .create_recipe(
                    "cycle",
                    serde_json::json!({
                        "name": "Cycle",
                        "nodes": [
                            { "id": "a", "kind": "action", "title": "A" },
                            { "id": "b", "kind": "action", "title": "B" }
                        ],
                        "edges": [
                            { "from": "a", "to": "b", "condition_type": "on_complete" },
                            { "from": "b", "to": "a", "condition_type": "on_complete" }
                        ]
                    }),
                )
                .await
                .is_err()
        );
        // Timer edge must target an action.
        assert!(
            store
                .create_recipe(
                    "bad-timer",
                    serde_json::json!({
                        "name": "Bad",
                        "nodes": [
                            { "id": "a", "kind": "action", "title": "A" },
                            { "id": "w", "kind": "event", "title": "W" }
                        ],
                        "edges": [
                            { "from": "a", "to": "w", "condition_type": "timer", "condition_value": "1 day" }
                        ]
                    }),
                )
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_cancel_run_tombstones_steps() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let id = make_recipe(
            &mut store,
            serde_json::json!({
                "name": "Packing List",
                "nodes": [
                    { "id": "a", "kind": "action", "title": "A" },
                    { "id": "b", "kind": "action", "title": "B" }
                ],
                "edges": []
            }),
        )
        .await;
        let run = store.create_run(id, serde_json::json!({}), None).await?;
        store.cancel_run(run.id).await?;
        assert_eq!(store.find_run(run.id).await?.unwrap().status, "cancelled");
        let view = store.workflow_run_view(run.id).await?;
        assert!(view.is_none(), "cancelled run steps are tombstoned");
        Ok(())
    }
}