use anyhow::Context;
use smart_claude_router::proxy::Proxy;

/// Hard-coded until the config file lands (#9).
const LISTEN: &str = "127.0.0.1:8899";
const UPSTREAM: &str = "https://api.anthropic.com";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let listener = tokio::net::TcpListener::bind(LISTEN)
        .await
        .with_context(|| format!("binding {LISTEN}"))?;
    tracing::info!("proxying http://{LISTEN} -> {UPSTREAM}");
    axum::serve(listener, Proxy::new(UPSTREAM)?.router()).await?;
    Ok(())
}
