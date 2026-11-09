use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const MODELS_DEV_URL: &str = "https://models.dev/catalog.json?type=all";
const FALLBACK_URL: &str = "https://models.dev/api.json?type=all";
const USER_AGENT: &str = "providers-catalog-ingest/0.1 (+https://models.dev)";

const LAB_ALIASES: &[(&str, &str)] = &[
    ("deepseek-ai", "deepseek"),
    ("meta-llama", "meta"),
    ("mistralai", "mistral"),
    ("moonshotai", "moonshot"),
    ("x-ai", "xai"),
    ("z-ai", "zai"),
    ("zhipuai", "zai"),
    ("google-vertex", "google"),
    ("google-vertex-anthropic", "anthropic"),
    ("qwen-ai", "qwen"),
];

const PROVIDER_LABS: &[(&str, &str)] = &[
    ("anthropic", "anthropic"),
    ("openai", "openai"),
    ("azure", "openai"),
    ("google", "google"),
    ("google-vertex", "google"),
    ("google-vertex-anthropic", "anthropic"),
    ("deepseek", "deepseek"),
    ("meta", "meta"),
    ("mistral", "mistral"),
    ("moonshotai", "moonshot"),
    ("moonshotai-cn", "moonshot"),
    ("xai", "xai"),
    ("zai", "zai"),
    ("qwen", "qwen"),
    ("alibaba", "qwen"),
    ("alibaba-cn", "qwen"),
];

#[derive(Debug, Deserialize)]
struct Catalog {
    #[serde(default)]
    providers: HashMap<String, Provider>,
}

#[derive(Debug, Deserialize)]
struct Provider {
    id: String,
    name: String,
    #[serde(default)]
    npm: Option<String>,
    #[serde(default)]
    api: Option<String>,
    #[serde(default)]
    doc: Option<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    models: HashMap<String, Value>,
}

#[derive(Debug, Clone)]
pub struct ProviderRow {
    pub provider_id: String,
    pub name: String,
    pub npm: Option<String>,
    pub api_url: Option<String>,
    pub doc_url: Option<String>,
    pub env_keys: Vec<String>,
    pub source_record: String,
}

#[derive(Debug, Clone)]
pub struct ModelRow {
    pub provider_id: String,
    pub model_id: String,
    pub model_type: Option<String>,
    pub canonical_model_id: String,
    pub name: Option<String>,
    pub family: Option<String>,
    pub description: Option<String>,
    pub release_date: Option<String>,
    pub last_updated: Option<String>,
    pub knowledge_date: Option<String>,
    pub open_weights: Option<bool>,
    pub attachment: Option<bool>,
    pub reasoning: Option<bool>,
    pub tool_call: Option<bool>,
    pub structured_output: Option<bool>,
    pub temperature: Option<bool>,
    pub interleaved: Option<String>,
    pub context_limit: Option<i64>,
    pub input_limit: Option<i64>,
    pub output_limit: Option<i64>,
    pub cost_input: Option<f64>,
    pub cost_output: Option<f64>,
    pub cost_reasoning: Option<f64>,
    pub cost_cache_read: Option<f64>,
    pub cost_cache_write: Option<f64>,
    pub cost_input_audio: Option<f64>,
    pub cost_output_audio: Option<f64>,
    pub modalities_input: Vec<String>,
    pub modalities_output: Vec<String>,
    pub status: Option<String>,
    pub source_record: String,
}

#[derive(Debug)]
pub struct FetchedCatalog {
    pub providers: Vec<ProviderRow>,
    pub models: Vec<ModelRow>,
    pub fetched_at: String,
    pub content_sha256: String,
    pub etag: Option<String>,
    pub source_url: String,
}

