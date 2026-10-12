//! The request log line carries metadata only: never auth headers, the query or body content.

mod common;

use std::{
    io,
    sync::{Arc, Mutex},
};

use axum::Router;
use common::serve;
use smart_claude_router::proxy::Proxy;
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

// `#[tokio::test]` runs on one thread, so the thread-local subscriber also sees the servers.
#[tokio::test]
async fn request_log_has_metadata_but_no_secrets() {
    let logs = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(false)
        // The most verbose setting `run --log-level` allows.
        .with_env_filter(EnvFilter::new("warn,smart_claude_router=trace"))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    async fn upstream() -> &'static str {
        "{}"
    }
    let upstream = serve(Router::new().fallback(upstream)).await;
    let proxy = serve(Proxy::new(&format!("http://{upstream}")).unwrap().router()).await;

    reqwest::Client::new()
        .post(format!("http://{proxy}/v1/messages?beta=true"))
        .header("authorization", "Bearer sk-ant-oat01-SECRET-TOKEN")
        .header("x-api-key", "sk-ant-api03-SECRET-KEY")
        .body(
            r#"{"model":"claude-opus-5-5","messages":[{"role":"user","content":"SECRET PROMPT"}]}"#,
        )
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();

    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    for expected in [
        "method=POST",
        "path=/v1/messages",
        "status=200",
        "model=claude-opus-5-5",
    ] {
        assert!(
            logs.contains(expected),
            "missing {expected:?} in logs:\n{logs}"
        );
    }
    for secret in ["SECRET", "sk-ant", "Bearer", "beta=true"] {
        assert!(
            !logs.contains(secret),
            "{secret:?} leaked into logs:\n{logs}"
        );
    }
}
