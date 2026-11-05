//! Test-only directories and repositories. Every helper owns a temporary path,
//! so a test can never write into the real application data directory.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use agent_harness::AgentHarness;
use agent_harness::fake_harness::{FakeHarness, FakeTurn};

use crate::git::{GitError, GitRepository, GitService, run_git};
use crate::harness::HarnessFactory;
use crate::{Command, CommandResult, Core, CoreError};

#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("git error: {0}")]
    Git(#[from] GitError),
    #[error("fixture path must be relative: {path}")]
    AbsolutePath { path: String },
}

pub struct TestDirectory {
    directory: tempfile::TempDir,
}

impl TestDirectory {
    pub fn new() -> Result<Self, FixtureError> {
        Ok(Self {
            directory: tempfile::tempdir()?,
        })
    }

    pub fn path(&self) -> &Path {
        self.directory.path()
    }

    pub fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.path().join(path)
    }
}

pub struct TestRepository {
    directory: TestDirectory,
    repository: GitRepository,
}

impl TestRepository {
    pub fn new() -> Result<Self, FixtureError> {
        let directory = TestDirectory::new()?;
        run_git(directory.path(), &["init", "-b", "main"])?;
        run_git(directory.path(), &["config", "user.name", "Shepherd Test"])?;
        run_git(
            directory.path(),
            &["config", "user.email", "shepherd@example.com"],
        )?;
        std::fs::write(directory.join("README.md"), "initial\n")?;
        run_git(directory.path(), &["add", "README.md"])?;
        run_git(directory.path(), &["commit", "-m", "initial"])?;
        let repository = GitService::discover(directory.path())?;
        Ok(Self {
            directory,
            repository,
        })
    }

    pub fn path(&self) -> &Path {
        self.directory.path()
    }

    pub fn repository(&self) -> &GitRepository {
        &self.repository
    }

    pub fn write_file(&self, path: impl AsRef<Path>, contents: &str) -> Result<(), FixtureError> {
        let relative = path.as_ref();
        if relative.is_absolute() {
            return Err(FixtureError::AbsolutePath {
                path: relative.display().to_string(),
            });
        }
        let path = self.directory.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)?;
        Ok(())
    }
}

/// A core wrapper that records every command, result and emitted event. If a
/// test panics while the scenario is alive, `Drop` prints the trace so the
/// failure includes the order that produced it.
pub struct TestCore {
    core: Core,
    trace: Vec<String>,
}

impl TestCore {
    pub fn new() -> Self {
        Self::with_core(Core::new())
    }

    pub fn with_core(core: Core) -> Self {
        Self {
            core,
            trace: Vec::new(),
        }
    }

    pub fn core(&self) -> &Core {
        &self.core
    }

    pub fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    pub async fn execute(&mut self, command: Command) -> Result<CommandResult, CoreError> {
        self.trace.push(format!("command: {command:?}"));
        let result = self.core.execute(command).await;
        match &result {
            Ok(result) => self.trace.push(format!("result: {result:?}")),
            Err(error) => self.trace.push(format!("error: {error:?}")),
        }
        for event in self.core.drain_events() {
            self.trace.push(format!("event: {event:?}"));
        }
        result
    }

    pub fn trace(&self) -> String {
        self.trace
            .iter()
            .enumerate()
            .map(|(index, step)| format!("{index}: {step}\n"))
            .collect()
    }
}

impl Default for TestCore {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TestCore {
    fn drop(&mut self) {
        if std::thread::panicking() && !self.trace.is_empty() {
            eprintln!("Shepherd scenario trace:\n{}", self.trace());
        }
    }
}

/// A queue of fake scripts. Each created harness consumes the next script, so
/// a test can model a fresh adapter process after a restart.
pub struct FakeHarnessFactory {
    scripts: Mutex<VecDeque<Vec<FakeTurn>>>,
    resume: bool,
}

impl FakeHarnessFactory {
    pub fn new(scripts: Vec<Vec<FakeTurn>>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into()),
            resume: true,
        }
    }

    /// A factory whose harnesses advertise no resume, for the fallback path.
    pub fn refusing_resume(scripts: Vec<Vec<FakeTurn>>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into()),
            resume: false,
        }
    }
}

impl HarnessFactory for FakeHarnessFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        let script = match self.scripts.lock() {
            Ok(mut scripts) => scripts.pop_front().unwrap_or_default(),
            Err(poisoned) => {
                let mut scripts = poisoned.into_inner();
                scripts.pop_front().unwrap_or_default()
            }
        };
        let harness = FakeHarness::new(script);
        if self.resume {
            Ok(Box::new(harness))
        } else {
            Ok(Box::new(harness.without_resume()))
        }
    }
}

pub struct TestDatabase {
    directory: TestDirectory,
}

impl TestDatabase {
    pub fn new() -> Result<Self, FixtureError> {
        Ok(Self {
            directory: TestDirectory::new()?,
        })
    }

    pub fn path(&self) -> &Path {
        self.directory.path()
    }

    pub fn uri(&self) -> String {
        format!("turso:{}", self.directory.join("shepherd.db").display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_scenario_trace_records_commands_results_and_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let mut scenario = TestCore::new();

        scenario
            .execute(Command::AddProject {
                path: directory.path().to_path_buf(),
            })
            .await?;

        let trace = scenario.trace();
        assert!(trace.contains("command: AddProject"));
        assert!(trace.contains("result: ProjectList"));
        assert!(trace.contains("event: ProjectListChanged"));
        assert!(trace.contains("event: TaskListChanged"));
        Ok(())
    }
}
