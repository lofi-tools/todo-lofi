use crate::OpenAiCompatible;
use crate::nous_portal::nous_auth_file::AuthFile;
use cersei::prelude::Auth;

pub fn provider() -> anyhow::Result<OpenAiCompatible> {
    let nous_auth_cfg = AuthFile::load_default()?;
    Ok(OpenAiCompatible {
        name: "nous-portal".to_string(),
        auth: Auth::ApiKey(nous_auth_cfg.agent_key()?.to_string()),
        base_url: "https://inference-api.nousresearch.com/v1".to_string(),
        default_model: "deepseek/deepseek-v4-flash".to_string(),
        client: reqwest::Client::new(),
    })
}

pub mod nous_auth_file {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::path::Path;
    use std::{path::PathBuf, sync::LazyLock};

    pub static HOME: LazyLock<PathBuf> =
        LazyLock::new(|| std::env::var("HOME").map(PathBuf::from).unwrap());
    pub static AUTH_FILE_PATH: LazyLock<PathBuf> = LazyLock::new(|| HOME.join(".hermes/auth.json"));

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct AuthFile {
        pub version: u32,
        pub providers: HashMap<String, ProviderConfig>,
        pub active_provider: String,
        pub updated_at: DateTime<Utc>,
        pub credential_pool: HashMap<String, Vec<Credential>>,
    }
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ProviderConfig {
        pub access_token: String,
        pub refresh_token: String,
        pub client_id: String,
        pub portal_base_url: String,
        pub inference_base_url: String,
        pub token_type: String,
        pub scope: String,
        pub obtained_at: DateTime<Utc>,
        pub expires_at: DateTime<Utc>,
        pub agent_key: String,
        pub agent_key_expires_at: DateTime<Utc>,
        pub tls: TlsConfig,
        pub agent_key_id: String,
        #[serde(default)]
        pub agent_key_expires_in: Option<i64>,
        #[serde(default)]
        pub agent_key_reused: bool,
        pub agent_key_obtained_at: DateTime<Utc>,
    }
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct TlsConfig {
        pub insecure: bool,
        pub ca_bundle: Option<String>,
    }
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Credential {
        pub id: String,
        pub label: String,
        pub auth_type: String,
        pub priority: i32,
        pub source: String,
        pub access_token: String,
        pub refresh_token: String,
        #[serde(default)]
        pub last_status: Option<serde_json::Value>,
        #[serde(default)]
        pub last_status_at: Option<DateTime<Utc>>,
        #[serde(default)]
        pub last_error_code: Option<serde_json::Value>,
        #[serde(default)]
        pub last_error_reason: Option<serde_json::Value>,
        #[serde(default)]
        pub last_error_message: Option<serde_json::Value>,
        #[serde(default)]
        pub last_error_reset_at: Option<DateTime<Utc>>,
        pub expires_at: DateTime<Utc>,
        pub inference_base_url: String,
        pub agent_key: String,
        pub agent_key_expires_at: DateTime<Utc>,
        #[serde(default)]
        pub request_count: u64,
        pub scope: String,
        pub portal_base_url: String,
        pub token_type: String,
        #[serde(default)]
        pub agent_key_reused: bool,
        pub client_id: String,
        pub agent_key_obtained_at: DateTime<Utc>,
        pub tls: TlsConfig,
        pub agent_key_id: String,
        #[serde(default)]
        pub agent_key_expires_in: Option<i64>,
        pub obtained_at: DateTime<Utc>,
    }

    impl AuthFile {
        pub fn load(path: &Path) -> anyhow::Result<Self> {
            let content = std::fs::read_to_string(path)?;
            let auth: AuthFile = serde_json::from_str(&content)?;
            Ok(auth)
        }
        pub fn load_default() -> anyhow::Result<Self> {
            Self::load(&AUTH_FILE_PATH)
        }

        pub fn agent_key(&self) -> anyhow::Result<&str> {
            let nous_provider_config = self
                .providers
                .get("nous")
                .ok_or_else(|| anyhow::anyhow!("No Nous provider config found"))?;
            Ok(&nous_provider_config.agent_key)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_load_auth_file() -> anyhow::Result<()> {
            let auth = AuthFile::load_default()?;
            if let Some(creds) = auth.credential_pool.get(&auth.active_provider)
                && let Some(active_cred) = creds.first()
            {
                println!("Agent key: {}", active_cred.agent_key);
                println!("Expires: {}", active_cred.agent_key_expires_at);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use cersei::{prelude::Provider, types::Message};

    use super::*;

    #[tokio::test]
    async fn test_provider() -> anyhow::Result<()> {
        let provider = provider()?;
        assert_eq!(provider.name, "nous-portal");
        // assert_eq!(
        //     provider.base_url,
        //     "https://inference-api.nousresearch.com/v1"
        // );
        // assert_eq!(provider.default_model, "deepseek/deepseek-v4-flash");

        // let resp: String = provider.complete(request).await?;

        let request = cersei::provider::CompletionRequest {
            model: provider.default_model.clone(),
            messages: vec![Message::user("What model are you ?".to_string())],
            system: Some("System prompt: You are 'Deepseek v4 Flash'.".into()),
            tools: Vec::new(),
            max_tokens: 4096,
            temperature: Some(0.0),
            stop_sequences: Vec::new(),
            options: cersei::provider::ProviderOptions::default(),
        };

        // Collect streaming response into a complete message
        let stream = provider.complete(request).await?;
        let mut rx = stream.into_receiver();
        let mut accumulator = cersei::provider::StreamAccumulator::new();
        while let Some(event) = rx.recv().await {
            dbg!(&event);
            accumulator.process_event(event);
        }
        let response = accumulator.into_response()?;
        let summary_text = response.message.get_all_text();

        dbg!(&summary_text);

        Ok(())
    }
}
