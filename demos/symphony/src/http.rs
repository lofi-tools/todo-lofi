//! OPTIONAL HTTP observability server extension (Section 13.7).
//!
//! The server is a read-only view over orchestrator state plus a best-effort
//! refresh trigger. Orchestrator correctness never depends on it.

use crate::clock::format_timestamp;
use crate::error::Result;
use crate::error::SymphonyError::*;
use crate::orchestrator::{ObservabilityState, snapshot_of};
use log::{debug, info, warn};
use std::sync::MutexGuard;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Default bind host for the observability server.
pub const DEFAULT_BIND_HOST: &str = "127.0.0.1";

/// Largest request we accept, guarding against unbounded reads.
const MAX_REQUEST_BYTES: usize = 32 * 1024;

/// Bind the observability server to loopback unless configured otherwise.
pub async fn bind(port: u16) -> Result<TcpListener> {
    let address = format!("{DEFAULT_BIND_HOST}:{port}");
    TcpListener::bind(&address)
        .await
        .map_err(|error| HttpServerError {
            message: format!("could not bind {address}: {error}"),
        })
}

/// Serve the observability endpoints until the process exits.
pub async fn serve(listener: TcpListener, observability: ObservabilityState) -> Result<()> {
    let address = listener
        .local_addr()
        .map(|address| address.to_string())
        .unwrap_or_else(|_| DEFAULT_BIND_HOST.to_string());
    info!(target: "symphony", "outcome=started server=http address={address}");

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                warn!(target: "symphony", "outcome=failed action=accept_http error={error}");
                continue;
            }
        };

        let observability = observability.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, observability).await {
                debug!(target: "symphony", "peer={peer} outcome=failed action=http_request error={error}");
            }
        });
    }
}

struct Request {
    method: String,
    path: String,
}

struct Response {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Response {
    fn json(status: u16, reason: &'static str, body: serde_json::Value) -> Self {
        Self {
            status,
            reason,
            content_type: "application/json",
            body: body.to_string().into_bytes(),
        }
    }

    fn html(body: String) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: "text/html; charset=utf-8",
            body: body.into_bytes(),
        }
    }

    fn error(status: u16, reason: &'static str, code: &str, message: &str) -> Self {
        Self::json(
            status,
            reason,
            serde_json::json!({ "error": { "code": code, "message": message } }),
        )
    }
}

async fn handle_connection(mut stream: TcpStream, observability: ObservabilityState) -> Result<()> {
    let request = match read_request(&mut stream).await {
        Ok(Some(request)) => request,
        Ok(None) => return Ok(()),
        Err(error) => {
            let response = Response::error(400, "Bad Request", "bad_request", &error.to_string());
            write_response(&mut stream, &response).await?;
            return Ok(());
        }
    };

    let response = route(&request, observability);
    write_response(&mut stream, &response).await
}

async fn read_request(stream: &mut TcpStream) -> Result<Option<Request>> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];

    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| HttpServerError {
                message: error.to_string(),
            })?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);

        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > MAX_REQUEST_BYTES {
            return Err(HttpServerError {
                message: "request headers too large".to_string(),
            });
        }
    }

    let head = match buffer.windows(4).position(|window| window == b"\r\n\r\n") {
        Some(position) => &buffer[..position],
        None => buffer.as_slice(),
    };
    let head = String::from_utf8_lossy(head);
    let Some(request_line) = head.lines().next() else {
        return Err(HttpServerError {
            message: "empty request".to_string(),
        });
    };

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/").to_string();
    if method.is_empty() {
        return Err(HttpServerError {
            message: "missing request method".to_string(),
        });
    }

    // Query strings are not used by this API.
    let path = target
        .split('?')
        .next()
        .unwrap_or("/")
        .trim_end_matches('/')
        .to_string();
    let path = if path.is_empty() { "/".to_string() } else { path };

    Ok(Some(Request { method, path }))
}

async fn write_response(stream: &mut TcpStream, response: &Response) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.reason,
        response.content_type,
        response.body.len()
    );

    stream
        .write_all(head.as_bytes())
        .await
        .map_err(|error| HttpServerError {
            message: error.to_string(),
        })?;
    stream
        .write_all(&response.body)
        .await
        .map_err(|error| HttpServerError {
            message: error.to_string(),
        })?;
    stream.flush().await.map_err(|error| HttpServerError {
        message: error.to_string(),
    })
}

