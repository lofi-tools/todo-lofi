use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use crate::{
    DeltaKind, HarnessError, HarnessEvent, HarnessResult, ItemId, ToolCallId, ToolStatus,
    settings::AgentSettings,
};

const MAX_METADATA_BYTES: u64 = 16 * 1024;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_MESSAGE_ITEMS: usize = 256;
const MAX_TRANSCRIPT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreebuffToolSnapshot {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub status: ToolStatus,
    pub output: Option<String>,
}

#[derive(Default)]
pub struct FreebuffToolReconciler {
    tools: BTreeMap<String, FreebuffToolSnapshot>,
    total_bytes: usize,
}

impl FreebuffToolReconciler {
    pub fn update(
        &mut self,
        snapshot: FreebuffToolSnapshot,
    ) -> HarnessResult<Option<HarnessEvent>> {
        if snapshot.id.is_empty()
            || snapshot.id.len() > 256
            || snapshot.title.is_empty()
            || snapshot.title.len() > 16 * 1024
            || snapshot.kind.is_empty()
            || snapshot.kind.len() > 256
            || snapshot
                .output
                .as_ref()
                .is_some_and(|output| output.len() > 64 * 1024)
        {
            return Err(HarnessError::Protocol(
                "invalid Freebuff tool snapshot".into(),
            ));
        }
        let previous = self.tools.get(&snapshot.id);
        if previous.is_none() && self.tools.len() >= MAX_MESSAGE_ITEMS {
            return Err(HarnessError::Protocol(
                "too many Freebuff tool calls in a turn".into(),
            ));
        }
        if let Some(previous) = previous {
            if previous == &snapshot {
                return Ok(None);
            }
            if previous.title != snapshot.title
                || previous.kind != snapshot.kind
                || matches!(
                    previous.status,
                    ToolStatus::Completed | ToolStatus::Failed | ToolStatus::Stopped
                ) && previous.status != snapshot.status
                || previous.status == ToolStatus::Running && snapshot.status == ToolStatus::Pending
            {
                return Err(HarnessError::Protocol(
                    "conflicting Freebuff tool snapshot".into(),
                ));
            }
        }
        let size = |tool: &FreebuffToolSnapshot| {
            tool.id.len()
                + tool.title.len()
                + tool.kind.len()
                + tool.output.as_ref().map_or(0, String::len)
        };
        let total_bytes = self
            .total_bytes
            .checked_sub(previous.map_or(0, size))
            .and_then(|total| total.checked_add(size(&snapshot)))
            .filter(|total| *total <= MAX_TRANSCRIPT_BYTES)
            .ok_or_else(|| {
                HarnessError::Protocol("Freebuff tool activity exceeds the supported size".into())
            })?;
        let event = HarnessEvent::ToolCall {
            tool_call_id: ToolCallId::new(&snapshot.id),
            title: snapshot.title.clone(),
            kind: snapshot.kind.clone(),
            status: snapshot.status,
            output: snapshot.output.clone(),
            diffs: Vec::new(),
            terminal_id: None,
            stream_permit: None,
        };
        self.tools.insert(snapshot.id.clone(), snapshot);
        self.total_bytes = total_bytes;
        Ok(Some(event))
    }

    pub fn clear(&mut self) {
        self.tools.clear();
        self.total_bytes = 0;
    }
}

pub struct FreebuffToolPublisher {
    reconciler: FreebuffToolReconciler,
    sender: tokio::sync::mpsc::UnboundedSender<HarnessEvent>,
    capacity: Arc<tokio::sync::Semaphore>,
}

impl FreebuffToolPublisher {
    pub fn new(
        sender: tokio::sync::mpsc::UnboundedSender<HarnessEvent>,
        capacity: usize,
    ) -> HarnessResult<Self> {
        if capacity == 0 || capacity > 256 {
            return Err(HarnessError::Protocol(
                "invalid Freebuff event capacity".into(),
            ));
        }
        Ok(Self::with_shared_capacity(
            sender,
            Arc::new(tokio::sync::Semaphore::new(capacity)),
        ))
    }

