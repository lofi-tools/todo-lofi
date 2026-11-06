//! An in-process fake ACP agent plus the client tests that drive it.
//!
//! Tests exercise the real connection path — the same `connect_over` the app
//! uses — over an in-memory channel instead of a child process.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol as acp;
use agent_client_protocol::schema::v1::{
    AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, CreateTerminalRequest,
    Implementation, InitializeRequest, InitializeResponse, LoadSessionRequest, LoadSessionResponse,
    McpCapabilities, McpServer, NewSessionRequest, NewSessionResponse, PermissionOption,
    PromptCapabilities, PromptRequest, PromptResponse, ReadTextFileRequest, ReleaseTerminalRequest,
    RequestPermissionRequest, SessionConfigOption, SessionNotification, SessionUpdate,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, StopReason,
    TerminalOutputRequest, TextContent, ToolCall, ToolCallUpdate, WaitForTerminalExitRequest,
    WriteTextFileRequest,
};

#[cfg(test)]
use agent_client_protocol::schema::v1::{
    HttpHeader, McpServerHttp, PermissionOptionKind, SessionConfigId, SessionConfigOptionValue,
};

/// What the fake agent should do during a prompt.
#[derive(Clone, Debug, Default)]
pub struct FakeScript {
    /// Override the negotiated protocol version to test compatibility failures.
    pub protocol_version: Option<acp::schema::ProtocolVersion>,
    pub prompt_images: bool,
    /// Text delivered as streamed agent messages.
    pub chunks: Vec<String>,
    /// A tool call announced before the prompt finishes.
    pub tool_call: Option<ToolCall>,
    /// A tool-call update sent after the tool call.
    pub tool_update: Option<ToolCallUpdate>,
    /// Ask the client to approve this tool call.
    pub permission: Option<ToolCallUpdate>,
    /// Options offered with the permission request.
    pub permission_options: Vec<PermissionOption>,
    /// `session/load` fails, as an agent without resume support would.
    pub fail_load: bool,
    /// Conversation updates replayed before `session/load` completes.
    pub load_updates: Vec<SessionUpdate>,
    /// Ask the client to run a command.
    pub terminal: Option<(String, Vec<String>)>,
    pub release_terminal: bool,
    /// Ask the client to read these paths in order.
    pub read_paths: Vec<PathBuf>,
    /// Ask the client to write this content to this path.
    pub write: Option<(PathBuf, String)>,
}

/// Everything the fake agent observed, for assertions.
#[derive(Debug, Default)]
pub struct FakeObservations {
    pub initialize_count: usize,
    pub new_session_cwds: Vec<PathBuf>,
    pub new_session_additional: Vec<Vec<PathBuf>>,
    pub new_session_mcp: Vec<Vec<McpServer>>,
    /// `session/set_config_option` requests, as they went over the wire.
    pub config_changes: Vec<serde_json::Value>,
    pub load_attempts: Vec<String>,
    pub prompts: Vec<String>,
    pub prompt_content: Vec<Vec<ContentBlock>>,
    pub cancels: Vec<String>,
    pub permission_responses: Vec<String>,
    pub terminal_output: Vec<String>,
    pub terminal_exit: Vec<Option<u32>>,
    pub file_reads: Vec<Result<String, String>>,
    pub file_writes: Vec<Result<(), String>>,
}

pub type Shared = Arc<Mutex<FakeObservations>>;

pub async fn connect_fake_agent(
    script: FakeScript,
    options: crate::ConnectOptions,
) -> Result<(crate::AcpConnection, Shared), crate::AcpError> {
    let (transport, observations) = spawn_fake_agent(script);
    let connection = crate::connection::connect_over(move |_, _| transport, options).await?;
    Ok((connection, observations))
}

pub async fn connect_fake_agent_with_terminals(
    script: FakeScript,
    options: crate::ConnectOptions,
    terminals: Arc<dyn crate::terminal::TerminalBackend>,
) -> Result<(crate::AcpConnection, Shared), crate::AcpError> {
    let (transport, observations) = spawn_fake_agent(script);
    let connection = crate::connection::connect_over_with_terminals(
        move |_, _| transport,
        options,
        Some(terminals),
    )
    .await?;
    Ok((connection, observations))
}

