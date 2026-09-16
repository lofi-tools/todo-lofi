//! GitHub device-flow connection (spec decision 2).
//!
//! Device flow rather than an authorization-code exchange because GitHub's
//! token endpoint is not PKCE-compatible and a desktop app cannot hold a client
//! secret: anyone can read it out of the binary. The app is a public client, so
//! the flow is "show a code, the user types it into GitHub, poll until the
//! token appears".
//!
//! The token is API-only. Pushing a branch uses the user's own git credentials
//! (spec decision 30), so a token never reaches `.git/config`, a remote URL, or
//! a credential helper.

use std::future::Future;
use std::time::Duration;

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
pub const API_BASE: &str = "https://api.github.com";

/// Private repos plus writing issues, labels and pull requests; `read:user`
/// only names the connected account in the UI.
const SCOPE: &str = "repo read:user";

/// Client id of the app registered under the `lofi-tools` org with device flow
/// enabled (spec decision 34). Public by design — device flow needs no secret.
/// Forks and self-builds override it with their own app via `GITHUB_CLIENT_ID`.
const DEFAULT_CLIENT_ID: &str = "Ov23liq9LUbDfFPGJ0dC";

/// How much GitHub asks us to add to the interval on `slow_down`.
const SLOW_DOWN_STEP: u64 = 5;

/// One initial connection attempt plus three retries.
const CONNECT_ATTEMPTS: u32 = 4;
/// First retry delay, doubled after each consecutive transport failure.
const CONNECT_BACKOFF_BASE: Duration = Duration::from_secs(1);
/// Ceiling on the retry delay, so an outage does not park the flow too long.
const CONNECT_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// A connection attempt's outcome, with its retry behavior attached. anyhow
/// flattens typed failures into strings, so the marker preserves whether a
/// transport failure may be retried separately from the message shown to users.
#[derive(Debug)]
struct AttemptError {
    error: anyhow::Error,
    retryable: bool,
}

impl AttemptError {
    fn retryable(error: anyhow::Error) -> Self {
        Self {
            error,
            retryable: true,
        }
    }

    fn permanent(error: anyhow::Error) -> Self {
        Self {
            error,
            retryable: false,
        }
    }

    fn is_retryable(&self) -> bool {
        self.retryable
    }

    fn into_inner(self) -> anyhow::Error {
        self.error
    }
}

impl std::fmt::Display for AttemptError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.error)
    }
}

impl std::error::Error for AttemptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

/// Delay before retrying a failed connection attempt, doubling each time.
fn connect_backoff_delay(failed_attempts: u32) -> Duration {
    let shift = failed_attempts.min(5);
    CONNECT_BACKOFF_BASE
        .saturating_mul(1u32.checked_shl(shift).unwrap_or(u32::MAX))
        .min(CONNECT_BACKOFF_MAX)
}

/// Delay after one retryable failure, then count it toward the next delay.
async fn note_retryable_failure(failed_attempts: &mut u32) {
    tokio::time::sleep(connect_backoff_delay(*failed_attempts)).await;
    *failed_attempts = failed_attempts.saturating_add(1);
}

/// Retry one connection operation with exponential backoff. Permanent
/// failures—bad configuration, denied authorization, invalid tokens—return
/// immediately instead of waiting and retrying.
async fn retry_connection<T, Attempt, AttemptFuture>(mut attempt: Attempt) -> anyhow::Result<T>
where
    Attempt: FnMut() -> AttemptFuture,
    AttemptFuture: Future<Output = Result<T, AttemptError>>,
{
    let mut failed_attempts = 0;
    loop {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(error)
                if error.is_retryable() && failed_attempts + 1 < CONNECT_ATTEMPTS =>
            {
                note_retryable_failure(&mut failed_attempts).await;
            }
            Err(error) => return Err(error.into_inner()),
        }
    }
}

/// Transport failures and rate limits may be retried; other statuses need the
/// user to fix credentials, permissions, or the request.
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

/// Read a JSON response while preserving whether its failure may be retried.
/// A malformed body from a retryable status is treated as another transport
/// failure; otherwise the malformed body itself is the permanent problem.
async fn read_json_response(
    response: reqwest::Response,
    source: &str,
) -> Result<(reqwest::StatusCode, serde_json::Value), AttemptError> {
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AttemptError::retryable(anyhow::anyhow!("{source} response could not be read: {error}"))
    })?;
    let body = serde_json::from_str(&text).map_err(|error| {
        let error = anyhow::anyhow!("{source} response was not JSON: {error}");
        if is_retryable_status(status) {
            AttemptError::retryable(error)
        } else {
            AttemptError::permanent(error)
        }
    })?;
    Ok((status, body))
}

