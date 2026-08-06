//! LLM 配置弹窗。配置保存为被 Git 忽略的 JSON 文件。

use crate::llm_ai::{
    CLOUD_KEY_ORIGIN_CHANGED, CloudAuth, LlmBackend, LlmConfig, LlmProfile, LlmSettings,
    config_path,
};
use macroquad::miniquad::window::clipboard_get;
use macroquad::prelude::*;
#[cfg(target_os = "macos")]
use std::process::Command;

const PANEL_X: f32 = 70.0;
const PANEL_Y: f32 = 105.0;
const PANEL_W: f32 = 500.0;
const PANEL_H: f32 = 560.0;
const MESSAGE_BASELINE_Y: f32 = 565.0;
const ACTION_BUTTON_Y: f32 = 595.0;
const ACTION_BUTTON_H: f32 = 40.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfigField {
    ApiKey,
    ApiKeyHeader,
    Model,
    ApiUrl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProfileSlot {
    Black,
    White,
}

impl ProfileSlot {
    fn index(self) -> usize {
        match self {
            Self::Black => 0,
            Self::White => 1,
        }
    }
}

pub(crate) enum ConfigAction {
    None,
    Cancel,
    Save(LlmSettings),
}

struct ProfileDraft {
    name: String,
    backend: LlmBackend,
    api_key: String,
    api_key_origin: Option<String>,
    auth_mode: CloudAuth,
    api_key_header: String,
    model: String,
    api_url: String,
    no_reasoning: bool,
    cloud_model: String,
    cloud_api_url: String,
    local_model: String,
    local_api_url: String,
}

impl ProfileDraft {
    fn new(saved: Option<&LlmProfile>, default_name: &str) -> Self {
        let config = saved.map(LlmProfile::config);
        let backend = config.map_or(LlmBackend::Cloud, LlmConfig::backend);
        let model = config.map_or_else(
            || backend.default_model().to_string(),
            |config| config.model().to_string(),
        );
        let api_url = config.map_or_else(
            || backend.default_api_url().to_string(),
            |config| config.api_url().to_string(),
        );
        let mut cloud_model = LlmBackend::Cloud.default_model().to_string();
        let mut cloud_api_url = LlmBackend::Cloud.default_api_url().to_string();
        let mut local_model = LlmBackend::Local.default_model().to_string();
        let mut local_api_url = LlmBackend::Local.default_api_url().to_string();
        match backend {
            LlmBackend::Cloud => {
                cloud_model.clone_from(&model);
                cloud_api_url.clone_from(&api_url);
            }
            LlmBackend::Local => {
                local_model.clone_from(&model);
                local_api_url.clone_from(&api_url);
            }
        }
        let api_key = config.map_or_else(String::new, |config| config.api_key().to_string());
        let api_key_origin = config
            .and_then(LlmConfig::api_key_origin)
            .map(str::to_owned);
        let (auth_mode, api_key_header) = if backend.is_cloud() {
            (
                config.map_or(CloudAuth::Bearer, LlmConfig::auth_mode),
                config.map_or_else(String::new, |config| config.api_key_header().to_string()),
            )
        } else {
            (CloudAuth::Bearer, String::new())
        };
        Self {
            name: saved.map_or_else(|| default_name.to_string(), |profile| profile.name().into()),
            backend,
            api_key,
            api_key_origin,
            auth_mode,
            api_key_header,
            model,
            api_url,
            no_reasoning: config.is_some_and(LlmConfig::no_reasoning_enabled),
            cloud_model,
            cloud_api_url,
            local_model,
            local_api_url,
        }
    }

    fn select_backend(&mut self, backend: LlmBackend) {
        if self.backend == backend {
            return;
        }

        match self.backend {
            LlmBackend::Cloud => {
                self.cloud_model.clone_from(&self.model);
                self.cloud_api_url.clone_from(&self.api_url);
            }
            LlmBackend::Local => {
                self.local_model.clone_from(&self.model);
                self.local_api_url.clone_from(&self.api_url);
            }
        }
        self.backend = backend;
        match backend {
            LlmBackend::Cloud => {
                self.model.clone_from(&self.cloud_model);
                self.api_url.clone_from(&self.cloud_api_url);
            }
            LlmBackend::Local => {
                self.model.clone_from(&self.local_model);
                self.api_url.clone_from(&self.local_api_url);
            }
        }
    }

    fn active_value_mut(&mut self, field: ConfigField) -> &mut String {
        match field {
            ConfigField::ApiKey => &mut self.api_key,
            ConfigField::ApiKeyHeader => &mut self.api_key_header,
            ConfigField::Model => &mut self.model,
            ConfigField::ApiUrl => &mut self.api_url,
        }
    }

    fn cloud_key_needs_reentry(&self) -> bool {
        if !self.backend.is_cloud() || !self.auth_mode.requires_key() || self.api_key.is_empty() {
            return false;
        }
        let Some(current_origin) = api_origin(&self.api_url) else {
            return false;
        };
        self.api_key_origin.as_deref() != Some(current_origin.as_str())
    }

    fn prepare_api_key_edit(&mut self) {
        if self.cloud_key_needs_reentry() {
            // Keep a credential usable if the user merely previews another host and switches
            // back, but never merge an old provider's secret into a credential for a new host.
            self.api_key.clear();
        }
        self.api_key_origin = api_origin(&self.api_url);
    }

    fn build(&self) -> Result<LlmProfile, String> {
        if self.cloud_key_needs_reentry() {
            return Err(CLOUD_KEY_ORIGIN_CHANGED.to_string());
        }
        let config = if self.backend.is_cloud() && self.auth_mode == CloudAuth::Bearer {
            LlmConfig::new(
                self.backend,
                self.api_key.clone(),
                self.api_url.clone(),
                self.model.clone(),
            )
        } else {
            LlmConfig::new_with_auth(
                self.backend,
                self.api_key.clone(),
                self.api_url.clone(),
                self.model.clone(),
                self.auth_mode,
                self.api_key_header.clone(),
            )
        }
        .map_err(|error| error.to_string())?
        .no_reasoning(self.no_reasoning);
        LlmProfile::new(self.name.clone(), config).map_err(|error| error.to_string())
    }
}

pub(crate) struct LlmConfigPage {
    profiles: [ProfileDraft; 2],
    active_profile: ProfileSlot,
    human_profile_index: usize,
    white_enabled: bool,
    require_pair: bool,
    active: ConfigField,
    reveal_key: bool,
    message: String,
    load_error: Option<String>,
    suppress_paste_chars: bool,
}

impl LlmConfigPage {
    pub(crate) fn new(saved: Option<&LlmSettings>, load_error: Option<String>) -> Self {
        let saved_profiles = saved.map(LlmSettings::profiles).unwrap_or_default();
        let white_default_name = if saved_profiles
            .first()
            .is_some_and(|profile| profile.name() == "White")
        {
            "White 2"
        } else {
            "White"
        };
        let profiles = [
            ProfileDraft::new(saved_profiles.first(), "Black"),
            ProfileDraft::new(saved_profiles.get(1), white_default_name),
        ];
        let white_enabled = saved_profiles.len() > 1;
        let active_profile = if white_enabled
            && saved.is_some_and(|settings| settings.active_profile_index() == 1)
        {
            ProfileSlot::White
        } else {
            ProfileSlot::Black
        };
        let active = Self::first_field(&profiles[active_profile.index()]);
        Self {
            profiles,
            active_profile,
            human_profile_index: saved.map_or(0, LlmSettings::active_profile_index),
            white_enabled,
            require_pair: false,
            active,
            reveal_key: false,
            message: String::new(),
            load_error,
            suppress_paste_chars: false,
        }
    }

    pub(crate) fn open(&mut self, require_pair: bool) {
        self.require_pair = require_pair;
        if require_pair && !self.white_enabled {
            self.white_enabled = true;
            self.active_profile = ProfileSlot::White;
        }
        self.active = Self::first_field(self.active_draft());
        self.reveal_key = false;
        self.message = self.load_error.clone().unwrap_or_default();
        if self.message.is_empty() {
            self.update_cloud_key_message();
        }
    }

    fn first_field(draft: &ProfileDraft) -> ConfigField {
        if draft.backend.is_cloud() {
            ConfigField::ApiUrl
        } else {
            ConfigField::Model
        }
    }

    fn shows_api_key(&self) -> bool {
        let draft = self.active_draft();
        draft.backend.is_cloud() && draft.auth_mode.requires_key()
    }

    fn shows_api_key_header(&self) -> bool {
        let draft = self.active_draft();
        draft.backend.is_cloud() && draft.auth_mode == CloudAuth::ApiKeyHeader
    }

    fn select_backend(&mut self, backend: LlmBackend) {
        if self.active_draft().backend == backend {
            return;
        }
        self.active_draft_mut().select_backend(backend);
        self.active = Self::first_field(self.active_draft());
        self.reveal_key = false;
        self.message.clear();
        self.update_cloud_key_message();
    }

    fn select_auth_mode(&mut self, auth_mode: CloudAuth) {
        if !self.active_draft().backend.is_cloud() || self.active_draft().auth_mode == auth_mode {
            return;
        }
        let draft = self.active_draft_mut();
        draft.auth_mode = auth_mode;
        if auth_mode == CloudAuth::ApiKeyHeader && draft.api_key_header.is_empty() {
            draft.api_key_header = "x-api-key".to_string();
        }
        if !auth_mode.requires_key() {
            self.reveal_key = false;
        }
        if matches!(self.active, ConfigField::ApiKey | ConfigField::ApiKeyHeader)
            && (!auth_mode.requires_key()
                || (self.active == ConfigField::ApiKeyHeader
                    && auth_mode != CloudAuth::ApiKeyHeader))
        {
            self.active = ConfigField::ApiUrl;
        }
        self.message.clear();
        self.update_cloud_key_message();
    }

    fn select_profile(&mut self, profile: ProfileSlot) {
        if profile == ProfileSlot::White {
            self.white_enabled = true;
        }
        self.active_profile = profile;
        self.active = Self::first_field(self.active_draft());
        self.reveal_key = false;
        self.message.clear();
        self.update_cloud_key_message();
    }

    fn use_one_profile(&mut self) {
        if self.human_profile_index == ProfileSlot::White.index() {
            self.profiles.swap(0, 1);
        }
        self.white_enabled = false;
        self.active_profile = ProfileSlot::Black;
        self.human_profile_index = 0;
        self.active = Self::first_field(self.active_draft());
        self.reveal_key = false;
        self.message = "Second profile will be removed when saved".to_string();
    }

    fn active_draft(&self) -> &ProfileDraft {
        &self.profiles[self.active_profile.index()]
    }

    fn active_draft_mut(&mut self) -> &mut ProfileDraft {
        &mut self.profiles[self.active_profile.index()]
    }

    fn active_value_mut(&mut self) -> &mut String {
        let active = self.active;
        self.active_draft_mut().active_value_mut(active)
    }

    fn edit_active_value(&mut self, edit: impl FnOnce(&mut String)) {
        let active = self.active;
        if active == ConfigField::ApiKey {
            self.active_draft_mut().prepare_api_key_edit();
        }
        edit(self.active_value_mut());
        self.update_cloud_key_message();
    }

    fn update_cloud_key_message(&mut self) {
        if self.active_draft().cloud_key_needs_reentry() {
            self.message = CLOUD_KEY_ORIGIN_CHANGED.to_string();
        } else if self.message == CLOUD_KEY_ORIGIN_CHANGED {
            self.message.clear();
        }
    }

    fn next_field(&mut self) {
        let draft = self.active_draft();
        let cloud = draft.backend.is_cloud();
        let auth_mode = draft.auth_mode;
        self.active = match (cloud, auth_mode, self.active) {
            (true, _, ConfigField::ApiUrl) => ConfigField::Model,
            (true, CloudAuth::ApiKeyHeader, ConfigField::Model) => ConfigField::ApiKeyHeader,
            (true, CloudAuth::Bearer, ConfigField::Model) => ConfigField::ApiKey,
            (true, CloudAuth::None, ConfigField::Model) => ConfigField::ApiUrl,
            (true, CloudAuth::ApiKeyHeader, ConfigField::ApiKeyHeader) => ConfigField::ApiKey,
            (true, _, ConfigField::ApiKey) => ConfigField::ApiUrl,
            (true, CloudAuth::Bearer | CloudAuth::None, ConfigField::ApiKeyHeader) => {
                ConfigField::ApiUrl
            }
            (false, _, ConfigField::Model) => ConfigField::ApiUrl,
            (false, _, ConfigField::ApiKey | ConfigField::ApiKeyHeader | ConfigField::ApiUrl) => {
                ConfigField::Model
            }
        };
    }

    fn build_settings(&self) -> Result<LlmSettings, String> {
        let black = self.profiles[ProfileSlot::Black.index()]
            .build()
            .map_err(|error| format!("Black: {error}"))?;
        let mut profiles = vec![black];
        if self.require_pair || self.white_enabled {
            profiles.push(
                self.profiles[ProfileSlot::White.index()]
                    .build()
                    .map_err(|error| format!("White: {error}"))?,
            );
        }
        let active_profile = self.human_profile_index.min(profiles.len() - 1);
        LlmSettings::new(profiles, active_profile).map_err(|error| error.to_string())
    }

    fn paste_active(&mut self) {
        let Some(value) = read_clipboard_text() else {
            self.message = "Clipboard is empty or unavailable".to_string();
            return;
        };
        self.apply_pasted_value(&value);
    }

    fn apply_pasted_value(&mut self, value: &str) {
        let value = value.trim_matches(|c: char| c.is_whitespace());
        if value.is_empty() {
            self.message = "Clipboard is empty or unavailable".to_string();
            return;
        }
        self.message.clear();
        self.edit_active_value(|active| value.clone_into(active));
    }

    fn handle_keyboard(&mut self) {
        // macOS 可能在 Cmd+V 后继续投递字符 'v'；在 V 释放前丢弃这批字符事件。
        if self.suppress_paste_chars {
            while get_char_pressed().is_some() {}
            if is_key_down(KeyCode::V) {
                return;
            }
            self.suppress_paste_chars = false;
        }

        if is_key_pressed(KeyCode::Tab) {
            self.next_field();
        }
        if is_key_pressed(KeyCode::Backspace) {
            self.edit_active_value(|active| {
                active.pop();
            });
        }

        let modifier = is_key_down(KeyCode::LeftControl)
            || is_key_down(KeyCode::RightControl)
            || is_key_down(KeyCode::LeftSuper)
            || is_key_down(KeyCode::RightSuper);
        let pasted = modifier && is_key_pressed(KeyCode::V);
        if pasted {
            self.paste_active();
            self.suppress_paste_chars = true;
            while get_char_pressed().is_some() {}
        } else {
            while let Some(character) = get_char_pressed() {
                if !character.is_control() {
                    self.edit_active_value(|active| active.push(character));
                }
            }
        }
    }

    pub(crate) fn draw_and_update(&mut self) -> ConfigAction {
        draw_rectangle(
            0.0,
            0.0,
            screen_width(),
            screen_height(),
            Color::from_rgba(0, 0, 0, 190),
        );
        draw_rectangle(
            PANEL_X,
            PANEL_Y,
            PANEL_W,
            PANEL_H,
            Color::from_rgba(38, 43, 53, 255),
        );
        draw_rectangle_lines(
            PANEL_X,
            PANEL_Y,
            PANEL_W,
            PANEL_H,
            2.0,
            Color::from_rgba(100, 135, 180, 255),
        );

        draw_text("LLM Configuration", 100.0, 150.0, 28.0, WHITE);
        let black_profile_rect = Rect::new(350.0, 122.0, 90.0, 34.0);
        let white_profile_rect = Rect::new(448.0, 122.0, 92.0, 34.0);
        let black_label = if self.require_pair {
            "Black"
        } else if self.human_profile_index == ProfileSlot::Black.index() {
            "Primary"
        } else {
            "Opponent"
        };
        if draw_choice_button(
            black_profile_rect,
            black_label,
            self.active_profile == ProfileSlot::Black,
        ) {
            self.select_profile(ProfileSlot::Black);
        }
        let white_label = if self.require_pair {
            "White"
        } else if self.white_enabled && self.human_profile_index == ProfileSlot::White.index() {
            "Primary"
        } else if self.white_enabled {
            "Opponent"
        } else {
            "Add +"
        };
        if draw_choice_button(
            white_profile_rect,
            white_label,
            self.active_profile == ProfileSlot::White,
        ) {
            self.select_profile(ProfileSlot::White);
        }

        draw_text(
            "Backend",
            100.0,
            188.0,
            18.0,
            Color::from_rgba(220, 225, 235, 255),
        );
        let cloud_backend_rect = Rect::new(190.0, 163.0, 110.0, 34.0);
        let local_backend_rect = Rect::new(308.0, 163.0, 72.0, 34.0);
        let active_backend = self.active_draft().backend;
        if draw_choice_button(
            cloud_backend_rect,
            "Cloud API",
            active_backend == LlmBackend::Cloud,
        ) {
            self.select_backend(LlmBackend::Cloud);
        }
        if draw_choice_button(
            local_backend_rect,
            "Local",
            active_backend == LlmBackend::Local,
        ) {
            self.select_backend(LlmBackend::Local);
        }
        if !self.require_pair
            && self.white_enabled
            && draw_button(Rect::new(390.0, 163.0, 150.0, 34.0), "Use One Only")
        {
            self.use_one_profile();
        }
        let auth_bearer_rect = Rect::new(155.0, 203.0, 72.0, 32.0);
        let auth_header_rect = Rect::new(234.0, 203.0, 72.0, 32.0);
        let auth_none_rect = Rect::new(313.0, 203.0, 60.0, 32.0);
        let api_key_header_rect = Rect::new(382.0, 203.0, 158.0, 32.0);
        if self.active_draft().backend.is_cloud() {
            draw_text(
                "Auth",
                100.0,
                227.0,
                18.0,
                Color::from_rgba(220, 225, 235, 255),
            );
            let auth_mode = self.active_draft().auth_mode;
            if draw_choice_button(
                auth_bearer_rect,
                CloudAuth::Bearer.label(),
                auth_mode == CloudAuth::Bearer,
            ) {
                self.select_auth_mode(CloudAuth::Bearer);
            }
            if draw_choice_button(
                auth_header_rect,
                CloudAuth::ApiKeyHeader.label(),
                auth_mode == CloudAuth::ApiKeyHeader,
            ) {
                self.select_auth_mode(CloudAuth::ApiKeyHeader);
            }
            if draw_choice_button(
                auth_none_rect,
                CloudAuth::None.label(),
                auth_mode == CloudAuth::None,
            ) {
                self.select_auth_mode(CloudAuth::None);
            }
            if self.shows_api_key_header() {
                draw_field(
                    api_key_header_rect,
                    &self.active_draft().api_key_header,
                    self.active == ConfigField::ApiKeyHeader,
                );
            }
        }

        let key_rect = Rect::new(100.0, 265.0, 295.0, 38.0);
        let paste_rect = Rect::new(405.0, 265.0, 65.0, 38.0);
        let show_rect = Rect::new(480.0, 265.0, 60.0, 38.0);
        let model_rect = Rect::new(100.0, 340.0, 440.0, 38.0);
        let url_rect = Rect::new(100.0, 415.0, 440.0, 38.0);
        draw_text(
            "Model",
            100.0,
            332.0,
            18.0,
            Color::from_rgba(220, 225, 235, 255),
        );
        draw_text(
            "Full Chat Completions API URL",
            100.0,
            407.0,
            18.0,
            Color::from_rgba(220, 225, 235, 255),
        );

        if self.shows_api_key() {
            draw_text(
                "API Key",
                100.0,
                257.0,
                18.0,
                Color::from_rgba(220, 225, 235, 255),
            );
            let key_display = if self.reveal_key {
                self.active_draft().api_key.clone()
            } else {
                masked_api_key(&self.active_draft().api_key)
            };
            draw_field(key_rect, &key_display, self.active == ConfigField::ApiKey);
        } else if self.active_draft().backend == LlmBackend::Local {
            draw_text(
                "Local OpenAI-compatible server",
                100.0,
                278.0,
                18.0,
                Color::from_rgba(205, 220, 235, 255),
            );
            draw_text(
                "No key is sent; only 127.0.0.1 or ::1 is allowed.",
                100.0,
                300.0,
                15.0,
                Color::from_rgba(155, 190, 170, 255),
            );
        } else {
            draw_text(
                "No authentication header will be sent.",
                100.0,
                285.0,
                17.0,
                Color::from_rgba(155, 190, 170, 255),
            );
        }
        draw_field(
            model_rect,
            &self.active_draft().model,
            self.active == ConfigField::Model,
        );
        draw_field(
            url_rect,
            &self.active_draft().api_url,
            self.active == ConfigField::ApiUrl,
        );

        let no_reasoning_rect = Rect::new(100.0, 470.0, 18.0, 18.0);
        if self.active_draft().backend.is_cloud() {
            let draft = self.active_draft();
            let checked = draft.no_reasoning;
            if checked {
                draw_rectangle(
                    100.0,
                    470.0,
                    18.0,
                    18.0,
                    Color::from_rgba(120, 200, 160, 255),
                );
                draw_text("✓", 103.0, 485.0, 16.0, Color::from_rgba(20, 40, 30, 255));
            } else {
                draw_rectangle_lines(
                    100.0,
                    470.0,
                    18.0,
                    18.0,
                    2.0,
                    Color::from_rgba(180, 190, 205, 255),
                );
            }
            draw_text(
                "Disable model reasoning (provider-specific)",
                128.0,
                484.0,
                15.0,
                Color::from_rgba(220, 225, 235, 255),
            );
        }

        let (mx, my) = mouse_position();
        let clicked = is_mouse_button_pressed(MouseButton::Left);
        if self.shows_api_key() && clicked && key_rect.contains(vec2(mx, my)) {
            self.active = ConfigField::ApiKey;
        }
        if self.shows_api_key_header() && clicked && api_key_header_rect.contains(vec2(mx, my)) {
            self.active = ConfigField::ApiKeyHeader;
        }
        if clicked && model_rect.contains(vec2(mx, my)) {
            self.active = ConfigField::Model;
        }
        if clicked && url_rect.contains(vec2(mx, my)) {
            self.active = ConfigField::ApiUrl;
        }
        if self.active_draft().backend.is_cloud()
            && clicked
            && no_reasoning_rect.contains(vec2(mx, my))
        {
            self.active_draft_mut().no_reasoning = !self.active_draft().no_reasoning;
        }
        if self.shows_api_key() {
            if draw_button(paste_rect, "Paste") {
                self.active = ConfigField::ApiKey;
                self.paste_active();
            }
            if draw_button(show_rect, if self.reveal_key { "Hide" } else { "Show" }) {
                self.reveal_key = !self.reveal_key;
            }
        }

        let help = if self.active_draft().backend.is_cloud() {
            "Custom HTTPS endpoint; only a non-secret api-version query is allowed."
        } else {
            "Ollama preset; also works with LM Studio and llama.cpp."
        };
        draw_text(
            help,
            100.0,
            520.0,
            15.0,
            Color::from_rgba(155, 170, 190, 255),
        );
        let storage_note = config_path().map_or_else(
            |error| format!("Configuration path unavailable: {error}"),
            |path| format!("Saved to {}", path.display()),
        );
        let storage_note = visible_tail_to_width(&storage_note, 440.0, 14);
        draw_text(
            &storage_note,
            100.0,
            542.0,
            14.0,
            Color::from_rgba(180, 190, 205, 255),
        );
        if !self.message.is_empty() {
            let message = visible_head_to_width(&self.message, 440.0, 16);
            draw_text(
                &message,
                100.0,
                MESSAGE_BASELINE_Y,
                16.0,
                Color::from_rgba(255, 145, 120, 255),
            );
        }

        self.handle_keyboard();
        let cancel = draw_button(action_button_rect(330.0), "Cancel");
        let save_label = if self.require_pair || self.white_enabled {
            "Save Both"
        } else {
            "Save"
        };
        let save = draw_button(action_button_rect(440.0), save_label);
        if cancel || is_key_pressed(KeyCode::Escape) {
            return ConfigAction::Cancel;
        }
        if save || is_key_pressed(KeyCode::Enter) {
            match self.build_settings() {
                Ok(settings) => match settings.save() {
                    Ok(()) => {
                        self.load_error = None;
                        return ConfigAction::Save(settings);
                    }
                    Err(error) => self.message = error.to_string(),
                },
                Err(error) => self.message = error.to_string(),
            }
        }
        ConfigAction::None
    }
}

fn api_origin(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value.trim()).ok()?;
    url.host_str()?;
    Some(url.origin().ascii_serialization())
}

