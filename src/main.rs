use anyhow::Context;
use smart_claude_router::{config::Config, proxy::Proxy};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // An explicit path arrives with the `--config` flag (#10).
    let (config, source) = Config::load(None)?;
    match &source {
        Some(path) => tracing::info!("config: {}", path.display()),
        None => tracing::info!("config: built-in defaults (no config file found)"),
    }

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding {}", config.listen))?;
    tracing::info!("proxying http://{} -> {}", config.listen, config.upstream);
    axum::serve(listener, Proxy::new(&config.upstream)?.router()).await?;
    Ok(())
}