/// Registered connection persisted to `~/.config/my-todo/github.json`. Tokens
/// live here too (owner-only permissions), matching the Todoist file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct GithubFile {
    client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access_token: Option<String>,
    /// Optional personal access token. When present, every API call
    /// authenticates with it instead of the device-flow token, so work repos
    /// stay reachable without an org admin approving the OAuth app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    personal_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    /// The account the token belongs to, for the integration card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    login: Option<String>,
    /// Background sync cadence in seconds (`None` means the default).
    /// `0` disables automatic syncing; syncing still happens on demand
    /// through the card's Sync now button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    poll_interval_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at: Option<i64>,
}

fn client_file_path() -> anyhow::Result<std::path::PathBuf> {
    Ok(crate::todoist_auth::config_dir()?.join("github.json"))
}

// The file itself is reached through these path-taking helpers so tests can
// use a temp file instead of mutating `MY_TODO_CONFIG_DIR`, which is process
// state and races with the Todoist tests under the parallel runner.
fn load_at(path: &std::path::Path) -> anyhow::Result<Option<GithubFile>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Could not read {}: {e}", path.display()))?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| anyhow::anyhow!("Could not parse {}: {e}", path.display()))
}

fn save_at(path: &std::path::Path, client: &GithubFile) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("Could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(client)?)
        .map_err(|e| anyhow::anyhow!("Could not write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
    }
    Ok(())
}

/// The client id to authenticate with: `GITHUB_CLIENT_ID` when set, otherwise
/// the compiled-in app.
fn configured_client_id() -> anyhow::Result<(String, bool)> {
    client_id_at(&client_file_path()?)
}

/// The same, against an explicit connection file. A connection already
/// records the `client_id` it was made with (§5.1), so a fork or a self-build
/// configures the app once and reconnects without the environment variable.
fn client_id_at(path: &std::path::Path) -> anyhow::Result<(String, bool)> {
    match std::env::var("GITHUB_CLIENT_ID") {
        Ok(id) if !id.is_empty() => Ok((id, true)),
        _ if !DEFAULT_CLIENT_ID.is_empty() => Ok((DEFAULT_CLIENT_ID.to_string(), false)),
        _ => match load_at(path)?.map(|file| file.client_id) {
            Some(id) if !id.is_empty() => Ok((id, false)),
            _ => Err(anyhow::anyhow!(
                "No GitHub OAuth app is configured: register one with device flow enabled \
                 (spec decision 34) or set GITHUB_CLIENT_ID"
            )),
        },
    }
}

/// A started device flow: what the card shows the user, and what the poller
/// needs to keep asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceLogin {
    pub user_code: String,
    pub verification_uri: String,
    pub device_code: String,
    /// Seconds until the code stops working.
    pub expires_in: u64,
    /// Seconds to wait between polls.
    pub interval: u64,
}

/// Parse the device-code response.
pub fn parse_device_login(body: &serde_json::Value) -> anyhow::Result<DeviceLogin> {
    let field = |name: &str| -> anyhow::Result<String> {
        body.get(name)
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("GitHub device flow response had no `{name}`: {body}"))
    };
    Ok(DeviceLogin {
        user_code: field("user_code")?,
        verification_uri: body
            .get("verification_uri")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("https://github.com/login/device")
            .to_string(),
        device_code: field("device_code")?,
        expires_in: body
            .get("expires_in")
            .and_then(|value| value.as_u64())
            .unwrap_or(900),
        interval: body
            .get("interval")
            .and_then(|value| value.as_u64())
            .unwrap_or(5),
    })
}

/// What one poll of the token endpoint told us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollAction {
    /// Keep waiting for the user to approve.
    Wait,
    /// GitHub asked us to slow down; the extra delay is applied to every
    /// later poll too, as the flow requires.
    SlowDown,
    /// The user approved.
    Done {
        access_token: String,
        scope: Option<String>,
    },
    /// Stop polling: the code expired, the user denied it, or the app is
    /// misconfigured.
    Failed(String),
}

/// Poll pacing that honours `authorization_pending` / `slow_down`.
#[derive(Debug, Clone)]
pub struct PollSchedule {
    interval: u64,
}

impl Default for PollSchedule {
    fn default() -> Self {
        Self { interval: 5 }
    }
}

