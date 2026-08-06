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
const LEGACY_SETTINGS_SCHEMA_VERSION: u32 = 2;
const SETTINGS_SCHEMA_VERSION: u32 = 4;
const MAX_LLM_PROFILES: usize = 2;
pub(crate) const DEFAULT_CLOUD_API_URL: &str = cloud::DEFAULT_API_URL;
pub(crate) const DEFAULT_CLOUD_MODEL: &str = cloud::DEFAULT_MODEL;
pub(crate) const DEFAULT_LOCAL_API_URL: &str = local::DEFAULT_API_URL;
pub(crate) const DEFAULT_LOCAL_MODEL: &str = local::DEFAULT_MODEL;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_ERROR_TEXT_CHARS: usize = 512;
pub(crate) const CLOUD_KEY_ORIGIN_CHANGED: &str =
    "Cloud API origin changed; re-enter the Cloud API Key";

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
    #[serde(rename = "cloud", alias = "openrouter")]
    Cloud,
    #[serde(rename = "local")]
    Local,
}

impl LlmBackend {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Cloud => "Cloud",
            Self::Local => "Local",
        }
    }

    pub(crate) fn default_api_url(self) -> &'static str {
        match self {
            Self::Cloud => DEFAULT_CLOUD_API_URL,
            Self::Local => DEFAULT_LOCAL_API_URL,
        }
    }

    pub(crate) fn default_model(self) -> &'static str {
        match self {
            Self::Cloud => DEFAULT_CLOUD_MODEL,
            Self::Local => DEFAULT_LOCAL_MODEL,
        }
    }

    pub(crate) fn is_cloud(self) -> bool {
        matches!(self, Self::Cloud)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CloudAuth {
    /// `Authorization: Bearer <API key>`，适用于大多数 OpenAI-compatible 服务。
    #[default]
    Bearer,
    /// 将 API key 原样放入用户指定的 Header，例如 `api-key` 或 `x-api-key`。
    ApiKeyHeader,
    /// 不发送认证信息，适用于无需鉴权的受信任网关。
    None,
}

impl CloudAuth {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Bearer => "Bearer",
            Self::ApiKeyHeader => "Header",
            Self::None => "None",
        }
    }

    pub(crate) fn requires_key(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct LlmConfig {
    #[serde(default)]
    backend: LlmBackend,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key_origin: Option<String>,
    #[serde(default)]
    auth_mode: CloudAuth,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    api_key_header: String,
    api_url: String,
    model: String,
    /// 推理模型在复杂局面会消耗大量 token 思考，导致最终答案为空。
    /// 开启后按照已知服务端协议发送关闭思考参数。
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
        let auth_mode = if backend.is_cloud() {
            CloudAuth::Bearer
        } else {
            CloudAuth::None
        };
        Self::new_with_auth(backend, api_key, api_url, model, auth_mode, String::new())
    }

    pub(crate) fn new_with_auth(
        backend: LlmBackend,
        api_key: String,
        api_url: String,
        model: String,
        auth_mode: CloudAuth,
        api_key_header: String,
    ) -> ConfigResult<Self> {
        let mut api_key = api_key.trim().to_string();
        let mut api_key_header = api_key_header.trim().to_string();
        // The URL is opaque user configuration. In particular, trimming its final `/`
        // could corrupt a query value such as `?api-version=preview/`.
        let api_url = api_url.trim().to_string();
        let model = model.trim().to_string();
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
        if parsed.fragment().is_some() {
            return Err(ConfigError::Invalid(
                "API URL must not contain a fragment".to_string(),
            ));
        }

        let (auth_mode, api_key_origin) = match backend {
            LlmBackend::Cloud => {
                cloud::validate_url(&parsed).map_err(ConfigError::Invalid)?;
                cloud::validate_auth(auth_mode, &api_key_header, &api_key)
                    .map_err(ConfigError::Invalid)?;
                if auth_mode != CloudAuth::ApiKeyHeader {
                    api_key_header.clear();
                }
                if !auth_mode.requires_key() {
                    api_key.clear();
                }
                (
                    auth_mode,
                    auth_mode
                        .requires_key()
                        .then(|| parsed.origin().ascii_serialization()),
                )
            }
            LlmBackend::Local => {
                local::validate_url(&parsed).map_err(ConfigError::Invalid)?;
                // A cloud credential must never be retained or sent while the
                // local transport is selected.
                api_key.clear();
                api_key_header.clear();
                (CloudAuth::None, None)
            }
        };
        Ok(Self {
            backend,
            api_key,
            api_key_origin,
            auth_mode,
            api_key_header,
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
        let auth_mode = if backend.is_cloud() {
            CloudAuth::Bearer
        } else {
            CloudAuth::None
        };
        Self::new_unchecked_with_auth(backend, api_key, api_url, model, auth_mode, String::new())
    }

    #[cfg(test)]
    pub(crate) fn new_unchecked_with_auth(
        backend: LlmBackend,
        api_key: String,
        api_url: String,
        model: String,
        auth_mode: CloudAuth,
        api_key_header: String,
    ) -> Self {
        let api_key_origin = (backend.is_cloud() && auth_mode.requires_key()).then(|| {
            reqwest::Url::parse(&api_url)
                .ok()
                .map(|url| url.origin().ascii_serialization())
        });
        Self {
            backend,
            api_key,
            api_key_origin: api_key_origin.flatten(),
            auth_mode,
            api_key_header,
            api_url,
            model,
            no_reasoning: false,
        }
    }

    /// 关闭推理模型的思考过程；具体请求字段由云端协议适配器选择。
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

    pub(crate) fn api_key_origin(&self) -> Option<&str> {
        self.api_key_origin.as_deref()
    }

    pub(crate) fn auth_mode(&self) -> CloudAuth {
        self.auth_mode
    }

    pub(crate) fn api_key_header(&self) -> &str {
        &self.api_key_header
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

    fn sends_api_key(&self) -> bool {
        self.backend.is_cloud() && self.auth_mode.requires_key()
    }

    fn validate_cloud_key_origin(&self) -> Result<(), String> {
        if !self.sends_api_key() {
            return Ok(());
        }
        let parsed = reqwest::Url::parse(&self.api_url)
            .map_err(|error| format!("Invalid API URL: {error}"))?;
        let current_origin = parsed.origin().ascii_serialization();
        if self.api_key_origin.as_deref() != Some(current_origin.as_str()) {
            return Err(CLOUD_KEY_ORIGIN_CHANGED.to_string());
        }
        Ok(())
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
        Self::new_with_origin_policy(profiles, active_profile, false)
    }

    fn new_with_origin_policy(
        profiles: Vec<LlmProfile>,
        active_profile: usize,
        allow_missing_cloud_origin: bool,
    ) -> ConfigResult<Self> {
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
            let config = normalize_config(profile.config, allow_missing_cloud_origin)?;
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
        if version < u64::from(LEGACY_SETTINGS_SCHEMA_VERSION) {
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
        let backend_used_legacy_name =
            value
                .get("profiles")
                .and_then(Value::as_array)
                .is_some_and(|profiles| {
                    profiles.iter().any(|profile| {
                        profile.get("backend").and_then(Value::as_str) == Some("openrouter")
                    })
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
        let settings = Self::new_with_origin_policy(
            raw.profiles,
            raw.active_profile,
            version == u64::from(LEGACY_SETTINGS_SCHEMA_VERSION),
        )?;
        let changed = version != u64::from(SETTINGS_SCHEMA_VERSION)
            || backend_was_missing
            || backend_used_legacy_name
            || active_profile_was_missing
            || settings
                .profiles
                .iter()
                .zip(raw_names.iter().zip(raw_configs))
                .any(|(profile, (raw_name, raw_config))| {
                    profile.name.as_str() != raw_name.as_str()
                        || profile.config.backend != raw_config.backend
                        || profile.config.api_key != raw_config.api_key
                        || profile.config.api_key_origin != raw_config.api_key_origin
                        || profile.config.auth_mode != raw_config.auth_mode
                        || profile.config.api_key_header != raw_config.api_key_header
                        || profile.config.api_url != raw_config.api_url
                        || profile.config.model != raw_config.model
                        || profile.config.no_reasoning != raw_config.no_reasoning
                });
        Ok((settings, changed))
    }

    fn read_legacy_value(value: Value, path: &Path) -> ConfigResult<(Self, bool)> {
        let raw: LlmConfig = serde_json::from_value(value).map_err(|source| ConfigError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        let config = normalize_config(raw, true)?;
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

fn normalize_config(raw: LlmConfig, allow_missing_cloud_origin: bool) -> ConfigResult<LlmConfig> {
    let repaired_key = repair_paste_artifact(&raw.api_key);
    let sends_api_key = raw.sends_api_key();
    let bound_origin = if sends_api_key {
        raw.api_key_origin
            .as_deref()
            .map(canonical_api_origin)
            .transpose()?
    } else {
        None
    };
    let config = LlmConfig::new_with_auth(
        raw.backend,
        repaired_key,
        raw.api_url,
        raw.model,
        raw.auth_mode,
        raw.api_key_header,
    )?;
    if config.sends_api_key() {
        if let Some(bound_origin) = bound_origin {
            if config.api_key_origin.as_deref() != Some(bound_origin.as_str()) {
                return Err(ConfigError::Invalid(CLOUD_KEY_ORIGIN_CHANGED.to_string()));
            }
        } else if !allow_missing_cloud_origin {
            return Err(ConfigError::Invalid(CLOUD_KEY_ORIGIN_CHANGED.to_string()));
        }
    }
    Ok(config.no_reasoning(raw.no_reasoning))
}

fn canonical_api_origin(value: &str) -> ConfigResult<String> {
    let parsed = reqwest::Url::parse(value.trim())
        .map_err(|_| ConfigError::Invalid(CLOUD_KEY_ORIGIN_CHANGED.to_string()))?;
    if parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
    {
        return Err(ConfigError::Invalid(CLOUD_KEY_ORIGIN_CHANGED.to_string()));
    }
    Ok(parsed.origin().ascii_serialization())
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
    let parts = content
        .as_array()?
        .iter()
        .filter_map(|part| part.get("text")?.as_str())
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn api_error_message(value: &Value) -> Option<&str> {
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(Value::as_str))
}

fn http_error_message(value: &Value) -> Option<&str> {
    api_error_message(value).or_else(|| value.get("message").and_then(Value::as_str))
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
/// 因此收集 JSON、x/y 键值对和括号坐标，并采用文本中最后一个明确坐标。
fn parse_move(text: &str) -> Option<(usize, usize)> {
    let trimmed = text.trim();

    // 整体就是一个 JSON 对象时直接解析。
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(move_point) = xy_from_value(&value) {
            return Some(move_point);
        }
    }

    let mut last_explicit = last_json_move(trimmed)?;

    // 也接受不在对象中的成对 x/y 键值；对象内部由 last_json_move 隔离处理，
    // 避免把不同候选或 metadata 中的单个键拼成一个坐标。
    if let Some(move_point) = last_keyed_move_at_depth(trimmed, 0) {
        keep_later_move(&mut last_explicit, move_point);
    }

    if let Some(move_point) = last_number_pair_in_parens(trimmed) {
        keep_later_move(&mut last_explicit, move_point);
    }

    if let Some((_, move_point)) = last_explicit {
        return Some(move_point);
    }

    // JSON 包装对象中只有 metadata 坐标时不能把其中的两个数字当成最终落点。
    if trimmed.contains('{') || trimmed.contains('}') {
        return None;
    }

    // 兜底：文本中恰好出现两个数字。
    exactly_two_plain_integers(trimmed)
}

fn keep_later_move(
    current: &mut Option<(usize, (usize, usize))>,
    candidate: (usize, (usize, usize)),
) {
    if current.is_none_or(|(index, _)| candidate.0 >= index) {
        *current = Some(candidate);
    }
}

/// 仅扫描文本中的最外层对象，返回结束位置最靠后的有效坐标。
/// 标准 JSON 只读取对象顶层的 x/y；非标准 JSON（例如中文冒号）则在
/// 同一个最外层对象中成对读取，避免嵌套 metadata 或残缺候选互相混配。
fn last_json_move(text: &str) -> Option<Option<(usize, (usize, usize))>> {
    let mut object_start = None;
    let mut object_depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut last_match = None;

    for (index, character) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }

        match character {
            '"' => in_string = true,
            '{' => {
                if object_depth == 0 {
                    object_start = Some(index);
                }
                object_depth += 1;
            }
            '}' if object_depth > 0 => {
                object_depth -= 1;
                if object_depth != 0 {
                    continue;
                }
                let Some(start) = object_start.take() else {
                    continue;
                };
                let end = index + character.len_utf8();
                let object = &text[start..end];
                let move_point = serde_json::from_str::<Value>(object)
                    .ok()
                    .and_then(|value| xy_from_value(&value))
                    .or_else(|| last_keyed_move_at_depth(object, 1).map(|(_, point)| point));
                if let Some(move_point) = move_point {
                    keep_later_move(&mut last_match, (end, move_point));
                }
            }
            _ => {}
        }
    }
    (object_depth == 0).then_some(last_match)
}

fn xy_from_value(value: &Value) -> Option<(usize, usize)> {
    let x = usize::try_from(value.get("x")?.as_u64()?).ok()?;
    let y = usize::try_from(value.get("y")?.as_u64()?).ok()?;
    Some((x, y))
}

#[derive(Clone, Copy)]
enum CoordinateKey {
    X,
    Y,
}

/// 在指定对象深度收集键值，并且只有同一次扫描中完整出现一对 x/y 才产出候选。
/// 每产出一对后立即清空状态，因此后续残缺的 x 或 y 不会与上一候选混配。
fn last_keyed_move_at_depth(text: &str, target_depth: usize) -> Option<(usize, (usize, usize))> {
    let mut object_depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut pending_x = None;
    let mut pending_y = None;
    let mut last_match = None;

    for (index, character) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }

        match character {
            '"' => {
                if object_depth == target_depth {
                    if let Some((key, value)) = keyed_number(&text[index..]) {
                        match key {
                            CoordinateKey::X => pending_x = Some((index, value)),
                            CoordinateKey::Y => pending_y = Some((index, value)),
                        }
                        if let (Some((x_index, x)), Some((y_index, y))) = (pending_x, pending_y) {
                            last_match = Some((x_index.max(y_index), (x, y)));
                            pending_x = None;
                            pending_y = None;
                        }
                    }
                }
                in_string = true;
            }
            '{' => {
                if object_depth == target_depth {
                    pending_x = None;
                    pending_y = None;
                }
                object_depth += 1;
            }
            '}' => {
                object_depth = object_depth.saturating_sub(1);
                if object_depth == target_depth {
                    pending_x = None;
                    pending_y = None;
                }
            }
            'x' | 'y' if object_depth == target_depth => {
                if let Some((key, value)) = keyed_number(&text[index..]) {
                    match key {
                        CoordinateKey::X => pending_x = Some((index, value)),
                        CoordinateKey::Y => pending_y = Some((index, value)),
                    }
                    if let (Some((x_index, x)), Some((y_index, y))) = (pending_x, pending_y) {
                        last_match = Some((x_index.max(y_index), (x, y)));
                        pending_x = None;
                        pending_y = None;
                    }
                }
            }
            _ => {}
        }
    }
    last_match
}

fn keyed_number(text: &str) -> Option<(CoordinateKey, usize)> {
    let (key, rest) = if let Some(rest) = text.strip_prefix("\"x\"") {
        (CoordinateKey::X, rest)
    } else if let Some(rest) = text.strip_prefix("\"y\"") {
        (CoordinateKey::Y, rest)
    } else if let Some(rest) = text.strip_prefix('x') {
        (CoordinateKey::X, rest)
    } else {
        (CoordinateKey::Y, text.strip_prefix('y')?)
    };
    let rest = rest.trim_start_matches([' ', '\t', '\n', '\r']);
    let rest = rest
        .strip_prefix(':')
        .or_else(|| rest.strip_prefix('：'))?
        .trim_start_matches([' ', '\t', '\n', '\r']);
    Some((key, coordinate_integer(rest)?))
}

/// 解析一个完整的非负十进制整数；允许 JSON 数字字符串，但拒绝把
/// 小数、科学计数或负数的整数前缀误当成坐标。
fn coordinate_integer(text: &str) -> Option<usize> {
    let (number, tail) = if let Some(quoted) = text.strip_prefix('"') {
        let digit_bytes = quoted
            .chars()
            .take_while(|character| character.is_ascii_digit())
            .count();
        if digit_bytes == 0 {
            return None;
        }
        let tail = &quoted[digit_bytes..];
        let tail = tail.strip_prefix('"')?;
        (&quoted[..digit_bytes], tail)
    } else {
        let digit_bytes = text
            .chars()
            .take_while(|character| character.is_ascii_digit())
            .count();
        if digit_bytes == 0 {
            return None;
        }
        (&text[..digit_bytes], &text[digit_bytes..])
    };

    if let Some(next) = tail.chars().next() {
        let boundary = next.is_whitespace()
            || matches!(next, ',' | '，' | '}' | ']' | ';' | '；' | ')' | '）');
        if !boundary {
            return None;
        }
    }
    number.parse().ok()
}

fn exactly_two_plain_integers(text: &str) -> Option<(usize, usize)> {
    let mut values = Vec::new();
    let mut characters = text.char_indices().peekable();

    while let Some((start, character)) = characters.next() {
        if !character.is_ascii_digit() {
            continue;
        }
        let mut end = start + character.len_utf8();
        while let Some(&(index, next)) = characters.peek() {
            if !next.is_ascii_digit() {
                break;
            }
            characters.next();
            end = index + next.len_utf8();
        }

        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        if before.is_some_and(|value| {
            matches!(value, '-' | '+' | '−' | '.') || value.is_ascii_alphanumeric()
        }) || after.is_some_and(|value| value == '.' || matches!(value, 'e' | 'E'))
        {
            return None;
        }

        values.push(text[start..end].parse::<usize>().ok()?);
    }

    (values.len() == 2).then(|| (values[0], values[1]))
}

/// 提取形如 `(7, 7)` 或 `（7，7）` 的括号坐标对，支持中文逗号与空格。
/// 模型可能在讨论多个候选点后给出结论，因此返回最后一个坐标及其位置。
fn last_number_pair_in_parens(text: &str) -> Option<(usize, (usize, usize))> {
    let mut last_match = None;
    let mut object_depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => {
                in_string = true;
                continue;
            }
            '{' => {
                object_depth += 1;
                continue;
            }
            '}' => {
                object_depth = object_depth.saturating_sub(1);
                continue;
            }
            _ => {}
        }
        if object_depth != 0 {
            continue;
        }
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
            last_match = Some((index, (x, y)));
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
        LlmBackend::Cloud => {
            config.validate_cloud_key_origin()?;
            cloud::send_request(client, config, &messages).await?
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
            .and_then(http_error_message)
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
        if let Some(message) = value
            .get("message")
            .and_then(Value::as_str)
            .filter(|message| !message.trim().is_empty())
        {
            return format!(
                "{} response has no text: {}",
                config.backend.label(),
                error_detail(message)
            );
        }
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
        LlmBackend::Cloud => cloud::resolve_route(&value, &config.model, &config.api_url),
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
