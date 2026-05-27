//! # Nous Portal OAuth Login
//!
//! OAuth 2.0 Device Code flow for Nous Portal authentication.
//! Polls for token completion, then mints a short-lived agent key for inference.
//! Supports automatic token refresh and shared credential storage across profiles.
//!
//! ```bash
//! cargo run --example nous_login --release
//! cargo run --example nous_login --release -- --force  # re-authenticate
//! ```

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::sleep;

// ─── OAuth constants ─────────────────────────────────────────────────────────

const DEFAULT_CLIENT_ID: &str = "hermes-cli";
const DEFAULT_PORTAL_URL: &str = "https://portal.nousresearch.com";
const DEFAULT_INFERENCE_URL: &str = "https://inference.nousresearch.com/v1";
const DEFAULT_SCOPE: &str = "inference:mint_agent_key";
const DEFAULT_AGENT_KEY_MIN_TTL: u64 = 300; // 5 minutes
const DEVICE_CODE_ENDPOINT: &str = "/api/oauth/device-code";
const TOKEN_ENDPOINT: &str = "/api/oauth/token";
const AGENT_KEY_ENDPOINT: &str = "/api/oauth/agent-key";

// ─── Token structures ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    #[serde(rename = "_schema", default = "default_schema")]
    pub schema_version: u32,

    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub scope: String,

    pub client_id: String,
    pub portal_base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_base_url: Option<String>,

    pub obtained_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    // Profile-specific agent key fields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_key_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_key_expires_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_key_reused: Option<bool>,
}

fn default_schema() -> u32 {
    1
}

impl OAuthTokens {
    pub fn is_expired(&self) -> bool {
        // Add 30s buffer for clock skew
        Utc::now() + chrono::Duration::seconds(30) >= self.expires_at
    }

    pub fn agent_key_expired(&self) -> bool {
        self.agent_key_expires_at
            .map(|exp| Utc::now() + chrono::Duration::seconds(30) >= exp)
            .unwrap_or(true)
    }

    pub fn shared_token_path() -> Result<PathBuf> {
        let root = dirs::home_dir()
            .ok_or_else(|| anyhow!("Could not determine home directory"))?
            .join(".hermes")
            .join("shared");
        std::fs::create_dir_all(&root)?;
        Ok(root.join("nous_auth.json"))
    }

    pub fn profile_token_path(hermes_home: &str) -> PathBuf {
        PathBuf::from(hermes_home).join("auth.json")
    }

    pub async fn load_shared() -> Option<Self> {
        let path = Self::shared_token_path().ok()?;
        let content = tokio::fs::read_to_string(&path).await.ok()?;
        serde_json::from_str(&content).ok()
    }

    pub async fn save_shared(&self) -> Result<()> {
        let path = Self::shared_token_path()?;
        let json = serde_json::to_string_pretty(self)?;
        tokio::fs::write(&path, json).await?;
        Ok(())
    }

    pub async fn load_profile(hermes_home: &str) -> Option<Self> {
        let path = Self::profile_token_path(hermes_home);
        let content = tokio::fs::read_to_string(&path).await.ok()?;
        serde_json::from_str(&content).ok()
    }

    pub async fn save_profile(&self, hermes_home: &str) -> Result<()> {
        let path = Self::profile_token_path(hermes_home);
        if let Some(p) = path.parent() {
            tokio::fs::create_dir_all(p).await?;
        }
        let json = serde_json::to_string_pretty(self)?;
        tokio::fs::write(&path, json).await?;
        Ok(())
    }

    pub fn credential(&self) -> Option<&str> {
        self.agent_key
            .as_deref()
            .or_else(|| self.access_token.as_str().into())
    }
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    verification_uri_complete: String,
    #[serde(default)]
    user_code: Option<String>,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    inference_base_url: Option<String>,
}

#[derive(Deserialize)]
struct AgentKeyResponse {
    api_key: String,
    key_id: String,
    expires_at: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    reused: Option<bool>,
    #[serde(default)]
    inference_base_url: Option<String>,
}

// ─── Config helpers ──────────────────────────────────────────────────────────

