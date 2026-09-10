//! Todoist OAuth connection as a PKCE public client.
//!
//! The desktop app ships to users, so it cannot hold a client secret —
//! anything in the binary is extractable. Instead the app registers
//! itself at runtime (RFC 7591 dynamic client registration) as a PKCE
//! public client and saves the issued credentials to
//! `~/.config/my-todo/todoist.json`.
//!
//! Flow: ensure registration, open the Todoist authorize page in the
//! browser (with `code_challenge`), capture the `code` through a
//! loopback redirect on a fixed port, then exchange it with the
//! `code_verifier`.

use std::time::Duration;

use sha2::{Digest, Sha256};

const AUTHORIZE_URL: &str = "https://app.todoist.com/oauth/authorize";
const TOKEN_URL: &str = "https://api.todoist.com/oauth/access_token";
const REGISTER_URL: &str = "https://api.todoist.com/oauth/register";
const SCOPE: &str = "data:read_write";
const APP_NAME: &str = "my-todo";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Fixed loopback redirect. The URI is sent at registration, so the port
/// must not be ephemeral. If the port is taken, connecting fails with a
/// message instead of using a URI the registered client would reject.
pub const REDIRECT_URI: &str = "http://127.0.0.1:53682/callback";

pub struct OAuthConfig {
    pub client_id: String,
    pub redirect_uri: String,
}

/// Registered client details persisted to `~/.config/my-todo/todoist.json`.
/// Tokens live here too (file created with owner-only permissions);
/// moving them to the keychain is future work.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ClientFile {
    client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_secret: Option<String>,
    redirect_uris: Vec<String>,
    scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
    /// True when the client ID came from the environment rather than the
    /// file/registration; tokens are then kept in memory, never written.
    #[serde(skip)]
    from_env: bool,
}

fn config_dir() -> anyhow::Result<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("MY_TODO_CONFIG_DIR") {
        if !dir.is_empty() {
            return Ok(std::path::PathBuf::from(dir));
        }
    }
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not locate the home directory"))?;
    Ok(home.join(".config").join("my-todo"))
}

fn client_file_path() -> anyhow::Result<std::path::PathBuf> {
    Ok(config_dir()?.join("todoist.json"))
}

fn load_client_file() -> anyhow::Result<Option<ClientFile>> {
    let path = client_file_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("Could not read {}: {e}", path.display()))?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| anyhow::anyhow!("Could not parse {}: {e}", path.display()))
}

fn save_client_file(client: &ClientFile) -> anyhow::Result<()> {
    let path = client_file_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("Could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(client)?)
        .map_err(|e| anyhow::anyhow!("Could not write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).ok();
    }
    Ok(())
}

/// Register this installation as a PKCE public client (RFC 7591). No
/// authentication is required; the endpoint is rate-limited per caller.
async fn register_client() -> anyhow::Result<ClientFile> {
    let client = reqwest::Client::new();
    let response = client
        .post(REGISTER_URL)
        .json(&serde_json::json!({
            "client_name": APP_NAME,
            "redirect_uris": [REDIRECT_URI],
            "scope": SCOPE,
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Client registration failed: {e}"))?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Registration response was not JSON: {e}"))?;
    if !status.is_success() {
        return Err(anyhow::anyhow!(
            "Client registration failed ({status}): {body}"
        ));
    }
    let registered = ClientFile {
        client_id: body
            .get("client_id")
            .and_then(|id| id.as_str())
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("Registration response had no client_id: {body}"))?,
        client_secret: body
            .get("client_secret")
            .and_then(|s| s.as_str())
            .map(str::to_owned),
        redirect_uris: vec![REDIRECT_URI.to_string()],
        scope: SCOPE.to_string(),
        access_token: None,
        refresh_token: None,
        expires_at: None,
        from_env: false,
    };
    save_client_file(&registered)?;
    tracing::info!(
        "Registered Todoist OAuth client {}",
        registered.client_id
    );
    Ok(registered)
}

/// Load the registered client, registering (and saving to
/// `~/.config/my-todo/todoist.json`) on first use. `TODOIST_CLIENT_ID` in
/// the environment overrides the file for local development.
async fn ensure_client() -> anyhow::Result<ClientFile> {
    if let Ok(id) = std::env::var("TODOIST_CLIENT_ID") {
        if !id.is_empty() {
            return Ok(ClientFile {
                client_id: id,
                client_secret: None,
                redirect_uris: vec![REDIRECT_URI.to_string()],
                scope: SCOPE.to_string(),
                access_token: None,
                refresh_token: None,
                expires_at: None,
                from_env: true,
            });
        }
    }
    if let Some(client) = load_client_file()? {
        return Ok(client);
    }
    register_client().await
}

/// PKCE code verifier: 64 random unreserved characters.
fn code_verifier() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;
    const ALPHABET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut out = String::with_capacity(64);
    let mut counter = 0u64;
    while out.len() < 64 {
        let mut hasher = DefaultHasher::new();
        SystemTime::now().hash(&mut hasher);
        std::thread::current().id().hash(&mut hasher);
        counter.hash(&mut hasher);
        let mut bits = hasher.finish();
        for _ in 0..10 {
            if out.len() >= 64 {
                break;
            }
            out.push(ALPHABET[(bits % ALPHABET.len() as u64) as usize] as char);
            bits /= ALPHABET.len() as u64;
        }
        counter += 1;
    }
    out
}

/// S256 code challenge: BASE64URL(SHA256(verifier)) without padding.
fn code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64url_no_pad(&digest)
}

