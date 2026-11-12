use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::acp::{TerminalBackend, TerminalLaunch, TerminalProcessEvent};
use crate::{
    AdapterTier, AgentDiagnostics, AgentHarness, ApprovalId, Capabilities, Decision, DeltaKind,
    HarnessError, HarnessEvent, HarnessResult, ItemId, PromptInput, SessionId, SpawnSpec,
    ToolCallId, ToolStatus, command_line, settings::AgentSettings,
};

const MAX_METADATA_BYTES: u64 = 16 * 1024;
const MAX_RUNTIME_BYTES: u64 = 512 * 1024 * 1024;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_MESSAGE_ITEMS: usize = 256;
const MAX_TRANSCRIPT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreebuffScreenWarning {
    Disconnected,
    LauncherRestart,
}

/// Observations can invalidate an interaction but never establish readiness.
pub fn screen_warning_from_rows<'a>(
    rows: impl IntoIterator<Item = &'a str>,
) -> Option<FreebuffScreenWarning> {
    for row in rows {
        let row = row.trim();
        if row == "Can't reach Freebuff's servers. Retrying… Try another network." {
            return Some(FreebuffScreenWarning::Disconnected);
        }
        if row.starts_with("Update available: ") && row.contains(" → ")
            || row.starts_with("Update failed; restarting freebuff ")
        {
            return Some(FreebuffScreenWarning::LauncherRestart);
        }
    }
    None
}

const COMPOSER_CURSOR: char = '▍';
const COMPOSER_PLACEHOLDER: &str = "Enter a coding task or / for commands";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreebuffComposer {
    pub text: String,
    pub empty: bool,
}