fn env_or_default(var: &str, default: &str) -> String {
    std::env::var(var)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn get_config() -> (String, String, String, String) {
    let client_id = env_or_default("HERMES_CLIENT_ID", DEFAULT_CLIENT_ID);
    let portal = env_or_default(
        "HERMES_PORTAL_BASE_URL",
        &env_or_default("NOUS_PORTAL_BASE_URL", DEFAULT_PORTAL_URL),
    );
    let inference = env_or_default("NOUS_INFERENCE_BASE_URL", DEFAULT_INFERENCE_URL);
    let scope = env_or_default("HERMES_SCOPE", DEFAULT_SCOPE);
    (
        client_id,
        portal.trim_end_matches('/').to_string(),
        inference,
        scope,
    )
}

// ─── Device code flow ────────────────────────────────────────────────────────

async fn request_device_code(
    client_id: &str,
    portal_url: &str,
    scope: &str,
) -> Result<DeviceCodeResponse> {
    let client = reqwest::Client::new();
    let url = format!("{portal_url}{DEVICE_CODE_ENDPOINT}");

    let resp = client
        .post(&url)
        // .form(&[("client_id", client_id), ("scope", scope)])
        .send()
        .await
        .context("Failed to request device code")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Device code request failed ({}): {}", status, text);
    }

    resp.json()
        .await
        .context("Failed to parse device code response")
}

async fn poll_for_token(
    device_code: &str,
    client_id: &str,
    portal_url: &str,
    interval: u64,
    expires_in: u64,
) -> Result<TokenResponse> {
    let client = reqwest::Client::new();
    let url = format!("{portal_url}{TOKEN_ENDPOINT}");
    let deadline = std::time::Instant::now() + Duration::from_secs(expires_in);

    loop {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("Device code expired");
        }

        let resp = client
            .post(&url)
            // .form(&[
            //     ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            //     ("device_code", device_code),
            //     ("client_id", client_id),
            // ])
            .send()
            .await
            .context("Token poll request failed")?;

        match resp.status().as_u16() {
            200 => return resp.json().await.context("Failed to parse token response"),
            400 => {
                // Check for authorization_pending vs slow_down
                let err: serde_json::Value = resp.json().await.unwrap_or_default();
                match err["error"].as_str() {
                    Some("authorization_pending") => {} // Continue polling
                    Some("slow_down") => sleep(Duration::from_secs(interval + 5)).await,
                    Some("expired_token") => anyhow::bail!("Device code expired"),
                    Some("access_denied") => anyhow::bail!("Authorization denied by user"),
                    _ => {}
                }
            }
            _ => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("Token poll failed ({}): {}", status, text);
            }
        }

        sleep(Duration::from_secs(interval)).await;
    }
}

// ─── Agent key minting ───────────────────────────────────────────────────────

async fn mint_agent_key(
    access_token: &str,
    portal_url: &str,
    min_ttl: u64,
) -> Result<AgentKeyResponse> {
    let client = reqwest::Client::new();
    let url = format!("{portal_url}{AGENT_KEY_ENDPOINT}");

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {access_token}"))
        .json(&serde_json::json!({ "min_ttl_seconds": min_ttl }))
        .send()
        .await
        .context("Agent key mint request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Agent key mint failed ({}): {}", status, text);
    }

    resp.json()
        .await
        .context("Failed to parse agent key response")
}

// ─── Token refresh ───────────────────────────────────────────────────────────

async fn refresh_access_token(
    refresh_token: &str,
    client_id: &str,
    portal_url: &str,
) -> Result<TokenResponse> {
    let client = reqwest::Client::new();
    let url = format!("{portal_url}{TOKEN_ENDPOINT}");

    let resp = client
        .post(&url)
        .header("x-nous-refresh-token", refresh_token)
        // .form(&[("grant_type", "refresh_token"), ("client_id", client_id)])
        .send()
        .await
        .context("Refresh token request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let err_code: String = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["error"].as_str().map(|s| s.to_string()))
            .unwrap_or("unknown".to_string());

        // Handle refresh token reuse detection (security measure)
        if err_code == "refresh_token_reused" || err_code == "invalid_grant" {
            anyhow::bail!("Refresh token revoked or reused - re-authentication required");
        }
        anyhow::bail!("Token refresh failed ({}): {}", status, text);
    }

    resp.json()
        .await
        .context("Failed to parse refresh response")
}

// ─── Browser opener ──────────────────────────────────────────────────────────

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let ps = format!("Start-Process '{}'", url.replace('\'', "''"));
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
            .spawn();
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

