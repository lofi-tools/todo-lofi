//! Strict, Liquid-compatible prompt template rendering.
//!
//! Sections 5.4 and 12 of the specification require a strict template engine:
//! unknown variables and unknown filters MUST fail rendering, and nested arrays
//! (labels, blockers) MUST stay iterable. The engine implements the subset of
//! Liquid that a `WORKFLOW.md` prompt needs (`{{ output }}`, `{% if %}`,
//! `{% for %}` and the common filters) and fails closed on everything else.

use crate::domain::Issue;
use crate::error::SymphonyError::{TemplateParseError, TemplateRenderError};
use crate::error::{Result, SymphonyError};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Build the template context for one run attempt.
///
/// `attempt` is `None` on the first run and an integer for retries/continuations,
/// matching Section 5.4 of the specification.
pub fn build_context(issue: &Issue, attempt: Option<u32>) -> Value {
    let mut issue_object = Map::new();
    issue_object.insert("id".into(), Value::String(issue.id.clone()));
    issue_object.insert("identifier".into(), Value::String(issue.identifier.clone()));
    issue_object.insert("title".into(), Value::String(issue.title.clone()));
    issue_object.insert(
        "description".into(),
        match &issue.description {
            Some(description) => Value::String(description.clone()),
            None => Value::Null,
        },
    );
    issue_object.insert(
        "priority".into(),
        match issue.priority {
            Some(priority) => Value::Number(priority.into()),
            None => Value::Null,
        },
    );
    issue_object.insert("state".into(), Value::String(issue.state.clone()));
    issue_object.insert(
        "branch_name".into(),
        option_string(issue.branch_name.as_deref()),
    );
    issue_object.insert("url".into(), option_string(issue.url.as_deref()));
    issue_object.insert(
        "labels".into(),
        Value::Array(issue.labels.iter().cloned().map(Value::String).collect()),
    );
    issue_object.insert(
        "blocked_by".into(),
        Value::Array(
            issue
                .blocked_by
                .iter()
                .map(|blocker| {
                    let mut object = Map::new();
                    object.insert("id".into(), option_string(blocker.id.as_deref()));
                    object.insert(
                        "identifier".into(),
                        option_string(blocker.identifier.as_deref()),
                    );
                    object.insert("state".into(), option_string(blocker.state.as_deref()));
                    Value::Object(object)
                })
                .collect(),
        ),
    );
    issue_object.insert(
        "created_at".into(),
        crate::clock::format_timestamp(issue.created_at),
    );
    issue_object.insert(
        "updated_at".into(),
        crate::clock::format_timestamp(issue.updated_at),
    );

    let mut root = Map::new();
    root.insert("issue".into(), Value::Object(issue_object));
    root.insert(
        "attempt".into(),
        match attempt {
            Some(attempt) => Value::Number(attempt.into()),
            None => Value::Null,
        },
    );
    Value::Object(root)
}

fn option_string(value: Option<&str>) -> Value {
    match value {
        Some(value) => Value::String(value.to_string()),
        None => Value::Null,
    }
}

/// A parsed prompt template.
#[derive(Debug, Clone)]
pub struct Template {
    nodes: Vec<Node>,
}

#[derive(Debug, Clone)]
enum Node {
    Text(String),
    Output(Filtered),
    If {
        branches: Vec<(Condition, Vec<Node>)>,
        fallback: Option<Vec<Node>>,
    },
    For {
        binding: String,
        iterable: Filtered,
        body: Vec<Node>,
    },
}

#[derive(Debug, Clone)]
struct Filtered {
    base: Primary,
    filters: Vec<FilterCall>,
}

#[derive(Debug, Clone)]
struct FilterCall {
    name: String,
    args: Vec<Primary>,
}

#[derive(Debug, Clone)]
enum Primary {
    Path(String),
    Literal(Value),
}

