use super::*;
use std::cell::{Cell as Counter, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

enum FakeResponse {
    Success(LlmMove),
    Error(String),
    Disconnected,
    Pending,
}

#[derive(Debug)]
struct RequestRecord {
    side: Cell,
    backend: LlmBackend,
    api_key: String,
    model: String,
    board: [[Cell; crate::game::BOARD]; crate::game::BOARD],
    candidates: Vec<(usize, usize)>,
    excluded: Vec<(usize, usize)>,
}

#[derive(Default)]
struct FakeWorkerState {
    responses: RefCell<VecDeque<FakeResponse>>,
    pending_senders: RefCell<Vec<mpsc::Sender<Result<LlmMove, String>>>>,
    requests: RefCell<Vec<RequestRecord>>,
    cancel_count: Counter<usize>,
}

struct FakeWorker {
    state: Rc<FakeWorkerState>,
}

impl LlmRequestWorker for FakeWorker {
    fn request(
        &self,
        config: LlmConfig,
        side: Cell,
        board: [[Cell; crate::game::BOARD]; crate::game::BOARD],
        candidates: Vec<(usize, usize)>,
        excluded: Vec<(usize, usize)>,
    ) -> Result<Receiver<Result<LlmMove, String>>, String> {
        self.state.requests.borrow_mut().push(RequestRecord {
            side,
            backend: config.backend(),
            api_key: config.api_key().to_string(),
            model: config.model().to_string(),
            board,
            candidates,
            excluded,
        });
        let response = self
            .state
            .responses
            .borrow_mut()
            .pop_front()
            .expect("the fake worker needs one response per request");
        let (sender, receiver) = mpsc::channel();
        match response {
            FakeResponse::Success(llm_move) => sender.send(Ok(llm_move)).unwrap(),
            FakeResponse::Error(error) => sender.send(Err(error)).unwrap(),
            FakeResponse::Disconnected => drop(sender),
            FakeResponse::Pending => self.state.pending_senders.borrow_mut().push(sender),
        }
        Ok(receiver)
    }

    fn cancel(&self) {
        self.state
            .cancel_count
            .set(self.state.cancel_count.get() + 1);
    }
}

fn local_config(model: &str, port: u16) -> LlmConfig {
    LlmConfig::new_unchecked(
        LlmBackend::Local,
        String::new(),
        format!("http://127.0.0.1:{port}/v1/chat/completions"),
        model.to_string(),
    )
}

fn single_settings() -> LlmSettings {
    LlmSettings::new(
        vec![
            crate::llm_ai::LlmProfile::new("Human opponent", local_config("test-model", 11434))
                .unwrap(),
        ],
        0,
    )
    .unwrap()
}

fn arena_settings() -> LlmSettings {
    let black = LlmConfig::new_unchecked(
        LlmBackend::OpenRouter,
        "black-secret".to_string(),
        crate::llm_ai::DEFAULT_CLOUD_API_URL.to_string(),
        "black-model".to_string(),
    );
    let white = local_config("white-model", 1234);
    LlmSettings::new(
        vec![
            crate::llm_ai::LlmProfile::new("Black player", black).unwrap(),
            crate::llm_ai::LlmProfile::new("White player", white).unwrap(),
        ],
        0,
    )
    .unwrap()
}

fn app_with_fake_worker(
    mode: Mode,
    settings: LlmSettings,
    responses: impl IntoIterator<Item = FakeResponse>,
) -> (App, Rc<FakeWorkerState>) {
    let state = Rc::new(FakeWorkerState {
        responses: RefCell::new(responses.into_iter().collect()),
        ..FakeWorkerState::default()
    });
    let mut game = Game::new(mode);
    if mode == Mode::HumanVsAi {
        assert!(game.place(crate::game::CENTER, crate::game::CENTER));
    }
    let llm_route = profile_routes(Some(&settings));
    let llm_status = profile_statuses(Some(&settings));
    let app = App {
        game,
        ai_algorithm: AiAlgorithm::LargeModel,
        ai_thinking: false,
        pending_llm: None,
        llm_worker: Some(Box::new(FakeWorker {
            state: Rc::clone(&state),
        })),
        ai_notice: String::new(),
        config_page: LlmConfigPage::new(Some(&settings), None),
        llm_settings: Some(settings),
        llm_route,
        llm_status,
        show_config: false,
        llm_attempt: 0,
        arena_state: if mode == Mode::LlmVsLlm {
            ArenaState::Running
        } else {
            ArenaState::Ready
        },
        match_id: 1,
        next_request_id: 0,
    };
    (app, state)
}

fn success(position: (usize, usize), model: &str) -> FakeResponse {
    FakeResponse::Success(LlmMove::new_for_test(position, model, None))
}

#[test]
fn compact_text_preserves_short_model_ids_and_truncates_long_ones() {
    assert_eq!(compact_text("openai/gpt-5-mini", 24), "openai/gpt-5-mini");
    assert_eq!(
        compact_text("provider/a-very-long-model-name", 16),
        "provider/a-ve..."
    );
    assert_eq!(compact_text("本地模型🦀版本", 5), "本地...");
    assert_eq!(compact_text("abcdef", 2), "..");
}

#[test]
fn llm_retries_twice_before_falling_back() {
    assert!(should_retry_llm(1));
    assert!(should_retry_llm(2));
    assert!(!should_retry_llm(3));
}

#[test]
fn extracts_rejected_move_only_from_out_of_candidate_errors() {
    assert_eq!(
        extract_rejected_move("模型返回了候选集外的落点: (7, 7)"),
        Some((7, 7))
    );
    assert_eq!(
        extract_rejected_move("模型返回了候选集外的落点: (12, 3)"),
        Some((12, 3))
    );
    assert_eq!(
        extract_rejected_move("无法解析模型落点: 我选择 x=7 y=7"),
        None
    );
    assert_eq!(extract_rejected_move("Cloud request failed: timeout"), None);
}

#[test]
fn retry_accumulates_excluded_rejected_moves() {
    let (mut app, state) = app_with_fake_worker(
        Mode::HumanVsAi,
        single_settings(),
        [
            FakeResponse::Error("模型返回了候选集外的落点: (7, 7)".to_string()),
            FakeResponse::Error("模型返回了候选集外的落点: (7, 6)".to_string()),
            FakeResponse::Success(LlmMove::new_for_test((7, 5), "m", None)),
        ],
    );

    app.update_ai();
    app.update_ai();
    app.update_ai();
    app.update_ai();

    let requests = state.requests.borrow();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].excluded, vec![]);
    assert_eq!(requests[1].excluded, vec![(7, 7)]);
    assert_eq!(requests[2].excluded, vec![(7, 7), (7, 6)]);
}

