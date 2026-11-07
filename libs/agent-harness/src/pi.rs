//! Pi's byte-framed stdio RPC transport.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::{HarnessError, HarnessResult, SpawnSpec};

const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const STDERR_TAIL_BYTES: usize = 16 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

pub struct PiRpc {
    input: Option<mpsc::Sender<Vec<u8>>>,
    replies: Replies,
    events: Mutex<mpsc::UnboundedReceiver<PiRpcEvent>>,
    supervisor: JoinHandle<()>,
    writer: JoinHandle<()>,
    shutdown_request: Option<oneshot::Sender<()>>,
}

pub enum PiRpcEvent {
    Record(Value),
    Exited {
        code: Option<i32>,
        stderr: String,
        protocol_error: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiSessionState {
    pub session_id: String,
    pub session_file: Option<std::path::PathBuf>,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub pending_message_count: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PromptDisposition {
    Started,
    Queued,
    Handled,
}

impl PromptDisposition {
    pub fn from_reply(mut reply: Value) -> HarnessResult<Self> {
        let disposition = reply
            .as_object_mut()
            .and_then(|object| object.remove("disposition"))
            .ok_or_else(|| protocol("Pi RPC does not support prompt disposition"))?;
        serde_json::from_value(disposition)
            .map_err(|error| protocol(format!("invalid Pi prompt disposition: {error}")))
    }
}

#[derive(Deserialize)]
struct Reply {
    command: String,
    success: bool,
    #[serde(default)]
    data: Value,
    error: Option<String>,
}

type Replies = Arc<Mutex<ReplyState>>;

#[derive(Default)]
struct ReplyState {
    pending: HashMap<String, oneshot::Sender<Reply>>,
    closed: bool,
}

impl PiRpc {
    pub async fn start(spec: &SpawnSpec) -> HarnessResult<(Self, PiSessionState)> {
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .args(["--mode", "rpc"])
            .current_dir(&spec.cwd)
            .envs(&spec.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|source| HarnessError::Spawn {
            program: spec.program.clone(),
            source,
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(protocol("Pi did not expose its RPC pipes"));
        };
        let (input, outgoing) = mpsc::channel(32);
        let (events, receiver) = mpsc::unbounded_channel();
        let (shutdown_request, mut shutdown) = oneshot::channel();
        let replies = Replies::default();
        let writer = tokio::spawn(write_input(stdin, outgoing));
        let supervisor = tokio::spawn({
            let replies = replies.clone();
            async move {
                let stderr_tail = Arc::new(Mutex::new(Vec::new()));
                let mut stderr = tokio::spawn(drain_stderr(stderr, stderr_tail.clone()));
                let reading = read_output(BufReader::new(stdout), &replies, &events);
                tokio::pin!(reading);
                let (protocol_error, stopped) = tokio::select! {
                    result = &mut reading => (result.err(), false),
                    _ = &mut shutdown => {
                        if let Err(error) = terminate(&child) {
                            tracing::debug!(%error, "could not signal Pi during shutdown");
                        }
                        let error = match timeout(SHUTDOWN_TIMEOUT, &mut reading).await {
                            Ok(result) => result.err(),
                            Err(_) => Some(protocol("Pi did not close its RPC output during shutdown")),
                        };
                        (error, true)
                    }
                };
                {
                    let mut replies = replies.lock().await;
                    replies.closed = true;
                    replies.pending.clear();
                }
                if protocol_error.is_some() {
                    if let Err(error) = terminate(&child) {
                        tracing::debug!(%error, "could not stop the Pi process group after an RPC error");
                    }
                    if let Err(error) = child.start_kill() {
                        tracing::debug!(%error, "could not stop Pi after an RPC framing error");
                    }
                }
                let waiting = if stopped {
                    timeout(SHUTDOWN_TIMEOUT, child.wait()).await
                } else {
                    tokio::select! {
                        result = timeout(SHUTDOWN_TIMEOUT, child.wait()) => result,
                        _ = &mut shutdown => {
                            if let Err(error) = terminate(&child) {
                                tracing::debug!(%error, "could not signal Pi during shutdown after output closed");
                            }
                            timeout(SHUTDOWN_TIMEOUT, child.wait()).await
                        }
                    }
                };
                let status = match waiting {
                    Ok(status) => status,
                    Err(_) => {
                        if let Err(error) = terminate(&child) {
                            tracing::debug!(%error, "could not stop the Pi process group after timeout");
                        }
                        match child.start_kill() {
                            Ok(()) => child.wait().await,
                            Err(error) => Err(error),
                        }
                    }
                };
                let code = match status {
                    Ok(status) => status.code(),
                    Err(error) => {
                        tracing::debug!(%error, "could not get the Pi exit status");
                        None
                    }
                };
                if timeout(SHUTDOWN_TIMEOUT, &mut stderr).await.is_err() {
                    stderr.abort();
                }
                let stderr = String::from_utf8_lossy(&stderr_tail.lock().await).into_owned();
                if let Err(error) = events.send(PiRpcEvent::Exited {
                    code,
                    stderr,
                    protocol_error: protocol_error.map(|error| error.to_string()),
                }) {
                    tracing::debug!(%error, "could not report Pi exit");
                }
            }
        });
        let mut rpc = Self {
            input: Some(input),
            replies,
            events: Mutex::new(receiver),
            supervisor,
            writer,
            shutdown_request: Some(shutdown_request),
        };
        // Separators in the echoed ID distinguish byte framing from Unicode line splitting.
        let id = format!("shepherd-probe\u{2028}\u{2029}-{}", uuid::Uuid::new_v4());
        let response = timeout(
            PROBE_TIMEOUT,
            rpc.request_with_id("get_state", Map::new(), id),
        )
        .await;
        let state = match response {
            Ok(Ok(data)) => serde_json::from_value::<PiSessionState>(data)
                .map_err(|error| protocol(format!("invalid Pi get_state reply: {error}")))?,
            Ok(Err(HarnessError::ChannelClosed)) => {
                return Err(rpc.startup_exit().await);
            }
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err(protocol("Pi get_state probe timed out")),
        };
        if state.session_id.is_empty() {
            return Err(protocol("Pi get_state returned an empty session ID"));
        }
        Ok((rpc, state))
    }

    pub async fn request(&self, command: &str, fields: Map<String, Value>) -> HarnessResult<Value> {
        self.request_with_id(command, fields, uuid::Uuid::new_v4().to_string())
            .await
    }

    async fn request_with_id(
        &self,
        command: &str,
        mut fields: Map<String, Value>,
        id: String,
    ) -> HarnessResult<Value> {
        fields.insert("type".into(), json!(command));
        fields.insert("id".into(), json!(id));
        let (sender, receiver) = oneshot::channel();
        {
            let mut replies = self.replies.lock().await;
            if replies.closed {
                return Err(HarnessError::ChannelClosed);
            }
            replies.pending.retain(|_, reply| !reply.is_closed());
            replies.pending.insert(id.clone(), sender);
        }
        if let Err(error) = self.send(Value::Object(fields)).await {
            self.replies.lock().await.pending.remove(&id);
            return Err(error);
        }
        let reply = receiver.await.map_err(|_| HarnessError::ChannelClosed)?;
        if reply.command != command {
            return Err(protocol(format!(
                "Pi replied to {command} with {}",
                reply.command
            )));
        }
        if !reply.success {
            return Err(protocol(format!(
                "Pi {command} failed: {}",
                reply.error.as_deref().unwrap_or("no error detail")
            )));
        }
        Ok(reply.data)
    }

    pub async fn reply_to_extension(
        &self,
        id: &str,
        mut answer: Map<String, Value>,
    ) -> HarnessResult<()> {
        answer.insert("type".into(), json!("extension_ui_response"));
        answer.insert("id".into(), json!(id));
        self.send(Value::Object(answer)).await
    }

    pub async fn next_event(&self) -> Option<PiRpcEvent> {
        self.events.lock().await.recv().await
    }

    pub async fn shutdown(mut self) -> HarnessResult<()> {
        self.input = None;
        if let Some(request) = self.shutdown_request.take()
            && request.send(()).is_err()
        {
            tracing::debug!("Pi already exited before shutdown");
        }
        timeout(SHUTDOWN_TIMEOUT * 3, &mut self.supervisor)
            .await
            .map_err(|_| protocol("Pi did not exit after closing stdin"))?
            .map_err(|error| protocol(format!("could not wait for Pi shutdown: {error}")))
    }

    async fn send(&self, record: Value) -> HarnessResult<()> {
        let mut bytes = serde_json::to_vec(&record)
            .map_err(|error| protocol(format!("could not encode Pi RPC command: {error}")))?;
        if bytes.len() >= MAX_FRAME_BYTES {
            return Err(protocol("Pi RPC command is too large"));
        }
        bytes.push(b'\n');
        self.input
            .as_ref()
            .ok_or(HarnessError::ChannelClosed)?
            .send(bytes)
            .await
            .map_err(|_| HarnessError::ChannelClosed)
    }

    async fn startup_exit(&mut self) -> HarnessError {
        match timeout(SHUTDOWN_TIMEOUT * 3, &mut self.supervisor).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return protocol(format!("could not wait for Pi startup exit: {error}"));
            }
            Err(_) => {
                return protocol("Pi did not exit after closing its RPC output during startup");
            }
        }
        while let Ok(event) = self.events.get_mut().try_recv() {
            if let PiRpcEvent::Exited {
                code,
                stderr,
                protocol_error,
            } = event
            {
                return protocol(protocol_error.unwrap_or_else(|| {
                    format!("Pi exited during RPC startup ({code:?}): {}", stderr.trim())
                }));
            }
        }
        HarnessError::ChannelClosed
    }
}

#[cfg(unix)]
fn terminate(child: &Child) -> std::io::Result<()> {
    let Some(process) = child.id() else {
        return Ok(());
    };
    let process = libc::pid_t::try_from(process).map_err(std::io::Error::other)?;
    // The supervisor still owns the unreaped child, so the OS cannot reuse its PID.
    if unsafe { libc::kill(-process, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(not(unix))]
fn terminate(_child: &Child) -> std::io::Result<()> {
    Ok(())
}

impl Drop for PiRpc {
    fn drop(&mut self) {
        self.writer.abort();
        if let Some(request) = self.shutdown_request.take()
            && request.send(()).is_err()
        {
            tracing::debug!("Pi already exited before shutdown");
        }
    }
}

async fn write_input(mut stdin: ChildStdin, mut outgoing: mpsc::Receiver<Vec<u8>>) {
    while let Some(frame) = outgoing.recv().await {
        if let Err(error) = stdin.write_all(&frame).await {
            tracing::debug!(%error, "could not write Pi RPC frame");
            break;
        }
    }
}

async fn read_output(
    mut reader: impl AsyncBufRead + Unpin,
    replies: &Replies,
    events: &mpsc::UnboundedSender<PiRpcEvent>,
) -> HarnessResult<()> {
    while let Some(record) = read_record(&mut reader).await? {
        let Some(kind) = record["type"].as_str() else {
            return Err(protocol("Pi RPC record has no type"));
        };
        if kind == "response" {
            let id = record["id"]
                .as_str()
                .ok_or_else(|| protocol("Pi RPC response has no correlation ID"))?
                .to_owned();
            let reply = serde_json::from_value(record)
                .map_err(|error| protocol(format!("invalid Pi RPC response: {error}")))?;
            if let Some(sender) = replies.lock().await.pending.remove(&id)
                && sender.send(reply).is_err()
            {
                tracing::debug!(%id, "Pi RPC caller released its response");
            }
        } else {
            events
                .send(PiRpcEvent::Record(record))
                .map_err(|_| HarnessError::ChannelClosed)?;
        }
    }
    Ok(())
}

async fn read_record(reader: &mut (impl AsyncBufRead + Unpin)) -> HarnessResult<Option<Value>> {
    let mut frame = Vec::new();
    let length = reader
        .take(MAX_FRAME_BYTES as u64)
        .read_until(b'\n', &mut frame)
        .await
        .map_err(|error| protocol(format!("could not read Pi RPC frame: {error}")))?;
    if length == 0 {
        return Ok(None);
    }
    let frame = frame
        .strip_suffix(b"\n")
        .ok_or_else(|| protocol("Pi RPC frame is incomplete or too large"))?;
    let frame = frame.strip_suffix(b"\r").unwrap_or(frame);
    serde_json::from_slice(frame)
        .map(Some)
        .map_err(|error| protocol(format!("invalid Pi RPC JSON: {error}")))
}

async fn drain_stderr(mut stderr: impl AsyncRead + Unpin, tail: Arc<Mutex<Vec<u8>>>) {
    let mut buffer = [0; 4096];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) => break,
            Ok(length) => {
                let mut tail = tail.lock().await;
                let excess = (tail.len() + length).saturating_sub(STDERR_TAIL_BYTES);
                tail.drain(..excess);
                tail.extend_from_slice(&buffer[..length]);
            }
            Err(error) => {
                tracing::debug!(%error, "could not read Pi stderr");
                break;
            }
        }
    }
}

fn protocol(message: impl Into<String>) -> HarnessError {
    HarnessError::Protocol(message.into())
}

#[cfg(test)]
mod tests {

