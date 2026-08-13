use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde_json::{json, Value};

use crate::types::Provider;

/// Extract the assistant's text from an Anthropic `content` block array.
/// With extended thinking on, `content[0]` is a `"thinking"` block and the
/// reply text is a later `"text"` block, not index 0 — so this scans for
/// type `"text"` rather than assuming a fixed position.
fn anthropic_text(resp: &Value) -> Result<String> {
    resp["content"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|block| block["type"] == "text")
        .and_then(|block| block["text"].as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no text block in Anthropic response content: {resp}"))
}

/// One piece of a system prompt. `cache` marks it as a candidate for
/// Anthropic prompt caching (`cache_control: {"type": "ephemeral"}`) — set
/// it on blocks whose content is either constant for the whole session
/// (role framing, concept, textbook excerpt) or append-only (the
/// `Chiron:`/`You:` transcript, which only grows, so its prefix matches
/// what was cached on the previous turn). Leave it false on anything that
/// changes shape between calls in ways that don't share a prefix (short
/// stage-specific instructions, gap lists) — caching those has no reuse
/// to pay off and just spends the cache-write premium for nothing.
///
/// Anthropic silently no-ops `cache_control` on a block under its minimum
/// cacheable size (1024 tokens for Sonnet/Opus, 2048 for Haiku) rather
/// than erroring, so marking a block `cache: true` is always safe even
/// when it turns out to be too small to matter.
pub struct SystemBlock {
    pub text: String,
    pub cache: bool,
}

impl SystemBlock {
    pub fn cached(text: impl Into<String>) -> Self {
        Self { text: text.into(), cache: true }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Self { text: text.into(), cache: false }
    }
}

/// Send a system+user message pair to the configured LLM provider.
/// Returns the assistant's text response. SYSTEM is a list of blocks, not
/// one flat string, so the caller can mark which parts are worth caching
/// (see `SystemBlock`) — this matters most for Anthropic, whose
/// `cache_control` is opt-in per block; OpenAI-compatible endpoints that
/// support caching at all (e.g. the real OpenAI API) do it automatically
/// on any stable prefix, so blocks are still ordered stable-content-first
/// there even though `cache` itself is a no-op on that branch.
pub async fn chat(client: &Client, provider: &Provider, system: &[SystemBlock], user: &str) -> Result<String> {
    match provider {
        Provider::Anthropic { api_key, model, base_url } => {
            let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));
            let system_json: Vec<Value> = system.iter().map(|block| {
                let mut v = json!({"type": "text", "text": block.text});
                if block.cache {
                    v["cache_control"] = json!({"type": "ephemeral"});
                }
                v
            }).collect();
            let resp: Value = client
                .post(&url)
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&json!({
                    "model": model,
                    "max_tokens": 1024,
                    "system": system_json,
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

            anthropic_text(&resp)
        }

        Provider::OpenAICompat { base_url, api_key, model } => {
            let url = format!(
                "{}/v1/chat/completions",
                base_url.trim_end_matches('/')
            );
            let system_text = system.iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            let resp: Value = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("content-type", "application/json")
                .json(&json!({
                    "model": model,
                    "messages": [
                        {"role": "system", "content": system_text},
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
            base_url: "https://api.anthropic.com".into(),
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
