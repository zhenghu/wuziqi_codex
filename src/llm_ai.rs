//! 通过 OpenRouter Chat Completions API 从战术引擎筛选出的合法点中选择落子。

use crate::game::{BOARD, Cell};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const CONFIG_FILE_NAME: &str = "llm_config.json";
pub(crate) const DEFAULT_API_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
pub(crate) const DEFAULT_MODEL: &str = "openai/gpt-5-mini";
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_ERROR_TEXT_CHARS: usize = 512;

#[derive(Debug)]
pub(crate) enum ConfigError {
    Path(String),
    Invalid(String),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
}

impl ConfigError {
    fn io(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(message) | Self::Invalid(message) => formatter.write_str(message),
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "Cannot {operation} {}: {source}", path.display()),
            Self::Json { path, source } => {
                write!(formatter, "Invalid JSON in {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::Path(_) | Self::Invalid(_) => None,
        }
    }
}

type ConfigResult<T> = Result<T, ConfigError>;

pub(crate) fn config_path() -> ConfigResult<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| ConfigError::Path("Cannot determine the user home directory".into()))?;
        Ok(PathBuf::from(home)
            .join("Library/Application Support/Wuziqi")
            .join(CONFIG_FILE_NAME))
    }

    #[cfg(target_os = "windows")]
    {
        let app_data = std::env::var_os("APPDATA").ok_or_else(|| {
            ConfigError::Path("Cannot determine the application data directory".into())
        })?;
        Ok(PathBuf::from(app_data)
            .join("Wuziqi")
            .join(CONFIG_FILE_NAME))
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .ok_or_else(|| {
                ConfigError::Path("Cannot determine the user configuration directory".into())
            })?;
        Ok(base.join("wuziqi").join(CONFIG_FILE_NAME))
    }
}

fn temporary_config_path(path: &Path) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(CONFIG_FILE_NAME);
    path.with_file_name(format!(".{file_name}.tmp-{}-{nonce}", std::process::id()))
}