fn action_button_rect(x: f32) -> Rect {
    Rect::new(x, ACTION_BUTTON_Y, 100.0, ACTION_BUTTON_H)
}

fn read_clipboard_text() -> Option<String> {
    if let Some(value) = clipboard_get() {
        return Some(value);
    }

    // miniquad 0.4.10 的 macOS 通用 Clipboard 实现固定返回 None，使用系统命令回退。
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("pbpaste").output().ok()?;
        if output.status.success() {
            return String::from_utf8(output.stdout).ok();
        }
    }
    None
}

fn masked_api_key(api_key: &str) -> String {
    "*".repeat(api_key.chars().count())
}

fn visible_tail(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    format!(
        "...{}",
        value
            .chars()
            .skip(count - max_chars + 3)
            .collect::<String>()
    )
}

fn visible_head(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    format!(
        "{}...",
        value
            .chars()
            .take(max_chars.saturating_sub(3))
            .collect::<String>()
    )
}

fn visible_head_to_width(value: &str, max_width: f32, font_size: u16) -> String {
    let mut max_chars = value.chars().count();
    loop {
        let candidate = visible_head(value, max_chars);
        if measure_text(&candidate, None, font_size, 1.0).width <= max_width || max_chars == 0 {
            return candidate;
        }
        max_chars -= 1;
    }
}

