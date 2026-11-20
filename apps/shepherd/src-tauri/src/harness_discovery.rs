use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{config_files, settings::Settings};

const MAX_AGE: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HarnessIssue {
    Missing,
    NotExecutable,
    CheckFailed,
    VersionFailed,
    VersionTimedOut,
    Configuration,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedHarness {
    agent: String,
    path: String,
    version: Option<String>,
    issue: Option<HarnessIssue>,
    pub found: bool,
    was_available: bool,
    last_verified_at: Option<u64>,
}

impl DetectedHarness {
    fn found(agent: &str, path: String, version: Option<String>) -> Self {
        Self {
            agent: agent.into(),
            path,
            version,
            issue: None,
            found: true,
            was_available: false,
            last_verified_at: None,
        }
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverySnapshot {
    checked_at: u64,
    #[serde(skip)]
    generation: u64,
    pub harnesses: Vec<DetectedHarness>,
    errors: BTreeMap<String, String>,
}

pub struct HarnessDiscovery {
    path: PathBuf,
    snapshot: Mutex<DiscoverySnapshot>,
    refresh_lock: Mutex<()>,
}

impl HarnessDiscovery {
    pub fn load(path: PathBuf) -> Self {
        let snapshot = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, "could not decode harness detection cache");
                DiscoverySnapshot::default()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                DiscoverySnapshot::default()
            }
            Err(error) => {
                tracing::warn!(%error, "could not read harness detection cache");
                DiscoverySnapshot::default()
            }
        };
        Self {
            path,
            snapshot: Mutex::new(snapshot),
            refresh_lock: Mutex::new(()),
        }
    }

    pub async fn snapshot(&self) -> DiscoverySnapshot {
        self.snapshot.lock().await.clone()
    }

    pub async fn refresh(
        &self,
        preferences: &Settings,
        availability: &BTreeMap<&str, bool>,
        force: bool,
    ) -> Result<DiscoverySnapshot, String> {
        let previous_generation = self.snapshot.lock().await.generation;
        let _refresh = self.refresh_lock.lock().await;
        let previous = self.snapshot().await;
        let now = unix_seconds();
        let fresh = previous.generation != 0
            && now
                .checked_sub(previous.checked_at)
                .is_some_and(|age| age < MAX_AGE.as_secs());
        if previous.generation != previous_generation || (!force && fresh) {
            return Ok(previous);
        }
        let settings = preferences.agent_settings().await;
        let (detected, errors) = detect_harnesses(&settings).await?;
        let harnesses = merge_discovery(previous.harnesses, detected, &errors, availability);
        let next = DiscoverySnapshot {
            checked_at: unix_seconds(),
            generation: previous.generation + 1,
            harnesses,
            errors,
        };
        *self.snapshot.lock().await = next.clone();
        let stored = next.clone();
        let path = self.path.clone();
        match tokio::task::spawn_blocking(move || -> Result<(), String> {
            let bytes = serde_json::to_vec(&stored).map_err(|error| error.to_string())?;
            config_files::write_atomic(&path, &bytes).map_err(|error| error.to_string())
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, "could not save harness detection cache"),
            Err(error) => tracing::warn!(%error, "could not finish saving harness detection cache"),
        }
        Ok(next)
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn merge_discovery(
    previous: Vec<DetectedHarness>,
    mut detected: Vec<DetectedHarness>,
    errors: &BTreeMap<String, String>,
    availability: &BTreeMap<&str, bool>,
) -> Vec<DetectedHarness> {
    let checked_at = unix_seconds();
    for entry in &mut detected {
        entry.last_verified_at = entry.issue.is_none().then_some(checked_at);
        entry.was_available = availability
            .get(entry.agent.as_str())
            .copied()
            .unwrap_or(false);
        if let Some(last) = previous
            .iter()
            .find(|last| last.agent == entry.agent && last.path == entry.path)
        {
            if entry.issue.is_some() {
                entry.was_available |= last.was_available;
                entry.last_verified_at = last.last_verified_at;
                entry.version = entry.version.take().or_else(|| last.version.clone());
            }
        }
    }
    for mut last in previous {
        if detected
            .iter()
            .any(|entry| entry.agent == last.agent && entry.path == last.path)
        {
            continue;
        }
        let issue = if errors.contains_key(&last.agent) {
            HarnessIssue::Configuration
        } else if let Some(issue) = path_issue(Path::new(&last.path)) {
            issue
        } else if detected
            .iter()
            .any(|entry| entry.agent == last.agent && entry.found)
        {
            continue;
        } else {
            HarnessIssue::CheckFailed
        };
        last.found = false;
        last.issue = Some(issue);
        detected.push(last);
    }
    detected
}

fn path_issue(path: &Path) -> Option<HarnessIssue> {
    let metadata = match path.metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(HarnessIssue::Missing);
        }
        Err(_) => return Some(HarnessIssue::CheckFailed),
    };
    if metadata.is_dir() && path.extension().is_some_and(|extension| extension == "app") {
        return None;
    }
    if !metadata.is_file() {
        return Some(HarnessIssue::NotExecutable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Some(HarnessIssue::NotExecutable);
        }
    }
    None
}