impl PollSchedule {
    pub fn new(interval: u64) -> Self {
        Self {
            interval: interval.max(1),
        }
    }

    pub fn delay(&self) -> Duration {
        Duration::from_secs(self.interval)
    }

    /// Fold one token response into the next action. Pure, so the whole error
    /// surface is unit-tested without a network.
    pub fn absorb(&mut self, body: &serde_json::Value) -> PollAction {
        if let Some(token) = body
            .get("access_token")
            .and_then(|value| value.as_str())
            .filter(|token| !token.is_empty())
        {
            return PollAction::Done {
                access_token: token.to_string(),
                scope: body
                    .get("scope")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
            };
        }
        match body.get("error").and_then(|value| value.as_str()) {
            Some("authorization_pending") => PollAction::Wait,
            Some("slow_down") => {
                self.interval += SLOW_DOWN_STEP;
                PollAction::SlowDown
            }
            Some("expired_token") => {
                PollAction::Failed("The code expired before it was approved".to_string())
            }
            Some("access_denied") => PollAction::Failed("The sign-in was denied".to_string()),
            Some("incorrect_client_credentials") => PollAction::Failed(
                "GitHub rejected the app's client id; reconnect the integration".to_string(),
            ),
            Some(other) => PollAction::Failed(format!("GitHub sign-in failed: {other}")),
            None => PollAction::Failed(format!("Unexpected GitHub response: {body}")),
        }
    }
}

/// Begin a device flow and return the code for the user to enter. Transport
/// failures back off and retry; configuration and response-shape failures do
/// not.
pub async fn begin() -> anyhow::Result<DeviceLogin> {
    retry_connection(begin_request).await
}

async fn begin_request() -> Result<DeviceLogin, AttemptError> {
    let (client_id, _) = configured_client_id().map_err(AttemptError::permanent)?;
    let body = format!(
        "client_id={}&scope={}",
        crate::todoist_auth::url_encode(&client_id),
        crate::todoist_auth::url_encode(SCOPE),
    );
    let response = reqwest::Client::new()
        .post(DEVICE_CODE_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|error| {
            AttemptError::retryable(anyhow::anyhow!("Could not reach GitHub: {error}"))
        })?;
    let (status, body) = read_json_response(response, "Device flow").await?;
    if !status.is_success() {
        let error = anyhow::anyhow!("GitHub device flow failed ({status}): {body}");
        if is_retryable_status(status) {
            return Err(AttemptError::retryable(error));
        }
        return Err(AttemptError::permanent(error));
    }
    parse_device_login(&body).map_err(AttemptError::permanent)
}

/// Poll until the user approves, then persist the token and return the login.
/// Runs on the Tokio runtime; never on GPUI's executor. Transport failures
/// back off and rejoin the poll; authorization failures stop it immediately.
pub async fn complete(login: DeviceLogin) -> anyhow::Result<String> {
    let (client_id, from_env) = configured_client_id()?;
    let mut schedule = PollSchedule::new(login.interval);
    let client = reqwest::Client::new();
    let mut failed_attempts = 0;
    loop {
        tokio::time::sleep(schedule.delay()).await;
        let body = format!(
            "client_id={}&device_code={}&grant_type={}",
            crate::todoist_auth::url_encode(&client_id),
            crate::todoist_auth::url_encode(&login.device_code),
            crate::todoist_auth::url_encode("urn:ietf:params:oauth:grant-type:device_code"),
        );
        let response = match client
            .post(TOKEN_URL)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await
        {
            Ok(response) => {
                failed_attempts = 0;
                response
            }
            Err(_error) => {
                note_retryable_failure(&mut failed_attempts).await;
                continue;
            }
        };
        let (status, body) = match read_json_response(response, "Token").await {
            Ok(response) => response,
            Err(error) if error.is_retryable() => {
                note_retryable_failure(&mut failed_attempts).await;
                continue;
            }
            Err(error) => return Err(error.into_inner()),
        };
        if !status.is_success() {
            let error = anyhow::anyhow!("GitHub sign-in failed ({status}): {body}");
            if is_retryable_status(status) {
                note_retryable_failure(&mut failed_attempts).await;
                continue;
            }
            return Err(error);
        }
        match schedule.absorb(&body) {
            PollAction::Wait | PollAction::SlowDown => continue,
            PollAction::Failed(message) => return Err(anyhow::anyhow!(message)),
            PollAction::Done {
                access_token,
                scope,
            } => {
                let login = account_login(&access_token).await.ok();
                // A client id from the environment means a developer's own
                // app, so its token is kept in memory rather than overwriting
                // a real connection in the config file.
                if !from_env {
                    let existing = load_at(&client_file_path()?).ok().flatten();
                    let personal_token = existing
                        .as_ref()
                        .and_then(|file| file.personal_token.clone());
                    let poll_interval_secs =
                        existing.and_then(|file| file.poll_interval_secs);
                    save_at(
                        &client_file_path()?,
                        &GithubFile {
                            client_id,
                            access_token: Some(access_token.clone()),
                            personal_token,
                            scope,
                            login,
                            poll_interval_secs,
                            created_at: Some(jiff::Timestamp::now().as_second()),
                        },
                    )?;
                }
                return Ok(access_token);
            }
        }
    }
}

