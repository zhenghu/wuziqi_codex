//! LLM 配置弹窗。配置保存为被 Git 忽略的 JSON 文件。

use crate::llm_ai::{LlmBackend, LlmConfig, config_path};
use macroquad::miniquad::window::clipboard_get;
use macroquad::prelude::*;
#[cfg(target_os = "macos")]
use std::process::Command;

const PANEL_X: f32 = 70.0;
const PANEL_Y: f32 = 105.0;
const PANEL_W: f32 = 500.0;
const PANEL_H: f32 = 490.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfigField {
    ApiKey,
    Model,
    ApiUrl,
}

pub(crate) enum ConfigAction {
    None,
    Cancel,
    Save(LlmConfig),
}

pub(crate) struct LlmConfigPage {
    backend: LlmBackend,
    api_key: String,
    model: String,
    api_url: String,
    openrouter_model: String,
    openrouter_api_url: String,
    local_model: String,
    local_api_url: String,
    active: ConfigField,
    reveal_key: bool,
    message: String,
    load_error: Option<String>,
    suppress_paste_chars: bool,
}

impl LlmConfigPage {
    pub(crate) fn new(saved: Option<&LlmConfig>, load_error: Option<String>) -> Self {
        let backend = saved.map_or(LlmBackend::OpenRouter, LlmConfig::backend);
        let model = saved.map_or_else(
            || backend.default_model().to_string(),
            |config| config.model().to_string(),
        );
        let api_url = saved.map_or_else(
            || backend.default_api_url().to_string(),
            |config| config.api_url().to_string(),
        );
        let mut openrouter_model = LlmBackend::OpenRouter.default_model().to_string();
        let mut openrouter_api_url = LlmBackend::OpenRouter.default_api_url().to_string();
        let mut local_model = LlmBackend::Local.default_model().to_string();
        let mut local_api_url = LlmBackend::Local.default_api_url().to_string();
        match backend {
            LlmBackend::OpenRouter => {
                openrouter_model.clone_from(&model);
                openrouter_api_url.clone_from(&api_url);
            }
            LlmBackend::Local => {
                local_model.clone_from(&model);
                local_api_url.clone_from(&api_url);
            }
        }
        Self {
            backend,
            api_key: saved.map_or_else(String::new, |config| config.api_key().to_string()),
            model,
            api_url,
            openrouter_model,
            openrouter_api_url,
            local_model,
            local_api_url,
            active: Self::first_field(backend),
            reveal_key: false,
            message: String::new(),
            load_error,
            suppress_paste_chars: false,
        }
    }

    pub(crate) fn open(&mut self) {
        self.active = Self::first_field(self.backend);
        self.reveal_key = false;
        self.message = self.load_error.clone().unwrap_or_default();
    }

    fn first_field(backend: LlmBackend) -> ConfigField {
        if backend.requires_api_key() {
            ConfigField::ApiKey
        } else {
            ConfigField::Model
        }
    }

    fn shows_api_key(&self) -> bool {
        self.backend.requires_api_key()
    }

    fn select_backend(&mut self, backend: LlmBackend) {
        if self.backend == backend {
            return;
        }

        match self.backend {
            LlmBackend::OpenRouter => {
                self.openrouter_model.clone_from(&self.model);
                self.openrouter_api_url.clone_from(&self.api_url);
            }
            LlmBackend::Local => {
                self.local_model.clone_from(&self.model);
                self.local_api_url.clone_from(&self.api_url);
            }
        }
        self.backend = backend;
        match backend {
            LlmBackend::OpenRouter => {
                self.model.clone_from(&self.openrouter_model);
                self.api_url.clone_from(&self.openrouter_api_url);
            }
            LlmBackend::Local => {
                self.model.clone_from(&self.local_model);
                self.api_url.clone_from(&self.local_api_url);
            }
        }
        self.active = Self::first_field(backend);
        self.reveal_key = false;
        self.message.clear();
    }

