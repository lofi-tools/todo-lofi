//! Model discovery over each gateway's model-list endpoint.
//!
//! "OpenAI-compatible" covers three different model-list shapes, and one of
//! them (OpenRouter) carries per-model context windows and pricing. The shape
//! is chosen by the provider's [`ProviderKind`](crate::providers::ProviderKind);
//! parsing is a pure function over already-parsed JSON so every shape has a
//! fixture test and nothing needs a network to be verified.

use crate::providers::{DiscoveryShape, ProviderKind};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One model as a gateway's model list describes it. Everything but the id is
/// optional: only OpenRouter's list carries the extra fields.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredModel {
    /// The canonical id, after the provider's normalization.
    pub id: String,
    /// The id exactly as the gateway sent it, kept so a request can be made
    /// with the wire form when it differs from the canonical one.
    pub raw_id: String,
    pub context_window: Option<u32>,
    /// When the gateway says the model was created.
    pub created: Option<SystemTime>,
    pub input_price_per_mtok: Option<f64>,
    pub output_price_per_mtok: Option<f64>,
}

/// Fetch a provider's model list from `{base_url}/models`, sorted and
/// de-duplicated, keeping only the ids. Used by the catalog's `/provider`
/// explorer, which needs names but not metadata.
pub async fn fetch_models(base_url: &str, api_key: &str) -> anyhow::Result<Vec<String>> {
    let kind = crate::providers::provider_kind("");
    let models = fetch_discovered(kind, base_url, api_key).await?;
    let mut ids: Vec<String> = models.into_iter().map(|model| model.id).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Fetch a provider's model list with all the metadata its shape carries.
pub async fn fetch_discovered(
    kind: &dyn ProviderKind,
    base_url: &str,
    api_key: &str,
) -> anyhow::Result<Vec<DiscoveredModel>> {
    let shape = kind.discovery();
    let url = discovery_url(shape, base_url);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    let mut request = client.get(&url);
    // A gateway that needs no key (a local Ollama, say) must not be sent a
    // syntactically invalid empty bearer header.
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    let response = request.send().await?;
    let response = response
        .error_for_status()
        .map_err(|error| anyhow::anyhow!("GET {url} failed: {error}"))?;
    let body: Value = response.json().await?;
    Ok(parse_discovered(kind, &body))
}

/// The model-list URL for a shape. The one non-obvious case is Ollama's tags,
/// which live at the server root rather than under the OpenAI `/v1` prefix the
/// agent configured.
pub fn discovery_url(shape: DiscoveryShape, base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    match shape {
        DiscoveryShape::OllamaTags => {
            let root = base.strip_suffix("/v1").unwrap_or(base);
            format!("{root}/api/tags")
        }
        _ => format!("{base}/models"),
    }
}

/// Parse a model-list body into models, applying the provider's id
/// normalization. Unknown shapes and malformed entries yield fewer models, not
/// an error: a gateway that changes its list shape should degrade to "no
/// discovered models" rather than fail the caller.
pub fn parse_discovered(kind: &dyn ProviderKind, body: &Value) -> Vec<DiscoveredModel> {
    let entries = match kind.discovery() {
        DiscoveryShape::OpenAiModels | DiscoveryShape::OpenRouterModels => {
            body.get("data").and_then(Value::as_array)
        }
        DiscoveryShape::GoogleModels | DiscoveryShape::OllamaTags => {
            body.get("models").and_then(Value::as_array)
        }
    };
    let Some(entries) = entries else {
        return Vec::new();
    };

    let mut models: Vec<DiscoveredModel> = entries
        .iter()
        .filter_map(|entry| parse_entry(kind, entry))
        .collect();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    models
}

fn parse_entry(kind: &dyn ProviderKind, entry: &Value) -> Option<DiscoveredModel> {
    // Google and Ollama both name the field `name`; OpenAI and OpenRouter use
    // `id`. Accept either rather than branching on shape again.
    let raw_id = entry
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| entry.get("name").and_then(Value::as_str))?;
    let raw_id = raw_id.trim();
    if raw_id.is_empty() {
        return None;
    }
    Some(DiscoveredModel {
        id: kind.canonical_id(raw_id),
        raw_id: raw_id.to_string(),
        context_window: entry
            .get("context_length")
            .or_else(|| entry.get("context_window_tokens"))
            .and_then(Value::as_u64)
            .and_then(|tokens| u32::try_from(tokens).ok()),
        created: entry
            .get("created")
            .and_then(Value::as_u64)
            .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds)),
        input_price_per_mtok: price_per_mtok(entry, "prompt"),
        output_price_per_mtok: price_per_mtok(entry, "completion"),
    })
}

