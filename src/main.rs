mod cli;
mod engine;
mod handler;
mod request;
mod response;
mod state;

use crate::cli::Args;
use crate::handler::handler;
use crate::state::AppState;
use axum::{Router, routing::any};
use clap::Parser;
use miette::{IntoDiagnostic, Result};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "mq_http=debug,tower_http=debug".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Args::parse();
    let port = std::env::var("MQ_HTTP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(args.port);
    let addr = std::env::var("MQ_HTTP_ADDR").unwrap_or_else(|_| args.addr.clone());

    let content = load_script_content(&args)?;
    let script_content = Arc::new(RwLock::new(Some(content)));

    let state = Arc::new(AppState {
        args: args.clone(),
        script_content,
    });

    if args.reload {
        if let Some(script_path) = &args.script {
            start_file_watcher(script_path.clone(), state.clone());
        } else {
            tracing::warn!("--reload requires a script file, not -c");
        }
    }

    if args.reload {
        tracing::warn!("--reload requires `--features watch`");
    }

    let app = Router::new()
        .route("/", any(handler))
        .route("/{*path}", any(handler))
        .layer(TraceLayer::new_for_http())
        .layer(RequestBodyLimitLayer::new(1024 * 1024))
        .with_state(state);

    let socket_addr: SocketAddr = format!("{}:{}", addr, port).parse().into_diagnostic()?;
    tracing::info!("listening on {}", socket_addr);

    let listener = tokio::net::TcpListener::bind(socket_addr)
        .await
        .into_diagnostic()?;

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .into_diagnostic()?;

    Ok(())
}

fn load_script_content(args: &Args) -> Result<String> {
    if let Some(command) = &args.command {
        Ok(command.clone())
    } else if let Some(script_path) = &args.script {
        std::fs::read_to_string(script_path).into_diagnostic()
    } else {
        Err(miette::miette!(
            "No script provided. Use a script file path or -c 'script'"
        ))
    }
}

fn start_file_watcher(path: std::path::PathBuf, state: Arc<AppState>) {
    use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    std::thread::spawn(move || {
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();

        let mut watcher = match RecommendedWatcher::new(tx, Config::default()) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("Failed to create file watcher: {:?}", e);
                return;
            }
        };

        if let Err(e) = watcher.watch(&path, RecursiveMode::NonRecursive) {
            tracing::error!("Failed to watch {:?}: {:?}", path, e);
            return;
        }

        tracing::info!("Watching {:?} for changes", path);
        let mut last_reload = Instant::now() - Duration::from_secs(10);

        for result in rx {
            match result {
                Ok(event) if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) => {
                    // Debounce: ignore bursts within 200ms.
                    if last_reload.elapsed() < Duration::from_millis(200) {
                        continue;
                    }
                    last_reload = Instant::now();

                    // Small delay to let the write complete.
                    std::thread::sleep(Duration::from_millis(50));

                    match std::fs::read_to_string(&path) {
                        Ok(content) => {
                            *state.script_content.write().unwrap() = Some(content);
                            tracing::info!("Script reloaded: {:?}", path);
                        }
                        Err(e) => tracing::error!("Failed to read script: {:?}", e),
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!("Watch error: {:?}", e);
                    break;
                }
            }
        }
    });
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("Shutdown signal received, shutting down gracefully...");
}
