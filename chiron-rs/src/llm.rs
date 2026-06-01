use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::{json, Value};

use crate::types::Provider;

/// Send a system+user message pair to the configured LLM provider.
/// Returns the assistant's text response.
pub async fn chat(client: &Client, provider: &Provider, system: &str, user: &str) -> Result<String> {
    match provider {
        Provider::Anthropic { api_key, model } => {
            let resp: Value = client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&json!({
                    "model": model,
                    "max_tokens": 1024,
                    "system": system,
                    "messages": [{"role": "user", "content": user}]
                }))
                .send()
                .await
                .context("Anthropic request failed")?
                .error_for_status()
                .context("Anthropic API error")?
                .json()
                .await
                .context("Anthropic response parse failed")?;

            Ok(resp["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string())
        }

        Provider::OpenAICompat { base_url, api_key, model } => {
            let url = format!(
                "{}/v1/chat/completions",
                base_url.trim_end_matches('/')
            );
            let resp: Value = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("content-type", "application/json")
                .json(&json!({
                    "model": model,
                    "messages": [
                        {"role": "system", "content": system},
                        {"role": "user",   "content": user}
                    ]
                }))
                .send()
                .await
                .context("OpenAI-compat request failed")?
                .error_for_status()
                .context("OpenAI-compat API error")?
                .json()
                .await
                .context("OpenAI-compat response parse failed")?;

            Ok(resp["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string())
        }
    }
}

pub fn provider_name(p: &Provider) -> &'static str {
    match p {
        Provider::Anthropic { .. }    => "anthropic",
        Provider::OpenAICompat { .. } => "openai-compat",
    }
}

pub fn model_name(p: &Provider) -> &str {
    match p {
        Provider::Anthropic { model, .. }    => model,
        Provider::OpenAICompat { model, .. } => model,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_names_are_stable() {
        let anth = Provider::Anthropic {
            api_key: "k".into(),
            model: "claude-sonnet-4-6".into(),
        };
        assert_eq!(provider_name(&anth), "anthropic");
        assert_eq!(model_name(&anth), "claude-sonnet-4-6");

        let compat = Provider::OpenAICompat {
            base_url: "https://api.groq.com/openai".into(),
            api_key: "k".into(),
            model: "llama3-70b-8192".into(),
        };
        assert_eq!(provider_name(&compat), "openai-compat");
    }
}
