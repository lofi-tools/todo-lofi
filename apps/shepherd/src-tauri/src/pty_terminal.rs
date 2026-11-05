//! The `portable-pty` implementation of the core's terminal boundary.
//!
//! One reader thread and one waiter thread per PTY; both report through the same
//! channel, so the shell sees output and the exit in order.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use shepherd_core::terminal::{
    TerminalError, TerminalEvent, TerminalId, TerminalService, TerminalSpec, TerminalWrite,
    validate_size,
};
use tokio::sync::{mpsc, oneshot};

const MAX_WRITE_BYTES: usize = 64 * 1024;

struct WriteRequest {
    bytes: Vec<u8>,
    completion: Option<oneshot::Sender<Result<(), TerminalError>>>,
}

struct Session {
    writer: mpsc::Sender<WriteRequest>,
    closing: bool,
    /// The waiting thread owns the child, so closing goes through a cloned killer.
    killer: Box<dyn ChildKiller + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    #[cfg(unix)]
    session_id: Option<libc::pid_t>,
}

#[cfg(unix)]
struct TerminalProcessGroup {
    group: libc::pid_t,
    session: libc::pid_t,
}

#[cfg(unix)]
impl TerminalProcessGroup {
    fn still_owned(&self) -> bool {
        self.group > 1
            && unsafe {
                self.group != libc::getpgrp()
                    && libc::getsid(self.group) == self.session
                    && libc::getpgid(self.group) == self.group
            }
    }
}

impl Session {
    #[cfg(unix)]
    fn process_groups(&self) -> impl Iterator<Item = TerminalProcessGroup> {
        self.session_id.into_iter().flat_map(|session| {
            let foreground = self
                .master
                .process_group_leader()
                .filter(|group| *group != session);
            [Some(session), foreground]
                .into_iter()
                .flatten()
                .map(move |group| TerminalProcessGroup { group, session })
                .filter(TerminalProcessGroup::still_owned)
        })
    }

    fn hangup(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        if let Some(group) = self
            .master
            .process_group_leader()
            .filter(|group| *group > 1)
        {
            // The master identifies the foreground group on this PTY. Exclude
            // Shepherd's own group before signaling a negative process ID.
            let own_group = unsafe { libc::getpgrp() };
            if group != own_group && unsafe { libc::killpg(group, libc::SIGHUP) } == -1 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    tracing::warn!(%error, group, "could not hang up terminal foreground group");
                }
            }
        }
        match self.killer.kill() {
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            result => result,
        }
    }
}

#[derive(Default)]
pub struct PtyTerminals {
    sessions: Arc<Mutex<BTreeMap<String, Session>>>,
}

impl PtyTerminals {
    pub fn new() -> Self {
        Self::default()
    }

    fn size(columns: u16, rows: u16) -> PtySize {
        PtySize {
            rows,
            cols: columns,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    pub fn terminate_all(&mut self) {
        let sessions = match self.sessions.lock() {
            Ok(mut sessions) => std::mem::take(&mut *sessions),
            Err(error) => {
                tracing::error!(%error, "could not drain terminal sessions");
                std::mem::take(&mut *error.into_inner())
            }
        };
        #[cfg(unix)]
        let mut groups: Vec<_> = sessions
            .values()
            .flat_map(Session::process_groups)
            .collect();
        for (terminal_id, mut session) in sessions {
            if let Err(error) = session.hangup() {
                tracing::warn!(%terminal_id, %error, "could not stop terminal process");
            }
        }
        #[cfg(unix)]
        {
            let deadline = std::time::Instant::now() + Duration::from_millis(200);
            groups.retain(TerminalProcessGroup::still_owned);
            while !groups.is_empty() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
                groups.retain(TerminalProcessGroup::still_owned);
            }
            for group in groups {
                // The PTY may already have detached from the surviving job. Check
                // its leader still belongs to the original session before signaling.
                if group.still_owned() && unsafe { libc::killpg(group.group, libc::SIGKILL) } == -1
                {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        tracing::warn!(%error, process_group = group.group, "could not stop terminal process group");
                    }
                }
            }
        }
    }

    pub fn write_reply(
        &self,
        terminal_id: &TerminalId,
        bytes: Vec<u8>,
    ) -> Result<(), TerminalError> {
        self.queue_write(
            terminal_id,
            WriteRequest {
                bytes,
                completion: None,
            },
        )
    }