/// OpenRouter quotes per-*token* prices as decimal strings; the catalog reports
/// per-million-token costs like every other price in this workspace.
fn price_per_mtok(entry: &Value, field: &str) -> Option<f64> {
    let pricing = entry.get("pricing")?;
    let value = pricing.get(field)?;
    let per_token = match value {
        Value::String(text) => text.parse::<f64>().ok()?,
        Value::Number(number) => number.as_f64()?,
        _ => return None,
    };
    if per_token.is_finite() {
        Some(per_token * 1_000_000.0)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::provider_kind;

    #[test]
    fn parses_sorts_and_dedups_the_plain_openai_shape() {
        let body = serde_json::json!({
            "object": "list",
            "data": [
                { "id": "z-ai/glm-5.3", "object": "model" },
                { "id": "a-model", "object": "model" },
                { "id": "z-ai/glm-5.3", "object": "model" }
            ]
        });
        let models = parse_discovered(provider_kind("groq"), &body);
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["a-model", "z-ai/glm-5.3"]
        );
        assert!(models[0].context_window.is_none());
    }

    #[test]
    fn keeps_openrouters_context_window_and_pricing() {
        let body = serde_json::json!({
            "data": [{
                "id": "google/gemini-3.7-flash",
                "context_length": 1_048_576,
                "created": 1_700_000_000,
                "pricing": { "prompt": "0.00000015", "completion": "0.0000006" }
            }]
        });
        let models = parse_discovered(provider_kind("openrouter"), &body);
        let model = &models[0];
        assert_eq!(model.context_window, Some(1_048_576));
        assert_eq!(
            model.created,
            Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        );
        // Per-token strings are reported per million tokens.
        assert!((model.input_price_per_mtok.unwrap() - 0.15).abs() < 1e-9);
        assert!((model.output_price_per_mtok.unwrap() - 0.6).abs() < 1e-9);
    }

    #[test]
    fn google_models_are_readable_from_the_models_key_and_stripped() {
        let body = serde_json::json!({
            "models": [
                { "name": "models/gemini-3.8-flash" },
                { "name": "models/gemini-3.7-flash" }
            ]
        });
        let models = parse_discovered(provider_kind("google"), &body);
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["gemini-3.7-flash", "gemini-3.8-flash"]
        );
        // The wire form is still available for the request path.
        assert_eq!(models[0].raw_id, "models/gemini-3.7-flash");
    }

    #[test]
    fn ollama_tags_use_the_name_field_and_keep_the_tag() {
        let body = serde_json::json!({
            "models": [
                { "name": "gpt-oss:120b", "size": 65_000_000_000u64 },
                { "name": "qwen3.8:latest" }
            ]
        });
        let models = parse_discovered(provider_kind("ollama"), &body);
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["gpt-oss:120b", "qwen3.8:latest"]
        );
    }

    #[test]
    fn a_recognisable_but_wrong_shape_yields_no_models_instead_of_panicking() {
        let body = serde_json::json!({ "object": "list", "data": "not an array" });
        assert!(parse_discovered(provider_kind("groq"), &body).is_empty());
        // Entries without any id are dropped, not fatal.
        let body = serde_json::json!({ "data": [{ "object": "model" }, { "id": "" }] });
        assert!(parse_discovered(provider_kind("groq"), &body).is_empty());
    }

    #[test]
    fn empty_data_is_an_empty_list() {
        let body = serde_json::json!({ "data": [] });
        assert!(parse_discovered(provider_kind("groq"), &body).is_empty());
    }

    #[test]
    fn discovery_urls_follow_each_gateways_layout() {
        assert_eq!(
            discovery_url(
                DiscoveryShape::OpenAiModels,
                "https://api.groq.com/openai/v1"
            ),
            "https://api.groq.com/openai/v1/models"
        );
        assert_eq!(
            discovery_url(
                DiscoveryShape::GoogleModels,
                "https://generativelanguage.googleapis.com/v1beta/openai/"
            ),
            "https://generativelanguage.googleapis.com/v1beta/openai/models"
        );
        // Ollama's tags live at the server root, not under the /v1 base.
        assert_eq!(
            discovery_url(DiscoveryShape::OllamaTags, "https://ollama.com/v1"),
            "https://ollama.com/api/tags"
        );
        // A base that never had the /v1 suffix is used as-is.
        assert_eq!(
            discovery_url(DiscoveryShape::OllamaTags, "http://localhost:11434"),
            "http://localhost:11434/api/tags"
        );
    }
}