/// The connected account's login, for the integration card's label. Transport
/// failures back off and retry; an unacceptable token fails immediately.
pub async fn account_login(token: &str) -> anyhow::Result<String> {
    let token = token.to_owned();
    retry_connection(move || account_login_request(token.clone())).await
}

async fn account_login_request(token: String) -> Result<String, AttemptError> {
    let response = reqwest::Client::new()
        .get(format!("{API_BASE}/user"))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(reqwest::header::USER_AGENT, "todo-lofi")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| {
            AttemptError::retryable(anyhow::anyhow!("Could not reach GitHub: {error}"))
        })?;
    let (status, body) = read_json_response(response, "Account").await?;
    if !status.is_success() {
        let error = anyhow::anyhow!("Could not read the GitHub account ({status}): {body}");
        if is_retryable_status(status) {
            return Err(AttemptError::retryable(error));
        }
        return Err(AttemptError::permanent(error));
    }
    body.get("login")
        .and_then(|value| value.as_str())
        .filter(|login| !login.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            AttemptError::permanent(anyhow::anyhow!("Account response had no `login`: {body}"))
        })
}

fn stored_credentials_at(path: &std::path::Path) -> Option<StoredCredentials> {
    let file = load_at(path).ok().flatten()?;
    Some(StoredCredentials {
        token: file
            .personal_token
            .clone()
            .or(file.access_token.clone())?,
        login: file.login.clone(),
    })
}

/// The background sync cadence in seconds: `0` means manual syncing only.
/// Defaults to 5 minutes when nothing was ever chosen.
pub fn poll_interval_secs() -> u64 {
    client_file_path()
        .ok()
        .and_then(|path| poll_interval_at(&path))
        .unwrap_or(DEFAULT_POLL_INTERVAL_SECS)
}

/// Default background sync cadence: every 5 minutes when idle.
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 300;

fn poll_interval_at(path: &std::path::Path) -> Option<u64> {
    load_at(path).ok().flatten()?.poll_interval_secs
}

/// Persist the background sync cadence (`0` for manual syncing only),
/// keeping the rest of the connection file.
pub fn set_poll_interval(secs: u64) -> anyhow::Result<()> {
    set_poll_interval_at(&client_file_path()?, secs)
}

fn set_poll_interval_at(path: &std::path::Path, secs: u64) -> anyhow::Result<()> {
    let mut file = load_at(path)?.unwrap_or(GithubFile {
        client_id: String::new(),
        access_token: None,
        personal_token: None,
        scope: None,
        login: None,
        poll_interval_secs: None,
        created_at: None,
    });
    file.poll_interval_secs = Some(secs);
    save_at(path, &file)
}

/// Whether a personal access token is stored, for the integration card.
pub fn has_personal_token() -> bool {
    client_file_path()
        .ok()
        .and_then(|path| load_at(&path).ok().flatten())
        .and_then(|file| file.personal_token)
        .is_some_and(|token| !token.is_empty())
}

/// Store (or clear, when `None` or blank) the personal access token, keeping
/// the rest of the connection file. Resolves the account the token belongs
/// to so the card can label it. Runs on the Tokio runtime.
pub async fn save_personal_token(token: Option<String>) -> anyhow::Result<Option<String>> {
    let token = token.map(|token| token.trim().to_string()).filter(|token| !token.is_empty());
    let path = client_file_path()?;
    let mut file = load_at(&path)?.unwrap_or(GithubFile {
        client_id: String::new(),
        access_token: None,
        personal_token: None,
        scope: None,
        login: None,
        poll_interval_secs: None,
        created_at: None,
    });
    file.personal_token = token.clone();
    file.login = None;
    save_at(&path, &file)?;
    let Some(token) = token else {
        return Ok(None);
    };
    let login = account_login(&token).await.map_err(|e| {
        anyhow::anyhow!("The token was saved but GitHub would not accept it: {e}")
    })?;
    file.login = Some(login.clone());
    save_at(&path, &file)?;
    Ok(Some(login))
}

