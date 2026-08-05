//! 云端 OpenAI 兼容大模型连接（OpenRouter、DeepSeek、ModelArts 等任意 HTTPS 服务）。

use super::ChatMessage;
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

pub(super) const DEFAULT_API_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
pub(super) const DEFAULT_MODEL: &str = "openai/gpt-5-mini";

/// 标准 OpenAI 兼容请求体，适配任意云端 chat/completions 服务。
#[derive(Serialize)]
struct ChatCompletionsRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    max_tokens: u32,
    temperature: f32,
    /// 推理模型专用开关，仅当配置开启时发送。
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
}

#[derive(Serialize)]
struct ThinkingConfig {
    r#type: &'static str,
}

fn build_request<'a>(
    model: &'a str,
    messages: &'a [ChatMessage],
    no_reasoning: bool,
) -> ChatCompletionsRequest<'a> {
    ChatCompletionsRequest {
        model,
        messages,
        max_tokens: 1024,
        temperature: 0.2,
        thinking: no_reasoning.then_some(ThinkingConfig { r#type: "disabled" }),
    }
}

pub(super) fn validate_url(url: &reqwest::Url) -> Result<(), String> {
    if url.scheme() != "https" {
        return Err("Cloud API URL must use HTTPS".to_string());
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
    no_reasoning: bool,
    messages: &[ChatMessage],
) -> Result<reqwest::Response, String> {
    let request = build_request(model, messages, no_reasoning);
    client
        .post(api_url)
        .bearer_auth(api_key)
        .json(&request)
        .send()
        .await
        .map_err(|error| format!("Cloud request failed: {error}"))
}

pub(super) fn resolve_route(value: &Value, configured_model: &str) -> (String, Option<String>) {
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .unwrap_or(configured_model)
        .to_string();
    let provider = value
        .get("provider")
        .and_then(Value::as_str)
        .filter(|provider| !provider.is_empty())
        .map(str::to_string);
    (model, provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_uses_standard_openai_compatible_fields() {
        let messages = [];
        let request = build_request(DEFAULT_MODEL, &messages, false);

        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["max_tokens"], 1024);
        assert!((value["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
        assert!(value.get("reasoning").is_none());
        assert!(value.get("max_completion_tokens").is_none());
        assert!(value.get("reasoning_effort").is_none());
        assert!(value.get("response_format").is_none());
        assert!(value.get("thinking").is_none());
    }

    #[test]
    fn request_sends_thinking_disabled_only_when_configured() {
        let messages = [];
        let request = build_request(DEFAULT_MODEL, &messages, true);

        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["thinking"]["type"], "disabled");
        let plain = build_request(DEFAULT_MODEL, &messages, false);
        assert!(
            serde_json::to_value(plain)
                .unwrap()
                .get("thinking")
                .is_none()
        );
    }

    #[test]
    fn validate_url_accepts_any_https_domain() {
        assert!(validate_url(&reqwest::Url::parse(DEFAULT_API_URL).unwrap()).is_ok());
        assert!(
            validate_url(
                &reqwest::Url::parse("https://api.example.com/v1/chat/completions").unwrap()
            )
            .is_ok()
        );
        assert!(
            validate_url(&reqwest::Url::parse("https://deepseek.com/v1/chat/completions").unwrap())
                .is_ok()
        );
        assert!(
            validate_url(
                &reqwest::Url::parse("http://api.example.com/v1/chat/completions").unwrap()
            )
            .is_err()
        );
    }
}
