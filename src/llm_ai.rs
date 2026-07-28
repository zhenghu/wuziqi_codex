//! 大模型共享配置、提示词、响应解析与后端分发。

mod local;
mod openrouter;

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

const CONFIG_FILE_NAME: &str = "llm_config.json";
pub(crate) const DEFAULT_OPENROUTER_API_URL: &str = openrouter::DEFAULT_API_URL;
pub(crate) const DEFAULT_OPENROUTER_MODEL: &str = openrouter::DEFAULT_MODEL;
pub(crate) const DEFAULT_LOCAL_API_URL: &str = local::DEFAULT_API_URL;
pub(crate) const DEFAULT_LOCAL_MODEL: &str = local::DEFAULT_MODEL;
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum LlmBackend {
    #[default]
    #[serde(rename = "openrouter")]
    OpenRouter,
    #[serde(rename = "local")]
    Local,
}

impl LlmBackend {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::OpenRouter => "OpenRouter",
            Self::Local => "Local",
        }
    }

    pub(crate) fn default_api_url(self) -> &'static str {
        match self {
            Self::OpenRouter => DEFAULT_OPENROUTER_API_URL,
            Self::Local => DEFAULT_LOCAL_API_URL,
        }
    }

    pub(crate) fn default_model(self) -> &'static str {
        match self {
            Self::OpenRouter => DEFAULT_OPENROUTER_MODEL,
            Self::Local => DEFAULT_LOCAL_MODEL,
        }
    }

    pub(crate) fn requires_api_key(self) -> bool {
        matches!(self, Self::OpenRouter)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct LlmConfig {
    #[serde(default)]
    backend: LlmBackend,
    #[serde(default, skip_serializing_if = "String::is_empty")]
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
    pub(crate) fn new(
        backend: LlmBackend,
        api_key: String,
        api_url: String,
        model: String,
    ) -> ConfigResult<Self> {
        let mut api_key = api_key.trim().to_string();
        let api_url = api_url.trim().trim_end_matches('/').to_string();
        let model = model.trim().to_string();
        if backend.requires_api_key() && api_key.is_empty() {
            return Err(ConfigError::Invalid(
                "OpenRouter API Key is required".to_string(),
            ));
        }
        if model.is_empty() {
            return Err(ConfigError::Invalid("Model name is required".to_string()));
        }
        let parsed = reqwest::Url::parse(&api_url)
            .map_err(|error| ConfigError::Invalid(format!("Invalid API URL: {error}")))?;
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

        match backend {
            LlmBackend::OpenRouter => {
                openrouter::validate_url(&parsed).map_err(ConfigError::Invalid)?;
            }
            LlmBackend::Local => {
                local::validate_url(&parsed).map_err(ConfigError::Invalid)?;
                // A cloud credential must never be retained or sent while the
                // local transport is selected.
                api_key.clear();
            }
        }
        Ok(Self {
            backend,
            api_key,
            api_url,
            model,
        })
    }

    #[cfg(test)]
    pub(crate) fn new_unchecked(
        backend: LlmBackend,
        api_key: String,
        api_url: String,
        model: String,
    ) -> Self {
        Self {
            backend,
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
        let value: Value = serde_json::from_str(&text).map_err(|source| ConfigError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        let backend_was_missing = value.get("backend").is_none();
        let api_key_was_present = value.get("api_key").is_some();
        let mut raw: Self = serde_json::from_value(value).map_err(|source| ConfigError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        let repaired = repair_paste_artifact(&raw.api_key);
        let changed = repaired != raw.api_key
            || backend_was_missing
            || (raw.backend == LlmBackend::Local && api_key_was_present);
        raw.api_key = repaired;
        let config = Self::new(raw.backend, raw.api_key, raw.api_url, raw.model)?;
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

    pub(crate) fn backend(&self) -> LlmBackend {
        self.backend
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
        .or_else(|| value.get("error").and_then(Value::as_str))
        .or_else(|| value.get("message").and_then(Value::as_str))
}

fn error_detail(text: &str) -> String {
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.is_empty() {
        "unknown API error".to_string()
    } else {
        truncate_text(&single_line, MAX_ERROR_TEXT_CHARS)
    }
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
    let messages = [
        ChatMessage {
            role: "system",
            content: "你是五子棋专家。只输出一个 JSON 对象，例如 {\"x\":7,\"y\":7}，不要解释。"
                .to_string(),
        },
        ChatMessage {
            role: "user",
            content: prompt,
        },
    ];
    let response = match config.backend {
        LlmBackend::OpenRouter => {
            openrouter::send_request(
                client,
                &config.api_url,
                &config.api_key,
                &config.model,
                &messages,
            )
            .await?
        }
        LlmBackend::Local => {
            local::send_request(client, &config.api_url, &config.model, &messages).await?
        }
    };
    let status = response.status();
    let body = read_limited_body(response, config.backend.label()).await?;
    let parsed = serde_json::from_str::<Value>(&body);
    if !status.is_success() {
        let detail = parsed
            .as_ref()
            .ok()
            .and_then(api_error_message)
            .unwrap_or(&body);
        return Err(format!(
            "{} HTTP {}: {}",
            config.backend.label(),
            status.as_u16(),
            error_detail(detail)
        ));
    }
    let value = parsed
        .map_err(|error| format!("{} returned invalid JSON: {error}", config.backend.label()))?;
    if let Some(error) = api_error_message(&value) {
        return Err(format!(
            "{} error: {}",
            config.backend.label(),
            error_detail(error)
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
            "{} response has no text (finish_reason={finish_reason}, reasoning_tokens={reasoning_tokens})",
            config.backend.label()
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
    let (model, provider) = match config.backend {
        LlmBackend::OpenRouter => openrouter::resolve_route(&value)?,
        LlmBackend::Local => local::resolve_route(&value, &config.model),
    };
    Ok(LlmMove {
        position: chosen,
        model,
        provider,
    })
}

pub(crate) fn build_openrouter_client() -> Result<reqwest::Client, String> {
    openrouter::build_client()
}

pub(crate) fn build_local_client() -> Result<reqwest::Client, String> {
    local::build_client()
}

async fn read_limited_body(
    mut response: reqwest::Response,
    backend_label: &str,
) -> Result<String, String> {
    let mut body = Vec::new();
    loop {
        let next_chunk = response
            .chunk()
            .await
            .map_err(|error| format!("Cannot read {backend_label} response: {error}"))?;
        let Some(chunk) = next_chunk else {
            break;
        };
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(format!(
                "{backend_label} response exceeds {MAX_RESPONSE_BYTES} bytes"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body)
        .map_err(|error| format!("{backend_label} response is not UTF-8: {error}"))
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
    use std::time::Duration;

    #[test]
    fn parses_json_and_plain_coordinates() {
        assert_eq!(parse_move(r#"{"x":7,"y":8}"#), Some((7, 8)));
        assert_eq!(parse_move("(12, 3)"), Some((12, 3)));
        assert_eq!(parse_move("选择 (12, 3)，因为它最好"), Some((12, 3)));
        assert_eq!(parse_move("7 8 9"), None);
    }

    #[test]
    fn extracts_openai_compatible_chat_completion_text() {
        let string_content: Value = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "{\"x\":1,\"y\":2}"}}]
        });
        let array_content: Value = serde_json::json!({
            "choices": [{"message": {"content": [{"type": "text", "text": "{\"x\":3,\"y\":4}"}]}}]
        });
        assert_eq!(
            response_text(&string_content).as_deref(),
            Some("{\"x\":1,\"y\":2}")
        );
        assert_eq!(
            response_text(&array_content).as_deref(),
            Some("{\"x\":3,\"y\":4}")
        );
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
    fn legacy_json_configuration_defaults_to_openrouter() {
        let raw = r#"{
            "api_key": "sk-or-test",
            "api_url": "https://openrouter.ai/api/v1/chat/completions",
            "model": "openai/gpt-5-mini"
        }"#;
        let parsed: LlmConfig = serde_json::from_str(raw).unwrap();
        let config =
            LlmConfig::new(parsed.backend, parsed.api_key, parsed.api_url, parsed.model).unwrap();
        assert_eq!(config.backend(), LlmBackend::OpenRouter);
        assert_eq!(config.api_key(), "sk-or-test");
        assert_eq!(config.api_url(), DEFAULT_OPENROUTER_API_URL);
        assert_eq!(config.model(), DEFAULT_OPENROUTER_MODEL);
    }

    #[test]
    fn repairs_only_the_known_cmd_v_paste_artifact() {
        let valid = format!("sk-or-v1-{}", "a".repeat(64));
        assert_eq!(repair_paste_artifact(&(valid.clone() + "v")), valid);
        assert_eq!(repair_paste_artifact("unrelated-keyv"), "unrelated-keyv");
        assert_eq!(repair_paste_artifact("sk-or-v1-shortv"), "sk-or-v1-shortv");
    }

    #[test]
    fn validates_openrouter_configuration() {
        assert!(
            LlmConfig::new(
                LlmBackend::OpenRouter,
                "key".into(),
                DEFAULT_OPENROUTER_API_URL.into(),
                "model".into()
            )
            .is_ok()
        );
        assert!(
            LlmConfig::new(
                LlmBackend::OpenRouter,
                "".into(),
                DEFAULT_OPENROUTER_API_URL.into(),
                "model".into()
            )
            .is_err()
        );
        assert!(
            LlmConfig::new(
                LlmBackend::OpenRouter,
                "key".into(),
                "not-a-url".into(),
                "model".into()
            )
            .is_err()
        );
        assert_eq!(
            LlmConfig::new(
                LlmBackend::OpenRouter,
                "key".into(),
                "http://openrouter.ai/api/v1/chat/completions".into(),
                "model".into()
            )
            .err()
            .map(|error| error.to_string())
            .as_deref(),
            Some("OpenRouter API URL must use HTTPS")
        );
        assert_eq!(
            LlmConfig::new(
                LlmBackend::OpenRouter,
                "key".into(),
                "https://example.com/api/v1/chat/completions".into(),
                "model".into()
            )
            .err()
            .map(|error| error.to_string())
            .as_deref(),
            Some("OpenRouter API URL host must be openrouter.ai")
        );
        assert!(
            LlmConfig::new(
                LlmBackend::OpenRouter,
                "key".into(),
                "https://openrouter.ai/api/v1".into(),
                "model".into()
            )
            .is_err()
        );
        assert!(
            LlmConfig::new(
                LlmBackend::OpenRouter,
                "key".into(),
                DEFAULT_OPENROUTER_API_URL.into(),
                "".into()
            )
            .is_err()
        );
    }

    #[test]
    fn local_configuration_accepts_only_numeric_loopback_chat_endpoints() {
        for url in [
            DEFAULT_LOCAL_API_URL,
            "http://[::1]:1234/v1/chat/completions",
            "https://127.0.0.1:1234/v1/chat/completions",
        ] {
            let config = LlmConfig::new(
                LlmBackend::Local,
                "cloud-secret".into(),
                url.into(),
                "model".into(),
            )
            .unwrap();
            assert!(config.api_key().is_empty());
        }

        for url in [
            "http://localhost:11434/v1/chat/completions",
            "http://127.0.0.2:11434/v1/chat/completions",
            "http://192.168.1.10:11434/v1/chat/completions",
            "http://0.0.0.0:11434/v1/chat/completions",
            "http://127.0.0.1.evil.example:11434/v1/chat/completions",
            "https://example.com/v1/chat/completions",
            "file://127.0.0.1/v1/chat/completions",
            "http://user:password@127.0.0.1:11434/v1/chat/completions",
            "http://127.0.0.1:11434/v1/chat/completions?secret=value",
            "http://127.0.0.1:11434/v1/chat/completions#fragment",
            "http://127.0.0.1:11434/api/v1/chat/completions",
            "http://[::ffff:127.0.0.1]:11434/v1/chat/completions",
        ] {
            assert!(
                LlmConfig::new(LlmBackend::Local, String::new(), url.into(), "model".into())
                    .is_err(),
                "unexpectedly accepted {url}"
            );
        }
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
                r#"{{"api_key":"{legacy_key}","api_url":"{DEFAULT_OPENROUTER_API_URL}","model":"model"}}"#
            ),
        )
        .unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o400)).unwrap();

        let loaded = LlmConfig::load_with_paths(&current, &legacy).unwrap();

        assert_eq!(loaded.api_key(), legacy_key.trim_end_matches('v'));
        assert_eq!(loaded.backend(), LlmBackend::OpenRouter);
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
        let migrated_json: Value =
            serde_json::from_str(&std::fs::read_to_string(&current).unwrap()).unwrap();
        assert_eq!(migrated_json["backend"], "openrouter");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn upgrades_a_current_three_field_configuration_in_place() {
        let unique = format!(
            "wuziqi-schema-upgrade-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let current = root.join(CONFIG_FILE_NAME);
        let missing_legacy = root.join("missing").join(CONFIG_FILE_NAME);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            &current,
            format!(
                r#"{{"api_key":"key","api_url":"{DEFAULT_OPENROUTER_API_URL}","model":"model"}}"#
            ),
        )
        .unwrap();

        let loaded = LlmConfig::load_with_paths(&current, &missing_legacy).unwrap();

        assert_eq!(loaded.backend(), LlmBackend::OpenRouter);
        let upgraded: Value =
            serde_json::from_str(&std::fs::read_to_string(&current).unwrap()).unwrap();
        assert_eq!(upgraded["backend"], "openrouter");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_configuration_round_trips_without_an_api_key() {
        let unique = format!(
            "wuziqi-local-config-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let path = root.join(CONFIG_FILE_NAME);
        let config = LlmConfig::new(
            LlmBackend::Local,
            "must-be-cleared".into(),
            DEFAULT_LOCAL_API_URL.into(),
            DEFAULT_LOCAL_MODEL.into(),
        )
        .unwrap();

        config.save_to_path(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("must-be-cleared"));
        assert!(!text.contains("api_key"));
        let (loaded, changed) = LlmConfig::read_from_path(&path).unwrap();
        assert_eq!(loaded.backend(), LlmBackend::Local);
        assert_eq!(loaded.api_url(), DEFAULT_LOCAL_API_URL);
        assert_eq!(loaded.model(), DEFAULT_LOCAL_MODEL);
        assert!(loaded.api_key().is_empty());
        assert!(!changed);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn loading_local_configuration_removes_a_stored_cloud_key() {
        let unique = format!(
            "wuziqi-local-key-cleanup-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let current = root.join(CONFIG_FILE_NAME);
        let missing_legacy = root.join("missing").join(CONFIG_FILE_NAME);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            &current,
            format!(
                r#"{{"backend":"local","api_key":"cloud-secret","api_url":"{DEFAULT_LOCAL_API_URL}","model":"{DEFAULT_LOCAL_MODEL}"}}"#
            ),
        )
        .unwrap();

        let loaded = LlmConfig::load_with_paths(&current, &missing_legacy).unwrap();

        assert!(loaded.api_key().is_empty());
        let rewritten = std::fs::read_to_string(&current).unwrap();
        assert!(!rewritten.contains("cloud-secret"));
        assert!(!rewritten.contains("api_key"));
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
        let original = LlmConfig::new(
            LlmBackend::OpenRouter,
            "old-key".into(),
            DEFAULT_OPENROUTER_API_URL.into(),
            "old".into(),
        )
        .unwrap();
        original.save_to_path(&path).unwrap();
        let replacement = LlmConfig::new(
            LlmBackend::OpenRouter,
            "new-key".into(),
            DEFAULT_OPENROUTER_API_URL.into(),
            "new".into(),
        )
        .unwrap();

        replacement.save_to_path(&path).unwrap();

        let (loaded, repaired) = LlmConfig::read_from_path(&path).unwrap();
        assert_eq!(loaded.api_key(), "new-key");
        assert_eq!(loaded.model(), "new");
        assert!(!repaired);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn spawn_http_server(status: &str, body: String) -> String {
        spawn_capturing_http_server(status, body).0
    }

    fn spawn_capturing_http_server(
        status: &str,
        body: String,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4 * 1024];
            loop {
                let count = match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => count,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        break;
                    }
                    Err(_) => break,
                };
                request.extend_from_slice(&chunk[..count]);
                let request_text = String::from_utf8_lossy(&request);
                let Some(header_end) = request_text.find("\r\n\r\n") else {
                    continue;
                };
                let content_length = request_text[..header_end]
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let _ = sender.send(String::from_utf8_lossy(&request).into_owned());
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        (format!("http://{address}/v1/chat/completions"), receiver)
    }

    #[test]
    fn rejects_an_oversized_api_response() {
        let url = spawn_http_server("200 OK", "x".repeat(MAX_RESPONSE_BYTES + 1));
        let config = LlmConfig::new_unchecked(LlmBackend::Local, "key".into(), url, "model".into());
        let client = build_local_client().unwrap();
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
        let config = LlmConfig::new_unchecked(LlmBackend::Local, "key".into(), url, "model".into());
        let client = build_local_client().unwrap();
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

        assert_eq!(error, "Local HTTP 429: rate limited");
    }

    #[test]
    fn reports_string_and_plain_text_http_api_errors() {
        let client = build_local_client().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for (body, expected) in [
            (
                r#"{"error":"model not found"}"#,
                "Local HTTP 500: model not found",
            ),
            (
                "service unavailable\ntry later",
                "Local HTTP 500: service unavailable try later",
            ),
        ] {
            let url = spawn_http_server("500 Internal Server Error", body.to_string());
            let config =
                LlmConfig::new_unchecked(LlmBackend::Local, String::new(), url, "model".into());

            let error = runtime
                .block_on(request_move(
                    &client,
                    &config,
                    &[[Cell::Empty; BOARD]; BOARD],
                    &[(7, 7)],
                ))
                .unwrap_err();

            assert_eq!(error, expected);
        }
    }

    #[test]
    fn local_request_never_sends_cloud_credentials_or_openrouter_fields() {
        let body = r#"{
            "choices":[{"message":{"content":"{\"x\":7,\"y\":7}"}}]
        }"#
        .to_string();
        let (url, captured_request) = spawn_capturing_http_server("200 OK", body);
        let secret = "must-not-leak-secret";
        let config =
            LlmConfig::new_unchecked(LlmBackend::Local, secret.into(), url, "qwen3:4b".into());
        let client = build_local_client().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let llm_move = runtime
            .block_on(request_move(
                &client,
                &config,
                &[[Cell::Empty; BOARD]; BOARD],
                &[(7, 7)],
            ))
            .unwrap();
        let request = captured_request
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let request_lowercase = request.to_ascii_lowercase();

        assert_eq!(llm_move.position, (7, 7));
        assert_eq!(llm_move.route_label(), "qwen3:4b via Local");
        assert!(!request.contains(secret));
        assert!(!request_lowercase.contains("authorization:"));
        assert!(!request_lowercase.contains("x-openrouter-title:"));
        assert!(!request.contains("\"reasoning\""));
        assert!(!request.contains("\"max_completion_tokens\""));
        assert!(request.contains("\"reasoning_effort\":\"none\""));
        assert!(request.contains("\"response_format\":{\"type\":\"json_object\"}"));
    }

    #[test]
    fn openrouter_request_still_sends_its_credentials_and_headers() {
        let body = r#"{
            "model":"openai/gpt-5-mini",
            "provider":"OpenAI",
            "choices":[{"message":{"content":"{\"x\":7,\"y\":7}"}}]
        }"#
        .to_string();
        let (url, captured_request) = spawn_capturing_http_server("200 OK", body);
        let config = LlmConfig::new_unchecked(
            LlmBackend::OpenRouter,
            "openrouter-secret".into(),
            url,
            DEFAULT_OPENROUTER_MODEL.into(),
        );
        let client = build_openrouter_client().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime
            .block_on(request_move(
                &client,
                &config,
                &[[Cell::Empty; BOARD]; BOARD],
                &[(7, 7)],
            ))
            .unwrap();
        let request = captured_request
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let request_lowercase = request.to_ascii_lowercase();

        assert!(request_lowercase.contains("authorization: bearer openrouter-secret"));
        assert!(request_lowercase.contains("x-openrouter-title: wuziqi"));
        assert!(request.contains("\"reasoning\""));
        assert!(request.contains("\"max_completion_tokens\":1024"));
    }
}
