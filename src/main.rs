use std::{
    future::IntoFuture,
    io::{self, IsTerminal},
    net::SocketAddr,
    path::PathBuf,
    time::Duration,
};

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use smart_claude_router::{config::Config, proxy::Proxy};
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
    match &source {
        Some(path) => tracing::info!("config: {}", path.display()),
        None => tracing::info!("config: built-in defaults (no config file found)"),
    }
    tracing::debug!(?config, "effective config");

    let listener = TcpListener::bind(config.listen).await.map_err(|err| {
        let hint = match err.kind() {
            io::ErrorKind::AddrInUse => " (is another smart-claude-router already running?)",
            _ => "",
        };
        anyhow::Error::new(err).context(format!("cannot listen on {}{hint}", config.listen))
    })?;
    tracing::info!("proxying http://{} -> {}", config.listen, config.upstream);

    let (stop, stopped) = oneshot::channel::<()>();
    let server = axum::serve(listener, Proxy::new(&config.upstream)?.router())
        .with_graceful_shutdown(async {
            let _ = stopped.await;
        });
    let mut server = std::pin::pin!(server.into_future());

    tokio::select! {
        result = &mut server => return result.context("server stopped"),
        () = shutdown_signal() => {}
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

async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}
