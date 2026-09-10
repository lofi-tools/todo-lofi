//! Todoist OAuth connection.
//!
//! Todoist exposes no device flow (RFC 8628), only the standard
//! authorization-code flow. Connecting therefore opens the Todoist
//! authorize page in the browser and captures the `code` through a
//! loopback redirect (`http://127.0.0.1:<ephemeral-port>/callback`),
//! then exchanges it for an access token.
//!
//! Credentials come from the environment:
//! - `TODOIST_CLIENT_ID` (required)
//! - `TODOIST_CLIENT_SECRET` (required)

use std::time::Duration;

const AUTHORIZE_URL: &str = "https://todoist.com/oauth/authorize";
const TOKEN_URL: &str = "https://todoist.com/oauth/access_token";
const SCOPE: &str = "data:read_write";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
}

impl OAuthConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let client_id = std::env::var("TODOIST_CLIENT_ID").map_err(|_| {
            anyhow::anyhow!("TODOIST_CLIENT_ID is not set (Todoist app settings)")
        })?;
        let client_secret = std::env::var("TODOIST_CLIENT_SECRET").map_err(|_| {
            anyhow::anyhow!("TODOIST_CLIENT_SECRET is not set (Todoist app settings)")
        })?;
        Ok(Self {
            client_id,
            client_secret,
        })
    }
}

fn random_state() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;
    let mut hasher = DefaultHasher::new();
    SystemTime::now().hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    format!("{:016x}{:016x}", hasher.finish(), SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0))
}

/// Run the full connect flow: open the browser, wait for the loopback
/// callback, exchange the code. Returns the access token.
pub async fn connect(config: &OAuthConfig) -> anyhow::Result<String> {
    let listener =
        tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| anyhow::anyhow!("OAuth callback bind failed: {e}"))?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let state = random_state();

    let url = format!(
        "{AUTHORIZE_URL}?client_id={}&scope={SCOPE}&state={state}&redirect_uri={}",
        url_encode(&config.client_id),
        url_encode(&redirect_uri),
    );
    open_browser(&url);

    let code = wait_for_code(listener, &state).await?;
    exchange_code(config, &code, &redirect_uri).await
}

fn url_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", url])
        .spawn();
    tracing::info!("Opened browser for Todoist OAuth: {url}");
}

/// Accept one loopback connection and pull `code`/`state` from the request
/// line. Replies with a page telling the user to return to the app.
async fn wait_for_code(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> anyhow::Result<String> {
    use tokio::io::AsyncReadExt;

    let (mut socket, _) = tokio::time::timeout(CALLBACK_TIMEOUT, listener.accept())
        .await
        .map_err(|_| anyhow::anyhow!("Timed out waiting for the Todoist redirect"))?
        .map_err(|e| anyhow::anyhow!("OAuth callback accept failed: {e}"))?;

    let mut buf = vec![0u8; 8192];
    let read = tokio::time::timeout(CALLBACK_TIMEOUT, socket.read(&mut buf))
        .await
        .map_err(|_| anyhow::anyhow!("Timed out reading the Todoist redirect"))?
        .map_err(|e| anyhow::anyhow!("OAuth callback read failed: {e}"))?;
    let request = String::from_utf8_lossy(&buf[..read]);
    let request_line = request.lines().next().unwrap_or_default();
    let target = request_line.split_whitespace().nth(1).unwrap_or_default();

    let query = target.split_once('?').map(|(_, q)| q).unwrap_or_default();
    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut error: Option<String> = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "code" => code = Some(url_decode(value)),
            "state" => state = Some(url_decode(value)),
            "error" => error = Some(url_decode(value)),
            _ => {}
        }
    }

    let code = match (code, state, error) {
        (Some(code), Some(state), _) if state == expected_state => {
            respond(&mut socket, "200 OK", "Connected! You can return to the app.").await?;
            code
        }
        (_, _, Some(message)) => {
            let _ = respond(&mut socket, "400 Bad Request", &message).await;
            return Err(anyhow::anyhow!("Todoist authorization failed: {message}"));
        }
        _ => {
            let _ =
                respond(&mut socket, "400 Bad Request", "Missing code or state.").await;
            return Err(anyhow::anyhow!("Todoist redirect was missing code/state"));
        }
    };
    Ok(code)
}

async fn respond(
    socket: &mut tokio::net::TcpStream,
    status: &str,
    body: &str,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let page = format!(
        "<html><body style=\"font-family:sans-serif;padding:2em\"><h1>{body}</h1></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        page.len(),
        page
    );
    socket
        .write_all(response.as_bytes())
        .await
        .map_err(|e| anyhow::anyhow!("OAuth callback reply failed: {e}"))?;
    Ok(())
}

fn url_decode(raw: &str) -> String {
    let mut out = Vec::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = &raw[index + 1..index + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => out.push(byte),
                    Err(_) => out.extend_from_slice(&bytes[index..index + 3]),
                }
                index += 3;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn exchange_code(
    config: &OAuthConfig,
    code: &str,
    redirect_uri: &str,
) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let body = format!(
        "client_id={}&client_secret={}&code={}&redirect_uri={}",
        url_encode(&config.client_id),
        url_encode(&config.client_secret),
        url_encode(code),
        url_encode(redirect_uri),
    );
    let response = client
        .post(TOKEN_URL)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Token exchange failed: {e}"))?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Token response was not JSON: {e}"))?;
    body.get("access_token")
        .and_then(|token| token.as_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Token exchange failed ({status}): {body}"))
}
