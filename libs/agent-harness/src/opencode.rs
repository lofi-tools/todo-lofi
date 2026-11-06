//! OpenCode's private HTTP server and session control.

use std::collections::{BTreeMap, VecDeque};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use reqwest::{Client, Method, RequestBuilder, Response, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::timeout;

use crate::{HarnessError, HarnessResult, SessionId, SpawnSpec};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const USERNAME: &str = "opencode";

#[derive(Deserialize)]
struct Settings {
    #[serde(default)]
    app: AppSettings,
}

#[derive(Default, Deserialize)]
struct AppSettings {
    #[serde(rename = "agents.opencode.executable")]
    executable: Option<PathBuf>,
}

/// Read the configured executable without resolving aliases, modifying the
/// settings file, or installing a binary.
pub fn configured_executable(settings_path: &Path) -> HarnessResult<Option<PathBuf>> {
    let bytes = match std::fs::read(settings_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(HarnessError::Protocol(format!(
                "could not read OpenCode settings at {}: {error}",
                settings_path.display()
            )));
        }
    };
    let settings: Settings = serde_json::from_slice(&bytes).map_err(|error| {
        HarnessError::Protocol(format!(
            "could not parse OpenCode settings at {}: {error}",
            settings_path.display()
        ))
    })?;
    if let Some(executable) = &settings.app.executable {
        require_absolute_executable(executable)?;
    }
    Ok(settings.app.executable)
}

fn require_absolute_executable(path: &Path) -> HarnessResult<()> {
    if !path.is_absolute() {
        return Err(HarnessError::Protocol(
            "agents.opencode.executable must be an absolute path".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCodeProbe {
    pub version: String,
    pub resume: bool,
}

#[derive(Deserialize)]
struct Health {
    version: String,
}

#[derive(Deserialize)]
struct Api {
    paths: BTreeMap<String, BTreeMap<String, Value>>,
}

impl Api {
    fn supports(&self, method: &str, path: &str) -> bool {
        self.paths
            .get(path)
            .is_some_and(|operations| operations.get(method).is_some_and(Value::is_object))
    }

    fn probe(&self, health: Health) -> HarnessResult<OpenCodeProbe> {
        if health.version.is_empty() {
            return Err(HarnessError::Protocol(
                "OpenCode did not report a healthy versioned server".into(),
            ));
        }
        for (method, path) in [
            ("get", "/api/event"),
            ("post", "/api/session"),
            ("post", "/api/session/{sessionID}/prompt"),
            ("post", "/api/session/{sessionID}/interrupt"),
            (
                "post",
                "/api/session/{sessionID}/permission/{requestID}/reply",
            ),
        ] {
            if !self.supports(method, path) {
                return Err(HarnessError::Protocol(format!(
                    "OpenCode server does not support {} {path}",
                    method.to_ascii_uppercase()
                )));
            }
        }
        Ok(OpenCodeProbe {
            version: health.version,
            resume: self.supports("get", "/api/session/{sessionID}"),
        })
    }
}

#[derive(Deserialize)]
struct SessionRecord {
    id: String,
    location: SessionLocation,
}

#[derive(Deserialize)]
struct SessionLocation {
    directory: PathBuf,
}

#[derive(Deserialize)]
struct ResponseData<T> {
    data: T,
}

#[derive(Deserialize)]
struct InterruptResponse {
    interrupted: bool,
}

/// A loopback server owned by one Shepherd task.
pub struct OpenCodeServer {
    child: Child,
    client: Client,
    address: Url,
    password: String,
    directory: String,
    probe: OpenCodeProbe,
}

#[derive(Debug, Deserialize)]
pub struct OpenCodeEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub data: Value,
}

pub struct OpenCodeEvents {
    response: Response,
    decoder: EventDecoder,
    pending: VecDeque<HarnessResult<OpenCodeEvent>>,
}

impl OpenCodeEvents {
    pub async fn next(&mut self) -> HarnessResult<OpenCodeEvent> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return event;
            }
            let chunk = self.response.chunk().await.map_err(|error| {
                protocol_error("could not read OpenCode event stream", error.without_url())
            })?;
            let chunk = chunk.ok_or_else(|| {
                HarnessError::Protocol("OpenCode event stream disconnected".into())
            })?;
            if let Err(error) = self.decoder.push(&chunk, &mut self.pending) {
                self.pending.push_back(Err(error));
            }
        }
    }
}

