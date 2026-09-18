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
    /// `on_result` edges. The reserved `@notes` key holds a coding run's
    /// ordered log (see [`RunNote`]).
    pub step_results: toasty::Json<Value>,
    #[default(jiff::Timestamp::now())]
    pub created_at: jiff::Timestamp,
    pub completed_at: Option<jiff::Timestamp>,
    /// The feature task a coding run is attached to. Phase steps materialize
    /// as its subtasks and completing it completes the run.
    pub root_task_id: Option<u64>,
    /// Coding runs: the feature branch, the branch it was cut from, and
    /// `proposed` | `active` | `merged` | `abandoned`. Kept after
    /// cancellation so the branch stays visible for cleanup.
    pub branch: Option<String>,
    pub base_branch: Option<String>,
    pub branch_status: Option<String>,
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
    /// Coding recipes only: which phase of the coding workflow this node
    /// drives (`interview` | `spec` | `implement` | `review` | `merge`).
    /// The app keys its stepper and phase prompts off this.
    pub phase: Option<String>,
    /// Materialize as a subtask of the run's root task instead of at the top
    /// level (coding phases hang off the feature task).
    pub subtask: bool,
    /// Coding recipes only: `step` materializes a run step that is *not* a
    /// subtask, so no subtask surface ever shows it. `subtask` is the
    /// explicit spelling of today's lazy spawn. Exactly one of the two.
    pub role: Option<String>,
    /// Node to re-open when this approval rejects. Defaults to the approval's
    /// feeder task, which is what generic recipes expect; coding recipes point
    /// it back at the interview step so a rejection re-specs the feature.
    pub retrigger_node: Option<String>,
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
    /// When set, the automation owns a managed tag of this name: enabling
    /// creates it, disabling removes it (see `trip::enable_managed_recipe`).
    pub managed_tag: Option<String>,
    pub params: HashMap<String, ParamSchema>,
    pub missed_policy: String,
    pub nodes: Vec<RecipeNode>,
    pub edges: Vec<RecipeEdge>,
}

/// Recipe summary for UI recipe pickers (the full graph lives in
/// `recipe_json`). `active_runs` is the number of runs currently in
/// `active` status, so the automations panel can show enable/disable
/// state without fetching the runs themselves. `managed_tag` is set for
/// automations that own a managed tag instead of spawning runs, and
/// `managed_enabled` tells whether that tag currently exists.
#[derive(Debug, Clone)]
pub struct RecipeMeta {
    pub id: u64,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub managed_tag: Option<String>,
    pub managed_enabled: bool,
    pub active_runs: usize,
    /// Whether the recipe's nodes declare coding phases. Phased recipes are
    /// started from a feature task, not from the workflows panel's start row.
    pub phased: bool,
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

/// Phase names a coding recipe node may declare.
pub const CODING_PHASES: [&str; 5] = ["interview", "spec", "implement", "review", "merge"];

/// Node roles a recipe may declare (decision #3): `step` is run scaffolding,
/// `subtask` is a real subtask the engine lazy-spawns.
pub const RECIPE_ROLES: [&str; 2] = ["step", "subtask"];

/// The `tasks.role` value of an engine-materialized run step.
pub const STEP_ROLE: &str = "step";

/// Whether a task row is a workflow step rather than a real subtask.
pub fn is_step(task: &crate::Task) -> bool {
    task.role.as_deref() == Some(STEP_ROLE)
}

/// Whether a materialized task row for `node` is a run step.
fn node_is_step(node: &RecipeNode) -> bool {
    node.role.as_deref() == Some("step")
}

/// How a subtask's spec coverage stands (decision #10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtaskCoverage {
    /// The subtask holds its own spec (`tasks.spec` non-empty).
    Own,
    /// The umbrella spec was accepted as covering it (`spec_covered_at`), or
    /// the subtask was promoted to a run of its own (decision #32).
    Covered,
    /// Neither: flagged in the subtask's own details, never blocking.
    Unspecced,
    /// Done subtasks are never flagged and leave the count (decision #11).
    NotApplicable,
}

impl SubtaskCoverage {
    /// The `spec` state the MCP context and the pane show: `own` | `covered`
    /// | `none`. `NotApplicable` (done) reports `none` and is excluded from
    /// the count, so a done subtask is never flagged.
    pub fn label(self) -> &'static str {
        match self {
            SubtaskCoverage::Own => "own",
            SubtaskCoverage::Covered => "covered",
            SubtaskCoverage::Unspecced | SubtaskCoverage::NotApplicable => "none",
        }
    }

    /// Whether the subtask counts as covered by the run's coverage line.
    pub fn is_covered(self) -> bool {
        matches!(self, SubtaskCoverage::Own | SubtaskCoverage::Covered)
    }
}

/// `spec covers covered/total subtasks`, over the open subtasks (decision #11;
/// done subtasks leave both numbers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageSummary {
    pub covered: usize,
    pub total: usize,
}

/// The coverage of one subtask. Done wins over everything: a finished subtask
/// is never flagged, even when nothing was ever written for it.
pub fn subtask_coverage(task: &crate::TaskWithMeta) -> SubtaskCoverage {
    if task.done {
        return SubtaskCoverage::NotApplicable;
    }
    if task
        .spec
        .as_deref()
        .is_some_and(|spec| !spec.trim().is_empty())
    {
        return SubtaskCoverage::Own;
    }
    if task.spec_covered_at.is_some() {
        return SubtaskCoverage::Covered;
    }
    SubtaskCoverage::Unspecced
}

/// The run's coverage line. A description never counts as a spec (decision
/// #6), so only `own` and the explicit mark do.
pub fn coverage_summary(subtasks: &[crate::TaskWithMeta]) -> CoverageSummary {
    let mut summary = CoverageSummary {
        covered: 0,
        total: 0,
    };
    for task in subtasks {
        let coverage = subtask_coverage(task);
        if coverage == SubtaskCoverage::NotApplicable {
            continue;
        }
        summary.total += 1;
        if coverage.is_covered() {
            summary.covered += 1;
        }
    }
    summary
}

/// One `<subtask, spec>` pair in a [`TodoStore::save_subtask_specs`] call.
#[derive(Debug, Clone)]
pub struct SubtaskSpecInput {
    pub task_id: u64,
    pub spec: String,
}

/// Reserved `workflow_runs.step_results` key holding the run's ordered log.
/// Node ids are `[A-Za-z0-9_-]+`, so `@` cannot collide with a node result.
/// The log must survive `retry_task` clearing `step_results[node_id]`, which is
/// why it lives on the run rather than on the step result.
pub const RUN_NOTES_KEY: &str = "@notes";

/// One entry of a run's ordered log: an annotation, a rejection, an approval,
/// a spec save, a branch change, or a merge.
///
/// `kind` is `annotation` | `reject` | `approve` | `spec` | `branch` | `merge`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunNote {
    pub at: u64,
    pub phase: String,
    pub node_id: String,
    pub kind: String,
    pub body: String,
}