    use super::*;

    #[tokio::test]
    async fn preserves_unicode_separators_and_routes_reversed_responses() -> HarnessResult<()> {
        let frames = concat!(
            "{\"type\":\"response\",\"id\":\"second\",\"command\":\"get_state\",\"success\":true,\"data\":2}\r\n",
            "{\"type\":\"message_update\",\"text\":\"one\u{2028}two\u{2029}three\"}\n",
            "{\"type\":\"response\",\"id\":\"first\",\"command\":\"get_state\",\"success\":true,\"data\":1}\n",
        );
        let replies = Replies::default();
        let (first, first_reply) = oneshot::channel();
        let (second, second_reply) = oneshot::channel();
        replies
            .lock()
            .await
            .pending
            .extend([("first".into(), first), ("second".into(), second)]);
        let (events, mut received) = mpsc::unbounded_channel();
        read_output(
            BufReader::with_capacity(2, frames.as_bytes()),
            &replies,
            &events,
        )
        .await?;
        assert_eq!(
            first_reply
                .await
                .map_err(|_| HarnessError::ChannelClosed)?
                .data,
            1
        );
        assert_eq!(
            second_reply
                .await
                .map_err(|_| HarnessError::ChannelClosed)?
                .data,
            2
        );
        assert!(
            matches!(received.try_recv(), Ok(PiRpcEvent::Record(record)) if record["text"] == "one\u{2028}two\u{2029}three")
        );
        Ok(())
    }

