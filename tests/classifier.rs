//! The classifier router behind the proxy, against a mock Ollama and a mock Anthropic upstream.

mod common;

use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use axum::{
    http::{HeaderMap, StatusCode},
    routing::{any, post},
    Router,
};
use common::serve;
use serde_json::{json, Value};
use smart_claude_router::{
    classifier::Prompt,
    classifier_router::ClassifierRouter,
    config::Config,
    ollama::{ClassifyError, OllamaClient},
    proxy::Proxy,
    router::{DecisionSource, RequestView},
};

const TIMEOUT_MS: u64 = 300;

/// What the mock Ollama saw.
#[derive(Default)]
struct Seen {
    calls: AtomicUsize,
    body: Mutex<Option<Value>>,
    headers: Mutex<Option<HeaderMap>>,
}

/// A mock Ollama whose `/api/chat` answers with `reply`.
async fn ollama<F, Fut>(reply: F) -> (SocketAddr, Arc<Seen>)
where
    F: Fn() -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = (StatusCode, String)> + Send + 'static,
{
    let seen = Arc::new(Seen::default());
    let state = seen.clone();
    let app = Router::new().route(
        "/api/chat",
        post(move |headers: HeaderMap, body: axum::body::Bytes| {
            let state = state.clone();
            let reply = reply.clone();
            async move {
                state.calls.fetch_add(1, Ordering::SeqCst);
                *state.body.lock().unwrap() = serde_json::from_slice(&body).ok();
                *state.headers.lock().unwrap() = Some(headers);
                reply().await
            }
        }),
    );
    (serve(app).await, seen)
}

/// An Ollama chat reply whose model said `content`.
fn chat(content: &str) -> (StatusCode, String) {
    (
        StatusCode::OK,
        json!({ "message": { "role": "assistant", "content": content } }).to_string(),
    )
}

fn grade(tier: &str, effort: &str) -> (StatusCode, String) {
    chat(&json!({ "tier": tier, "effort": effort, "confidence": 0.9 }).to_string())
}

/// A classifier router against `ollama_addr`. `classifier_extra` is added to the `[classifier]`
/// table and `rest` after it (for example a `[rules]` table).
fn router_with(ollama_addr: SocketAddr, classifier_extra: &str, rest: &str) -> ClassifierRouter {
    let config = Config::from_toml(&format!(
        "router = \"classifier\"\n[classifier]\nurl = \"http://{ollama_addr}\"\ntimeout_ms = {TIMEOUT_MS}\n{classifier_extra}\n{rest}"
    ))
    .unwrap();
    let client = OllamaClient::new(&config.classifier).unwrap();
    ClassifierRouter::new(&config, client, Prompt::builtin())
}

/// The summary of a new user turn, as the proxy would give it to the router.
fn view(text: &str) -> RequestView {
    RequestView::parse(&HeaderMap::new(), user_turn(text).to_string().as_bytes()).unwrap()
}

async fn setup(
    ollama_addr: SocketAddr,
) -> impl Fn(Value, &'static str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value>>> {
    setup_with(router_with(ollama_addr, "", "")).await
}

/// A proxy using `router`, in front of a mock Anthropic. Returns a function that posts one
/// `/v1/messages` request and gives back the JSON body the upstream received.
async fn setup_with(
    router: ClassifierRouter,
) -> impl Fn(Value, &'static str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value>>> {
    let received = Arc::new(Mutex::new(None::<Vec<u8>>));
    let sink = received.clone();
    let upstream = serve(Router::new().fallback(any(
        move |_: HeaderMap, body: axum::body::Bytes| async move {
            *sink.lock().unwrap() = Some(body.to_vec());
            "ok"
        },
    )))
    .await;
    let proxy = Proxy::new(&format!("http://{upstream}"))
        .unwrap()
        .with_router(Arc::new(router));
    let proxy_addr = serve(proxy.router()).await;
    move |body: Value, session: &'static str| {
        let received = received.clone();
        Box::pin(async move {
            reqwest::Client::new()
                .post(format!("http://{proxy_addr}/v1/messages"))
                .header("x-claude-code-session-id", session)
                .header("authorization", "Bearer secret-login")
                .body(body.to_string())
                .send()
                .await
                .unwrap();
            let sent = received.lock().unwrap().take().unwrap();
            serde_json::from_slice(&sent).unwrap()
        })
    }
}

