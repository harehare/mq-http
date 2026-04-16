mod cli;
mod engine;
mod handler;
mod middleware;
mod openapi;
mod request;
mod response;
mod state;

use crate::cli::Args;
use crate::handler::handler;
use crate::middleware::RateLimiter;
use crate::state::AppState;
use axum::{
    Router,
    extract::State,
    http::{HeaderValue, header},
    middleware as axum_middleware,
    response::{Html, IntoResponse, Response},
    routing::{any, get},
};
use clap::Parser;
use miette::{IntoDiagnostic, Result};
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

async fn swagger_ui_handler(State(state): State<Arc<AppState>>) -> Html<String> {
    Html(openapi::swagger_ui_html(&state.args.docs_title))
}

async fn openapi_json_handler(State(state): State<Arc<AppState>>) -> Response {
    let script = match state.script_content.read().unwrap().clone() {
        Some(s) => s,
        None => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Script not loaded",
            )
                .into_response();
        }
    };
    let routes = openapi::parse_script(&script);
    let spec = openapi::build_openapi_json(
        &state.args.docs_title,
        &state.args.docs_version,
        &routes,
    );
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(spec.to_string()))
        .unwrap_or_default()
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if args.tls_cert.is_some() != args.tls_key.is_some() {
        return Err(miette::miette!(
            "Both --tls-cert and --tls-key must be provided together"
        ));
    }

    let (otel_layer, otel_provider) = if let Some(endpoint) = &args.otel_endpoint {
        match init_tracer(endpoint, &args.otel_service_name) {
            Ok(provider) => {
                use opentelemetry::trace::TracerProvider as _;
                let tracer = provider.tracer("mq-http");
                let layer = tracing_opentelemetry::layer().with_tracer(tracer);
                (Some(layer), Some(provider))
            }
            Err(e) => {
                eprintln!("Failed to initialize OpenTelemetry tracer: {:?}", e);
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "mq_http=debug,tower_http=debug".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .with(otel_layer)
        .init();

    let port = std::env::var("MQ_HTTP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(args.port);
    let addr = std::env::var("MQ_HTTP_ADDR").unwrap_or_else(|_| args.addr.clone());

    let content = load_script_content(&args)?;
    let script_content = Arc::new(RwLock::new(Some(content)));

    let rate_limiter = args.rate_limit.map(RateLimiter::new);

    let state = Arc::new(AppState {
        args: args.clone(),
        script_content,
        rate_limiter,
    });

    if args.reload {
        if let Some(script_path) = &args.script {
            start_file_watcher(script_path.clone(), state.clone());
        } else {
            tracing::warn!("--reload requires a script file, not -c");
        }
    }

    let base = Router::new()
        .route("/", any(handler))
        .route("/{*path}", any(handler));

    let app = if args.docs {
        tracing::info!(
            "API docs enabled — Swagger UI: /_docs  OpenAPI JSON: /_openapi.json"
        );
        Router::new()
            .route("/_docs", get(swagger_ui_handler))
            .route("/_openapi.json", get(openapi_json_handler))
            .merge(base)
    } else {
        base
    };

    let cors = build_cors_layer(args.cors_origins.as_deref());

    let app = app
        // Innermost: body size limit applied before any user logic
        .layer(RequestBodyLimitLayer::new(1024 * 1024))
        // Combined custom middleware: request-id, rate limiting, auth, timeout
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::middleware,
        ))
        // CORS headers / preflight (runs before auth so OPTIONS isn't rejected)
        .layer(cors)
        // Compress responses when the client sends Accept-Encoding
        .layer(CompressionLayer::new())
        // Outermost: trace every request (sees final status after all layers)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    #[cfg(unix)]
    if let Some(socket_path) = &args.socket {
        let result = serve_unix(app, socket_path).await;
        if let Some(provider) = otel_provider
            && let Err(e) = provider.shutdown()
        {
            tracing::warn!("Failed to shutdown OpenTelemetry provider: {:?}", e);
        }
        return result;
    }

    let socket_addr: SocketAddr = format!("{}:{}", addr, port).parse().into_diagnostic()?;

    let result = serve(
        app,
        socket_addr,
        args.tls_cert.as_deref(),
        args.tls_key.as_deref(),
    )
    .await;

    if let Some(provider) = otel_provider
        && let Err(e) = provider.shutdown()
    {
        tracing::warn!("Failed to shutdown OpenTelemetry provider: {:?}", e);
    }

    result
}

#[cfg(unix)]
async fn serve_unix(app: Router, socket_path: &std::path::Path) -> Result<()> {
    if socket_path.exists() {
        std::fs::remove_file(socket_path).into_diagnostic()?;
    }

    let listener = tokio::net::UnixListener::bind(socket_path).into_diagnostic()?;
    tracing::info!("listening on unix:{}", socket_path.display());

    let result = axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .into_diagnostic();

    // Clean up socket file on shutdown.
    let _ = std::fs::remove_file(socket_path);

    result
}

async fn serve(
    app: Router,
    socket_addr: SocketAddr,
    tls_cert: Option<&std::path::Path>,
    tls_key: Option<&std::path::Path>,
) -> Result<()> {
    if let (Some(cert), Some(key)) = (tls_cert, tls_key) {
        let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
            .await
            .into_diagnostic()?;

        let handle = axum_server::Handle::new();
        tokio::spawn({
            let handle = handle.clone();
            async move {
                shutdown_signal().await;
                handle.graceful_shutdown(Some(std::time::Duration::from_secs(30)));
            }
        });

        tracing::info!("listening on https://{}", socket_addr);

        axum_server::bind_rustls(socket_addr, tls_config)
            .handle(handle)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .into_diagnostic()?;
    } else {
        tracing::info!("listening on http://{}", socket_addr);

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
    }

    Ok(())
}

fn init_tracer(
    endpoint: &str,
    service_name: &str,
) -> Result<opentelemetry_sdk::trace::SdkTracerProvider> {
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::Resource;

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()
        .into_diagnostic()?;

    let resource = Resource::builder()
        .with_service_name(service_name.to_string())
        .build();

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build();

    opentelemetry::global::set_tracer_provider(provider.clone());
    Ok(provider)
}

fn load_script_content(args: &Args) -> Result<String> {
    if let Some(command) = &args.command {
        Ok(command.clone())
    } else if let Some(script_path) = &args.script {
        std::fs::read_to_string(script_path).into_diagnostic()
    } else if args.stdin || is_stdin_piped() {
        use std::io::Read;
        let mut content = String::new();
        std::io::stdin()
            .read_to_string(&mut content)
            .into_diagnostic()?;
        if content.trim().is_empty() {
            Err(miette::miette!("No script provided via stdin"))
        } else {
            Ok(content)
        }
    } else {
        Err(miette::miette!(
            "No script provided. Use a script file path, -c 'script', or pipe a script via stdin"
        ))
    }
}

fn is_stdin_piped() -> bool {
    use std::io::IsTerminal;
    !std::io::stdin().is_terminal()
}

/// Build a `CorsLayer` from the `--cors-origins` CLI value.
///
/// - `None`  → no CORS support (cross-origin requests are blocked by the browser's same-origin policy)
/// - `"*"`   → permissive: any origin, any method, any header
/// - `"a,b"` → allow specific origins only
fn build_cors_layer(origins: Option<&str>) -> CorsLayer {
    match origins {
        None => CorsLayer::new(), // no allowed origins → CORS requests rejected
        Some("*") => CorsLayer::permissive(),
        Some(list) => {
            let values: Vec<HeaderValue> = list
                .split(',')
                .filter_map(|o| o.trim().parse().ok())
                .collect();
            CorsLayer::new()
                .allow_origin(values)
                .allow_methods(Any)
                .allow_headers(Any)
        }
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