/// Recognizes the input box of the recorded ready screen. Anything that does
/// not match the recorded shape returns `None`: the caller must not guess at
/// the entry state, because a wrong guess types into the transcript or loses a
/// prompt.
pub fn composer_from_rows<'a>(rows: impl IntoIterator<Item = &'a str>) -> Option<FreebuffComposer> {
    let rows = rows.into_iter().collect::<Vec<_>>();
    let start = rows.iter().rposition(|row| row.contains('╭'))?;
    let end = start + rows.iter().skip(start).position(|row| row.contains('╰'))?;
    let mut entry = None;
    for row in &rows[start..end] {
        if !row.contains(COMPOSER_CURSOR) {
            continue;
        }
        if entry.is_some() {
            return None;
        }
        entry = Some((*row).trim_start().strip_prefix('│')?.strip_suffix('│')?);
    }
    let (before, after) = entry?.strip_prefix("  ")?.split_once(COMPOSER_CURSOR)?;
    if before.len() > MAX_MESSAGE_BYTES {
        return None;
    }
    if !before.trim_end().is_empty() {
        return after.trim_start().is_empty().then(|| FreebuffComposer {
            text: before.to_owned(),
            empty: false,
        });
    }
    after
        .trim_start()
        .strip_prefix(COMPOSER_PLACEHOLDER)
        .map(|rest| FreebuffComposer {
            text: String::new(),
            empty: rest.trim().is_empty(),
        })
        .filter(|composer| composer.empty)
}

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
        if self.sender.is_closed() {
            return Err(HarnessError::TransportClosed(
                "Freebuff event receiver closed".into(),
            ));
        }
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
        if self.sender.is_closed() {
            return Err(HarnessError::TransportClosed(
                "Freebuff event receiver closed".into(),
            ));
        }
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
    active_turn: Option<String>,
    used_turns: BTreeSet<String>,
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
            active_turn: None,
            used_turns: BTreeSet::new(),
        })
    }

    /// `turn_id` must be unique across the persisted session, including resumes.
    pub fn begin_turn(&mut self, turn_id: &str) -> HarnessResult<()> {
        if self.active_turn.is_some() {
            return Err(HarnessError::Protocol(
                "Freebuff turn is already active".into(),
            ));
        }
        if turn_id.is_empty() || turn_id.len() > 128 || self.used_turns.contains(turn_id) {
            return Err(HarnessError::Protocol(
                "invalid or reused Freebuff turn identity".into(),
            ));
        }
        if self.used_turns.len() >= 16_384 {
            return Err(HarnessError::Protocol(
                "too many Freebuff turns in one session".into(),
            ));
        }
        self.used_turns.insert(turn_id.to_owned());
        self.active_turn = Some(turn_id.to_owned());
        Ok(())
    }

    fn scoped_id(&self, item_id: &str) -> HarnessResult<String> {
        let turn_id = self
            .active_turn
            .as_ref()
            .ok_or_else(|| HarnessError::Protocol("Freebuff has no active turn".into()))?;
        let scoped = format!("{}:{turn_id}:{item_id}", turn_id.len());
        if item_id.is_empty() || scoped.len() > 256 {
            return Err(HarnessError::Protocol(
                "invalid Freebuff item identity".into(),
            ));
        }
        Ok(scoped)
    }

    pub async fn text(&mut self, item_id: &str, text: &str) -> HarnessResult<()> {
        let item_id = self.scoped_id(item_id)?;
        self.text.update(&item_id, text).await
    }

    pub async fn tool(&mut self, mut snapshot: FreebuffToolSnapshot) -> HarnessResult<()> {
        snapshot.id = self.scoped_id(&snapshot.id)?;
        self.tools.update(snapshot).await
    }

    pub fn finish_turn(&mut self) -> HarnessResult<()> {
        if self.active_turn.take().is_none() {
            return Err(HarnessError::Protocol("Freebuff has no active turn".into()));
        }
        self.text.clear();
        self.tools.clear();
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct FreebuffRuntimeMetadata {
    pub version: String,
    pub target: String,
}

#[derive(Deserialize)]
struct FreebuffLauncherPackage {
    name: String,
    version: String,
}

/// Reads package metadata without running the launcher. This does not verify
/// that the executable uses the package or that its screen protocol is supported.
pub fn read_launcher_version(executable: &Path) -> HarnessResult<Option<String>> {
    let executable = std::fs::canonicalize(executable).map_err(|error| {
        HarnessError::Protocol(format!(
            "could not inspect Freebuff executable {}: {error}",
            executable.display()
        ))
    })?;
    let Some(parent) = executable.parent() else {
        return Ok(None);
    };
    let package = if executable.file_name() == Some(OsStr::new("index.js"))
        && parent.file_name() == Some(OsStr::new("freebuff"))
    {
        parent.join("package.json")
    } else if executable.file_name() == Some(OsStr::new("freebuff"))
        && parent.file_name() == Some(OsStr::new("bin"))
    {
        let Some(root) = parent.parent() else {
            return Ok(None);
        };
        root.join("lib/node_modules/freebuff/package.json")
    } else {
        return Ok(None);
    };
    let file = match std::fs::File::open(&package) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(HarnessError::Protocol(format!(
                "could not open Freebuff launcher metadata at {}: {error}",
                package.display()
            )));
        }
    };
    let mut bytes = Vec::new();
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            HarnessError::Protocol(format!(
                "could not read Freebuff launcher metadata at {}: {error}",
                package.display()
            ))
        })?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(HarnessError::Protocol(format!(
            "Freebuff launcher metadata at {} exceeds {MAX_METADATA_BYTES} bytes",
            package.display()
        )));
    }
    let metadata: FreebuffLauncherPackage = serde_json::from_slice(&bytes).map_err(|error| {
        HarnessError::Protocol(format!(
            "invalid Freebuff launcher metadata at {}: {error}",
            package.display()
        ))
    })?;
    if metadata.name != "freebuff" || metadata.version.trim().is_empty() {
        return Err(HarnessError::Protocol(format!(
            "invalid Freebuff launcher package identity at {}",
            package.display()
        )));
    }
    Ok(Some(metadata.version))
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