    #[tokio::test]
    async fn refuses_an_unterminated_record() {
        let mut reader = BufReader::new(b"{\"type\":\"agent_settled\"}".as_slice());
        assert!(read_record(&mut reader).await.is_err());
    }

    #[test]
    fn prompt_disposition_is_required_to_detect_a_handled_command() -> HarnessResult<()> {
        assert_eq!(
            PromptDisposition::from_reply(json!({"disposition": "handled"}))?,
            PromptDisposition::Handled
        );
        assert!(PromptDisposition::from_reply(Value::Null).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_disconnect_reports_process_diagnostics()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let spec = SpawnSpec {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf 'fixture startup failure\\n' >&2; exit 23".into(),
            ],
            cwd: directory.path().to_owned(),
            ..SpawnSpec::default()
        };
        let result = PiRpc::start(&spec).await;
        let error = match result {
            Ok(_) => return Err("Pi unexpectedly started".into()),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("fixture startup failure"), "{error}");
        assert!(error.contains("23"), "{error}");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_and_drop_stop_pi_descendants() -> Result<(), Box<dyn std::error::Error>> {
        for drop_transport in [false, true] {
            let directory = tempfile::tempdir()?;
            let script = directory.path().join("pi-child.sh");
            std::fs::write(
                &script,
                r#"
(sleep 1; printf survived > child-survived) &
IFS= read -r state || exit 3
state_id=$(printf '%s\n' "$state" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"type":"response","id":"%s","command":"get_state","success":true,"data":{"sessionId":"pi-fixture","sessionFile":null,"isStreaming":false,"isCompacting":false,"pendingMessageCount":0}}\n' "$state_id"
while IFS= read -r line; do :; done
"#,
            )?;
            let spec = SpawnSpec {
                program: "/bin/sh".into(),
                args: vec![script.to_string_lossy().into_owned()],
                cwd: directory.path().to_owned(),
                ..SpawnSpec::default()
            };
            let (rpc, _) = PiRpc::start(&spec).await?;
            if drop_transport {
                drop(rpc);
            } else {
                rpc.shutdown().await?;
            }
            tokio::time::sleep(Duration::from_millis(1500)).await;
            assert!(!directory.path().join("child-survived").exists());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pending_request_fails_when_output_closes_before_process_exit()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let script = directory.path().join("pi-disconnect.sh");
        std::fs::write(
            &script,
            r#"
IFS= read -r state || exit 3
state_id=$(printf '%s\n' "$state" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"type":"response","id":"%s","command":"get_state","success":true,"data":{"sessionId":"pi-fixture","sessionFile":null,"isStreaming":false,"isCompacting":false,"pendingMessageCount":0}}\n' "$state_id"
IFS= read -r request || exit 4
(sleep 1; printf survived > child-survived) >/dev/null 2>&1 &
exec 1>&-
exec sleep 30
"#,
        )?;
        let spec = SpawnSpec {
            program: "/bin/sh".into(),
            args: vec![script.to_string_lossy().into_owned()],
            cwd: directory.path().to_owned(),
            ..SpawnSpec::default()
        };
        let (rpc, _) = PiRpc::start(&spec).await?;
        let result = timeout(Duration::from_secs(1), rpc.request("get_state", Map::new())).await?;
        assert!(matches!(result, Err(HarnessError::ChannelClosed)));
        timeout(Duration::from_secs(1), rpc.shutdown()).await??;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!directory.path().join("child-survived").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn subprocess_routes_an_extension_dialog_and_keeps_stderr_off_the_wire()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let script = directory.path().join("pi-rpc-fixture.sh");
        std::fs::write(
            &script,
            r#"
[ "$1" = --mode ] && [ "$2" = rpc ] || exit 2
IFS= read -r state || exit 3
state_id=$(printf '%s\n' "$state" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '{"type":"response","id":"%s","command":"get_state","success":true,"data":{"sessionId":"pi-fixture","sessionFile":null,"isStreaming":false,"isCompacting":false,"pendingMessageCount":0}}\r\n' "$state_id"
IFS= read -r prompt || exit 4
case "$prompt" in *'"type":"prompt"'*) ;; *) exit 5;; esac
case "$prompt" in *'"message":"/fixture"'*) ;; *) exit 5;; esac
prompt_id=$(printf '%s\n' "$prompt" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
printf '%s\n' '{"type":"extension_ui_request","id":"dialog-1","method":"confirm","title":"Fixture","message":"Continue?"}'
IFS= read -r reply || exit 6
case "$reply" in *'"confirmed":true'*) ;; *) exit 7;; esac
case "$reply" in *'"id":"dialog-1"'*) ;; *) exit 7;; esac
case "$reply" in *'"type":"extension_ui_response"'*) ;; *) exit 7;; esac
printf '{"type":"response","id":"%s","command":"prompt","success":true,"data":{"disposition":"handled"}}\n' "$prompt_id"
printf '%s\n' '{"type":"diagnostic","text":"fixture stderr"}' >&2
exit 23
"#,
        )?;
        let spec = SpawnSpec {
            program: "/bin/sh".into(),
            args: vec![script.to_string_lossy().into_owned()],
            cwd: directory.path().to_owned(),
            ..SpawnSpec::default()
        };
        let (rpc, state) = PiRpc::start(&spec).await?;
        assert_eq!(state.session_id, "pi-fixture");
        let prompt = rpc.request(
            "prompt",
            Map::from_iter([("message".into(), json!("/fixture"))]),
        );
        let answer = async {
            let Some(PiRpcEvent::Record(request)) = rpc.next_event().await else {
                return Err(protocol("expected an extension dialog"));
            };
            assert_eq!(request["type"], "extension_ui_request");
            assert_eq!(request["method"], "confirm");
            let id = request["id"]
                .as_str()
                .ok_or_else(|| protocol("dialog lacked an ID"))?;
            rpc.reply_to_extension(id, Map::from_iter([("confirmed".into(), json!(true))]))
                .await
        };
        let (reply, ()) =
            timeout(PROBE_TIMEOUT, async { tokio::try_join!(prompt, answer) }).await??;
        assert_eq!(
            PromptDisposition::from_reply(reply)?,
            PromptDisposition::Handled
        );
        assert!(
            matches!(timeout(PROBE_TIMEOUT, rpc.next_event()).await?, Some(PiRpcEvent::Exited {
            code: Some(23), stderr, protocol_error: None,
        }) if stderr.contains("fixture stderr"))
        );
        rpc.shutdown().await?;
        Ok(())
    }

    #[cfg(feature = "live-pi")]
    #[tokio::test]
    async fn installed_pi_exchanges_extension_dialog_records()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::path::PathBuf;

        let executable = std::env::var("SHEPHERD_PI_TEST_EXECUTABLE")?;
        if !PathBuf::from(&executable).is_absolute() {
            return Err("set SHEPHERD_PI_TEST_EXECUTABLE to an absolute path".into());
        }
        let root = tempfile::tempdir()?;
        let path = root.path().join("dialog.ts");
        std::fs::write(
            &path,
            r#"
            export default function (pi) {
                pi.registerCommand("shepherd-rpc-fixture", {
                    description: "Local RPC probe",
                    handler: async (_, ctx) => {
                        const accepted = await ctx.ui.confirm("Fixture", "Does RPC answer by ID?");
                        if (!accepted) throw new Error("Fixture did not receive the approval");
                    },
                });
            }
        "#,
        )?;
        let spec = SpawnSpec {
            program: executable,
            args: vec![
                "--no-session".into(),
                "--no-extensions".into(),
                "--no-skills".into(),
                "--no-prompt-templates".into(),
                "--extension".into(),
                path.to_string_lossy().into_owned(),
            ],
            cwd: root.path().to_owned(),
            environment: [
                ("HOME".into(), root.path().to_string_lossy().into_owned()),
                (
                    "PI_CODING_AGENT_DIR".into(),
                    root.path().join("agent").to_string_lossy().into_owned(),
                ),
                ("PI_OFFLINE".into(), "1".into()),
                ("PI_SKIP_VERSION_CHECK".into(), "1".into()),
            ]
            .into(),
        };
        let (rpc, state) = PiRpc::start(&spec).await?;
        assert!(!state.is_streaming);
        let commands = rpc.request("get_commands", Map::new()).await?;
        assert!(commands["commands"].as_array().is_some_and(|commands| {
            commands.iter().any(|command| {
                command["name"] == "shepherd-rpc-fixture" && command["source"] == "extension"
            })
        }));
        let prompt = rpc.request(
            "prompt",
            Map::from_iter([("message".into(), json!("/shepherd-rpc-fixture"))]),
        );
        let answer = async {
            while let Some(event) = rpc.next_event().await {
                if let PiRpcEvent::Record(record) = event
                    && record["type"] == "extension_ui_request"
                    && record["method"] == "confirm"
                {
                    let id = record["id"]
                        .as_str()
                        .ok_or_else(|| protocol("Pi confirm lacked an ID"))?;
                    return rpc
                        .reply_to_extension(id, Map::from_iter([("confirmed".into(), json!(true))]))
                        .await;
                }
            }
            Err(HarnessError::ChannelClosed)
        };
        let (reply, ()) =
            timeout(PROBE_TIMEOUT, async { tokio::try_join!(prompt, answer) }).await??;
        if reply.is_null() {
            assert!(PromptDisposition::from_reply(reply).is_err());
            eprintln!("Pi answered the extension dialog but lacks prompt disposition");
        } else {
            assert_eq!(
                PromptDisposition::from_reply(reply)?,
                PromptDisposition::Handled
            );
        }
        rpc.shutdown().await?;
        Ok(())
    }
}