fn route(request: &Request, observability: ObservabilityState) -> Response {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => Response::html(render_dashboard(&snapshot(&observability))),
        ("GET", "/api/v1/state") => {
            Response::json(200, "OK", snapshot(&observability).to_json())
        }
        ("POST", "/api/v1/refresh") => {
            observability.refresh.notify_one();
            info!(target: "symphony", "outcome=queued action=refresh");
            Response::json(
                202,
                "Accepted",
                serde_json::json!({
                    "queued": true,
                    "coalesced": false,
                    "requested_at": format_timestamp(Some(std::time::SystemTime::now())),
                    "operations": ["poll", "reconcile"],
                }),
            )
        }
        (_, "/") | (_, "/api/v1/state") | (_, "/api/v1/refresh") => Response::error(
            405,
            "Method Not Allowed",
            "method_not_allowed",
            "method not allowed for this route",
        ),
        ("GET", path) => match path.strip_prefix("/api/v1/") {
            Some(identifier) if !identifier.is_empty() && !identifier.contains('/') => {
                issue_detail(identifier, &observability)
            }
            _ => Response::error(404, "Not Found", "not_found", "unknown route"),
        },
        (_, path) if path.starts_with("/api/v1/") => Response::error(
            405,
            "Method Not Allowed",
            "method_not_allowed",
            "method not allowed for this route",
        ),
        _ => Response::error(404, "Not Found", "not_found", "unknown route"),
    }
}

fn snapshot(observability: &ObservabilityState) -> crate::domain::RuntimeSnapshot {
    let state = observability
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    snapshot_of(&state)
}