pub async fn fetch(client: &reqwest::Client) -> Result<FetchedCatalog> {
    let primary = fetch_one(client, MODELS_DEV_URL).await;
    let (body, url, etag) = match primary {
        Ok(value) => value,
        Err(primary_error) => match fetch_one(client, FALLBACK_URL).await {
            Ok(value) => value,
            Err(fallback_error) => {
                return Err(anyhow::anyhow!(
                    "models.dev failed at both endpoints: {primary_error}; {fallback_error}"
                ));
            }
        },
    };
    let fetched_at = chrono::Utc::now()
        .format("%Y-%m-%d %H:%M:%S%.f")
        .to_string();
    let content_sha256 = hex_digest(body.as_bytes());
    let catalog = parse(&body)?;

    let mut providers = Vec::with_capacity(catalog.providers.len());
    let mut models = Vec::new();
    for (provider_key, provider) in &catalog.providers {
        providers.push(ProviderRow {
            provider_id: provider.id.clone(),
            name: provider.name.clone(),
            npm: provider.npm.clone(),
            api_url: provider.api.clone(),
            doc_url: provider.doc.clone(),
            env_keys: provider.env.clone(),
            source_record: provider_key.clone(),
        });
        for (model_key, model) in &provider.models {
            models.push(map_model(&provider.id, provider_key, model_key, model));
        }
    }

    // JSON object order is not meaningful, and the first model seen names a
    // new canonical row. Sort so a rebuild of the same catalog is identical.
    providers.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
    models.sort_by(|left, right| {
        (left.provider_id.as_str(), left.model_id.as_str())
            .cmp(&(right.provider_id.as_str(), right.model_id.as_str()))
    });

    Ok(FetchedCatalog {
        providers,
        models,
        fetched_at,
        content_sha256,
        etag,
        source_url: url,
    })
}

async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
) -> Result<(String, String, Option<String>)> {
    let response = client
        .get(url)
        .header("user-agent", USER_AGENT)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?;
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if !response.status().is_success() {
        bail!("models.dev returned HTTP {}", response.status());
    }
    let body = response.text().await?;
    Ok((body, url.to_string(), etag))
}

fn parse(body: &str) -> Result<Catalog> {
    let value: Value = serde_json::from_str(body).context("models.dev returned invalid JSON")?;
    let provider_map = match value.get("providers") {
        Some(providers) => providers.clone(),
        None => value,
    };
    let providers: HashMap<String, Provider> =
        serde_json::from_value(provider_map).context("models.dev provider map is invalid")?;
    Ok(Catalog { providers })
}

fn map_model(provider_id: &str, provider_key: &str, model_key: &str, model: &Value) -> ModelRow {
    let model_id = string_at(model, "id").unwrap_or_else(|| model_key.to_string());
    let canonical_model_id = canonical_model_id_for(
        provider_id,
        &model_id,
        string_at(model, "canonical_model_id"),
    );
    let limits = model.get("limit");
    let costs = model.get("cost");
    let modalities = model.get("modalities");

    ModelRow {
        provider_id: provider_id.to_string(),
        model_id,
        model_type: string_at(model, "type"),
        canonical_model_id,
        name: string_at(model, "name"),
        family: string_at(model, "family"),
        description: string_at(model, "description"),
        release_date: string_at(model, "release_date"),
        last_updated: string_at(model, "last_updated"),
        knowledge_date: string_at(model, "knowledge"),
        open_weights: bool_at(model, "open_weights"),
        attachment: bool_at(model, "attachment"),
        reasoning: bool_at(model, "reasoning"),
        tool_call: bool_at(model, "tool_call"),
        structured_output: bool_at(model, "structured_output"),
        temperature: bool_at(model, "temperature"),
        interleaved: model
            .get("interleaved")
            .filter(|value| !value.is_null())
            .map(|value| value.to_string()),
        context_limit: number_at(limits, "context").map(|value| value as i64),
        input_limit: number_at(limits, "input").map(|value| value as i64),
        output_limit: number_at(limits, "output").map(|value| value as i64),
        cost_input: number_at(costs, "input"),
        cost_output: number_at(costs, "output"),
        cost_reasoning: number_at(costs, "reasoning"),
        cost_cache_read: number_at(costs, "cache_read"),
        cost_cache_write: number_at(costs, "cache_write"),
        cost_input_audio: number_at(costs, "input_audio"),
        cost_output_audio: number_at(costs, "output_audio"),
        modalities_input: strings_at(modalities, "input"),
        modalities_output: strings_at(modalities, "output"),
        status: string_at(model, "status")
            .filter(|status| matches!(status.as_str(), "alpha" | "beta" | "deprecated")),
        source_record: format!("{provider_key}/{model_key}"),
    }
}

fn string_at(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_string)
}