#[test]
fn llm_failure_reason_is_single_line_and_bounded() {
    assert_eq!(
        llm_failure_summary("OpenRouter HTTP 429:\nrate   limited", 40),
        "OpenRouter HTTP 429: rate limited"
    );
    assert_eq!(
        llm_failure_summary(
            "OpenRouter request failed because the network is unavailable",
            24
        ),
        "OpenRouter request fa..."
    );
}

#[test]
fn human_mode_retries_three_times_then_places_one_tactical_fallback() {
    let (mut app, state) = app_with_fake_worker(
        Mode::HumanVsAi,
        single_settings(),
        [
            FakeResponse::Error("first failure".to_string()),
            FakeResponse::Error("second failure".to_string()),
            FakeResponse::Error("third failure".to_string()),
        ],
    );

    app.update_ai();
    app.update_ai();
    app.update_ai();
    app.update_ai();

    assert_eq!(state.requests.borrow().len(), 3);
    assert_eq!(app.game.history.len(), 2);
    assert_eq!(app.game.turn, Cell::Black);
    assert!(!app.ai_thinking);
    assert!(app.pending_llm.is_none());
    assert_eq!(app.llm_attempt, 0);
    assert!(app.ai_notice.contains("fallback"));
    assert!(app.llm_status[0].contains("third failure"));
}

#[test]
fn successful_human_retry_places_the_llm_move_and_resets_state() {
    let (mut app, state) = app_with_fake_worker(
        Mode::HumanVsAi,
        single_settings(),
        [
            FakeResponse::Error("temporary failure".to_string()),
            success(
                (crate::game::CENTER, crate::game::CENTER + 1),
                "resolved-model",
            ),
        ],
    );

    app.update_ai();
    app.update_ai();
    app.update_ai();

    assert_eq!(state.requests.borrow().len(), 2);
    assert_eq!(app.game.history.len(), 2);
    assert_eq!(
        app.game.board[crate::game::CENTER + 1][crate::game::CENTER],
        Cell::White
    );
    assert_eq!(app.game.turn, Cell::Black);
    assert!(!app.ai_thinking);
    assert!(app.pending_llm.is_none());
    assert_eq!(app.llm_attempt, 0);
    assert_eq!(app.ai_notice, "LLM move");
    assert_eq!(app.llm_route[0], "resolved-model");
    assert_eq!(app.llm_status[0], "connected");
}

#[test]
fn cancellation_discards_the_pending_receiver_and_notifies_the_worker() {
    let (mut app, state) =
        app_with_fake_worker(Mode::HumanVsAi, single_settings(), [FakeResponse::Pending]);

    app.update_ai();
    assert!(app.pending_llm.is_some());
    app.cancel_ai();

    assert_eq!(state.requests.borrow().len(), 1);
    assert_eq!(state.cancel_count.get(), 1);
    assert!(!app.ai_thinking);
    assert!(app.pending_llm.is_none());
    assert_eq!(app.game.history.len(), 1);
    let sender = state.pending_senders.borrow_mut().pop().unwrap();
    assert!(sender.send(Err("late response".to_string())).is_err());
}

