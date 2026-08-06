//! 云端 OpenAI-compatible Chat Completions 连接，支持任意 HTTPS 服务端点。

use super::{ChatMessage, CloudAuth, LlmConfig};
use reqwest::header::{HeaderName, HeaderValue};
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

pub(super) const DEFAULT_API_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
pub(super) const DEFAULT_MODEL: &str = "openai/gpt-5-mini";

/// 最小标准 OpenAI 兼容请求体。可选字段越少，供应商兼容性越高。
#[derive(Serialize)]
struct ChatCompletionsRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    /// 当前解析器消费完整 JSON 响应，因此显式关闭流式输出。
    stream: bool,
    /// MaaS V2、DeepSeek 等服务使用的推理开关。
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
    /// 华为 MaaS OpenAI-compatible 接口使用的推理开关。
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<ChatTemplateKwargs>,
}

#[derive(Serialize)]
struct ThinkingConfig {
    r#type: &'static str,
}

#[derive(Serialize)]
struct ChatTemplateKwargs {
    thinking: bool,
}

fn build_request<'a>(
    model: &'a str,
    messages: &'a [ChatMessage],
    no_reasoning: bool,
    api_url: &str,
) -> ChatCompletionsRequest<'a> {
    let huawei_openai_reasoning = no_reasoning && is_huawei_maas_openai_endpoint(api_url);
    ChatCompletionsRequest {
        model,
        messages,
        stream: false,
        thinking: (no_reasoning && !huawei_openai_reasoning)
            .then_some(ThinkingConfig { r#type: "disabled" }),
        chat_template_kwargs: huawei_openai_reasoning
            .then_some(ChatTemplateKwargs { thinking: false }),
    }
}

fn is_huawei_maas_openai_endpoint(api_url: &str) -> bool {
    reqwest::Url::parse(api_url).is_ok_and(|url| {
        let host = url.host_str().unwrap_or_default();
        (host == "modelarts-maas.com" || host.ends_with(".modelarts-maas.com"))
            && url.path().trim_end_matches('/') == "/openai/v1/chat/completions"
    })
}

pub(super) fn validate_url(url: &reqwest::Url) -> Result<(), String> {
    if url.scheme() != "https" {
        return Err("Cloud API URL must use HTTPS".to_string());
    }
    if url.host_str().is_none() {
        return Err("Cloud API URL must include a host".to_string());
    }
    if url.path().is_empty() || url.path() == "/" {
        return Err(
            "Cloud API URL must be the full Chat Completions endpoint, not a base URL".to_string(),
        );
    }
    if url.query_pairs().any(|(name, _)| {
        let normalized = name
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>();
        normalized != "apiversion"
    }) {
        return Err(
            "Cloud API URL only allows the non-secret api-version query parameter".to_string(),
        );
    }
    Ok(())
}

pub(super) fn validate_auth(
    auth_mode: CloudAuth,
    api_key_header: &str,
    api_key: &str,
) -> Result<(), String> {
    if auth_mode.requires_key() && api_key.is_empty() {
        return Err("Cloud API Key is required for the selected authentication mode".to_string());
    }
    if auth_mode == CloudAuth::ApiKeyHeader {
        if api_key_header.is_empty() {
            return Err("Cloud API Key Header is required".to_string());
        }
        HeaderName::from_bytes(api_key_header.as_bytes())
            .map_err(|_| "Cloud API Key Header is invalid".to_string())?;
        let normalized = api_key_header.to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "host"
                | "content-length"
                | "content-type"
                | "connection"
                | "transfer-encoding"
                | "cookie"
                | "set-cookie"
                | "proxy-authorization"
                | "proxy-connection"
                | "keep-alive"
                | "te"
                | "trailer"
                | "upgrade"
        ) {
            return Err("Cloud API Key Header is not allowed".to_string());
        }
    }
    if auth_mode.requires_key() {
        HeaderValue::from_str(api_key)
            .map_err(|_| "Cloud API Key contains invalid header characters".to_string())?;
    }
    Ok(())
}

pub(super) fn build_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        // 推理模型可能超过原来的 30 秒；任务仍可由 worker 立即 abort。
        // 取 60 秒，将三次自动重试的最坏等待控制在约三分钟。
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("Cannot create LLM client: {error}"))
}

pub(super) async fn send_request(
    client: &reqwest::Client,
    config: &LlmConfig,
    messages: &[ChatMessage],
) -> Result<reqwest::Response, String> {
    let request = build_http_request(client, config, messages)?;
    client
        .execute(request)
        .await
        .map_err(|error| format!("Cloud request failed: {error}"))
}

