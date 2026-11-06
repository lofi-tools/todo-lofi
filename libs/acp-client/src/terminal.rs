//! Terminal capability: runs the commands the agent asks for and streams their
//! output back to the UI.
//!
//! A host can supply interactive terminals. The registry owns the protocol
//! identifiers, exit waiters and bounded output for either launch path.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1::{
    CreateTerminalRequest, CreateTerminalResponse, KillTerminalResponse, ReleaseTerminalResponse,
    TerminalExitStatus, TerminalId, TerminalOutputResponse, WaitForTerminalExitResponse,
};
use async_trait::async_trait;
use futures::channel::oneshot;
use futures::io::AsyncReadExt as _;

use crate::{AcpEvent, fs::SessionRoots};

/// Default retained output per terminal when the agent does not ask for a limit.
const DEFAULT_OUTPUT_LIMIT: usize = 256 * 1024;

pub struct TerminalLaunch {
    pub id: String,
    pub cwd: PathBuf,
    pub command: String,
    pub args: Vec<String>,
    pub environment: BTreeMap<String, String>,
}

pub enum TerminalProcessEvent {
    Output(Vec<u8>),
    Exited(TerminalExitStatus),
}

#[async_trait]
pub trait TerminalBackend: Send + Sync {
    async fn spawn(
        &self,
        launch: TerminalLaunch,
        events: tokio::sync::mpsc::Sender<TerminalProcessEvent>,
    ) -> Result<(), String>;

    async fn stop(&self, id: &str) -> Result<(), String>;
}

/// Environment overrides that stop agent commands hanging behind a pager.
fn base_env() -> Vec<(String, String)> {
    vec![
        ("PAGER".to_string(), String::new()),
        ("GIT_PAGER".to_string(), "cat".to_string()),
    ]
}

struct TerminalEntry {
    output: String,
    truncated: bool,
    exit: Option<TerminalExitStatus>,
    waiters: Vec<oneshot::Sender<TerminalExitStatus>>,
    byte_limit: usize,
}

/// Shared state for every terminal started in a session.
#[derive(Clone)]
pub struct TerminalRegistry {
    inner: Arc<Mutex<HashMap<String, TerminalEntry>>>,
    roots: SessionRoots,
    events: tokio::sync::mpsc::WeakUnboundedSender<AcpEvent>,
    backend: Option<Arc<dyn TerminalBackend>>,
    closed: Arc<tokio::sync::Mutex<bool>>,
}

