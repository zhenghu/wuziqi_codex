//! 大模型共享配置、提示词、响应解析与后端分发。

mod cloud;
mod local;

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
const SETTINGS_SCHEMA_VERSION: u32 = 2;
const MAX_LLM_PROFILES: usize = 2;
pub(crate) const DEFAULT_CLOUD_API_URL: &str = cloud::DEFAULT_API_URL;
pub(crate) const DEFAULT_CLOUD_MODEL: &str = cloud::DEFAULT_MODEL;
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

pub(crate) type ConfigResult<T> = Result<T, ConfigError>;

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
            Self::OpenRouter => DEFAULT_CLOUD_API_URL,
            Self::Local => DEFAULT_LOCAL_API_URL,
        }
    }

    pub(crate) fn default_model(self) -> &'static str {
        match self {
            Self::OpenRouter => DEFAULT_CLOUD_MODEL,
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
    /// 推理模型（如 DeepSeek 的 reasoning 系列）在复杂局面会消耗大量 token
    /// 思考，导致最终答案为空。开启后在请求中发送 `thinking: {"type":"disabled"}`。
    #[serde(default, skip_serializing_if = "is_false")]
    no_reasoning: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct LlmProfile {
    name: String,
    #[serde(flatten)]
    config: LlmConfig,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct LlmSettings {
    schema_version: u32,
    profiles: Vec<LlmProfile>,
    #[serde(default)]
    active_profile: usize,
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
                cloud::validate_url(&parsed).map_err(ConfigError::Invalid)?;
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
            no_reasoning: false,
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
            no_reasoning: false,
        }
    }

    /// 关闭推理模型的思考过程（发送 `thinking: {"type":"disabled"}`）。
    pub(crate) fn no_reasoning(mut self, enabled: bool) -> Self {
        self.no_reasoning = enabled;
        self
    }

    pub(crate) fn no_reasoning_enabled(&self) -> bool {
        self.no_reasoning
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

impl LlmProfile {
    pub(crate) fn new(name: impl Into<String>, config: LlmConfig) -> ConfigResult<Self> {
        let name = name.into().trim().to_string();
        if name.is_empty() {
            return Err(ConfigError::Invalid(
                "LLM profile name is required".to_string(),
            ));
        }
        Ok(Self { name, config })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn config(&self) -> &LlmConfig {
        &self.config
    }
}

impl LlmSettings {
    pub(crate) fn new(profiles: Vec<LlmProfile>, active_profile: usize) -> ConfigResult<Self> {
        if profiles.is_empty() {
            return Err(ConfigError::Invalid(
                "At least one LLM profile is required".to_string(),
            ));
        }
        if profiles.len() > MAX_LLM_PROFILES {
            return Err(ConfigError::Invalid(format!(
                "At most {MAX_LLM_PROFILES} LLM profiles are supported"
            )));
        }
        if active_profile >= profiles.len() {
            return Err(ConfigError::Invalid(format!(
                "Active LLM profile index {active_profile} is out of range"
            )));
        }

        let mut normalized = Vec::with_capacity(profiles.len());
        for profile in profiles {
            let config = normalize_config(profile.config)?;
            let profile = LlmProfile::new(profile.name, config)?;
            if normalized
                .iter()
                .any(|existing: &LlmProfile| existing.name == profile.name)
            {
                return Err(ConfigError::Invalid(format!(
                    "Duplicate LLM profile name: {}",
                    profile.name
                )));
            }
            normalized.push(profile);
        }

        Ok(Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            profiles: normalized,
            active_profile,
        })
    }

    pub(crate) fn profiles(&self) -> &[LlmProfile] {
        &self.profiles
    }

    pub(crate) fn active_profile(&self) -> &LlmProfile {
        &self.profiles[self.active_profile]
    }

    pub(crate) fn active_profile_index(&self) -> usize {
        self.active_profile
    }

    pub(crate) fn arena_pair(&self) -> Option<(&LlmProfile, &LlmProfile)> {
        (self.profiles.len() == MAX_LLM_PROFILES).then(|| (&self.profiles[0], &self.profiles[1]))
    }

    pub(crate) fn load() -> ConfigResult<Self> {
        let current = config_path()?;
        let legacy = legacy_config_path();
        Self::load_with_paths(&current, &legacy)
    }

    fn load_with_paths(current: &Path, legacy: &Path) -> ConfigResult<Self> {
        if current.exists() {
            let (settings, repaired) = Self::read_from_path(current)?;
            if repaired {
                settings.save_to_path(current)?;
            }
            return Ok(settings);
        }
        if legacy.exists() {
            // Never modify the legacy source. Apply repairs in memory and write
            // the final configuration directly to the system location.
            let (settings, _) = Self::read_from_path(legacy)?;
            settings.save_to_path(current)?;
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
            return Ok(settings);
        }
        Self::read_from_path(current).map(|(settings, _)| settings)
    }

    fn read_from_path(path: &Path) -> ConfigResult<(Self, bool)> {
        let text =
            std::fs::read_to_string(path).map_err(|error| ConfigError::io("read", path, error))?;
        let value: Value = serde_json::from_str(&text).map_err(|source| ConfigError::Json {
            path: path.to_path_buf(),
            source,
        })?;

        if value.get("profiles").is_some() || value.get("schema_version").is_some() {
            Self::read_versioned_value(value, path)
        } else {
            Self::read_legacy_value(value, path)
        }
    }

    fn read_versioned_value(value: Value, path: &Path) -> ConfigResult<(Self, bool)> {
        let version = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "Missing or invalid schema_version in {}",
                    path.display()
                ))
            })?;
        if version > u64::from(SETTINGS_SCHEMA_VERSION) {
            return Err(ConfigError::Invalid(format!(
                "Unsupported future LLM configuration schema version {version}"
            )));
        }
        if version != u64::from(SETTINGS_SCHEMA_VERSION) {
            return Err(ConfigError::Invalid(format!(
                "Unsupported LLM configuration schema version {version}"
            )));
        }

        let backend_was_missing =
            value
                .get("profiles")
                .and_then(Value::as_array)
                .is_some_and(|profiles| {
                    profiles
                        .iter()
                        .any(|profile| profile.get("backend").is_none())
                });
        let active_profile_was_missing = value.get("active_profile").is_none();
        let raw: Self = serde_json::from_value(value).map_err(|source| ConfigError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        let raw_names = raw
            .profiles
            .iter()
            .map(|profile| profile.name.clone())
            .collect::<Vec<_>>();
        let raw_configs = raw
            .profiles
            .iter()
            .map(|profile| profile.config.clone())
            .collect::<Vec<_>>();
        let settings = Self::new(raw.profiles, raw.active_profile)?;
        let changed = backend_was_missing
            || active_profile_was_missing
            || settings
                .profiles
                .iter()
                .zip(raw_names.iter().zip(raw_configs))
                .any(|(profile, (raw_name, raw_config))| {
                    profile.name.as_str() != raw_name.as_str()
                        || profile.config.backend != raw_config.backend
                        || profile.config.api_key != raw_config.api_key
                        || profile.config.api_url != raw_config.api_url
                        || profile.config.model != raw_config.model
                });
        Ok((settings, changed))
    }

    fn read_legacy_value(value: Value, path: &Path) -> ConfigResult<(Self, bool)> {
        let raw: LlmConfig = serde_json::from_value(value).map_err(|source| ConfigError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        let config = normalize_config(raw)?;
        let profile = LlmProfile::new(config.model().to_string(), config)?;
        Ok((Self::new(vec![profile], 0)?, true))
    }

    pub(crate) fn save(&self) -> ConfigResult<()> {
        self.save_to_path(&config_path()?)
    }

    fn save_to_path(&self, path: &Path) -> ConfigResult<()> {
        write_json_atomically(self, path)
    }
}

