//! OpenRouter 云端大模型连接。

use super::ChatMessage;
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

pub(super) const DEFAULT_API_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
pub(super) const DEFAULT_MODEL: &str = "openai/gpt-5-mini";

#[derive(Serialize)]
struct ChatCompletionsRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    max_completion_tokens: u32,
    temperature: f32,
    reasoning: ReasoningConfig,
}

#[derive(Serialize)]
struct ReasoningConfig {
    effort: &'static str,
    exclude: bool,
}

fn build_request<'a>(model: &'a str, messages: &'a [ChatMessage]) -> ChatCompletionsRequest<'a> {
    ChatCompletionsRequest {
        model,
        messages,
        max_completion_tokens: 1_024,
        temperature: 0.2,
        reasoning: ReasoningConfig {
            effort: "minimal",
            exclude: true,
        },
    }
}

pub(super) fn validate_url(url: &reqwest::Url) -> Result<(), String> {
    if url.scheme() != "https" {
        return Err("OpenRouter API URL must use HTTPS".to_string());
    }
    if url.host_str() != Some("openrouter.ai") {
        return Err("OpenRouter API URL host must be openrouter.ai".to_string());
    }
    if url.path() != "/api/v1/chat/completions" {
        return Err("OpenRouter API URL must use /api/v1/chat/completions".to_string());
    }
    Ok(())
}

pub(super) fn build_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("Cannot create LLM client: {error}"))
}

pub(super) async fn send_request(
    client: &reqwest::Client,
    api_url: &str,
    api_key: &str,
    model: &str,
    messages: &[ChatMessage],
) -> Result<reqwest::Response, String> {
    let request = build_request(model, messages);
    client
        .post(api_url)
        .bearer_auth(api_key)
        .header("X-OpenRouter-Title", "Wuziqi")
        .json(&request)
        .send()
        .await
        .map_err(|error| format!("OpenRouter request failed: {error}"))
}

pub(super) fn resolve_route(value: &Value) -> Result<(String, Option<String>), String> {
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| "OpenRouter response is missing the routed model".to_string())?
        .to_string();
    let provider = value
        .get("provider")
        .and_then(Value::as_str)
        .filter(|provider| !provider.is_empty())
        .map(str::to_string);
    Ok((model, provider))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_reserves_completion_tokens_and_limits_reasoning() {
        let messages = [];
        let request = build_request(DEFAULT_MODEL, &messages);

        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["max_completion_tokens"], 1_024);
        assert!((value["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
        assert_eq!(value["reasoning"]["effort"], "minimal");
        assert_eq!(value["reasoning"]["exclude"], true);
        assert!(value.get("max_tokens").is_none());
        assert!(value.get("reasoning_effort").is_none());
        assert!(value.get("response_format").is_none());
    }
}