fn build_http_request(
    client: &reqwest::Client,
    config: &LlmConfig,
    messages: &[ChatMessage],
) -> Result<reqwest::Request, String> {
    let request = build_request(
        config.model(),
        messages,
        config.no_reasoning_enabled(),
        config.api_url(),
    );
    let mut builder = client.post(config.api_url());
    match config.auth_mode() {
        CloudAuth::Bearer => builder = builder.bearer_auth(config.api_key()),
        CloudAuth::ApiKeyHeader => {
            let header = HeaderName::from_bytes(config.api_key_header().as_bytes())
                .map_err(|_| "Cloud API Key Header is invalid".to_string())?;
            let mut value = HeaderValue::from_str(config.api_key())
                .map_err(|_| "Cloud API Key contains invalid header characters".to_string())?;
            value.set_sensitive(true);
            builder = builder.header(header, value);
        }
        CloudAuth::None => {}
    }
    builder
        .json(&request)
        .build()
        .map_err(|error| format!("Cannot build Cloud request: {error}"))
}

pub(super) fn resolve_route(
    value: &Value,
    configured_model: &str,
    api_url: &str,
) -> (String, Option<String>) {
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
        .or_else(|| {
            reqwest::Url::parse(api_url)
                .ok()?
                .host_str()
                .map(str::to_string)
        });
    (model, provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_body(request: &reqwest::Request) -> Value {
        let body = request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .expect("JSON request body must be buffered");
        serde_json::from_slice(body).expect("request body must be valid JSON")
    }

    #[test]
    fn request_uses_standard_openai_compatible_fields() {
        let messages = [];
        let request = build_request(DEFAULT_MODEL, &messages, false, DEFAULT_API_URL);

        let value = serde_json::to_value(request).unwrap();

        assert!(value.get("max_tokens").is_none());
        assert_eq!(value["stream"], false);
        assert!(value.get("temperature").is_none());
        assert!(value.get("reasoning").is_none());
        assert!(value.get("max_completion_tokens").is_none());
        assert!(value.get("reasoning_effort").is_none());
        assert!(value.get("response_format").is_none());
        assert!(value.get("thinking").is_none());
        assert!(value.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn request_sends_thinking_disabled_only_when_configured() {
        let messages = [];
        let request = build_request(DEFAULT_MODEL, &messages, true, DEFAULT_API_URL);

        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["thinking"]["type"], "disabled");
        assert!(value.get("chat_template_kwargs").is_none());
        let plain = build_request(DEFAULT_MODEL, &messages, false, DEFAULT_API_URL);
        assert!(
            serde_json::to_value(plain)
                .unwrap()
                .get("thinking")
                .is_none()
        );
    }

    #[test]
    fn huawei_openai_request_body_selects_the_documented_reasoning_field() {
        let messages = [];
        let api_url = "https://api.modelarts-maas.com/openai/v1/chat/completions";
        let request = build_request("openpangu-2.0-pro", &messages, true, api_url);

        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["chat_template_kwargs"]["thinking"], false);
        assert!(value.get("thinking").is_none());

        let plain = build_request("openpangu-2.0-pro", &messages, false, api_url);
        let plain = serde_json::to_value(plain).unwrap();
        assert!(plain.get("thinking").is_none());
        assert!(plain.get("chat_template_kwargs").is_none());

        let v2 = build_request(
            "openpangu-2.0-pro",
            &messages,
            true,
            "https://api.modelarts-maas.com/v2/chat/completions",
        );
        let v2 = serde_json::to_value(v2).unwrap();
        assert_eq!(v2["thinking"]["type"], "disabled");
        assert!(v2.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn huawei_openai_detection_accepts_regional_hosts_but_not_lookalikes() {
        assert!(is_huawei_maas_openai_endpoint(
            "https://api-ap-southeast-1.modelarts-maas.com/openai/v1/chat/completions"
        ));
        assert!(!is_huawei_maas_openai_endpoint(
            "https://evilmodelarts-maas.com/openai/v1/chat/completions"
        ));
        assert!(!is_huawei_maas_openai_endpoint(
            "https://api.modelarts-maas.com/v2/chat/completions"
        ));
    }

    #[test]
    fn huawei_openai_http_request_uses_documented_wire_contract() {
        let config = LlmConfig::new_unchecked(
            super::super::LlmBackend::Cloud,
            "test-secret".into(),
            "https://api.modelarts-maas.com/openai/v1/chat/completions".into(),
            "openpangu-2.0-pro".into(),
        )
        .no_reasoning(true);
        let request = build_http_request(&build_client().unwrap(), &config, &[]).unwrap();
        let body = request_body(&request);

        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(
            request.url().as_str(),
            "https://api.modelarts-maas.com/openai/v1/chat/completions"
        );
        assert_eq!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .unwrap(),
            "Bearer test-secret"
        );
        assert_eq!(body["model"], "openpangu-2.0-pro");
        assert_eq!(body["stream"], false);
        assert_eq!(body["chat_template_kwargs"]["thinking"], false);
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn huawei_maas_v2_http_request_keeps_v2_reasoning_contract() {
        let config = LlmConfig::new_unchecked(
            super::super::LlmBackend::Cloud,
            "test-secret".into(),
            "https://api.modelarts-maas.com/v2/chat/completions".into(),
            "openpangu-2.0-pro".into(),
        )
        .no_reasoning(true);
        let request = build_http_request(&build_client().unwrap(), &config, &[]).unwrap();
        let body = request_body(&request);

        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(
            request.url().as_str(),
            "https://api.modelarts-maas.com/v2/chat/completions"
        );
        assert_eq!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .unwrap(),
            "Bearer test-secret"
        );
        assert_eq!(body["stream"], false);
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn deepseek_http_request_uses_generic_reasoning_contract() {
        let config = LlmConfig::new_unchecked(
            super::super::LlmBackend::Cloud,
            "test-secret".into(),
            "https://api.deepseek.com/chat/completions".into(),
            "deepseek-v4-pro".into(),
        )
        .no_reasoning(true);
        let request = build_http_request(&build_client().unwrap(), &config, &[]).unwrap();
        let body = request_body(&request);

        assert_eq!(
            request.url().as_str(),
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .unwrap(),
            "Bearer test-secret"
        );
        assert_eq!(body["model"], "deepseek-v4-pro");
        assert_eq!(body["stream"], false);
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn azure_style_http_request_preserves_api_version_and_custom_header() {
        let config = LlmConfig::new_unchecked_with_auth(
            super::super::LlmBackend::Cloud,
            "test-secret".into(),
            "https://models.example.com/openai/deployments/demo/chat/completions?api-version=2026-01-01"
                .into(),
            "deployment-name".into(),
            CloudAuth::ApiKeyHeader,
            "api-key".into(),
        );
        let request = build_http_request(&build_client().unwrap(), &config, &[]).unwrap();
        let body = request_body(&request);

        assert_eq!(
            request.url().as_str(),
            "https://models.example.com/openai/deployments/demo/chat/completions?api-version=2026-01-01"
        );
        assert_eq!(request.headers().get("api-key").unwrap(), "test-secret");
        assert_eq!(
            request
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .unwrap(),
            "application/json"
        );
        assert!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .is_none()
        );
        assert_eq!(body["model"], "deployment-name");
        assert_eq!(body["stream"], false);
        assert!(body.get("thinking").is_none());
        assert!(body.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn validate_url_accepts_any_https_domain() {
        assert!(validate_url(&reqwest::Url::parse(DEFAULT_API_URL).unwrap()).is_ok());
        assert!(
            validate_url(
                &reqwest::Url::parse(
                    "https://api.example.com/v1/chat/completions?api-version=2026-01-01"
                )
                .unwrap()
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
        assert!(validate_url(&reqwest::Url::parse("https://api.example.com").unwrap()).is_err());
        for query in [
            "api_key=secret",
            "client_secret=secret",
            "credential=secret",
            "password=secret",
            "jwt=secret",
            "x-amz-signature=secret",
            "token_v2=secret",
            "preview=true",
        ] {
            assert!(
                validate_url(
                    &reqwest::Url::parse(&format!(
                        "https://api.example.com/chat/completions?{query}"
                    ))
                    .unwrap()
                )
                .is_err(),
                "unexpectedly accepted query {query}"
            );
        }
    }

    #[test]
    fn validates_generic_cloud_authentication_modes() {
        assert!(validate_auth(CloudAuth::Bearer, "", "secret").is_ok());
        assert!(validate_auth(CloudAuth::ApiKeyHeader, "api-key", "secret").is_ok());
        assert!(validate_auth(CloudAuth::ApiKeyHeader, "x-goog-api-key", "secret").is_ok());
        assert!(validate_auth(CloudAuth::None, "", "").is_ok());
        assert!(validate_auth(CloudAuth::Bearer, "", "").is_err());
        assert!(validate_auth(CloudAuth::ApiKeyHeader, "", "secret").is_err());
        for header in ["Content-Type", "Keep-Alive", "TE", "Trailer", "Upgrade"] {
            assert!(
                validate_auth(CloudAuth::ApiKeyHeader, header, "secret").is_err(),
                "unexpectedly accepted header {header}"
            );
        }
    }
}