    fn active_value_mut(&mut self) -> &mut String {
        match self.active {
            ConfigField::ApiKey => &mut self.api_key,
            ConfigField::Model => &mut self.model,
            ConfigField::ApiUrl => &mut self.api_url,
        }
    }

    fn next_field(&mut self) {
        self.active = match (self.backend.requires_api_key(), self.active) {
            (true, ConfigField::ApiKey) => ConfigField::Model,
            (true, ConfigField::Model) => ConfigField::ApiUrl,
            (true, ConfigField::ApiUrl) => ConfigField::ApiKey,
            (false, ConfigField::ApiKey | ConfigField::ApiUrl) => ConfigField::Model,
            (false, ConfigField::Model) => ConfigField::ApiUrl,
        };
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
        *self.active_value_mut() = value.to_string();
        self.message.clear();
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
            self.active_value_mut().pop();
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
                    self.active_value_mut().push(character);
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
        let openrouter_backend_rect = Rect::new(350.0, 122.0, 110.0, 34.0);
        let local_backend_rect = Rect::new(468.0, 122.0, 72.0, 34.0);
        if draw_choice_button(
            openrouter_backend_rect,
            "OpenRouter",
            self.backend == LlmBackend::OpenRouter,
        ) {
            self.select_backend(LlmBackend::OpenRouter);
        }
        if draw_choice_button(
            local_backend_rect,
            "Local",
            self.backend == LlmBackend::Local,
        ) {
            self.select_backend(LlmBackend::Local);
        }
        let storage_note = config_path().map_or_else(
            |error| format!("Configuration path unavailable: {error}"),
            |path| format!("Saved to {}", path.display()),
        );
        let storage_note = visible_tail(&storage_note, 64);
        draw_text(
            &storage_note,
            100.0,
            178.0,
            16.0,
            Color::from_rgba(180, 190, 205, 255),
        );

        let key_rect = Rect::new(100.0, 220.0, 295.0, 38.0);
        let paste_rect = Rect::new(405.0, 220.0, 65.0, 38.0);
        let show_rect = Rect::new(480.0, 220.0, 60.0, 38.0);
        let model_rect = Rect::new(100.0, 305.0, 440.0, 38.0);
        let url_rect = Rect::new(100.0, 390.0, 440.0, 38.0);
        draw_text(
            "Model",
            100.0,
            297.0,
            18.0,
            Color::from_rgba(220, 225, 235, 255),
        );
        draw_text(
            "Chat Completions API URL",
            100.0,
            382.0,
            18.0,
            Color::from_rgba(220, 225, 235, 255),
        );

        if self.shows_api_key() {
            draw_text(
                "OpenRouter API Key",
                100.0,
                212.0,
                18.0,
                Color::from_rgba(220, 225, 235, 255),
            );
            let key_display = if self.reveal_key {
                self.api_key.clone()
            } else {
                "*".repeat(self.api_key.chars().count())
            };
            draw_field(key_rect, &key_display, self.active == ConfigField::ApiKey);
        } else {
            draw_text(
                "Local OpenAI-compatible server",
                100.0,
                225.0,
                18.0,
                Color::from_rgba(205, 220, 235, 255),
            );
            draw_text(
                "No key is sent; only 127.0.0.1 or ::1 is allowed.",
                100.0,
                250.0,
                15.0,
                Color::from_rgba(155, 190, 170, 255),
            );
        }
        draw_field(model_rect, &self.model, self.active == ConfigField::Model);
        draw_field(url_rect, &self.api_url, self.active == ConfigField::ApiUrl);

        let (mx, my) = mouse_position();
        let clicked = is_mouse_button_pressed(MouseButton::Left);
        if self.shows_api_key() && clicked && key_rect.contains(vec2(mx, my)) {
            self.active = ConfigField::ApiKey;
        }
        if clicked && model_rect.contains(vec2(mx, my)) {
            self.active = ConfigField::Model;
        }
        if clicked && url_rect.contains(vec2(mx, my)) {
            self.active = ConfigField::ApiUrl;
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

        let help = if self.shows_api_key() {
            "Use Paste or Cmd/Ctrl+V. The key is never shown in logs."
        } else {
            "Ollama preset; also works with LM Studio and llama.cpp."
        };
        draw_text(
            help,
            100.0,
            458.0,
            15.0,
            Color::from_rgba(155, 170, 190, 255),
        );
        if !self.message.is_empty() {
            let message = visible_head(&self.message, 70);
            draw_text(
                &message,
                100.0,
                490.0,
                16.0,
                Color::from_rgba(255, 145, 120, 255),
            );
        }

        self.handle_keyboard();
        let cancel = draw_button(Rect::new(330.0, 525.0, 100.0, 40.0), "Cancel");
        let save = draw_button(Rect::new(440.0, 525.0, 100.0, 40.0), "Save");
        if cancel || is_key_pressed(KeyCode::Escape) {
            return ConfigAction::Cancel;
        }
        if save || is_key_pressed(KeyCode::Enter) {
            match LlmConfig::new(
                self.backend,
                self.api_key.clone(),
                self.api_url.clone(),
                self.model.clone(),
            ) {
                Ok(config) => match config.save() {
                    Ok(()) => {
                        self.load_error = None;
                        return ConfigAction::Save(config);
                    }
                    Err(error) => self.message = error.to_string(),
                },
                Err(error) => self.message = error.to_string(),
            }
        }
        ConfigAction::None
    }
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

fn visible_tail(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
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
    format!(
        "{}...",
        value
            .chars()
            .take(max_chars.saturating_sub(3))
            .collect::<String>()
    )
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
    let shown = visible_tail(value, 48);
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
    fn pasting_an_api_key_replaces_the_previous_value() {
        let mut page = LlmConfigPage::new(None, None);
        page.active = ConfigField::ApiKey;
        page.api_key = "old-key".to_string();

        page.apply_pasted_value("  new-key\n");

        assert_eq!(page.api_key, "new-key");
        assert!(page.message.is_empty());
    }

    #[test]
    fn opening_configuration_surfaces_a_previous_load_error() {
        let mut page = LlmConfigPage::new(None, Some("Invalid configuration".to_string()));

        page.open();

        assert_eq!(page.message, "Invalid configuration");
    }

    #[test]
    fn selecting_local_applies_ollama_defaults_and_skips_api_key() {
        let mut page = LlmConfigPage::new(None, None);
        page.api_key = "keep-for-openrouter".to_string();

        page.select_backend(LlmBackend::Local);

        assert_eq!(page.backend, LlmBackend::Local);
        assert_eq!(page.model, LlmBackend::Local.default_model());
        assert_eq!(page.api_url, LlmBackend::Local.default_api_url());
        assert_eq!(page.api_key, "keep-for-openrouter");
        assert_eq!(page.active, ConfigField::Model);
        assert!(!page.shows_api_key());

        page.next_field();
        assert_eq!(page.active, ConfigField::ApiUrl);
        page.next_field();
        assert_eq!(page.active, ConfigField::Model);
    }

    #[test]
    fn switching_backends_preserves_each_backend_draft() {
        let mut page = LlmConfigPage::new(None, None);
        page.model = "custom-router-model".to_string();
        page.select_backend(LlmBackend::Local);
        page.model = "custom-local".to_string();
        page.api_url = "http://127.0.0.1:1234/v1/chat/completions".to_string();

        page.select_backend(LlmBackend::OpenRouter);

        assert_eq!(page.backend, LlmBackend::OpenRouter);
        assert_eq!(page.model, "custom-router-model");
        assert_eq!(page.api_url, LlmBackend::OpenRouter.default_api_url());
        assert_eq!(page.active, ConfigField::ApiKey);
        assert!(page.shows_api_key());

        page.select_backend(LlmBackend::Local);
        assert_eq!(page.model, "custom-local");
        assert_eq!(page.api_url, "http://127.0.0.1:1234/v1/chat/completions");
    }
}