fn visible_tail_to_width(value: &str, max_width: f32, font_size: u16) -> String {
    let mut max_chars = value.chars().count();
    loop {
        let candidate = visible_tail(value, max_chars);
        if measure_text(&candidate, None, font_size, 1.0).width <= max_width || max_chars == 0 {
            return candidate;
        }
        max_chars -= 1;
    }
}

fn draw_field(rect: Rect, value: &str, active: bool) {
    draw_rectangle(
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        Color::from_rgba(25, 29, 36, 255),
    );
    draw_rectangle_lines(
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        if active { 2.0 } else { 1.0 },
        if active {
            Color::from_rgba(100, 165, 235, 255)
        } else {
            Color::from_rgba(90, 100, 115, 255)
        },
    );
    let shown = visible_tail_to_width(value, rect.w - 20.0, 18);
    draw_text(&shown, rect.x + 10.0, rect.y + 25.0, 18.0, WHITE);
    if active && (get_time() * 2.0) as i64 % 2 == 0 {
        let width = measure_text(&shown, None, 18, 1.0).width;
        draw_line(
            rect.x + 11.0 + width,
            rect.y + 9.0,
            rect.x + 11.0 + width,
            rect.y + 29.0,
            1.5,
            WHITE,
        );
    }
}

fn draw_button(rect: Rect, label: &str) -> bool {
    let (mx, my) = mouse_position();
    let hover = rect.contains(vec2(mx, my));
    draw_rectangle(
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        if hover {
            Color::from_rgba(90, 130, 180, 255)
        } else {
            Color::from_rgba(70, 105, 150, 255)
        },
    );
    let size = measure_text(label, None, 18, 1.0);
    draw_text(
        label,
        rect.x + (rect.w - size.width) / 2.0,
        rect.y + (rect.h + size.height) / 2.0 - 2.0,
        18.0,
        WHITE,
    );
    hover && is_mouse_button_pressed(MouseButton::Left)
}