async fn detect_harnesses(
    settings: &agent_harness::settings::AgentSettings,
) -> Result<(Vec<DetectedHarness>, BTreeMap<String, String>), String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let current_directory = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut errors = BTreeMap::new();
    let mut detected = Vec::new();
    match settings.executable("agents.codex.executable") {
        Ok(configured) => {
            use agent_harness::codex::{CodexInstallKind, installation_candidates};
            let mut found_on_path = false;
            for (candidate, kind) in installation_candidates(configured.as_deref()) {
                if (kind == CodexInstallKind::Path && found_on_path)
                    || path_issue(&candidate).is_some()
                    || detected
                        .iter()
                        .any(|entry: &DetectedHarness| entry.path == candidate.to_string_lossy())
                {
                    continue;
                }
                found_on_path |= kind == CodexInstallKind::Path;
                detected.push(DetectedHarness::found(
                    "codex",
                    candidate.to_string_lossy().into_owned(),
                    None,
                ));
            }
        }
        Err(error) => {
            errors.insert("codex".into(), error.to_string());
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut applications = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/Applications/Nix Apps"),
    ];
    if let Some(home) = &home {
        applications.push(home.join("Applications"));
        applications.push(home.join("Applications/Home Manager Apps"));
        applications.push(home.join("Applications/NixTrampolines"));
    }
    let home_path = home.as_deref();
    let user = std::env::var_os("USER");
    match agent_harness::acp::discover_antigravity(settings, &path, home_path, user.as_deref()) {
        Ok(Some(launch)) => {
            detected.push(DetectedHarness::found("antigravity", launch.program, None));
        }
        Ok(None) => {}
        Err(error) => {
            errors.insert("antigravity".into(), error.to_string());
        }
    }
    for app in applications
        .into_iter()
        .map(|directory| directory.join("Antigravity.app"))
    {
        if app.is_dir() {
            detected.push(DetectedHarness::found(
                "antigravity",
                app.to_string_lossy().into_owned(),
                None,
            ));
        }
    }
    for (agent, executable) in [
        ("claude", "claude"),
        ("opencode", "opencode"),
        ("pi", "pi"),
        ("freebuff", "freebuff"),
        ("kilocode", "kilo"),
    ] {
        let configured = match settings.executable(&format!("agents.{agent}.executable")) {
            Ok(configured) => configured,
            Err(error) => {
                errors.insert(agent.into(), error.to_string());
                continue;
            }
        };
        let known_candidates = {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .into_iter()
                .flat_map(|home| {
                    [
                        home.join(".local/bin").join(executable),
                        home.join(".nix-profile/bin").join(executable),
                        home.join(".local/state/nix/profile/bin").join(executable),
                    ]
                })
                .chain(std::env::var_os("USER").map(|user| {
                    PathBuf::from("/etc/profiles/per-user")
                        .join(user)
                        .join("bin")
                        .join(executable)
                }))
                .chain([
                    PathBuf::from("/run/current-system/sw/bin").join(executable),
                    PathBuf::from("/nix/var/nix/profiles/default/bin").join(executable),
                    PathBuf::from("/opt/homebrew/bin").join(executable),
                    PathBuf::from("/usr/local/bin").join(executable),
                ])
                .collect::<Vec<_>>()
        };
        let candidates = configured.into_iter().chain(known_candidates).chain(
            std::env::split_paths(&path)
                .map(|directory| current_directory.join(directory).join(executable)),
        );
        for candidate in candidates {
            if path_issue(&candidate).is_some() {
                continue;
            }
            detected.push(DetectedHarness::found(
                agent,
                candidate.to_string_lossy().into_owned(),
                None,
            ));
            break;
        }
    }
    let mut version_checks = tokio::task::JoinSet::new();
    for (index, harness) in detected.iter().enumerate() {
        if harness.version.is_none() {
            let path = harness.path.clone();
            version_checks.spawn(async move { (index, harness_version(&path).await) });
        }
    }
    while let Some(result) = version_checks.join_next().await {
        match result {
            Ok((index, result)) => {
                if let Some(harness) = detected.get_mut(index) {
                    match result {
                        Ok(version) => harness.version = Some(version),
                        Err(issue) => harness.issue = Some(issue),
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "harness version task failed"),
        }
    }
    Ok((detected, errors))
}

async fn harness_version(path: &str) -> Result<String, HarnessIssue> {
    let mut command = if Path::new(path).is_dir() {
        let mut command = tokio::process::Command::new("/usr/bin/plutil");
        command.args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-"]);
        command.arg(Path::new(path).join("Contents/Info.plist"));
        command
    } else {
        let mut command = tokio::process::Command::new(path);
        command.arg("--version");
        command
    };
    command
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    match tokio::time::timeout(std::time::Duration::from_secs(3), command.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if version.is_empty() {
                Err(HarnessIssue::VersionFailed)
            } else {
                Ok(version)
            }
        }
        Ok(Ok(output)) => {
            tracing::debug!(%path, status = %output.status, "harness version check failed");
            Err(HarnessIssue::VersionFailed)
        }
        Ok(Err(error)) => {
            tracing::debug!(%path, %error, "could not read harness version");
            Err(HarnessIssue::VersionFailed)
        }
        Err(error) => {
            tracing::debug!(%path, %error, "harness version check timed out");
            Err(HarnessIssue::VersionTimedOut)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rechecking_keeps_last_known_installation_and_recovers()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory
            .path()
            .join("codex")
            .to_string_lossy()
            .into_owned();
        let mut last = DetectedHarness::found("codex", path.clone(), Some("1.2.3".into()));
        last.was_available = true;
        last.last_verified_at = Some(42);
        let cache_path = directory.path().join("harness-discovery.json");
        let stored = DiscoverySnapshot {
            checked_at: 42,
            generation: 1,
            harnesses: vec![last],
            errors: BTreeMap::new(),
        };
        config_files::write_atomic(&cache_path, &serde_json::to_vec(&stored)?)?;
        let mut reopened = HarnessDiscovery::load(cache_path);
        let cached = reopened.snapshot.get_mut();
        assert_eq!(cached.checked_at, 42);
        assert_eq!(cached.generation, 0);
        let previous = merge_discovery(
            std::mem::take(&mut cached.harnesses),
            vec![],
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert!(matches!(previous.as_slice(), [entry] if
            !entry.found && entry.was_available && entry.issue == Some(HarnessIssue::Missing)
            && entry.version.as_deref() == Some("1.2.3") && entry.last_verified_at == Some(42)));

        let mut timed_out = DetectedHarness::found("codex", path.clone(), None);
        timed_out.issue = Some(HarnessIssue::VersionTimedOut);
        let current = merge_discovery(
            previous,
            vec![timed_out],
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert!(matches!(current.as_slice(), [entry] if
            entry.found && entry.was_available && entry.issue == Some(HarnessIssue::VersionTimedOut)
            && entry.version.as_deref() == Some("1.2.3") && entry.last_verified_at == Some(42)));

        let restored = DetectedHarness::found("codex", path, Some("1.2.4".into()));
        let next = merge_discovery(
            current,
            vec![restored],
            &BTreeMap::new(),
            &BTreeMap::from([("codex", true)]),
        );
        assert!(matches!(next.as_slice(), [entry] if
            entry.found && entry.was_available && entry.issue.is_none()
            && entry.version.as_deref() == Some("1.2.4")));
        Ok(())
    }
}
