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
    members: Vec<libc::pid_t>,
}

#[cfg(unix)]
impl TerminalProcessGroup {
    fn still_owned(&self) -> bool {
        self.group > 1
            && self.group != unsafe { libc::getpgrp() }
            && self.members.iter().any(|&process| unsafe {
                libc::getsid(process) == self.session && libc::getpgid(process) == self.group
            })
    }

    fn signal(&self, signal: libc::c_int) -> bool {
        if !self.still_owned() {
            return false;
        }
        if unsafe { libc::killpg(self.group, signal) } == 0 {
            return true;
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            tracing::warn!(%error, process_group = self.group, signal, "could not signal terminal process group");
        }
        false
    }

    fn finish_hangup(mut groups: Vec<Self>) {
        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        groups.retain(Self::still_owned);
        while !groups.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            groups.retain(Self::still_owned);
        }
        for group in groups {
            group.signal(libc::SIGKILL);
        }
    }
}

#[cfg(target_os = "macos")]
fn list_processes() -> std::io::Result<Vec<libc::pid_t>> {
    let estimate = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if estimate <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut capacity = estimate as usize + 64;
    loop {
        let bytes = libc::c_int::try_from(capacity * std::mem::size_of::<libc::pid_t>())
            .map_err(std::io::Error::other)?;
        let mut processes = vec![0; capacity];
        let count = unsafe { libc::proc_listallpids(processes.as_mut_ptr().cast(), bytes) };
        if count <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        if (count as usize) < capacity {
            processes.truncate(count as usize);
            return Ok(processes);
        }
        capacity *= 2;
    }
}

#[cfg(target_os = "linux")]
fn list_processes() -> std::io::Result<Vec<libc::pid_t>> {
    let mut processes = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        if let Some(process) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        {
            processes.push(process);
        }
    }
    Ok(processes)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn list_processes() -> std::io::Result<Vec<libc::pid_t>> {
    Ok(Vec::new())
}

#[cfg(unix)]
fn terminal_processes() -> Vec<libc::pid_t> {
    match list_processes() {
        Ok(processes) => processes,
        Err(error) => {
            tracing::warn!(%error, "could not enumerate background terminal jobs");
            Vec::new()
        }
    }
}

impl Session {
    #[cfg(unix)]
    fn process_groups(&self, processes: &[libc::pid_t]) -> Vec<TerminalProcessGroup> {
        let Some(session) = self.session_id else {
            return Vec::new();
        };
        let own_group = unsafe { libc::getpgrp() };
        let mut groups = BTreeMap::<libc::pid_t, Vec<libc::pid_t>>::new();
        // The shell and foreground leader are still available if the process
        // table could not be read. Other members can outlive their group leader.
        let fallback = [Some(session), self.master.process_group_leader()];
        for process in processes
            .iter()
            .copied()
            .chain(fallback.into_iter().flatten())
        {
            if process <= 1 || unsafe { libc::getsid(process) } != session {
                continue;
            }
            let group = unsafe { libc::getpgid(process) };
            if group > 1 && group != own_group {
                groups.entry(group).or_default().push(process);
            }
        }
        groups
            .into_iter()
            .map(|(group, members)| TerminalProcessGroup {
                group,
                session,
                members,
            })
            .collect()
    }