fn user_turn(text: &str) -> Value {
    json!({
        "model": "claude-opus-5-5",
        "max_tokens": 100,
        "output_config": { "effort": "xhigh" },
        "messages": [{ "role": "user", "content": text }],
    })
}

const OPUS: &str = "claude-opus-5-5";
const HAIKU: &str = "claude-haiku-5-5";

#[tokio::test]
async fn a_valid_grade_picks_the_tier_and_lowers_effort() {
    let (addr, seen) = ollama(|| async { grade("low", "low") }).await;
    let send = setup(addr).await;

    let sent = send(user_turn("fix the typo in README"), "s1").await;

    assert_eq!(sent["model"], HAIKU);
    // The low tier is configured with effort medium; the classifier asked for less.
    assert_eq!(sent["output_config"]["effort"], "low");
    assert_eq!(seen.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn effort_is_never_raised_above_the_tier() {
    let (addr, _) = ollama(|| async { grade("low", "high") }).await;
    let send = setup(addr).await;

    let sent = send(user_turn("fix the typo"), "s1").await;

    assert_eq!(sent["model"], HAIKU);
    assert_eq!(sent["output_config"]["effort"], "medium");
}

#[tokio::test]
async fn ollama_gets_the_prompt_and_the_message_but_not_the_login() {
    let (addr, seen) = ollama(|| async { grade("medium", "medium") }).await;
    let send = setup(addr).await;

    send(user_turn("add a --verbose flag"), "s1").await;

    let body = seen.body.lock().unwrap().take().unwrap();
    assert_eq!(body["model"], "qwen2.5:3b");
    assert_eq!(body["stream"], false);
    assert_eq!(
        body["format"]["required"],
        json!(["tier", "effort", "confidence"])
    );
    assert_eq!(body["messages"][0]["content"], Prompt::builtin().text());
    let input = body["messages"][1]["content"].as_str().unwrap();
    assert!(
        input.starts_with("message: \"add a --verbose flag\"\n"),
        "{input}"
    );

    let headers = seen.headers.lock().unwrap().take().unwrap();
    assert!(headers.get("authorization").is_none());
    assert!(headers.get("x-claude-code-session-id").is_none());
}

#[tokio::test]
async fn a_continuation_reuses_the_decision_without_asking_again() {
    let (addr, seen) = ollama(|| async { grade("low", "low") }).await;
    let send = setup(addr).await;
    send(user_turn("fix the typo"), "s1").await;

    let continuation = json!({
        "model": "claude-opus-5-5",
        "max_tokens": 100,
        "output_config": { "effort": "xhigh" },
        "messages": [
            { "role": "user", "content": "fix the typo" },
            { "role": "assistant", "content": [{ "type": "tool_use", "id": "t1", "name": "Edit", "input": {} }] },
            { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "done" }] },
        ],
    });
    let sent = send(continuation, "s1").await;

    assert_eq!(sent["model"], HAIKU);
    assert_eq!(seen.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_answer_that_breaks_the_schema_uses_the_default_tier() {
    for content in [
        "not json".to_string(),
        r#"{"tier":"low"}"#.to_string(),
        json!({ "tier": "low", "effort": "max", "confidence": 0.9 }).to_string(),
        json!({ "tier": "low", "effort": "low", "confidence": 7 }).to_string(),
    ] {
        let reply = content.clone();
        let (addr, _) = ollama(move || {
            let reply = reply.clone();
            async move { chat(&reply) }
        })
        .await;
        let send = setup(addr).await;

        let sent = send(user_turn("fix the typo"), "s1").await;

        assert_eq!(sent["model"], OPUS, "{content}");
        assert_eq!(sent["output_config"]["effort"], "xhigh", "{content}");
    }
}

#[tokio::test]
async fn an_unreadable_reply_uses_the_default_tier() {
    let (addr, _) = ollama(|| async { (StatusCode::OK, "<html>".to_string()) }).await;
    let sent = setup(addr).await(user_turn("fix the typo"), "s1").await;
    assert_eq!(sent["model"], OPUS);
}

#[tokio::test]
async fn an_ollama_error_uses_the_default_tier() {
    let (addr, _) =
        ollama(|| async { (StatusCode::NOT_FOUND, "model not found".to_string()) }).await;
    let sent = setup(addr).await(user_turn("fix the typo"), "s1").await;
    assert_eq!(sent["model"], OPUS);
}

#[tokio::test]
async fn ollama_not_running_uses_the_default_tier() {
    // A port that was free a moment ago and has nothing listening.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let sent = setup(closed).await(user_turn("fix the typo"), "s1").await;
    assert_eq!(sent["model"], OPUS);
    assert_eq!(sent["output_config"]["effort"], "xhigh");
}

#[tokio::test]
async fn a_slow_ollama_does_not_hold_the_request_past_the_timeout() {
    let (addr, _) = ollama(|| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        grade("low", "low")
    })
    .await;
    let send = setup(addr).await;

    let started = Instant::now();
    let sent = send(user_turn("fix the typo"), "s1").await;
    let took = started.elapsed();

    assert_eq!(sent["model"], OPUS);
    assert!(took < Duration::from_millis(TIMEOUT_MS + 1000), "{took:?}");
}

#[tokio::test]
async fn warm_up_loads_the_model_and_reports_failures() {
    let loaded = Arc::new(AtomicUsize::new(0));
    let count = loaded.clone();
    let app = Router::new().route(
        "/api/generate",
        post(move |body: axum::body::Bytes| {
            let count = count.clone();
            async move {
                let body: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["model"], "qwen2.5:3b");
                assert_eq!(body["keep_alive"], "30m");
                assert!(body.get("prompt").is_none());
                count.fetch_add(1, Ordering::SeqCst);
                "{}"
            }
        }),
    );
    let addr = serve(app).await;
    let config = Config::from_toml(&format!("[classifier]\nurl = \"http://{addr}\"")).unwrap();
    let router = ClassifierRouter::new(
        &config,
        OllamaClient::new(&config.classifier).unwrap(),
        Prompt::builtin(),
    );
    router.warm_up().await.unwrap();
    assert_eq!(loaded.load(Ordering::SeqCst), 1);

    let missing = serve(Router::new()).await;
    let config = Config::from_toml(&format!("[classifier]\nurl = \"http://{missing}\"")).unwrap();
    let router = ClassifierRouter::new(
        &config,
        OllamaClient::new(&config.classifier).unwrap(),
        Prompt::builtin(),
    );
    assert_eq!(router.warm_up().await, Err(ClassifyError::Status(404)));
}

