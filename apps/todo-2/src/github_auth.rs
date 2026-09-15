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

use std::time::Duration;

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
pub const API_BASE: &str = "https://api.github.com";

/// Private repos plus writing issues, labels and pull requests; `read:user`
/// only names the connected account in the UI.
const SCOPE: &str = "repo read:user";

/// Client id of the app registered under the `lofi-tools` org with device flow
/// enabled (spec decision 34). Public by design — device flow needs no secret.
/// Empty until that app exists, in which case `GITHUB_CLIENT_ID` must supply
/// one (forks and self-builds use their own app).
const DEFAULT_CLIENT_ID: &str = "";

/// How much GitHub asks us to add to the interval on `slow_down`.
const SLOW_DOWN_STEP: u64 = 5;

/// Registered connection persisted to `~/.config/my-todo/github.json`. Tokens
/// live here too (owner-only permissions), matching the Todoist file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct GithubFile {
    client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    /// The account the token belongs to, for the integration card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    login: Option<String>,
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

/// Begin a device flow and return the code for the user to enter.
pub async fn begin() -> anyhow::Result<DeviceLogin> {
    let (client_id, _) = configured_client_id()?;
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
        .map_err(|e| anyhow::anyhow!("Could not reach GitHub: {e}"))?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Device flow response was not JSON: {e}"))?;
    if !status.is_success() {
        return Err(anyhow::anyhow!(
            "GitHub device flow failed ({status}): {body}"
        ));
    }
    parse_device_login(&body)
}

/// Poll until the user approves, then persist the token and return the login.
/// Runs on the Tokio runtime; never on GPUI's executor.
pub async fn complete(login: DeviceLogin) -> anyhow::Result<String> {
    let (client_id, from_env) = configured_client_id()?;
    let mut schedule = PollSchedule::new(login.interval);
    let client = reqwest::Client::new();
    loop {
        tokio::time::sleep(schedule.delay()).await;
        let body = format!(
            "client_id={}&device_code={}&grant_type={}",
            crate::todoist_auth::url_encode(&client_id),
            crate::todoist_auth::url_encode(&login.device_code),
            crate::todoist_auth::url_encode("urn:ietf:params:oauth:grant-type:device_code"),
        );
        let response = client
            .post(TOKEN_URL)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Could not reach GitHub: {e}"))?;
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Token response was not JSON: {e}"))?;
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
                    save_at(
                        &client_file_path()?,
                        &GithubFile {
                            client_id,
                            access_token: Some(access_token.clone()),
                            scope,
                            login,
                            created_at: Some(jiff::Timestamp::now().as_second()),
                        },
                    )?;
                }
                return Ok(access_token);
            }
        }
    }
}

/// The connected account's login, for the integration card's label.
pub async fn account_login(token: &str) -> anyhow::Result<String> {
    let response = reqwest::Client::new()
        .get(format!("{API_BASE}/user"))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(reqwest::header::USER_AGENT, "todo-lofi")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Could not reach GitHub: {e}"))?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Account response was not JSON: {e}"))?;
    if !status.is_success() {
        return Err(anyhow::anyhow!(
            "Could not read the GitHub account ({status}): {body}"
        ));
    }
    body.get("login")
        .and_then(|value| value.as_str())
        .filter(|login| !login.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Account response had no `login`: {body}"))
}

fn stored_credentials_at(path: &std::path::Path) -> Option<StoredCredentials> {
    let file = load_at(path).ok().flatten()?;
    Some(StoredCredentials {
        token: file.access_token.clone()?,
        login: file.login.clone(),
    })
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

/// Forget the connection (the integration row is removed by the caller).
pub fn disconnect() -> anyhow::Result<()> {
    let path = client_file_path()?;
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| anyhow::anyhow!("Could not remove {}: {e}", path.display()))?;
    }
    Ok(())
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
                scope: None,
                login: None,
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
                scope: Some(SCOPE.to_string()),
                login: Some("me".to_string()),
                created_at: Some(7),
            },
        )
        .unwrap();
        let credentials = stored_credentials_at(&path).expect("token round-trips through the file");
        assert_eq!(credentials.token, "gho_token");
        assert_eq!(credentials.login.as_deref(), Some("me"));

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
                    scope: None,
                    login: None,
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
}