/// The stored token, when a connection exists. Pure file read, so the app can
/// call it at startup to restore the integration row.
pub fn stored_credentials() -> Option<StoredCredentials> {
    stored_credentials_at(&client_file_path().ok()?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCredentials {
    pub token: String,
    pub login: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn login() -> DeviceLogin {
        DeviceLogin {
            user_code: "ABCD-1234".to_string(),
            verification_uri: "https://github.com/login/device".to_string(),
            device_code: "device-code".to_string(),
            expires_in: 900,
            interval: 5,
        }
    }

    #[test]
    fn device_login_parses_githubs_response() {
        let parsed = parse_device_login(&json!({
            "device_code": "dc",
            "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device",
            "expires_in": 900,
            "interval": 5,
        }))
        .unwrap();
        assert_eq!(parsed.user_code, "ABCD-1234");
        assert_eq!(parsed.interval, 5);
        assert_eq!(parsed.expires_in, 900);
    }

    #[test]
    fn device_login_falls_back_to_the_public_uri_and_defaults() {
        let parsed = parse_device_login(&json!({
            "device_code": "dc",
            "user_code": "ABCD-1234",
        }))
        .unwrap();
        assert_eq!(parsed.verification_uri, "https://github.com/login/device");
        assert_eq!(parsed.interval, 5);
        assert_eq!(parsed.expires_in, 900);
    }

    #[test]
    fn device_login_requires_the_codes() {
        assert!(parse_device_login(&json!({ "user_code": "ABCD" })).is_err());
        assert!(parse_device_login(&json!({ "device_code": "dc" })).is_err());
        // An empty string is not a code.
        assert!(parse_device_login(&json!({ "device_code": "dc", "user_code": "" })).is_err());
    }

    #[test]
    fn polling_waits_until_the_user_approves() {
        let mut schedule = PollSchedule::default();
        assert_eq!(
            schedule.absorb(&json!({ "error": "authorization_pending" })),
            PollAction::Wait
        );
        assert_eq!(
            schedule.absorb(&json!({
                "access_token": "gho_token",
                "scope": "repo,read:user",
            })),
            PollAction::Done {
                access_token: "gho_token".to_string(),
                scope: Some("repo,read:user".to_string()),
            }
        );
    }

    #[test]
    fn slow_down_stretches_every_later_poll() {
        let mut schedule = PollSchedule::new(5);
        assert_eq!(schedule.delay().as_secs(), 5);
        assert_eq!(
            schedule.absorb(&json!({ "error": "slow_down" })),
            PollAction::SlowDown
        );
        assert_eq!(schedule.delay().as_secs(), 10);
        // The increase sticks for the rest of the flow.
        schedule.absorb(&json!({ "error": "slow_down" }));
        assert_eq!(schedule.delay().as_secs(), 15);
    }

    #[test]
    fn terminal_errors_stop_polling_with_a_reason() {
        let mut schedule = PollSchedule::default();
        for (code, expected) in [
            ("expired_token", "expired"),
            ("access_denied", "denied"),
            ("incorrect_client_credentials", "client id"),
        ] {
            match schedule.absorb(&json!({ "error": code })) {
                PollAction::Failed(message) => {
                    assert!(message.contains(expected), "{code}: {message}")
                }
                other => panic!("{code} should stop polling, got {other:?}"),
            }
        }
        // An unknown error and a shapeless body are both failures, never a wait.
        assert!(matches!(
            PollSchedule::default().absorb(&json!({ "error": "teapot" })),
            PollAction::Failed(_)
        ));
        assert!(matches!(
            PollSchedule::default().absorb(&json!({})),
            PollAction::Failed(_)
        ));
    }

    #[test]
    fn connection_backoff_doubles_until_its_ceiling() {
        assert_eq!(connect_backoff_delay(0), Duration::from_secs(1));
        assert_eq!(connect_backoff_delay(1), Duration::from_secs(2));
        assert_eq!(connect_backoff_delay(2), Duration::from_secs(4));
        assert_eq!(connect_backoff_delay(3), Duration::from_secs(8));
        assert_eq!(connect_backoff_delay(10), CONNECT_BACKOFF_MAX);
    }

    #[test]
    fn only_transport_like_statuses_are_retryable() {
        use reqwest::StatusCode;

        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
        ] {
            assert!(is_retryable_status(status), "{status} should back off");
        }
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::NOT_FOUND,
            StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            assert!(!is_retryable_status(status), "{status} needs the user");
        }
    }

    /// The whole file lifecycle, against an explicit path so it never has to
    /// mutate the process-wide config dir.
    #[test]
    fn the_token_file_is_a_connection_only_when_it_holds_a_token() {
        let dir = std::env::temp_dir().join(format!("todo2-github-auth-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let path = dir.join("github.json");

        assert!(stored_credentials_at(&path).is_none(), "nothing stored yet");

        // A file written without a token is not a connection.
        save_at(
            &path,
            &GithubFile {
                client_id: "client".to_string(),
                access_token: None,
                personal_token: None,
                scope: None,
                login: None,
                poll_interval_secs: None,
                created_at: None,
            },
        )
        .unwrap();
        assert!(stored_credentials_at(&path).is_none());

        save_at(
            &path,
            &GithubFile {
                client_id: "client".to_string(),
                access_token: Some("gho_token".to_string()),
                personal_token: None,
                scope: Some(SCOPE.to_string()),
                login: Some("me".to_string()),
                poll_interval_secs: None,
                created_at: Some(7),
            },
        )
        .unwrap();
        let credentials = stored_credentials_at(&path).expect("token round-trips through the file");
        assert_eq!(credentials.token, "gho_token");
        assert_eq!(credentials.login.as_deref(), Some("me"));

        // A personal token wins over the device-flow token everywhere.
        save_at(
            &path,
            &GithubFile {
                client_id: "client".to_string(),
                access_token: Some("gho_token".to_string()),
                personal_token: Some("github_pat_token".to_string()),
                scope: Some(SCOPE.to_string()),
                login: Some("me".to_string()),
                poll_interval_secs: None,
                created_at: Some(7),
            },
        )
        .unwrap();
        let credentials = stored_credentials_at(&path).expect("token round-trips through the file");
        assert_eq!(credentials.token, "github_pat_token");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "the token file must not be world-readable"
            );
        }

        std::fs::remove_file(&path).unwrap();
        assert!(stored_credentials_at(&path).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_client_id_comes_from_the_env_then_the_app_then_the_connection_file() {
        unsafe { std::env::remove_var("GITHUB_CLIENT_ID") };
        let dir = std::env::temp_dir().join(format!("todo2-github-client-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let path = dir.join("github.json");

        if DEFAULT_CLIENT_ID.is_empty() {
            // Until the `lofi-tools` app exists, an env client id is the only
            // way to start a connection, and the error must say so.
            let error = client_id_at(&path).unwrap_err().to_string();
            assert!(error.contains("GITHUB_CLIENT_ID"), "{error}");

            // A stored one keeps a self-build reconnecting without it.
            save_at(
                &path,
                &GithubFile {
                    client_id: "from-file".to_string(),
                    access_token: Some("gho_token".to_string()),
                    personal_token: None,
                    scope: None,
                    login: None,
                    poll_interval_secs: None,
                    created_at: None,
                },
            )
            .unwrap();
            assert_eq!(
                client_id_at(&path).unwrap(),
                ("from-file".to_string(), false)
            );

            // The environment still wins over the file.
            unsafe { std::env::set_var("GITHUB_CLIENT_ID", "from-env") };
            assert_eq!(
                client_id_at(&path).unwrap(),
                ("from-env".to_string(), true)
            );
            unsafe { std::env::remove_var("GITHUB_CLIENT_ID") };
        } else {
            assert_eq!(
                client_id_at(&path).unwrap(),
                (DEFAULT_CLIENT_ID.to_string(), false)
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn a_device_login_is_serializable_for_the_card() {
        // The UI holds this across awaits, so it stays plain data with no
        // borrowed references.
        let login = login();
        let copy = login.clone();
        assert_eq!(copy.device_code, "device-code");
    }

    #[test]
    fn the_sync_cadence_defaults_and_round_trips() {
        let dir =
            std::env::temp_dir().join(format!("todo2-github-interval-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let path = dir.join("github.json");

        assert_eq!(
            poll_interval_at(&path),
            None,
            "nothing chosen yet: the card shows the default"
        );
        set_poll_interval_at(&path, 60).unwrap();
        assert_eq!(poll_interval_at(&path), Some(60));
        set_poll_interval_at(&path, 0).unwrap();
        assert_eq!(poll_interval_at(&path), Some(0), "manual syncing is kept");

        std::fs::remove_dir_all(&dir).ok();
    }
}
