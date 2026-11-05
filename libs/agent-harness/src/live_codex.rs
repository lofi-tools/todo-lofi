use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;

use crate::SpawnSpec;
use crate::codex::{CodexHarness, CodexInstall, detect};

const MODEL: &str = "gpt-6.1-terra";
const EFFORT: &str = "low";
const WORKTREE: &str = "__SHEPHERD_TEST_WORKTREE__";

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub(crate) enum WireEntry {
    Request(Value),
    Response(Value),
    Exit(String),
}

#[derive(Clone)]
pub(crate) struct WireCapture {
    entries: Arc<Mutex<Vec<WireEntry>>>,
    cwd: String,
    record_exit: bool,
}

impl WireCapture {
    pub(crate) fn set_cwd(&mut self, cwd: &Path) {
        self.cwd = cwd.to_string_lossy().into_owned();
    }

    pub(crate) fn push(&self, mut entry: WireEntry) {
        match &mut entry {
            WireEntry::Request(value) | WireEntry::Response(value) => normalize(value, &self.cwd),
            WireEntry::Exit(_) if !self.record_exit => return,
            WireEntry::Exit(stderr) => *stderr = stderr.replace(&self.cwd, WORKTREE),
        }
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(entry);
    }
}

fn normalize(value: &mut Value, cwd: &str) {
    match value {
        Value::String(value) => *value = value.replace(cwd, WORKTREE),
        Value::Array(values) => values.iter_mut().for_each(|value| normalize(value, cwd)),
        Value::Object(values) => values.values_mut().for_each(|value| normalize(value, cwd)),
        _ => {}
    }
}

#[derive(Serialize, Deserialize)]
struct CachedRun {
    model: String,
    effort: String,
    version: Option<String>,
    sessions: BTreeMap<String, Vec<WireEntry>>,
    note: Option<String>,
}

type Captures = BTreeMap<String, Arc<Mutex<Vec<WireEntry>>>>;

#[derive(Clone)]
pub struct LiveCodexRun {
    cache_path: PathBuf,
    cached: Option<Arc<CachedRun>>,
    install: Option<CodexInstall>,
    captures: Arc<Mutex<Captures>>,
    _cache_lock: Option<Arc<fs::File>>,
}

impl LiveCodexRun {
    fn cache_path(scenario: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        if scenario.is_empty()
            || !scenario
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("invalid live Codex scenario name".into());
        }
        Ok(workspace().join(format!(
            ".cache/shepherd-live/codex/terra-low-v1/{scenario}.json"
        )))
    }

    pub fn load_replay(scenario: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let cache_path = Self::cache_path(scenario)?;
        Self::cached(&cache_path)?
            .ok_or_else(|| format!("missing cached Codex run at {}", cache_path.display()).into())
    }

    pub async fn load(scenario: &str) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let cache_path = Self::cache_path(scenario)?;
        if let Some(replay) = Self::cached(&cache_path)? {
            return Ok(Some(replay));
        }
        let parent = cache_path
            .parent()
            .ok_or("live cache has no parent directory")?;
        fs::create_dir_all(parent)?;
        let lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(cache_path.with_extension("lock"))?;
        let lock = tokio::task::spawn_blocking(move || -> io::Result<fs::File> {
            lock.lock()?;
            Ok(lock)
        })
        .await??;
        if let Some(replay) = Self::cached(&cache_path)? {
            return Ok(Some(replay));
        }

        let cwd = std::env::current_dir()?;
        let detection = detect(None, &cwd).await;
        let Some(install) = detection.install.clone() else {
            eprintln!("skipping the live run: {}", detection.summary());
            return Ok(None);
        };
        let mut login = Command::new(&install.program);
        without_api_credentials(&mut login);
        let status = login.args(["login", "status"]).output().await?;
        let chatgpt = [&status.stdout, &status.stderr].iter().any(|output| {
            String::from_utf8_lossy(output)
                .to_ascii_lowercase()
                .contains("logged in using chatgpt")
        });
        if !status.status.success() || !chatgpt {
            return Err("live Codex tests require a CLI logged in through ChatGPT".into());
        }
        eprintln!(
            "recording {} at {} effort in {}",
            MODEL,
            EFFORT,
            cache_path.display()
        );
        Ok(Some(Self {
            cache_path,
            cached: None,
            install: Some(install),
            captures: Arc::default(),
            _cache_lock: Some(Arc::new(lock)),
        }))
    }

    fn cached(cache_path: &Path) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        match fs::File::open(cache_path) {
            Ok(file) => {
                let cached: CachedRun = serde_json::from_reader(file)?;
                if cached.model != MODEL || cached.effort != EFFORT {
                    return Err(
                        "cached Codex model or effort differs from the live-test settings".into(),
                    );
                }
                eprintln!("replaying Codex responses from {}", cache_path.display());
                Ok(Some(Self {
                    cache_path: cache_path.to_path_buf(),
                    cached: Some(Arc::new(cached)),
                    install: None,
                    captures: Arc::default(),
                    _cache_lock: None,
                }))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn harness(&self, session: &str, record_exit: bool) -> CodexHarness {
        let mut harness = CodexHarness::new();
        let version = self
            .install
            .as_ref()
            .and_then(|install| install.probe.version.as_ref())
            .or_else(|| {
                self.cached
                    .as_ref()
                    .and_then(|cache| cache.version.as_ref())
            });
        if let Some(version) = version {
            harness = harness.with_version(version.clone());
        }
        if self.cached.is_none() {
            let entries = self
                .captures
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(session.to_owned())
                .or_default()
                .clone();
            harness = harness.with_wire_capture(WireCapture {
                entries,
                cwd: String::new(),
                record_exit,
            });
        }
        harness
    }

    pub fn spec(&self, cwd: PathBuf, session: &str, exit_gate: Option<&Path>) -> SpawnSpec {
        if let Some(install) = &self.install {
            SpawnSpec {
                program: install.program.to_string_lossy().into_owned(),
                args: live_args(),
                cwd,
                ..SpawnSpec::default()
            }
        } else {
            let script = workspace().join("scripts/replay-codex-live.py");
            let mut args = vec![
                script.to_string_lossy().into_owned(),
                self.cache_path.to_string_lossy().into_owned(),
                session.to_owned(),
                cwd.to_string_lossy().into_owned(),
            ];
            if let Some(gate) = exit_gate {
                args.push(gate.to_string_lossy().into_owned());
            }
            SpawnSpec {
                program: "python3".into(),
                args,
                cwd,
                ..SpawnSpec::default()
            }
        }
    }

    pub fn is_replay(&self) -> bool {
        self.cached.is_some()
    }

    pub fn cached_note(&self) -> Option<&str> {
        self.cached
            .as_ref()
            .and_then(|cached| cached.note.as_deref())
    }

    pub fn finish(&self, note: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
        if self.cached.is_some() {
            return Ok(());
        }
        let sessions = self
            .captures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(name, entries)| {
                let entries = entries
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (name.clone(), entries.clone())
            })
            .collect::<BTreeMap<_, _>>();
        if sessions.is_empty() || sessions.values().any(|entries| entries.is_empty()) {
            return Err("the live test did not capture its Codex sessions".into());
        }
        let cached = CachedRun {
            model: MODEL.into(),
            effort: EFFORT.into(),
            version: self
                .install
                .as_ref()
                .and_then(|install| install.probe.version.clone()),
            sessions,
            note,
        };
        let parent = self
            .cache_path
            .parent()
            .ok_or("live cache has no parent directory")?;
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(temporary.as_file_mut(), &cached)?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&self.cache_path) {
            Ok(_) => Ok(()),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error.error.into()),
        }
    }
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn live_args() -> Vec<String> {
    [
        "-c".into(),
        format!("model=\"{MODEL}\""),
        "-c".into(),
        format!("model_reasoning_effort=\"{EFFORT}\""),
        "-c".into(),
        "model_provider=\"openai\"".into(),
        "-c".into(),
        "forced_login_method=\"chatgpt\"".into(),
        "app-server".into(),
    ]
    .into()
}