    fn with_shared_capacity(
        sender: tokio::sync::mpsc::UnboundedSender<HarnessEvent>,
        capacity: Arc<tokio::sync::Semaphore>,
    ) -> Self {
        Self {
            reconciler: FreebuffToolReconciler::default(),
            sender,
            capacity,
        }
    }

    pub async fn update(&mut self, snapshot: FreebuffToolSnapshot) -> HarnessResult<()> {
        let permit = tokio::select! {
            _ = self.sender.closed() => return Err(HarnessError::TransportClosed("Freebuff event receiver closed".into())),
            permit = self.capacity.clone().acquire_owned() => permit.map_err(|_| HarnessError::TransportClosed("Freebuff event capacity closed".into()))?,
        };
        let Some(mut event) = self.reconciler.update(snapshot)? else {
            return Ok(());
        };
        if let HarnessEvent::ToolCall { stream_permit, .. } = &mut event {
            *stream_permit = Some(Arc::new(permit));
        } else {
            return Err(HarnessError::Protocol(
                "unexpected Freebuff tool event".into(),
            ));
        }
        self.sender
            .send(event)
            .map_err(|_| HarnessError::TransportClosed("Freebuff event receiver closed".into()))?;
        Ok(())
    }

    pub fn clear(&mut self) {
        self.reconciler.clear();
    }
}

#[derive(Default)]
pub struct FreebuffTextReconciler {
    messages: BTreeMap<String, String>,
    total_bytes: usize,
}

impl FreebuffTextReconciler {
    pub fn update(&mut self, item_id: &str, text: &str) -> HarnessResult<Option<HarnessEvent>> {
        if item_id.is_empty() || item_id.len() > 256 {
            return Err(HarnessError::Protocol(
                "invalid Freebuff message identity".into(),
            ));
        }
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(HarnessError::Protocol(
                "Freebuff message exceeds the supported size".into(),
            ));
        }
        let previous = self.messages.get(item_id);
        if previous.is_none() && self.messages.len() >= MAX_MESSAGE_ITEMS {
            return Err(HarnessError::Protocol(
                "too many Freebuff messages in a turn".into(),
            ));
        }
        let previous_size = previous.map_or(0, String::len);
        let total_bytes = self
            .total_bytes
            .checked_sub(previous_size)
            .and_then(|size| size.checked_add(text.len()))
            .filter(|size| *size <= MAX_TRANSCRIPT_BYTES)
            .ok_or_else(|| {
                HarnessError::Protocol("Freebuff turn text exceeds the supported size".into())
            })?;
        let event = match previous {
            Some(previous) if previous == text => return Ok(None),
            Some(previous) => match text.strip_prefix(previous.as_str()) {
                Some(suffix) => HarnessEvent::Delta {
                    item_id: ItemId::new(item_id),
                    kind: DeltaKind::Message,
                    text: suffix.to_owned(),
                    stream_permit: None,
                },
                None => HarnessEvent::TextReplaced {
                    item_id: ItemId::new(item_id),
                    kind: DeltaKind::Message,
                    text: text.to_owned(),
                    stream_permit: None,
                },
            },
            None => HarnessEvent::Delta {
                item_id: ItemId::new(item_id),
                kind: DeltaKind::Message,
                text: text.to_owned(),
                stream_permit: None,
            },
        };
        self.messages.insert(item_id.to_owned(), text.to_owned());
        self.total_bytes = total_bytes;
        Ok(Some(event))
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.total_bytes = 0;
    }
}

pub struct FreebuffTextPublisher {
    reconciler: FreebuffTextReconciler,
    sender: tokio::sync::mpsc::UnboundedSender<HarnessEvent>,
    capacity: Arc<tokio::sync::Semaphore>,
}

