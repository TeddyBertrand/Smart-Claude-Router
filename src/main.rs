use std::{
    future::IntoFuture,
    io::{self, IsTerminal},
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use smart_claude_router::{
    classifier::Prompt,
    classifier_router::ClassifierRouter,
    config::{Config, Mode, RouterKind},
    ollama::OllamaClient,
    proxy::Proxy,
    rules::RulesRouter,
};
use tokio::{net::TcpListener, sync::oneshot};
use tracing_subscriber::EnvFilter;

/// How long in-flight answers get to finish after Ctrl+C / SIGTERM.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Local proxy between Claude Code and the Anthropic API.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the proxy in the foreground (Ctrl+C to stop).
    Run(RunArgs),
}

#[derive(Args)]
struct RunArgs {
    /// Config file to use instead of ~/.config/smart-claude-router/config.toml.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Address to listen on, overriding the config file.
    #[arg(long, value_name = "IP:PORT")]
    listen: Option<SocketAddr>,
    /// Dry run: log each routing decision but forward every request unchanged. Overrides
    /// `mode` in the config file.
    #[arg(long)]
    dry_run: bool,
    /// How much to log. Without it, RUST_LOG is used if set, else `info`.
    #[arg(long, value_enum)]
    log_level: Option<LogLevel>,
}

#[derive(Clone, Copy, ValueEnum)]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Run(args) => run(args).await,
    }
}

async fn run(args: RunArgs) -> anyhow::Result<()> {
    init_logging(args.log_level);

    let (mut config, source) = Config::load(args.config.as_deref())?;
    if let Some(listen) = args.listen {
        config.listen = listen;
    }
    if args.dry_run {
        config.mode = Mode::Observe;
    }
    match &source {
        Some(path) => tracing::info!("config: {}", path.display()),
        None => tracing::info!("config: built-in defaults (no config file found)"),
    }
    tracing::debug!(?config, "effective config");

    // Before the port opens: once something can connect, Ctrl+C / SIGTERM must already be caught.
    let shutdown = shutdown_signal();

    let listener = TcpListener::bind(config.listen).await.map_err(|err| {
        let hint = match err.kind() {
            io::ErrorKind::AddrInUse => " (is another smart-claude-router already running?)",
            _ => "",
        };
        anyhow::Error::new(err).context(format!("cannot listen on {}{hint}", config.listen))
    })?;
    tracing::info!("proxying http://{} -> {}", config.listen, config.upstream);

    let (stop, stopped) = oneshot::channel::<()>();
    let observe = config.mode == Mode::Observe;
    let mut proxy = Proxy::new(&config.upstream)?.with_dry_run(observe);
    let routing = match config.router {
        RouterKind::Classifier => {
            let prompt = Prompt::load(&config.classifier)?;
            let summary = format!(
                "classifier router (model {} at {}, prompt {}), fallback: {}",
                config.classifier.model,
                config.classifier.url,
                prompt.version(),
                if config.rules.enabled {
                    "rules router"
                } else {
                    "default tier"
                }
            );
            if !is_local(&config.classifier.url) {
                tracing::warn!(
                    "classifier.url is not on this machine: the text of your messages is sent to {}",
                    config.classifier.url
                );
            }
            let client = OllamaClient::new(&config.classifier)?;
            let router = Arc::new(ClassifierRouter::new(&config, client, prompt));
            let warming = router.clone();
            tokio::spawn(async move {
                match warming.warm_up().await {
                    Ok(()) => tracing::info!("classifier model loaded"),
                    Err(err) => tracing::warn!(
                        cause = %err.label(),
                        "cannot load the classifier model; messages use the default tier until Ollama answers"
                    ),
                }
            });
            proxy = proxy.with_router(router);
            Some(summary)
        }
        RouterKind::Rules if config.rules.enabled => {
            proxy = proxy.with_router(Arc::new(RulesRouter::new(&config)));
            Some(format!("rules router ({} rules)", config.rules.rule.len()))
        }
        RouterKind::Rules => None,
    };
    match (observe, routing) {
        (false, None) => tracing::info!("mode: route, but rules are off: requests are forwarded unchanged"),
        (false, Some(routing)) => tracing::info!("mode: route, {routing}"),
        (true, Some(routing)) => tracing::info!(
            "mode: observe (dry run), {routing}: decisions are logged, requests are forwarded unchanged"
        ),
        (true, None) => tracing::warn!(
            "mode: observe (dry run), but rules are off ([rules] enabled = false): there is nothing to observe"
        ),
    }
    let server = axum::serve(listener, proxy.router()).with_graceful_shutdown(async {
        let _ = stopped.await;
    });
    let mut server = std::pin::pin!(server.into_future());

    tokio::select! {
        result = &mut server => return result.context("server stopped"),
        () = shutdown => {}
    }
    tracing::info!(
        "shutting down, waiting up to {}s for answers in progress",
        SHUTDOWN_GRACE.as_secs()
    );
    let _ = stop.send(());
    match tokio::time::timeout(SHUTDOWN_GRACE, server).await {
        Ok(result) => result.context("server stopped")?,
        Err(_) => tracing::warn!("answers still in progress after the grace period; closing them"),
    }
    tracing::info!("stopped");
    Ok(())
}

/// Whether `url` points at this machine. The config is validated, so it parses.
fn is_local(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]"))
}

/// Logs go to stderr (the journal under systemd), colored only on a terminal. Our own crate logs
/// at the chosen level; dependencies only at `warn` unless RUST_LOG says otherwise.
fn init_logging(level: Option<LogLevel>) {
    let filter = match level {
        Some(level) => {
            let level = match level {
                LogLevel::Error => "error",
                LogLevel::Warn => "warn",
                LogLevel::Info => "info",
                LogLevel::Debug => "debug",
                LogLevel::Trace => "trace",
            };
            EnvFilter::new(format!("warn,smart_claude_router={level}"))
        }
        None => EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("warn,smart_claude_router=info")),
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .init();
}

/// Resolves on Ctrl+C or SIGTERM. The handlers are installed when this is called, not when the
/// future is first polled, so a signal that arrives right after the port opens is not lost.
#[cfg(unix)]
fn shutdown_signal() -> impl std::future::Future<Output = ()> {
    use tokio::signal::unix::{signal, SignalKind};
    let interrupt = signal(SignalKind::interrupt());
    let terminate = signal(SignalKind::terminate());
    async move {
        match (interrupt, terminate) {
            (Ok(mut interrupt), Ok(mut terminate)) => tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
            },
            _ => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
}

#[cfg(not(unix))]
fn shutdown_signal() -> impl std::future::Future<Output = ()> {
    async {
        let _ = tokio::signal::ctrl_c().await;
    }
}
