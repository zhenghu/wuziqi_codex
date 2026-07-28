//! 应用主循环：协调棋局、界面、传统 AI 与大模型请求。

use crate::ai::{ai_move, llm_candidate_moves};
use crate::board_view::{self, Button, TOP_BAR, WIN_H, WIN_W};
use crate::config_ui::{ConfigAction, LlmConfigPage};
use crate::game::{Cell, Game, Mode, Status};
use crate::llm_ai::{
    LlmBackend, LlmConfig, LlmMove, build_local_client, build_openrouter_client, config_exists,
    request_move,
};
use macroquad::prelude::*;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use tokio::sync::mpsc as tokio_mpsc;

const MAX_LLM_ATTEMPTS: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AiAlgorithm {
    TacticalSearch,
    LargeModel,
}

struct PendingLlmRequest {
    result: Receiver<Result<LlmMove, String>>,
}

enum LlmCommand {
    Request {
        config: LlmConfig,
        board: Box<[[Cell; crate::game::BOARD]; crate::game::BOARD]>,
        candidates: Vec<(usize, usize)>,
        result: mpsc::Sender<Result<LlmMove, String>>,
    },
    Cancel,
}

struct LlmWorker {
    commands: tokio_mpsc::UnboundedSender<LlmCommand>,
}

impl LlmWorker {
    fn new() -> Result<Self, String> {
        let cloud_client = build_openrouter_client()?;
        let local_client = build_local_client()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("Cannot start LLM runtime: {error}"))?;
        let (commands, mut receiver) = tokio_mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("wuziqi-llm".to_string())
            .spawn(move || {
                runtime.block_on(async move {
                    let mut active: Option<tokio::task::JoinHandle<()>> = None;
                    loop {
                        let received = receiver.recv().await;
                        let Some(command) = received else {
                            break;
                        };
                        if let Some(request) = active.take() {
                            request.abort();
                        }
                        match command {
                            LlmCommand::Request {
                                config,
                                board,
                                candidates,
                                result,
                            } => {
                                let client = match config.backend() {
                                    LlmBackend::OpenRouter => cloud_client.clone(),
                                    LlmBackend::Local => local_client.clone(),
                                };
                                active = Some(tokio::spawn(async move {
                                    let response =
                                        request_move(&client, &config, &board, &candidates).await;
                                    let _ = result.send(response);
                                }));
                            }
                            LlmCommand::Cancel => {}
                        }
                    }
                    if let Some(request) = active {
                        request.abort();
                    }
                });
            })
            .map_err(|error| format!("Cannot start LLM worker: {error}"))?;
        Ok(Self { commands })
    }

    fn request(
        &self,
        config: LlmConfig,
        board: [[Cell; crate::game::BOARD]; crate::game::BOARD],
        candidates: Vec<(usize, usize)>,
    ) -> Result<Receiver<Result<LlmMove, String>>, String> {
        let (result, receiver) = mpsc::channel();
        self.commands
            .send(LlmCommand::Request {
                config,
                board: Box::new(board),
                candidates,
                result,
            })
            .map_err(|_| "LLM worker has stopped".to_string())?;
        Ok(receiver)
    }

    fn cancel(&self) {
        let _ = self.commands.send(LlmCommand::Cancel);
    }
}

pub(crate) struct App {
    game: Game,
    ai_algorithm: AiAlgorithm,
    ai_thinking: bool,
    pending_llm: Option<PendingLlmRequest>,
    llm_worker: Option<LlmWorker>,
    ai_notice: String,
    config_page: LlmConfigPage,
    active_llm_config: Option<LlmConfig>,
    llm_status: String,
    show_config: bool,
    llm_attempt: u8,
}

impl App {
    pub(crate) fn new() -> Self {
        let had_config = config_exists();
        let (active_llm_config, config_load_error) = match LlmConfig::load() {
            Ok(config) => (Some(config), None),
            Err(error) => {
                if had_config {
                    eprintln!("大模型配置加载失败: {error}");
                    (None, Some(format!("Configuration load failed: {error}")))
                } else {
                    (None, None)
                }
            }
        };
        let ai_algorithm = if active_llm_config.is_some() {
            AiAlgorithm::LargeModel
        } else {
            AiAlgorithm::TacticalSearch
        };
        let llm_status = active_llm_config
            .as_ref()
            .map(|config| format!("{}: not connected", config.backend().label()))
            .unwrap_or_else(|| "LLM: not configured".to_string());
        let llm_worker = match LlmWorker::new() {
            Ok(worker) => Some(worker),
            Err(error) => {
                eprintln!("LLM worker unavailable: {error}");
                None
            }
        };
        Self {
            game: Game::new(Mode::HumanVsAi),
            ai_algorithm,
            ai_thinking: false,
            pending_llm: None,
            llm_worker,
            ai_notice: String::new(),
            config_page: LlmConfigPage::new(active_llm_config.as_ref(), config_load_error),
            active_llm_config,
            llm_status,
            show_config: false,
            llm_attempt: 0,
        }
    }