impl FreebuffTextPublisher {
    pub fn new(
        sender: tokio::sync::mpsc::UnboundedSender<HarnessEvent>,
        capacity: usize,
    ) -> HarnessResult<Self> {
        if capacity == 0 || capacity > 256 {
            return Err(HarnessError::Protocol(
                "invalid Freebuff event capacity".into(),
            ));
        }
        Ok(Self::with_shared_capacity(
            sender,
            Arc::new(tokio::sync::Semaphore::new(capacity)),
        ))
    }

    fn with_shared_capacity(
        sender: tokio::sync::mpsc::UnboundedSender<HarnessEvent>,
        capacity: Arc<tokio::sync::Semaphore>,
    ) -> Self {
        Self {
            reconciler: FreebuffTextReconciler::default(),
            sender,
            capacity,
        }
    }

    pub async fn update(&mut self, item_id: &str, text: &str) -> HarnessResult<()> {
        let permit = tokio::select! {
            _ = self.sender.closed() => return Err(HarnessError::TransportClosed("Freebuff event receiver closed".into())),
            permit = self.capacity.clone().acquire_owned() => permit.map_err(|_| HarnessError::TransportClosed("Freebuff event capacity closed".into()))?,
        };
        let Some(mut event) = self.reconciler.update(item_id, text)? else {
            return Ok(());
        };
        match &mut event {
            HarnessEvent::Delta { stream_permit, .. }
            | HarnessEvent::TextReplaced { stream_permit, .. } => {
                *stream_permit = Some(Arc::new(permit));
            }
            _ => {
                return Err(HarnessError::Protocol(
                    "unexpected Freebuff text event".into(),
                ));
            }
        }
        self.sender
            .send(event)
            .map_err(|_| HarnessError::TransportClosed("Freebuff event receiver closed".into()))?;
        Ok(())
    }

    pub fn clear(&mut self) {
        self.reconciler.clear();
    }
}

pub struct FreebuffEventPublisher {
    text: FreebuffTextPublisher,
    tools: FreebuffToolPublisher,
}

impl FreebuffEventPublisher {
    pub fn new(
        sender: tokio::sync::mpsc::UnboundedSender<HarnessEvent>,
        capacity: usize,
    ) -> HarnessResult<Self> {
        if capacity == 0 || capacity > 256 {
            return Err(HarnessError::Protocol(
                "invalid Freebuff event capacity".into(),
            ));
        }
        let capacity = Arc::new(tokio::sync::Semaphore::new(capacity));
        Ok(Self {
            text: FreebuffTextPublisher::with_shared_capacity(sender.clone(), capacity.clone()),
            tools: FreebuffToolPublisher::with_shared_capacity(sender, capacity),
        })
    }

    pub async fn text(&mut self, item_id: &str, text: &str) -> HarnessResult<()> {
        self.text.update(item_id, text).await
    }