fn issue_detail(identifier: &str, observability: &ObservabilityState) -> Response {
    let state: MutexGuard<'_, crate::domain::OrchestratorState> = observability
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let running = state
        .running
        .values()
        .find(|entry| entry.attempt.issue_identifier == identifier);
    let retry = state
        .retry_attempts
        .values()
        .find(|entry| entry.identifier == identifier);

    if running.is_none() && retry.is_none() {
        return Response::error(
            404,
            "Not Found",
            "issue_not_found",
            &format!("issue `{identifier}` is not tracked in memory"),
        );
    }

    let running_json = running.map(|entry| {
        serde_json::json!({
            "session_id": entry.session.session_id,
            "turn_count": entry.session.turn_count,
            "state": entry.issue.state,
            "started_at": format_timestamp(Some(entry.attempt.started_at)),
            "last_event": entry.session.last_codex_event,
            "last_message": entry.session.last_codex_message.clone().unwrap_or_default(),
            "last_event_at": format_timestamp(entry.session.last_codex_timestamp),
            "attempt": entry.attempt.attempt,
            "status": entry.attempt.status.as_str(),
            "tokens": {
                "input_tokens": entry.session.codex_input_tokens,
                "output_tokens": entry.session.codex_output_tokens,
                "total_tokens": entry.session.codex_total_tokens,
            },
            "workspace": { "path": entry.attempt.workspace_path.to_string_lossy() },
        })
    });

    let retry_json = retry.map(|entry| {
        serde_json::json!({
            "attempt": entry.attempt,
            "due_at": crate::clock::now_plus_monotonic(entry.due_at_ms),
            "error": entry.error,
        })
    });

    let status = if running.is_some() {
        "running"
    } else {
        "retrying"
    };
    let issue_id = running
        .map(|entry| entry.attempt.issue_id.clone())
        .or_else(|| retry.map(|entry| entry.issue_id.clone()))
        .unwrap_or_default();
    let workspace_path = running
        .map(|entry| entry.attempt.workspace_path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let restart_count = state.running.len() as u64;
    let last_error = retry.and_then(|entry| entry.error.clone());

    Response::json(
        200,
        "OK",
        serde_json::json!({
            "issue_identifier": identifier,
            "issue_id": issue_id,
            "status": status,
            "workspace": { "path": workspace_path },
            "attempts": {
                "restart_count": restart_count,
                "current_retry_attempt": retry_json.as_ref().and_then(|retry| retry.get("attempt")).cloned().unwrap_or(serde_json::Value::Null),
            },
            "running": running_json,
            "retry": retry_json,
            "logs": { "codex_session_logs": [] },
            "recent_events": [],
            "last_error": last_error,
            "tracked": {},
        }),
    )
}

fn render_dashboard(snapshot: &crate::domain::RuntimeSnapshot) -> String {
    let mut rows = String::new();
    for row in &snapshot.running {
        rows.push_str(&format!(
            "<tr><td>{identifier}</td><td>{state}</td><td>{session}</td><td class=\"num\">{turns}</td>\
             <td>{last_event}</td><td class=\"num\">{input}</td><td class=\"num\">{output}</td>\
             <td class=\"num\">{total}</td><td class=\"path\">{workspace}</td></tr>",
            identifier = escape(&row.issue_identifier),
            state = escape(&row.state),
            session = escape(row.session_id.as_deref().unwrap_or("-")),
            turns = row.turn_count,
            last_event = escape(row.last_event.as_deref().unwrap_or("-")),
            input = row.tokens.input_tokens,
            output = row.tokens.output_tokens,
            total = row.tokens.total_tokens,
            workspace = escape(&row.workspace_path),
        ));
    }
    if rows.is_empty() {
        rows.push_str("<tr><td colspan=\"9\" class=\"empty\">No running sessions</td></tr>");
    }

    let mut retry_rows = String::new();
    for row in &snapshot.retrying {
        retry_rows.push_str(&format!(
            "<tr><td>{identifier}</td><td class=\"num\">{attempt}</td><td>{due}</td><td>{error}</td></tr>",
            identifier = escape(&row.issue_identifier),
            attempt = row.attempt,
            due = escape(
                format_timestamp(Some(row.due_at))
                    .as_str()
                    .unwrap_or("-")
            ),
            error = escape(row.error.as_deref().unwrap_or("-")),
        ));
    }
    if retry_rows.is_empty() {
        retry_rows.push_str("<tr><td colspan=\"4\" class=\"empty\">No queued retries</td></tr>");
    }

    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<title>Symphony</title>\n<style>\n\
:root {{ color-scheme: dark }}\n\
body {{ margin: 0; padding: 24px; background: #101418; color: #e6e6e6;\n\
  font: 14px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; }}\n\
h1 {{ font-size: 18px; margin: 0 0 4px; }}\n\
h2 {{ font-size: 14px; margin: 28px 0 8px; color: #9ecbff; }}\n\
.sub {{ color: #8b949e; margin-bottom: 8px; }}\n\
table {{ width: 100%; border-collapse: collapse; }}\n\
th, td {{ text-align: left; padding: 6px 10px; border-bottom: 1px solid #232a31; white-space: nowrap; }}\n\
th {{ color: #8b949e; font-weight: 600; }}\n\
td.num {{ text-align: right; }}\n\
td.path {{ max-width: 380px; overflow: hidden; text-overflow: ellipsis; }}\n\
.empty {{ color: #6e7681; }}\n\
.cards {{ display: flex; flex-wrap: wrap; gap: 12px; margin-top: 12px; }}\n\
.card {{ background: #161b22; border: 1px solid #232a31; border-radius: 8px; padding: 10px 14px; min-width: 140px; }}\n\
.card .label {{ color: #8b949e; font-size: 12px; }}\n\
.card .value {{ font-size: 18px; }}\n\
</style>\n</head>\n<body>\n\
<h1>Symphony</h1>\n\
<div class=\"sub\">generated_at={generated}</div>\n\
<div class=\"cards\">\n\
<div class=\"card\"><div class=\"label\">running</div><div class=\"value\">{running_count}</div></div>\n\
<div class=\"card\"><div class=\"label\">retrying</div><div class=\"value\">{retrying_count}</div></div>\n\
<div class=\"card\"><div class=\"label\">input tokens</div><div class=\"value\">{input}</div></div>\n\
<div class=\"card\"><div class=\"label\">output tokens</div><div class=\"value\">{output}</div></div>\n\
<div class=\"card\"><div class=\"label\">total tokens</div><div class=\"value\">{total}</div></div>\n\
<div class=\"card\"><div class=\"label\">runtime (s)</div><div class=\"value\">{seconds:.1}</div></div>\n\
</div>\n\
<h2>Running sessions</h2>\n\
<table>\n<thead><tr><th>issue</th><th>state</th><th>session</th><th>turns</th><th>last event</th>\
<th>input</th><th>output</th><th>total</th><th>workspace</th></tr></thead>\n\
<tbody>{rows}</tbody>\n</table>\n\
<h2>Retry queue</h2>\n\
<table>\n<thead><tr><th>issue</th><th>attempt</th><th>due at</th><th>error</th></tr></thead>\n\
<tbody>{retry_rows}</tbody>\n</table>\n\
</body>\n</html>\n",
        generated = format_timestamp(Some(snapshot.generated_at))
            .as_str()
            .unwrap_or("-"),
        running_count = snapshot.running.len(),
        retrying_count = snapshot.retrying.len(),
        input = snapshot.codex_totals.input_tokens,
        output = snapshot.codex_totals.output_tokens,
        total = snapshot.codex_totals.total_tokens,
        seconds = snapshot.codex_totals.seconds_running,
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;
    use crate::orchestrator::snapshot_of;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, RwLock};
    use std::time::SystemTime;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpStream;

    fn observability_with_state(state: OrchestratorState) -> ObservabilityState {
        ObservabilityState {
            state: Arc::new(Mutex::new(state)),
            refresh: Arc::new(tokio::sync::Notify::new()),
            effective: Arc::new(RwLock::new(crate::config::EffectiveWorkflow {
                path: PathBuf::from("/tmp/WORKFLOW.md"),
                config: crate::config::ServiceConfig {
                    tracker: crate::config::TrackerConfig {
                        kind: "linear".to_string(),
                        endpoint: "https://api.linear.app/graphql".to_string(),
                        api_key: "test".to_string(),
                        project_slug: "proj".to_string(),
                        active_states: vec!["Todo".to_string()],
                        terminal_states: vec!["Done".to_string()],
                    },
                    polling: crate::config::PollingConfig { interval_ms: 30_000 },
                    workspace: crate::config::WorkspaceConfig {
                        root: PathBuf::from("/tmp/ws"),
                    },
                    hooks: crate::config::HooksConfig::default(),
                    agent: crate::config::AgentConfig {
                        max_concurrent_agents: 10,
                        max_turns: 20,
                        max_retry_backoff_ms: 300_000,
                        max_concurrent_agents_by_state: HashMap::new(),
                    },
                    codex: crate::config::CodexConfig {
                        command: "codex app-server".to_string(),
                        approval_policy: "never".to_string(),
                        thread_sandbox: "workspace-write".to_string(),
                        turn_sandbox_policy: "workspace-write".to_string(),
                        turn_timeout_ms: 1_000,
                        read_timeout_ms: 1_000,
                        stall_timeout_ms: 300_000,
                    },
                    server: crate::config::ServerConfig { port: None },
                },
                prompt_template: crate::prompt::parse("prompt").expect("template"),
            })),
        }
    }

    fn running_entry() -> RunningEntry {
        RunningEntry {
            attempt: RunAttempt {
                issue_id: "abc123".to_string(),
                issue_identifier: "MT-649".to_string(),
                attempt: None,
                workspace_path: PathBuf::from("/tmp/symphony_workspaces/MT-649"),
                started_at: SystemTime::now(),
                status: RunAttemptStatus::StreamingTurn,
                error: None,
            },
            issue: Issue {
                id: "abc123".to_string(),
                identifier: "MT-649".to_string(),
                title: "Fix".to_string(),
                description: None,
                priority: Some(1),
                state: "In Progress".to_string(),
                branch_name: None,
                url: None,
                labels: Vec::new(),
                blocked_by: Vec::new(),
                created_at: None,
                updated_at: None,
            },
            session: LiveSession {
                session_id: Some("thread-1-turn-1".to_string()),
                turn_count: 7,
                last_codex_event: Some("notification".to_string()),
                last_codex_message: Some("Working on tests".to_string()),
                codex_input_tokens: 1_200,
                codex_output_tokens: 800,
                codex_total_tokens: 2_000,
                ..LiveSession::default()
            },
            retry_attempt: None,
        }
    }

    fn request(method: &str, path: &str) -> Request {
        Request {
            method: method.to_string(),
            path: path.to_string(),
        }
    }

    #[test]
    fn state_endpoint_returns_snapshot() {
        let mut state = OrchestratorState {
            max_concurrent_agents: 10,
            ..OrchestratorState::default()
        };
        state.running.insert("abc123".to_string(), running_entry());
        let observability = observability_with_state(state);

        let response = route(&request("GET", "/api/v1/state"), observability);
        assert_eq!(response.status, 200);
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("json body");
        assert_eq!(body["counts"]["running"], 1);
        assert_eq!(body["running"][0]["issue_identifier"], "MT-649");
        assert_eq!(body["running"][0]["turn_count"], 7);
        assert_eq!(body["codex_totals"]["total_tokens"], 0);
    }

    #[test]
    fn issue_endpoint_returns_detail_and_404() {
        let mut state = OrchestratorState {
            max_concurrent_agents: 10,
            ..OrchestratorState::default()
        };
        state.running.insert("abc123".to_string(), running_entry());
        let observability = observability_with_state(state);

        let response = route(&request("GET", "/api/v1/MT-649"), observability.clone());
        assert_eq!(response.status, 200);
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("json body");
        assert_eq!(body["status"], "running");
        assert_eq!(body["issue_id"], "abc123");
        assert_eq!(
            body["workspace"]["path"],
            "/tmp/symphony_workspaces/MT-649"
        );
        assert_eq!(body["running"]["tokens"]["total_tokens"], 2_000);

        let response = route(&request("GET", "/api/v1/MT-999"), observability);
        assert_eq!(response.status, 404);
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("json body");
        assert_eq!(body["error"]["code"], "issue_not_found");
    }

    #[test]
    fn refresh_returns_202_and_triggers_a_poll() {
        let observability = observability_with_state(OrchestratorState::default());
        let refresh = Arc::clone(&observability.refresh);

        let response = route(&request("POST", "/api/v1/refresh"), observability);
        assert_eq!(response.status, 202);
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("json body");
        assert_eq!(body["queued"], true);
        assert_eq!(body["operations"][0], "poll");

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_millis(100), refresh.notified())
                .await
                .expect("refresh was notified");
        });
    }

    #[test]
    fn unsupported_methods_return_405_and_unknown_routes_404() {
        let observability = observability_with_state(OrchestratorState::default());
        assert_eq!(
            route(&request("PUT", "/api/v1/state"), observability.clone()).status,
            405
        );
        assert_eq!(
            route(&request("DELETE", "/api/v1/refresh"), observability.clone()).status,
            405
        );
        assert_eq!(
            route(&request("GET", "/nope"), observability).status,
            404
        );
    }

    #[test]
    fn dashboard_escapes_and_renders_state() {
        let mut state = OrchestratorState {
            max_concurrent_agents: 10,
            ..OrchestratorState::default()
        };
        state.running.insert("abc123".to_string(), running_entry());
        let html = render_dashboard(&snapshot_of(&state));

        assert!(html.contains("MT-649"));
        assert!(html.contains("Working on tests") || html.contains("notification"));
        assert!(html.contains("Retry queue"));

        let mut escaped = OrchestratorState {
            max_concurrent_agents: 10,
            ..OrchestratorState::default()
        };
        let mut entry = running_entry();
        entry.attempt.issue_identifier = "<script>alert(1)</script>".to_string();
        escaped.running.insert("abc123".to_string(), entry);
        let html = render_dashboard(&snapshot_of(&escaped));
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[tokio::test]
    async fn serves_over_tcp() {
        let mut state = OrchestratorState {
            max_concurrent_agents: 10,
            ..OrchestratorState::default()
        };
        state.running.insert("abc123".to_string(), running_entry());
        let observability = observability_with_state(state);

        let listener = bind(0).await.expect("binds an ephemeral port");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            if let Err(error) = serve(listener, observability).await {
                eprintln!("server error: {error}");
            }
        });

        let mut stream = TcpStream::connect(address).await.expect("connect");
        stream
            .write_all(b"GET /api/v1/state HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("write request");

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        let response = String::from_utf8_lossy(&response);

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("issue_identifier"));
        assert!(response.contains("MT-649"));
    }
}
