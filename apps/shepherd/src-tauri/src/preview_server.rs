//! A loopback-only HTML wrapper around one configured project preview.
//!
//! The wrapper deliberately does not proxy the preview: it embeds the configured
//! loopback URL in an iframe. Element selection remains an in-app preview
//! feature because a cross-origin iframe cannot be inspected from here.

use std::borrow::Cow;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tauri::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REQUEST_BYTES: usize = 16 * 1024;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PreviewServerError {
    #[error("Enter a loopback HTTP or HTTPS preview URL without credentials.")]
    InvalidTarget,
    #[error("Could not start the preview wrapper: {0}")]
    Start(#[from] io::Error),
    #[error("Could not stop the preview wrapper: {0}")]
    Stop(String),
    #[error("Could not build the preview wrapper URL: {0}")]
    Address(String),
}

#[derive(Default)]
pub(crate) struct PreviewServer {
    sessions: Arc<Mutex<Sessions>>,
    running: Mutex<Option<Running>>,
}

#[derive(Default)]
struct Sessions {
    by_project: std::collections::BTreeMap<String, Session>,
    by_token: std::collections::BTreeMap<String, Url>,
}

struct Session {
    token: String,
    target: Url,
}

struct Running {
    address: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl PreviewServer {
    pub(crate) async fn open(
        &self,
        project_id: &str,
        target: &Url,
    ) -> Result<Url, PreviewServerError> {
        validate_target(target)?;
        let mut running = self.running.lock().await;
        if running.is_none() {
            *running = Some(start_server(self.sessions.clone()).await?);
        }
        let address = running
            .as_ref()
            .map(|running| running.address)
            .ok_or_else(|| {
                PreviewServerError::Address("the preview wrapper did not start".into())
            })?;
        let mut sessions = self.sessions.lock().await;
        let token = match sessions.by_project.get(project_id) {
            Some(session) if session.target == *target => session.token.clone(),
            _ => {
                let token = new_token();
                if let Some(previous) = sessions.by_project.remove(project_id) {
                    sessions.by_token.remove(&previous.token);
                }
                sessions.by_project.insert(
                    project_id.to_owned(),
                    Session {
                        token: token.clone(),
                        target: target.clone(),
                    },
                );
                sessions.by_token.insert(token.clone(), target.clone());
                token
            }
        };
        let wrapper = format!("http://{address}/preview/{token}/");
        Url::parse(&wrapper).map_err(|error| PreviewServerError::Address(error.to_string()))
    }

    pub(crate) async fn forget_project(&self, project_id: &str) {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.by_project.remove(project_id) {
            sessions.by_token.remove(&session.token);
        }
    }

    pub(crate) async fn stop(&self) -> Result<(), PreviewServerError> {
        let running = self.running.lock().await.take();
        {
            let mut sessions = self.sessions.lock().await;
            sessions.by_project.clear();
            sessions.by_token.clear();
        }
        let Some(running) = running else {
            return Ok(());
        };
        if running.shutdown.send(()).is_err() {
            tracing::debug!("the preview wrapper was already stopping");
        }
        running
            .task
            .await
            .map_err(|error| PreviewServerError::Stop(error.to_string()))
    }
}

async fn start_server(sessions: Arc<Mutex<Sessions>>) -> Result<Running, PreviewServerError> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let (shutdown, mut stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                _ = &mut stopped => break,
                accepted = listener.accept() => accepted,
            };
            match accepted {
                Ok((stream, peer)) => {
                    if !peer.ip().is_loopback() {
                        continue;
                    }
                    let sessions = sessions.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_connection(stream, sessions, address).await {
                            tracing::debug!(%error, "preview wrapper connection failed");
                        }
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "preview wrapper stopped accepting connections");
                    break;
                }
            }
        }
    });
    Ok(Running {
        address,
        shutdown,
        task,
    })
}

fn validate_target(target: &Url) -> Result<(), PreviewServerError> {
    if !matches!(target.scheme(), "http" | "https")
        || !target.username().is_empty()
        || target.password().is_some()
    {
        return Err(PreviewServerError::InvalidTarget);
    }
    let host = target.host_str().ok_or(PreviewServerError::InvalidTarget)?;
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
    {
        Ok(())
    } else {
        Err(PreviewServerError::InvalidTarget)
    }
}

fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

async fn handle_connection(
    mut stream: TcpStream,
    sessions: Arc<Mutex<Sessions>>,
    address: SocketAddr,
) -> io::Result<()> {
    let head = match read_head(&mut stream).await {
        Ok(head) => head,
        Err(response) => return respond(&mut stream, response).await,
    };
    let request = match parse_request(&head, address) {
        Ok(request) => request,
        Err(response) => return respond(&mut stream, response).await,
    };
    let Some(token) = token_from_path(&request.path).map(str::to_owned) else {
        return respond(&mut stream, Response::not_found()).await;
    };
    let target = {
        let sessions = sessions.lock().await;
        sessions.by_token.get(&token).cloned()
    };
    let Some(target) = target else {
        return respond(&mut stream, Response::not_found()).await;
    };
    let (body, csp) = wrapper_html(&target, &request.path);
    respond(&mut stream, Response::html(body, csp)).await
}

async fn read_head(stream: &mut TcpStream) -> Result<String, Response> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let read = tokio::time::timeout(REQUEST_TIMEOUT, stream.read(&mut chunk))
            .await
            .map_err(|_| Response::timeout())?
            .map_err(|_| Response::bad_request())?;
        if read == 0 {
            return Err(Response::bad_request());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_REQUEST_BYTES {
            return Err(Response::too_large());
        }
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            buffer.truncate(end);
            return String::from_utf8(buffer).map_err(|_| Response::bad_request());
        }
    }
}

struct Request {
    path: String,
}

fn parse_request(head: &str, address: SocketAddr) -> Result<Request, Response> {
    let mut lines = head.split("\r\n");
    let request_line = lines.next().ok_or_else(Response::bad_request)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or_else(Response::bad_request)?;
    let path = parts.next().ok_or_else(Response::bad_request)?;
    let version = parts.next().ok_or_else(Response::bad_request)?;
    if parts.next().is_some() || version != "HTTP/1.1" || !path.starts_with('/') {
        return Err(Response::bad_request());
    }
    if method != "GET" {
        return Err(Response::method_not_allowed());
    }

    let mut host = None;
    let mut origin = None;
    let mut destination = None;
    let mut mode = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or_else(Response::bad_request)?;
        if name.eq_ignore_ascii_case("host") {
            if host.is_some() {
                return Err(Response::bad_request());
            }
            host = Some(value.trim().to_owned());
        } else if name.eq_ignore_ascii_case("origin") {
            origin = Some(value.trim().to_owned());
        } else if name.eq_ignore_ascii_case("sec-fetch-dest") {
            destination = Some(value.trim().to_owned());
        } else if name.eq_ignore_ascii_case("sec-fetch-mode") {
            mode = Some(value.trim().to_owned());
        }
    }
    let host = host.ok_or_else(Response::bad_request)?;
    if !host_matches(&host, address) {
        return Err(Response::forbidden());
    }
    if origin.is_some_and(|origin| origin != format!("http://{host}")) {
        return Err(Response::forbidden());
    }
    if destination.is_some_and(|destination| !destination.eq_ignore_ascii_case("document")) {
        return Err(Response::forbidden());
    }
    if mode.is_some_and(|mode| !mode.eq_ignore_ascii_case("navigate")) {
        return Err(Response::forbidden());
    }
    Ok(Request {
        path: path.to_owned(),
    })
}

fn host_matches(host: &str, address: SocketAddr) -> bool {
    let host = host.trim().to_ascii_lowercase();
    let port = address.port();
    host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
}