impl TerminalRegistry {
    pub fn new(
        roots: SessionRoots,
        events: &tokio::sync::mpsc::UnboundedSender<AcpEvent>,
        backend: Option<Arc<dyn TerminalBackend>>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            roots,
            events: events.downgrade(),
            backend,
            closed: Arc::new(tokio::sync::Mutex::new(false)),
        }
    }

    /// Start a command for the agent. Returns as soon as the process is up.
    pub async fn create(
        &self,
        request: CreateTerminalRequest,
    ) -> Result<CreateTerminalResponse, String> {
        let cwd = match &request.cwd {
            Some(cwd) => self
                .roots
                .resolve(cwd)
                .map_err(|error| format!("refusing terminal cwd: {error}"))?,
            None => self
                .roots
                .resolve(PathBuf::from(".").as_path())
                .map_err(|error| format!("no session directory: {error}"))?,
        };
        let terminal_id = TerminalId::new(format!("term-{}", next_terminal_number()));
        let key = terminal_id.0.to_string();
        let byte_limit = request
            .output_byte_limit
            .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX))
            .unwrap_or(DEFAULT_OUTPUT_LIMIT);
        if let Some(backend) = &self.backend {
            let closed = self.closed.lock().await;
            if *closed {
                return Err("the terminal connection is closed".into());
            }
            let title = request.command.clone();
            let launch = TerminalLaunch {
                id: key.clone(),
                cwd,
                command: default_shell(),
                args: vec!["-c".into(), shell_command(&request.command, &request.args)],
                environment: base_env()
                    .into_iter()
                    .chain(request.env.into_iter().map(|var| (var.name, var.value)))
                    .collect(),
            };
            let (events, receiver) = tokio::sync::mpsc::channel(32);
            backend.spawn(launch, events).await?;
            self.register(&key, byte_limit);
            self.publish(AcpEvent::TerminalStarted {
                session_id: request.session_id,
                terminal_id: key.clone(),
                title,
            });
            let registry = self.clone();
            tokio::spawn(async move { registry.pump_backend(key, receiver).await });
            return Ok(CreateTerminalResponse::new(terminal_id));
        }
        let shell = default_shell();
        let mut std_command = std::process::Command::new(&shell);
        std_command
            .arg("-c")
            .arg(shell_command(&request.command, &request.args))
            .current_dir(&cwd)
            .envs(base_env())
            .envs(
                request
                    .env
                    .iter()
                    .map(|var| (var.name.clone(), var.value.clone())),
            );
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            // Own process group so the whole tree can be killed at once.
            std_command.process_group(0);
        }

        // The stdio configuration has to be applied after the conversion: the
        // converted command does not inherit it, and without this the child
        // would write to the app's own stdout instead of a pipe.
        let mut command = async_process::Command::from(std_command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| format!("failed to start `{}`: {error}", request.command))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        self.register(&key, byte_limit);

        let registry = self.clone();
        let event_key = key.clone();
        tokio::spawn(async move {
            if let Some(stdout) = stdout {
                registry.pump(stdout, event_key.clone()).await;
            }
        });
        let registry = self.clone();
        let event_key = key.clone();
        tokio::spawn(async move {
            if let Some(stderr) = stderr {
                registry.pump(stderr, event_key.clone()).await;
            }
        });
        let registry = self.clone();
        tokio::spawn(async move {
            let status = child.status().await;
            let exit = match status {
                Ok(status) => {
                    TerminalExitStatus::new().exit_code(status.code().map(|code| code as u32))
                }
                Err(error) => {
                    tracing::warn!("terminal {key} could not be waited on: {error}");
                    TerminalExitStatus::new()
                }
            };
            registry.finish(&key, exit);
        });

        Ok(CreateTerminalResponse::new(terminal_id))
    }

    fn register(&self, key: &str, byte_limit: usize) {
        self.entries().insert(
            key.to_owned(),
            TerminalEntry {
                output: String::new(),
                truncated: false,
                exit: None,
                waiters: Vec::new(),
                byte_limit,
            },
        );
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, TerminalEntry>> {
        match self.inner.lock() {
            Ok(entries) => entries,
            Err(error) => {
                tracing::error!(%error, "ACP terminal registry lock was poisoned");
                error.into_inner()
            }
        }
    }

    pub fn output(&self, terminal_id: &TerminalId) -> TerminalOutputResponse {
        let guard = self.entries();
        match guard.get(terminal_id.0.as_ref()) {
            Some(entry) => TerminalOutputResponse::new(entry.output.clone(), entry.truncated)
                .exit_status(entry.exit.clone()),
            None => TerminalOutputResponse::new(String::new(), false),
        }
    }

    /// Resolve once the terminal exits, immediately when it already has.
    pub async fn wait_for_exit(
        &self,
        terminal_id: &TerminalId,
    ) -> Result<WaitForTerminalExitResponse, String> {
        let rx = {
            let mut guard = self.entries();
            let entry = guard
                .get_mut(terminal_id.0.as_ref())
                .ok_or_else(|| format!("unknown terminal {}", terminal_id.0))?;
            match &entry.exit {
                Some(exit) => return Ok(WaitForTerminalExitResponse::new(exit.clone())),
                None => {
                    let (tx, rx) = oneshot::channel();
                    entry.waiters.push(tx);
                    rx
                }
            }
        };
        match rx.await {
            Ok(exit) => Ok(WaitForTerminalExitResponse::new(exit)),
            Err(_) => Err("terminal was released before it exited".to_string()),
        }
    }

    pub async fn kill(&self, terminal_id: &TerminalId) -> Result<KillTerminalResponse, String> {
        let key = terminal_id.0.as_ref();
        if let Some(backend) = &self.backend {
            let _guard = self.closed.lock().await;
            if self.entries().contains_key(key) {
                backend.stop(key).await?;
            }
        } else {
            self.entries().remove(key);
        }
        Ok(KillTerminalResponse::new())
    }

    pub async fn release(
        &self,
        terminal_id: &TerminalId,
    ) -> Result<ReleaseTerminalResponse, String> {
        let key = terminal_id.0.as_ref();
        if let Some(backend) = &self.backend {
            let _guard = self.closed.lock().await;
            if self.entries().contains_key(key) {
                backend.stop(key).await?;
                self.entries().remove(key);
                self.publish(AcpEvent::TerminalReleased {
                    terminal_id: key.to_owned(),
                });
            }
        } else {
            self.entries().remove(key);
        }
        Ok(ReleaseTerminalResponse::new())
    }

    pub(crate) async fn shutdown(&self) {
        let Some(backend) = &self.backend else {
            return;
        };
        let mut closed = self.closed.lock().await;
        *closed = true;
        let terminal_ids: Vec<_> = self.entries().keys().cloned().collect();
        for id in terminal_ids {
            match backend.stop(&id).await {
                Ok(()) => {
                    self.entries().remove(&id);
                }
                Err(error) => {
                    tracing::warn!(terminal_id = id, %error, "could not stop ACP terminal during shutdown")
                }
            }
        }
    }

    async fn pump_backend(
        &self,
        key: String,
        mut events: tokio::sync::mpsc::Receiver<TerminalProcessEvent>,
    ) {
        let mut pending = Vec::new();
        let mut status = TerminalExitStatus::new();
        while let Some(event) = events.recv().await {
            match event {
                TerminalProcessEvent::Output(bytes) => {
                    pending.extend(bytes);
                    let incomplete = pending.utf8_chunks().last().map_or(0, |chunk| {
                        let invalid = chunk.invalid();
                        if std::str::from_utf8(invalid)
                            .is_err_and(|error| error.error_len().is_none())
                        {
                            invalid.len()
                        } else {
                            0
                        }
                    });
                    let tail = pending.split_off(pending.len() - incomplete);
                    if !pending.is_empty() {
                        self.push(&key, String::from_utf8_lossy(&pending).into_owned());
                    }
                    pending = tail;
                }
                TerminalProcessEvent::Exited(exit) => {
                    status = exit;
                    break;
                }
            }
        }
        if !pending.is_empty() {
            self.push(&key, String::from_utf8_lossy(&pending).into_owned());
        }
        self.finish(&key, status);
    }

    async fn pump(&self, mut reader: impl futures::AsyncRead + Unpin, key: String) {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => {
                    let chunk = String::from_utf8_lossy(&buffer[..read]).into_owned();
                    self.push(&key, chunk);
                }
                Err(error) => {
                    tracing::debug!("terminal {key} output ended: {error}");
                    break;
                }
            }
        }
    }

    fn push(&self, key: &str, chunk: String) {
        let truncated = {
            let mut guard = self.entries();
            let Some(entry) = guard.get_mut(key) else {
                return;
            };
            entry.output.push_str(&chunk);
            if entry.output.len() > entry.byte_limit {
                let overflow = entry.output.len() - entry.byte_limit;
                let cut = entry
                    .output
                    .char_indices()
                    .map(|(index, _)| index)
                    .find(|index| *index >= overflow)
                    .unwrap_or(entry.output.len());
                entry.output.drain(..cut);
                entry.truncated = true;
            }
            entry.truncated
        };
        self.publish(AcpEvent::TerminalOutput {
            terminal_id: key.to_string(),
            chunk,
            truncated,
        });
    }

    fn finish(&self, key: &str, exit: TerminalExitStatus) {
        let waiters = {
            let mut guard = self.entries();
            let Some(entry) = guard.get_mut(key) else {
                return;
            };
            entry.exit = Some(exit.clone());
            self.publish(AcpEvent::TerminalExited {
                terminal_id: key.to_string(),
                exit_code: exit.exit_code,
                signal: exit.signal.clone(),
            });
            std::mem::take(&mut entry.waiters)
        };
        for waiter in waiters {
            drop(waiter.send(exit.clone()));
        }
    }

    fn publish(&self, event: AcpEvent) {
        if let Some(events) = self.events.upgrade()
            && events.send(event).is_err()
        {
            tracing::debug!("ACP terminal listener closed");
        }
    }
}

fn next_terminal_number() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// The user's shell, preferring bash, falling back to `sh`.
fn default_shell() -> String {
    if let Some(shell) = std::env::var_os("SHELL") {
        let shell = shell.to_string_lossy().into_owned();
        if !shell.is_empty() && std::path::Path::new(&shell).exists() {
            return shell;
        }
    }
    if crate::agent::resolve_program("bash").is_some() {
        return "bash".to_string();
    }
    "/bin/sh".to_string()
}

/// Build the shell command string, quoting arguments that need it.
fn shell_command(command: &str, args: &[String]) -> String {
    let mut line = command.to_string();
    for arg in args {
        line.push_str(" '");
        line.push_str(&arg.replace('\'', "'\\''"));
        line.push('\'');
    }
    line
}