    fn queue_write(
        &self,
        terminal_id: &TerminalId,
        request: WriteRequest,
    ) -> Result<(), TerminalError> {
        if request.bytes.len() > MAX_WRITE_BYTES {
            return Err(TerminalError::Backend(format!(
                "terminal input exceeds {MAX_WRITE_BYTES} bytes per request"
            )));
        }
        let sessions = self.sessions()?;
        let session = sessions
            .get(terminal_id.as_str())
            .filter(|session| !session.closing)
            .ok_or_else(|| {
                TerminalError::Backend(format!("no live terminal {}", terminal_id.as_str()))
            })?;
        session
            .writer
            .try_send(request)
            .map_err(|error| TerminalError::Backend(format!("terminal input queue: {error}")))
    }

    fn sessions(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, Session>>, TerminalError> {
        self.sessions
            .lock()
            .map_err(|_| TerminalError::Backend("the terminal table is poisoned".to_string()))
    }
}

impl Drop for PtyTerminals {
    fn drop(&mut self) {
        self.terminate_all();
    }
}

impl TerminalService for PtyTerminals {
    fn spawn(
        &mut self,
        spec: TerminalSpec,
        events: mpsc::Sender<TerminalEvent>,
    ) -> Result<TerminalId, TerminalError> {
        validate_size(spec.columns, spec.rows)?;
        let terminal_id = spec.id;
        if self.sessions()?.contains_key(terminal_id.as_str()) {
            return Err(TerminalError::Backend("terminal is already running".into()));
        }
        let system = native_pty_system();
        let pair = system
            .openpty(Self::size(spec.columns, spec.rows))
            .map_err(|error| TerminalError::Backend(format!("openpty failed: {error}")))?;

        let mut command = CommandBuilder::new(&spec.command);
        command.args(&spec.args);
        command.cwd(&spec.cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("SHEPHERD_TERMINAL_ID", terminal_id.as_str());
        for (key, value) in &spec.environment {
            command.env(key, value);
        }

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| TerminalError::Backend(format!("reader failed: {error}")))?;
        let mut writer = pair
            .master
            .take_writer()
            .map_err(|error| TerminalError::Backend(format!("writer failed: {error}")))?;

        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| TerminalError::Backend(format!("spawn failed: {error}")))?;
        drop(pair.slave);