#[derive(Debug, Clone)]
enum Condition {
    Value(Filtered),
    Comparison {
        left: Filtered,
        operator: ComparisonOperator,
        right: Filtered,
    },
    And(Box<Condition>, Box<Condition>),
    Or(Box<Condition>, Box<Condition>),
    Not(Box<Condition>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComparisonOperator {
    Equal,
    NotEqual,
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,
    Contains,
}

#[derive(Debug, Clone)]
enum Tag {
    Output(String),
    Statement(String),
}

/// Parse template source into a renderable template.
pub fn parse(source: &str) -> Result<Template> {
    let tags = tokenize(source)?;
    let mut cursor = 0;
    let nodes = parse_nodes(&tags, &mut cursor, None)?;
    if cursor != tags.len() {
        return Err(unexpected_tag(&tags[cursor]));
    }
    Ok(Template { nodes })
}

/// Render a parsed template against a context object.
pub fn render(template: &Template, context: &Value) -> Result<String> {
    let mut output = String::new();
    let mut scope = Scope::new(context);
    render_nodes(&template.nodes, &mut scope, &mut output)?;
    Ok(output)
}

/// Convenience helper: parse and render in one step.
pub fn render_source(source: &str, context: &Value) -> Result<String> {
    let template = parse(source)?;
    render(&template, context)
}

fn tokenize(source: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut rest = source;
    let mut offset = 0usize;

    loop {
        let Some(start) = rest.find(['{']) else {
            push_text(&mut tokens, rest);
            break;
        };
        // A brace that does not open a tag is plain text.
        let after = &rest[start..];
        let is_output = after.starts_with("{{");
        let is_statement = after.starts_with("{%");
        if !is_output && !is_statement {
            push_text(&mut tokens, &rest[..start + 1]);
            rest = &rest[start + 1..];
            offset += start + 1;
            continue;
        }

        push_text(&mut tokens, &rest[..start]);
        let closing = if is_output { "}}" } else { "%}" };
        let Some(end) = after.find(closing) else {
            return Err(template_parse_error(format!(
                "unterminated template tag starting at byte {offset}"
            )));
        };
        let inner = after[2..end].trim().to_string();
        tokens.push(if is_output {
            Token::Tag(Tag::Output(inner))
        } else {
            Token::Tag(Tag::Statement(inner))
        });
        let consumed = end + closing.len();
        offset += start + consumed;
        rest = &rest[start + consumed..];
    }

    Ok(tokens)
}

fn push_text(tokens: &mut Vec<Token>, text: &str) {
    if text.is_empty() {
        return;
    }
    match tokens.last_mut() {
        Some(Token::Text(existing)) => existing.push_str(text),
        _ => tokens.push(Token::Text(text.to_string())),
    }
}

fn parse_nodes(tokens: &[Token], cursor: &mut usize, expected: Option<&str>) -> Result<Vec<Node>> {
    let mut nodes = Vec::new();

    while *cursor < tokens.len() {
        match &tokens[*cursor] {
            Token::Text(text) => {
                nodes.push(Node::Text(text.clone()));
                *cursor += 1;
            }
            Token::Tag(Tag::Output(expression)) => {
                nodes.push(Node::Output(parse_filtered(expression)?));
                *cursor += 1;
            }
            Token::Tag(Tag::Statement(statement)) => {
                let (keyword, remainder) = split_keyword(statement);
                match keyword {
                    "if" => nodes.push(parse_if(remainder, tokens, cursor)?),
                    "for" => nodes.push(parse_for(remainder, tokens, cursor)?),
                    // Block terminators end the current body; the caller decides
                    // whether that keyword was the one it was waiting for.
                    "elsif" | "else" | "endif" | "endfor" => match expected {
                        Some(_) => return Ok(nodes),
                        None => return Err(unexpected_tag(&tokens[*cursor])),
                    },
                    other => {
                        return Err(template_parse_error(format!("unknown tag `{other}`")));
                    }
                }
            }
        }
    }

    match expected {
        Some(expected) => Err(template_parse_error(format!(
            "missing `{expected}` for open block"
        ))),
        None => Ok(nodes),
    }
}

fn parse_if(remainder: &str, tokens: &[Token], cursor: &mut usize) -> Result<Node> {
    let condition = parse_condition(remainder)?;
    *cursor += 1;

    let body = parse_nodes(tokens, cursor, Some("elsif"))?;
    let mut branches = vec![(condition, body)];
    let mut fallback = None;

    loop {
        let Some(Token::Tag(Tag::Statement(statement))) = tokens.get(*cursor) else {
            return Err(template_parse_error("missing `endif` for open `if`"));
        };
        let (keyword, remainder) = split_keyword(statement);
        match keyword {
            "elsif" => {
                let condition = parse_condition(remainder)?;
                *cursor += 1;
                let body = parse_nodes(tokens, cursor, Some("elsif"))?;
                branches.push((condition, body));
            }
            "else" => {
                *cursor += 1;
                fallback = Some(parse_nodes(tokens, cursor, Some("endif"))?);
                expect_keyword(tokens, cursor, "endif")?;
                break;
            }
            "endif" => {
                *cursor += 1;
                break;
            }
            other => {
                return Err(template_parse_error(format!(
                    "unexpected tag `{other}` inside `if`"
                )));
            }
        }
    }

    Ok(Node::If { branches, fallback })
}

fn parse_for(remainder: &str, tokens: &[Token], cursor: &mut usize) -> Result<Node> {
    let Some((binding, iterable)) = remainder.split_once(" in ") else {
        return Err(template_parse_error(format!(
            "invalid `for` expression `{remainder}`"
        )));
    };
    let binding = binding.trim();
    if binding.is_empty() || !binding.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err(template_parse_error(format!(
            "invalid `for` binding `{binding}`"
        )));
    }
    let iterable = parse_filtered(iterable.trim())?;

    *cursor += 1;
    let body = parse_nodes(tokens, cursor, Some("endfor"))?;
    expect_keyword(tokens, cursor, "endfor")?;

    Ok(Node::For {
        binding: binding.to_string(),
        iterable,
        body,
    })
}

fn expect_keyword(tokens: &[Token], cursor: &mut usize, expected: &str) -> Result<()> {
    match tokens.get(*cursor) {
        Some(Token::Tag(Tag::Statement(statement)))
            if split_keyword(statement).0 == expected =>
        {
            *cursor += 1;
            Ok(())
        }
        Some(other) => Err(unexpected_tag(other)),
        None => Err(template_parse_error(format!("missing `{expected}`"))),
    }
}

fn unexpected_tag(token: &Token) -> SymphonyError {
    match token {
        Token::Tag(Tag::Output(inner)) => {
            template_parse_error(format!("unexpected output tag `{{{{ {inner} }}}}`"))
        }
        Token::Tag(Tag::Statement(inner)) => {
            template_parse_error(format!("unexpected tag `{{% {inner} %}}`"))
        }
        Token::Text(text) => template_parse_error(format!("unexpected text `{text}`")),
    }
}

#[derive(Debug, Clone)]
enum Token {
    Text(String),
    Tag(Tag),
}

fn split_keyword(statement: &str) -> (&str, &str) {
    let statement = statement.trim();
    match statement.split_once(char::is_whitespace) {
        Some((keyword, remainder)) => (keyword, remainder.trim()),
        None => (statement, ""),
    }
}

fn template_parse_error(message: impl Into<String>) -> SymphonyError {
    TemplateParseError {
        source: Box::new(std::io::Error::other(message.into())),
    }
}

fn template_render_error(message: impl Into<String>) -> SymphonyError {
    TemplateRenderError {
        source: Box::new(std::io::Error::other(message.into())),
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

struct Scope<'a> {
    root: &'a Value,
    frames: Vec<Map<String, Value>>,
}

impl<'a> Scope<'a> {
    fn new(root: &'a Value) -> Self {
        Self {
            root,
            frames: Vec::new(),
        }
    }

    fn push(&mut self, frame: Map<String, Value>) {
        self.frames.push(frame);
    }

    fn pop(&mut self) {
        self.frames.pop();
    }

    fn lookup(&self, name: &str) -> Option<&Value> {
        for frame in self.frames.iter().rev() {
            if let Some(value) = frame.get(name) {
                return Some(value);
            }
        }
        self.root.get(name)
    }
}

fn render_nodes(nodes: &[Node], scope: &mut Scope<'_>, output: &mut String) -> Result<()> {
    for node in nodes {
        match node {
            Node::Text(text) => output.push_str(text),
            Node::Output(expression) => {
                let value = evaluate_filtered(expression, scope)?;
                output.push_str(&to_display_string(&value));
            }
            Node::If {
                branches,
                fallback,
            } => {
                let mut rendered = false;
                for (condition, body) in branches {
                    if evaluate_condition(condition, scope)? {
                        render_nodes(body, scope, output)?;
                        rendered = true;
                        break;
                    }
                }
                if !rendered
                    && let Some(fallback) = fallback
                {
                    render_nodes(fallback, scope, output)?;
                }
            }
            Node::For {
                binding,
                iterable,
                body,
            } => {
                let value = evaluate_filtered(iterable, scope)?;
                // Objects iterate over their values; scalars iterate zero times.
                let items: Vec<Value> = match value {
                    Value::Array(items) => items,
                    Value::Object(map) => map.into_values().collect(),
                    _ => Vec::new(),
                };
                let length = items.len();
                for (index, item) in items.into_iter().enumerate() {
                    let mut frame = Map::new();
                    frame.insert(binding.clone(), item);
                    let mut forloop = Map::new();
                    forloop.insert("index".into(), Value::Number(((index + 1) as u64).into()));
                    forloop.insert("index0".into(), Value::Number((index as u64).into()));
                    forloop.insert("first".into(), Value::Bool(index == 0));
                    forloop.insert("last".into(), Value::Bool(index + 1 == length));
                    forloop.insert("length".into(), Value::Number((length as u64).into()));
                    frame.insert("forloop".into(), Value::Object(forloop));
                    scope.push(frame);
                    let result = render_nodes(body, scope, output);
                    scope.pop();
                    result?;
                }
            }
        }
    }
    Ok(())
}

fn evaluate_condition(condition: &Condition, scope: &Scope<'_>) -> Result<bool> {
    match condition {
        Condition::Value(expression) => Ok(is_truthy(&evaluate_filtered(expression, scope)?)),
        Condition::Not(inner) => Ok(!evaluate_condition(inner, scope)?),
        Condition::And(left, right) => {
            Ok(evaluate_condition(left, scope)? && evaluate_condition(right, scope)?)
        }
        Condition::Or(left, right) => {
            Ok(evaluate_condition(left, scope)? || evaluate_condition(right, scope)?)
        }
        Condition::Comparison {
            left,
            operator,
            right,
        } => {
            let left = evaluate_filtered(left, scope)?;
            let right = evaluate_filtered(right, scope)?;
            compare_values(&left, *operator, &right)
        }
    }
}

fn evaluate_filtered(expression: &Filtered, scope: &Scope<'_>) -> Result<Value> {
    let mut value = evaluate_primary(&expression.base, scope)?;
    for filter in &expression.filters {
        value = apply_filter(filter, value, scope)?;
    }
    Ok(value)
}

fn evaluate_primary(primary: &Primary, scope: &Scope<'_>) -> Result<Value> {
    match primary {
        Primary::Literal(value) => Ok(value.clone()),
        Primary::Path(path) => resolve_path(path, scope),
    }
}

fn resolve_path(path: &str, scope: &Scope<'_>) -> Result<Value> {
    let mut segments = path.split('.');
    let Some(head) = segments.next() else {
        return Err(template_render_error("empty variable path".to_string()));
    };
    let mut current = scope
        .lookup(head)
        .cloned()
        .ok_or_else(|| template_render_error(format!("unknown variable `{path}`")))?;

    for segment in segments {
        current = match &current {
            Value::Object(map) => map
                .get(segment)
                .cloned()
                .ok_or_else(|| template_render_error(format!("unknown variable `{path}`")))?,
            Value::Array(items) => match segment.parse::<usize>() {
                Ok(index) if index < items.len() => items[index].clone(),
                _ => {
                    return Err(template_render_error(format!(
                        "unknown variable `{path}`"
                    )));
                }
            },
            _ => {
                return Err(template_render_error(format!(
                    "unknown variable `{path}`"
                )));
            }
        };
    }

    Ok(current)
}

fn is_truthy(value: &Value) -> bool {
    !matches!(value, Value::Null | Value::Bool(false))
}

fn to_display_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(value) => value.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(to_display_string)
            .collect::<Vec<_>>()
            .join(""),
        Value::Object(_) => value.to_string(),
    }
}