fn number_at(value: Option<&Value>, key: &str) -> Option<f64> {
    let value = value?.get(key)?;
    if let Some(number) = value.as_f64() {
        return Some(number);
    }
    value.as_str()?.trim().parse::<f64>().ok()
}

fn bool_at(value: &Value, key: &str) -> Option<bool> {
    value.get(key)?.as_bool()
}

fn strings_at(value: Option<&Value>, key: &str) -> Vec<String> {
    value
        .and_then(|record| record.get(key))
        .and_then(|items| items.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub fn canonical_model_id_for(
    provider_id: &str,
    model_id: &str,
    source_canonical_id: Option<String>,
) -> String {
    let raw = source_canonical_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(model_id);
    let segments: Vec<&str> = raw
        .split('/')
        .filter(|segment| !segment.trim().is_empty())
        .collect();
    let slug = normalize_slug(segments.last().copied().unwrap_or(raw));
    // A leading segment can normalize away (Cloudflare ids look like
    // @cf/cloudflare/clef), so take the first segment that names a lab.
    let lab = if segments.len() > 1 {
        segments[..segments.len() - 1]
            .iter()
            .map(|segment| lab_alias(&normalize_slug(segment)))
            .find(|lab| !lab.is_empty())
            .unwrap_or_else(|| provider_lab(provider_id))
    } else {
        provider_lab(provider_id)
    };
    format!("{lab}/{slug}")
}

pub fn normalize_slug(value: &str) -> String {
    let lowered = value.to_lowercase();
    let without_suffix = lowered
        .trim_end_matches(":free")
        .trim_end_matches(":beta")
        .trim_end_matches(":extended")
        .trim_end_matches(":thinking")
        .trim_end_matches(":online");
    without_suffix
        .split('@')
        .next()
        .unwrap_or(without_suffix)
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '.' || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .replace('.', "-")
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn lab_alias(lab: &str) -> String {
    LAB_ALIASES
        .iter()
        .find(|(alias, _)| *alias == lab)
        .map(|(_, target)| (*target).to_string())
        .unwrap_or_else(|| lab.to_string())
}

fn provider_lab(provider_id: &str) -> String {
    PROVIDER_LABS
        .iter()
        .find(|(provider, _)| *provider == provider_id)
        .map(|(_, lab)| (*lab).to_string())
        .unwrap_or_else(|| lab_alias(&normalize_slug(provider_id)))
}

pub fn family_id_for(canonical_model_id: &str, family_ids: &[String]) -> String {
    let slug = canonical_model_id
        .split('/')
        .next_back()
        .unwrap_or(canonical_model_id);
    family_ids
        .iter()
        .filter(|family_id| !family_id.is_empty() && slug.contains(family_id.as_str()))
        .max_by_key(|family_id| family_id.len())
        .cloned()
        .unwrap_or_else(|| {
            canonical_model_id
                .split('/')
                .next()
                .unwrap_or(canonical_model_id)
                .to_string()
        })
}

pub fn humanize(value: &str) -> String {
    value
        .split('-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_source_canonical_id_when_present() {
        assert_eq!(
            canonical_model_id_for(
                "opencode-go",
                "deepseek-v4.1",
                Some("deepseek/deepseek-v4.1".into())
            ),
            "deepseek/deepseek-v4-1"
        );
    }

    #[test]
    fn falls_back_to_the_provider_lab() {
        assert_eq!(
            canonical_model_id_for("anthropic", "claude-opus-5.5", None),
            "anthropic/claude-opus-5-5"
        );
    }

    #[test]
    fn normalizes_free_suffixes_and_dots() {
        assert_eq!(normalize_slug("gpt-5.5:free"), "gpt-5-5");
    }

    #[test]
    fn skips_lab_segments_that_normalize_away() {
        assert_eq!(
            canonical_model_id_for("cloudflare-workers-ai", "@cf/cloudflare/clef", None),
            "cloudflare/clef"
        );
    }

    #[test]
    fn ignores_the_empty_family_id() {
        let families = vec![String::new(), "opencode".to_string()];
        assert_eq!(
            family_id_for("opencode/muse-ember-1-0", &families),
            "opencode"
        );
    }

    #[test]
    fn picks_the_longest_matching_family() {
        let families = vec!["gpt".to_string(), "gpt-5".to_string()];
        assert_eq!(family_id_for("openai/gpt-5-6", &families), "gpt-5");
    }
}