#[cfg(not(target_os = "windows"))]
fn replace_config_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(target_os = "windows")]
fn replace_config_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    if !destination.exists() {
        return std::fs::rename(source, destination);
    }

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // ReplaceFileW preserves an existing valid destination until the replacement
    // succeeds, unlike remove-then-rename.
    let replaced = unsafe {
        windows_sys::Win32::Storage::FileSystem::ReplaceFileW(
            destination.as_ptr(),
            source.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn legacy_config_path() -> PathBuf {
    PathBuf::from(CONFIG_FILE_NAME)
}

fn migrated_legacy_config_path(legacy: &Path) -> PathBuf {
    legacy.with_file_name(format!("{CONFIG_FILE_NAME}.migrated"))
}

#[cfg(unix)]
fn secure_legacy_file(path: &Path) {
    if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        eprintln!("无法限制旧配置文件 {} 的权限: {error}", path.display());
    }
}

#[cfg(not(unix))]
fn secure_legacy_file(_path: &Path) {}

pub(crate) fn config_exists() -> bool {
    config_path().is_ok_and(|path| path.exists()) || legacy_config_path().exists()
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct LlmConfig {
    api_key: String,
    api_url: String,
    model: String,
}

#[derive(Debug)]
pub(crate) struct LlmMove {
    pub(crate) position: (usize, usize),
    model: String,
    provider: Option<String>,
}

impl LlmMove {
    pub(crate) fn route_label(&self) -> String {
        match self.provider.as_deref() {
            Some(provider) if !provider.is_empty() => format!("{} via {provider}", self.model),
            _ => self.model.clone(),
        }
    }
}

impl LlmConfig {
    pub(crate) fn new(api_key: String, api_url: String, model: String) -> ConfigResult<Self> {
        let api_key = api_key.trim().to_string();
        let api_url = api_url.trim().trim_end_matches('/').to_string();
        let model = model.trim().to_string();
        if api_key.is_empty() {
            return Err(ConfigError::Invalid(
                "OpenRouter API Key is required".to_string(),
            ));
        }
        if model.is_empty() {
            return Err(ConfigError::Invalid(
                "OpenRouter model name is required".to_string(),
            ));
        }
        let parsed = reqwest::Url::parse(&api_url)
            .map_err(|error| ConfigError::Invalid(format!("Invalid API URL: {error}")))?;
        if parsed.scheme() != "https" {
            return Err(ConfigError::Invalid("API URL must use HTTPS".to_string()));
        }
        if parsed.host_str() != Some("openrouter.ai") {
            return Err(ConfigError::Invalid(
                "API URL host must be openrouter.ai".to_string(),
            ));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(ConfigError::Invalid(
                "API URL must not contain credentials".to_string(),
            ));
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(ConfigError::Invalid(
                "API URL must not contain a query or fragment".to_string(),
            ));
        }
        if parsed.path() != "/api/v1/chat/completions" {
            return Err(ConfigError::Invalid(
                "OpenRouter API URL must use /api/v1/chat/completions".to_string(),
            ));
        }
        Ok(Self {
            api_key,
            api_url,
            model,
        })
    }

    #[cfg(test)]
    pub(crate) fn new_unchecked(api_key: String, api_url: String, model: String) -> Self {
        Self {
            api_key,
            api_url,
            model,
        }
    }

    pub(crate) fn load() -> ConfigResult<Self> {
        let current = config_path()?;
        let legacy = legacy_config_path();
        Self::load_with_paths(&current, &legacy)
    }

    fn load_with_paths(current: &Path, legacy: &Path) -> ConfigResult<Self> {
        if current.exists() {
            let (config, repaired) = Self::read_from_path(current)?;
            if repaired {
                config.save_to_path(current)?;
            }
            return Ok(config);
        }
        if legacy.exists() {
            // Never modify the legacy source. Apply repairs in memory and write
            // the final configuration directly to the system location.
            let (config, _) = Self::read_from_path(legacy)?;
            config.save_to_path(current)?;
            let archived = migrated_legacy_config_path(legacy);
            if archived.exists() {
                secure_legacy_file(legacy);
                secure_legacy_file(&archived);
                eprintln!(
                    "配置已迁移到 {}；归档文件 {} 已存在，因此保留旧文件 {}",
                    current.display(),
                    archived.display(),
                    legacy.display()
                );
            } else {
                secure_legacy_file(legacy);
                if let Err(error) = std::fs::rename(legacy, &archived) {
                    eprintln!(
                        "配置已迁移到 {}，但无法将旧文件 {} 归档为 {}: {error}",
                        current.display(),
                        legacy.display(),
                        archived.display()
                    );
                } else {
                    secure_legacy_file(&archived);
                }
            }
            return Ok(config);
        }
        Self::read_from_path(current).map(|(config, _)| config)
    }

    fn read_from_path(path: &Path) -> ConfigResult<(Self, bool)> {
        let text =
            std::fs::read_to_string(path).map_err(|error| ConfigError::io("read", path, error))?;
        let mut raw: Self = serde_json::from_str(&text).map_err(|source| ConfigError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        let repaired = repair_paste_artifact(&raw.api_key);
        let changed = repaired != raw.api_key;
        raw.api_key = repaired;
        let config = Self::new(raw.api_key, raw.api_url, raw.model)?;
        Ok((config, changed))
    }

    pub(crate) fn save(&self) -> ConfigResult<()> {
        self.save_to_path(&config_path()?)
    }

    fn save_to_path(&self, path: &Path) -> ConfigResult<()> {
        let json = serde_json::to_string_pretty(self).map_err(|error| {
            ConfigError::Invalid(format!("Cannot serialize {}: {error}", path.display()))
        })?;
        let directory = path.parent().ok_or_else(|| {
            ConfigError::Path(format!("Invalid configuration path: {}", path.display()))
        })?;
        std::fs::create_dir_all(directory)
            .map_err(|error| ConfigError::io("create", directory, error))?;
        #[cfg(unix)]
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| ConfigError::io("secure", directory, error))?;
        let temporary = temporary_config_path(path);
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let result = (|| {
            let mut file = options
                .open(&temporary)
                .map_err(|error| ConfigError::io("create", &temporary, error))?;
            file.write_all(json.as_bytes())
                .and_then(|_| file.write_all(b"\n"))
                .and_then(|_| file.sync_all())
                .map_err(|error| ConfigError::io("write", &temporary, error))?;
            drop(file);
            #[cfg(unix)]
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| ConfigError::io("secure", &temporary, error))?;
            replace_config_file(&temporary, path)
                .map_err(|error| ConfigError::io("replace", path, error))?;
            #[cfg(unix)]
            std::fs::File::open(directory)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| ConfigError::io("sync", directory, error))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }

    pub(crate) fn api_url(&self) -> &str {
        &self.api_url
    }

    pub(crate) fn model(&self) -> &str {
        &self.model
    }
}

/// 修复旧版配置页在 Cmd+V 后错误追加的单个字符 `v`。
fn repair_paste_artifact(api_key: &str) -> String {
    const PREFIX: &str = "sk-or-v1-";
    let Some(without_v) = api_key.strip_suffix('v') else {
        return api_key.to_string();
    };
    let Some(secret) = without_v.strip_prefix(PREFIX) else {
        return api_key.to_string();
    };
    if secret.len() == 64 && secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        without_v.to_string()
    } else {
        api_key.to_string()
    }
}

#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct ChatCompletionsRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage>,
    max_completion_tokens: u32,
    temperature: f32,
    reasoning: ReasoningConfig,
}