// ─── Login flow ──────────────────────────────────────────────────────────────

pub async fn login(
    hermes_home: Option<&str>,
    force: bool,
    min_agent_key_ttl: Option<u64>,
) -> Result<OAuthTokens> {
    let (client_id, portal_url, inference_url, scope) = get_config();
    let min_ttl = min_agent_key_ttl.unwrap_or(DEFAULT_AGENT_KEY_MIN_TTL);

    // // Try loading existing tokens
    let existing: Option<OAuthTokens> = if !force {
        // OAuthTokens::load_shared().await.or_else(|| {
        //     hermes_home.and_then(|h| futures::executor::block_on(OAuthTokens::load_profile(h)))
        // })
        todo!()
    } else {
        None
    };

    if let Some(mut tokens) = existing {
        if !tokens.is_expired() {
            println!("✓ Valid cached tokens found");
            // Try to mint fresh agent key if needed
            if tokens.agent_key_expired() {
                print!("  Minting new agent key... ");
                std::io::stdout().flush().ok();
                match mint_agent_key(&tokens.access_token, &portal_url, min_ttl).await {
                    Ok(key_resp) => {
                        println!("done");
                        update_agent_key(
                            &mut tokens,
                            &key_resp,
                            // &inference_url
                        );
                        tokens.updated_at = Utc::now();
                        tokens.save_shared().await?;
                        if let Some(home) = hermes_home {
                            tokens.save_profile(home).await?;
                        }
                    }
                    Err(e) => println!("failed: {} (will retry on next API call)", e),
                }
            }
            return Ok(tokens);
        }

        // Try refresh if we have a refresh token
        if let Some(ref rt) = tokens.refresh_token {
            print!("  Token expired, refreshing... ");
            std::io::stdout().flush().ok();
            match refresh_access_token(rt, &client_id, &portal_url).await {
                Ok(refreshed) => {
                    println!("done");
                    tokens = update_tokens(tokens, refreshed, &portal_url, &inference_url);
                    // Mint agent key with fresh access token
                    match mint_agent_key(&tokens.access_token, &portal_url, min_ttl).await {
                        Ok(key_resp) => {
                            update_agent_key(
                                &mut tokens,
                                &key_resp,
                                // &inference_url
                            )
                        }
                        Err(e) => eprintln!("  Warning: Could not mint agent key: {}", e),
                    }
                    tokens.updated_at = Utc::now();
                    tokens.save_shared().await?;
                    if let Some(home) = hermes_home {
                        tokens.save_profile(home).await?;
                    }
                    return Ok(tokens);
                }
                Err(e) => println!("failed: {}", e),
            }
        }
    }

    // Full device code flow
    println!("\n  Requesting device code from Nous Portal...");
    let device_resp = request_device_code(&client_id, &portal_url, &scope).await?;

    println!("\n  Authenticate in your browser:");
    println!("  {}", device_resp.verification_uri_complete);
    if let Some(ref code) = device_resp.user_code {
        println!("  (User code: {})", code);
    }

    open_browser(&device_resp.verification_uri_complete);
    print!("\n  Waiting for authorization");
    std::io::stdout().flush().ok();

    let token_resp = poll_for_token(
        &device_resp.device_code,
        &client_id,
        &portal_url,
        device_resp.interval,
        device_resp.expires_in,
    )
    .await?;
    println!("\n  ✓ Authorization complete");

    // Build base tokens
    let now = Utc::now();
    let mut tokens = OAuthTokens {
        schema_version: 1,
        access_token: token_resp.access_token,
        refresh_token: token_resp.refresh_token,
        token_type: token_resp.token_type.unwrap_or_else(|| "Bearer".into()),
        scope: token_resp.scope.unwrap_or_else(|| scope.clone()),
        client_id: client_id.clone(),
        portal_base_url: portal_url.clone(),
        inference_base_url: token_resp
            .inference_base_url
            .or_else(|| env_or_default("NOUS_INFERENCE_BASE_URL", DEFAULT_INFERENCE_URL).into()),
        obtained_at: now,
        expires_at: now + chrono::Duration::seconds(token_resp.expires_in as i64),
        updated_at: now,
        agent_key: None,
        agent_key_id: None,
        agent_key_expires_at: None,
        agent_key_reused: None,
    };

    // Mint agent key
    print!("  Minting agent key... ");
    std::io::stdout().flush().ok();
    match mint_agent_key(&tokens.access_token, &portal_url, min_ttl).await {
        Ok(key_resp) => {
            println!("done");
            update_agent_key(
                &mut tokens,
                &key_resp,
                // &tokens.inference_base_url.clone().unwrap_or(inference_url),
            );
        }
        Err(e) => eprintln!("failed: {} (inference may still work with access token)", e),
    }

    // Persist
    tokens.save_shared().await?;
    println!(
        "  ✓ Tokens saved to: {}",
        OAuthTokens::shared_token_path()?.display()
    );

    if let Some(home) = hermes_home {
        tokens.save_profile(home).await?;
        println!(
            "  ✓ Profile auth updated: {}",
            OAuthTokens::profile_token_path(home).display()
        );
    }

    Ok(tokens)
}