pub(crate) fn without_api_credentials(command: &mut Command) {
    for name in ["OPENAI_API_KEY", "CODEX_API_KEY", "OPENAI_BASE_URL"] {
        command.env_remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentHarness, PromptInput};
    use std::time::Duration;

    #[tokio::test]
    async fn records_and_replays_codex_responses_across_worktrees()
    -> Result<(), Box<dyn std::error::Error>> {
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        let cache_path = first.path().join("fixture.json");
        let run = LiveCodexRun {
            cache_path: cache_path.clone(),
            cached: None,
            install: None,
            captures: Arc::default(),
            _cache_lock: None,
        };
        let stub = r#"
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":1,"result":{}}\n' ;;
    *'"method":"thread/start"'*) printf '{"id":2,"result":{"thread":{"id":"thr_test"}}}\n' ;;
    *'"method":"turn/start"'*)
      printf '{"id":3,"result":{"turn":{"id":"turn_test"}}}\n'
      printf '{"method":"item/agentMessage/delta","params":{"itemId":"message","delta":"fixture reply"}}\n'
      printf '{"method":"turn/completed","params":{"turn":{"id":"turn_test","status":"completed"}}}\n'
      ;;
  esac
done
"#;
        let spec = SpawnSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), stub.into()],
            cwd: first.path().to_path_buf(),
            ..SpawnSpec::default()
        };
        check_turn(run.harness("initial", false), spec).await?;
        run.finish(Some("recorded change\n".into()))?;

        let replay = LiveCodexRun::cached(&cache_path)?.ok_or("the responses were not cached")?;
        assert_eq!(replay.cached_note(), Some("recorded change\n"));
        check_turn(
            replay.harness("initial", false),
            replay.spec(second.path().to_path_buf(), "initial", None),
        )
        .await?;
        Ok(())
    }

    async fn check_turn(
        mut harness: CodexHarness,
        spec: SpawnSpec,
    ) -> Result<(), Box<dyn std::error::Error>> {
        harness.start(spec).await?;
        let session = harness.open_session(None).await?;
        assert_eq!(session.as_str(), "thr_test");
        harness
            .prompt(PromptInput {
                text: "fixture prompt".into(),
            })
            .await?;
        let mut answer = String::new();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), harness.events().recv())
                .await?
                .ok_or("adapter events ended")?;
            match event {
                crate::HarnessEvent::Delta { text, .. } => answer.push_str(&text),
                crate::HarnessEvent::TurnFinished { reason } => {
                    assert_eq!(reason, crate::TurnFinishReason::Completed);
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(answer, "fixture reply");
        harness.shutdown().await?;
        Ok(())
    }
}