fn token_from_path(path: &str) -> Option<&str> {
    let token = path.strip_prefix("/preview/")?.strip_suffix('/')?;
    (token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(token)
}

struct Response {
    status: &'static str,
    body: Cow<'static, str>,
    content_type: &'static str,
    csp: Option<String>,
    allow: Option<&'static str>,
}

impl Response {
    fn text(status: &'static str, body: &'static str) -> Self {
        Self {
            status,
            body: Cow::Borrowed(body),
            content_type: "text/plain; charset=utf-8",
            csp: None,
            allow: None,
        }
    }

    fn html(body: String, csp: String) -> Self {
        Self {
            status: "200 OK",
            body: Cow::Owned(body),
            content_type: "text/html; charset=utf-8",
            csp: Some(csp),
            allow: None,
        }
    }

    fn bad_request() -> Self {
        Self::text("400 Bad Request", "The request was malformed.")
    }

    fn forbidden() -> Self {
        Self::text(
            "403 Forbidden",
            "The wrapper only serves its configured origin.",
        )
    }

    fn not_found() -> Self {
        Self::text("404 Not Found", "The preview wrapper was not found.")
    }

    fn method_not_allowed() -> Self {
        let mut response = Self::text("405 Method Not Allowed", "Use GET.");
        response.allow = Some("GET");
        response
    }

    fn timeout() -> Self {
        Self::text("408 Request Timeout", "The request timed out.")
    }

    fn too_large() -> Self {
        Self::text(
            "431 Request Header Fields Too Large",
            "The request headers are too large.",
        )
    }
}

async fn respond(stream: &mut TcpStream, response: Response) -> io::Result<()> {
    let Response {
        status,
        body,
        content_type,
        csp,
        allow,
    } = response;
    let allow = allow
        .map(|methods| format!("Allow: {methods}\r\n"))
        .unwrap_or_default();
    let csp = csp
        .map(|policy| format!("Content-Security-Policy: {policy}\r\n"))
        .unwrap_or_default();
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nPragma: no-cache\r\n\
         X-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\n\
         Referrer-Policy: no-referrer\r\nCross-Origin-Resource-Policy: same-origin\r\n\
         Connection: close\r\n{allow}{csp}\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await
}

fn wrapper_html(target: &Url, path: &str) -> (String, String) {
    let origin = target.origin().ascii_serialization();
    let target = escape_html(target.as_str());
    let reload = escape_html(path);
    let html = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="no-referrer">
<link rel="icon" href="data:,">
<title>Shepherd preview</title>
<style>
:root { color-scheme: light dark; --background: #f6f7f9; --surface: #ffffff; --text: #20242c; --muted: #5d6675; --border: #d7dce4; --accent: #6387ff; }
@media (prefers-color-scheme: dark) { :root { --background: #15181d; --surface: #1d222a; --text: #edf0f5; --muted: #9aa4b2; --border: #343b47; --accent: #8ba3ff; } }
* { box-sizing: border-box; }
html, body { height: 100%; margin: 0; }
body { display: flex; flex-direction: column; background: var(--background); color: var(--text); font: 13px/1.4 ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
.toolbar { display: flex; align-items: center; gap: 8px; min-height: 44px; padding: 6px 12px; border-bottom: 1px solid var(--border); background: var(--surface); }
.brand { font-weight: 650; }
.tag { padding: 2px 7px; border: 1px solid color-mix(in srgb, var(--accent) 55%, var(--border)); border-radius: 999px; color: var(--accent); font-size: 11px; }
.hint { color: var(--muted); }
.actions { display: flex; gap: 12px; margin-left: auto; }
.actions a { color: var(--accent); text-decoration: none; }
.actions a:hover { text-decoration: underline; }
iframe { flex: 1; width: 100%; min-height: 0; border: 0; background: var(--surface); }
@media (max-width: 620px) { .hint { display: none; } }
</style>
</head>
<body data-shepherd-preview-wrapper>
<header class="toolbar" role="toolbar" aria-label="Preview annotation toolbar">
  <span class="brand">Shepherd</span>
  <span class="tag">Annotation wrapper</span>
  <span class="hint">Element selection is available in Shepherd's Preview tab.</span>
  <span class="actions"><a href="__RELOAD__">Reload</a><a href="__TARGET__" target="_blank" rel="noreferrer">Open page</a></span>
</header>
<iframe title="Project website" src="__TARGET__" referrerpolicy="no-referrer" sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-downloads"></iframe>
</body>
</html>"#;
    let html = html
        .replace("__TARGET__", &target)
        .replace("__RELOAD__", &reload);
    let csp = format!(
        "default-src 'none'; style-src 'unsafe-inline'; img-src data:; frame-src {origin}; \
         frame-ancestors 'none'; base-uri 'none'; form-action 'none'; object-src 'none'"
    );
    (html, csp)
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    async fn request(
        url: &Url,
        host: Option<&str>,
        extra_headers: &str,
    ) -> Result<(u16, String, String), Box<dyn Error>> {
        let authority = format!(
            "{}:{}",
            url.host_str().ok_or("missing host")?,
            url.port().ok_or("missing port")?
        );
        let host = host.unwrap_or(&authority);
        let mut path = url.path().to_owned();
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }
        let mut stream = TcpStream::connect(&authority).await?;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n{extra_headers}\r\n"
        );
        stream.write_all(request.as_bytes()).await?;
        let mut response = String::new();
        stream.read_to_string(&mut response).await?;
        let (head, body) = response
            .split_once("\r\n\r\n")
            .ok_or("missing response body")?;
        let status = head
            .lines()
            .next()
            .ok_or("missing status line")?
            .split_whitespace()
            .nth(1)
            .ok_or("missing status code")?
            .parse()?;
        Ok((status, head.to_ascii_lowercase(), body.to_owned()))
    }

    #[tokio::test]
    async fn serves_only_the_configured_token_and_never_proxies() -> Result<(), Box<dyn Error>> {
        let server = PreviewServer::default();
        let target = Url::parse("http://localhost:3000/app?plan=pro&view=compact")?;
        let wrapper = server.open("project-1", &target).await?;
        assert_eq!(wrapper.host_str(), Some("127.0.0.1"));
        assert!(wrapper.port().is_some());
        assert!(wrapper.path().starts_with("/preview/"));

        let (status, headers, body) = request(&wrapper, None, "").await?;
        assert_eq!(status, 200);
        assert!(body.contains("data-shepherd-preview-wrapper"));
        assert!(body.contains("http://localhost:3000/app?plan=pro&amp;view=compact"));
        assert!(headers.contains("content-security-policy: default-src 'none'"));
        assert!(headers.contains("frame-src http://localhost:3000"));
        assert!(headers.contains("cache-control: no-store"));
        assert!(headers.contains("x-frame-options: deny"));

        let (status, _, _) = request(&wrapper, Some("evil.example:3000"), "").await?;
        assert_eq!(status, 403);
        let (status, _, _) = request(&wrapper, None, "Origin: http://evil.example\r\n").await?;
        assert_eq!(status, 403);
        let (status, _, _) = request(
            &wrapper,
            None,
            "Sec-Fetch-Dest: iframe\r\nSec-Fetch-Mode: navigate\r\n",
        )
        .await?;
        assert_eq!(status, 403);

        let mut wrong = wrapper.clone();
        wrong.set_path(&format!("/preview/{}/", "0".repeat(64)));
        assert_eq!(request(&wrong, None, "").await?.0, 404);
        let mut root = wrapper.clone();
        root.set_path("/");
        assert_eq!(request(&root, None, "").await?.0, 404);

        server.forget_project("project-1").await;
        assert_eq!(request(&wrapper, None, "").await?.0, 404);
        server.stop().await?;
        let address = format!(
            "{}:{}",
            wrapper.host_str().ok_or("missing host")?,
            wrapper.port().ok_or("missing port")?
        );
        assert!(TcpStream::connect(address).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn reuses_a_project_token_and_rotates_it_with_the_target() -> Result<(), Box<dyn Error>> {
        let server = PreviewServer::default();
        let first_target = Url::parse("http://localhost:3000/first")?;
        let second_target = Url::parse("http://127.0.0.1:3001/second")?;
        let first = server.open("project-1", &first_target).await?;
        let same = server.open("project-1", &first_target).await?;
        assert_eq!(first, same);
        let rotated = server.open("project-1", &second_target).await?;
        assert_ne!(first, rotated);
        assert_eq!(request(&first, None, "").await?.0, 404);
        let (status, _, body) = request(&rotated, None, "").await?;
        assert_eq!(status, 200);
        assert!(body.contains("http://127.0.0.1:3001/second"));
        server.stop().await?;
        Ok(())
    }

    #[tokio::test]
    async fn rejects_targets_outside_loopback() -> Result<(), Box<dyn Error>> {
        let server = PreviewServer::default();
        for invalid in [
            "https://example.com/",
            "http://localhost.example.com/",
            "http://user:secret@localhost:3000/",
            "file:///tmp/preview.html",
        ] {
            let target = Url::parse(invalid)?;
            assert!(matches!(
                server.open("project-1", &target).await,
                Err(PreviewServerError::InvalidTarget)
            ));
        }
        Ok(())
    }
}