#[test]
fn arena_alternates_black_and_white_with_isolated_profiles() {
    let (mut app, state) = app_with_fake_worker(
        Mode::LlmVsLlm,
        arena_settings(),
        [
            success((crate::game::CENTER, crate::game::CENTER), "black-route"),
            success(
                (crate::game::CENTER, crate::game::CENTER + 1),
                "white-route",
            ),
        ],
    );

    app.update_ai();
    app.update_ai();
    app.update_ai();
    {
        let requests = state.requests.borrow();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].side, Cell::Black);
        assert_eq!(requests[0].backend, LlmBackend::OpenRouter);
        assert_eq!(requests[0].api_key, "black-secret");
        assert_eq!(requests[0].model, "black-model");
        assert_eq!(requests[0].candidates, vec![(7, 7)]);
        assert!(
            requests[0]
                .board
                .iter()
                .flatten()
                .all(|cell| *cell == Cell::Empty)
        );
        assert_eq!(requests[1].side, Cell::White);
        assert_eq!(requests[1].backend, LlmBackend::Local);
        assert!(requests[1].api_key.is_empty());
        assert_eq!(requests[1].model, "white-model");
        assert_eq!(requests[1].board[7][7], Cell::Black);
    }

    app.update_ai();
    assert_eq!(app.game.history.len(), 2);
    assert_eq!(app.game.turn, Cell::Black);
    assert_eq!(app.llm_route[0], "black-route");
    assert_eq!(app.llm_route[1], "white-route");
    assert_eq!(app.llm_status[0], "connected");
    assert_eq!(app.llm_status[1], "connected");
}

#[test]
fn arena_exhausted_retries_are_a_technical_loss_without_fallback() {
    let (mut app, state) = app_with_fake_worker(
        Mode::LlmVsLlm,
        arena_settings(),
        [
            FakeResponse::Error("first failure".to_string()),
            FakeResponse::Error("second failure".to_string()),
            FakeResponse::Error("third failure".to_string()),
        ],
    );

    app.update_ai();
    app.update_ai();
    app.update_ai();
    app.update_ai();

    assert_eq!(state.requests.borrow().len(), 3);
    assert!(app.game.history.is_empty());
    assert_eq!(app.game.turn, Cell::Black);
    assert!(matches!(
        app.arena_state,
        ArenaState::Stopped(ArenaStop::TechnicalLoss {
            loser: Cell::Black,
            ..
        })
    ));
    assert!(!app.ai_thinking);
    assert!(app.pending_llm.is_none());
}

#[test]
fn arena_worker_disconnect_aborts_instead_of_awarding_a_win() {
    let (mut app, state) = app_with_fake_worker(
        Mode::LlmVsLlm,
        arena_settings(),
        [FakeResponse::Disconnected],
    );

    app.update_ai();
    app.update_ai();

    assert_eq!(state.requests.borrow().len(), 1);
    assert!(app.game.history.is_empty());
    assert!(matches!(
        app.arena_state,
        ArenaState::Stopped(ArenaStop::Aborted(_))
    ));
}

#[test]
fn swapping_arena_colors_resets_and_makes_the_old_white_profile_black() {
    let (mut app, state) = app_with_fake_worker(
        Mode::LlmVsLlm,
        arena_settings(),
        [success((7, 7), "white-as-black")],
    );

    assert!(app.handle_action(Action::ToggleAlgorithm));
    assert!(app.game.history.is_empty());
    assert_eq!(app.arena_state, ArenaState::Ready);
    let settings = app.llm_settings.as_ref().unwrap();
    assert_eq!(settings.profiles()[0].config().model(), "white-model");
    assert_eq!(settings.profiles()[1].config().model(), "black-model");
    assert_eq!(settings.active_profile().config().model(), "black-model");
    app.toggle_arena();
    app.update_ai();

    let requests = state.requests.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].side, Cell::Black);
    assert_eq!(requests[0].model, "white-model");
}

#[test]
fn stale_arena_response_is_ignored_without_mutating_the_board() {
    let (mut app, state) =
        app_with_fake_worker(Mode::LlmVsLlm, arena_settings(), [FakeResponse::Pending]);

    app.update_ai();
    app.match_id = app.match_id.wrapping_add(1);
    state
        .pending_senders
        .borrow_mut()
        .pop()
        .unwrap()
        .send(Ok(LlmMove::new_for_test((7, 7), "late-model", None)))
        .unwrap();
    app.update_ai();

    assert!(app.game.history.is_empty());
    assert_eq!(app.game.turn, Cell::Black);
    assert_eq!(app.ai_notice, "Stale LLM response ignored");
    assert!(app.pending_llm.is_none());
}

