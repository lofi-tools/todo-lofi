//! Durable per-project agent state: the ACP session id used to resume a
//! conversation after a restart, and the project's approval preference.
//!
//! The app's database is in-memory, so this lives in the same config directory
//! as the Todoist token (`$MY_TODO_CONFIG_DIR`, else `~/.config/my-todo`).
//! Entries are keyed by project path, which survives tag renames.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const STORE_VERSION: u32 = 1;
const FILE_NAME: &str = "acp-sessions.json";

/// One project's persisted agent state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSession {
    /// Canonical project directory; the key.
    pub project_path: String,
    /// Tag id at the time of writing, for display only.
    #[serde(default)]
    pub tag_id: u64,
    /// Agent id the session belongs to (`opencode`).
    #[serde(default)]
    pub agent_id: String,
    /// ACP session id, used with `session/load`.
    pub session_id: String,
    /// Unix seconds of the last update.
    #[serde(default)]
    pub updated_at: i64,
    /// Whether tool calls are auto-approved for this project.
    #[serde(default)]
    pub auto_approve: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    sessions: Vec<StoredSession>,
}

/// JSON-file-backed store. Every operation reads and rewrites the file, which
/// keeps the format obvious at the cost of a few small writes.
#[derive(Clone, Debug)]
pub struct SessionStore {
    path: PathBuf,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new(default_path())
    }
}

impl SessionStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The project's entry, if one was stored.
    pub fn get(&self, project_path: &str) -> Option<StoredSession> {
        self.load()
            .sessions
            .into_iter()
            .find(|session| session.project_path == project_path)
    }

    /// Insert or replace a project's entry.
    pub fn put(&self, session: StoredSession) -> anyhow::Result<()> {
        let mut file = self.load();
        match file
            .sessions
            .iter_mut()
            .find(|existing| existing.project_path == session.project_path)
        {
            Some(existing) => *existing = session,
            None => file.sessions.push(session),
        }
        self.save(&file)
    }

    /// Update only the approval preference, creating an entry when needed.
    pub fn set_auto_approve(&self, project_path: &str, auto_approve: bool) -> anyhow::Result<()> {
        let mut file = self.load();
        match file
            .sessions
            .iter_mut()
            .find(|existing| existing.project_path == project_path)
        {
            Some(existing) => existing.auto_approve = auto_approve,
            None => file.sessions.push(StoredSession {
                project_path: project_path.to_string(),
                auto_approve,
                ..Default::default()
            }),
        }
        self.save(&file)
    }

    /// Drop a project's entry (used when its session is torn down).
    pub fn remove(&self, project_path: &str) -> anyhow::Result<()> {
        let mut file = self.load();
        file.sessions
            .retain(|session| session.project_path != project_path);
        self.save(&file)
    }

    fn load(&self) -> StoreFile {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return StoreFile::default();
        };
        match serde_json::from_str(&raw) {
            Ok(file) => file,
            Err(error) => {
                // A corrupt store must not stop the agent panel from working;
                // starting from empty loses only resume ids.
                tracing::warn!("ignoring unreadable {}: {error}", self.path.display());
                StoreFile::default()
            }
        }
    }

    fn save(&self, file: &StoreFile) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", parent.display()))?;
        }
        let file = StoreFile {
            version: STORE_VERSION,
            sessions: file.sessions.clone(),
        };
        let raw = serde_json::to_string_pretty(&file)
            .map_err(|e| anyhow::anyhow!("failed to serialize sessions: {e}"))?;
        std::fs::write(&self.path, raw)
            .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", self.path.display()))
    }
}

/// `$MY_TODO_CONFIG_DIR/acp-sessions.json`, else `~/.config/my-todo/...`.
pub fn default_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("MY_TODO_CONFIG_DIR") {
        return PathBuf::from(dir).join(FILE_NAME);
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("my-todo").join(FILE_NAME)
}

/// Unix seconds now, for `updated_at`.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}
