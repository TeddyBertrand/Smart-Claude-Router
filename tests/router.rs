//! `RequestView` against the captured request shapes (`tests/fixtures/requests/`), and the
//! proxy's call into the router.

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    http::{HeaderMap, HeaderName, HeaderValue},
    routing::any,
    Router,
};
use serde_json::{json, Value};
use smart_claude_router::{
    config::Effort,
    proxy::Proxy,
    router::{
        background_kind, BackgroundKind, BoxFuture, DecisionSource, PassthroughRouter, RequestView,
        Router as RouteDecider, RoutingDecision,
    },
};
use tokio::net::TcpListener;

struct Fixture {
    name: String,
    kind: String,
    turn: Option<String>,
    headers: HeaderMap,
    body: Vec<u8>,
}

fn fixtures() -> Vec<Fixture> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/requests");
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let json: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let mut headers = HeaderMap::new();
        for (name, value) in json["headers"].as_object().unwrap() {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value.as_str().unwrap()).unwrap(),
            );
        }
        out.push(Fixture {
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            kind: json["expect"]["kind"].as_str().unwrap().to_owned(),
            turn: json["expect"]["turn"].as_str().map(str::to_owned),
            headers,
            body: serde_json::to_vec(&json["body"]).unwrap(),
        });
    }
    assert!(!out.is_empty());
    out
}

fn view(fixture: &Fixture) -> RequestView {
    RequestView::parse(&fixture.headers, &fixture.body)
        .unwrap_or_else(|| panic!("{} should parse", fixture.name))
}

#[test]
fn every_fixture_parses_with_model_session_and_size() {
    for fixture in fixtures() {
        let view = view(&fixture);
        assert!(view.model.starts_with("claude-"), "{}", fixture.name);
        assert_eq!(
            view.session_id.as_deref(),
            Some("<uuid>"),
            "{}",
            fixture.name
        );
        assert_eq!(view.body_bytes, fixture.body.len(), "{}", fixture.name);
        assert!(view.message_count >= 1, "{}", fixture.name);
    }
}

#[test]
fn new_turns_have_user_text_and_continuations_do_not() {
    for fixture in fixtures() {
        let view = view(&fixture);
        match fixture.turn.as_deref() {
            Some("new") => assert!(!view.last_user_text.is_empty(), "{}", fixture.name),
            Some("continuation") => {
                assert!(view.last_user_text.is_empty(), "{}", fixture.name);
                assert!(view.has_tool_use, "{}", fixture.name);
            }
            _ => {}
        }
    }
}

#[test]
fn agent_id_is_set_on_subagent_requests_only() {
    for fixture in fixtures() {
        let view = view(&fixture);
        assert_eq!(
            view.agent_id.is_some(),
            fixture.kind == "subagent",
            "{}",
            fixture.name
        );
    }
}

#[test]
fn background_requests_have_their_signatures() {
    for fixture in fixtures() {
        let view = view(&fixture);
        match fixture.kind.as_str() {
            "background_quota_probe" => {
                assert_eq!(view.max_tokens, Some(1));
                assert_eq!(view.message_count, 1);
                assert_eq!(view.effort, None);
                assert!(!view.has_output_format);
            }
            "background_title" => {
                assert!(view.has_output_format);
                assert_eq!(view.effort, Some(Effort::Medium));
                assert_eq!(view.tool_count, 0);
            }
            _ => assert!(!view.has_output_format, "{}", fixture.name),
        }
    }
}

#[test]
fn main_thread_requests_carry_effort_and_tools() {
    for fixture in fixtures().iter().filter(|f| f.kind == "user_turn") {
        let view = view(fixture);
        assert!(view.effort.is_some(), "{}", fixture.name);
        assert!(view.tool_count > 0 || fixture.name.contains("continuation"));
    }
}

#[test]
fn counts_images_and_reads_string_content() {
    let body = json!({
        "model": "claude-haiku-5-5",
        "messages": [
            {"role": "user", "content": [
                {"type": "image", "source": {"type": "base64"}},
                {"type": "text", "text": "what is this"},
                {"type": "text", "text": "and this"},
            ]},
            {"role": "assistant", "content": "plain"},
            {"role": "user", "content": "last words"},
            {"role": "system", "content": "reminder"},
        ],
        "output_config": {"effort": "some-future-value"},
    });
    let view = RequestView::parse(&HeaderMap::new(), body.to_string().as_bytes()).unwrap();
    assert_eq!(view.image_count, 1);
    assert_eq!(view.message_count, 4);
    assert_eq!(view.last_user_text, "last words");
    assert_eq!(view.effort, None);
    assert_eq!(view.session_id, None);
}

