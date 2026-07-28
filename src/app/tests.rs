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

#[derive(Default)]
struct FakeWorkerState {
    responses: RefCell<VecDeque<FakeResponse>>,
    pending_senders: RefCell<Vec<mpsc::Sender<Result<LlmMove, String>>>>,
    request_count: Counter<usize>,
    cancel_count: Counter<usize>,
}

struct FakeWorker {
    state: Rc<FakeWorkerState>,
}

impl LlmRequestWorker for FakeWorker {
    fn request(
        &self,
        _config: LlmConfig,
        _board: [[Cell; crate::game::BOARD]; crate::game::BOARD],
        _candidates: Vec<(usize, usize)>,
    ) -> Result<Receiver<Result<LlmMove, String>>, String> {
        self.state
            .request_count
            .set(self.state.request_count.get() + 1);
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

fn app_with_fake_worker(
    responses: impl IntoIterator<Item = FakeResponse>,
) -> (App, Rc<FakeWorkerState>) {
    let state = Rc::new(FakeWorkerState {
        responses: RefCell::new(responses.into_iter().collect()),
        ..FakeWorkerState::default()
    });
    let config = LlmConfig::new_unchecked(
        LlmBackend::Local,
        String::new(),
        "http://127.0.0.1:11434/v1/chat/completions".to_string(),
        "test-model".to_string(),
    );
    let mut game = Game::new(Mode::HumanVsAi);
    assert!(game.place(crate::game::CENTER, crate::game::CENTER));
    let app = App {
        game,
        ai_algorithm: AiAlgorithm::LargeModel,
        ai_thinking: false,
        pending_llm: None,
        llm_worker: Some(Box::new(FakeWorker {
            state: Rc::clone(&state),
        })),
        ai_notice: String::new(),
        config_page: LlmConfigPage::new(Some(&config), None),
        active_llm_config: Some(config),
        llm_status: String::new(),
        show_config: false,
        llm_attempt: 0,
    };
    (app, state)
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
fn three_llm_failures_retry_then_place_exactly_one_fallback_move() {
    let (mut app, state) = app_with_fake_worker([
        FakeResponse::Error("first failure".to_string()),
        FakeResponse::Error("second failure".to_string()),
        FakeResponse::Error("third failure".to_string()),
    ]);

    app.update_ai();
    app.update_ai();
    app.update_ai();
    app.update_ai();

    assert_eq!(state.request_count.get(), 3);
    assert_eq!(app.game.history.len(), 2);
    assert_eq!(app.game.turn, Cell::Black);
    assert!(!app.ai_thinking);
    assert!(app.pending_llm.is_none());
    assert_eq!(app.llm_attempt, 0);
    assert!(app.ai_notice.contains("fallback"));
    assert!(app.llm_status.contains("third failure"));
}

#[test]
fn a_successful_retry_places_the_llm_move_and_resets_retry_state() {
    let llm_move = LlmMove::new_for_test(
        (crate::game::CENTER, crate::game::CENTER + 1),
        "resolved-model",
        Some("local-provider".to_string()),
    );
    let (mut app, state) = app_with_fake_worker([
        FakeResponse::Error("temporary failure".to_string()),
        FakeResponse::Success(llm_move),
    ]);

    app.update_ai();
    app.update_ai();
    app.update_ai();

    assert_eq!(state.request_count.get(), 2);
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
    assert_eq!(app.llm_status, "resolved-model via local-provider");
}

#[test]
fn a_disconnected_worker_falls_back_without_retrying() {
    let (mut app, state) = app_with_fake_worker([FakeResponse::Disconnected]);

    app.update_ai();
    app.update_ai();

    assert_eq!(state.request_count.get(), 1);
    assert_eq!(app.game.history.len(), 2);
    assert!(!app.ai_thinking);
    assert!(app.pending_llm.is_none());
    assert!(app.ai_notice.contains("fallback"));
}

#[test]
fn cancellation_discards_the_pending_receiver_and_notifies_the_worker() {
    let (mut app, state) = app_with_fake_worker([FakeResponse::Pending]);

    app.update_ai();
    assert!(app.pending_llm.is_some());

    app.cancel_ai();

    assert_eq!(state.request_count.get(), 1);
    assert_eq!(state.cancel_count.get(), 1);
    assert!(!app.ai_thinking);
    assert!(app.pending_llm.is_none());
    assert_eq!(app.llm_attempt, 0);
    assert_eq!(app.game.history.len(), 1);
    let sender = state.pending_senders.borrow_mut().pop().unwrap();
    assert!(sender.send(Err("late response".to_string())).is_err());
}
