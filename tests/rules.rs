//! The rules router behind the proxy: what Claude Code sends, and what reaches Anthropic.

mod common;

use std::sync::{Arc, Mutex};

use axum::{http::HeaderMap, routing::any, Router};
use common::serve;
use serde_json::{json, Value};
use smart_claude_router::{config::Config, proxy::Proxy, rules::RulesRouter};

const CONFIG: &str = r#"
[rules]
enabled = true

[[rules.rule]]
name = "typos"
tier = "low"
keywords = ["typo"]

[[rules.rule]]
name = "long-conversation"
tier = "high"
min_messages = 5
"#;

/// A proxy with the rules router in front of a mock upstream. `send` posts one `/v1/messages`
/// request and returns the JSON body the upstream received.
async fn setup(
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
    let config = Config::from_toml(CONFIG).unwrap();
    let proxy = Proxy::new(&format!("http://{upstream}"))
        .unwrap()
        .with_router(Arc::new(RulesRouter::new(&config)));
    let proxy_addr = serve(proxy.router()).await;
    move |body: Value, session: &'static str| {
        let received = received.clone();
        Box::pin(async move {
            reqwest::Client::new()
                .post(format!("http://{proxy_addr}/v1/messages?beta=true"))
                .header("x-claude-code-session-id", session)
                .body(body.to_string())
                .send()
                .await
                .unwrap();
            let sent = received.lock().unwrap().take().unwrap();
            serde_json::from_slice(&sent).unwrap()
        })
    }
}

fn request(text: &str, extra_messages: usize) -> Value {
    let mut messages = vec![json!({"role": "user", "content": text})];
    for _ in 0..extra_messages {
        messages.push(json!({"role": "assistant", "content": "ok"}));
        messages.push(json!({"role": "user", "content": text}));
    }
    json!({
        "model": "claude-opus-5-5",
        "max_tokens": 128000,
        "messages": messages,
        "output_config": {"effort": "xhigh"},
    })
}

#[tokio::test]
async fn a_matching_rule_changes_model_and_effort() {
    let send = setup().await;
    let sent = send(request("fix this typo", 0), "s1").await;
    assert_eq!(sent["model"], "claude-haiku-5-5");
    assert_eq!(sent["output_config"]["effort"], "medium");
    assert_eq!(sent["max_tokens"], 128000);
}

#[tokio::test]
async fn no_match_uses_the_default_tier_which_is_what_claude_code_uses() {
    let send = setup().await;
    let sent = send(request("write a parser", 0), "s2").await;
    assert_eq!(sent["model"], "claude-opus-5-5");
    assert_eq!(sent["output_config"]["effort"], "xhigh");
}

#[tokio::test]
async fn override_markers_win_over_rules() {
    let send = setup().await;
    let sent = send(request("fix this typo !high", 0), "s3").await;
    assert_eq!(sent["model"], "claude-opus-5-5");
    let sent = send(request("write a parser !medium", 0), "s3").await;
    assert_eq!(sent["model"], "claude-sonnet-5-5");
    assert_eq!(sent["output_config"]["effort"], "high");
    // The marker stays in the message: only the model and effort are edited.
    assert_eq!(sent["messages"][0]["content"], "write a parser !medium");
}

#[tokio::test]
async fn conversation_length_counts_all_messages() {
    let send = setup().await;
    let sent = send(request("fix this typo", 3), "s4").await;
    // The typo rule comes first and matches before the length rule.
    assert_eq!(sent["model"], "claude-haiku-5-5");
    let sent = send(request("hello", 3), "s4").await;
    assert_eq!(sent["model"], "claude-opus-5-5");
}