const RULES: &str = r#"
[rules]
enabled = true

[[rules.rule]]
name = "typos"
tier = "low"
keywords = ["typo"]
"#;

#[tokio::test]
async fn the_rules_decide_when_the_classifier_fails_and_they_are_enabled() {
    let (addr, _) = ollama(|| async { (StatusCode::NOT_FOUND, String::new()) }).await;
    let router = router_with(addr, "", RULES);

    let matched = router.route(&view("fix the typo")).await;
    assert_eq!(matched.model, HAIKU);
    assert_eq!(matched.source, DecisionSource::Fallback);
    assert_eq!(
        matched.reason,
        "classifier fallback (ollama status 404): rule typos"
    );

    let unmatched = router.route(&view("redesign everything")).await;
    assert_eq!(unmatched.model, OPUS);
    assert_eq!(unmatched.source, DecisionSource::Fallback);
    assert_eq!(
        unmatched.reason,
        "classifier fallback (ollama status 404): no rule matched, default tier"
    );
}

#[tokio::test]
async fn the_default_tier_decides_when_the_rules_are_off() {
    let (addr, _) = ollama(|| async { (StatusCode::NOT_FOUND, String::new()) }).await;
    let decision = router_with(addr, "", "").route(&view("fix the typo")).await;

    assert_eq!(decision.model, OPUS);
    assert_eq!(decision.source, DecisionSource::Fallback);
    assert_eq!(
        decision.reason,
        "classifier fallback (ollama status 404): default tier"
    );
}

