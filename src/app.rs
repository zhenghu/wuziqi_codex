//! 应用主循环：协调棋局、界面、传统 AI 与大模型请求。

use crate::ai::{ai_move, llm_candidate_moves};
use crate::board_view::{self, Button, TOP_BAR, WIN_H, WIN_W};
use crate::config_ui::{ConfigAction, LlmConfigPage};
use crate::game::{Cell, Game, Mode, Status, opponent};
use crate::llm_ai::{
    LlmBackend, LlmConfig, LlmMove, LlmSettings, build_cloud_client, build_local_client,
    config_exists, request_move,
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
    turn: LlmTurnRequest,
    request_id: u64,
    result: Receiver<Result<LlmMove, String>>,
}

#[derive(Clone)]
struct LlmTurnRequest {
    match_id: u64,
    expected_ply: usize,
    side: Cell,
    profile_index: usize,
    config: LlmConfig,
    board: [[Cell; crate::game::BOARD]; crate::game::BOARD],
    candidates: Vec<(usize, usize)>,
    /// 重试时累积的已被模型选过但不可用的位置，用于提示模型避开。
    excluded: Vec<(usize, usize)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ArenaStop {
    TechnicalLoss { loser: Cell, reason: String },
    Aborted(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ArenaState {
    Ready,
    Running,
    Paused,
    Finished,
    Stopped(ArenaStop),
}

enum LlmCommand {
    Request {
        config: LlmConfig,
        side: Cell,
        board: Box<[[Cell; crate::game::BOARD]; crate::game::BOARD]>,
        candidates: Vec<(usize, usize)>,
        excluded: Vec<(usize, usize)>,
        result: mpsc::Sender<Result<LlmMove, String>>,
    },
    Cancel,
}

struct LlmWorker {
    commands: tokio_mpsc::UnboundedSender<LlmCommand>,
}

trait LlmRequestWorker {
    fn request(
        &self,
        config: LlmConfig,
        side: Cell,
        board: [[Cell; crate::game::BOARD]; crate::game::BOARD],
        candidates: Vec<(usize, usize)>,
        excluded: Vec<(usize, usize)>,
    ) -> Result<Receiver<Result<LlmMove, String>>, String>;

    fn cancel(&self);
}

impl LlmWorker {
    fn new() -> Result<Self, String> {
        let cloud_client = build_cloud_client()?;
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
                                side,
                                board,
                                candidates,
                                excluded,
                                result,
                            } => {
                                let client = match config.backend() {
                                    LlmBackend::OpenRouter => cloud_client.clone(),
                                    LlmBackend::Local => local_client.clone(),
                                };
                                active = Some(tokio::spawn(async move {
                                    let response = request_move(
                                        &client,
                                        &config,
                                        &board,
                                        side,
                                        &candidates,
                                        &excluded,
                                    )
                                    .await;
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
        side: Cell,
        board: [[Cell; crate::game::BOARD]; crate::game::BOARD],
        candidates: Vec<(usize, usize)>,
        excluded: Vec<(usize, usize)>,
    ) -> Result<Receiver<Result<LlmMove, String>>, String> {
        let (result, receiver) = mpsc::channel();
        self.commands
            .send(LlmCommand::Request {
                config,
                side,
                board: Box::new(board),
                candidates,
                excluded,
                result,
            })
            .map_err(|_| "LLM worker has stopped".to_string())?;
        Ok(receiver)
    }

    fn cancel(&self) {
        let _ = self.commands.send(LlmCommand::Cancel);
    }
}

impl LlmRequestWorker for LlmWorker {
    fn request(
        &self,
        config: LlmConfig,
        side: Cell,
        board: [[Cell; crate::game::BOARD]; crate::game::BOARD],
        candidates: Vec<(usize, usize)>,
        excluded: Vec<(usize, usize)>,
    ) -> Result<Receiver<Result<LlmMove, String>>, String> {
        LlmWorker::request(self, config, side, board, candidates, excluded)
    }

    fn cancel(&self) {
        LlmWorker::cancel(self);
    }
}

pub(crate) struct App {
    game: Game,
    ai_algorithm: AiAlgorithm,
    ai_thinking: bool,
    pending_llm: Option<PendingLlmRequest>,
    llm_worker: Option<Box<dyn LlmRequestWorker>>,
    ai_notice: String,
    config_page: LlmConfigPage,
    llm_settings: Option<LlmSettings>,
    llm_route: [String; 2],
    llm_status: [String; 2],
    show_config: bool,
    llm_attempt: u8,
    arena_state: ArenaState,
    match_id: u64,
    next_request_id: u64,
}

impl App {
    pub(crate) fn new() -> Self {
        let had_config = config_exists();
        let (llm_settings, config_load_error) = match LlmSettings::load() {
            Ok(settings) => (Some(settings), None),
            Err(error) => {
                if had_config {
                    eprintln!("大模型配置加载失败: {error}");
                    (None, Some(format!("Configuration load failed: {error}")))
                } else {
                    (None, None)
                }
            }
        };
        let ai_algorithm = if llm_settings.is_some() {
            AiAlgorithm::LargeModel
        } else {
            AiAlgorithm::TacticalSearch
        };
        let llm_route = profile_routes(llm_settings.as_ref());
        let llm_status = profile_statuses(llm_settings.as_ref());
        let llm_worker: Option<Box<dyn LlmRequestWorker>> = match LlmWorker::new() {
            Ok(worker) => Some(Box::new(worker)),
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
            config_page: LlmConfigPage::new(llm_settings.as_ref(), config_load_error),
            llm_settings,
            llm_route,
            llm_status,
            show_config: false,
            llm_attempt: 0,
            arena_state: ArenaState::Ready,
            match_id: 0,
            next_request_id: 0,
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
            && match self.game.mode {
                Mode::HumanVsAi => self.game.turn == Cell::Black,
                Mode::HumanVsHuman => true,
                Mode::LlmVsLlm => false,
            };
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
        let text = if self.game.mode == Mode::LlmVsLlm {
            self.arena_header()
        } else {
            match self.game.status {
                Status::Playing if self.ai_thinking => "AI is thinking...".to_string(),
                Status::Playing => match (self.game.mode, self.game.turn) {
                    (Mode::HumanVsAi, Cell::Black) if !self.ai_notice.is_empty() => {
                        format!("Your turn (Black) - {}", self.ai_notice)
                    }
                    (Mode::HumanVsAi, Cell::Black) => "Your turn (Black)".to_string(),
                    (Mode::HumanVsAi, _) => "AI's turn (White)".to_string(),
                    (Mode::HumanVsHuman, Cell::Black) => "Black's turn".to_string(),
                    (_, _) => "White's turn".to_string(),
                },
                Status::Won(Cell::Black) if self.game.mode == Mode::HumanVsAi => {
                    "You win! Press R".to_string()
                }
                Status::Won(Cell::Black) => "Black wins! Press R".to_string(),
                Status::Won(_) if self.game.mode == Mode::HumanVsAi => {
                    "AI wins! Press R".to_string()
                }
                Status::Won(_) => "White wins! Press R".to_string(),
                Status::Draw => "Draw! Press R".to_string(),
            }
        };
        let text = compact_text_to_width(&text, WIN_W - 28.0, 20);
        draw_text(
            text,
            14.0,
            TOP_BAR - 6.0,
            20.0,
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
        let text = match self.game.mode {
            Mode::HumanVsAi => match self.ai_algorithm {
                AiAlgorithm::TacticalSearch => "AI engine: Tactical Search".to_string(),
                AiAlgorithm::LargeModel => format!("AI route: {}", self.ai_model_label()),
            },
            Mode::HumanVsHuman => String::new(),
            Mode::LlmVsLlm => format!(
                "Duel · B: {} · W: {}",
                self.arena_model_label(Cell::Black),
                self.arena_model_label(Cell::White)
            ),
        };
        let version = format!("v{}", env!("CARGO_PKG_VERSION"));
        let size = measure_text(&version, None, 16, 1.0);
        if !text.is_empty() {
            let available_width = WIN_W - size.width - 58.0;
            draw_text(
                compact_text_to_width(&text, available_width, 16),
                22.0,
                WIN_H - 5.0,
                16.0,
                color,
            );
        }
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
            AiAlgorithm::LargeModel => self
                .active_profile_index()
                .map(|index| compact_text(&self.llm_route[index], 38))
                .unwrap_or_else(|| "LLM: not configured".to_string()),
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
                Mode::LlmVsLlm => "Mode: LLM Duel",
            },
        );
        let control_label = if self.game.mode == Mode::LlmVsLlm {
            match self.arena_state {
                ArenaState::Ready => "Start",
                ArenaState::Running => "Pause",
                ArenaState::Paused => "Resume",
                ArenaState::Finished => "Rematch",
                ArenaState::Stopped(ArenaStop::TechnicalLoss { .. }) => "Rematch",
                ArenaState::Stopped(ArenaStop::Aborted(_)) => "Retry",
            }
        } else {
            "Undo (U)"
        };
        let undo = Button::new(174.0, 12.0, 90.0, 30.0, control_label);
        let restart = Button::new(276.0, 12.0, 110.0, 30.0, "Restart (R)");
        let algorithm = Button::new(
            398.0,
            12.0,
            130.0,
            30.0,
            match (self.game.mode, self.ai_algorithm) {
                (Mode::LlmVsLlm, _) => "Swap + Reset",
                (_, AiAlgorithm::TacticalSearch) => "AI: Tactical",
                (_, AiAlgorithm::LargeModel) => "AI: LLM",
            },
        );
        let config = Button::new(
            540.0,
            12.0,
            83.0,
            30.0,
            if self.game.mode == Mode::LlmVsLlm {
                "Setup (C)"
            } else {
                "Config (C)"
            },
        );

        let mode_clicked = mode.draw();
        let undo_clicked = undo.draw();
        let restart_clicked = restart.draw();
        let algorithm_clicked = algorithm.draw();
        let config_clicked = config.draw();

        if is_key_pressed(KeyCode::C) || config_clicked {
            Action::OpenConfig
        } else if (self.game.mode == Mode::LlmVsLlm && is_key_pressed(KeyCode::S))
            || (self.game.mode != Mode::LlmVsLlm && is_key_pressed(KeyCode::A))
            || algorithm_clicked
        {
            Action::ToggleAlgorithm
        } else if is_key_pressed(KeyCode::M) || mode_clicked {
            Action::ToggleMode
        } else if is_key_pressed(KeyCode::R) || restart_clicked {
            Action::Restart
        } else if (self.game.mode != Mode::LlmVsLlm && is_key_pressed(KeyCode::U))
            || (self.game.mode == Mode::LlmVsLlm && is_key_pressed(KeyCode::Space))
            || undo_clicked
        {
            Action::Undo
        } else {
            Action::None
        }
    }

    fn handle_action(&mut self, action: Action) -> bool {
        match action {
            Action::None => false,
            Action::OpenConfig => {
                if self.game.mode == Mode::LlmVsLlm
                    && matches!(self.arena_state, ArenaState::Running)
                {
                    self.arena_state = ArenaState::Paused;
                }
                self.cancel_ai();
                self.config_page.open(self.game.mode == Mode::LlmVsLlm);
                self.show_config = true;
                true
            }
            Action::ToggleAlgorithm => {
                if self.game.mode == Mode::LlmVsLlm {
                    self.swap_arena_players();
                    return true;
                }
                let next = match self.ai_algorithm {
                    AiAlgorithm::TacticalSearch => AiAlgorithm::LargeModel,
                    AiAlgorithm::LargeModel => AiAlgorithm::TacticalSearch,
                };
                if next == AiAlgorithm::LargeModel && self.active_config().is_none() {
                    self.config_page.open(false);
                    self.show_config = true;
                } else {
                    self.ai_algorithm = next;
                }
                self.cancel_ai();
                true
            }
            Action::ToggleMode => {
                let mode = self.game.mode.next();
                self.game = Game::new(mode);
                self.cancel_ai();
                self.arena_state = ArenaState::Ready;
                if mode == Mode::LlmVsLlm && !self.has_arena_pair() {
                    self.config_page.open(true);
                    self.show_config = true;
                }
                true
            }
            Action::Restart => {
                if self.game.mode == Mode::LlmVsLlm {
                    self.reset_arena();
                } else {
                    self.game = Game::new(self.game.mode);
                    self.cancel_ai();
                }
                true
            }
            Action::Undo => {
                if self.game.mode == Mode::LlmVsLlm {
                    self.toggle_arena();
                } else {
                    self.game.undo();
                    self.cancel_ai();
                }
                true
            }
        }
    }

    fn update_config_page(&mut self) {
        match self.config_page.draw_and_update() {
            ConfigAction::None => {}
            ConfigAction::Cancel => {
                self.show_config = false;
                self.config_page = LlmConfigPage::new(self.llm_settings.as_ref(), None);
            }
            ConfigAction::Save(settings) => {
                self.llm_route = profile_routes(Some(&settings));
                self.llm_status = profile_statuses(Some(&settings));
                let backend = settings.active_profile().config().backend().label();
                self.llm_settings = Some(settings);
                self.config_page = LlmConfigPage::new(self.llm_settings.as_ref(), None);
                self.ai_algorithm = AiAlgorithm::LargeModel;
                self.cancel_ai();
                self.ai_notice = format!("{backend} config saved");
                self.show_config = false;
                if self.game.mode == Mode::LlmVsLlm {
                    self.game = Game::new(Mode::LlmVsLlm);
                    self.arena_state = ArenaState::Ready;
                }
            }
        }
    }

    fn should_ai_move(&self) -> bool {
        match self.game.mode {
            Mode::HumanVsAi => self.game.turn == Cell::White,
            Mode::HumanVsHuman => false,
            Mode::LlmVsLlm => matches!(self.arena_state, ArenaState::Running),
        }
    }

    fn update_ai(&mut self) {
        let algorithm = if self.game.mode == Mode::LlmVsLlm {
            AiAlgorithm::LargeModel
        } else {
            self.ai_algorithm
        };
        match algorithm {
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
                self.start_llm_turn();
            }
            AiAlgorithm::LargeModel => self.poll_llm_request(),
        }
    }

    fn start_llm_turn(&mut self) {
        let side = self.game.turn;
        let board = self.game.board;
        let candidates = llm_candidate_moves(&board, side, self.game.history.len());
        if candidates.is_empty() {
            if self.game.mode == Mode::LlmVsLlm {
                self.stop_arena(ArenaStop::Aborted(
                    "candidate engine returned no legal moves".to_string(),
                ));
            } else {
                self.fallback_to_tactical("No LLM candidates; used fallback");
            }
            return;
        }
        let Some((profile_index, config)) = self.config_for_side(side) else {
            if self.game.mode == Mode::LlmVsLlm {
                self.stop_arena(ArenaStop::Aborted(format!(
                    "{} player is not configured",
                    side_label(side)
                )));
            } else {
                eprintln!("大模型未配置，使用战术搜索");
                self.fallback_to_tactical("LLM not configured; used fallback");
            }
            return;
        };
        let turn = LlmTurnRequest {
            match_id: self.match_id,
            expected_ply: self.game.history.len(),
            side,
            profile_index,
            config,
            board,
            candidates,
            excluded: Vec::new(),
        };
        self.dispatch_llm_request(turn, None);
    }

    fn dispatch_llm_request(&mut self, turn: LlmTurnRequest, previous_error: Option<&str>) {
        self.llm_attempt += 1;
        self.ai_thinking = true;
        self.ai_notice.clear();
        let model = turn.config.model().to_string();
        let backend = turn.config.backend().label();
        self.llm_status[turn.profile_index] = match previous_error {
            Some(error) => {
                let reason = llm_failure_summary(error, 28);
                format!("Retry {}/{}: {reason}", self.llm_attempt, MAX_LLM_ATTEMPTS)
            }
            None => format!("{backend}: connecting..."),
        };
        let Some(worker) = &self.llm_worker else {
            if self.game.mode == Mode::LlmVsLlm {
                self.stop_arena(ArenaStop::Aborted("LLM worker unavailable".to_string()));
            } else {
                self.fallback_to_tactical("LLM worker unavailable; used fallback");
            }
            return;
        };
        let result = match worker.request(
            turn.config.clone(),
            turn.side,
            turn.board,
            turn.candidates.clone(),
            turn.excluded.clone(),
        ) {
            Ok(result) => result,
            Err(error) => {
                eprintln!("无法开始大模型请求: {error}");
                if self.game.mode == Mode::LlmVsLlm {
                    self.stop_arena(ArenaStop::Aborted(error));
                } else {
                    self.fallback_to_tactical("LLM could not start; used fallback");
                }
                return;
            }
        };
        self.next_request_id = self.next_request_id.wrapping_add(1);
        self.pending_llm = Some(PendingLlmRequest {
            turn,
            request_id: self.next_request_id,
            result,
        });
        eprintln!(
            "正在请求大模型 {model} 为{}选择落点……",
            side_label(self.game.turn)
        );
    }

    fn poll_llm_request(&mut self) {
        let Some(pending) = &self.pending_llm else {
            return;
        };
        let received = pending.result.try_recv();
        match received {
            Ok(Ok(llm_move)) => {
                let pending = self
                    .pending_llm
                    .take()
                    .expect("received result must have a pending request");
                if !self.pending_is_current(&pending) {
                    self.ignore_stale_llm_result();
                    return;
                }
                if !self.game.place(llm_move.position.0, llm_move.position.1) {
                    if self.game.mode == Mode::LlmVsLlm {
                        self.stop_arena(ArenaStop::Aborted(
                            "validated LLM move could not be applied".to_string(),
                        ));
                    } else {
                        self.fallback_to_tactical("Invalid LLM move; used fallback");
                    }
                    return;
                }
                self.llm_route[pending.turn.profile_index] = llm_move.route_label();
                self.llm_status[pending.turn.profile_index] = "connected".to_string();
                self.ai_thinking = false;
                self.llm_attempt = 0;
                self.ai_notice = "LLM move".to_string();
                if self.game.mode == Mode::LlmVsLlm && self.game.status != Status::Playing {
                    self.arena_state = ArenaState::Finished;
                }
            }
            Ok(Err(error)) => {
                let pending = self
                    .pending_llm
                    .take()
                    .expect("received error must have a pending request");
                if !self.pending_is_current(&pending) {
                    self.ignore_stale_llm_result();
                    return;
                }
                if should_retry_llm(self.llm_attempt) {
                    eprintln!(
                        "大模型落子失败，准备第 {} 次尝试: {error}",
                        self.llm_attempt + 1
                    );
                    let mut turn = pending.turn;
                    if let Some(rejected) = extract_rejected_move(&error) {
                        if !turn.excluded.contains(&rejected) {
                            turn.excluded.push(rejected);
                        }
                    }
                    self.dispatch_llm_request(turn, Some(&error));
                } else {
                    let reason = llm_failure_summary(&error, 30);
                    self.llm_status[pending.turn.profile_index] = format!("Failed: {reason}");
                    if self.game.mode == Mode::LlmVsLlm {
                        eprintln!("大模型连续 {MAX_LLM_ATTEMPTS} 次失败，判技术负: {error}");
                        self.stop_arena(ArenaStop::TechnicalLoss {
                            loser: pending.turn.side,
                            reason,
                        });
                    } else {
                        eprintln!("大模型连续 {MAX_LLM_ATTEMPTS} 次失败，使用战术搜索: {error}");
                        self.fallback_to_tactical(&format!("LLM failed: {reason}; fallback"));
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                let pending = self
                    .pending_llm
                    .take()
                    .expect("disconnected result must have a pending request");
                if !self.pending_is_current(&pending) {
                    self.ignore_stale_llm_result();
                    return;
                }
                let backend = pending.turn.config.backend().label();
                self.llm_status[pending.turn.profile_index] = format!("{backend}: call stopped");
                if self.game.mode == Mode::LlmVsLlm {
                    self.stop_arena(ArenaStop::Aborted(
                        "LLM worker stopped unexpectedly".to_string(),
                    ));
                } else {
                    self.fallback_to_tactical("LLM stopped; used fallback");
                }
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
        self.match_id = self.match_id.wrapping_add(1);
    }

    fn active_profile_index(&self) -> Option<usize> {
        self.llm_settings
            .as_ref()
            .map(LlmSettings::active_profile_index)
    }

    fn pending_is_current(&self, pending: &PendingLlmRequest) -> bool {
        pending.turn.match_id == self.match_id
            && pending.turn.expected_ply == self.game.history.len()
            && pending.turn.side == self.game.turn
            && pending.request_id == self.next_request_id
    }

    fn ignore_stale_llm_result(&mut self) {
        self.ai_thinking = false;
        self.llm_attempt = 0;
        self.ai_notice = "Stale LLM response ignored".to_string();
    }

    fn active_config(&self) -> Option<&LlmConfig> {
        self.llm_settings
            .as_ref()
            .map(|settings| settings.active_profile().config())
    }

    fn has_arena_pair(&self) -> bool {
        self.llm_settings
            .as_ref()
            .and_then(LlmSettings::arena_pair)
            .is_some()
    }

    fn arena_profile_index(&self, side: Cell) -> Option<usize> {
        let index = match side {
            Cell::Black => 0,
            Cell::White => 1,
            Cell::Empty => return None,
        };
        self.llm_settings
            .as_ref()?
            .profiles()
            .get(index)
            .map(|_| index)
    }

    fn config_for_side(&self, side: Cell) -> Option<(usize, LlmConfig)> {
        let settings = self.llm_settings.as_ref()?;
        let profile_index = if self.game.mode == Mode::LlmVsLlm {
            self.arena_profile_index(side)?
        } else {
            settings.active_profile_index()
        };
        let config = settings.profiles().get(profile_index)?.config().clone();
        Some((profile_index, config))
    }

    fn arena_model_label(&self, side: Cell) -> String {
        let Some(index) = self.arena_profile_index(side) else {
            return "not configured".to_string();
        };
        compact_text(&self.llm_route[index], 26)
    }

    fn arena_header(&self) -> String {
        match self.game.status {
            Status::Won(side) => format!(
                "{} · {} wins on board · R to restart",
                side_label(side),
                self.arena_model_label(side)
            ),
            Status::Draw => format!(
                "Duel draw after {} moves · R to restart",
                self.game.history.len()
            ),
            Status::Playing => match &self.arena_state {
                ArenaState::Ready if self.has_arena_pair() => {
                    "Duel ready · Black starts · Space to start".to_string()
                }
                ArenaState::Ready => "Configure both duel players (C)".to_string(),
                ArenaState::Running if self.ai_thinking => format!(
                    "{} · {} · {}",
                    side_label(self.game.turn),
                    self.arena_model_label(self.game.turn),
                    self.arena_profile_index(self.game.turn)
                        .map(|index| compact_text(&self.llm_status[index], 28))
                        .unwrap_or_else(|| "thinking".to_string())
                ),
                ArenaState::Running => format!(
                    "Move {} · preparing {}",
                    self.game.history.len() + 1,
                    side_label(self.game.turn)
                ),
                ArenaState::Paused => format!(
                    "Duel paused before {} · Space to resume",
                    side_label(self.game.turn)
                ),
                ArenaState::Finished => "Duel finished · R to restart".to_string(),
                ArenaState::Stopped(ArenaStop::TechnicalLoss { loser, reason }) => format!(
                    "{} wins · {} technical loss: {}",
                    side_label(opponent(*loser)),
                    side_label(*loser),
                    compact_text(reason, 34)
                ),
                ArenaState::Stopped(ArenaStop::Aborted(reason)) => {
                    format!(
                        "Duel aborted: {} · Space to retry",
                        compact_text(reason, 44)
                    )
                }
            },
        }
    }

    fn toggle_arena(&mut self) {
        match self.arena_state.clone() {
            ArenaState::Ready => {
                if self.has_arena_pair() {
                    self.arena_state = ArenaState::Running;
                } else {
                    self.config_page.open(true);
                    self.show_config = true;
                }
            }
            ArenaState::Running => {
                self.cancel_ai();
                self.arena_state = ArenaState::Paused;
            }
            ArenaState::Paused => {
                if self.has_arena_pair() {
                    self.arena_state = ArenaState::Running;
                } else {
                    self.config_page.open(true);
                    self.show_config = true;
                }
            }
            ArenaState::Finished => {
                self.reset_arena();
                self.arena_state = ArenaState::Running;
            }
            ArenaState::Stopped(ArenaStop::TechnicalLoss { .. }) => {
                self.reset_arena();
                self.arena_state = ArenaState::Running;
            }
            ArenaState::Stopped(ArenaStop::Aborted(_)) => {
                if self.has_arena_pair() {
                    self.arena_state = ArenaState::Running;
                } else {
                    self.config_page.open(true);
                    self.show_config = true;
                }
            }
        }
    }

    fn reset_arena(&mut self) {
        self.cancel_ai();
        self.game = Game::new(Mode::LlmVsLlm);
        self.arena_state = ArenaState::Ready;
    }

    fn swap_arena_players(&mut self) {
        self.cancel_ai();
        if let Some(settings) = self.llm_settings.take() {
            if settings.arena_pair().is_some() {
                let active_profile = settings.active_profile_index();
                let mut profiles = settings.profiles().to_vec();
                profiles.swap(0, 1);
                self.llm_settings = Some(
                    LlmSettings::new(profiles, 1 - active_profile)
                        .expect("swapping two valid profiles must remain valid"),
                );
                self.llm_route.swap(0, 1);
                self.llm_status.swap(0, 1);
            } else {
                self.llm_settings = Some(settings);
            }
        }
        self.config_page = LlmConfigPage::new(self.llm_settings.as_ref(), None);
        self.game = Game::new(Mode::LlmVsLlm);
        self.arena_state = ArenaState::Ready;
    }

    fn stop_arena(&mut self, stop: ArenaStop) {
        if self.pending_llm.is_some() {
            if let Some(worker) = &self.llm_worker {
                worker.cancel();
            }
        }
        self.ai_thinking = false;
        self.pending_llm = None;
        self.llm_attempt = 0;
        self.arena_state = ArenaState::Stopped(stop);
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

fn profile_routes(settings: Option<&LlmSettings>) -> [String; 2] {
    let mut routes = [
        "LLM: not configured".to_string(),
        "LLM: not configured".to_string(),
    ];
    if let Some(settings) = settings {
        for (index, profile) in settings.profiles().iter().enumerate() {
            routes[index] = profile.config().model().to_string();
        }
    }
    routes
}

fn profile_statuses(settings: Option<&LlmSettings>) -> [String; 2] {
    let mut statuses = ["not configured".to_string(), "not configured".to_string()];
    if let Some(settings) = settings {
        for (index, profile) in settings.profiles().iter().enumerate() {
            statuses[index] = format!("{}: not connected", profile.config().backend().label());
        }
    }
    statuses
}

fn side_label(side: Cell) -> &'static str {
    match side {
        Cell::Black => "Black",
        Cell::White => "White",
        Cell::Empty => "Empty",
    }
}

pub(crate) fn compact_text(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let keep = max_chars.saturating_sub(3);
    format!("{}...", value.chars().take(keep).collect::<String>())
}

fn compact_text_to_width(value: &str, max_width: f32, font_size: u16) -> String {
    let mut max_chars = value.chars().count();
    loop {
        let candidate = compact_text(value, max_chars);
        if measure_text(&candidate, None, font_size, 1.0).width <= max_width || max_chars == 0 {
            return candidate;
        }
        max_chars -= 1;
    }
}

pub(crate) fn should_retry_llm(attempt: u8) -> bool {
    attempt < MAX_LLM_ATTEMPTS
}

pub(crate) fn llm_failure_summary(error: &str, max_chars: usize) -> String {
    let single_line = error.split_whitespace().collect::<Vec<_>>().join(" ");
    compact_text(&single_line, max_chars)
}

/// 从"模型返回了候选集外的落点: (x, y)"这类错误中提取模型选中的坐标，
/// 供重试时排除该位置并提示模型避开。
fn extract_rejected_move(error: &str) -> Option<(usize, usize)> {
    if !error.contains("候选集外的落点") {
        return None;
    }
    let digits: Vec<usize> = error
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<usize>().ok())
        .collect();
    (digits.len() == 2).then(|| (digits[0], digits[1]))
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
        let (request_started_sender, request_started_receiver) = mpsc::channel();
        let (server_shutdown_sender, server_shutdown_receiver) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let bytes_read = stream.read(&mut request).unwrap();
            assert!(bytes_read > 0);
            request_started_sender.send(()).unwrap();
            let _ = server_shutdown_receiver.recv_timeout(Duration::from_secs(5));
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
                Cell::White,
                [[Cell::Empty; crate::game::BOARD]; crate::game::BOARD],
                vec![(7, 7)],
                Vec::new(),
            )
            .unwrap();

        request_started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("the server should receive the request before it is cancelled");
        worker.cancel();

        let cancellation_result = result.recv_timeout(Duration::from_secs(1));
        let _ = server_shutdown_sender.send(());
        server.join().unwrap();

        assert!(matches!(
            cancellation_result,
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }
}

#[cfg(test)]
#[path = "app/tests.rs"]
mod tests;