#[derive(Default)]
struct EventDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
}

impl EventDecoder {
    fn push(
        &mut self,
        chunk: &[u8],
        events: &mut VecDeque<HarnessResult<OpenCodeEvent>>,
    ) -> HarnessResult<()> {
        for fragment in chunk.split_inclusive(|byte| *byte == b'\n') {
            if self.line.len().saturating_add(fragment.len()) > MAX_RESPONSE_BYTES {
                return Err(HarnessError::Protocol(
                    "OpenCode event line is too large".into(),
                ));
            }
            self.line.extend_from_slice(fragment);
            if !self.line.ends_with(b"\n") {
                continue;
            }
            let line = self.line.strip_suffix(b"\n").unwrap_or(&self.line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() && !self.data.is_empty() {
                let event = serde_json::from_slice(&self.data).map_err(|error| {
                    protocol_error("could not parse OpenCode stream event", error)
                })?;
                events.push_back(Ok(event));
                self.data.clear();
            } else if let Some(data) = line.strip_prefix(b"data:") {
                let data = data.strip_prefix(b" ").unwrap_or(data);
                if self.data.len().saturating_add(data.len()).saturating_add(1) > MAX_RESPONSE_BYTES
                {
                    return Err(HarnessError::Protocol("OpenCode event is too large".into()));
                }
                self.data.extend_from_slice(data);
                self.data.push(b'\n');
            } else if line == b"event: effect/httpapi/stream/failure" {
                return Err(HarnessError::Protocol(
                    "OpenCode reported an event stream failure".into(),
                ));
            }
            self.line.clear();
        }
        Ok(())
    }
}

impl OpenCodeServer {
    pub async fn start(spec: &SpawnSpec) -> HarnessResult<Self> {
        require_absolute_executable(Path::new(&spec.program))?;
        let directory = tokio::fs::canonicalize(&spec.cwd)
            .await
            .map_err(|error| {
                HarnessError::Protocol(format!("invalid OpenCode directory: {error}"))
            })?
            .into_os_string()
            .into_string()
            .map_err(|_| HarnessError::Protocol("OpenCode requires a UTF-8 directory".into()))?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| protocol_error("could not configure OpenCode HTTP client", error))?;
        let password = uuid::Uuid::new_v4().to_string();
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .args(["serve", "--hostname", "127.0.0.1", "--port", "0"])
            .current_dir(&directory)
            .envs(&spec.environment)
            .env("OPENCODE_PASSWORD", &password)
            .env("OPENCODE_SERVER_PASSWORD", &password)
            .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| HarnessError::Spawn {
                program: spec.program.clone(),
                source,
            })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            HarnessError::Protocol("OpenCode did not provide a startup pipe".into())
        })?;
        let mut startup = BufReader::new(stdout).take(64 * 1024).lines();
        let address = timeout(STARTUP_TIMEOUT, async {
            while let Some(line) = startup
                .next_line()
                .await
                .map_err(|error| protocol_error("could not read OpenCode startup", error))?
            {
                if let Some(address) = line.trim().strip_prefix("server listening on ") {
                    return loopback_address(address);
                }
            }
            Err(HarnessError::Protocol(
                "OpenCode did not announce a loopback server".into(),
            ))
        })
        .await
        .map_err(|_| HarnessError::Protocol("OpenCode server startup timed out".into()))??;
        let mut stdout = startup.into_inner().into_inner();
        tokio::spawn(async move {
            if let Err(error) = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await {
                tracing::debug!(%error, "could not drain OpenCode stdout");
            }
        });
        let request = |path: &str| {
            client
                .get(format!("{}{path}", address.as_str().trim_end_matches('/')))
                .basic_auth(USERNAME, Some(&password))
        };
        let health = response_json(request("/api/info")).await?;
        let api: Api = response_json(request("/openapi.json")).await?;
        let probe = api.probe(health)?;
        Ok(Self {
            child,
            client,
            address,
            password,
            directory,
            probe,
        })
    }

    pub fn probe(&self) -> &OpenCodeProbe {
        &self.probe
    }

    pub async fn subscribe(&self) -> HarnessResult<OpenCodeEvents> {
        let handshake = async {
            let response = self
                .request(Method::GET, "/api/event")
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .send()
                .await
                .and_then(Response::error_for_status)
                .map_err(|error| {
                    protocol_error(
                        "could not subscribe to OpenCode events",
                        error.without_url(),
                    )
                })?;
            let content_type = response.headers().get(reqwest::header::CONTENT_TYPE);
            if !content_type
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.split(';').next() == Some("text/event-stream"))
            {
                return Err(HarnessError::Protocol(
                    "OpenCode did not return an event stream".into(),
                ));
            }
            let mut events = OpenCodeEvents {
                response,
                decoder: EventDecoder::default(),
                pending: VecDeque::new(),
            };
            if events.next().await?.kind != "server.connected" {
                return Err(HarnessError::Protocol(
                    "OpenCode did not acknowledge the event stream".into(),
                ));
            }
            Ok(events)
        };
        timeout(REQUEST_TIMEOUT, handshake).await.map_err(|_| {
            HarnessError::Protocol("OpenCode event stream handshake timed out".into())
        })?
    }

    pub async fn create_session(&self) -> HarnessResult<SessionId> {
        let response: ResponseData<SessionRecord> =
            response_json(self.request(Method::POST, "/api/session").json(&json!({
                "location": { "directory": self.directory },
                "permissions": [{ "action": "*", "resource": "*", "effect": "ask" }]
            })))
            .await?;
        self.session_id(response.data)
    }

    pub async fn resume_session(&self, session: &SessionId) -> HarnessResult<SessionId> {
        if !self.probe.resume {
            return Err(HarnessError::Protocol(
                "OpenCode server does not support session lookup".into(),
            ));
        }
        let path = session_path(session)?;
        let response: ResponseData<SessionRecord> =
            response_json(self.request(Method::GET, &path)).await?;
        let resumed = self.session_id(response.data)?;
        if &resumed != session {
            return Err(HarnessError::Protocol(
                "OpenCode returned a different session".into(),
            ));
        }
        Ok(resumed)
    }

    pub async fn interrupt_session(&self, session: &SessionId) -> HarnessResult<bool> {
        let path = format!("{}/interrupt", session_path(session)?);
        let response: InterruptResponse = response_json(self.request(Method::POST, &path)).await?;
        Ok(response.interrupted)
    }

    pub async fn shutdown(mut self) -> HarnessResult<()> {
        self.child
            .kill()
            .await
            .map_err(|error| protocol_error("could not stop OpenCode server", error))
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(
                method,
                format!("{}{path}", self.address.as_str().trim_end_matches('/')),
            )
            .basic_auth(USERNAME, Some(&self.password))
    }

    fn session_id(&self, record: SessionRecord) -> HarnessResult<SessionId> {
        if record.location.directory != Path::new(&self.directory) {
            return Err(HarnessError::Protocol(
                "OpenCode returned a session outside the task directory".into(),
            ));
        }
        let session = SessionId::new(record.id);
        session_path(&session)?;
        Ok(session)
    }
}

