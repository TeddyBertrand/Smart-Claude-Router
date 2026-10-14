//! Dry run: the whole routing pipeline runs and each decision is logged, but what reaches
//! Anthropic is exactly what Claude Code sent.

mod common;

use std::{
    io,
    sync::{Arc, Mutex},
};

use axum::{routing::any, Router};
use common::serve;
use serde_json::json;
use smart_claude_router::{config::Config, proxy::Proxy, rules::RulesRouter};
use tracing_subscriber::{fmt::MakeWriter, EnvFilter};

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

const CONFIG: &str = r#"
[rules]
enabled = true

[[rules.rule]]
name = "typos"
tier = "low"
keywords = ["typo"]
"#;

fn request(text: &str) -> String {
    json!({
        "model": "claude-opus-5-5",
        "max_tokens": 128000,
        "messages": [{"role": "user", "content": text}],
        "output_config": {"effort": "xhigh"},
    })
    .to_string()
}

/// Sends each body to a proxy with the rules router and returns what the upstream received and
/// what was logged, once with dry run on and once off.
async fn run(dry_run: bool, bodies: &[String]) -> (Vec<Vec<u8>>, String) {
    let logs = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(false)
        .with_env_filter(EnvFilter::new("warn,smart_claude_router=trace"))
        .finish();
    // `#[tokio::test]` runs on one thread, so the thread-local subscriber also sees the servers.
    let _guard = tracing::subscriber::set_default(subscriber);

    let received = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let sink = received.clone();
    let upstream = serve(
        Router::new().fallback(any(move |body: axum::body::Bytes| async move {
            sink.lock().unwrap().push(body.to_vec());
            "ok"
        })),
    )
    .await;
    let config = Config::from_toml(CONFIG).unwrap();
    let proxy = Proxy::new(&format!("http://{upstream}"))
        .unwrap()
        .with_router(Arc::new(RulesRouter::new(&config)))
        .with_dry_run(dry_run);
    let proxy_addr = serve(proxy.router()).await;

    for body in bodies {
        reqwest::Client::new()
            .post(format!("http://{proxy_addr}/v1/messages?beta=true"))
            .header("x-claude-code-session-id", "s1")
            .body(body.clone())
            .send()
            .await
            .unwrap();
    }
    let received = received.lock().unwrap().clone();
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    (received, logs)
}

#[tokio::test]
async fn upstream_requests_are_byte_identical_and_decisions_are_logged() {
    let bodies = [
        request("fix this typo"),
        request("write a parser"),
        request("another typo !high"),
    ];
    let (received, logs) = run(true, &bodies).await;

    let sent: Vec<_> = bodies.iter().map(|b| b.as_bytes().to_vec()).collect();
    assert_eq!(received, sent);

    for expected in [
        "dry run: would route",
        "requested=claude-opus-5-5",
        "would_route=claude-haiku-5-5",
        "effort=medium",
        "reason=rule typos",
        "reason=no rule matched, default tier",
        "reason=override !high",
        "turn=new",
    ] {
        assert!(logs.contains(expected), "missing {expected:?} in:\n{logs}");
    }
    // Nothing was routed, so the request line says so.
    assert!(logs.contains("routed=-"), "{logs}");
    assert!(!logs.contains("routed=claude-haiku"), "{logs}");
    for secret in ["fix this", "write a parser"] {
        assert!(
            !logs.contains(secret),
            "{secret:?} leaked into logs:\n{logs}"
        );
    }
}

#[tokio::test]
async fn the_same_requests_are_rewritten_when_dry_run_is_off() {
    let bodies = [request("fix this typo")];
    let (received, logs) = run(false, &bodies).await;

    assert_ne!(received[0], bodies[0].as_bytes());
    assert!(String::from_utf8_lossy(&received[0]).contains("claude-haiku-5-5"));
    assert!(!logs.contains("dry run"), "{logs}");
    assert!(logs.contains("routed=claude-haiku-5-5"), "{logs}");
}
