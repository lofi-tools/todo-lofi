//! The boundary the core uses for PTYs. No rendering or terminal workspace is
//! implemented here; S1 supplies a `portable-pty` implementation and S4 deepens
//! it behind this trait.

use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TerminalId(String);

impl TerminalId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct TerminalSpec {
    pub cwd: PathBuf,
    pub command: String,
    pub args: Vec<String>,
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("terminal service error: {0}")]
    Backend(String),
}

pub trait TerminalService: Send {
    fn spawn(&mut self, spec: TerminalSpec) -> Result<TerminalId, TerminalError>;
    fn write(&mut self, terminal_id: &TerminalId, bytes: &[u8]) -> Result<(), TerminalError>;
    fn resize(
        &mut self,
        terminal_id: &TerminalId,
        columns: u16,
        rows: u16,
    ) -> Result<(), TerminalError>;
    fn close(&mut self, terminal_id: &TerminalId) -> Result<(), TerminalError>;
}