#[test]
fn stale_arena_error_is_ignored_without_retry_or_technical_loss() {
    let (mut app, state) =
        app_with_fake_worker(Mode::LlmVsLlm, arena_settings(), [FakeResponse::Pending]);

    app.update_ai();
    app.match_id = app.match_id.wrapping_add(1);
    state
        .pending_senders
        .borrow_mut()
        .pop()
        .unwrap()
        .send(Err("late failure".to_string()))
        .unwrap();
    app.update_ai();

    assert_eq!(state.requests.borrow().len(), 1);
    assert!(app.game.history.is_empty());
    assert_eq!(app.arena_state, ArenaState::Running);
    assert_eq!(app.ai_notice, "Stale LLM response ignored");
    assert!(app.pending_llm.is_none());
}

#[test]
fn restarting_an_in_flight_arena_match_cancels_and_returns_to_ready() {
    let (mut app, state) =
        app_with_fake_worker(Mode::LlmVsLlm, arena_settings(), [FakeResponse::Pending]);

    app.update_ai();
    assert!(app.handle_action(Action::Restart));

    assert!(app.game.history.is_empty());
    assert_eq!(app.game.turn, Cell::Black);
    assert_eq!(app.arena_state, ArenaState::Ready);
    assert_eq!(state.cancel_count.get(), 1);
    let sender = state.pending_senders.borrow_mut().pop().unwrap();
    assert!(
        sender
            .send(Ok(LlmMove::new_for_test((7, 7), "late", None)))
            .is_err()
    );
}

#[test]
fn an_arena_with_only_one_profile_opens_pair_configuration() {
    let (mut app, _) = app_with_fake_worker(Mode::LlmVsLlm, single_settings(), std::iter::empty());
    app.arena_state = ArenaState::Ready;

    assert_eq!(app.arena_profile_index(Cell::Black), Some(0));
    assert_eq!(app.arena_profile_index(Cell::White), None);
    app.toggle_arena();

    assert!(app.show_config);
    assert_eq!(app.arena_state, ArenaState::Ready);
    assert!(app.pending_llm.is_none());
}

#[test]
fn technical_loss_rematch_starts_from_an_empty_board() {
    let (mut app, _) = app_with_fake_worker(
        Mode::LlmVsLlm,
        arena_settings(),
        [
            FakeResponse::Error("one".to_string()),
            FakeResponse::Error("two".to_string()),
            FakeResponse::Error("three".to_string()),
            success((7, 7), "rematch-black"),
        ],
    );
    assert!(app.game.place(3, 7));
    assert!(app.game.place(3, 8));

    app.update_ai();
    app.update_ai();
    app.update_ai();
    app.update_ai();
    assert!(matches!(
        app.arena_state,
        ArenaState::Stopped(ArenaStop::TechnicalLoss { .. })
    ));
    assert_eq!(app.game.history.len(), 2);

    app.toggle_arena();
    assert_eq!(app.arena_state, ArenaState::Running);
    assert!(app.game.history.is_empty());
    assert_eq!(app.game.turn, Cell::Black);
    app.update_ai();

    assert!(app.pending_llm.is_some());
}

#[test]
fn pause_cancels_the_turn_and_resume_requests_the_same_side_again() {
    let (mut app, state) = app_with_fake_worker(
        Mode::LlmVsLlm,
        arena_settings(),
        [FakeResponse::Pending, success((7, 7), "resumed-black")],
    );

    app.update_ai();
    app.toggle_arena();
    assert_eq!(app.arena_state, ArenaState::Paused);
    assert_eq!(state.cancel_count.get(), 1);
    assert!(app.pending_llm.is_none());

    app.toggle_arena();
    app.update_ai();
    app.update_ai();

    assert_eq!(state.requests.borrow().len(), 2);
    assert!(
        state
            .requests
            .borrow()
            .iter()
            .all(|request| request.side == Cell::Black)
    );
    assert_eq!(app.game.history, vec![(7, 7)]);
    assert_eq!(app.game.turn, Cell::White);
}

#[test]
fn a_board_win_finishes_the_arena_without_scheduling_the_opponent() {
    let (mut app, state) = app_with_fake_worker(
        Mode::LlmVsLlm,
        arena_settings(),
        [success((7, 7), "winning-black")],
    );
    for x in 3..7 {
        assert!(app.game.place(x, 7));
        assert!(app.game.place(x, 8));
    }

    app.update_ai();
    app.update_ai();

    assert_eq!(app.game.status, Status::Won(Cell::Black));
    assert_eq!(app.arena_state, ArenaState::Finished);
    assert!(!app.should_ai_move());
    assert_eq!(state.requests.borrow().len(), 1);
}