/// Start a fake agent, returning the channel end the client should connect to.
pub fn spawn_fake_agent(script: FakeScript) -> (acp::Channel, Shared) {
    let (client_side, agent_side) = acp::Channel::duplex();
    let observations: Shared = Arc::new(Mutex::new(FakeObservations::default()));
    let script = Arc::new(script);

    let initialize_observations = observations.clone();
    let protocol_version = script.protocol_version;
    let prompt_images = script.prompt_images;
    let load_session = !script.load_updates.is_empty();
    let new_session_observations = observations.clone();
    let config_observations = observations.clone();
    let load_observations = observations.clone();
    let load_script = script.clone();
    let prompt_observations = observations.clone();
    let prompt_script = script.clone();
    let cancel_observations = observations.clone();

    tokio::spawn(async move {
        let result = acp::Agent
            .builder()
            .name("fake-agent")
            .on_receive_request(
                async move |initialize: InitializeRequest, responder, _cx| {
                    if let Ok(mut state) = initialize_observations.lock() {
                        state.initialize_count += 1;
                    }
                    responder.respond(
                        InitializeResponse::new(
                            protocol_version.unwrap_or(initialize.protocol_version),
                        )
                        .agent_capabilities(
                            AgentCapabilities::default()
                                .load_session(load_session)
                                .prompt_capabilities(PromptCapabilities::new().image(prompt_images))
                                .mcp_capabilities(McpCapabilities::new().http(true).sse(true)),
                        )
                        .agent_info(Implementation::new("fake-agent", "1.0")),
                    )
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: NewSessionRequest, responder, _cx| {
                    if let Ok(mut state) = new_session_observations.lock() {
                        state.new_session_cwds.push(request.cwd.clone());
                        state
                            .new_session_additional
                            .push(request.additional_directories.clone());
                        state.new_session_mcp.push(request.mcp_servers.clone());
                    }
                    responder.respond(
                        NewSessionResponse::new("session-1").config_options(advertised_options()),
                    )
                },
                acp::on_receive_request!(),
            )
            // A config change is answered with the new set and no
            // `config_option_update` notification, the way opencode answers a
            // model pick.
            .on_receive_request(
                async move |request: SetSessionConfigOptionRequest, responder, _cx| {
                    if let Ok(mut state) = config_observations.lock() {
                        state.config_changes.push(
                            serde_json::to_value(&request).unwrap_or(serde_json::Value::Null),
                        );
                    }
                    responder.respond(SetSessionConfigOptionResponse::new(options_after_a_switch()))
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: LoadSessionRequest, responder, cx| {
                    if let Ok(mut state) = load_observations.lock() {
                        state.load_attempts.push(request.session_id.0.to_string());
                    }
                    if load_script.fail_load {
                        responder.respond_with_internal_error("resume unsupported")
                    } else {
                        for update in &load_script.load_updates {
                            cx.send_notification(SessionNotification::new(
                                request.session_id.clone(),
                                update.clone(),
                            ))?;
                        }
                        responder.respond(LoadSessionResponse::new())
                    }
                },
                acp::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: PromptRequest, responder, cx| {
                    let text = request
                        .prompt
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text(text) => Some(text.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if let Ok(mut state) = prompt_observations.lock() {
                        state.prompts.push(text);
                        state.prompt_content.push(request.prompt);
                    }
                    let session_id = request.session_id.clone();
                    let connection = cx.clone();
                    // Cloned per call: this closure is `AsyncFnMut`, so it cannot
                    // hand its own captures to the spawned task.
                    let script = prompt_script.clone();
                    let observations = prompt_observations.clone();
                    // Run off the message handler: awaiting a request from inside
                    // a handler would deadlock this agent's own event loop.
                    cx.spawn(async move {
                        run_turn(script, observations, connection, session_id, responder).await
                    })?;
                    Ok(())
                },
                acp::on_receive_request!(),
            )
            .on_receive_notification(
                async move |notification: CancelNotification, _cx| {
                    if let Ok(mut state) = cancel_observations.lock() {
                        state.cancels.push(notification.session_id.0.to_string());
                    }
                    Ok(())
                },
                acp::on_receive_notification!(),
            )
            .connect_to(agent_side)
            .await;
        if let Err(error) = result {
            tracing::debug!("fake agent stopped: {error}");
        }
    });

    (client_side, observations)
}

async fn run_turn(
    script: Arc<FakeScript>,
    observations: Shared,
    connection: acp::ConnectionTo<acp::Client>,
    session_id: acp::schema::v1::SessionId,
    responder: acp::Responder<PromptResponse>,
) -> Result<(), acp::Error> {
    let cancel_count = || {
        observations.lock().map_or(0, |observed| {
            observed
                .cancels
                .iter()
                .filter(|id| id.as_str() == session_id.0.as_ref())
                .count()
        })
    };
    let initial_cancels = cancel_count();
    for chunk in &script.chunks {
        connection.send_notification(SessionNotification::new(
            session_id.clone(),
            SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                TextContent::new(chunk.clone()),
            ))),
        ))?;
    }
    if let Some(tool_call) = &script.tool_call {
        connection.send_notification(SessionNotification::new(
            session_id.clone(),
            SessionUpdate::ToolCall(tool_call.clone()),
        ))?;
    }
    if let Some(update) = &script.tool_update {
        connection.send_notification(SessionNotification::new(
            session_id.clone(),
            SessionUpdate::ToolCallUpdate(update.clone()),
        ))?;
    }
    if let Some(tool_call) = &script.permission {
        let response = connection
            .send_request(RequestPermissionRequest::new(
                session_id.clone(),
                tool_call.clone(),
                script.permission_options.clone(),
            ))
            .block_task()
            .await?;
        if let Ok(mut state) = observations.lock() {
            state
                .permission_responses
                .push(format!("{:?}", response.outcome));
        }
    }
    for path in &script.read_paths {
        let result = connection
            .send_request(ReadTextFileRequest::new(session_id.clone(), path.clone()))
            .block_task()
            .await
            .map(|response| response.content)
            .map_err(|error| error.to_string());
        if let Ok(mut state) = observations.lock() {
            state.file_reads.push(result);
        }
    }
    if let Some((path, content)) = &script.write {
        let result = connection
            .send_request(WriteTextFileRequest::new(
                session_id.clone(),
                path.clone(),
                content.clone(),
            ))
            .block_task()
            .await
            .map(|_| ())
            .map_err(|error| error.to_string());
        if let Ok(mut state) = observations.lock() {
            state.file_writes.push(result);
        }
    }
    if let Some((command, args)) = &script.terminal {
        let created = connection
            .send_request(
                CreateTerminalRequest::new(session_id.clone(), command.clone()).args(args.clone()),
            )
            .block_task()
            .await?;
        // Wait first, then snapshot: `terminal/output` is a point-in-time read,
        // so an early call legitimately returns nothing yet.
        let exit = connection
            .send_request(WaitForTerminalExitRequest::new(
                session_id.clone(),
                created.terminal_id.clone(),
            ))
            .block_task()
            .await?;
        let output = connection
            .send_request(TerminalOutputRequest::new(
                session_id.clone(),
                created.terminal_id.clone(),
            ))
            .block_task()
            .await?;
        if let Ok(mut state) = observations.lock() {
            state.terminal_output.push(output.output.clone());
            state.terminal_exit.push(exit.exit_status.exit_code);
        }
        if script.release_terminal {
            connection
                .send_request(ReleaseTerminalRequest::new(
                    session_id.clone(),
                    created.terminal_id,
                ))
                .block_task()
                .await?;
        }
    }
    let cancelled = cancel_count() > initial_cancels;
    let reason = if cancelled {
        StopReason::Cancelled
    } else {
        StopReason::EndTurn
    };
    responder.respond(PromptResponse::new(reason))
}