fn draw_choice_button(rect: Rect, label: &str, selected: bool) -> bool {
    let (mx, my) = mouse_position();
    let hover = rect.contains(vec2(mx, my));
    let color = if selected {
        Color::from_rgba(70, 135, 195, 255)
    } else if hover {
        Color::from_rgba(75, 95, 125, 255)
    } else {
        Color::from_rgba(55, 65, 82, 255)
    };
    draw_rectangle(rect.x, rect.y, rect.w, rect.h, color);
    draw_rectangle_lines(
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        if selected { 2.0 } else { 1.0 },
        if selected {
            Color::from_rgba(140, 195, 245, 255)
        } else {
            Color::from_rgba(90, 105, 125, 255)
        },
    );
    let size = measure_text(label, None, 16, 1.0);
    draw_text(
        label,
        rect.x + (rect.w - size.width) / 2.0,
        rect.y + (rect.h + size.height) / 2.0 - 2.0,
        16.0,
        WHITE,
    );
    hover && is_mouse_button_pressed(MouseButton::Left)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_tail_keeps_short_values_and_truncates_long_ones() {
        assert_eq!(visible_tail("short", 10), "short");
        assert_eq!(visible_tail("abcdefghijkl", 8), "...hijkl");
    }

    #[test]
    fn visible_head_keeps_the_actionable_start_of_long_errors() {
        assert_eq!(visible_head("abcdefghijkl", 8), "abcde...");
    }

    #[test]
    fn text_truncation_preserves_unicode_character_boundaries() {
        assert_eq!(visible_head("🚀模型配置", 4), "🚀...");
        assert_eq!(visible_tail("配置模型🚀", 4), "...🚀");
    }

    #[test]
    fn text_truncation_respects_limits_smaller_than_the_ellipsis() {
        assert_eq!(visible_head("abcdef", 0), "");
        assert_eq!(visible_head("abcdef", 2), "..");
        assert_eq!(visible_tail("abcdef", 2), "..");
    }

    #[test]
    fn api_key_mask_does_not_expose_sensitive_characters() {
        let secret = "sk-or-v1-秘密";
        let masked = masked_api_key(secret);

        assert_eq!(masked, "*".repeat(secret.chars().count()));
        assert!(!masked.contains("sk-or"));
        assert!(!masked.contains('秘'));
    }

    #[test]
    fn pasting_an_api_key_replaces_the_previous_value() {
        let mut page = LlmConfigPage::new(None, None);
        page.active = ConfigField::ApiKey;
        page.active_draft_mut().api_key = "old-key".to_string();

        page.apply_pasted_value("  new-key\n");

        assert_eq!(page.active_draft().api_key, "new-key");
        assert!(page.message.is_empty());
    }

    #[test]
    fn pasting_unicode_replaces_the_previous_model_value() {
        let mut page = LlmConfigPage::new(None, None);
        page.active = ConfigField::Model;
        page.active_draft_mut().model = "old-model".to_string();

        page.apply_pasted_value("  本地模型🦀\n");

        assert_eq!(page.active_draft().model, "本地模型🦀");
        assert!(page.message.is_empty());
    }

    #[test]
    fn opening_configuration_surfaces_a_previous_load_error() {
        let mut page = LlmConfigPage::new(None, Some("Invalid configuration".to_string()));

        page.open(false);

        assert_eq!(page.message, "Invalid configuration");
    }

    #[test]
    fn constructor_uses_the_persisted_key_origin_instead_of_the_current_url() {
        let settings: LlmSettings = serde_json::from_value(serde_json::json!({
            "schema_version": 3,
            "profiles": [{
                "name": "Cloud",
                "backend": "openrouter",
                "api_key": "provider-a-secret",
                "api_key_origin": "https://provider-a.example",
                "api_url": "https://provider-b.example/v1/chat/completions",
                "model": "model"
            }],
            "active_profile": 0
        }))
        .unwrap();
        let mut page = LlmConfigPage::new(Some(&settings), None);

        page.open(false);

        assert_eq!(
            page.active_draft().api_key_origin.as_deref(),
            Some("https://provider-a.example")
        );
        assert_eq!(page.message, CLOUD_KEY_ORIGIN_CHANGED);
    }

    #[test]
    fn message_and_action_buttons_have_separate_vertical_space() {
        let cancel = action_button_rect(330.0);

        assert!(MESSAGE_BASELINE_Y < cancel.y);
        assert!(cancel.y + cancel.h <= PANEL_Y + PANEL_H);
    }

    #[test]
    fn changing_only_the_cloud_api_path_keeps_the_key_usable() {
        let mut page = LlmConfigPage::new(None, None);
        page.active = ConfigField::ApiKey;
        page.apply_pasted_value("cloud-secret");
        page.active = ConfigField::ApiUrl;

        page.apply_pasted_value("https://openrouter.ai/custom/chat/completions");

        assert!(page.message.is_empty());
        let settings = page.build_settings().unwrap();
        assert_eq!(settings.active_profile().config().api_key(), "cloud-secret");
    }

    #[test]
    fn changing_only_a_non_secret_cloud_query_keeps_the_key_usable() {
        let mut page = LlmConfigPage::new(None, None);
        page.active = ConfigField::ApiKey;
        page.apply_pasted_value("cloud-secret");
        page.active = ConfigField::ApiUrl;

        page.apply_pasted_value(
            "https://openrouter.ai/api/v1/chat/completions?api-version=2026-01-01",
        );

        assert!(page.message.is_empty());
        let settings = page.build_settings().unwrap();
        assert_eq!(settings.active_profile().config().api_key(), "cloud-secret");
    }

    #[test]
    fn changing_cloud_scheme_or_port_requires_key_reentry() {
        let mut page = LlmConfigPage::new(None, None);
        page.active = ConfigField::ApiKey;
        page.apply_pasted_value("cloud-secret");
        page.active = ConfigField::ApiUrl;

        for changed_origin in [
            "http://openrouter.ai/api/v1/chat/completions",
            "https://openrouter.ai:8443/api/v1/chat/completions",
        ] {
            page.apply_pasted_value(changed_origin);
            assert_eq!(page.message, CLOUD_KEY_ORIGIN_CHANGED);
            let error = match page.build_settings() {
                Ok(_) => panic!("a key bound to another cloud origin must not be saved"),
                Err(error) => error,
            };
            assert!(error.contains("origin changed"));

            page.apply_pasted_value(LlmBackend::Cloud.default_api_url());
            assert!(page.message.is_empty());
            assert!(page.build_settings().is_ok());
        }
    }

    #[test]
    fn changing_cloud_host_blocks_the_key_until_it_is_reentered() {
        let mut page = LlmConfigPage::new(None, None);
        page.active = ConfigField::ApiKey;
        page.apply_pasted_value("openrouter-secret");
        page.active = ConfigField::ApiUrl;

        page.apply_pasted_value("https://api.deepseek.com/chat/completions");

        assert_eq!(page.active_draft().api_key, "openrouter-secret");
        assert_eq!(page.message, CLOUD_KEY_ORIGIN_CHANGED);
        let error = match page.build_settings() {
            Ok(_) => panic!("a key bound to another cloud host must not be saved"),
            Err(error) => error,
        };
        assert!(error.contains("re-enter"));
        assert!(!error.contains("openrouter-secret"));

        page.apply_pasted_value(LlmBackend::Cloud.default_api_url());
        assert!(page.message.is_empty());
        assert_eq!(
            page.build_settings()
                .unwrap()
                .active_profile()
                .config()
                .api_key(),
            "openrouter-secret"
        );

        page.apply_pasted_value("https://api.deepseek.com/chat/completions");
        page.active = ConfigField::ApiKey;
        page.apply_pasted_value("deepseek-secret");

        let settings = page.build_settings().unwrap();
        assert_eq!(
            settings.active_profile().config().api_key(),
            "deepseek-secret"
        );
        assert_eq!(
            settings.active_profile().config().api_url(),
            "https://api.deepseek.com/chat/completions"
        );
    }

    #[test]
    fn editing_a_key_after_changing_cloud_host_does_not_reuse_the_old_secret() {
        let mut page = LlmConfigPage::new(None, None);
        page.active = ConfigField::ApiKey;
        page.apply_pasted_value("old-provider-secret");
        page.active = ConfigField::ApiUrl;
        page.apply_pasted_value("https://api.example.com/v1/chat/completions");
        page.active = ConfigField::ApiKey;

        page.edit_active_value(|key| key.push('n'));

        assert_eq!(page.active_draft().api_key, "n");
        assert!(!page.active_draft().api_key.contains("old-provider-secret"));
    }

    #[test]
    fn backend_and_profile_switches_refresh_the_cloud_origin_warning() {
        let mut page = LlmConfigPage::new(None, None);
        page.active = ConfigField::ApiKey;
        page.apply_pasted_value("black-cloud-secret");
        page.active = ConfigField::ApiUrl;
        page.apply_pasted_value("https://api.deepseek.com/chat/completions");
        assert_eq!(page.message, CLOUD_KEY_ORIGIN_CHANGED);

        page.select_backend(LlmBackend::Local);
        assert!(page.message.is_empty());
        page.select_backend(LlmBackend::Cloud);
        assert_eq!(page.message, CLOUD_KEY_ORIGIN_CHANGED);

        page.select_profile(ProfileSlot::White);
        assert!(page.message.is_empty());
        page.select_profile(ProfileSlot::Black);
        assert_eq!(page.message, CLOUD_KEY_ORIGIN_CHANGED);
    }

    #[test]
    fn selecting_local_applies_ollama_defaults_and_skips_api_key() {
        let mut page = LlmConfigPage::new(None, None);
        page.active_draft_mut().api_key = "keep-for-cloud".to_string();

        page.select_backend(LlmBackend::Local);

        assert_eq!(page.active_draft().backend, LlmBackend::Local);
        assert_eq!(page.active_draft().model, LlmBackend::Local.default_model());
        assert_eq!(
            page.active_draft().api_url,
            LlmBackend::Local.default_api_url()
        );
        assert_eq!(page.active_draft().api_key, "keep-for-cloud");
        assert_eq!(page.active, ConfigField::Model);
        assert!(!page.shows_api_key());

        page.next_field();
        assert_eq!(page.active, ConfigField::ApiUrl);
        page.next_field();
        assert_eq!(page.active, ConfigField::Model);
    }

    #[test]
    fn cloud_configuration_starts_with_the_endpoint_and_tabs_through_bearer_fields() {
        let mut page = LlmConfigPage::new(None, None);

        assert_eq!(page.active, ConfigField::ApiUrl);
        assert_eq!(page.active_draft().auth_mode, CloudAuth::Bearer);
        assert!(page.shows_api_key());
        assert!(!page.shows_api_key_header());

        page.next_field();
        assert_eq!(page.active, ConfigField::Model);
        page.next_field();
        assert_eq!(page.active, ConfigField::ApiKey);
        page.next_field();
        assert_eq!(page.active, ConfigField::ApiUrl);
    }

    #[test]
    fn custom_header_auth_exposes_and_persists_the_header_name() {
        let mut page = LlmConfigPage::new(None, None);
        page.select_auth_mode(CloudAuth::ApiKeyHeader);

        assert!(page.shows_api_key());
        assert!(page.shows_api_key_header());
        assert_eq!(page.active_draft().api_key_header, "x-api-key");

        page.next_field();
        assert_eq!(page.active, ConfigField::Model);
        page.next_field();
        assert_eq!(page.active, ConfigField::ApiKeyHeader);
        page.next_field();
        assert_eq!(page.active, ConfigField::ApiKey);

        page.active_draft_mut().api_key = "provider-secret".to_string();
        let origin = api_origin(&page.active_draft().api_url);
        page.active_draft_mut().api_key_origin = origin;
        page.active_draft_mut().api_key_header = "api-key".to_string();
        let settings = page.build_settings().unwrap();
        let config = settings.active_profile().config();

        assert_eq!(config.auth_mode(), CloudAuth::ApiKeyHeader);
        assert_eq!(config.api_key_header(), "api-key");
        assert_eq!(config.api_key(), "provider-secret");
    }

    #[test]
    fn no_auth_cloud_configuration_hides_and_drops_the_api_key() {
        let mut page = LlmConfigPage::new(None, None);
        page.active_draft_mut().api_key = "unused-secret".to_string();

        page.select_auth_mode(CloudAuth::None);

        assert!(!page.shows_api_key());
        assert!(!page.shows_api_key_header());
        page.next_field();
        assert_eq!(page.active, ConfigField::Model);
        page.next_field();
        assert_eq!(page.active, ConfigField::ApiUrl);

        let settings = page.build_settings().unwrap();
        let config = settings.active_profile().config();
        assert_eq!(config.auth_mode(), CloudAuth::None);
        assert!(config.api_key().is_empty());
        assert_eq!(config.api_key_origin(), None);
    }

    #[test]
    fn switching_backends_preserves_each_backend_draft() {
        let mut page = LlmConfigPage::new(None, None);
        page.active_draft_mut().model = "custom-router-model".to_string();
        page.select_backend(LlmBackend::Local);
        page.active_draft_mut().model = "custom-local".to_string();
        page.active_draft_mut().api_url = "http://127.0.0.1:1234/v1/chat/completions".to_string();

        page.select_backend(LlmBackend::Cloud);

        assert_eq!(page.active_draft().backend, LlmBackend::Cloud);
        assert_eq!(page.active_draft().model, "custom-router-model");
        assert_eq!(
            page.active_draft().api_url,
            LlmBackend::Cloud.default_api_url()
        );
        assert_eq!(page.active, ConfigField::ApiUrl);
        assert!(page.shows_api_key());

        page.select_backend(LlmBackend::Local);
        assert_eq!(page.active_draft().model, "custom-local");
        assert_eq!(
            page.active_draft().api_url,
            "http://127.0.0.1:1234/v1/chat/completions"
        );
    }

    #[test]
    fn black_and_white_keep_independent_drafts() {
        let mut page = LlmConfigPage::new(None, None);
        page.active_draft_mut().model = "black-router".to_string();
        page.active_draft_mut().api_key = "black-secret".to_string();

        page.select_profile(ProfileSlot::White);
        page.active_draft_mut().api_key = "white-secret".to_string();
        page.select_backend(LlmBackend::Local);
        page.active_draft_mut().model = "white-local".to_string();

        page.select_profile(ProfileSlot::Black);
        assert_eq!(page.active_draft().backend, LlmBackend::Cloud);
        assert_eq!(page.active_draft().model, "black-router");
        assert_eq!(page.active_draft().api_key, "black-secret");

        page.select_profile(ProfileSlot::White);
        assert_eq!(page.active_draft().backend, LlmBackend::Local);
        assert_eq!(page.active_draft().model, "white-local");
        assert_eq!(page.active_draft().api_key, "white-secret");
    }

    #[test]
    fn entering_the_white_tab_enables_a_second_profile() {
        let mut page = LlmConfigPage::new(None, None);
        assert!(!page.white_enabled);

        page.select_profile(ProfileSlot::White);

        assert!(page.white_enabled);
        assert_eq!(page.active_profile, ProfileSlot::White);
    }

    #[test]
    fn pair_configuration_enables_and_focuses_a_missing_white_profile() {
        let mut page = LlmConfigPage::new(None, None);

        page.open(true);

        assert!(page.require_pair);
        assert!(page.white_enabled);
        assert_eq!(page.active_profile, ProfileSlot::White);
    }

    #[test]
    fn normal_configuration_can_still_build_one_profile() {
        let mut page = LlmConfigPage::new(None, None);
        page.select_backend(LlmBackend::Local);

        let settings = page.build_settings().unwrap();

        assert_eq!(settings.profiles().len(), 1);
        assert_eq!(settings.active_profile_index(), 0);
    }

    #[test]
    fn pair_configuration_reports_an_invalid_white_profile_separately() {
        let mut page = LlmConfigPage::new(None, None);
        page.select_backend(LlmBackend::Local);
        page.open(true);

        let error = match page.build_settings() {
            Ok(_) => panic!("pair settings should require a valid white profile"),
            Err(error) => error,
        };

        assert!(error.starts_with("White:"));
        assert!(error.contains("API Key"));
    }

    #[test]
    fn enabling_white_builds_both_profiles_in_one_settings_value() {
        let mut page = LlmConfigPage::new(None, None);
        page.select_backend(LlmBackend::Local);
        page.select_profile(ProfileSlot::White);
        page.select_backend(LlmBackend::Local);

        let settings = page.build_settings().unwrap();

        assert_eq!(settings.profiles().len(), 2);
        assert_eq!(settings.active_profile_index(), 0);
        assert_eq!(settings.profiles()[0].name(), "Black");
        assert_eq!(settings.profiles()[1].name(), "White");
    }

    #[test]
    fn use_one_profile_removes_the_optional_opponent_on_save() {
        let mut page = LlmConfigPage::new(None, None);
        page.select_backend(LlmBackend::Local);
        page.select_profile(ProfileSlot::White);
        page.select_backend(LlmBackend::Local);

        page.use_one_profile();
        let settings = page.build_settings().unwrap();

        assert!(!page.white_enabled);
        assert_eq!(page.active_profile, ProfileSlot::Black);
        assert_eq!(settings.profiles().len(), 1);
        assert_eq!(settings.active_profile_index(), 0);
    }

    #[test]
    fn use_one_profile_keeps_the_current_human_ai_when_it_is_in_the_second_slot() {
        let mut page = LlmConfigPage::new(None, None);
        page.select_backend(LlmBackend::Local);
        page.active_draft_mut().model = "opponent-model".to_string();
        page.select_profile(ProfileSlot::White);
        page.select_backend(LlmBackend::Local);
        page.active_draft_mut().model = "primary-model".to_string();
        page.human_profile_index = 1;

        page.use_one_profile();
        let settings = page.build_settings().unwrap();

        assert_eq!(settings.profiles().len(), 1);
        assert_eq!(settings.active_profile_index(), 0);
        assert_eq!(settings.active_profile().config().model(), "primary-model");
    }

    #[test]
    fn constructor_restores_both_profiles_and_the_active_tab() {
        let black = LlmProfile::new(
            "Black model",
            LlmConfig::new(
                LlmBackend::Local,
                String::new(),
                LlmBackend::Local.default_api_url().to_string(),
                "black-local".to_string(),
            )
            .unwrap(),
        )
        .unwrap();
        let white = LlmProfile::new(
            "White model",
            LlmConfig::new(
                LlmBackend::Local,
                String::new(),
                LlmBackend::Local.default_api_url().to_string(),
                "white-local".to_string(),
            )
            .unwrap(),
        )
        .unwrap();
        let settings = LlmSettings::new(vec![black, white], 1).unwrap();

        let page = LlmConfigPage::new(Some(&settings), None);

        assert!(page.white_enabled);
        assert_eq!(page.active_profile, ProfileSlot::White);
        assert_eq!(page.profiles[0].model, "black-local");
        assert_eq!(page.profiles[1].model, "white-local");
    }

    #[test]
    fn editing_the_other_duel_profile_does_not_change_the_human_ai_profile() {
        let black = LlmProfile::new(
            "Black",
            LlmConfig::new(
                LlmBackend::Local,
                String::new(),
                LlmBackend::Local.default_api_url().to_string(),
                "black-local".to_string(),
            )
            .unwrap(),
        )
        .unwrap();
        let white = LlmProfile::new(
            "White",
            LlmConfig::new(
                LlmBackend::Local,
                String::new(),
                LlmBackend::Local.default_api_url().to_string(),
                "white-local".to_string(),
            )
            .unwrap(),
        )
        .unwrap();
        let original = LlmSettings::new(vec![black, white], 0).unwrap();
        let mut page = LlmConfigPage::new(Some(&original), None);

        page.select_profile(ProfileSlot::White);
        page.active_draft_mut().model = "edited-white".to_string();
        let saved = page.build_settings().unwrap();

        assert_eq!(saved.active_profile_index(), 0);
        assert_eq!(saved.active_profile().config().model(), "black-local");
        assert_eq!(saved.profiles()[1].config().model(), "edited-white");
    }

    #[test]
    fn generated_white_name_does_not_duplicate_an_existing_profile_name() {
        let profile = LlmProfile::new(
            "White",
            LlmConfig::new(
                LlmBackend::Local,
                String::new(),
                LlmBackend::Local.default_api_url().to_string(),
                "black-local".to_string(),
            )
            .unwrap(),
        )
        .unwrap();
        let settings = LlmSettings::new(vec![profile], 0).unwrap();
        let mut page = LlmConfigPage::new(Some(&settings), None);
        page.select_profile(ProfileSlot::White);
        page.select_backend(LlmBackend::Local);

        let settings = page.build_settings().unwrap();

        assert_eq!(settings.profiles()[0].name(), "White");
        assert_eq!(settings.profiles()[1].name(), "White 2");
    }
}
