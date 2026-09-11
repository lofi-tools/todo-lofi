//! Durable per-project agent state: the ACP session id used to resume a
//! conversation after a restart, and the project's tool approval policy.
//!
//! The app's database is in-memory, so this lives in the same config directory
//! as the Todoist token (`$MY_TODO_CONFIG_DIR`, else `~/.config/my-todo`).
//! Entries are keyed by project path, which survives tag renames.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::permissions::{PermissionRule, ToolPermissions};

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
    /// The project's tool approval policy, in Zed's `tool_permissions` shape.
    #[serde(default)]
    pub tool_permissions: ToolPermissions,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    sessions: Vec<StoredSession>,
}

/// Previous file shape, for the one-shot migration in `load`: the old single
/// `auto_approve` toggle becomes a global allow when no policy exists yet.
#[derive(Debug, Deserialize)]
struct LegacyStoredSession {
    #[serde(default)]
    auto_approve: bool,
    #[serde(flatten)]
    session: StoredSession,
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

    /// Update only the approval policy, creating an entry when needed.
    pub fn set_tool_permissions(
        &self,
        project_path: &str,
        permissions: ToolPermissions,
    ) -> anyhow::Result<()> {
        let mut file = self.load();
        match file
            .sessions
            .iter_mut()
            .find(|existing| existing.project_path == project_path)
        {
            Some(existing) => existing.tool_permissions = permissions,
            None => file.sessions.push(StoredSession {
                project_path: project_path.to_string(),
                tool_permissions: permissions,
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
        // Parse through the legacy shape so a stored `auto_approve: true`
        // with no policy yet migrates to a global allow instead of being
        // silently dropped. Unknown keys are ignored, so this also reads
        // current files.
        #[derive(Deserialize)]
        struct LegacyFile {
            #[serde(default)]
            version: u32,
            #[serde(default)]
            sessions: Vec<LegacyStoredSession>,
        }
        match serde_json::from_str::<LegacyFile>(&raw) {
            Ok(legacy) => {
                let sessions = legacy
                    .sessions
                    .into_iter()
                    .map(|entry| {
                        let mut session = entry.session;
                        if entry.auto_approve
                            && session.tool_permissions == ToolPermissions::default()
                        {
                            session.tool_permissions.default = PermissionRule::Allow;
                        }
                        session
                    })
                    .collect();
                StoreFile {
                    version: legacy.version,
                    sessions,
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_auto_approve_migrates_to_a_global_allow() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("acp-sessions.json");
        std::fs::write(
            &path,
            r#"{"version":1,"sessions":[
                {"project_path":"/x","tag_id":1,"agent_id":"opencode","session_id":"s","updated_at":0,"auto_approve":true},
                {"project_path":"/y","tag_id":2,"agent_id":"opencode","session_id":"t","updated_at":0}
            ]}"#,
        )
        .expect("write legacy file");
        let store = SessionStore::new(&path);
        let migrated = store.get("/x").expect("entry");
        assert_eq!(migrated.tool_permissions.default, PermissionRule::Allow);
        let untouched = store.get("/y").expect("entry");
        assert_eq!(untouched.tool_permissions, ToolPermissions::default());
        // Policies round-trip through the new shape.
        store
            .set_tool_permissions(
                "/z",
                ToolPermissions {
                    default: PermissionRule::Deny,
                    ..Default::default()
                },
            )
            .expect("save");
        let reread = SessionStore::new(&path).get("/z").expect("entry");
        assert_eq!(reread.tool_permissions.default, PermissionRule::Deny);
    }
}
