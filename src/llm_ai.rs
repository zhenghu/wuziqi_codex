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

    #[cfg(test)]
    pub(crate) fn new_for_test(
        position: (usize, usize),
        model: impl Into<String>,
        provider: Option<String>,
    ) -> Self {
        Self {
            position,
            model: model.into(),
            provider,
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
mod tests;