fn loopback_address(address: &str) -> HarnessResult<Url> {
    let address = Url::parse(address)
        .map_err(|error| protocol_error("OpenCode returned an invalid listener address", error))?;
    if address.scheme() != "http"
        || address.host_str() != Some("127.0.0.1")
        || !address.port().is_some_and(|port| port != 0)
        || !address.username().is_empty()
        || address.password().is_some()
        || address.path() != "/"
        || address.query().is_some()
        || address.fragment().is_some()
    {
        return Err(HarnessError::Protocol(
            "OpenCode did not bind an HTTP loopback port".into(),
        ));
    }
    Ok(address)
}

fn session_path(session: &SessionId) -> HarnessResult<String> {
    let id = session.as_str();
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
    {
        return Err(HarnessError::Protocol("invalid OpenCode session ID".into()));
    }
    Ok(format!("/api/session/{id}"))
}

async fn response_json<T: DeserializeOwned>(request: RequestBuilder) -> HarnessResult<T> {
    let mut response = request
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| protocol_error("OpenCode HTTP request failed", error.without_url()))?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| protocol_error("could not read OpenCode response", error.without_url()))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(HarnessError::Protocol(
                "OpenCode response is too large".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| protocol_error("could not parse OpenCode response", error))
}

fn protocol_error(context: &str, error: impl std::fmt::Display) -> HarnessError {
    HarnessError::Protocol(format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_explicit_opencode_executable() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let settings = directory.path().join("settings.json");
        let executable = directory.path().join("profile/bin/opencode");
        assert_eq!(configured_executable(&settings)?, None);
        std::fs::write(
            &settings,
            serde_json::to_vec(&json!({
                "version": 1,
                "app": { "agents.opencode.executable": executable, "theme": "dark" }
            }))?,
        )?;
        assert_eq!(configured_executable(&settings)?, Some(executable));
        std::fs::write(
            &settings,
            r#"{"app":{"agents.opencode.executable":"opencode"}}"#,
        )?;
        assert!(configured_executable(&settings).is_err());
        Ok(())
    }

    #[test]
    fn requires_the_server_to_advertise_permission_replies()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut api: Api = serde_json::from_value(json!({"paths": {
            "/api/event": {"get": {}},
            "/api/session": {"post": {}},
            "/api/session/{sessionID}/prompt": {"post": {}},
            "/api/session/{sessionID}/interrupt": {"post": {}}
        }}))?;
        let health = || Health {
            version: "fixture".into(),
        };
        let error = api
            .probe(health())
            .expect_err("permission handling is required");
        assert!(error.to_string().contains("/permission/{requestID}/reply"));
        api.paths.insert(
            "/api/session/{sessionID}/permission/{requestID}/reply".into(),
            BTreeMap::from([("post".into(), json!({}))]),
        );
        assert!(!api.probe(health())?.resume);
        api.paths.insert(
            "/api/session/{sessionID}".into(),
            BTreeMap::from([("get".into(), json!({}))]),
        );
        assert!(api.probe(health())?.resume);
        Ok(())
    }

    #[test]
    fn decodes_split_event_lines_and_preserves_events_before_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut decoder = EventDecoder::default();
        let mut events = VecDeque::new();
        for byte in ": heartbeat\n\r\ndata: {\"type\":\"session.text.delta\",\r\n\
                     data: \"data\":{\"delta\":\"héllo\"}}\r\n\r\n"
            .as_bytes()
            .chunks(1)
        {
            decoder.push(byte, &mut events)?;
        }
        let error = decoder.push(
            b"data: {\"type\":\"session.execution.succeeded\",\"data\":{}}\n\n\
              event: effect/httpapi/stream/failure\n",
            &mut events,
        );
        let text = events.pop_front().ok_or("expected text event")??;
        assert_eq!(text.kind, "session.text.delta");
        assert_eq!(text.data, json!({"delta": "héllo"}));
        let finished = events.pop_front().ok_or("expected execution event")??;
        assert_eq!(finished.kind, "session.execution.succeeded");
        assert!(events.is_empty());
        assert!(error.is_err());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires a configured OpenCode executable and a loopback socket"]
    async fn installed_server_creates_and_resumes_a_session()
    -> Result<(), Box<dyn std::error::Error>> {
        let settings = PathBuf::from(
            std::env::var_os("SHEPHERD_OPENCODE_TEST_SETTINGS")
                .ok_or("set SHEPHERD_OPENCODE_TEST_SETTINGS to the path of a settings.json file")?,
        );
        let executable =
            configured_executable(&settings)?.ok_or("configure agents.opencode.executable")?;
        let root = tempfile::tempdir()?;
        let project = root.path().join("project");
        std::fs::create_dir(&project)?;
        let mut environment = BTreeMap::new();
        for (key, child) in [
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_STATE_HOME", "state"),
        ] {
            environment.insert(
                key.into(),
                root.path().join(child).to_string_lossy().into_owned(),
            );
        }
        environment.insert("OPENCODE_DISABLE_MODELS_FETCH".into(), "1".into());
        let spec = SpawnSpec {
            program: executable
                .into_os_string()
                .into_string()
                .map_err(|_| "OpenCode path must be UTF-8")?,
            args: Vec::new(),
            cwd: project,
            environment,
        };
        let server = OpenCodeServer::start(&spec).await?;
        assert!(server.probe().resume);
        let mut events = server.subscribe().await?;
        let session = server.create_session().await?;
        let created = timeout(REQUEST_TIMEOUT, async {
            loop {
                let event = events.next().await?;
                if event.kind == "session.created" {
                    return Ok::<_, HarnessError>(event);
                }
            }
        })
        .await??;
        assert_eq!(created.data["sessionID"], session.as_str());
        server.shutdown().await?;

        let restarted = OpenCodeServer::start(&spec).await?;
        assert_eq!(restarted.resume_session(&session).await?, session);
        assert!(!restarted.interrupt_session(&session).await?);
        restarted.shutdown().await?;
        Ok(())
    }
}