fn normalize_config(raw: LlmConfig) -> ConfigResult<LlmConfig> {
    let repaired_key = repair_paste_artifact(&raw.api_key);
    LlmConfig::new(raw.backend, repaired_key, raw.api_url, raw.model)
}

fn write_json_atomically(value: &impl Serialize, path: &Path) -> ConfigResult<()> {
    let json = serde_json::to_string_pretty(value).map_err(|error| {
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

/// 从模型回复中提取落点坐标。推理模型常在 JSON 前后附带解释文字，
/// 因此按多种格式逐级尝试：整体 JSON、包裹的 JSON、x/y 键值对、括号坐标、恰好两个数字。
fn parse_move(text: &str) -> Option<(usize, usize)> {
    let trimmed = text.trim();

    // 1. 整体就是一个 JSON 对象
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(move_point) = xy_from_value(&value) {
            return Some(move_point);
        }
    }

    // 2. 文本中包裹的 JSON 对象（前后可能有解释文字或 Markdown 代码块）
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if start < end {
            if let Ok(value) = serde_json::from_str::<Value>(&trimmed[start..=end]) {
                if let Some(move_point) = xy_from_value(&value) {
                    return Some(move_point);
                }
            }
        }
    }

    // 3. "x": N 与 "y": N 键值对（顺序无关，支持中文冒号）
    if let (Some(x), Some(y)) = (number_after_key(trimmed, "x"), number_after_key(trimmed, "y")) {
        return Some((x, y));
    }

    // 4. (x, y) 括号坐标（支持中文逗号与空格）
    if let Some(move_point) = number_pair_in_parens(trimmed) {
        return Some(move_point);
    }

    // 5. 兜底：文本中恰好出现两个数字
    let values: Vec<_> = trimmed
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<usize>().ok())
        .collect();
    (values.len() == 2).then(|| (values[0], values[1]))
}