    pub(crate) async fn run(&mut self) {
        loop {
            self.draw_and_update();
            next_frame().await;
        }
    }

    fn draw_and_update(&mut self) {
        clear_background(Color::from_rgba(40, 44, 52, 255));
        self.draw_header();

        let human_turn = self.game.status == Status::Playing
            && !(self.game.mode == Mode::HumanVsAi && self.game.turn == Cell::White);
        board_view::draw(&self.game, human_turn);
        self.draw_ai_info();

        if self.show_config {
            self.update_config_page();
            return;
        }

        let action = self.read_action();
        if self.handle_action(action) {
            return;
        }
        if self.game.status == Status::Playing {
            if self.should_ai_move() {
                self.update_ai();
            } else if human_turn && is_mouse_button_pressed(MouseButton::Left) {
                self.place_from_mouse();
            }
        }
    }

    fn draw_header(&self) {
        draw_rectangle(0.0, 0.0, WIN_W, TOP_BAR, Color::from_rgba(30, 33, 40, 255));
        let text = match self.game.status {
            Status::Playing if self.ai_thinking => "AI is thinking...".to_string(),
            Status::Playing => match (self.game.mode, self.game.turn) {
                (Mode::HumanVsAi, Cell::Black) if !self.ai_notice.is_empty() => {
                    format!("Your turn (Black) - {}", self.ai_notice)
                }
                (Mode::HumanVsAi, Cell::Black) => "Your turn (Black)".to_string(),
                (Mode::HumanVsAi, _) => "AI's turn (White)".to_string(),
                (_, Cell::Black) => "Black's turn".to_string(),
                (_, _) => "White's turn".to_string(),
            },
            Status::Won(Cell::Black) if self.game.mode == Mode::HumanVsAi => {
                "You win! Press R".to_string()
            }
            Status::Won(Cell::Black) => "Black wins! Press R".to_string(),
            Status::Won(_) if self.game.mode == Mode::HumanVsAi => "AI wins! Press R".to_string(),
            Status::Won(_) => "White wins! Press R".to_string(),
            Status::Draw => "Draw! Press R".to_string(),
        };
        draw_text(
            &text,
            14.0,
            TOP_BAR - 12.0,
            24.0,
            Color::from_rgba(255, 210, 120, 255),
        );

        if self.game.mode == Mode::HumanVsAi {
            let badge = format!("AI: {}", self.ai_model_label());
            let size = measure_text(&badge, None, 16, 1.0);
            let x = WIN_W - size.width - 18.0;
            draw_rectangle(
                x - 6.0,
                TOP_BAR - 28.0,
                size.width + 12.0,
                23.0,
                Color::from_rgba(45, 75, 110, 245),
            );
            draw_text(
                &badge,
                x,
                TOP_BAR - 11.0,
                16.0,
                Color::from_rgba(215, 230, 250, 255),
            );
        }
    }

    fn draw_ai_info(&self) {
        let color = Color::from_rgba(190, 205, 225, 255);
        if self.game.mode == Mode::HumanVsAi {
            let text = match self.ai_algorithm {
                AiAlgorithm::TacticalSearch => "AI engine: Tactical Search".to_string(),
                AiAlgorithm::LargeModel => format!("AI route: {}", self.ai_model_label()),
            };
            draw_text(&text, 22.0, WIN_H - 5.0, 16.0, color);
        }

        let version = format!("v{}", env!("CARGO_PKG_VERSION"));
        let size = measure_text(&version, None, 16, 1.0);
        draw_text(
            &version,
            WIN_W - size.width - 22.0,
            WIN_H - 5.0,
            16.0,
            color,
        );
    }

    fn ai_model_label(&self) -> String {
        match self.ai_algorithm {
            AiAlgorithm::TacticalSearch => "Tactical Search".to_string(),
            AiAlgorithm::LargeModel => compact_text(&self.llm_status, 38),
        }
    }

