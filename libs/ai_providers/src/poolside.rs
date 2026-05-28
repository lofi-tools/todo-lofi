use crate::{OpenAiCompatible, RealRunner};
use cersei::prelude::Auth;

pub fn provider() -> anyhow::Result<OpenAiCompatible> {
    let mut runner = RealRunner {};
    let key_file_path =
        String::from_utf8_lossy(runner.exec_cmd(["pool-key-file"])?.stdout.as_slice()).to_string();

    let key = std::fs::read_to_string(&key_file_path)?.trim().to_string();

    Ok(OpenAiCompatible {
        name: "poolside".to_string(),
        auth: Auth::ApiKey(key),
        base_url: "https://inference.poolside.ai/v1".to_string(),
        default_model: "poolside/laguna-xs.2".to_string(),
        client: reqwest::Client::new(),
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use cersei::{prelude::Provider, types::Message};

    #[tokio::test]
    #[ignore = "needs network + depends on live server"]
    async fn test_completion() -> anyhow::Result<()> {
        let provider = provider()?;
        assert_eq!(provider.name, "poolside");

        let request = cersei::provider::CompletionRequest {
            model: provider.default_model.clone(),
            messages: vec![Message::user("What model are you ?".to_string())],
            // system: Some("System prompt: You are 'Deepseek v4 Flash'.".into()),
            system: None,
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
            // dbg!(&event);
            accumulator.process_event(event);
        }
        let response = accumulator.into_response()?;
        let summary_text = response.message.get_all_text();

        dbg!(&summary_text);

        Ok(())
    }
}