/// Fingerprints the cached file, without asserting that it is the running child.
pub fn cached_runtime_sha256(directory: &Path) -> HarnessResult<Option<String>> {
    let path = directory.join(format!("freebuff{}", std::env::consts::EXE_SUFFIX));
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(HarnessError::Protocol(format!(
                "could not open cached Freebuff runtime at {}: {error}",
                path.display()
            )));
        }
    };
    let metadata = file.metadata().map_err(|error| {
        HarnessError::Protocol(format!(
            "could not inspect cached Freebuff runtime at {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() || metadata.len() > MAX_RUNTIME_BYTES {
        return Err(HarnessError::Protocol(format!(
            "cached Freebuff runtime at {} is not a supported regular file",
            path.display()
        )));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let count = file.read(&mut buffer).map_err(|error| {
            HarnessError::Protocol(format!(
                "could not read cached Freebuff runtime at {}: {error}",
                path.display()
            ))
        })?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .filter(|size| *size <= MAX_RUNTIME_BYTES)
            .ok_or_else(|| {
                HarnessError::Protocol("cached Freebuff runtime exceeds the supported size".into())
            })?;
        digest.update(&buffer[..count]);
    }
    let after = file.metadata().map_err(|error| {
        HarnessError::Protocol(format!(
            "could not recheck cached Freebuff runtime at {}: {error}",
            path.display()
        ))
    })?;
    let modified = metadata.modified().map_err(|error| {
        HarnessError::Protocol(format!(
            "could not read cached Freebuff runtime modification time: {error}"
        ))
    })?;
    let after_modified = after.modified().map_err(|error| {
        HarnessError::Protocol(format!(
            "could not recheck cached Freebuff runtime modification time: {error}"
        ))
    })?;
    if total != metadata.len() || after.len() != metadata.len() || modified != after_modified {
        return Err(HarnessError::Protocol(
            "cached Freebuff runtime changed while fingerprinting".into(),
        ));
    }
    Ok(Some(
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    ))
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

/// A terminal-tier adapter for Freebuff's interactive CLI.
///
/// Freebuff has no machine protocol; it is a TUI. This wrapper launches it in
/// the task worktree and keeps its terminal visible, but advertises only what
/// it can prove. Screen recognition is not connected yet, so composer prompts
/// fail visibly instead of being submitted into an unrecognized screen.
pub struct FreebuffHarness {
    events: mpsc::UnboundedReceiver<HarnessEvent>,
    sender: mpsc::UnboundedSender<HarnessEvent>,
    terminals: Option<Arc<dyn TerminalBackend>>,
    terminal_id: Option<String>,
    diagnostics: AgentDiagnostics,
    watcher: Option<tokio::task::JoinHandle<()>>,
}

impl FreebuffHarness {
    pub fn new() -> Self {
        let (sender, events) = mpsc::unbounded_channel();
        Self {
            events,
            sender,
            terminals: None,
            terminal_id: None,
            diagnostics: AgentDiagnostics::default(),
            watcher: None,
        }
    }

    pub fn with_terminals(mut self, terminals: Arc<dyn TerminalBackend>) -> Self {
        self.terminals = Some(terminals);
        self
    }

    async fn stop_terminal(&mut self) -> HarnessResult<()> {
        let (Some(terminals), Some(id)) = (self.terminals.clone(), self.terminal_id.take()) else {
            return Ok(());
        };
        terminals.stop(&id).await.map_err(|error| {
            HarnessError::Protocol(format!("could not stop the Freebuff terminal: {error}"))
        })
    }
}

#[async_trait]
impl AgentHarness for FreebuffHarness {
    fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
        &mut self.events
    }

    fn diagnostics(&self) -> AgentDiagnostics {
        self.diagnostics.clone()
    }

    fn tier(&self) -> AdapterTier {
        AdapterTier::TerminalWrapper
    }

    async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities> {
        let terminals = self.terminals.clone().ok_or_else(|| {
            HarnessError::Protocol("Freebuff terminal backend is unavailable".into())
        })?;
        let terminal_id = format!("freebuff-{}", uuid::Uuid::new_v4());
        let (process_sender, mut process_events) = mpsc::channel(64);
        terminals
            .spawn(
                TerminalLaunch {
                    id: terminal_id.clone(),
                    cwd: spec.cwd.clone(),
                    command: spec.program.clone(),
                    args: spec.args.clone(),
                    environment: spec.environment.clone(),
                    observe_screen: true,
                    block_user_input: true,
                },
                process_sender,
            )
            .await
            .map_err(|error| {
                HarnessError::Protocol(format!("could not start Freebuff: {error}"))
            })?;
        self.diagnostics = AgentDiagnostics {
            command: command_line(&spec),
            protocol: "terminal wrapper (Freebuff)".into(),
            fallback_reason: Some("Freebuff screen interaction is not connected".into()),
            ..AgentDiagnostics::default()
        };
        self.terminal_id = Some(terminal_id.clone());
        if self
            .sender
            .send(HarnessEvent::AgentTerminalStarted {
                id: terminal_id.clone(),
                title: "Freebuff".into(),
                stream_permit: None,
            })
            .is_err()
        {
            self.stop_terminal().await?;
            return Err(HarnessError::TransportClosed(
                "Freebuff terminal event listener closed".into(),
            ));
        }
        let events = self.sender.clone();
        let exited_id = terminal_id.clone();
        self.watcher = Some(tokio::spawn(async move {
            let mut last_warning = None;
            while let Some(event) = process_events.recv().await {
                match event {
                    TerminalProcessEvent::ScreenRows(rows) => {
                        let warning = screen_warning_from_rows(rows.iter().map(String::as_str));
                        if let Some(warning) =
                            warning.filter(|warning| Some(*warning) != last_warning)
                        {
                            let text = match warning {
                                FreebuffScreenWarning::Disconnected => {
                                    "Freebuff reported a connection failure; its current prompt and session state are not verified."
                                }
                                FreebuffScreenWarning::LauncherRestart => {
                                    "Freebuff reported a launcher update or restart; its runtime and session state must be verified again."
                                }
                            };
                            if events
                                .send(HarnessEvent::AgentTerminalError {
                                    id: exited_id.clone(),
                                    text: text.into(),
                                    stream_permit: None,
                                })
                                .is_err()
                            {
                                break;
                            }
                            last_warning = Some(warning);
                        }
                    }
                    TerminalProcessEvent::Exited(status) => {
                        if events
                            .send(HarnessEvent::AgentTerminalExited {
                                id: exited_id,
                                exit_code: status.exit_code,
                                stream_permit: None,
                            })
                            .is_err()
                        {
                            tracing::debug!("Freebuff terminal event listener closed before exit");
                        }
                        break;
                    }
                    TerminalProcessEvent::Output(_) => {}
                }
            }
        }));
        Ok(Capabilities {
            cancel: true,
            agent_terminals: true,
            ..Capabilities::default()
        })
    }

    async fn open_session(&mut self, resume: Option<SessionId>) -> HarnessResult<SessionId> {
        if resume.is_some() {
            return Err(HarnessError::Protocol(
                "Freebuff terminal sessions cannot be resumed".into(),
            ));
        }
        Ok(SessionId::new(uuid::Uuid::new_v4().to_string()))
    }

    async fn prompt(&mut self, _input: PromptInput) -> HarnessResult<()> {
        Err(HarnessError::Protocol(
            "Freebuff composer prompts are not supported yet: screen interaction is not verified. Use Freebuff (terminal) in the meantime."
                .into(),
        ))
    }

    async fn cancel(&mut self) -> HarnessResult<()> {
        self.stop_terminal().await
    }

    async fn respond(
        &mut self,
        _approval_id: ApprovalId,
        _decision: Decision,
    ) -> HarnessResult<()> {
        Err(HarnessError::Protocol(
            "Freebuff does not support approval responses".into(),
        ))
    }

    async fn shutdown(&mut self) -> HarnessResult<()> {
        if let Some(watcher) = self.watcher.take() {
            watcher.abort();
        }
        self.stop_terminal().await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct TerminalFixture {
        spawned: std::sync::Mutex<Vec<TerminalLaunch>>,
        stopped: std::sync::Mutex<Vec<String>>,
        events: std::sync::Mutex<Option<mpsc::Sender<TerminalProcessEvent>>>,
    }

    #[async_trait]
    impl TerminalBackend for TerminalFixture {
        async fn spawn(
            &self,
            launch: TerminalLaunch,
            events: mpsc::Sender<TerminalProcessEvent>,
        ) -> Result<(), String> {
            events
                .try_send(TerminalProcessEvent::ScreenRows(vec![
                    "Can't reach Freebuff's servers. Retrying… Try another network.".into(),
                ]))
                .map_err(|error| error.to_string())?;
            *self.events.lock().map_err(|_| "event lock poisoned")? = Some(events);
            self.spawned
                .lock()
                .map_err(|_| "spawn lock poisoned".to_string())?
                .push(launch);
            Ok(())
        }

        async fn stop(&self, id: &str) -> Result<(), String> {
            self.stopped
                .lock()
                .map_err(|_| "stop lock poisoned".to_string())?
                .push(id.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn terminal_wrapper_launches_freebuff_and_rejects_unverified_prompts()
    -> Result<(), Box<dyn std::error::Error>> {
        let backend = Arc::new(TerminalFixture {
            spawned: Default::default(),
            stopped: Default::default(),
            events: Default::default(),
        });
        let mut harness = FreebuffHarness::new().with_terminals(backend.clone());
        assert_eq!(harness.tier(), AdapterTier::TerminalWrapper);
        let capabilities = harness
            .start(SpawnSpec {
                program: "freebuff".into(),
                cwd: PathBuf::from("/worktree"),
                ..SpawnSpec::default()
            })
            .await?;
        assert!(capabilities.cancel && capabilities.agent_terminals);
        assert!(!capabilities.streaming);
        assert!(!capabilities.approvals);
        assert!(!capabilities.resume);
        assert!(!capabilities.model_picker);
        assert!(harness.diagnostics().protocol.contains("terminal wrapper"));
        match harness.events().recv().await {
            Some(HarnessEvent::AgentTerminalStarted { id, title, .. }) => {
                assert_eq!(title, "Freebuff");
                assert!(id.starts_with("freebuff-"));
            }
            other => return Err(format!("unexpected event: {other:?}").into()),
        }
        assert!(
            backend
                .spawned
                .lock()
                .map_err(|_| "spawn lock poisoned")?
                .first()
                .ok_or("missing terminal launch")?
                .observe_screen
        );
        assert!(
            backend
                .spawned
                .lock()
                .map_err(|_| "spawn lock poisoned")?
                .first()
                .ok_or("missing terminal launch")?
                .block_user_input
        );
        let events = backend
            .events
            .lock()
            .map_err(|_| "event lock poisoned")?
            .clone()
            .ok_or("missing terminal events")?;
        assert!(
            matches!(harness.events().recv().await, Some(HarnessEvent::AgentTerminalError { text, .. }) if text.contains("connection failure"))
        );
        events
            .send(TerminalProcessEvent::ScreenRows(vec!["Reconnected".into()]))
            .await?;
        events
            .send(TerminalProcessEvent::ScreenRows(vec![
                "Update available: 0.2.36 → 0.2.37".into(),
            ]))
            .await?;
        assert!(
            matches!(harness.events().recv().await, Some(HarnessEvent::AgentTerminalError { text, .. }) if text.contains("launcher update"))
        );
        {
            let spawned = backend.spawned.lock().map_err(|_| "spawn lock poisoned")?;
            assert_eq!(spawned.len(), 1);
            assert_eq!(spawned[0].command, "freebuff");
            assert_eq!(spawned[0].cwd, PathBuf::from("/worktree"));
        }
        assert!(harness.prompt(PromptInput::text("hello")).await.is_err());
        harness.shutdown().await?;
        assert_eq!(
            backend
                .stopped
                .lock()
                .map_err(|_| "stop lock poisoned")?
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn composer_recognition_reads_the_recorded_ready_entry() {
        let ready = [
            " ┌──────────────────────────────────────────────┐",
            " │ May use data for AI training                 │",
            " └──────────────────────────────────────────────┘",
            "╭──────────────────────────────────────────────╮",
            "│                                              │",
            "│  ▍Enter a coding task or / for commands      │",
            "│                                              │",
            "╰──────────────────────────────────────────────╯",
            " DeepSeek V4.1 Flash Fast • low · chat: New chat",
        ];
        assert_eq!(
            composer_from_rows(ready),
            Some(FreebuffComposer {
                text: String::new(),
                empty: true,
            })
        );
        let typed = [
            "╭──────────────────────────────────────────────╮",
            "│                                              │",
            "│  give me the dotenv fragment to override▍    │",
            "│                                              │",
            "╰──────────────────────────────────────────────╯",
        ];
        assert_eq!(
            composer_from_rows(typed),
            Some(FreebuffComposer {
                text: "give me the dotenv fragment to override".into(),
                empty: false,
            })
        );
    }

    #[test]
    fn composer_recognition_rejects_unrecorded_entries() {
        assert!(composer_from_rows(["no composer here"]).is_none());
        assert!(
            composer_from_rows([
                "╭──────────────────────────────╮",
                "│  ▍                           │",
                "╰──────────────────────────────╯",
            ])
            .is_none()
        );
        assert!(
            composer_from_rows([
                "╭──────────────────────────────╮",
                "│  ▍one                          │",
                "│  ▍two                          │",
                "╰──────────────────────────────╯",
            ])
            .is_none()
        );
        assert!(
            composer_from_rows([
                "╭──────────────────────────────╮",
                "│  no cursor here             │"
            ])
            .is_none()
        );
    }

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
        publisher.begin_turn("turn-1")?;
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

    #[tokio::test]
    async fn reused_terminal_item_ids_stay_distinct_across_turns()
    -> Result<(), Box<dyn std::error::Error>> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut publisher = FreebuffEventPublisher::new(sender, 4)?;
        let tool = FreebuffToolSnapshot {
            id: "tool-1".into(),
            title: "Read file".into(),
            kind: "read".into(),
            status: ToolStatus::Completed,
            output: None,
        };
        let mut transcript = crate::Transcript::default();
        for turn in ["turn-1", "turn-2"] {
            publisher.begin_turn(turn)?;
            publisher.tool(tool.clone()).await?;
            let event = receiver.recv().await.ok_or("missing tool event")?;
            transcript.apply(&event);
            publisher.finish_turn()?;
        }
        assert_eq!(transcript.len(), 2);
        assert!(publisher.begin_turn("turn-1").is_err());
        assert!(publisher.tool(tool).await.is_err());
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
    fn path_lookup_finds_freebuff_without_configuration() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let executable = directory.path().join("freebuff");
        std::fs::write(&executable, b"fixture")?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
        let result = discover(None, directory.path().as_os_str());
        assert_eq!(
            result.install,
            Some(FreebuffInstall {
                executable,
                source: FreebuffInstallSource::Path,
            })
        );
        Ok(())
    }

    #[test]
    fn missing_executable_has_actionable_diagnostics() {
        let result = discover(None, OsStr::new(""));
        assert!(result.install.is_none());
        assert!(result.summary().contains("configure its path"));
        assert!(result.summary().contains("PATH"));
    }

    #[test]
    fn launcher_version_is_read_without_executing_the_wrapper()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let binary = directory.path().join("bin/freebuff");
        let package = directory
            .path()
            .join("lib/node_modules/freebuff/package.json");
        std::fs::create_dir_all(binary.parent().ok_or("missing binary parent")?)?;
        std::fs::create_dir_all(package.parent().ok_or("missing package parent")?)?;
        std::fs::write(&binary, b"not an executable program")?;
        assert_eq!(read_launcher_version(&binary)?, None);
        std::fs::write(&package, br#"{"name":"freebuff","version":"0.0.171"}"#)?;
        assert_eq!(read_launcher_version(&binary)?.as_deref(), Some("0.0.171"));
        std::fs::write(&package, br#"{"name":"other","version":"0.0.171"}"#)?;
        assert!(read_launcher_version(&binary).is_err());
        std::fs::write(&package, vec![b' '; MAX_METADATA_BYTES as usize + 1])?;
        assert!(read_launcher_version(&binary).is_err());
        Ok(())
    }

    #[test]
    fn cached_runtime_fingerprint_reads_the_file_without_running_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        assert_eq!(cached_runtime_sha256(directory.path())?, None);
        let path = directory
            .path()
            .join(format!("freebuff{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&path, b"abc")?;
        assert_eq!(
            cached_runtime_sha256(directory.path())?.as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            std::fs::remove_file(&path)?;
            let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
            if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            assert!(cached_runtime_sha256(directory.path()).is_err());
        }
        Ok(())
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
