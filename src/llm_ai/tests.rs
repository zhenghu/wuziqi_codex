use super::*;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const TEST_IO_TIMEOUT: Duration = Duration::from_secs(5);
static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        loop {
            let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("wuziqi-{label}-{}-{sequence}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self { path },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create test directory {}: {error}", path.display()),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.path.join(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        match std::fs::remove_dir_all(&self.path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                eprintln!(
                    "cannot remove test directory {}: {error}",
                    self.path.display()
                );
            }
            _ => {}
        }
    }
}

struct TestServer {
    address: SocketAddr,
    url: String,
    captured_request: Receiver<String>,
    handle: Option<JoinHandle<Result<(), String>>>,
}

impl TestServer {
    fn new(status: &str, body: impl Into<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        let body = body.into();
        let (sender, captured_request) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .map_err(|error| format!("cannot accept test HTTP request: {error}"))?;
            stream
                .set_read_timeout(Some(TEST_IO_TIMEOUT))
                .map_err(|error| format!("cannot set test HTTP read timeout: {error}"))?;
            let request = read_http_request(&mut stream)
                .map_err(|error| format!("cannot read test HTTP request: {error}"))?;
            sender
                .send(String::from_utf8_lossy(&request).into_owned())
                .map_err(|_| "test HTTP request receiver was dropped".to_string())?;
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .map_err(|error| format!("cannot write test HTTP response: {error}"))
        });
        Self {
            address,
            url: format!("http://{address}/v1/chat/completions"),
            captured_request,
            handle: Some(handle),
        }
    }

    fn url(&self) -> &str {
        &self.url
    }

    fn finish(mut self) -> String {
        let request = self
            .captured_request
            .recv_timeout(TEST_IO_TIMEOUT)
            .expect("test server did not capture an HTTP request");
        self.join().expect("test HTTP server failed");
        request
    }

    fn join(&mut self) -> Result<(), String> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        handle
            .join()
            .map_err(|_| "test HTTP server panicked".to_string())?
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if self.handle.is_none() {
            return;
        }

        // Unblock accept/read if a test panics before sending the real request.
        if let Ok(stream) = TcpStream::connect(self.address) {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Err(error) = self.join() {
            eprintln!("{error}");
        }
    }
}

fn read_http_request(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 4 * 1024];
    loop {
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            break;
        }
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
    Ok(request)
}

fn run_request(config: &LlmConfig, candidates: &[(usize, usize)]) -> Result<LlmMove, String> {
    let client = match config.backend() {
        LlmBackend::OpenRouter => build_openrouter_client(),
        LlmBackend::Local => build_local_client(),
    }
    .unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(request_move(
            &client,
            config,
            &[[Cell::Empty; BOARD]; BOARD],
            candidates,
        ))
}

fn local_test_config(server: &TestServer) -> LlmConfig {
    LlmConfig::new_unchecked(
        LlmBackend::Local,
        String::new(),
        server.url().to_string(),
        "model".into(),
    )
}

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
            LlmConfig::new(LlmBackend::Local, String::new(), url.into(), "model".into()).is_err(),
            "unexpectedly accepted {url}"
        );
    }
}

#[test]
fn migrates_legacy_configuration_to_the_system_path() {
    let root = TestDir::new("config-migration");
    let legacy = root.join(CONFIG_FILE_NAME);
    let current = root.join("system").join(CONFIG_FILE_NAME);
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
}