fn update_tokens(
    mut tokens: OAuthTokens,
    resp: TokenResponse,
    portal_url: &str,
    inference_url: &str,
) -> OAuthTokens {
    let now = Utc::now();
    tokens.access_token = resp.access_token;
    if let Some(rt) = resp.refresh_token {
        tokens.refresh_token = Some(rt);
    }
    tokens.expires_at = now + chrono::Duration::seconds(resp.expires_in as i64);
    tokens.updated_at = now;
    if let Some(s) = resp.scope {
        tokens.scope = s;
    }
    tokens.inference_base_url = resp.inference_base_url.or(Some(inference_url.to_string()));
    tokens.portal_base_url = portal_url.to_string();
    tokens
}

fn update_agent_key(
    tokens: &mut OAuthTokens,
    resp: &AgentKeyResponse,
    // inference_url: &str
) {
    tokens.agent_key = Some(resp.api_key.clone());
    tokens.agent_key_id = Some(resp.key_id.clone());
    tokens.agent_key_expires_at = DateTime::parse_from_rfc3339(&resp.expires_at)
        .ok()
        .map(|dt| dt.with_timezone(&Utc));
    tokens.agent_key_reused = resp.reused;
    if let Some(ttl) = resp.expires_in {
        tokens.agent_key_expires_at = Some(Utc::now() + chrono::Duration::seconds(ttl as i64));
    }
    // if let Some(ref url) = resp.inference_base_url {
    //     tokens.inference_base_url = Some(url.clone());
    // } else if tokens.inference_base_url.is_none() {
    //     tokens.inference_base_url = Some(inference_url.to_string());
    // }
}

// ─── Main example entrypoint ─────────────────────────────────────────────────

// #[tokio::main]
async fn nous_oauth_login() -> Result<()> {
    // use clap::Parser;

    // #[derive(clap::Parser)]
    // #[command(name = "nous_login", about = "Authenticate with Nous Portal")]
    struct Args {
        /// Force re-authentication even if valid tokens exist
        // #[arg(long)]
        force: bool,
        /// Hermes home directory for profile-specific auth
        // #[arg(long, env = "HERMES_HOME")]
        hermes_home: Option<String>,
        /// Minimum agent key TTL in seconds (default: 300)
        // #[arg(long, default_value = "300")]
        agent_key_ttl: u64,
    }
    impl Default for Args {
        fn default() -> Self {
            Self {
                force: false,
                hermes_home: None,
                agent_key_ttl: 300,
            }
        }
    }

    let args = Args::default();

    println!("╔════════════════════════════════════════╗");
    println!("║  Nous Portal OAuth — Device Code Flow  ║");
    println!("╚════════════════════════════════════════╝");

    let tokens = login(
        args.hermes_home.as_deref(),
        args.force,
        Some(args.agent_key_ttl),
    )
    .await?;

    println!("\n  Authenticated successfully");
    println!("  Portal: {}", tokens.portal_base_url);
    println!(
        "  Inference: {}",
        tokens.inference_base_url.as_deref().unwrap_or("(default)")
    );
    println!("  Scopes: {}", tokens.scope);
    println!("  Access token expires: {}", tokens.expires_at.to_rfc3339());

    if let Some(ref key) = tokens.agent_key {
        println!(
            "  Agent key: {}... (expires: {})",
            &key[..min(12, key.len())],
            tokens
                .agent_key_expires_at
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| "unknown".into())
        );
    }

    Ok(())
}

fn min(a: usize, b: usize) -> usize {
    if a < b { a } else { b }
}