    fn read_action(&self) -> Action {
        let mode = Button::new(
            12.0,
            12.0,
            150.0,
            30.0,
            match self.game.mode {
                Mode::HumanVsAi => "Mode: You vs AI",
                Mode::HumanVsHuman => "Mode: 2 Players",
            },
        );
        let undo = Button::new(174.0, 12.0, 90.0, 30.0, "Undo (U)");
        let restart = Button::new(276.0, 12.0, 110.0, 30.0, "Restart (R)");
        let algorithm = Button::new(
            398.0,
            12.0,
            130.0,
            30.0,
            match self.ai_algorithm {
                AiAlgorithm::TacticalSearch => "AI: Tactical",
                AiAlgorithm::LargeModel => "AI: LLM",
            },
        );
        let config = Button::new(540.0, 12.0, 83.0, 30.0, "Config (C)");

        let mode_clicked = mode.draw();
        let undo_clicked = undo.draw();
        let restart_clicked = restart.draw();
        let algorithm_clicked = algorithm.draw();
        let config_clicked = config.draw();

        if is_key_pressed(KeyCode::C) || config_clicked {
            Action::OpenConfig
        } else if is_key_pressed(KeyCode::A) || algorithm_clicked {
            Action::ToggleAlgorithm
        } else if is_key_pressed(KeyCode::M) || mode_clicked {
            Action::ToggleMode
        } else if is_key_pressed(KeyCode::R) || restart_clicked {
            Action::Restart
        } else if is_key_pressed(KeyCode::U) || undo_clicked {
            Action::Undo
        } else {
            Action::None
        }
    }

    fn handle_action(&mut self, action: Action) -> bool {
        match action {
            Action::None => false,
            Action::OpenConfig => {
                self.config_page.open();
                self.show_config = true;
                true
            }
            Action::ToggleAlgorithm => {
                let next = match self.ai_algorithm {
                    AiAlgorithm::TacticalSearch => AiAlgorithm::LargeModel,
                    AiAlgorithm::LargeModel => AiAlgorithm::TacticalSearch,
                };
                if next == AiAlgorithm::LargeModel && self.active_llm_config.is_none() {
                    self.config_page.open();
                    self.show_config = true;
                } else {
                    self.ai_algorithm = next;
                }
                self.cancel_ai();
                true
            }
            Action::ToggleMode => {
                let mode = match self.game.mode {
                    Mode::HumanVsAi => Mode::HumanVsHuman,
                    Mode::HumanVsHuman => Mode::HumanVsAi,
                };
                self.game = Game::new(mode);
                self.cancel_ai();
                true
            }
            Action::Restart => {
                self.game = Game::new(self.game.mode);
                self.cancel_ai();
                true
            }
            Action::Undo => {
                self.game.undo();
                self.cancel_ai();
                true
            }
        }
    }

    fn update_config_page(&mut self) {
        match self.config_page.draw_and_update() {
            ConfigAction::None => {}
            ConfigAction::Cancel => self.show_config = false,
            ConfigAction::Save(config) => {
                let backend = config.backend().label();
                self.llm_status = format!("{backend}: not connected");
                self.active_llm_config = Some(config);
                self.ai_algorithm = AiAlgorithm::LargeModel;
                self.cancel_ai();
                self.ai_notice = format!("{backend} config saved");
                self.show_config = false;
            }
        }
    }

    fn should_ai_move(&self) -> bool {
        self.game.mode == Mode::HumanVsAi && self.game.turn == Cell::White
    }

    fn update_ai(&mut self) {
        match self.ai_algorithm {
            AiAlgorithm::TacticalSearch => {
                if self.ai_thinking {
                    self.place_tactical_move();
                    self.ai_thinking = false;
                } else {
                    self.ai_thinking = true;
                    self.ai_notice.clear();
                }
            }
            AiAlgorithm::LargeModel if !self.ai_thinking => {
                self.llm_attempt = 0;
                self.start_llm_request(None);
            }
            AiAlgorithm::LargeModel => self.poll_llm_request(),
        }
    }