    pub async fn tool(&mut self, snapshot: FreebuffToolSnapshot) -> HarnessResult<()> {
        self.tools.update(snapshot).await
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.tools.clear();
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct FreebuffRuntimeMetadata {
    pub version: String,
    pub target: String,
}

pub fn read_runtime_metadata(directory: &Path) -> HarnessResult<Option<FreebuffRuntimeMetadata>> {
    let path = directory.join("freebuff-metadata.json");
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(HarnessError::Protocol(format!(
                "could not open Freebuff runtime metadata at {}: {error}",
                path.display()
            )));
        }
    };
    let mut bytes = Vec::new();
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            HarnessError::Protocol(format!(
                "could not read Freebuff runtime metadata at {}: {error}",
                path.display()
            ))
        })?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(HarnessError::Protocol(format!(
            "Freebuff runtime metadata at {} exceeds {MAX_METADATA_BYTES} bytes",
            path.display()
        )));
    }
    let metadata: FreebuffRuntimeMetadata = serde_json::from_slice(&bytes).map_err(|error| {
        HarnessError::Protocol(format!(
            "invalid Freebuff runtime metadata at {}: {error}",
            path.display()
        ))
    })?;
    if metadata.version.trim().is_empty() || metadata.target.trim().is_empty() {
        return Err(HarnessError::Protocol(format!(
            "Freebuff runtime metadata at {} has an empty version or target",
            path.display()
        )));
    }
    Ok(Some(metadata))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreebuffInstallSource {
    Configured,
    Path,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FreebuffInstall {
    pub executable: PathBuf,
    pub source: FreebuffInstallSource,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FreebuffAttempt {
    pub executable: PathBuf,
    pub source: FreebuffInstallSource,
    pub reason: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct FreebuffDiscovery {
    pub install: Option<FreebuffInstall>,
    pub attempts: Vec<FreebuffAttempt>,
}

impl FreebuffDiscovery {
    pub fn summary(&self) -> String {
        if let Some(install) = &self.install {
            return format!(
                "Freebuff executable found at {}",
                install.executable.display()
            );
        }
        if self.attempts.is_empty() {
            return "Freebuff executable not found; configure its path or add it to PATH".into();
        }
        let attempts = self
            .attempts
            .iter()
            .map(|attempt| format!("{} ({})", attempt.executable.display(), attempt.reason))
            .collect::<Vec<_>>()
            .join(", ");
        format!("Freebuff executable not found; tried {attempts}")
    }
}

pub fn discover(configured: Option<&Path>, path: &OsStr) -> FreebuffDiscovery {
    let mut discovery = FreebuffDiscovery::default();
    if let Some(executable) = configured {
        if !executable.is_absolute() {
            discovery.attempts.push(FreebuffAttempt {
                executable: executable.to_path_buf(),
                source: FreebuffInstallSource::Configured,
                reason: "configured executable must be an absolute path".into(),
            });
        } else if inspect(
            executable,
            FreebuffInstallSource::Configured,
            &mut discovery,
        ) {
            return discovery;
        }
    }
    for directory in std::env::split_paths(path) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        let executable = directory.join(if cfg!(windows) {
            "freebuff.exe"
        } else {
            "freebuff"
        });
        let executable = if executable.is_absolute() {
            executable
        } else {
            match std::env::current_dir() {
                Ok(current_directory) => current_directory.join(executable),
                Err(error) => {
                    discovery.attempts.push(FreebuffAttempt {
                        executable,
                        source: FreebuffInstallSource::Path,
                        reason: format!("could not resolve relative PATH entry: {error}"),
                    });
                    continue;
                }
            }
        };
        if inspect(&executable, FreebuffInstallSource::Path, &mut discovery) {
            return discovery;
        }
    }
    discovery
}

pub fn discover_from_settings(
    settings: &AgentSettings,
    path: &OsStr,
) -> HarnessResult<FreebuffDiscovery> {
    let executable = settings.executable("agents.freebuff.executable")?;
    Ok(discover(executable.as_deref(), path))
}

fn inspect(path: &Path, source: FreebuffInstallSource, discovery: &mut FreebuffDiscovery) -> bool {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && source == FreebuffInstallSource::Path =>
        {
            return false;
        }
        Err(error) => {
            discovery.attempts.push(FreebuffAttempt {
                executable: path.to_path_buf(),
                source,
                reason: format!("could not inspect executable: {error}"),
            });
            return false;
        }
    };
    let reason = if !metadata.is_file() {
        Some("not a regular file")
    } else if !executable_permissions(&metadata) {
        Some("not executable")
    } else {
        None
    };
    if let Some(reason) = reason {
        discovery.attempts.push(FreebuffAttempt {
            executable: path.to_path_buf(),
            source,
            reason: reason.into(),
        });
        return false;
    }
    discovery.install = Some(FreebuffInstall {
        executable: path.to_path_buf(),
        source,
    });
    true
}

#[cfg(unix)]
fn executable_permissions(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable_permissions(_metadata: &std::fs::Metadata) -> bool {
    true
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn tool_redraws_update_one_call_and_reject_conflicting_states()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut reconciler = FreebuffToolReconciler::default();
        let mut snapshot = FreebuffToolSnapshot {
            id: "tool-1".into(),
            title: "Read file".into(),
            kind: "read".into(),
            status: ToolStatus::Running,
            output: None,
        };
        assert!(matches!(
            reconciler.update(snapshot.clone())?,
            Some(HarnessEvent::ToolCall {
                status: ToolStatus::Running,
                ..
            })
        ));
        assert!(reconciler.update(snapshot.clone())?.is_none());
        snapshot.output = Some("result".into());
        snapshot.status = ToolStatus::Completed;
        assert!(
            matches!(reconciler.update(snapshot.clone())?, Some(HarnessEvent::ToolCall { status: ToolStatus::Completed, output: Some(output), .. }) if output == "result")
        );
        snapshot.status = ToolStatus::Running;
        assert!(reconciler.update(snapshot.clone()).is_err());
        snapshot.status = ToolStatus::Completed;
        assert!(reconciler.update(snapshot)?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn tool_publisher_waits_for_consumption_and_stops_on_disconnect()
    -> Result<(), Box<dyn std::error::Error>> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut publisher = FreebuffToolPublisher::new(sender, 1)?;
        let mut snapshot = FreebuffToolSnapshot {
            id: "tool-1".into(),
            title: "Read file".into(),
            kind: "read".into(),
            status: ToolStatus::Running,
            output: None,
        };
        publisher.update(snapshot.clone()).await?;
        snapshot.status = ToolStatus::Completed;
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(30),
                publisher.update(snapshot.clone())
            )
            .await
            .is_err()
        );
        let first = receiver.recv().await.ok_or("missing tool update")?;
        drop(first);
        publisher.update(snapshot.clone()).await?;
        assert!(matches!(
            receiver.recv().await,
            Some(HarnessEvent::ToolCall {
                status: ToolStatus::Completed,
                ..
            })
        ));
        drop(receiver);
        assert!(publisher.update(snapshot).await.is_err());
        Ok(())
    }

    #[test]
    fn reconciles_streamed_snapshots_and_redraws_without_duplicates()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut reconciler = FreebuffTextReconciler::default();
        let mut transcript = crate::Transcript::default();
        for text in ["h", "hé", "hé", "hello", "hello!"] {
            if let Some(event) = reconciler.update("message", text)? {
                transcript.apply(&event);
            }
        }
        assert_eq!(transcript.len(), 1);
        assert_eq!(
            transcript.entry(0).map(crate::Entry::summary),
            Some("agent: hello!".into())
        );
        assert!(
            reconciler
                .update("message", &"x".repeat(MAX_MESSAGE_BYTES + 1))
                .is_err()
        );
        assert!(reconciler.update("message", "hello!")?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn text_publisher_applies_backpressure_until_events_are_released()
    -> Result<(), Box<dyn std::error::Error>> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut publisher = FreebuffTextPublisher::new(sender, 1)?;
        publisher.update("message", "first").await?;
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(30),
                publisher.update("message", "second")
            )
            .await
            .is_err()
        );
        let first = receiver.recv().await.ok_or("missing first event")?;
        drop(first);
        publisher.update("message", "second").await?;
        assert!(
            matches!(receiver.recv().await, Some(HarnessEvent::TextReplaced { text, .. }) if text == "second")
        );
        drop(receiver);
        assert!(publisher.update("message", "third").await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn text_and_tool_updates_share_capacity_and_preserve_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut publisher = FreebuffEventPublisher::new(sender, 1)?;
        let tool = FreebuffToolSnapshot {
            id: "tool-1".into(),
            title: "Read file".into(),
            kind: "read".into(),
            status: ToolStatus::Running,
            output: None,
        };
        publisher.text("message-1", "checking").await?;
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(30),
                publisher.tool(tool.clone())
            )
            .await
            .is_err()
        );
        let first = receiver.recv().await.ok_or("missing message")?;
        assert!(matches!(&first, HarnessEvent::Delta { text, .. } if text == "checking"));
        drop(first);
        publisher.tool(tool).await?;
        assert!(matches!(
            receiver.recv().await,
            Some(HarnessEvent::ToolCall {
                status: ToolStatus::Running,
                ..
            })
        ));
        publisher.text("message-1", "checking done").await?;
        assert!(
            matches!(receiver.recv().await, Some(HarnessEvent::Delta { text, .. }) if text == " done")
        );
        Ok(())
    }

    #[test]
    fn discovery_reads_the_configured_executable_from_agent_settings()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let executable = directory.path().join("custom-freebuff");
        std::fs::write(&executable, b"fixture")?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
        let settings_path = directory.path().join("settings.json");
        std::fs::write(
            &settings_path,
            serde_json::to_vec(&serde_json::json!({
                "app": { "agents.freebuff.executable": executable }
            }))?,
        )?;
        let settings = AgentSettings::read(&settings_path)?;
        let discovery = discover_from_settings(&settings, OsStr::new(""))?;
        assert_eq!(
            discovery.install.map(|install| install.executable),
            Some(executable)
        );
        Ok(())
    }

    #[test]
    fn configured_executable_precedes_path_without_running_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let configured = directory.path().join("configured");
        let on_path = directory.path().join("freebuff");
        for path in [&configured, &on_path] {
            std::fs::write(path, b"not a runnable program")?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
        }
        let result = discover(Some(&configured), directory.path().as_os_str());
        assert_eq!(
            result.install,
            Some(FreebuffInstall {
                executable: configured,
                source: FreebuffInstallSource::Configured,
            })
        );
        assert!(result.attempts.is_empty());
        Ok(())
    }

    #[test]
    fn unusable_configured_and_path_candidates_are_reported_before_fallback()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        std::fs::create_dir_all(&first)?;
        std::fs::create_dir_all(&second)?;
        std::fs::write(first.join("freebuff"), b"not executable")?;
        let executable = second.join("freebuff");
        std::fs::write(&executable, b"fixture")?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
        let path = std::env::join_paths([first, second])?;
        let result = discover(Some(&directory.path().join("missing")), &path);
        assert_eq!(
            result.install,
            Some(FreebuffInstall {
                executable,
                source: FreebuffInstallSource::Path,
            })
        );
        assert_eq!(result.attempts.len(), 2);
        assert!(
            result
                .attempts
                .iter()
                .any(|attempt| attempt.reason == "not executable")
        );
        Ok(())
    }

    #[test]
    fn missing_executable_has_actionable_diagnostics() {
        let result = discover(None, OsStr::new(""));
        assert!(result.install.is_none());
        assert!(result.summary().contains("configure its path"));
    }

    #[test]
    fn runtime_metadata_is_read_without_launching_freebuff()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        assert!(read_runtime_metadata(directory.path())?.is_none());
        std::fs::write(
            directory.path().join("freebuff-metadata.json"),
            br#"{"version":"0.2.36","target":"darwin-arm64"}"#,
        )?;
        assert_eq!(
            read_runtime_metadata(directory.path())?,
            Some(FreebuffRuntimeMetadata {
                version: "0.2.36".into(),
                target: "darwin-arm64".into(),
            })
        );
        Ok(())
    }

    #[test]
    fn malformed_or_oversized_runtime_metadata_is_rejected()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("freebuff-metadata.json");
        std::fs::write(&path, br#"{"version":"","target":"darwin-arm64"}"#)?;
        assert!(read_runtime_metadata(directory.path()).is_err());
        std::fs::write(&path, vec![b' '; MAX_METADATA_BYTES as usize + 1])?;
        assert!(read_runtime_metadata(directory.path()).is_err());
        Ok(())
    }
}