fn compare_values(
    left: &Value,
    operator: ComparisonOperator,
    right: &Value,
) -> Result<bool> {
    match operator {
        ComparisonOperator::Equal => Ok(values_equal(left, right)),
        ComparisonOperator::NotEqual => Ok(!values_equal(left, right)),
        ComparisonOperator::Contains => match (left, right) {
            (Value::String(haystack), Value::String(needle)) => Ok(haystack.contains(needle)),
            (Value::Array(items), needle) => Ok(items.iter().any(|item| values_equal(item, needle))),
            _ => Err(template_render_error(
                "`contains` expects a string or array on the left".to_string(),
            )),
        },
        _ => {
            let ordering = match (left, right) {
                (Value::Number(left), Value::Number(right)) => left
                    .as_f64()
                    .partial_cmp(&right.as_f64())
                    .ok_or_else(|| template_render_error("cannot compare NaN".to_string()))?,
                (Value::String(left), Value::String(right)) => left.cmp(right),
                _ => {
                    return Err(template_render_error(
                        "ordering comparison requires two numbers or two strings".to_string(),
                    ));
                }
            };
            Ok(match operator {
                ComparisonOperator::Greater => ordering.is_gt(),
                ComparisonOperator::GreaterOrEqual => ordering.is_ge(),
                ComparisonOperator::Less => ordering.is_lt(),
                ComparisonOperator::LessOrEqual => ordering.is_le(),
                _ => false,
            })
        }
    }
}

fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => match (left.as_f64(), right.as_f64()) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        },
        _ => left == right,
    }
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

fn apply_filter(call: &FilterCall, input: Value, scope: &Scope<'_>) -> Result<Value> {
    let mut args = Vec::with_capacity(call.args.len());
    for argument in &call.args {
        args.push(evaluate_primary(argument, scope)?);
    }

    let name = call.name.as_str();
    let result = match name {
        "default" => match args.first() {
            Some(fallback) => {
                if matches!(input, Value::Null) {
                    fallback.clone()
                } else {
                    input
                }
            }
            None => {
                return Err(template_render_error(
                    "filter `default` requires an argument".to_string(),
                ));
            }
        },
        "upcase" => Value::String(to_display_string(&input).to_uppercase()),
        "downcase" => Value::String(to_display_string(&input).to_lowercase()),
        "capitalize" => {
            let text = to_display_string(&input);
            let mut chars = text.chars();
            match chars.next() {
                Some(first) => {
                    Value::String(format!("{}{}", first.to_uppercase(), chars.as_str().to_lowercase()))
                }
                None => Value::String(String::new()),
            }
        }
        "strip" => Value::String(to_display_string(&input).trim().to_string()),
        "lstrip" => Value::String(to_display_string(&input).trim_start().to_string()),
        "rstrip" => Value::String(to_display_string(&input).trim_end().to_string()),
        "prepend" => Value::String(format!(
            "{}{}",
            to_display_string(args.first().unwrap_or(&Value::Null)),
            to_display_string(&input)
        )),
        "append" => Value::String(format!(
            "{}{}",
            to_display_string(&input),
            to_display_string(args.first().unwrap_or(&Value::Null))
        )),
        "replace" => {
            let from = to_display_string(args.first().unwrap_or(&Value::Null));
            let to = to_display_string(args.get(1).unwrap_or(&Value::Null));
            Value::String(to_display_string(&input).replace(&from, &to))
        }
        "truncate" => {
            let limit = as_integer(args.first()).unwrap_or(50).max(0) as usize;
            let ellipsis = match args.get(1) {
                Some(value) => to_display_string(value),
                None => "...".to_string(),
            };
            let text = to_display_string(&input);
            if text.chars().count() <= limit {
                Value::String(text)
            } else {
                let kept: String = text.chars().take(limit).collect();
                Value::String(format!("{kept}{ellipsis}"))
            }
        }
        "join" => {
            let separator = match args.first() {
                Some(value) => to_display_string(value),
                None => " ".to_string(),
            };
            match &input {
                Value::Array(items) => Value::String(
                    items
                        .iter()
                        .map(to_display_string)
                        .collect::<Vec<_>>()
                        .join(&separator),
                ),
                other => Value::String(to_display_string(other)),
            }
        }
        "split" => {
            let separator = to_display_string(args.first().unwrap_or(&Value::Null));
            Value::Array(
                to_display_string(&input)
                    .split(&separator)
                    .map(|part| Value::String(part.to_string()))
                    .collect(),
            )
        }
        "size" | "length" => match &input {
            Value::Array(items) => Value::Number((items.len() as u64).into()),
            Value::Object(map) => Value::Number((map.len() as u64).into()),
            other => Value::Number((to_display_string(other).chars().count() as u64).into()),
        },
        "first" => match input {
            Value::Array(mut items) => {
                if items.is_empty() {
                    Value::Null
                } else {
                    items.remove(0)
                }
            }
            other => Value::String(to_display_string(&other).chars().next().map(String::from).unwrap_or_default()),
        },
        "last" => match input {
            Value::Array(items) => items.into_iter().last().unwrap_or(Value::Null),
            other => Value::String(
                to_display_string(&other)
                    .chars()
                    .next_back()
                    .map(String::from)
                    .unwrap_or_default(),
            ),
        },
        "sort" => match input {
            Value::Array(mut items) => {
                items.sort_by(|left, right| {
                    let left = to_display_string(left);
                    let right = to_display_string(right);
                    left.cmp(&right)
                });
                Value::Array(items)
            }
            other => other,
        },
        "uniq" => match input {
            Value::Array(items) => {
                let mut seen: Vec<String> = Vec::new();
                let mut unique = Vec::new();
                for item in items {
                    let key = to_display_string(&item);
                    if !seen.contains(&key) {
                        seen.push(key);
                        unique.push(item);
                    }
                }
                Value::Array(unique)
            }
            other => other,
        },
        "reverse" => match input {
            Value::Array(mut items) => {
                items.reverse();
                Value::Array(items)
            }
            other => Value::String(to_display_string(&other).chars().rev().collect()),
        },
        "map" => {
            let key = to_display_string(args.first().unwrap_or(&Value::Null));
            match input {
                Value::Array(items) => Value::Array(
                    items
                        .into_iter()
                        .map(|item| match item {
                            Value::Object(map) => map.get(&key).cloned().unwrap_or(Value::Null),
                            _ => Value::Null,
                        })
                        .collect(),
                ),
                other => other,
            }
        }
        "plus" | "minus" | "times" | "divided_by" | "modulo" => {
            let left = as_number(&input).ok_or_else(|| {
                template_render_error(format!("filter `{name}` requires a number on the left"))
            })?;
            let right = args
                .first()
                .and_then(as_number)
                .ok_or_else(|| {
                    template_render_error(format!("filter `{name}` requires a numeric argument"))
                })?;
            let result = match name {
                "plus" => left + right,
                "minus" => left - right,
                "times" => left * right,
                "divided_by" => {
                    if right == 0.0 {
                        return Err(template_render_error(
                            "filter `divided_by` cannot divide by zero".to_string(),
                        ));
                    }
                    left / right
                }
                _ => {
                    if right == 0.0 {
                        return Err(template_render_error(
                            "filter `modulo` cannot divide by zero".to_string(),
                        ));
                    }
                    left % right
                }
            };
            number_value(result)
        }
        "round" | "ceil" | "floor" | "abs" => {
            let value = as_number(&input).ok_or_else(|| {
                template_render_error(format!("filter `{name}` requires a number"))
            })?;
            let result = match name {
                "ceil" => value.ceil(),
                "floor" => value.floor(),
                "abs" => value.abs(),
                _ => {
                    let digits = as_integer(args.first()).unwrap_or(0) as i32;
                    let factor = 10f64.powi(digits);
                    (value * factor).round() / factor
                }
            };
            number_value(result)
        }
        "json" => Value::String(input.to_string()),
        "inspect" => Value::String(format!("{input:?}")),
        "escape" => Value::String(
            to_display_string(&input)
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&#39;"),
        ),
        other => {
            return Err(template_render_error(format!("unknown filter `{other}`")));
        }
    };

    Ok(result)
}