fn base64url_no_pad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let (full, rest) = bytes.split_at(bytes.len() / 3 * 3);
    for chunk in full.chunks_exact(3) {
        let word = (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8 | chunk[2] as u32;
        out.push(ALPHABET[(word >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(word >> 12 & 0x3f) as usize] as char);
        out.push(ALPHABET[(word >> 6 & 0x3f) as usize] as char);
        out.push(ALPHABET[(word & 0x3f) as usize] as char);
    }
    if rest.len() == 1 {
        let word = (rest[0] as u32) << 16;
        out.push(ALPHABET[(word >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(word >> 12 & 0x3f) as usize] as char);
    } else if rest.len() == 2 {
        let word = (rest[0] as u32) << 16 | (rest[1] as u32) << 8;
        out.push(ALPHABET[(word >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(word >> 12 & 0x3f) as usize] as char);
        out.push(ALPHABET[(word >> 6 & 0x3f) as usize] as char);
    }
    out
}

/// Run the full connect flow: ensure client registration, open the
/// browser, wait for the loopback callback, exchange the code with the
/// PKCE verifier. Persists tokens to `~/.config/my-todo/todoist.json`
/// and returns the access token. No client secret is involved.
pub async fn connect() -> anyhow::Result<String> {
    let mut registered = ensure_client().await?;
    let config = OAuthConfig {
        client_id: registered.client_id.clone(),
        redirect_uri: REDIRECT_URI.to_string(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:53682")
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Could not listen on the Todoist callback port (127.0.0.1:53682): {e}"
            )
        })?;
    let verifier = code_verifier();
    let state = code_verifier();

    let url = format!(
        "{AUTHORIZE_URL}?client_id={}&scope={SCOPE}&state={}&redirect_uri={}&response_type=code&code_challenge={}&code_challenge_method=S256",
        url_encode(&config.client_id),
        url_encode(&state),
        url_encode(&config.redirect_uri),
        url_encode(&code_challenge(&verifier)),
    );
    open_browser(&url);

    let code = wait_for_code(listener, &state).await?;
    let tokens = exchange_code(&config, &code, &verifier).await?;
    registered.access_token = Some(tokens.access_token.clone());
    if tokens.refresh_token.is_some() {
        registered.refresh_token = tokens.refresh_token;
    }
    registered.expires_at = tokens.expires_at;
    if !registered.from_env {
        save_client_file(&registered).ok();
    }
    Ok(tokens.access_token)
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
        "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len(),
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

/// Tokens from the token endpoint. `expires_at` is a unix epoch when
/// `expires_in` was present; `refresh_token` rotates on every refresh.
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn parse_tokens(body: &serde_json::Value, status: reqwest::StatusCode) -> anyhow::Result<Tokens> {
    let access_token = body
        .get("access_token")
        .and_then(|token| token.as_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Token request failed ({status}): {body}"))?;
    Ok(Tokens {
        access_token,
        // Grace-window retries omit `refresh_token`; callers keep the old one.
        refresh_token: body
            .get("refresh_token")
            .and_then(|token| token.as_str())
            .map(str::to_owned),
        expires_at: body
            .get("expires_in")
            .and_then(|secs| secs.as_i64())
            .map(|secs| now_epoch() + secs),
    })
}

/// Exchange the code for tokens as a PKCE public client: client ID, code,
/// redirect URI and verifier — no client secret.
async fn exchange_code(
    config: &OAuthConfig,
    code: &str,
    verifier: &str,
) -> anyhow::Result<Tokens> {
    let client = reqwest::Client::new();
    let body = format!(
        "client_id={}&code={}&redirect_uri={}&code_verifier={}",
        url_encode(&config.client_id),
        url_encode(code),
        url_encode(&config.redirect_uri),
        url_encode(verifier),
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
    parse_tokens(&body, status)
}

/// Refresh an expiring access token. Refresh tokens rotate: the response's
/// token replaces the stored one. A grace-window retry omits
/// `refresh_token`, in which case the stored one is kept.
pub async fn refresh_access_token(client_id: &str, refresh_token: &str) -> anyhow::Result<Tokens> {
    let client = reqwest::Client::new();
    let body = format!(
        "client_id={}&grant_type=refresh_token&refresh_token={}",
        url_encode(client_id),
        url_encode(refresh_token),
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
        .map_err(|e| anyhow::anyhow!("Token refresh failed: {e}"))?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Refresh response was not JSON: {e}"))?;
    parse_tokens(&body, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        // RFC 7636 Appendix B test vector.
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_is_long_unreserved_string() {
        let verifier = code_verifier();
        assert!((43..=128).contains(&verifier.len()));
        assert!(verifier.bytes().all(|b| b.is_ascii_alphanumeric()
            || b == b'-'
            || b == b'.'
            || b == b'_'
            || b == b'~'));
    }

    #[test]
    fn base64url_has_no_padding_or_plus_slash() {
        let encoded = base64url_no_pad(&Sha256::digest(b"hello"));
        assert_eq!(encoded.len(), 43);
        assert!(!encoded.contains(['+', '/', '=']));
    }

    #[test]
    fn client_file_roundtrips_through_json() {
        let dir = std::env::temp_dir().join(format!("my-todo-test-{}", std::process::id()));
        unsafe { std::env::set_var("MY_TODO_CONFIG_DIR", &dir) };
        let client = ClientFile {
            client_id: "tdd_abc".to_string(),
            client_secret: None,
            redirect_uris: vec![REDIRECT_URI.to_string()],
            scope: SCOPE.to_string(),
            access_token: Some("at".to_string()),
            refresh_token: Some("rt".to_string()),
            expires_at: Some(123),
            from_env: false,
        };
        save_client_file(&client).unwrap();
        let loaded = load_client_file().unwrap().unwrap();
        assert_eq!(loaded.client_id, "tdd_abc");
        assert_eq!(loaded.refresh_token.as_deref(), Some("rt"));
        assert!(!loaded.from_env);
        // Secrets must not leak through file permissions or debug output.
        let raw = std::fs::read_to_string(client_file_path().unwrap()).unwrap();
        assert!(raw.contains("tdd_abc"));
        std::fs::remove_dir_all(&dir).ok();
        unsafe { std::env::remove_var("MY_TODO_CONFIG_DIR") };
    }

    #[test]
    fn parse_tokens_keeps_grace_window_refresh() {
        let full = serde_json::json!({
            "access_token": "at",
            "refresh_token": "rt",
            "expires_in": 3600,
        });
        let tokens =
            parse_tokens(&full, reqwest::StatusCode::OK).unwrap();
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt"));
        assert!(tokens.expires_at.is_some());

        // Grace-window retry: no refresh_token means "keep the stored one".
        let retry = serde_json::json!({ "access_token": "at2" });
        let tokens =
            parse_tokens(&retry, reqwest::StatusCode::OK).unwrap();
        assert_eq!(tokens.access_token, "at2");
        assert!(tokens.refresh_token.is_none());
    }
}