impl RunNote {
    pub fn new(
        kind: impl Into<String>,
        phase: impl Into<String>,
        node_id: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            at: now_secs(),
            phase: phase.into(),
            node_id: node_id.into(),
            kind: kind.into(),
            body: body.into(),
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "at": self.at,
            "phase": self.phase,
            "node_id": self.node_id,
            "kind": self.kind,
            "body": self.body,
        })
    }

    pub fn from_json(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        Some(Self {
            at: object.get("at").and_then(Value::as_u64).unwrap_or(0),
            phase: object
                .get("phase")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            node_id: object
                .get("node_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            kind: object
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            body: object
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }
}

/// The run's ordered log, oldest first. Missing or malformed entries are
/// skipped rather than failing the whole read.
pub fn run_notes(results: &Value) -> Vec<RunNote> {
    results
        .get(RUN_NOTES_KEY)
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter_map(RunNote::from_json).collect())
        .unwrap_or_default()
}

/// The coding recipe declarations shipped with the app. `coding-task` is the
/// full feature pipeline; `coding-sub-interview` is the single-node run used
/// when a sub-task needs its own interview round. `coding-task` v2 declares
/// its phase nodes as `role: "step"` (decision #3) so run scaffolding stops
/// masquerading as a subtask.
pub fn coding_recipes() -> Vec<(&'static str, Value)> {
    vec![
        (
            "coding-task",
            serde_json::json!({
                "name": "Coding task",
                "description": "AI-assisted feature development: interview and spec, approve, implement on a branch, review and annotate, then merge.",
                "params": {
                    "branch": { "type": "string", "default": "" }
                },
                "nodes": [
                    {
                        "id": "interview",
                        "kind": "action",
                        "title": "Interview & spec the feature",
                        "description": "Interview & spec the feature out (the app composes the full interview prompt), ask clarifying questions, then save the spec.",
                        "ai": true,
                        "phase": "interview",
                        "role": "step"
                    },
                    {
                        "id": "spec",
                        "kind": "action",
                        "title": "Approve the spec",
                        "description": "Read the spec. Approve to start implementation, or reject with notes to re-interview.",
                        "phase": "spec",
                        "role": "step",
                        "approval": true,
                        "retrigger_on_reject": true,
                        "retrigger_node": "interview"
                    },
                    {
                        "id": "implement",
                        "kind": "action",
                        "title": "Implement the feature",
                        "description": "Implement the approved spec on the feature branch, then confirm.",
                        "ai": true,
                        "phase": "implement",
                        "role": "step"
                    },
                    {
                        "id": "review",
                        "kind": "action",
                        "title": "Review & annotate",
                        "description": "Review the implementation, annotate findings, then approve to merge or reject to re-spec.",
                        "ai": true,
                        "phase": "review",
                        "role": "step",
                        "approval": true,
                        "retrigger_on_reject": true,
                        "retrigger_node": "interview"
                    },
                    {
                        "id": "merge",
                        "kind": "action",
                        "title": "Merge the branch",
                        "description": "Merge the feature branch into its base branch and mark the feature complete.",
                        "phase": "merge",
                        "role": "step"
                    }
                ],
                "edges": [
                    { "from": "interview", "to": "spec", "condition_type": "on_result", "condition_value": {} },
                    { "from": "spec", "to": "implement", "condition_type": "on_result", "condition_value": { "approved": true } },
                    { "from": "implement", "to": "review", "condition_type": "on_result", "condition_value": {} },
                    { "from": "review", "to": "merge", "condition_type": "on_result", "condition_value": { "approved": true } }
                ]
            }),
        ),
        (
            "coding-sub-interview",
            serde_json::json!({
                "name": "Sub-task interview",
                "description": "Interview a single under-specified sub-task and save its spec.",
                "nodes": [
                    {
                        "id": "interview",
                        "kind": "action",
                        "title": "Interview the sub-task",
                        "description": "Interview the sub-task out (the app composes the full interview prompt), then save the spec.",
                        "ai": true,
                        "phase": "interview",
                        "subtask": true
                    }
                ],
                "edges": []
            }),
        ),
    ]
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
    let managed_tag = root
        .get("managed_tag")
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
        let phase = obj
            .get("phase")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(phase) = &phase
            && !CODING_PHASES.contains(&phase.as_str())
        {
            return Err(format!(
                "node `{id}`: invalid phase `{phase}` (expected one of {})",
                CODING_PHASES.join(", ")
            ));
        }
        let subtask = obj
            .get("subtask")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if subtask && kind != "action" {
            return Err(format!(
                "node `{id}`: `subtask` is only valid on action nodes"
            ));
        }
        let role = obj.get("role").and_then(|v| v.as_str()).map(str::to_string);
        if let Some(role) = &role {
            if !RECIPE_ROLES.contains(&role.as_str()) {
                return Err(format!(
                    "node `{id}`: invalid role `{role}` (expected one of {})",
                    RECIPE_ROLES.join(", ")
                ));
            }
            if subtask {
                return Err(format!(
                    "node `{id}`: `role` and `subtask: true` declare the same thing; keep one"
                ));
            }
            if role == "step" {
                if kind != "action" {
                    return Err(format!(
                        "node `{id}`: `role: \"step\"` is only valid on action nodes"
                    ));
                }
                if obj.get("phase").and_then(|v| v.as_str()).is_none() {
                    return Err(format!(
                        "node `{id}`: `role: \"step\"` requires a `phase`"
                    ));
                }
            }
        }
        let retrigger_node = obj
            .get("retrigger_node")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if retrigger_node.is_some() && !retrigger_on_reject {
            return Err(format!(
                "node `{id}`: `retrigger_node` requires `retrigger_on_reject`"
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
            phase,
            subtask,
            role,
            retrigger_node,
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
        // `retrigger_node` re-opens a node on rejection, so it must name an
        // automated node of this recipe (a plain node has nothing to re-run).
        if let Some(target_id) = &node.retrigger_node {
            match nodes.iter().find(|n| &n.id == target_id) {
                None => {
                    return Err(format!(
                        "node `{}`: `retrigger_node` references unknown node `{target_id}`",
                        node.id
                    ));
                }
                Some(target) if target.id == node.id => {
                    return Err(format!(
                        "node `{}`: `retrigger_node` must not be itself",
                        node.id
                    ));
                }
                Some(target) if !target.ai => {
                    return Err(format!(
                        "node `{}`: `retrigger_node` must reference an `ai: true` node",
                        node.id
                    ));
                }
                Some(_) => {}
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
        managed_tag,
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

/// The log entry implied by a completed approval step: `approve` or `reject`,
/// carrying the reviewer's notes. Non-approval steps log nothing.
fn phase_note_for(recipe: &Recipe, node_id: &str, result: &Value) -> Option<RunNote> {
    let node = recipe.nodes.iter().find(|n| n.id == node_id)?;
    if !node.approval {
        return None;
    }
    let approved = result
        .get("approved")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let notes = result
        .get("notes")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let (kind, body) = if approved {
        (
            "approve",
            if notes.is_empty() {
                "Approved".to_string()
            } else {
                notes
            },
        )
    } else {
        ("reject", notes)
    };
    Some(RunNote::new(
        kind,
        node.phase.clone().unwrap_or_default(),
        node.id.clone(),
        body,
    ))
}

/// Filler words dropped from an issue-derived branch slug (decision 16).
const BRANCH_STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "of", "to", "in", "on", "for", "with", "at", "by",
    "from", "is", "are", "be", "as", "it", "its", "this", "that", "into", "via", "when",
    "add", "adds", "fix", "fixes", "fixed", "feat", "feature", "implement", "implements",
    "support", "supports", "update", "updates", "improve", "improves", "make", "makes",
    "allow", "allows", "use", "using", "should", "must",
];

/// The slug caps of decision 16: a few words, a short name.
const BRANCH_SLUG_WORDS: usize = 5;
const BRANCH_SLUG_CHARS: usize = 60;

/// The slug for an issue-backed branch: the title's meaningful words, lower
/// cased and hyphenated, with filler words dropped and the result truncated to
/// [`BRANCH_SLUG_WORDS`] words / [`BRANCH_SLUG_CHARS`] characters, then run
/// through [`normalize_branch_name`] so non-ASCII and emoji are dropped rather
/// than passed through (decision 16).
///
/// A title made entirely of filler words falls back to its own words, so the
/// slug is empty only for a title with no usable characters at all.
pub fn issue_branch_slug(title: &str) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let meaningful: Vec<&String> = words
        .iter()
        .filter(|word| !BRANCH_STOPWORDS.contains(&word.as_str()))
        .collect();
    let chosen: Vec<&str> = if meaningful.is_empty() {
        words.iter().map(String::as_str).collect()
    } else {
        meaningful.into_iter().map(String::as_str).collect()
    };
    let slug = chosen
        .into_iter()
        .take(BRANCH_SLUG_WORDS)
        .collect::<Vec<_>>()
        .join("-");
    let slug = normalize_branch_name(&slug).to_lowercase();
    truncate_branch_slug(&slug)
}

/// Cut a slug to the character cap on a word boundary, so the branch name is
/// still readable rather than mid-word.
fn truncate_branch_slug(slug: &str) -> String {
    if slug.chars().count() <= BRANCH_SLUG_CHARS {
        return slug.to_string();
    }
    let cut: String = slug.chars().take(BRANCH_SLUG_CHARS).collect();
    match cut.rfind('-') {
        Some(index) if index > 0 => cut[..index].to_string(),
        _ => cut,
    }
}

/// The branch name of an issue-backed run: `feature/<issue-number>-<slug>`
/// (decision 13), with the number kept even when the title yields no slug.
pub fn issue_branch_name(issue_number: u64, title: &str) -> String {
    let slug = issue_branch_slug(title);
    if slug.is_empty() {
        format!("feature/{issue_number}")
    } else {
        format!("feature/{issue_number}-{slug}")
    }
}

/// Sanitize a proposed branch name into something git accepts: whitespace
/// becomes `-`, other characters outside `[A-Za-z0-9._/-]` are dropped, and
/// leading/trailing separators are trimmed. Empty means "not usable".
pub fn normalize_branch_name(raw: &str) -> String {
    let mut out: String = raw
        .trim()
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
        .collect();
    out = out.trim_matches(|c| c == '/' || c == '-').to_string();
    if out.contains("..") || out == "." {
        return String::new();
    }
    out
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
                    managed_tag: None,
                    params: HashMap::new(),
                    missed_policy: "skip".to_string(),
                    nodes: Vec::new(),
                    edges: Vec::new(),
                });
            let managed_enabled = match &parsed.managed_tag {
                Some(tag_name) => self.get_tag_by_name(tag_name).await?.is_some(),
                None => false,
            };
            metas.push(RecipeMeta {
                id: recipe.id,
                slug: recipe.slug,
                name: parsed.name,
                description: parsed.description,
                managed_tag: parsed.managed_tag,
                managed_enabled,
                active_runs: active.get(&recipe.id).copied().unwrap_or(0),
                phased: parsed.nodes.iter().any(|node| node.phase.is_some()),
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
            r#"SELECT id, recipe_id, schedule_id, status, params, step_results, created_at, completed_at,
                      root_task_id, branch, base_branch, branch_status
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
            toasty::stmt::Type::I64,
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
            r#"SELECT id, recipe_id, schedule_id, status, params, step_results, created_at, completed_at,
                      root_task_id, branch, base_branch, branch_status
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
            toasty::stmt::Type::I64,
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
            r#"SELECT id, recipe_id, schedule_id, status, params, step_results, created_at, completed_at,
                      root_task_id, branch, base_branch, branch_status
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
            toasty::stmt::Type::I64,
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

    /// Insert the run row for a recipe: validate/merge params and record
    /// the run. Returns the run and its parsed recipe.
    async fn insert_run_row(
        &mut self,
        recipe_id: u64,
        params: Value,
        schedule_id: Option<u64>,
        root_task_id: Option<u64>,
    ) -> QueryResult<(WorkflowRun, Recipe)> {
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
            .root_task_id(root_task_id)
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "create workflow run",
            })?;
        Ok((run, recipe))
    }

    /// Instantiate a recipe: insert the run row, validate/merge params,
    /// and materialize every start node as a task.
    pub async fn create_run(
        &mut self,
        recipe_id: u64,
        params: Value,
        schedule_id: Option<u64>,
    ) -> QueryResult<WorkflowRun> {
        let (run, recipe) = self
            .insert_run_row(recipe_id, params, schedule_id, None)
            .await?;

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

    /// Create the run row for a managed-tag recipe without materializing
    /// start nodes: the tag's own flow (e.g. the travel trip builder)
    /// generates the run's content instead.
    pub async fn create_managed_run(&mut self, recipe_id: u64, params: Value) -> QueryResult<WorkflowRun> {
        let (run, _) = self.insert_run_row(recipe_id, params, None, None).await?;
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
        // A coding run keeps its branch reference while being cancelled: the
        // branch outlives the run and has to be cleaned up deliberately.
        let mut update = WorkflowRun::update_by_id(run_id).status("cancelled".to_string());
        if let Some(run) = self.find_run(run_id).await?
            && run.branch.is_some()
        {
            update = update.branch_status(Some("abandoned".to_string()));
        }
        update
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "cancel workflow run",
            })?;
        Ok(())
    }

    /// Cancel every active run of a recipe (the automations panel's
    /// "Disable"), and remove the managed tag the recipe owns, if any.
    /// Returns how many runs were cancelled.
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
        self.remove_managed_tag_content(recipe_id).await?;
        Ok(cancelled)
    }

    /// Active runs with their steps, for the run banner in the UI. Runs of
    /// managed-tag recipes are skipped: their content lives in the tag's
    /// task list (a travel trip is its checklist), not the workflow panel.
    pub async fn list_active_run_views(&mut self) -> QueryResult<Vec<RunView>> {
        let runs = self.list_workflow_runs().await?;
        let mut views = Vec::new();
        for run in runs {
            if run.status != "active" {
                continue;
            }
            let managed = match self.get_recipe(run.recipe_id).await {
                Ok(recipe) => parse_recipe(&recipe.recipe_json.0)
                    .map(|parsed| parsed.managed_tag.is_some())
                    .unwrap_or(false),
                Err(_) => true,
            };
            if managed {
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
        // Cancelled runs without a branch have nothing left to show (every
        // step is tombstoned). One that still holds a branch stays visible so
        // the branch can be cleaned up deliberately.
        if run.status == "cancelled" && run.branch.is_none() {
            return Ok(None);
        }
        let recipe_row = self.get_recipe(run.recipe_id).await?;
        let recipe = parse_recipe(&recipe_row.recipe_json.0).unwrap_or_else(|_| Recipe {
            name: recipe_row.slug.clone(),
            description: None,
            managed_tag: None,
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
        // Coding approvals are also appended to the run's ordered log, so the
        // review history survives the retrigger that clears this node's result.
        if let Some(note) = phase_note_for(&recipe, &node_id, &result) {
            match results_obj
                .entry(RUN_NOTES_KEY.to_string())
                .or_insert_with(|| Value::Array(Vec::new()))
            {
                Value::Array(entries) => entries.push(note.to_json()),
                slot => *slot = Value::Array(vec![note.to_json()]),
            }
        }
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

        // Approval rejection with `retrigger_on_reject`: re-open the automated
        // node so it can produce a revised result. `retrigger_node` names that
        // node (coding recipes point both gates back at the interview step so a
        // rejection re-specs the feature); without it the approval's feeder
        // task is re-opened, which is what generic recipes expect.
        if let Some(node) = recipe.nodes.iter().find(|n| n.id == node_id)
            && node.approval
            && node.retrigger_on_reject
            && is_rejection(&result)
        {
            let target = match node.retrigger_node.as_deref() {
                Some(target_node) => self.node_latest_task_id(&run, target_node).await?,
                None => task.parent_id,
            };
            if let Some(target) = target {
                self.retry_task(target, &recipe, &run).await?;
            }
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
        self.node_done_task_id_impl(run, node_id, false).await
    }

    /// The most recent done task of `node_id`, for re-opening the right step
    /// when a node ran more than once (re-spec cycles).
    async fn node_latest_task_id(
        &mut self,
        run: &WorkflowRun,
        node_id: &str,
    ) -> QueryResult<Option<u64>> {
        self.node_done_task_id_impl(run, node_id, true).await
    }

    async fn node_done_task_id_impl(
        &mut self,
        run: &WorkflowRun,
        node_id: &str,
        latest: bool,
    ) -> QueryResult<Option<u64>> {
        let order = if latest { "DESC" } else { "ASC" };
        let rows = toasty::sql::query(format!(
            r#"SELECT id FROM tasks WHERE workflow_run_id = ?1 AND node_id = ?2
               AND done = 1 AND deleted_at IS NULL ORDER BY id {order} LIMIT 1"#
        ))
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
        // A node that already has an open row in this run is re-used rather
        // than duplicated (decision #30): a re-opened interview is picked up
        // again, a pending implement survives a rewind, and each round's spec
        // gate is a fresh row because the previous one completed.
        if let Some(existing) = self.open_node_task_id(run.id, node_id).await? {
            return self.get_task(existing).await;
        }
        // Coding phases hang off the run's feature task; a step is always a
        // direct child of the run root (decision #15). An explicit parent
        // (an approval gate reviewing a step) always wins.
        let parent_id = parent_id.or_else(|| {
            (node.subtask || node_is_step(node))
                .then_some(run.root_task_id)
                .flatten()
        });
        let task = self
            .create_task(
                crate::Task::create()
                    .title(node.title.clone())
                    .description(node.description.clone())
                    .workflow_run_id(Some(run.id))
                    .node_id(Some(node.id.clone()))
                    .role(node_is_step(node).then(|| STEP_ROLE.to_string()))
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

    /// The id of `node_id`'s open (pending) task row in this run, if any.
    async fn open_node_task_id(
        &mut self,
        run_id: u64,
        node_id: &str,
    ) -> QueryResult<Option<u64>> {
        let rows = toasty::sql::query(
            r#"SELECT id FROM tasks WHERE workflow_run_id = ?1 AND node_id = ?2
               AND done = 0 AND deleted_at IS NULL ORDER BY id LIMIT 1"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(run_id as i64)
        .bind(node_id)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "find open node task",
        })?;
        Ok(rows.first().and_then(|row| row_id(row)))
    }

    /// Re-open an automated task (done=false, result cleared). Pending rows
    /// are left in place: they are re-used when the node runs again, so a
    /// rejection or a rewind adds a round instead of rewriting history
    /// (decision #30).
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

// ─── Coding runs ───────────────────────────────────────────────────────────

impl TodoStore {
    /// Install the built-in coding recipes. A missing slug is created; an
    /// existing one is left alone unless the shipped declaration changed, in
    /// which case the changed declaration becomes a new immutable version
    /// (runs pin the recipe row they started with, so existing runs keep the
    /// semantics they were created under).
    pub async fn ensure_coding_recipes(&mut self) -> QueryResult<()> {
        for (slug, recipe_json) in coding_recipes() {
            match self.recipe_id_by_slug(slug).await? {
                None => {
                    self.create_recipe(slug, recipe_json).await?;
                }
                Some(installed) => {
                    let current = self.get_recipe(installed).await?;
                    if current.recipe_json.0 != recipe_json {
                        self.create_recipe(slug, recipe_json).await?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The newest version of a recipe, by slug.
    pub async fn recipe_id_by_slug(&mut self, slug: &str) -> QueryResult<Option<u64>> {
        Ok(self
            .list_recipes()
            .await?
            .iter()
            .filter(|recipe| recipe.slug == slug)
            .max_by_key(|recipe| recipe.version)
            .map(|recipe| recipe.id))
    }

    /// Start a coding run for an existing feature task: the task becomes the
    /// run root, phase steps materialize as its subtasks, and completing it
    /// completes the run. Rejects a second active run on the same task; runs
    /// on different feature tasks of one project coexist.
    pub async fn create_task_run(
        &mut self,
        task_id: u64,
        recipe_id: u64,
        params: Value,
    ) -> QueryResult<WorkflowRun> {
        let task = self.get_task(task_id).await?;
        if task.deleted_at.is_some() {
            return Err(invalid(format!("task {task_id} is deleted")));
        }
        if let Some(existing) = self.find_run_by_root_task(task_id).await?
            && existing.status == "active"
        {
            return Err(invalid(format!(
                "task {task_id} already has an active coding run"
            )));
        }
        let (run, recipe) = self
            .insert_run_row(recipe_id, params, None, Some(task_id))
            .await?;
        crate::Task::update_by_id(task_id)
            .workflow_run_id(Some(run.id))
            .exec(&mut self.db)
            .await
            .context(crate::error::UpdateTaskSnafu { id: task_id })?;
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

    /// The newest run rooted at `task_id`, as the UI needs it (steps included).
    /// Completed runs are returned too, so the details panel keeps showing the
    /// workflow after it ends.
    pub async fn coding_run_for_task(&mut self, task_id: u64) -> QueryResult<Option<RunView>> {
        let Some(run) = self.find_run_by_root_task(task_id).await? else {
            return Ok(None);
        };
        self.workflow_run_view(run.id).await
    }

    /// Re-open a coding run's interview step as a real rewind (decision #26):
    /// the run turns back to the interview in place, keeping the branch, the
    /// commits and every earlier row, and the rewind is logged as a rejection
    /// so `Round N` advances (decision #29). Available in any phase; the
    /// caller stops a running agent turn first (decision #27).
    pub async fn reopen_coding_interview(
        &mut self,
        run_id: u64,
        reason: &str,
    ) -> QueryResult<()> {
        let run = self.get_run(run_id).await?;
        let recipe_row = self.get_recipe(run.recipe_id).await?;
        let recipe = parse_recipe(&recipe_row.recipe_json.0)
            .map_err(|message| invalid(format!("recipe {}: {message}", run.recipe_id)))?;
        let existing = self.open_node_task_id(run.id, "interview").await?;
        let interview = match existing {
            Some(step) => Some(step),
            None => self.node_latest_task_id(&run, "interview").await?,
        }
        .ok_or_else(|| invalid("this run has no interview step to re-open"))?;
        self.retry_task(interview, &recipe, &run).await?;
        let body = if reason.trim().is_empty() {
            "Reopened the interview".to_string()
        } else {
            reason.trim().to_string()
        };
        self.append_run_note(run_id, "reject", "interview", "interview", &body)
            .await?;
        Ok(())
    }

    /// The run's real subtasks: direct children of the root that are not
    /// steps (decision #5). Ordered by id, so the pane, the MCP context and
    /// the coverage line all read the same list in the same order.
    pub async fn run_subtasks(
        &mut self,
        root_task_id: u64,
    ) -> QueryResult<Vec<crate::TaskWithMeta>> {
        let rows = toasty::sql::query(
            r#"SELECT id FROM tasks WHERE parent_id = ?1 AND deleted_at IS NULL
               AND (role IS NULL OR role <> 'step') ORDER BY id"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(root_task_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "list run subtasks",
        })?;
        let mut subtasks = Vec::with_capacity(rows.len());
        for row in rows {
            if let Some(id) = row_id(&row) {
                subtasks.push(self.get_task_with_meta(id).await?);
            }
        }
        Ok(subtasks)
    }

    /// Whether `subtask_id` may be spelled out by a spec payload: a direct,
    /// non-step child of `root_task_id`. A hallucinated id is refused rather
    /// than written into another task (decision #31, §6.2).
    pub async fn is_direct_subtask(
        &mut self,
        root_task_id: u64,
        subtask_id: u64,
    ) -> QueryResult<bool> {
        let rows = toasty::sql::query(
            r#"SELECT id FROM tasks WHERE id = ?1 AND parent_id = ?2 AND deleted_at IS NULL
               AND (role IS NULL OR role <> 'step')"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(subtask_id as i64)
        .bind(root_task_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "check subtask belongs to run root",
        })?;
        Ok(rows.first().and_then(row_id).is_some())
    }

    /// Write an interview's whole output in one transaction: the umbrella spec
    /// on the run root, each subtask's own spec, each covered mark, the run's
    /// `spec` note, and the completed `interview` step. Every id is validated
    /// first, so a bad id writes nothing at all. A step id, or an id that is
    /// not a direct child of the root, is refused (decisions #6/#31).
    pub async fn save_subtask_specs(
        &mut self,
        root_task_id: u64,
        umbrella: Option<String>,
        umbrella_path: Option<String>,
        subtask_specs: Vec<SubtaskSpecInput>,
        covered_ids: Vec<u64>,
    ) -> QueryResult<()> {
        for id in subtask_specs
            .iter()
            .map(|input| input.task_id)
            .chain(covered_ids.iter().copied())
        {
            if !self.is_direct_subtask(root_task_id, id).await? {
                return Err(invalid(format!(
                    "task {id} is not an open subtask of task {root_task_id}"
                )));
            }
        }
        let now = jiff::Timestamp::now();
        self.with_transaction(move |store| {
            Box::pin(async move {
                if umbrella.is_some() || umbrella_path.is_some() {
                    store
                        .save_task_spec(root_task_id, umbrella, umbrella_path)
                        .await?;
                }
                for input in &subtask_specs {
                    store
                        .save_task_spec(input.task_id, Some(input.spec.clone()), None)
                        .await?;
                }
                for id in &covered_ids {
                    store.cover_subtask_at(*id, now).await?;
                }
                if let Some(view) = store.coding_run_for_task(root_task_id).await?
                    && view.run.status == "active"
                {
                    store
                        .append_run_note(view.run.id, "spec", "interview", "interview", "Spec saved")
                        .await?;
                    if let Some(step) = view
                        .steps
                        .iter()
                        .find(|step| step.node.id == "interview" && !step.task.done)
                        .map(|step| step.task.id)
                    {
                        store
                            .complete_workflow_step(step, serde_json::json!({}))
                            .await?;
                    }
                }
                Ok(())
            })
        })
        .await
    }

    /// The newest run whose root task is `task_id`.
    pub async fn find_run_by_root_task(
        &mut self,
        task_id: u64,
    ) -> QueryResult<Option<WorkflowRun>> {
        let rows = toasty::sql::query(
            r#"SELECT id, recipe_id, schedule_id, status, params, step_results, created_at, completed_at,
                      root_task_id, branch, base_branch, branch_status
               FROM workflow_runs WHERE root_task_id = ?1 ORDER BY id DESC LIMIT 1"#,
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
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
        ])
        .bind(task_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "find run by root task",
        })?;
        Ok(rows.first().and_then(parse_run_row))
    }

}

impl TodoStore {
    /// Runs that still hold a branch and are no longer active, for the
    /// "Branches to clean up" section of the workflow panel.
    pub async fn list_branch_cleanup_runs(&mut self) -> QueryResult<Vec<RunView>> {
        let runs = self.list_workflow_runs().await?;
        let mut views = Vec::new();
        for run in runs {
            if run.status == "active" || run.branch.is_none() {
                continue;
            }
            if run.branch_status.as_deref() == Some("deleted") {
                continue;
            }
            if let Some(view) = self.workflow_run_view(run.id).await? {
                views.push(view);
            }
        }
        Ok(views)
    }

    /// Append an entry to a run's ordered log (`step_results["@notes"]`).
    pub async fn append_run_note(
        &mut self,
        run_id: u64,
        kind: &str,
        phase: &str,
        node_id: &str,
        body: &str,
    ) -> QueryResult<()> {
        let run = self.get_run(run_id).await?;
        let note = RunNote::new(kind, phase, node_id, body);
        let mut results = run.step_results.0.clone();
        let Some(object) = results.as_object_mut() else {
            return Err(invalid(
                "workflow run step_results is corrupt (expected an object)",
            ));
        };
        match object
            .entry(RUN_NOTES_KEY.to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
        {
            Value::Array(entries) => entries.push(note.to_json()),
            slot => *slot = Value::Array(vec![note.to_json()]),
        }
        WorkflowRun::update_by_id(run_id)
            .step_results(toasty::Json(results))
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "append run note",
            })?;
        Ok(())
    }

    /// Store the agent's branch proposal (the branch does not exist yet).
    /// Returns the sanitized name that was kept.
    pub async fn propose_run_branch(&mut self, run_id: u64, branch: &str) -> QueryResult<String> {
        let branch = normalize_branch_name(branch);
        if branch.is_empty() {
            return Err(invalid("branch name is not usable"));
        }
        WorkflowRun::update_by_id(run_id)
            .branch(Some(branch.clone()))
            .branch_status(Some("proposed".to_string()))
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "propose run branch",
            })?;
        Ok(branch)
    }

    /// Record the branch the app created, and the branch it was cut from.
    pub async fn set_run_branch(
        &mut self,
        run_id: u64,
        branch: &str,
        base_branch: Option<&str>,
    ) -> QueryResult<()> {
        WorkflowRun::update_by_id(run_id)
            .branch(Some(branch.to_string()))
            .base_branch(base_branch.map(str::to_owned))
            .branch_status(Some("active".to_string()))
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "set run branch",
            })?;
        Ok(())
    }

    /// Detach the branch reference, e.g. after the branch was deleted.
    pub async fn clear_run_branch(&mut self, run_id: u64) -> QueryResult<()> {
        WorkflowRun::update_by_id(run_id)
            .branch(None)
            .base_branch(None)
            .branch_status(Some("deleted".to_string()))
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "clear run branch",
            })?;
        Ok(())
    }

    /// Mark a run's branch as merged.
    pub async fn mark_branch_merged(&mut self, run_id: u64) -> QueryResult<()> {
        WorkflowRun::update_by_id(run_id)
            .branch_status(Some("merged".to_string()))
            .exec(&mut self.db)
            .await
            .context(crate::error::QueryTagsSnafu {
                context: "mark branch merged",
            })?;
        Ok(())
    }

    /// Complete the merge step and the root feature task of a coding run (the
    /// app has already performed the git merge), record the merge in the run
    /// log, and let the run auto-complete.
    pub async fn complete_coding_merge(&mut self, run_id: u64) -> QueryResult<()> {
        self.finish_coding_run(run_id, None).await
    }

    /// Complete the merge step after every pull request was merged (or
    /// waived), recording the PRs instead of a local merge (decision 19).
    pub async fn complete_coding_pull_requests(
        &mut self,
        run_id: u64,
        note: &str,
    ) -> QueryResult<()> {
        self.finish_coding_run(run_id, Some(note.to_string())).await
    }

    async fn finish_coding_run(
        &mut self,
        run_id: u64,
        note: Option<String>,
    ) -> QueryResult<()> {
        let run = self.get_run(run_id).await?;
        let rows = toasty::sql::query(
            r#"SELECT id FROM tasks WHERE workflow_run_id = ?1 AND node_id = 'merge'
               AND done = 0 AND deleted_at IS NULL ORDER BY id"#,
        )
        .column_types([toasty::stmt::Type::I64])
        .bind(run_id as i64)
        .exec(&mut self.db)
        .await
        .context(crate::error::QueryTagsSnafu {
            context: "find merge step",
        })?;
        for row in rows {
            if let Some(id) = row_id(&row) {
                self.complete_workflow_step(id, serde_json::json!({ "merged": true }))
                    .await?;
            }
        }
        let merged = run.branch.clone();
        if merged.is_some() {
            self.mark_branch_merged(run_id).await?;
        }
        let body = note.unwrap_or_else(|| match &merged {
            Some(branch) => format!("Merged {branch}"),
            None => "Merged".to_string(),
        });
        self.append_run_note(run_id, "merge", "merge", "merge", &body)
            .await?;
        if let Some(root) = run.root_task_id {
            self.update_task_done(root, true).await?;
        }
        self.check_run_complete(run_id).await?;
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
    let root_task_id = record.get(8).and_then(|v| v.to_i64()).map(|id| id as u64);
    let branch = record.get(9).and_then(|v| v.as_str()).map(str::to_owned);
    let base_branch = record.get(10).and_then(|v| v.as_str()).map(str::to_owned);
    let branch_status = record.get(11).and_then(|v| v.as_str()).map(str::to_owned);
    Some(WorkflowRun {
        id,
        recipe_id,
        schedule_id,
        status,
        params,
        step_results,
        created_at,
        completed_at,
        root_task_id,
        branch,
        base_branch,
        branch_status,
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

    async fn coding_recipe_id(store: &mut TodoStore) -> u64 {
        store.ensure_coding_recipes().await.expect("recipes seed");
        store
            .recipe_id_by_slug("coding-task")
            .await
            .expect("recipe lookup")
            .expect("coding recipe exists")
    }

    /// Create a feature task and start a coding run on it.
    async fn start_coding_run(
        store: &mut TodoStore,
        title: &str,
    ) -> anyhow::Result<(u64, WorkflowRun)> {
        let recipe_id = coding_recipe_id(store).await;
        let feature = store
            .create_task(crate::Task::create().title(title.to_string()))
            .await?;
        let run = store
            .create_task_run(feature.id, recipe_id, serde_json::json!({}))
            .await?;
        Ok((feature.id, run))
    }

    /// The pending step of `node_id` in the run's current view.
    fn pending_step<'a>(view: &'a RunView, node_id: &str) -> &'a RunStepView {
        view.steps
            .iter()
            .filter(|step| step.node.id == node_id)
            .next_back()
            .unwrap_or_else(|| panic!("step `{node_id}` should exist"))
    }

    #[tokio::test]
    async fn test_coding_recipes_seed_idempotently() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        store.ensure_coding_recipes().await?;
        // A second call must not create another version.
        store.ensure_coding_recipes().await?;
        let recipes = store.list_recipes().await?;
        assert_eq!(
            recipes
                .iter()
                .filter(|recipe| recipe.slug == "coding-task")
                .count(),
            1
        );
        assert_eq!(
            recipes
                .iter()
                .filter(|recipe| recipe.slug == "coding-sub-interview")
                .count(),
            1
        );

        let id = store
            .recipe_id_by_slug("coding-task")
            .await?
            .expect("coding recipe");
        let row = store.get_recipe(id).await?;
        let recipe = parse_recipe(&row.recipe_json.0).expect("recipe validates");
        assert_eq!(recipe.nodes.len(), 5);
        let review = recipe
            .nodes
            .iter()
            .find(|node| node.id == "review")
            .expect("review node");
        assert_eq!(review.phase.as_deref(), Some("review"));
        // v2 declares steps explicitly: run scaffolding is never a subtask
        // (decisions #3/#4).
        assert_eq!(review.role.as_deref(), Some("step"));
        assert!(!review.subtask);
        assert_eq!(review.retrigger_node.as_deref(), Some("interview"));
        Ok(())
    }

    /// Changing the shipped declaration appends exactly one new version, and
    /// the runs that pinned the old one keep its semantics (decision #3).
    #[tokio::test]
    async fn test_ensure_coding_recipes_upserts_a_changed_builtin() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        // Install the pre-change v1 declaration by hand, then let the startup
        // path notice the shipped v2 differs.
        let v1 = serde_json::json!({
            "name": "Coding task",
            "nodes": [
                {
                    "id": "interview",
                    "kind": "action",
                    "title": "Interview & spec the feature",
                    "ai": true,
                    "phase": "interview",
                    "subtask": true
                }
            ],
            "edges": []
        });
        let installed = store.create_recipe("coding-task", v1).await?;
        store.ensure_coding_recipes().await?;
        store.ensure_coding_recipes().await?;
        let versions: Vec<u64> = store
            .list_recipes()
            .await?
            .iter()
            .filter(|recipe| recipe.slug == "coding-task")
            .map(|recipe| recipe.version)
            .collect();
        assert_eq!(versions.len(), 2, "one upsert, then idempotent: {versions:?}");
        let current = store
            .recipe_id_by_slug("coding-task")
            .await?
            .expect("newest version");
        assert_ne!(current, installed.id, "the newest version is the shipped one");
        // The pinned v1 row still declares its step as a subtask.
        let v1_recipe = parse_recipe(&store.get_recipe(installed.id).await?.recipe_json.0)
            .expect("v1 validates");
        assert!(v1_recipe.nodes[0].subtask);
        Ok(())
    }

    #[tokio::test]
    async fn test_recipe_role_validation() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        // `role` and `subtask` together declare the same thing twice.
        assert!(
            store
                .create_recipe(
                    "role-and-subtask",
                    serde_json::json!({
                        "name": "Bad",
                        "nodes": [{ "id": "a", "kind": "action", "title": "A", "phase": "interview", "role": "step", "subtask": true }],
                        "edges": []
                    }),
                )
                .await
                .is_err()
        );
        // An unknown role.
        assert!(
            store
                .create_recipe(
                    "bad-role",
                    serde_json::json!({
                        "name": "Bad",
                        "nodes": [{ "id": "a", "kind": "action", "title": "A", "phase": "interview", "role": "middle" }],
                        "edges": []
                    }),
                )
                .await
                .is_err()
        );
        // A step always belongs to a phase.
        assert!(
            store
                .create_recipe(
                    "step-without-phase",
                    serde_json::json!({
                        "name": "Bad",
                        "nodes": [{ "id": "a", "kind": "action", "title": "A", "role": "step" }],
                        "edges": []
                    }),
                )
                .await
                .is_err()
        );
        Ok(())
    }

    /// A small helper for the coverage tests.
    fn coverage_task(id: u64, done: bool) -> crate::TaskWithMeta {
        crate::TaskWithMeta {
            task: crate::Task {
                id,
                title: format!("task {id}"),
                description: None,
                branch_name: None,
                labels: None,
                deadline: None,
                blocked_until: None,
                importance_factor: 1.0,
                urgency_factor: 1.0,
                done,
                completed_at: None,
                created_at: jiff::Timestamp::now(),
                updated_at: jiff::Timestamp::now(),
                parent_id: None,
                source_task_id: None,
                deleted_at: None,
                timezone: None,
                comments: None,
                workflow_run_id: None,
                node_id: None,
                spec: None,
                spec_path: None,
                role: None,
                spec_covered_at: None,
                subtasks: toasty::Deferred::default(),
                parent: toasty::Deferred::default(),
            },
            direct_tags: Vec::new(),
            inherited_tags: Vec::new(),
            inferred_tags: Vec::new(),
            leaf_tags: Vec::new(),
            blocked: false,
            managed_by: None,
            managed_label: None,
            managed_mode: None,
            managed_editable: false,
            user_modified: false,
        }
    }

    /// Coverage: an own spec beats the mark, the mark is the umbrella's
    /// answer, done tasks never count, and a description never counts
    /// (decisions #6/#10/#11).
    #[test]
    fn test_subtask_coverage_states() {
        let mut own = coverage_task(1, false);
        own.task.spec = Some("its own spec".to_string());
        let mut marked = coverage_task(2, false);
        marked.task.spec_covered_at = Some(jiff::Timestamp::now());
        let mut bare = coverage_task(3, false);
        bare.task.description = Some("a description is not a spec".to_string());
        let done = coverage_task(4, true);
        let mut done_but_specced = coverage_task(5, true);
        done_but_specced.task.spec = Some("spec".to_string());

        assert_eq!(subtask_coverage(&own), SubtaskCoverage::Own);
        assert_eq!(subtask_coverage(&marked), SubtaskCoverage::Covered);
        assert_eq!(subtask_coverage(&bare), SubtaskCoverage::Unspecced);
        assert_eq!(subtask_coverage(&done), SubtaskCoverage::NotApplicable);
        assert_eq!(
            subtask_coverage(&done_but_specced),
            SubtaskCoverage::NotApplicable
        );
        assert_eq!(subtask_coverage(&own).label(), "own");
        assert_eq!(subtask_coverage(&bare).label(), "none");

        // The count excludes done subtasks and never counts a description.
        let summary = coverage_summary(&[own, marked, bare, done, done_but_specced]);
        assert_eq!(summary.total, 3);
        assert_eq!(summary.covered, 2);
    }

    #[tokio::test]
    async fn test_coding_run_walks_every_phase() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let (feature_id, run) = start_coding_run(&mut store, "Add OAuth").await?;
        assert_eq!(run.root_task_id, Some(feature_id));
        assert_eq!(
            store.get_task(feature_id).await?.workflow_run_id,
            Some(run.id)
        );

        // The start node materializes as a subtask of the feature task.
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        assert_eq!(view.steps.len(), 1);
        let interview = pending_step(&view, "interview");
        assert_eq!(interview.task.parent_id, Some(feature_id));

        // The agent's spec signal completes the interview step; the spec gate
        // spawns under the feature task.
        store
            .complete_workflow_step(interview.task.id, serde_json::json!({}))
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let spec = pending_step(&view, "spec");
        assert!(spec.node.approval);
        // Approval gates nest under the step they review (the engine's existing
        // rule), which is itself a subtask of the feature task.
        assert_eq!(spec.task.parent_id, Some(interview.task.id));

        // Approving the spec spawns the implementation.
        store
            .complete_workflow_step(spec.task.id, serde_json::json!({ "approved": true }))
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let implement = pending_step(&view, "implement");

        // Confirming the implementation spawns the review as its subtask.
        store
            .complete_workflow_step(implement.task.id, serde_json::json!({}))
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let review = pending_step(&view, "review");
        assert_eq!(review.task.parent_id, Some(implement.task.id));

        // Approval spawns the merge step; the app merges and the run finishes.
        store
            .complete_workflow_step(review.task.id, serde_json::json!({ "approved": true }))
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        assert!(view.steps.iter().any(|step| step.node.id == "merge"));
        store.complete_coding_merge(run.id).await?;

        assert_eq!(store.find_run(run.id).await?.expect("run").status, "completed");
        assert!(store.get_task(feature_id).await?.done);
        let notes = run_notes(&view.run.step_results.0);
        assert!(notes.iter().any(|note| note.kind == "approve"));
        Ok(())
    }

    #[tokio::test]
    async fn test_coding_rejection_respecs_and_keeps_the_log() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let (_feature_id, run) = start_coding_run(&mut store, "Add OAuth").await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let interview = pending_step(&view, "interview").task.id;
        store
            .complete_workflow_step(interview, serde_json::json!({}))
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let spec = pending_step(&view, "spec").task.id;
        store
            .complete_workflow_step(spec, serde_json::json!({ "approved": true }))
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let implement = pending_step(&view, "implement").task.id;
        store
            .complete_workflow_step(implement, serde_json::json!({}))
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let review = pending_step(&view, "review").task.id;

        // Rejecting the review re-opens the interview (re-spec) rather than
        // re-running the implementation.
        store
            .complete_workflow_step(
                review,
                serde_json::json!({ "approved": false, "notes": "Needs a migration test." }),
            )
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let reopened = view
            .steps
            .iter()
            .find(|step| step.node.id == "interview" && !step.task.done)
            .expect("the interview step re-opens");
        let notes = run_notes(&view.run.step_results.0);
        assert!(
            notes
                .iter()
                .any(|note| note.kind == "reject" && note.body == "Needs a migration test."),
            "the rejection is logged: {notes:?}"
        );

        // Saving the spec again spawns a second spec gate: cycle two.
        store
            .complete_workflow_step(reopened.task.id, serde_json::json!({}))
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        assert_eq!(
            view.steps
                .iter()
                .filter(|step| step.node.id == "spec")
                .count(),
            2
        );
        // The rejection cleared the interview result, so the re-opened step's
        // result is the fresh one.
        assert!(run_notes(&view.run.step_results.0).len() >= 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_coding_runs_coexist_but_not_on_one_task() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let recipe_id = coding_recipe_id(&mut store).await;
        let tag = store.create_tag("managed:oauth").await?;
        let first = store
            .create_task(crate::Task::create().title("First"))
            .await?;
        let second = store
            .create_task(crate::Task::create().title("Second"))
            .await?;
        store.assign_tag_to_task(first.id, &tag.name).await?;
        store.assign_tag_to_task(second.id, &tag.name).await?;

        store
            .create_task_run(first.id, recipe_id, serde_json::json!({}))
            .await?;
        // Two features of one project drive their own runs: they share the
        // project's single agent session at the pane level, which is not the
        // store's business.
        store
            .create_task_run(second.id, recipe_id, serde_json::json!({}))
            .await?;
        // One task, though, owns one active run.
        let blocked = store
            .create_task_run(first.id, recipe_id, serde_json::json!({}))
            .await;
        assert!(blocked.is_err(), "a second run on one task is blocked");
        Ok(())
    }

    #[tokio::test]
    async fn test_cancelled_run_keeps_its_branch_visible() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let (_feature_id, run) = start_coding_run(&mut store, "Add OAuth").await?;
        store
            .set_run_branch(run.id, "feature/1-add-oauth", Some("main"))
            .await?;
        store.cancel_run(run.id).await?;

        let cancelled = store.find_run(run.id).await?.expect("run");
        assert_eq!(cancelled.status, "cancelled");
        assert_eq!(cancelled.branch_status.as_deref(), Some("abandoned"));
        assert_eq!(cancelled.base_branch.as_deref(), Some("main"));
        // The branch stays listed for cleanup...
        assert!(store.workflow_run_view(run.id).await?.is_some());
        assert_eq!(store.list_branch_cleanup_runs().await?.len(), 1);
        // ...until the branch reference is dropped.
        store.clear_run_branch(run.id).await?;
        assert!(store.workflow_run_view(run.id).await?.is_none());
        assert!(store.list_branch_cleanup_runs().await?.is_empty());

        // A cancelled run that never had a branch stays hidden. (No tags, so
        // the per-project guard does not block this second run.)
        let (_other_feature, other) = start_coding_run(&mut store, "Add SSO").await?;
        store.cancel_run(other.id).await?;
        assert!(store.workflow_run_view(other.id).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_branch_proposal_is_normalized() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let (_feature_id, run) = start_coding_run(&mut store, "Add OAuth").await?;
        let kept = store.propose_run_branch(run.id, "Feature/Add OAuth!!").await?;
        assert_eq!(kept, "Feature/Add-OAuth");
        let stored = store.find_run(run.id).await?.expect("run");
        assert_eq!(stored.branch.as_deref(), Some("Feature/Add-OAuth"));
        assert_eq!(stored.branch_status.as_deref(), Some("proposed"));
        assert!(
            store.propose_run_branch(run.id, "//").await.is_err(),
            "an unusable name is rejected"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_coding_node_field_validation() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        // Unknown phase.
        assert!(
            store
                .create_recipe(
                    "bad-phase",
                    serde_json::json!({
                        "name": "Bad",
                        "nodes": [{ "id": "a", "kind": "action", "title": "A", "phase": "deploy" }],
                        "edges": []
                    }),
                )
                .await
                .is_err()
        );
        // `subtask` only makes sense on action nodes.
        assert!(
            store
                .create_recipe(
                    "bad-subtask",
                    serde_json::json!({
                        "name": "Bad",
                        "nodes": [{ "id": "a", "kind": "event", "title": "A", "subtask": true }],
                        "edges": []
                    }),
                )
                .await
                .is_err()
        );
        // `retrigger_node` needs `retrigger_on_reject`.
        assert!(
            store
                .create_recipe(
                    "bad-retrigger",
                    serde_json::json!({
                        "name": "Bad",
                        "nodes": [
                            { "id": "draft", "kind": "action", "title": "D", "ai": true },
                            { "id": "gate", "kind": "action", "title": "G", "approval": true, "retrigger_node": "draft" }
                        ],
                        "edges": [
                            { "from": "draft", "to": "gate", "condition_type": "on_result", "condition_value": {} }
                        ]
                    }),
                )
                .await
                .is_err()
        );
        // `retrigger_node` must name an automated node.
        assert!(
            store
                .create_recipe(
                    "bad-retrigger-target",
                    serde_json::json!({
                        "name": "Bad",
                        "nodes": [
                            { "id": "draft", "kind": "action", "title": "D", "ai": true },
                            { "id": "plain", "kind": "action", "title": "P" },
                            { "id": "gate", "kind": "action", "title": "G", "approval": true, "retrigger_on_reject": true, "retrigger_node": "plain" }
                        ],
                        "edges": [
                            { "from": "draft", "to": "gate", "condition_type": "on_result", "condition_value": {} },
                            { "from": "plain", "to": "gate", "condition_type": "on_complete" }
                        ]
                    }),
                )
                .await
                .is_err()
        );
        // `retrigger_node` must exist.
        assert!(
            store
                .create_recipe(
                    "bad-retrigger-unknown",
                    serde_json::json!({
                        "name": "Bad",
                        "nodes": [
                            { "id": "draft", "kind": "action", "title": "D", "ai": true },
                            { "id": "gate", "kind": "action", "title": "G", "approval": true, "retrigger_on_reject": true, "retrigger_node": "nope" }
                        ],
                        "edges": [
                            { "from": "draft", "to": "gate", "condition_type": "on_result", "condition_value": {} }
                        ]
                    }),
                )
                .await
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn test_issue_branch_names_drop_filler_words_and_stay_short() {
        assert_eq!(
            issue_branch_name(123, "Add a login screen to the settings page"),
            "feature/123-login-screen-settings-page"
        );
        // Filler words are dropped but a title made only of them still yields
        // a usable slug.
        assert_eq!(issue_branch_name(7, "Fix the bug"), "feature/7-bug");
        assert_eq!(issue_branch_name(9, "Fix it"), "feature/9-fix-it");
        // No usable characters at all: the number alone is still the branch.
        assert_eq!(issue_branch_name(11, "!! ???"), "feature/11");
        // Non-ASCII and emoji are dropped rather than passed through, and the
        // result is lower case and git-safe.
        let emoji = issue_branch_name(12, "Ajouter la connexion 🚀 au panneau");
        assert!(emoji.starts_with("feature/12-"), "{emoji}");
        assert!(
            emoji
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.')),
            "{emoji}"
        );
        assert_eq!(emoji, emoji.to_lowercase());
        // Long titles are capped at five words and 60 characters.
        let long = issue_branch_slug(
            "Refactor the synchronisation engine so that it can handle every remote transport",
        );
        assert!(long.split('-').count() <= 5, "{long}");
        assert!(long.chars().count() <= 60, "{long}");
        assert!(!long.ends_with('-'), "{long}");
    }

    /// Steps are not subtasks: they carry the explicit role, and none of the
    /// subtask surfaces return them (decisions #4/#5).
    #[tokio::test]
    async fn test_steps_are_not_subtasks() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let (feature_id, run) = start_coding_run(&mut store, "Add OAuth").await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let interview = pending_step(&view, "interview");
        assert_eq!(interview.task.role.as_deref(), Some("step"));
        assert!(store.run_subtasks(feature_id).await?.is_empty());
        assert!(!store.list_subtasks(feature_id).await?.iter().any(|task| task.id == interview.task.id));
        assert!(
            store
                .subtasks_map(&[feature_id])
                .await?
                .get(&feature_id)
                .is_none()
        );

        // Approval gates nest under the step they review, and are steps too.
        store
            .complete_workflow_step(interview.task.id, serde_json::json!({}))
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let spec = pending_step(&view, "spec");
        assert_eq!(spec.task.parent_id, Some(interview.task.id));
        assert_eq!(spec.task.role.as_deref(), Some("step"));
        assert!(store.list_subtasks(interview.task.id).await?.is_empty());

        // A real subtask of the feature task does show up.
        let subtask = store
            .create_task(
                crate::Task::create()
                    .title("Add token refresh".to_string())
                    .parent_id(Some(feature_id)),
            )
            .await?;
        let seen = store.run_subtasks(feature_id).await?;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].id, subtask.id);
        assert!(store.is_direct_subtask(feature_id, subtask.id).await?);
        assert!(
            !store
                .is_direct_subtask(feature_id, spec.task.id)
                .await?,
            "a step is never a settable subtask"
        );
        Ok(())
    }

    /// One `save_subtask_specs` writes the umbrella, the subtask specs and
    /// the covered marks atomically, completes the interview step, and
    /// refuses any id that is not a direct, non-step child (decisions
    /// #6/#10/#23/#31).
    #[tokio::test]
    async fn test_save_subtask_specs_writes_and_guards() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let (feature_id, run) = start_coding_run(&mut store, "Add OAuth").await?;
        let owned = store
            .create_task(
                crate::Task::create()
                    .title("Add token refresh".to_string())
                    .parent_id(Some(feature_id)),
            )
            .await?;
        let covered = store
            .create_task(
                crate::Task::create()
                    .title("Wire the callback URL".to_string())
                    .parent_id(Some(feature_id)),
            )
            .await?;
        let grandchild = store
            .create_task(
                crate::Task::create()
                    .title("Deeper".to_string())
                    .parent_id(Some(owned.id)),
            )
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let interview_step = pending_step(&view, "interview").task.id;

        // A grandchild id is refused before anything is written.
        let refused = store
            .save_subtask_specs(
                feature_id,
                Some("# Umbrella".to_string()),
                None,
                vec![SubtaskSpecInput {
                    task_id: grandchild.id,
                    spec: "nope".to_string(),
                }],
                Vec::new(),
            )
            .await;
        assert!(refused.is_err());
        assert!(store.get_task(feature_id).await?.spec.is_none());
        assert!(store.get_task(grandchild.id).await?.spec.is_none());

        store
            .save_subtask_specs(
                feature_id,
                Some("# Umbrella".to_string()),
                Some("docs/spec/add-oauth-spec.md".to_string()),
                vec![SubtaskSpecInput {
                    task_id: owned.id,
                    spec: "Refresh hourly".to_string(),
                }],
                vec![covered.id],
            )
            .await?;
        let feature = store.get_task(feature_id).await?;
        assert_eq!(feature.spec.as_deref(), Some("# Umbrella"));
        assert_eq!(
            store.get_task(owned.id).await?.spec.as_deref(),
            Some("Refresh hourly")
        );
        assert!(store.get_task(covered.id).await?.spec_covered_at.is_some());
        // The interview step completed, so the run advanced to the spec gate.
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        assert!(
            view.steps
                .iter()
                .any(|step| step.task.id == interview_step && step.task.done)
        );
        assert!(view.steps.iter().any(|step| step.node.id == "spec"));
        assert!(
            run_notes(&view.run.step_results.0)
                .iter()
                .any(|note| note.kind == "spec")
        );
        // A step id is refused too.
        assert!(
            store
                .save_subtask_specs(
                    feature_id,
                    None,
                    None,
                    Vec::new(),
                    vec![interview_step],
                )
                .await
                .is_err()
        );
        Ok(())
    }

    /// Re-opening the interview re-uses rows instead of duplicating them: the
    /// interview row re-opens in place, a pending implement survives, and each
    /// round's spec gate is a fresh row (decision #30).
    #[tokio::test]
    async fn test_rewind_reuses_pending_rows() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let (feature_id, run) = start_coding_run(&mut store, "Add OAuth").await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let interview = pending_step(&view, "interview").task.id;
        store
            .complete_workflow_step(interview, serde_json::json!({}))
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let spec = pending_step(&view, "spec").task.id;
        store
            .complete_workflow_step(spec, serde_json::json!({ "approved": true }))
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let implement = pending_step(&view, "implement").task.id;

        // Rewind from the implement phase: the interview row is the same row,
        // the pending implement row is untouched, and the rewind is logged as
        // a rejection.
        store.reopen_coding_interview(run.id, "Scope changed").await?;
        // The rewind turns the run back, it does not start another one.
        assert_eq!(
            store.get_task(feature_id).await?.workflow_run_id,
            Some(run.id)
        );
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let reopened = pending_step(&view, "interview");
        assert_eq!(reopened.task.id, interview);
        assert_eq!(pending_step(&view, "implement").task.id, implement);
        assert!(
            run_notes(&view.run.step_results.0)
                .iter()
                .any(|note| note.kind == "reject" && note.body == "Scope changed")
        );
        assert_eq!(
            view.steps.iter().filter(|step| step.node.id == "spec").count(),
            1
        );

        // The new round approves the spec and picks the same implement row
        // back up rather than spawning a second one.
        store
            .complete_workflow_step(reopened.task.id, serde_json::json!({}))
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let spec = pending_step(&view, "spec").task.id;
        assert_eq!(
            view.steps.iter().filter(|step| step.node.id == "spec").count(),
            2,
            "each round's spec gate is a fresh row"
        );
        store
            .complete_workflow_step(spec, serde_json::json!({ "approved": true }))
            .await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        assert_eq!(
            view.steps
                .iter()
                .filter(|step| step.node.id == "implement")
                .count(),
            1
        );
        assert_eq!(pending_step(&view, "implement").task.id, implement);
        Ok(())
    }

    /// A hand-written subtask spec is just `tasks.spec`, so coverage accepts
    /// it; a covered subtask later given an own spec reports `own` (the mark
    /// stays as history).
    #[tokio::test]
    async fn test_own_spec_wins_over_the_coverage_mark() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let (feature_id, _run) = start_coding_run(&mut store, "Add OAuth").await?;
        let subtask = store
            .create_task(
                crate::Task::create()
                    .title("Wire the callback URL".to_string())
                    .parent_id(Some(feature_id)),
            )
            .await?;
        store.cover_subtask_at(subtask.id, jiff::Timestamp::now()).await?;
        let marked = store.get_task_with_meta(subtask.id).await?;
        assert_eq!(subtask_coverage(&marked), SubtaskCoverage::Covered);

        store
            .save_task_spec(subtask.id, Some("Write it by hand".to_string()), None)
            .await?;
        let own = store.get_task_with_meta(subtask.id).await?;
        assert_eq!(subtask_coverage(&own), SubtaskCoverage::Own);
        assert!(own.spec_covered_at.is_some(), "the mark stays as history");

        // Steps refuse a spec outright (decision #31).
        let (_feature, run) = start_coding_run(&mut store, "Another feature").await?;
        let view = store.workflow_run_view(run.id).await?.expect("run view");
        let step = pending_step(&view, "interview").task.id;
        assert!(
            store
                .save_task_spec(step, Some("nope".to_string()), None)
                .await
                .is_err()
        );
        Ok(())
    }

    /// Promotion gives a subtask its own run and counts it as covered in the
    /// parent: nested runs are exempt from the one-run-per-project guard
    /// (decisions #16/#32).
    #[tokio::test]
    async fn test_promoted_subtask_gets_a_nested_run() -> anyhow::Result<()> {
        let mut store = TodoStore::for_test().await?;
        let (feature_id, _run) = start_coding_run(&mut store, "Add OAuth").await?;
        let subtask = store
            .create_task(
                crate::Task::create()
                    .title("Session storage".to_string())
                    .parent_id(Some(feature_id)),
            )
            .await?;
        store
            .cover_subtask_at(subtask.id, jiff::Timestamp::now())
            .await?;
        let recipes = store.recipe_id_by_slug("coding-task").await?.expect("recipe");
        let nested = store
            .create_task_run(subtask.id, recipes, serde_json::json!({}))
            .await?;
        assert_eq!(nested.root_task_id, Some(subtask.id));

        // It is still a subtask of the parent, and counted covered there.
        let subtasks = store.run_subtasks(feature_id).await?;
        assert_eq!(subtasks.len(), 1);
        assert_eq!(subtasks[0].id, subtask.id);
        assert!(subtasks[0].workflow_run_id.is_some());
        let summary = coverage_summary(&subtasks);
        assert_eq!((summary.covered, summary.total), (1, 1));
        Ok(())
    }
}