#[tokio::test]
async fn an_unsure_answer_is_not_used() {
    let unsure = |confidence: f64| {
        let content = json!({ "tier": "low", "effort": "low", "confidence": confidence });
        move || {
            let content = content.to_string();
            async move { chat(&content) }
        }
    };

    let (addr, _) = ollama(unsure(0.3)).await;
    let decision = router_with(addr, "", RULES).route(&view("redesign")).await;
    assert_eq!(decision.source, DecisionSource::Fallback);
    assert_eq!(decision.model, OPUS);
    assert_eq!(
        decision.reason,
        "classifier fallback (confidence 0.30 below 0.50): no rule matched, default tier"
    );

    // Exactly at the threshold is accepted, and the threshold can be changed.
    let (addr, _) = ollama(unsure(0.5)).await;
    let decision = router_with(addr, "", "").route(&view("redesign")).await;
    assert_eq!(decision.source, DecisionSource::Classifier);
    assert_eq!(decision.model, HAIKU);

    let (addr, _) = ollama(unsure(0.5)).await;
    let decision = router_with(addr, "min_confidence = 0.8", "")
        .route(&view("redesign"))
        .await;
    assert_eq!(decision.source, DecisionSource::Fallback);
}

#[tokio::test]
async fn repeated_failures_pause_the_classifier_and_it_comes_back() {
    let healthy = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = healthy.clone();
    let (addr, seen) = ollama(move || {
        let up = flag.load(Ordering::SeqCst);
        async move {
            if up {
                grade("low", "low")
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, String::new())
            }
        }
    })
    .await;
    let router = router_with(addr, "failure_limit = 2\ncooldown_secs = 1", "");
    let calls = || seen.calls.load(Ordering::SeqCst);

    for _ in 0..2 {
        let decision = router.route(&view("fix it")).await;
        assert_eq!(
            decision.reason,
            "classifier fallback (ollama status 500): default tier"
        );
    }
    assert_eq!(calls(), 2);

    // Paused: Ollama is not called and the answer is immediate.
    let started = Instant::now();
    let decision = router.route(&view("fix it")).await;
    assert_eq!(calls(), 2);
    assert_eq!(
        decision.reason,
        "classifier fallback (paused after repeated failures): default tier"
    );
    assert!(started.elapsed() < Duration::from_millis(50));

    // After the cooldown the next call checks whether Ollama is back.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    healthy.store(true, Ordering::SeqCst);
    let decision = router.route(&view("fix it")).await;
    assert_eq!(calls(), 3);
    assert_eq!(decision.source, DecisionSource::Classifier);

    router.route(&view("fix it")).await;
    assert_eq!(calls(), 4);
}

#[tokio::test]
async fn a_failed_check_keeps_the_classifier_paused() {
    let (addr, seen) =
        ollama(|| async { (StatusCode::INTERNAL_SERVER_ERROR, String::new()) }).await;
    let router = router_with(addr, "failure_limit = 1\ncooldown_secs = 1", "");

    router.route(&view("fix it")).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    router.route(&view("fix it")).await;
    assert_eq!(seen.calls.load(Ordering::SeqCst), 2);

    let decision = router.route(&view("fix it")).await;
    assert_eq!(seen.calls.load(Ordering::SeqCst), 2);
    assert!(decision.reason.contains("paused"), "{}", decision.reason);
}

#[tokio::test]
async fn a_slow_ollama_pauses_the_classifier_too() {
    let (addr, seen) = ollama(|| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        grade("low", "low")
    })
    .await;
    let router = router_with(addr, "failure_limit = 1", "");

    let first = router.route(&view("fix it")).await;
    assert_eq!(first.reason, "classifier fallback (timeout): default tier");

    let started = Instant::now();
    router.route(&view("fix it")).await;
    assert!(started.elapsed() < Duration::from_millis(50));
    assert_eq!(seen.calls.load(Ordering::SeqCst), 1);
}