    fn hangup(&mut self, #[cfg(unix)] groups: &[TerminalProcessGroup]) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            let mut shell_signalled = false;
            for group in groups {
                if group.signal(libc::SIGHUP) && group.group == group.session {
                    shell_signalled = true;
                }
            }
            if shell_signalled
                || self
                    .session_id
                    .is_some_and(|process| unsafe { libc::getsid(process) != process })
            {
                return Ok(());
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
        if sessions.is_empty() {
            return;
        }
        #[cfg(unix)]
        let processes = terminal_processes();
        #[cfg(unix)]
        let mut all_groups = Vec::new();
        for (terminal_id, mut session) in sessions {
            #[cfg(unix)]
            let groups = session.process_groups(&processes);
            if let Err(error) = session.hangup(
                #[cfg(unix)]
                &groups,
            ) {
                tracing::warn!(%terminal_id, %error, "could not stop terminal process");
            }
            #[cfg(unix)]
            all_groups.extend(groups);
        }
        #[cfg(unix)]
        TerminalProcessGroup::finish_hangup(all_groups);
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
        #[cfg(unix)]
        let groups = session.process_groups(&terminal_processes());
        session
            .hangup(
                #[cfg(unix)]
                &groups,
            )
            .map_err(|error| TerminalError::Backend(format!("kill failed: {error}")))?;
        session.closing = true;
        drop(sessions);
        #[cfg(unix)]
        TerminalProcessGroup::finish_hangup(groups);
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

    #[cfg(unix)]
    #[tokio::test]
    async fn closing_a_live_terminal_stops_a_job_that_ignores_hangup()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut engine = PtyTerminals::new();
        let (sender, mut receiver) = mpsc::channel(8);
        let terminal_id = engine.spawn(
            TerminalSpec {
                id: TerminalId::new("test-terminal"),
                columns: 80,
                rows: 24,
                cwd: std::env::temp_dir(),
                command: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "trap '' HUP; printf ready; sleep 10".to_string(),
                ],
                environment: BTreeMap::new(),
            },
            sender,
        )?;
        let ready = tokio::time::timeout(Duration::from_secs(3), receiver.recv()).await?;
        assert!(matches!(ready, Some(TerminalEvent::Output(bytes)) if bytes == b"ready"));

        engine.close(&terminal_id)?;
        assert!(
            engine.write(&terminal_id, b"x".to_vec()).is_err(),
            "a closed terminal keeps no writer"
        );
        let stopped = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(event) = receiver.recv().await {
                if matches!(event, TerminalEvent::Exited { .. }) {
                    return true;
                }
            }
            false
        })
        .await?;
        assert!(stopped, "the terminal should report its exit");
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn closing_a_terminal_stops_a_separate_background_process_group()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut engine = PtyTerminals::new();
        let (sender, mut receiver) = mpsc::channel(8);
        let terminal_id = engine.spawn(
            TerminalSpec {
                id: TerminalId::new("background-terminal"),
                columns: 80,
                rows: 24,
                cwd: std::env::temp_dir(),
                command: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    r#"python3 -u -c 'import os, signal, time; os.setpgid(0, 0); signal.signal(signal.SIGHUP, signal.SIG_IGN); print(f"background:{os.getpid()}"); time.sleep(10)' & wait"#.into(),
                ],
                environment: BTreeMap::new(),
            },
            sender,
        )?;
        let process = tokio::time::timeout(Duration::from_secs(3), async {
            let mut output = String::new();
            while let Some(TerminalEvent::Output(bytes)) = receiver.recv().await {
                output.push_str(&String::from_utf8_lossy(&bytes));
                if let Some(process) = output.lines().find_map(|line| {
                    line.strip_prefix("background:")?
                        .parse::<libc::pid_t>()
                        .ok()
                }) {
                    return Some(process);
                }
            }
            None
        })
        .await?
        .ok_or("the background job did not start")?;
        let session = unsafe { libc::getsid(process) };
        let group = unsafe { libc::getpgid(process) };
        assert!(
            session > 1 && group > 1 && session != group,
            "session {session}, group {group}"
        );

        engine.close(&terminal_id)?;
        let stopped = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let status = std::process::Command::new("ps")
                    .args(["-p", &process.to_string(), "-o", "stat="])
                    .output()?;
                let state = String::from_utf8(status.stdout)?;
                if state.trim().is_empty() || state.trim_start().starts_with('Z') {
                    return Ok::<(), Box<dyn std::error::Error>>(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        if !matches!(&stopped, Ok(Ok(())))
            && unsafe { libc::getsid(process) == session && libc::getpgid(process) == group }
            && unsafe { libc::kill(process, libc::SIGKILL) } == -1
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        stopped??;
        Ok(())
    }
}
