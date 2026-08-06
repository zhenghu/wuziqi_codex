//! 本地 OpenAI 兼容大模型连接。

use super::ChatMessage;
use serde::Serialize;
use serde_json::Value;
use std::net::IpAddr;
use std::time::Duration;

pub(super) const DEFAULT_API_URL: &str = "http://127.0.0.1:11434/v1/chat/completions";
pub(super) const DEFAULT_MODEL: &str = "qwen3:4b";

#[derive(Serialize)]
struct ChatCompletionsRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    max_tokens: u32,
    temperature: f32,
    reasoning_effort: &'static str,
    response_format: ResponseFormat,
}

#[derive(Serialize)]
struct ResponseFormat {
    r#type: &'static str,
}

fn build_request<'a>(model: &'a str, messages: &'a [ChatMessage]) -> ChatCompletionsRequest<'a> {
    ChatCompletionsRequest {
        model,
        messages,
        max_tokens: 256,
        temperature: 0.0,
        reasoning_effort: "none",
        response_format: ResponseFormat {
            r#type: "json_object",
        },
    }
}

pub(super) fn validate_url(url: &reqwest::Url) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Local API URL must use HTTP or HTTPS".to_string());
    }
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let loopback = host.parse::<IpAddr>().is_ok_and(|address| {
        address == IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
            || address == IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
    });
    if !loopback {
        return Err(
            "Local API URL must use a numeric loopback address (127.0.0.1 or ::1)".to_string(),
        );
    }
    if url.path() != "/v1/chat/completions" {
        return Err("Local API URL must use /v1/chat/completions".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("Local API URL must not contain a query or fragment".to_string());
    }
    Ok(())
}

pub(super) fn build_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        // 回环请求必须直连，不能受 HTTP_PROXY/HTTPS_PROXY 环境变量影响。
        .no_proxy()
        .build()
        .map_err(|error| format!("Cannot create local LLM client: {error}"))
}

pub(super) async fn send_request(
    client: &reqwest::Client,
    api_url: &str,
    model: &str,
    messages: &[ChatMessage],
) -> Result<reqwest::Response, String> {
    let request = build_request(model, messages);
    client
        .post(api_url)
        .json(&request)
        .send()
        .await
        .map_err(|error| format!("Local request failed: {error}"))
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
        .map(str::to_string)
        .or_else(|| Some("Local".to_string()));
    (model, provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_uses_only_compatible_fields_and_json_output() {
        let messages = [];
        let request = build_request(DEFAULT_MODEL, &messages);

        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["max_tokens"], 256);
        assert_eq!(value["temperature"], 0.0);
        assert_eq!(value["reasoning_effort"], "none");
        assert_eq!(value["response_format"]["type"], "json_object");
        assert!(value.get("max_completion_tokens").is_none());
        assert!(value.get("reasoning").is_none());
    }
}