#[derive(Serialize)]
struct ReasoningConfig {
    effort: &'static str,
    exclude: bool,
}

fn board_text(board: &[[Cell; BOARD]; BOARD]) -> String {
    let mut text = String::with_capacity(BOARD * (BOARD + 1));
    for row in board {
        for cell in row {
            text.push(match cell {
                Cell::Empty => '.',
                Cell::Black => 'X',
                Cell::White => 'O',
            });
        }
        text.push('\n');
    }
    text
}

fn response_text(value: &Value) -> Option<String> {
    let content = value
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    content.as_array()?.iter().find_map(|part| {
        part.get("text")?
            .as_str()
            .map(std::string::ToString::to_string)
    })
}

fn api_error_message(value: &Value) -> Option<&str> {
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
}

fn parse_move(text: &str) -> Option<(usize, usize)> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        let x = value.get("x")?.as_u64()? as usize;
        let y = value.get("y")?.as_u64()? as usize;
        return Some((x, y));
    }

    let values: Vec<_> = trimmed
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<usize>().ok())
        .collect();
    (values.len() == 2).then(|| (values[0], values[1]))
}

pub(crate) async fn request_move(
    client: &reqwest::Client,
    config: &LlmConfig,
    board: &[[Cell; BOARD]; BOARD],
    candidates: &[(usize, usize)],
) -> Result<LlmMove, String> {
    if candidates.is_empty() {
        return Err("没有合法候选点".to_string());
    }

    let prompt = format!(
        "你执白棋 O，对手执黑棋 X，坐标从 0 到 14，格式为 (x,y)，左上角是 (0,0)。\n\
         当前棋盘（每行对应 y=0..14）：\n{}\n\
         战术引擎给出的合法候选点：{:?}\n\
         综合进攻、防守、后续威胁和中心控制，选出最佳一手。只能从候选点中选择。",
        board_text(board),
        candidates
    );
    let request = ChatCompletionsRequest {
        model: &config.model,
        messages: vec![
            ChatMessage {
                role: "system",
                content: "你是五子棋专家。只输出一个 JSON 对象，例如 {\"x\":7,\"y\":7}，不要解释。"
                    .to_string(),
            },
            ChatMessage {
                role: "user",
                content: prompt,
            },
        ],
        max_completion_tokens: 1_024,
        temperature: 0.2,
        reasoning: ReasoningConfig {
            effort: "minimal",
            exclude: true,
        },
    };

    let response = client
        .post(&config.api_url)
        .bearer_auth(&config.api_key)
        .header("X-OpenRouter-Title", "Wuziqi")
        .json(&request)
        .send()
        .await
        .map_err(|error| format!("OpenRouter request failed: {error}"))?;
    let status = response.status();
    let body = read_limited_body(response).await?;
    let parsed = serde_json::from_str::<Value>(&body);
    if !status.is_success() {
        let detail = parsed
            .as_ref()
            .ok()
            .and_then(api_error_message)
            .unwrap_or("unknown API error");
        return Err(format!(
            "OpenRouter HTTP {}: {}",
            status.as_u16(),
            truncate_text(detail, MAX_ERROR_TEXT_CHARS)
        ));
    }
    let value = parsed.map_err(|error| format!("OpenRouter returned invalid JSON: {error}"))?;
    if let Some(error) = api_error_message(&value) {
        return Err(format!(
            "OpenRouter error: {}",
            truncate_text(error, MAX_ERROR_TEXT_CHARS)
        ));
    }
    let text = response_text(&value).ok_or_else(|| {
        let finish_reason = value
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let reasoning_tokens = value
            .pointer("/usage/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        format!(
            "OpenRouter response has no text (finish_reason={finish_reason}, reasoning_tokens={reasoning_tokens})"
        )
    })?;
    let chosen = parse_move(&text).ok_or_else(|| {
        format!(
            "无法解析模型落点: {}",
            truncate_text(&text, MAX_ERROR_TEXT_CHARS)
        )
    })?;
    if !candidates.contains(&chosen) {
        return Err(format!("模型返回了候选集外的落点: {chosen:?}"));
    }
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
    Ok(LlmMove {
        position: chosen,
        model,
        provider,
    })
}

