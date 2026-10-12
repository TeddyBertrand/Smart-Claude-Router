//! Proxy against a mock upstream: what Claude Code sends must arrive unchanged, and what the
//! upstream answers must come back unchanged.

mod common;

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    body::{to_bytes, Body, Bytes},
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    Router,
};
use common::serve;
use smart_claude_router::proxy::Proxy;
use tokio::net::TcpListener;

/// Stand-in for gzip-encoded SSE: arbitrary bytes the proxy must not decode or alter.
const OPAQUE_BODY: &[u8] = b"\x1f\x8b\x08\x00not-really-gzip\xff\x00";

#[derive(Default)]
struct Seen {
    method: String,
    path_and_query: String,
    headers: HeaderMap,
    body: Bytes,
}

type Shared = Arc<Mutex<Option<Seen>>>;

async fn upstream_handler(State(seen): State<Shared>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let path = parts.uri.path().to_owned();
    *seen.lock().unwrap() = Some(Seen {
        method: parts.method.to_string(),
        path_and_query: parts.uri.path_and_query().unwrap().to_string(),
        headers: parts.headers,
        body: to_bytes(body, usize::MAX).await.unwrap(),
    });

    let builder = Response::builder();
    match path.as_str() {
        "/rate-limited" => builder
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header("x-should-retry", "true")
            .body(Body::from("{\"type\":\"error\"}")),
        "/redirect" => builder
            .status(StatusCode::TEMPORARY_REDIRECT)
            .header("location", "/elsewhere")
            .body(Body::empty()),
        _ => builder
            .header("content-type", "text/event-stream; charset=utf-8")
            .header("content-encoding", "gzip")
            .header("anthropic-ratelimit-unified-5h-utilization", "0.17")
            .body(Body::from(OPAQUE_BODY)),
    }
    .unwrap()
}

/// Starts mock upstream + proxy; returns (proxy base URL, upstream addr, what upstream saw).
async fn setup() -> (String, SocketAddr, Shared) {
    let seen = Shared::default();
    let upstream = serve(
        Router::new()
            .fallback(upstream_handler)
            .with_state(seen.clone()),
    )
    .await;
    let proxy = Proxy::new(&format!("http://{upstream}/")).unwrap();
    let proxy_addr = serve(proxy.router()).await;
    (format!("http://{proxy_addr}"), upstream, seen)
}

fn take(seen: &Shared) -> Seen {
    seen.lock()
        .unwrap()
        .take()
        .expect("upstream received no request")
}

#[tokio::test]
async fn messages_request_arrives_unchanged_and_response_comes_back_unchanged() {
    let (proxy, upstream, seen) = setup().await;
    let body = r#"{"model":"claude-opus-5-5","stream":true,"messages":[]}"#;

    let resp = reqwest::Client::new()
        .post(format!("{proxy}/v1/messages?beta=true"))
        .header("authorization", "Bearer test-token")
        .header("anthropic-beta", "oauth-2025-04-20,effort-2025-11-24")
        .header("anthropic-version", "2023-06-01")
        .header("x-claude-code-session-id", "session-1")
        .header("content-type", "application/json")
        .header("keep-alive", "timeout=5")
        .header("proxy-authorization", "Basic not-for-upstream")
        .body(body)
        .send()
        .await
        .unwrap();

    let got = take(&seen);
    assert_eq!(got.method, "POST");
    assert_eq!(got.path_and_query, "/v1/messages?beta=true");
    assert_eq!(got.body, body.as_bytes());
    for (name, value) in [
        ("authorization", "Bearer test-token"),
        ("anthropic-beta", "oauth-2025-04-20,effort-2025-11-24"),
        ("anthropic-version", "2023-06-01"),
        ("x-claude-code-session-id", "session-1"),
        ("content-type", "application/json"),
        ("content-length", &body.len().to_string()),
    ] {
        assert_eq!(got.headers[name], value, "header {name}");
    }
    assert_eq!(got.headers["host"], upstream.to_string());
    assert!(!got.headers.contains_key("keep-alive"));
    assert!(!got.headers.contains_key("proxy-authorization"));

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-encoding"], "gzip");
    assert_eq!(
        resp.headers()["anthropic-ratelimit-unified-5h-utilization"],
        "0.17"
    );
    assert_eq!(resp.bytes().await.unwrap(), OPAQUE_BODY);
}

#[tokio::test]
async fn any_method_and_path_is_forwarded() {
    let (proxy, _, seen) = setup().await;
    let client = reqwest::Client::new();

    let resp = client
        .head(format!("{proxy}/api/hello"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let got = take(&seen);
    assert_eq!(
        (got.method.as_str(), got.path_and_query.as_str()),
        ("HEAD", "/api/hello")
    );
    assert!(got.body.is_empty());

    client
        .get(format!("{proxy}/v1/models?limit=5"))
        .send()
        .await
        .unwrap();
    let got = take(&seen);
    assert_eq!(
        (got.method.as_str(), got.path_and_query.as_str()),
        ("GET", "/v1/models?limit=5")
    );
}

#[tokio::test]
async fn upstream_status_headers_and_redirects_pass_through() {
    let (proxy, _, _) = setup().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let resp = client
        .post(format!("{proxy}/rate-limited"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(resp.headers()["x-should-retry"], "true");

    let resp = client
        .get(format!("{proxy}/redirect"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(resp.headers()["location"], "/elsewhere");
}

#[tokio::test]
async fn unreachable_upstream_returns_bad_gateway() {
    // Grab a free port, then close it so nothing listens there.
    let dead = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let proxy = serve(Proxy::new(&format!("http://{dead}")).unwrap().router()).await;

    let resp = reqwest::get(format!("http://{proxy}/v1/messages"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}