/// The option set opencode advertises when a session is created: a model, with
/// `a` selected.
fn advertised_options() -> Vec<SessionConfigOption> {
    serde_json::from_value(serde_json::json!([{
        "id": "model",
        "name": "Model",
        "category": "model",
        "type": "select",
        "currentValue": "a",
        "options": [
            {"value": "a", "name": "A"},
            {"value": "b", "name": "B"}
        ]
    }]))
    .expect("advertised options")
}

/// What an agent answers a switch to `b` with: the same model set, plus an
/// option that only exists for the model switched to.
fn options_after_a_switch() -> Vec<SessionConfigOption> {
    serde_json::from_value(serde_json::json!([
        {
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": "b",
            "options": [
                {"value": "a", "name": "A"},
                {"value": "b", "name": "B"}
            ]
        },
        {
            "id": "effort",
            "name": "Effort",
            "category": "thought_level",
            "type": "select",
            "currentValue": "default",
            "options": [{"value": "default", "name": "Default"}]
        }
    ]))
    .expect("options after a switch")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AcpEvent;
    use crate::connection::{ConnectOptions, connect_over};
    use crate::fs::SessionRoots;
    use crate::permissions::{PermissionDecision, PermissionRule, ToolPermissions};
    use crate::thread::{EntryKind, ToolStatus, Transcript};
    use agent_client_protocol::schema::v1::{SessionConfigKind, SessionConfigOptionCategory};

    /// Connect a client to a fresh fake agent using a temporary project directory.
    async fn connect_to_fake(
        script: FakeScript,
        tool_permissions: ToolPermissions,
    ) -> (crate::AcpConnection, Shared, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let roots = SessionRoots::new([dir.path().to_path_buf()]);
        let (client_side, observations) = spawn_fake_agent(script);
        let connection = connect_over(
            move |_events, _stderr| client_side,
            ConnectOptions {
                roots,
                tool_permissions,
            },
        )
        .await
        .expect("client should connect");
        (connection, observations, dir)
    }

    /// The value id the transcript's model-shaped option is set to.
    fn selected_model(transcript: &Transcript) -> String {
        let option = transcript
            .controls()
            .config_options
            .iter()
            .find(|option| matches!(option.category, Some(SessionConfigOptionCategory::Model)))
            .expect("a model option");
        let SessionConfigKind::Select(select) = &option.kind else {
            panic!("a select option");
        };
        select.current_value.0.to_string()
    }

    /// Ask for everything: the default policy.
    fn confirm_all() -> ToolPermissions {
        ToolPermissions::default()
    }

    #[tokio::test]
    async fn refuses_an_unsupported_protocol_version() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let (transport, _) = spawn_fake_agent(FakeScript {
            protocol_version: Some(acp::schema::ProtocolVersion::V0),
            ..FakeScript::default()
        });
        let result = connect_over(
            move |_, _| transport,
            ConnectOptions {
                roots: SessionRoots::new([directory.path().to_path_buf()]),
                tool_permissions: confirm_all(),
            },
        )
        .await;
        assert!(
            matches!(result, Err(crate::AcpError::UnsupportedVersion { agent }) if agent == "0")
        );
        Ok(())
    }

    /// Auto-approve everything: the old toggle's "on" position, as a policy.
    fn allow_all() -> ToolPermissions {
        ToolPermissions {
            default: PermissionRule::Allow,
            ..Default::default()
        }
    }

    async fn new_session(
        connection: &crate::AcpConnection,
        dir: &tempfile::TempDir,
    ) -> acp::schema::v1::SessionId {
        connection
            .requester
            .new_session(dir.path().to_path_buf(), Vec::new(), Vec::new())
            .await
            .expect("session")
            .0
    }

    /// Drain the events the client has already received into a transcript.
    fn drain(connection: &mut crate::AcpConnection, transcript: &mut Transcript) {
        while let Ok(event) = connection.events.try_recv() {
            if let AcpEvent::SessionUpdate(notification) = event {
                transcript.apply(&notification);
            }
        }
    }

    /// Wait until `check` passes, or give up after a second.
    async fn wait_for(mut check: impl FnMut() -> bool) {
        for _ in 0..100 {
            if check() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn connects_and_reports_agent_info() {
        let (connection, observations, _dir) =
            connect_to_fake(FakeScript::default(), confirm_all()).await;
        assert_eq!(connection.info.name, "fake-agent");
        assert_eq!(observations.lock().unwrap().initialize_count, 1);
    }

    #[tokio::test]
    async fn new_session_carries_cwd_and_additional_roots() {
        let (connection, observations, dir) =
            connect_to_fake(FakeScript::default(), confirm_all()).await;
        let extra = dir.path().join("extra");
        std::fs::create_dir_all(&extra).expect("extra dir");
        let session_id = connection
            .requester
            .new_session(dir.path().to_path_buf(), vec![extra.clone()], Vec::new())
            .await
            .expect("session")
            .0;
        assert_eq!(session_id.0.as_ref(), "session-1");
        let state = observations.lock().unwrap();
        assert_eq!(state.new_session_cwds, vec![dir.path().to_path_buf()]);
        assert_eq!(state.new_session_additional, vec![vec![extra]]);
    }

    /// The client attaches its loopback MCP endpoint as an ACP `McpServer::Http`
    /// when the agent advertises the transport (spec §7.2).
    #[tokio::test]
    async fn new_session_carries_the_mcp_server() {
        let (connection, observations, dir) =
            connect_to_fake(FakeScript::default(), confirm_all()).await;
        assert!(connection.info.mcp_http, "the agent advertises http MCP");
        let server = McpServer::Http(
            McpServerHttp::new("taskstream-desktop-coding", "http://127.0.0.1:4444/mcp")
                .headers(vec![HttpHeader::new("Authorization", "Bearer token")]),
        );
        connection
            .requester
            .new_session(dir.path().to_path_buf(), Vec::new(), vec![server.clone()])
            .await
            .expect("session");
        let state = observations.lock().unwrap();
        assert_eq!(state.new_session_mcp, vec![vec![server]]);
    }

    /// A model switch is reported by the response to it and nowhere else: opencode
    /// answers `session/set_config_option` with the new set and sends no
    /// `config_option_update`. This drives the pane's own sequence — take the
    /// advertised set, ask for a model, fold the answer in — over the real
    /// connection path, so the option the UI shows is the agent's answer rather
    /// than a local guess, and the request is the shape opencode validates (a
    /// config id and a plain value id).
    #[tokio::test]
    async fn a_config_change_is_reported_by_its_response_alone() {
        let (mut connection, observations, dir) =
            connect_to_fake(FakeScript::default(), confirm_all()).await;
        let (session_id, _modes, advertised) = connection
            .requester
            .new_session(dir.path().to_path_buf(), Vec::new(), Vec::new())
            .await
            .expect("session");
        let mut transcript = Transcript::default();
        transcript.seed_controls(None, advertised);
        assert_eq!(selected_model(&transcript), "a");

        let answer = connection
            .requester
            .set_config_option(
                &session_id,
                SessionConfigId::new("model"),
                SessionConfigOptionValue::value_id("b"),
            )
            .await
            .expect("config change");

        let observed = observations.lock().expect("observations");
        let request = observed.config_changes.first().expect("a config request");
        assert_eq!(request["configId"], "model");
        assert_eq!(
            request["value"], "b",
            "opencode rejects a value that is not a string"
        );
        drop(observed);

        transcript.set_config_options(answer);
        assert_eq!(selected_model(&transcript), "b");
        assert!(
            transcript
                .controls()
                .config_options
                .iter()
                .any(|option| option.id.0.as_ref() == "effort"),
            "the whole answered set is taken, not just the model"
        );

        // Nothing else reports it: the transcript knows only what the response
        // said, and the connection stays silent.
        drain(&mut connection, &mut transcript);
        assert_eq!(selected_model(&transcript), "b");
    }

    #[tokio::test]
    async fn load_session_reports_failure_so_the_caller_can_start_fresh() {
        let (connection, observations, dir) = connect_to_fake(
            FakeScript {
                fail_load: true,
                ..Default::default()
            },
            confirm_all(),
        )
        .await;
        let result = connection
            .requester
            .load_session(
                acp::schema::v1::SessionId::new("stale"),
                dir.path().to_path_buf(),
                Vec::new(),
                Vec::new(),
            )
            .await;
        assert!(result.is_err(), "a failed resume must be reported");
        assert_eq!(observations.lock().unwrap().load_attempts, vec!["stale"]);
    }

    #[tokio::test]
    async fn prompt_streams_one_transcript_row() {
        let (mut connection, observations, dir) = connect_to_fake(
            FakeScript {
                chunks: vec!["Hello ".into(), "world".into()],
                ..Default::default()
            },
            confirm_all(),
        )
        .await;
        let session_id = new_session(&connection, &dir).await;

        let mut transcript = Transcript::default();
        transcript.push_user_message("hi");
        let stop = connection
            .requester
            .prompt(&session_id, "hi".into())
            .await
            .expect("turn");
        drain(&mut connection, &mut transcript);

        assert_eq!(stop, StopReason::EndTurn);
        assert_eq!(observations.lock().unwrap().prompts, vec!["hi"]);
        let agent_text: Vec<&str> = transcript
            .entries()
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::AgentText { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            agent_text,
            vec!["Hello world"],
            "chunks must merge into one row"
        );
        assert_eq!(transcript.entries().len(), 2, "one user row, one agent row");
    }

    #[tokio::test]
    async fn tool_call_update_mutates_the_existing_row() {
        let mut fields = acp::schema::v1::ToolCallUpdateFields::new();
        fields.status = Some(acp::schema::v1::ToolCallStatus::Completed);
        let (mut connection, _observations, dir) = connect_to_fake(
            FakeScript {
                tool_call: Some(ToolCall::new("call-1", "Read file")),
                tool_update: Some(ToolCallUpdate::new("call-1", fields)),
                ..Default::default()
            },
            confirm_all(),
        )
        .await;
        let session_id = new_session(&connection, &dir).await;
        connection
            .requester
            .prompt(&session_id, "read it".into())
            .await
            .expect("turn");

        let mut transcript = Transcript::default();
        drain(&mut connection, &mut transcript);
        assert_eq!(
            transcript.entries().len(),
            1,
            "the update must not add a row"
        );
        match &transcript.entries()[0].kind {
            EntryKind::ToolCall { status, title, .. } => {
                assert_eq!(*status, ToolStatus::Completed);
                assert_eq!(title, "Read file");
            }
            other => panic!("expected a tool call row, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn permission_request_reaches_the_ui_and_the_answer_goes_back() {
        let mut fields = acp::schema::v1::ToolCallUpdateFields::new();
        fields.title = Some("Run tests".to_string());
        let options = vec![
            PermissionOption::new("allow-once", "Allow once", PermissionOptionKind::AllowOnce),
            PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
        ];
        let (mut connection, observations, dir) = connect_to_fake(
            FakeScript {
                permission: Some(ToolCallUpdate::new("call-2", fields)),
                permission_options: options,
                ..Default::default()
            },
            confirm_all(),
        )
        .await;
        let session_id = new_session(&connection, &dir).await;
        // The turn cannot finish until the request is answered, so it runs
        // concurrently with the answer loop rather than being awaited first.
        let turn = tokio::spawn({
            let requester = connection.requester.clone();
            let session_id = session_id.clone();
            async move { requester.prompt(&session_id, "go".into()).await }
        });

        let mut answered = false;
        while !turn.is_finished() {
            while let Ok(event) = connection.events.try_recv() {
                if let AcpEvent::PermissionRequested {
                    options, decision, ..
                } = event
                {
                    assert_eq!(options.len(), 2);
                    drop(
                        decision.answer(PermissionDecision::Selected(options[0].option_id.clone())),
                    );
                    answered = true;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        turn.await.expect("turn task").expect("turn");
        assert!(answered, "the client must surface permission requests");
        wait_for(|| {
            observations
                .lock()
                .unwrap()
                .permission_responses
                .iter()
                .any(|line| line.contains("allow-once"))
        })
        .await;
    }

    #[tokio::test]
    async fn allow_policy_answers_without_asking_the_ui() {
        let mut fields = acp::schema::v1::ToolCallUpdateFields::new();
        fields.title = Some("Write file".to_string());
        let options = vec![
            PermissionOption::new("once", "Once", PermissionOptionKind::AllowOnce),
            PermissionOption::new("always", "Always", PermissionOptionKind::AllowAlways),
        ];
        let (mut connection, observations, dir) = connect_to_fake(
            FakeScript {
                permission: Some(ToolCallUpdate::new("call-3", fields)),
                permission_options: options,
                ..Default::default()
            },
            allow_all(),
        )
        .await;
        let session_id = new_session(&connection, &dir).await;
        connection
            .requester
            .prompt(&session_id, "go".into())
            .await
            .expect("turn");

        let mut surfaced = false;
        while let Ok(event) = connection.events.try_recv() {
            if matches!(event, AcpEvent::PermissionRequested { .. }) {
                surfaced = true;
            }
        }
        assert!(!surfaced, "allow policy must not surface a prompt");
        wait_for(|| {
            observations
                .lock()
                .unwrap()
                .permission_responses
                .iter()
                .any(|line| line.contains("always"))
        })
        .await;
    }

    #[tokio::test]
    async fn deny_policy_rejects_without_asking_the_ui() {
        let mut fields = acp::schema::v1::ToolCallUpdateFields::new();
        fields.title = Some("Delete everything".to_string());
        let options = vec![
            PermissionOption::new("once", "Once", PermissionOptionKind::AllowOnce),
            PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
        ];
        let (mut connection, observations, dir) = connect_to_fake(
            FakeScript {
                permission: Some(ToolCallUpdate::new("call-4", fields)),
                permission_options: options,
                ..Default::default()
            },
            ToolPermissions {
                default: crate::permissions::PermissionRule::Deny,
                ..Default::default()
            },
        )
        .await;
        let session_id = new_session(&connection, &dir).await;
        connection
            .requester
            .prompt(&session_id, "go".into())
            .await
            .expect("turn");

        let mut surfaced = false;
        while let Ok(event) = connection.events.try_recv() {
            if matches!(event, AcpEvent::PermissionRequested { .. }) {
                surfaced = true;
            }
        }
        assert!(!surfaced, "deny policy must not surface a prompt");
        wait_for(|| {
            observations
                .lock()
                .unwrap()
                .permission_responses
                .iter()
                .any(|line| line.contains("reject"))
        })
        .await;
    }

    #[tokio::test]
    async fn cancel_is_sent_as_a_notification() {
        let (connection, observations, dir) =
            connect_to_fake(FakeScript::default(), confirm_all()).await;
        let session_id = new_session(&connection, &dir).await;
        connection
            .requester
            .cancel(&session_id)
            .expect("cancel should send");
        wait_for(|| !observations.lock().unwrap().cancels.is_empty()).await;
        assert_eq!(observations.lock().unwrap().cancels, vec!["session-1"]);
    }

    #[tokio::test]
    async fn file_requests_are_confined_to_the_session_roots() {
        // Relative paths resolve against the session's first root, so both requests
        // are expressed before the temporary directory exists.
        let target = PathBuf::from("notes.txt");
        let outside = std::env::temp_dir().join("acp-client-outside.txt");
        let (connection, observations, dir) = connect_to_fake(
            FakeScript {
                read_paths: vec![outside],
                write: Some((target.clone(), "hello".to_string())),
                ..Default::default()
            },
            confirm_all(),
        )
        .await;
        let session_id = new_session(&connection, &dir).await;
        connection
            .requester
            .prompt(&session_id, "files".into())
            .await
            .expect("turn");

        wait_for(|| {
            let state = observations.lock().unwrap();
            !state.file_reads.is_empty() && !state.file_writes.is_empty()
        })
        .await;
        let state = observations.lock().unwrap();
        assert!(
            state.file_reads[0].is_err(),
            "reads outside the roots must be refused"
        );
        assert!(
            state.file_writes[0].is_ok(),
            "writes inside the roots succeed"
        );
        drop(state);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(&target)).unwrap(),
            "hello"
        );
    }

    #[tokio::test]
    async fn terminals_round_trip_through_the_client() {
        let (connection, observations, dir) = connect_to_fake(
            FakeScript {
                terminal: Some(("echo".to_string(), vec!["terminal-ok".to_string()])),
                ..Default::default()
            },
            confirm_all(),
        )
        .await;
        let session_id = new_session(&connection, &dir).await;
        connection
            .requester
            .prompt(&session_id, "run it".into())
            .await
            .expect("turn");

        wait_for(|| !observations.lock().unwrap().terminal_output.is_empty()).await;
        let state = observations.lock().unwrap();
        assert!(
            state
                .terminal_output
                .iter()
                .any(|output| output.contains("terminal-ok")),
            "terminal output should reach the agent: {:?}",
            state.terminal_output
        );
        assert_eq!(state.terminal_exit, vec![Some(0)]);
    }
}