#[test]
fn unusable_bodies_give_no_view() {
    let parse = |body: &[u8]| RequestView::parse(&HeaderMap::new(), body);
    assert!(parse(b"").is_none());
    assert!(parse(b"not json").is_none());
    assert!(parse(b"[1,2]").is_none());
    assert!(parse(br#"{"messages":[]}"#).is_none());
    assert!(parse(br#"{"model":"m","messages":"nope"}"#).is_none());
}

#[test]
fn debug_output_hides_prompt_text() {
    let body = json!({
        "model": "claude-haiku-5-5",
        "messages": [{"role": "user", "content": "SECRET PROMPT"}],
    });
    let view = RequestView::parse(&HeaderMap::new(), body.to_string().as_bytes()).unwrap();
    assert!(!format!("{view:?}").contains("SECRET"));
}

#[tokio::test]
async fn passthrough_router_keeps_model_and_effort() {
    let body = json!({
        "model": "claude-opus-5-5",
        "messages": [{"role": "user", "content": "hi"}],
        "output_config": {"effort": "xhigh"},
    });
    let view = RequestView::parse(&HeaderMap::new(), body.to_string().as_bytes()).unwrap();
    let decision = PassthroughRouter.decide(&view).await;
    assert_eq!(decision.model, "claude-opus-5-5");
    assert_eq!(decision.effort, Some(Effort::Xhigh));
    assert_eq!(decision.source, DecisionSource::Passthrough);
}

/// Records the model of every request it is asked about.
struct Recorder(Arc<Mutex<Vec<String>>>);

impl RouteDecider for Recorder {
    fn decide<'a>(&'a self, req: &'a RequestView) -> BoxFuture<'a, RoutingDecision> {
        Box::pin(async move {
            self.0.lock().unwrap().push(req.model.clone());
            RoutingDecision {
                model: "claude-haiku-5-5".into(),
                effort: None,
                reason: "test".into(),
                source: DecisionSource::Rules,
            }
        })
    }
}

async fn serve(router: Router) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

#[tokio::test]
async fn proxy_asks_the_router_only_for_messages_and_forwards_everything_else_unchanged() {
    let received = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let sink = received.clone();
    let upstream = serve(
        Router::new().fallback(any(move |body: axum::body::Bytes| async move {
            sink.lock().unwrap().push(body.to_vec());
            "ok"
        })),
    )
    .await;

    let asked = Arc::new(Mutex::new(Vec::new()));
    let proxy = Proxy::new(&format!("http://{upstream}"))
        .unwrap()
        .with_router(Arc::new(Recorder(asked.clone())));
    let proxy_addr = serve(proxy.router()).await;

    let body = r#"{"model":"claude-opus-5-5","messages":[{"role":"user","content":"hi"}]}"#;
    let client = reqwest::Client::new();
    for path in ["/v1/messages?beta=true", "/v1/messages/count_tokens"] {
        client
            .post(format!("http://{proxy_addr}{path}"))
            .body(body)
            .send()
            .await
            .unwrap();
    }
    client
        .get(format!("http://{proxy_addr}/v1/messages"))
        .send()
        .await
        .unwrap();
    client
        .post(format!("http://{proxy_addr}/v1/messages"))
        .body("not json")
        .send()
        .await
        .unwrap();

    assert_eq!(*asked.lock().unwrap(), ["claude-opus-5-5"]);
    let received = received.lock().unwrap();
    // The recorder routes to Haiku, so only the model changes in the one routed request.
    assert_eq!(
        received[0],
        body.replace("claude-opus-5-5", "claude-haiku-5-5")
            .as_bytes()
    );
    assert_eq!(received[1], body.as_bytes());
    assert_eq!(received.len(), 4);
}

/// Always answers with the same decision.
struct Fixed(RoutingDecision);

impl RouteDecider for Fixed {
    fn decide<'a>(&'a self, _: &'a RequestView) -> BoxFuture<'a, RoutingDecision> {
        Box::pin(async move { self.0.clone() })
    }
}

/// Sends `body` to `/v1/messages` through a proxy whose router always gives `decision`. Returns
/// what the upstream received: its body and its `content-length`.
async fn send_routed(body: Vec<u8>, decision: RoutingDecision) -> (Vec<u8>, Option<usize>) {
    let received = Arc::new(Mutex::new(None));
    let sink = received.clone();
    let upstream = serve(Router::new().fallback(any(
        move |headers: HeaderMap, body: axum::body::Bytes| async move {
            let length = headers
                .get("content-length")
                .and_then(|v| v.to_str().ok()?.parse().ok());
            *sink.lock().unwrap() = Some((body.to_vec(), length));
            "ok"
        },
    )))
    .await;
    let proxy = Proxy::new(&format!("http://{upstream}"))
        .unwrap()
        .with_router(Arc::new(Fixed(decision)));
    let proxy_addr = serve(proxy.router()).await;
    reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/messages?beta=true"))
        .body(body)
        .send()
        .await
        .unwrap();
    let received = received.lock().unwrap().take().unwrap();
    received
}

#[tokio::test]
async fn routed_requests_differ_only_in_model_and_effort() {
    let decision = RoutingDecision {
        model: "claude-haiku-5-5".into(),
        effort: Some(Effort::Low),
        reason: "test".into(),
        source: DecisionSource::Rules,
    };
    for fixture in fixtures() {
        let (sent, length) = send_routed(fixture.body.clone(), decision.clone()).await;
        assert_eq!(length, Some(sent.len()), "{}", fixture.name);
        if fixture.kind.starts_with("background_") {
            assert_eq!(sent, fixture.body, "{} is background", fixture.name);
            continue;
        }

        let mut expected: Value = serde_json::from_slice(&fixture.body).unwrap();
        expected["model"] = json!("claude-haiku-5-5");
        if expected["output_config"]["effort"].is_string() {
            expected["output_config"]["effort"] = json!("low");
        }
        let last_effort = expected["messages"].as_array_mut().and_then(|messages| {
            messages
                .iter_mut()
                .rev()
                .find(|m| m["role"] == "system" && m["output_config"]["effort"].is_string())
        });
        if let Some(message) = last_effort {
            message["output_config"]["effort"] = json!("low");
        }
        let actual: Value = serde_json::from_slice(&sent).unwrap();
        assert_eq!(actual, expected, "{}", fixture.name);
    }
}

#[tokio::test]
async fn routing_to_the_same_model_and_effort_forwards_the_original_bytes() {
    let body = br#"{"model":"claude-opus-5-5",  "max_tokens":1e3,
 "output_config":{"effort":"xhigh"}, "messages":[{"role":"user","content":"hi"}]}"#
        .to_vec();
    let decision = RoutingDecision {
        model: "claude-opus-5-5".into(),
        effort: Some(Effort::Xhigh),
        reason: "test".into(),
        source: DecisionSource::Rules,
    };
    let (sent, length) = send_routed(body.clone(), decision).await;
    assert_eq!(sent, body);
    assert_eq!(length, Some(body.len()));
}

#[tokio::test]
async fn unexpected_body_shapes_are_forwarded_unchanged() {
    let decision = RoutingDecision {
        model: "claude-haiku-5-5".into(),
        effort: Some(Effort::Low),
        reason: "test".into(),
        source: DecisionSource::Rules,
    };
    // `output_config` is not an object: the request cannot be routed and goes through as is.
    let body = br#"{"model":"claude-opus-5-5","messages":[],"output_config":"odd"}"#.to_vec();
    let (sent, _) = send_routed(body.clone(), decision).await;
    assert_eq!(sent, body);
}

#[test]
fn only_the_background_fixtures_are_background() {
    for fixture in fixtures() {
        let expected = match fixture.kind.as_str() {
            "background_quota_probe" => Some(BackgroundKind::QuotaProbe),
            "background_title" => Some(BackgroundKind::TitleGeneration),
            _ => None,
        };
        assert_eq!(
            background_kind(&view(&fixture)),
            expected,
            "{}",
            fixture.name
        );
    }
}

#[tokio::test]
async fn background_requests_skip_the_router_and_arrive_byte_identical() {
    let received = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let sink = received.clone();
    let upstream = serve(
        Router::new().fallback(any(move |body: axum::body::Bytes| async move {
            sink.lock().unwrap().push(body.to_vec());
            "ok"
        })),
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let proxy = Proxy::new(&format!("http://{upstream}"))
        .unwrap()
        .with_router(Arc::new(Recorder(asked.clone())));
    let proxy_addr = serve(proxy.router()).await;

    let background: Vec<_> = fixtures()
        .into_iter()
        .filter(|f| f.kind.starts_with("background_"))
        .collect();
    assert_eq!(background.len(), 2);
    let client = reqwest::Client::new();
    for fixture in &background {
        client
            .post(format!("http://{proxy_addr}/v1/messages?beta=true"))
            .body(fixture.body.clone())
            .send()
            .await
            .unwrap();
    }

    assert!(asked.lock().unwrap().is_empty());
    let sent: Vec<_> = background.into_iter().map(|f| f.body).collect();
    assert_eq!(*received.lock().unwrap(), sent);
}
