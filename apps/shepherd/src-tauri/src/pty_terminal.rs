//! The `portable-pty` implementation of the core's terminal boundary.
//!
//! One reader thread and one waiter thread per PTY; both report through the same
//! channel, so the shell sees output and the exit in order.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::Mutex;

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use shepherd_core::terminal::{
    TerminalError, TerminalEvent, TerminalId, TerminalService, TerminalSpec,
};
use tokio::sync::mpsc::UnboundedSender;

struct Session {
    writer: Box<dyn Write + Send>,
    /// The waiting thread owns the child, so closing goes through a cloned killer.
    killer: Box<dyn ChildKiller + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
}

#[derive(Default)]
pub struct PtyTerminals {
    sessions: Mutex<BTreeMap<String, Session>>,
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

    fn sessions(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, Session>>, TerminalError> {
        self.sessions
            .lock()
            .map_err(|_| TerminalError::Backend("the terminal table is poisoned".to_string()))
    }
}

impl TerminalService for PtyTerminals {
    fn spawn(
        &mut self,
        spec: TerminalSpec,
        events: UnboundedSender<TerminalEvent>,
    ) -> Result<TerminalId, TerminalError> {
        let system = native_pty_system();
        let pair = system
            .openpty(Self::size(80, 24))
            .map_err(|error| TerminalError::Backend(format!("openpty failed: {error}")))?;

        let mut command = CommandBuilder::new(&spec.command);
        command.args(&spec.args);
        command.cwd(&spec.cwd);
        for (key, value) in &spec.environment {
            command.env(key, value);
        }

        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| TerminalError::Backend(format!("spawn failed: {error}")))?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| TerminalError::Backend(format!("reader failed: {error}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| TerminalError::Backend(format!("writer failed: {error}")))?;

        let terminal_id = TerminalId::new(uuid::Uuid::new_v4().to_string());

        let output_events = events.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if output_events
                            .send(TerminalEvent::Output(buffer[..read].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::debug!(%error, "terminal reader stopped");
                        break;
                    }
                }
            }
        });

        let killer = child.clone_killer();
        std::thread::spawn(move || {
            let exit_code = match child.wait() {
                Ok(status) => Some(status.exit_code() as i32),
                Err(error) => {
                    tracing::debug!(%error, "waiting for the terminal child failed");
                    None
                }
            };
            let _ = events.send(TerminalEvent::Exited { exit_code });
        });

        self.sessions()?.insert(
            terminal_id.as_str().to_string(),
            Session {
                writer,
                killer,
                master: pair.master,
            },
        );

        Ok(terminal_id)
    }

    fn write(&mut self, terminal_id: &TerminalId, bytes: &[u8]) -> Result<(), TerminalError> {
        let mut sessions = self.sessions()?;
        let session = sessions.get_mut(terminal_id.as_str()).ok_or_else(|| {
            TerminalError::Backend(format!("no terminal {}", terminal_id.as_str()))
        })?;
        session
            .writer
            .write_all(bytes)
            .and_then(|()| session.writer.flush())
            .map_err(|error| TerminalError::Backend(format!("write failed: {error}")))
    }

    fn resize(
        &mut self,
        terminal_id: &TerminalId,
        columns: u16,
        rows: u16,
    ) -> Result<(), TerminalError> {
        let sessions = self.sessions()?;
        let session = sessions.get(terminal_id.as_str()).ok_or_else(|| {
            TerminalError::Backend(format!("no terminal {}", terminal_id.as_str()))
        })?;
        session
            .master
            .resize(Self::size(columns, rows))
            .map_err(|error| TerminalError::Backend(format!("resize failed: {error}")))
    }

    fn close(&mut self, terminal_id: &TerminalId) -> Result<(), TerminalError> {
        let mut sessions = self.sessions()?;
        let Some(mut session) = sessions.remove(terminal_id.as_str()) else {
            return Ok(());
        };
        session
            .killer
            .kill()
            .map_err(|error| TerminalError::Backend(format!("kill failed: {error}")))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_pty_reports_its_output_and_exit_code() {
        let mut engine = PtyTerminals::new();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let terminal_id = engine
            .spawn(
                TerminalSpec {
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

    #[test]
    fn closing_a_live_terminal_removes_it() {
        let mut engine = PtyTerminals::new();
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let terminal_id = engine
            .spawn(
                TerminalSpec {
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
            engine.write(&terminal_id, b"x").is_err(),
            "a closed terminal keeps no writer"
        );
    }
}