#[test]
fn upgrades_a_current_three_field_configuration_in_place() {
    let root = TestDir::new("schema-upgrade");
    let current = root.join(CONFIG_FILE_NAME);
    let missing_legacy = root.join("missing").join(CONFIG_FILE_NAME);
    std::fs::write(
        &current,
        format!(r#"{{"api_key":"key","api_url":"{DEFAULT_OPENROUTER_API_URL}","model":"model"}}"#),
    )
    .unwrap();

    let loaded = LlmConfig::load_with_paths(&current, &missing_legacy).unwrap();

    assert_eq!(loaded.backend(), LlmBackend::OpenRouter);
    let upgraded: Value =
        serde_json::from_str(&std::fs::read_to_string(&current).unwrap()).unwrap();
    assert_eq!(upgraded["backend"], "openrouter");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn local_configuration_round_trips_without_an_api_key() {
    let root = TestDir::new("local-config");
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
}

#[test]
fn loading_local_configuration_removes_a_stored_cloud_key() {
    let root = TestDir::new("local-key-cleanup");
    let current = root.join(CONFIG_FILE_NAME);
    let missing_legacy = root.join("missing").join(CONFIG_FILE_NAME);
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
}

#[test]
fn atomically_replaces_an_existing_configuration() {
    let root = TestDir::new("atomic-save");
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
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn failed_replacement_preserves_the_destination_and_cleans_up_the_temporary_file() {
    let root = TestDir::new("failed-save");
    let destination = root.join(CONFIG_FILE_NAME);
    std::fs::create_dir(&destination).unwrap();
    let marker = destination.join("keep");
    std::fs::write(&marker, "original destination").unwrap();
    let config = LlmConfig::new(
        LlmBackend::Local,
        String::new(),
        DEFAULT_LOCAL_API_URL.into(),
        DEFAULT_LOCAL_MODEL.into(),
    )
    .unwrap();

    let error = config.save_to_path(&destination).unwrap_err();

    assert!(error.to_string().contains("replace"));
    assert!(destination.is_dir());
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        "original destination"
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn rejects_an_oversized_api_response() {
    let server = TestServer::new("200 OK", "x".repeat(MAX_RESPONSE_BYTES + 1));
    let config = local_test_config(&server);

    let error = run_request(&config, &[(7, 7)]).unwrap_err();
    server.finish();

    assert!(error.contains("exceeds 65536 bytes"));
}

#[test]
fn reports_structured_http_api_errors() {
    let server = TestServer::new(
        "429 Too Many Requests",
        r#"{"error":{"message":"rate limited"}}"#,
    );
    let config = local_test_config(&server);

    let error = run_request(&config, &[(7, 7)]).unwrap_err();
    server.finish();

    assert_eq!(error, "Local HTTP 429: rate limited");
}

#[test]
fn reports_string_and_plain_text_http_api_errors() {
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
        let server = TestServer::new("500 Internal Server Error", body);
        let config = local_test_config(&server);

        let error = run_request(&config, &[(7, 7)]).unwrap_err();
        server.finish();

        assert_eq!(error, expected);
    }
}

#[test]
fn rejects_invalid_json_in_a_successful_response() {
    let server = TestServer::new("200 OK", "not-json");
    let config = local_test_config(&server);

    let error = run_request(&config, &[(7, 7)]).unwrap_err();
    server.finish();

    assert!(error.starts_with("Local returned invalid JSON:"));
}

#[test]
fn reports_a_successful_response_without_text() {
    let server = TestServer::new(
        "200 OK",
        r#"{"choices":[{"finish_reason":"stop","message":{"content":null}}]}"#,
    );
    let config = local_test_config(&server);

    let error = run_request(&config, &[(7, 7)]).unwrap_err();
    server.finish();

    assert_eq!(
        error,
        "Local response has no text (finish_reason=stop, reasoning_tokens=0)"
    );
}

#[test]
fn reports_a_business_error_in_a_successful_response() {
    let server = TestServer::new("200 OK", r#"{"error":{"message":"model unavailable"}}"#);
    let config = local_test_config(&server);

    let error = run_request(&config, &[(7, 7)]).unwrap_err();
    server.finish();

    assert_eq!(error, "Local error: model unavailable");
}

#[test]
fn rejects_a_move_outside_the_candidate_set() {
    let server = TestServer::new(
        "200 OK",
        r#"{"choices":[{"message":{"content":"{\"x\":1,\"y\":2}"}}]}"#,
    );
    let config = local_test_config(&server);

    let error = run_request(&config, &[(7, 7)]).unwrap_err();
    server.finish();

    assert_eq!(error, "模型返回了候选集外的落点: (1, 2)");
}

#[test]
fn openrouter_requires_the_routed_model_in_a_successful_response() {
    let server = TestServer::new(
        "200 OK",
        r#"{"choices":[{"message":{"content":"{\"x\":7,\"y\":7}"}}]}"#,
    );
    let config = LlmConfig::new_unchecked(
        LlmBackend::OpenRouter,
        "openrouter-secret".into(),
        server.url().to_string(),
        DEFAULT_OPENROUTER_MODEL.into(),
    );

    let error = run_request(&config, &[(7, 7)]).unwrap_err();
    server.finish();

    assert_eq!(error, "OpenRouter response is missing the routed model");
}

#[test]
fn local_request_never_sends_cloud_credentials_or_openrouter_fields() {
    let server = TestServer::new(
        "200 OK",
        r#"{"choices":[{"message":{"content":"{\"x\":7,\"y\":7}"}}]}"#,
    );
    let secret = "must-not-leak-secret";
    let config = LlmConfig::new_unchecked(
        LlmBackend::Local,
        secret.into(),
        server.url().to_string(),
        "qwen3:4b".into(),
    );

    let llm_move = run_request(&config, &[(7, 7)]).unwrap();
    let request = server.finish();
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
    let server = TestServer::new(
        "200 OK",
        r#"{
            "model":"openai/gpt-5-mini",
            "provider":"OpenAI",
            "choices":[{"message":{"content":"{\"x\":7,\"y\":7}"}}]
        }"#,
    );
    let config = LlmConfig::new_unchecked(
        LlmBackend::OpenRouter,
        "openrouter-secret".into(),
        server.url().to_string(),
        DEFAULT_OPENROUTER_MODEL.into(),
    );

    run_request(&config, &[(7, 7)]).unwrap();
    let request = server.finish();
    let request_lowercase = request.to_ascii_lowercase();

    assert!(request_lowercase.contains("authorization: bearer openrouter-secret"));
    assert!(request_lowercase.contains("x-openrouter-title: wuziqi"));
    assert!(request.contains("\"reasoning\""));
    assert!(request.contains("\"max_completion_tokens\":1024"));
}