fn xy_from_value(value: &Value) -> Option<(usize, usize)> {
    let x = value.get("x")?.as_u64()? as usize;
    let y = value.get("y")?.as_u64()? as usize;
    Some((x, y))
}

/// 提取形如 `"x": 7` 的键值对数字，支持中文冒号与空格。
fn number_after_key(text: &str, key: &str) -> Option<usize> {
    let needle = format!("\"{key}\"");
    let rest = &text[text.find(&needle)? + needle.len()..];
    let rest = rest.trim_start_matches([':', '：', ' ', '\t', '\n', '\r']);
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// 提取形如 `(7, 7)` 或 `（7，7）` 的括号坐标对，支持中文逗号与空格。
/// 模型可能在讨论多个候选点后给出结论，因此返回最后一个坐标对。
fn number_pair_in_parens(text: &str) -> Option<(usize, usize)> {
    let mut last_match: Option<(usize, usize)> = None;
    for (index, character) in text.char_indices() {
        if character != '(' && character != '（' {
            continue;
        }
        let after = &text[index + character.len_utf8()..];
        let x_digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if x_digits.is_empty() {
            continue;
        }
        let Some(x) = x_digits.parse::<usize>().ok() else {
            continue;
        };
        let rest = after[x_digits.len()..].trim_start_matches(&[' ', ',', '，', '\t'][..]);
        let y_digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if y_digits.is_empty() {
            continue;
        }
        let Some(y) = y_digits.parse::<usize>().ok() else {
            continue;
        };
        let rest = rest[y_digits.len()..].trim_start_matches(&[' ', '\t'][..]);
        if rest.starts_with(')') || rest.starts_with('）') {
            last_match = Some((x, y));
        }
    }
    last_match
}

pub(crate) async fn request_move(
    client: &reqwest::Client,
    config: &LlmConfig,
    board: &[[Cell; BOARD]; BOARD],
    side: Cell,
    candidates: &[(usize, usize)],
    excluded: &[(usize, usize)],
) -> Result<LlmMove, String> {
    let (side_name, side_stone, opponent_name, opponent_stone) = match side {
        Cell::Black => ("黑", 'X', "白", 'O'),
        Cell::White => ("白", 'O', "黑", 'X'),
        Cell::Empty => return Err("大模型执子方不能是空棋子".to_string()),
    };
    if candidates.is_empty() {
        return Err("没有合法候选点".to_string());
    }

    let mut prompt = format!(
        "你执{side_name}棋 {side_stone}，对手执{opponent_name}棋 {opponent_stone}，当前轮到你落子。\
         坐标从 0 到 14，格式为 (x,y)，左上角是 (0,0)。\n\
         当前棋盘（每行对应 y=0..14）：\n{}\n\
         战术引擎给出的合法候选点：{:?}\n\
         综合进攻、防守、后续威胁和中心控制，选出最佳一手。只能从候选点中选择。",
        board_text(board),
        candidates
    );
    if !excluded.is_empty() {
        prompt.push_str(&format!(
            "\n注意：以下位置已被占据或不可用，绝不能选择它们：{excluded:?}"
        ));
    }
    let messages = [
        ChatMessage {
            role: "system",
            content:
                "你是五子棋专家。根据当前执子方选择最佳落点。只输出一个 JSON 对象，例如 {\"x\":7,\"y\":7}，不要解释。"
                    .to_string(),
        },
        ChatMessage {
            role: "user",
            content: prompt,
        },
    ];
    let response = match config.backend {
        LlmBackend::OpenRouter => {
            cloud::send_request(
                client,
                &config.api_url,
                &config.api_key,
                &config.model,
                config.no_reasoning,
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
        LlmBackend::OpenRouter => cloud::resolve_route(&value, &config.model),
        LlmBackend::Local => local::resolve_route(&value, &config.model),
    };
    Ok(LlmMove {
        position: chosen,
        model,
        provider,
    })
}

pub(crate) fn build_cloud_client() -> Result<reqwest::Client, String> {
    cloud::build_client()
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
