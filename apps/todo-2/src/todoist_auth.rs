//! Todoist OAuth connection as a PKCE public client.
//!
//! The desktop app ships to users, so it cannot hold a client secret —
//! anything in the binary is extractable. The client ID is public by
//! design and hardcoded below; authentication uses PKCE
//! (`token_endpoint_auth_method: none`) instead of a secret, which is
//! the flow Todoist documents for desktop apps.
//!
//! Flow: open the Todoist authorize page in the browser (with
//! `code_challenge`), capture the `code` through a loopback redirect on
//! a fixed port, then exchange it with the `code_verifier`.

use std::time::Duration;

use sha2::{Digest, Sha256};

const AUTHORIZE_URL: &str = "https://todoist.com/oauth/authorize";
const TOKEN_URL: &str = "https://todoist.com/oauth/access_token";
const SCOPE: &str = "data:read_write";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Todoist client ID. Public identifier, safe to distribute — but replace
/// the placeholder with the real ID from the App Management Console
/// (registered as a PKCE public client) before release. `TODOIST_CLIENT_ID`
/// in the environment overrides it for local development.
pub const TODOIST_CLIENT_ID: &str = "YOUR_TODOIST_CLIENT_ID";

/// Fixed loopback redirect. The port is part of the redirect URI
/// pre-registered in the App Management Console, so it must not be
/// ephemeral. If the port is taken, connecting fails with a message
/// instead of silently using a URI Todoist would reject.
pub const REDIRECT_URI: &str = "http://127.0.0.1:53682/callback";

pub struct OAuthConfig {
    pub client_id: String,
    pub redirect_uri: String,
}

impl OAuthConfig {
    pub fn load() -> anyhow::Result<Self> {
        let client_id = std::env::var("TODOIST_CLIENT_ID")
            .ok()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| TODOIST_CLIENT_ID.to_string());
        if client_id == "YOUR_TODOIST_CLIENT_ID" {
            return Err(anyhow::anyhow!(
                "Todoist is not registered yet: set TODOIST_CLIENT_ID or bake the client ID into todoist_auth.rs"
            ));
        }
        Ok(Self {
            client_id,
            redirect_uri: REDIRECT_URI.to_string(),
        })
    }
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

/// Run the full connect flow: open the browser, wait for the loopback
/// callback, exchange the code with the PKCE verifier. Returns the access
/// token. No client secret is involved at any point.
pub async fn connect(config: &OAuthConfig) -> anyhow::Result<String> {
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
    exchange_code(config, &code, &verifier).await
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

/// Exchange the code for a token as a PKCE public client: client ID, code,
/// redirect URI and verifier — no client secret.
async fn exchange_code(
    config: &OAuthConfig,
    code: &str,
    verifier: &str,
) -> anyhow::Result<String> {
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
    body.get("access_token")
        .and_then(|token| token.as_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Token exchange failed ({status}): {body}"))
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
}