    fn start_llm_request(&mut self, previous_error: Option<&str>) {
        self.llm_attempt += 1;
        self.ai_thinking = true;
        self.ai_notice.clear();
        let board = self.game.board;
        let candidates = llm_candidate_moves(&board, Cell::White, self.game.history.len());
        let Some(config) = self.active_llm_config.clone() else {
            eprintln!("大模型未配置，使用战术搜索");
            self.fallback_to_tactical("LLM not configured; used fallback");
            return;
        };
        let model = config.model().to_string();
        let backend = config.backend().label();
        self.llm_status = match previous_error {
            Some(error) => {
                let reason = llm_failure_summary(error, 28);
                format!("Retry {}/{}: {reason}", self.llm_attempt, MAX_LLM_ATTEMPTS)
            }
            None => format!("{backend}: connecting..."),
        };
        let Some(worker) = &self.llm_worker else {
            self.fallback_to_tactical("LLM worker unavailable; used fallback");
            return;
        };
        let result = match worker.request(config, board, candidates) {
            Ok(result) => result,
            Err(error) => {
                eprintln!("无法开始大模型请求: {error}");
                self.fallback_to_tactical("LLM could not start; used fallback");
                return;
            }
        };
        self.pending_llm = Some(PendingLlmRequest { result });
        eprintln!("正在请求大模型 {model} 选择落点……");
    }

    fn poll_llm_request(&mut self) {
        let Some(pending) = &self.pending_llm else {
            return;
        };
        match pending.result.try_recv() {
            Ok(Ok(llm_move)) => {
                self.llm_status = llm_move.route_label();
                self.game.place(llm_move.position.0, llm_move.position.1);
                self.ai_thinking = false;
                self.pending_llm = None;
                self.llm_attempt = 0;
                self.ai_notice = "LLM move".to_string();
            }
            Ok(Err(error)) => {
                self.pending_llm = None;
                if should_retry_llm(self.llm_attempt) {
                    eprintln!(
                        "大模型落子失败，准备第 {} 次尝试: {error}",
                        self.llm_attempt + 1
                    );
                    self.start_llm_request(Some(&error));
                } else {
                    let reason = llm_failure_summary(&error, 30);
                    self.llm_status = format!("Failed: {reason}");
                    eprintln!("大模型连续 {MAX_LLM_ATTEMPTS} 次失败，使用战术搜索: {error}");
                    self.fallback_to_tactical(&format!("LLM failed: {reason}; fallback"));
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                let backend = self
                    .active_llm_config
                    .as_ref()
                    .map(|config| config.backend().label())
                    .unwrap_or("LLM");
                self.llm_status = format!("{backend}: call stopped");
                self.fallback_to_tactical("LLM stopped; used fallback");
            }
        }
    }

    fn fallback_to_tactical(&mut self, notice: &str) {
        self.place_tactical_move();
        self.ai_thinking = false;
        self.pending_llm = None;
        self.llm_attempt = 0;
        self.ai_notice = notice.to_string();
    }

    fn place_tactical_move(&mut self) {
        let (x, y) = ai_move(&self.game.board, Cell::White, self.game.history.len());
        self.game.place(x, y);
    }

    fn cancel_ai(&mut self) {
        self.ai_thinking = false;
        if let Some(worker) = &self.llm_worker {
            worker.cancel();
        }
        self.pending_llm = None;
        self.llm_attempt = 0;
        self.ai_notice.clear();
    }

    fn place_from_mouse(&mut self) {
        let (mx, my) = mouse_position();
        if my > TOP_BAR {
            if let Some((x, y)) = board_view::pixel_to_cell(mx, my) {
                self.game.place(x, y);
            }
        }
    }
}

enum Action {
    None,
    Restart,
    Undo,
    ToggleMode,
    ToggleAlgorithm,
    OpenConfig,
}

pub(crate) fn compact_text(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    format!("{}...", value.chars().take(keep).collect::<String>())
}

pub(crate) fn should_retry_llm(attempt: u8) -> bool {
    attempt < MAX_LLM_ATTEMPTS
}

pub(crate) fn llm_failure_summary(error: &str, max_chars: usize) -> String {
    let single_line = error.split_whitespace().collect::<Vec<_>>().join(" ");
    compact_text(&single_line, max_chars)
}

#[cfg(test)]
mod worker_tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::time::Duration;

    #[test]
    fn cancelling_the_worker_drops_the_in_flight_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            std::thread::sleep(Duration::from_secs(2));
        });
        let config = LlmConfig::new_unchecked(
            LlmBackend::Local,
            "key".into(),
            format!("http://{address}/v1/chat/completions"),
            "model".into(),
        );
        let worker = LlmWorker::new().unwrap();
        let result = worker
            .request(
                config,
                [[Cell::Empty; crate::game::BOARD]; crate::game::BOARD],
                vec![(7, 7)],
            )
            .unwrap();

        worker.cancel();

        assert!(matches!(
            result.recv_timeout(Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }
}