        let (reader_finished, reader_status) = std::sync::mpsc::sync_channel(1);
        let output_events = events.clone();
        let sending = Arc::new(AtomicBool::new(false));
        let reader_sending = Arc::clone(&sending);
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        reader_sending.store(true, Ordering::Release);
                        let sent = output_events
                            .blocking_send(TerminalEvent::Output(buffer[..read].to_vec()));
                        reader_sending.store(false, Ordering::Release);
                        if sent.is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        tracing::debug!(%error, "terminal reader stopped");
                        break;
                    }
                }
            }
            if reader_finished.send(()).is_err() {
                tracing::debug!("terminal waiter stopped");
            }
        });

        let (input, mut writes) = mpsc::channel::<WriteRequest>(32);
        std::thread::spawn(move || {
            while let Some(request) = writes.blocking_recv() {
                let result = writer
                    .write_all(&request.bytes)
                    .and_then(|()| writer.flush())
                    .map_err(|error| TerminalError::Backend(format!("write failed: {error}")));
                match request.completion {
                    Some(completion) => {
                        if let Err(Err(error)) = completion.send(result) {
                            tracing::debug!(%error, "terminal input requester disconnected");
                        }
                    }
                    None => {
                        if let Err(error) = result {
                            tracing::debug!(%error, "could not write terminal reply");
                        }
                    }
                }
            }
        });

        let killer = child.clone_killer();
        #[cfg(unix)]
        let session_id = child
            .process_id()
            .and_then(|pid| libc::pid_t::try_from(pid).ok());
        self.sessions()?.insert(
            terminal_id.as_str().to_string(),
            Session {
                writer: input,
                closing: false,
                killer,
                master: pair.master,
                #[cfg(unix)]
                session_id,
            },
        );

        let sessions = Arc::clone(&self.sessions);
        let id = terminal_id.as_str().to_string();
        std::thread::spawn(move || {
            let exit_code = match child.wait() {
                Ok(status) => Some(status.exit_code() as i32),
                Err(error) => {
                    tracing::debug!(%error, "waiting for the terminal child failed");
                    None
                }
            };
            match sessions.lock() {
                Ok(mut sessions) => {
                    sessions.remove(&id);
                }
                Err(error) => tracing::error!(%error, "could not release terminal descriptors"),
            }
            loop {
                match reader_status.recv_timeout(Duration::from_millis(150)) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        // A full queue still has output ahead of the exit. Only time out
                        // a reader waiting for descendants that inherited the slave.
                        if !sending.load(Ordering::Acquire) || events.is_closed() {
                            tracing::debug!("terminal reader did not reach EOF before exit");
                            break;
                        }
                    }
                }
            }
            if events
                .blocking_send(TerminalEvent::Exited { exit_code })
                .is_err()
            {
                tracing::debug!("terminal event receiver closed");
            }
        });

        Ok(terminal_id)
    }

    fn write(
        &mut self,
        terminal_id: &TerminalId,
        bytes: Vec<u8>,
    ) -> Result<TerminalWrite, TerminalError> {
        let (completion, result) = oneshot::channel();
        self.queue_write(
            terminal_id,
            WriteRequest {
                bytes,
                completion: Some(completion),
            },
        )?;
        Ok(result)
    }

    fn resize(
        &mut self,
        terminal_id: &TerminalId,
        columns: u16,
        rows: u16,
    ) -> Result<(), TerminalError> {
        validate_size(columns, rows)?;
        let sessions = self.sessions()?;
        let session = sessions
            .get(terminal_id.as_str())
            .filter(|session| !session.closing)
            .ok_or_else(|| {
                TerminalError::Backend(format!("no live terminal {}", terminal_id.as_str()))
            })?;
        session
            .master
            .resize(Self::size(columns, rows))
            .map_err(|error| TerminalError::Backend(format!("resize failed: {error}")))
    }

    fn close(&mut self, terminal_id: &TerminalId) -> Result<(), TerminalError> {
        let mut sessions = self.sessions()?;
        // The waiter owns removal so a failed signal can be retried.
        let Some(session) = sessions
            .get_mut(terminal_id.as_str())
            .filter(|session| !session.closing)
        else {
            return Ok(());
        };
        session
            .hangup()
            .map_err(|error| TerminalError::Backend(format!("kill failed: {error}")))?;
        session.closing = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn a_pty_reports_its_output_and_exit_code() {
        let mut engine = PtyTerminals::new();
        let (sender, mut receiver) = mpsc::channel(8);
        engine
            .spawn(
                TerminalSpec {
                    id: TerminalId::new("test-terminal"),
                    columns: 80,
                    rows: 24,
                    cwd: std::env::temp_dir(),
                    command: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), "printf hello".to_string()],
                    environment: BTreeMap::new(),
                },
                sender,
            )
            .expect("the PTY should spawn");

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut output = String::new();
        let mut exit_code = None;
        while Instant::now() < deadline && exit_code.is_none() {
            match receiver.blocking_recv() {
                Some(TerminalEvent::Output(bytes)) => {
                    output.push_str(&String::from_utf8_lossy(&bytes));
                }
                Some(TerminalEvent::Exited { exit_code: code }) => exit_code = Some(code),
                None => break,
            }
        }

        assert!(output.contains("hello"), "output was {output:?}");
        assert_eq!(exit_code, Some(Some(0)));
    }

    #[tokio::test]
    async fn a_slow_reader_gets_all_output_before_exit() -> Result<(), Box<dyn std::error::Error>> {
        let mut engine = PtyTerminals::new();
        let (sender, mut receiver) = mpsc::channel(1);
        engine.spawn(
            TerminalSpec {
                id: TerminalId::new("burst-terminal"),
                columns: 80,
                rows: 24,
                cwd: std::env::temp_dir(),
                command: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "head -c 1048576 /dev/zero; printf tail".to_string(),
                ],
                environment: BTreeMap::new(),
            },
            sender,
        )?;

        tokio::time::sleep(Duration::from_millis(200)).await;
        let (output, status) = tokio::time::timeout(Duration::from_secs(10), async {
            let mut output = Vec::new();
            while let Some(event) = receiver.recv().await {
                match event {
                    TerminalEvent::Output(bytes) => output.extend(bytes),
                    TerminalEvent::Exited { exit_code } => return (output, exit_code),
                }
            }
            (output, None)
        })
        .await?;
        assert_eq!(output.len(), 1_048_580);
        assert!(output.ends_with(b"tail"));
        assert_eq!(status, Some(0));
        Ok(())
    }

    #[test]
    fn closing_a_live_terminal_removes_it() {
        let mut engine = PtyTerminals::new();
        let (sender, _receiver) = mpsc::channel(8);
        let terminal_id = engine
            .spawn(
                TerminalSpec {
                    id: TerminalId::new("test-terminal"),
                    columns: 80,
                    rows: 24,
                    cwd: std::env::temp_dir(),
                    command: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), "sleep 30".to_string()],
                    environment: BTreeMap::new(),
                },
                sender,
            )
            .expect("the PTY should spawn");

        engine.close(&terminal_id).expect("close should succeed");
        assert!(
            engine.write(&terminal_id, b"x".to_vec()).is_err(),
            "a closed terminal keeps no writer"
        );
    }
}