pub(crate) fn build_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("Cannot create OpenRouter client: {error}"))
}

async fn read_limited_body(mut response: reqwest::Response) -> Result<String, String> {
    let mut body = Vec::new();
    loop {
        let next_chunk = response
            .chunk()
            .await
            .map_err(|error| format!("Cannot read OpenRouter response: {error}"))?;
        let Some(chunk) = next_chunk else {
            break;
        };
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(format!(
                "OpenRouter response exceeds {MAX_RESPONSE_BYTES} bytes"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|error| format!("OpenRouter response is not UTF-8: {error}"))
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    format!(
        "{}...",
        text.chars()
            .take(max_chars.saturating_sub(3))
            .collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_and_plain_coordinates() {
        assert_eq!(parse_move(r#"{"x":7,"y":8}"#), Some((7, 8)));
        assert_eq!(parse_move("(12, 3)"), Some((12, 3)));
        assert_eq!(parse_move("选择 (12, 3)，因为它最好"), Some((12, 3)));
        assert_eq!(parse_move("7 8 9"), None);
    }

    #[test]
    fn extracts_openrouter_chat_completion_text() {
        let value: Value = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "{\"x\":1,\"y\":2}"}}]
        });
        assert_eq!(response_text(&value).as_deref(), Some("{\"x\":1,\"y\":2}"));
    }

    #[test]
    fn labels_the_route_reported_by_openrouter() {
        let routed_move = LlmMove {
            position: (1, 2),
            model: "anthropic/claude-sonnet-4".to_string(),
            provider: Some("Anthropic".to_string()),
        };
        assert_eq!(
            routed_move.route_label(),
            "anthropic/claude-sonnet-4 via Anthropic"
        );
    }

    #[test]
    fn chat_request_limits_reasoning_and_reserves_completion_tokens() {
        let request = ChatCompletionsRequest {
            model: DEFAULT_MODEL,
            messages: vec![],
            max_completion_tokens: 1_024,
            temperature: 0.2,
            reasoning: ReasoningConfig {
                effort: "minimal",
                exclude: true,
            },
        };
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["max_completion_tokens"], 1_024);
        assert_eq!(value["reasoning"]["effort"], "minimal");
        assert_eq!(value["reasoning"]["exclude"], true);
        assert!(value.get("max_tokens").is_none());
    }

    #[test]
    fn parses_json_configuration() {
        let raw = r#"{
            "api_key": "sk-or-test",
            "api_url": "https://openrouter.ai/api/v1/chat/completions",
            "model": "openai/gpt-5-mini"
        }"#;
        let parsed: LlmConfig = serde_json::from_str(raw).unwrap();
        let config = LlmConfig::new(parsed.api_key, parsed.api_url, parsed.model).unwrap();
        assert_eq!(config.api_key(), "sk-or-test");
        assert_eq!(config.api_url(), DEFAULT_API_URL);
        assert_eq!(config.model(), DEFAULT_MODEL);
    }

    #[test]
    fn repairs_only_the_known_cmd_v_paste_artifact() {
        let valid = format!("sk-or-v1-{}", "a".repeat(64));
        assert_eq!(repair_paste_artifact(&(valid.clone() + "v")), valid);
        assert_eq!(repair_paste_artifact("unrelated-keyv"), "unrelated-keyv");
        assert_eq!(repair_paste_artifact("sk-or-v1-shortv"), "sk-or-v1-shortv");
    }

    #[test]
    fn validates_manual_configuration() {
        assert!(LlmConfig::new("key".into(), DEFAULT_API_URL.into(), "model".into()).is_ok());
        assert!(LlmConfig::new("".into(), DEFAULT_API_URL.into(), "model".into()).is_err());
        assert!(LlmConfig::new("key".into(), "not-a-url".into(), "model".into()).is_err());
        assert_eq!(
            LlmConfig::new(
                "key".into(),
                "http://openrouter.ai/api/v1/chat/completions".into(),
                "model".into()
            )
            .err()
            .map(|error| error.to_string())
            .as_deref(),
            Some("API URL must use HTTPS")
        );
        assert_eq!(
            LlmConfig::new(
                "key".into(),
                "https://example.com/api/v1/chat/completions".into(),
                "model".into()
            )
            .err()
            .map(|error| error.to_string())
            .as_deref(),
            Some("API URL host must be openrouter.ai")
        );
        assert!(
            LlmConfig::new(
                "key".into(),
                "https://openrouter.ai/api/v1".into(),
                "model".into()
            )
            .is_err()
        );
        assert!(LlmConfig::new("key".into(), DEFAULT_API_URL.into(), "".into()).is_err());
    }

    #[test]
    fn migrates_legacy_configuration_to_the_system_path() {
        let unique = format!(
            "wuziqi-config-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let legacy = root.join(CONFIG_FILE_NAME);
        let current = root.join("system").join(CONFIG_FILE_NAME);
        std::fs::create_dir_all(&root).unwrap();
        let legacy_key = format!("sk-or-v1-{}v", "a".repeat(64));
        std::fs::write(
            &legacy,
            format!(
                r#"{{"api_key":"{legacy_key}","api_url":"{DEFAULT_API_URL}","model":"model"}}"#
            ),
        )
        .unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o400)).unwrap();

        let loaded = LlmConfig::load_with_paths(&current, &legacy).unwrap();

        assert_eq!(loaded.api_key(), legacy_key.trim_end_matches('v'));
        assert!(current.exists());
        assert!(!legacy.exists());
        assert!(migrated_legacy_config_path(&legacy).exists());
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(migrated_legacy_config_path(&legacy))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let (migrated, repaired) = LlmConfig::read_from_path(&current).unwrap();
        assert_eq!(migrated.model(), "model");
        assert!(!repaired);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn atomically_replaces_an_existing_configuration() {
        let unique = format!(
            "wuziqi-save-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let path = root.join(CONFIG_FILE_NAME);
        let original =
            LlmConfig::new("old-key".into(), DEFAULT_API_URL.into(), "old".into()).unwrap();
        original.save_to_path(&path).unwrap();
        let replacement =
            LlmConfig::new("new-key".into(), DEFAULT_API_URL.into(), "new".into()).unwrap();

        replacement.save_to_path(&path).unwrap();

        let (loaded, repaired) = LlmConfig::read_from_path(&path).unwrap();
        assert_eq!(loaded.api_key(), "new-key");
        assert_eq!(loaded.model(), "new");
        assert!(!repaired);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn spawn_http_server(status: &str, body: String) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 16 * 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        format!("http://{address}/api/v1/chat/completions")
    }

    #[test]
    fn rejects_an_oversized_api_response() {
        let url = spawn_http_server("200 OK", "x".repeat(MAX_RESPONSE_BYTES + 1));
        let config = LlmConfig::new_unchecked("key".into(), url, "model".into());
        let client = build_client().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime
            .block_on(request_move(
                &client,
                &config,
                &[[Cell::Empty; BOARD]; BOARD],
                &[(7, 7)],
            ))
            .unwrap_err();

        assert!(error.contains("exceeds 65536 bytes"));
    }

    #[test]
    fn reports_structured_http_api_errors() {
        let url = spawn_http_server(
            "429 Too Many Requests",
            r#"{"error":{"message":"rate limited"}}"#.to_string(),
        );
        let config = LlmConfig::new_unchecked("key".into(), url, "model".into());
        let client = build_client().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime
            .block_on(request_move(
                &client,
                &config,
                &[[Cell::Empty; BOARD]; BOARD],
                &[(7, 7)],
            ))
            .unwrap_err();

        assert_eq!(error, "OpenRouter HTTP 429: rate limited");
    }
}
