//! Model discovery over the OpenAI-compatible `/models` endpoint, used by the
//! agent's `/provider` explorer to show models the config doesn't list.

use anyhow::Context as _;

/// Fetch a provider's model ids from `{base_url}/models`, sorted and
/// de-duplicated.
pub async fn fetch_models(base_url: &str, api_key: &str) -> anyhow::Result<Vec<String>> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let resp = client
        .get(&url)
        .bearer_auth(api_key)
        .send()
        .await?
        .error_for_status()
        .with_context(|| format!("GET {url} failed"))?;
    let body: ModelsResponse = resp.json().await?;
    Ok(parse_model_ids(&body))
}

/// OpenAI-compatible `/models` response shape.
#[derive(serde::Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(serde::Deserialize)]
struct ModelEntry {
    id: String,
}

/// Extract, sort, and de-duplicate model ids from a parsed response.
fn parse_model_ids(resp: &ModelsResponse) -> Vec<String> {
    let mut models: Vec<String> = resp.data.iter().map(|m| m.id.clone()).collect();
    models.sort();
    models.dedup();
    models
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sorts_and_dedups_model_ids() {
        // Sample OpenAI-compatible /models response body.
        let body = r#"{
            "object": "list",
            "data": [
                { "id": "z-ai/glm-5.3", "object": "model" },
                { "id": "a-model", "object": "model" },
                { "id": "z-ai/glm-5.3", "object": "model" }
            ]
        }"#;
        let parsed: ModelsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parse_model_ids(&parsed), vec!["a-model", "z-ai/glm-5.3"]);
    }

    #[test]
    fn empty_data_is_an_empty_list() {
        let parsed: ModelsResponse = serde_json::from_str(r#"{"data": []}"#).unwrap();
        assert!(parse_model_ids(&parsed).is_empty());
    }
}