fn number_value(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < i64::MAX as f64 {
        Value::Number((value as i64).into())
    } else {
        match serde_json::Number::from_f64(value) {
            Some(number) => Value::Number(number),
            None => Value::Null,
        }
    }
}

fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn as_integer(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(number)) => number.as_i64(),
        Some(Value::String(text)) => text.trim().parse().ok(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Expression parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum ExprToken {
    Ident(String),
    Integer(i64),
    Float(f64),
    Str(String),
    Pipe,
    Colon,
    Comma,
    Dot,
    Operator(String),
    OpenParen,
    CloseParen,
}

fn parse_condition(source: &str) -> Result<Condition> {
    let tokens = lex_expression(source)?;
    let mut cursor = 0;
    let condition = parse_or(&tokens, &mut cursor)?;
    if cursor != tokens.len() {
        return Err(template_parse_error(format!(
            "unexpected token in condition `{source}`"
        )));
    }
    Ok(condition)
}

fn parse_filtered(source: &str) -> Result<Filtered> {
    let tokens = lex_expression(source)?;
    let mut cursor = 0;
    let filtered = parse_filtered_tokens(&tokens, &mut cursor)?;
    if cursor != tokens.len() {
        return Err(template_parse_error(format!(
            "unexpected token in expression `{source}`"
        )));
    }
    Ok(filtered)
}

fn parse_or(tokens: &[ExprToken], cursor: &mut usize) -> Result<Condition> {
    let mut condition = parse_and(tokens, cursor)?;
    while matches!(tokens.get(*cursor), Some(ExprToken::Ident(word)) if word == "or") {
        *cursor += 1;
        let right = parse_and(tokens, cursor)?;
        condition = Condition::Or(Box::new(condition), Box::new(right));
    }
    Ok(condition)
}

fn parse_and(tokens: &[ExprToken], cursor: &mut usize) -> Result<Condition> {
    let mut condition = parse_not(tokens, cursor)?;
    while matches!(tokens.get(*cursor), Some(ExprToken::Ident(word)) if word == "and") {
        *cursor += 1;
        let right = parse_not(tokens, cursor)?;
        condition = Condition::And(Box::new(condition), Box::new(right));
    }
    Ok(condition)
}

fn parse_not(tokens: &[ExprToken], cursor: &mut usize) -> Result<Condition> {
    if matches!(tokens.get(*cursor), Some(ExprToken::Ident(word)) if word == "not") {
        *cursor += 1;
        return Ok(Condition::Not(Box::new(parse_not(tokens, cursor)?)));
    }
    parse_comparison(tokens, cursor)
}

fn parse_comparison(tokens: &[ExprToken], cursor: &mut usize) -> Result<Condition> {
    let left = parse_filtered_tokens(tokens, cursor)?;
    let operator = match tokens.get(*cursor) {
        Some(ExprToken::Operator(operator)) => {
            let operator = match operator.as_str() {
                "==" => ComparisonOperator::Equal,
                "!=" => ComparisonOperator::NotEqual,
                ">" => ComparisonOperator::Greater,
                ">=" => ComparisonOperator::GreaterOrEqual,
                "<" => ComparisonOperator::Less,
                "<=" => ComparisonOperator::LessOrEqual,
                other => {
                    return Err(template_parse_error(format!(
                        "unsupported operator `{other}`"
                    )));
                }
            };
            *cursor += 1;
            operator
        }
        Some(ExprToken::Ident(word)) if word == "contains" => {
            *cursor += 1;
            ComparisonOperator::Contains
        }
        _ => return Ok(Condition::Value(left)),
    };

    let right = parse_filtered_tokens(tokens, cursor)?;
    Ok(Condition::Comparison {
        left,
        operator,
        right,
    })
}

fn parse_filtered_tokens(tokens: &[ExprToken], cursor: &mut usize) -> Result<Filtered> {
    let base = parse_primary(tokens, cursor)?;
    let mut filters = Vec::new();
    while matches!(tokens.get(*cursor), Some(ExprToken::Pipe)) {
        *cursor += 1;
        let Some(ExprToken::Ident(name)) = tokens.get(*cursor) else {
            return Err(template_parse_error("expected a filter name after `|`".to_string()));
        };
        let name = name.clone();
        *cursor += 1;
        let mut args = Vec::new();
        if matches!(tokens.get(*cursor), Some(ExprToken::Colon)) {
            *cursor += 1;
            loop {
                args.push(parse_primary(tokens, cursor)?);
                if matches!(tokens.get(*cursor), Some(ExprToken::Comma)) {
                    *cursor += 1;
                    continue;
                }
                break;
            }
        }
        filters.push(FilterCall { name, args });
    }
    Ok(Filtered { base, filters })
}

fn parse_primary(tokens: &[ExprToken], cursor: &mut usize) -> Result<Primary> {
    let Some(token) = tokens.get(*cursor) else {
        return Err(template_parse_error("expected an expression".to_string()));
    };

    match token {
        ExprToken::Str(text) => {
            *cursor += 1;
            Ok(Primary::Literal(Value::String(text.clone())))
        }
        ExprToken::Integer(number) => {
            *cursor += 1;
            Ok(Primary::Literal(Value::Number((*number).into())))
        }
        ExprToken::Float(number) => {
            *cursor += 1;
            Ok(Primary::Literal(number_value(*number)))
        }
        ExprToken::Ident(name) => {
            match name.as_str() {
                "true" => {
                    *cursor += 1;
                    return Ok(Primary::Literal(Value::Bool(true)));
                }
                "false" => {
                    *cursor += 1;
                    return Ok(Primary::Literal(Value::Bool(false)));
                }
                "nil" | "null" => {
                    *cursor += 1;
                    return Ok(Primary::Literal(Value::Null));
                }
                _ => {}
            }

            let mut path = name.clone();
            *cursor += 1;
            loop {
                match tokens.get(*cursor) {
                    Some(ExprToken::Dot) => {
                        *cursor += 1;
                        match tokens.get(*cursor) {
                            Some(ExprToken::Ident(segment)) => {
                                path.push('.');
                                path.push_str(segment);
                                *cursor += 1;
                            }
                            Some(ExprToken::Integer(segment)) => {
                                path.push('.');
                                path.push_str(&segment.to_string());
                                *cursor += 1;
                            }
                            _ => {
                                return Err(template_parse_error(format!(
                                    "expected a path segment after `.` in `{path}`"
                                )));
                            }
                        }
                    }
                    Some(ExprToken::Ident(_)) => {
                        return Err(template_parse_error(format!(
                            "unexpected word after `{path}`"
                        )));
                    }
                    _ => break,
                }
            }
            Ok(Primary::Path(path))
        }
        ExprToken::OpenParen => {
            *cursor += 1;
            let filtered = parse_filtered_tokens(tokens, cursor)?;
            if !matches!(tokens.get(*cursor), Some(ExprToken::CloseParen)) {
                return Err(template_parse_error("missing `)`".to_string()));
            }
            *cursor += 1;
            Ok(filtered.base)
        }
        other => Err(template_parse_error(format!(
            "unexpected token `{other:?}` in expression"
        ))),
    }
}

fn lex_expression(source: &str) -> Result<Vec<ExprToken>> {
    let characters: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < characters.len() {
        let character = characters[index];
        if character.is_whitespace() {
            index += 1;
            continue;
        }

        match character {
            '|' => {
                tokens.push(ExprToken::Pipe);
                index += 1;
            }
            ':' => {
                tokens.push(ExprToken::Colon);
                index += 1;
            }
            ',' => {
                tokens.push(ExprToken::Comma);
                index += 1;
            }
            '(' => {
                tokens.push(ExprToken::OpenParen);
                index += 1;
            }
            ')' => {
                tokens.push(ExprToken::CloseParen);
                index += 1;
            }
            '.' => {
                tokens.push(ExprToken::Dot);
                index += 1;
            }
            '=' | '!' | '<' | '>' => {
                let mut operator = String::from(character);
                index += 1;
                if index < characters.len() && characters[index] == '=' {
                    operator.push('=');
                    index += 1;
                }
                tokens.push(ExprToken::Operator(operator));
            }
            '"' | '\'' => {
                let quote = character;
                index += 1;
                let mut text = String::new();
                let mut terminated = false;
                while index < characters.len() {
                    let character = characters[index];
                    if character == '\\' && index + 1 < characters.len() {
                        text.push(characters[index + 1]);
                        index += 2;
                        continue;
                    }
                    if character == quote {
                        terminated = true;
                        index += 1;
                        break;
                    }
                    text.push(character);
                    index += 1;
                }
                if !terminated {
                    return Err(template_parse_error("unterminated string literal".to_string()));
                }
                tokens.push(ExprToken::Str(text));
            }
            _ if character.is_ascii_digit() => {
                let mut digits = String::new();
                while index < characters.len() && characters[index].is_ascii_digit() {
                    digits.push(characters[index]);
                    index += 1;
                }
                let previous_is_dot = matches!(tokens.last(), Some(ExprToken::Dot));
                if !previous_is_dot
                    && index + 1 < characters.len()
                    && characters[index] == '.'
                    && characters[index + 1].is_ascii_digit()
                {
                    let mut number = digits;
                    number.push('.');
                    index += 1;
                    while index < characters.len() && characters[index].is_ascii_digit() {
                        number.push(characters[index]);
                        index += 1;
                    }
                    let parsed = number.parse::<f64>().map_err(|error| {
                        template_parse_error(format!("invalid number `{number}`: {error}"))
                    })?;
                    tokens.push(ExprToken::Float(parsed));
                } else {
                    let parsed = digits.parse::<i64>().map_err(|error| {
                        template_parse_error(format!("invalid number `{digits}`: {error}"))
                    })?;
                    tokens.push(ExprToken::Integer(parsed));
                }
            }
            _ if character.is_alphanumeric() || character == '_' => {
                let mut word = String::new();
                while index < characters.len()
                    && (characters[index].is_alphanumeric() || characters[index] == '_')
                {
                    word.push(characters[index]);
                    index += 1;
                }
                tokens.push(ExprToken::Ident(word));
            }
            other => {
                return Err(template_parse_error(format!(
                    "unexpected character `{other}` in expression"
                )));
            }
        }
    }

    Ok(tokens)
}

/// Minimal map of extra variables offered to templates by extensions.
pub type TemplateVariables = HashMap<String, Value>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BlockerRef, Issue};

    fn issue() -> Issue {
        Issue {
            id: "abc".to_string(),
            identifier: "MT-649".to_string(),
            title: "Fix the thing".to_string(),
            description: Some("Details".to_string()),
            priority: Some(2),
            state: "Todo".to_string(),
            branch_name: None,
            url: None,
            labels: vec!["frontend".to_string(), "bug".to_string()],
            blocked_by: vec![BlockerRef {
                id: Some("blocker".to_string()),
                identifier: Some("MT-1".to_string()),
                state: Some("Todo".to_string()),
            }],
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn renders_issue_fields_and_attempt() {
        let context = build_context(&issue(), Some(3));
        let rendered = render_source("{{ issue.identifier }}: {{ issue.title }}", &context).unwrap();
        assert_eq!(rendered, "MT-649: Fix the thing");

        let rendered = render_source("attempt={{ attempt }}", &context).unwrap();
        assert_eq!(rendered, "attempt=3");

        let first = build_context(&issue(), None);
        let rendered = render_source("attempt={{ attempt }}", &first).unwrap();
        assert_eq!(rendered, "attempt=");
    }

    #[test]
    fn fails_on_unknown_variable() {
        let context = build_context(&issue(), None);
        let error = render_source("{{ issue.nope }}", &context).unwrap_err();
        assert!(matches!(error, SymphonyError::TemplateRenderError { .. }));

        let error = render_source("{{ nope }}", &context).unwrap_err();
        assert!(matches!(error, SymphonyError::TemplateRenderError { .. }));
    }

    #[test]
    fn fails_on_unknown_filter() {
        let context = build_context(&issue(), None);
        let error = render_source("{{ issue.title | not_a_filter }}", &context).unwrap_err();
        assert!(matches!(error, SymphonyError::TemplateRenderError { .. }));
    }

    #[test]
    fn iterates_labels_and_blockers() {
        let context = build_context(&issue(), None);
        let source = "{% for label in issue.labels %}[{{ label }}]{% endfor %}";
        assert_eq!(render_source(source, &context).unwrap(), "[frontend][bug]");

        let source = "{% for blocker in issue.blocked_by %}{{ blocker.identifier }}{% endfor %}";
        assert_eq!(render_source(source, &context).unwrap(), "MT-1");
    }

    #[test]
    fn supports_conditionals_and_filters() {
        let context = build_context(&issue(), None);
        let source = "{% if issue.state == \"Todo\" %}blocked{% else %}ready{% endif %}";
        assert_eq!(render_source(source, &context).unwrap(), "blocked");

        let source = "{% if attempt %}retry{% else %}first{% endif %}";
        assert_eq!(render_source(source, &context).unwrap(), "first");

        let source = "{{ issue.labels | join: \", \" | upcase }}";
        assert_eq!(render_source(source, &context).unwrap(), "FRONTEND, BUG");

        let source = "{{ issue.priority | plus: 1 }}";
        assert_eq!(render_source(source, &context).unwrap(), "3");
    }

    #[test]
    fn supports_nested_loops_with_forloop() {
        let context = build_context(&issue(), None);
        let source = "{% for label in issue.labels %}{{ forloop.index }}:{{ label }}{% unless false %}{% endunless %}{% endfor %}";
        assert!(matches!(
            render_source(source, &context).unwrap_err(),
            SymphonyError::TemplateParseError { .. }
        ));

        let source = "{% for label in issue.labels %}{{ forloop.index }}{{ label }}{% endfor %}";
        assert_eq!(render_source(source, &context).unwrap(), "1frontend2bug");
    }

    #[test]
    fn fails_on_unbalanced_blocks() {
        let context = build_context(&issue(), None);
        assert!(matches!(
            render_source("{% if issue.id %}oops", &context).unwrap_err(),
            SymphonyError::TemplateParseError { .. }
        ));
        assert!(matches!(
            render_source("{% endif %}", &context).unwrap_err(),
            SymphonyError::TemplateParseError { .. }
        ));
        assert!(matches!(
            render_source("{{ issue.id }}", &serde_json::json!({})).unwrap_err(),
            SymphonyError::TemplateRenderError { .. }
        ));
    }

    #[test]
    fn ordering_comparison_requires_numbers() {
        let context = build_context(&issue(), None);
        let error = render_source("{% if issue.title > 1 %}x{% endif %}", &context).unwrap_err();
        assert!(matches!(error, SymphonyError::TemplateRenderError { .. }));
    }
}
