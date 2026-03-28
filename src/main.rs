//! AI Agent Blockchain Execution Platform — hackathon edition.
//!
//! Minimal boot: no workers, no Redis, no DB, no payment middleware.
//! Just router + serve. Inline synchronous execution.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

use agent_execution_platform::{
    api::routes::{self, AppState},
    config::AppConfig,
    execution_engine::ExecutionEngine,
    relayer::{ethereum::EthereumRelayer, orchestrator::RelayerOrchestrator},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── Logging ─────────────────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .init();

    info!("starting agent-execution-platform (hackathon mode)");

    // ── Configuration ───────────────────────────────────────────────
    let config = AppConfig::from_env()?;
    info!(host = %config.host, port = config.port, "config loaded");

    // ── Execution Engine ────────────────────────────────────────────
    let engine = ExecutionEngine::new(config.clone())?;
    info!("execution engine initialized");

    // ── Relayer Orchestrator ────────────────────────────────────────
    let eth_relayer =
        EthereumRelayer::new(&config.relayer_private_key, &config.ethereum_rpc_url)?;
    let orchestrator = RelayerOrchestrator::new().with_ethereum(eth_relayer);
    info!("relayer orchestrator ready");

    // ── App State ───────────────────────────────────────────────────
    let state = AppState {
        engine,
        orchestrator,
        store: Arc::new(Mutex::new(HashMap::new())),
    };

    // ── Router ──────────────────────────────────────────────────────
    let app = Router::new()
        .route("/health", get(routes::health_handler))
        .route("/execute", post(routes::execute_handler))
        .route("/simulate", post(routes::simulate_handler))
        .route("/status/:id", get(routes::status_handler))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state);

    // ── Serve ───────────────────────────────────────────────────────
    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    info!(address = %addr, "HTTP server listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("server shut down gracefully");
    Ok(())
}

/// Wait for SIGINT (Ctrl-C) or SIGTERM to initiate graceful shutdown.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received SIGINT, shutting down"),
        _ = terminate => info!("received SIGTERM, shutting down"),
    }
}
