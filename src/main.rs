//! AI Agent Blockchain Execution Platform — entry point.
//!
//! Boots:
//! 1. Configuration from env / .env
//! 2. PostgreSQL connection pool + migrations
//! 3. Redis connection
//! 4. Execution engine (providers)
//! 5. Relayer orchestrator (Ethereum relayer)
//! 6. Background worker(s)
//! 7. Axum HTTP server with routing + middleware

use std::net::SocketAddr;

use axum::{
    extract::Request,
    http::StatusCode,
    middleware,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

use agent_execution_platform::{
    api::{
        middleware::{x402_middleware, PaymentVerifierState},
        routes::{self, AppState},
    },
    config::AppConfig,
    db,
    execution_engine::ExecutionEngine,
    queue,
    relayer::{ethereum::EthereumRelayer, orchestrator::RelayerOrchestrator},
    worker,
};

/// Supervisor that restarts a worker if it panics.  Each restart recovers
/// stale jobs from that worker's processing list first, so nothing is lost.
async fn worker_supervisor(
    redis_conn: redis::aio::ConnectionManager,
    db_pool: sqlx::PgPool,
    orchestrator: RelayerOrchestrator,
    worker_id: u32,
) {
    loop {
        let rc = redis_conn.clone();
        let pool = db_pool.clone();
        let orch = orchestrator.clone();

        let handle = tokio::spawn(async move {
            worker::run_worker(rc, pool, orch, worker_id).await;
        });

        match handle.await {
            Ok(()) => {
                // run_worker only returns if its loop breaks (shouldn't happen)
                tracing::warn!(worker_id, "worker exited cleanly — restarting");
            }
            Err(e) => {
                tracing::error!(worker_id, error = %e, "worker panicked — recovering and restarting");
                // Recover any in-flight jobs that were in this worker's processing list
                let mut rc = redis_conn.clone();
                if let Err(re) = queue::recover_stale_jobs(&mut rc, worker_id).await {
                    tracing::error!(worker_id, error = %re, "failed to recover stale jobs after panic");
                }
            }
        }

        // Brief cooldown before restart to avoid tight loops on persistent panics
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// API key authentication middleware.
///
/// If `API_KEY` is configured, every request must carry a matching
/// `X-API-Key` header.  Unset API_KEY = auth disabled (local dev only).
async fn api_key_middleware(
    axum::extract::State(expected_key): axum::extract::State<Option<String>>,
    req: Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    match &expected_key {
        None => next.run(req).await.into_response(), // auth disabled
        Some(key) => {
            let provided = req
                .headers()
                .get("x-api-key")
                .and_then(|v| v.to_str().ok());
            match provided {
                Some(v) if v == key => next.run(req).await.into_response(),
                _ => (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({ "error": "missing or invalid X-API-Key" })),
                )
                    .into_response(),
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── Logging ─────────────────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .init();

    info!("starting agent-execution-platform");

    // ── Configuration ───────────────────────────────────────────────
    let config = AppConfig::from_env()?;
    info!(host = %config.host, port = config.port, "config loaded");

    // ── Database ────────────────────────────────────────────────────
    let db_pool = db::create_pool(&config.database_url).await?;
    db::run_migrations(&db_pool).await?;
    info!("database connected and migrations applied");

    // ── Redis ───────────────────────────────────────────────────────
    let redis_conn = queue::create_redis_connection(&config.redis_url).await?;
    info!("redis connected");

    // ── Execution Engine ────────────────────────────────────────────
    let engine = ExecutionEngine::new(config.clone())?;
    info!("execution engine initialized");

    // ── Relayer Orchestrator ────────────────────────────────────────
    let eth_relayer =
        EthereumRelayer::new(&config.relayer_private_key, &config.ethereum_rpc_url)?;
    let orchestrator = RelayerOrchestrator::new().with_ethereum(eth_relayer);
    info!("relayer orchestrator ready");

    // ── Background Workers ──────────────────────────────────────────
    let num_workers: u32 = std::env::var("NUM_WORKERS")
        .unwrap_or_else(|_| "2".into())
        .parse()?;

    // Recover any stale jobs left in processing lists from a previous crash.
    // This must happen before workers start consuming, so recovered jobs are
    // available immediately.
    for wid in 0..num_workers {
        let mut rc = redis_conn.clone();
        let recovered = queue::recover_stale_jobs(&mut rc, wid).await?;
        if recovered > 0 {
            info!(worker_id = wid, recovered, "recovered stale jobs from previous run");
        }
    }

    // Spawn workers with a supervisor that auto-restarts on panic.
    for wid in 0..num_workers {
        let redis_clone = redis_conn.clone();
        let pool_clone = db_pool.clone();
        let orch_clone = orchestrator.clone();
        tokio::spawn(worker_supervisor(redis_clone, pool_clone, orch_clone, wid));
    }
    info!(workers = num_workers, "background workers spawned with supervisors");

    // ── App State ───────────────────────────────────────────────────
    let state = AppState {
        db_pool: db_pool.clone(),
        redis_conn,
        engine,
        config: config.clone(),
    };

    // ── Payment Verifier State (for x402 middleware) ────────────────
    let payment_verifier = PaymentVerifierState {
        config: config.clone(),
        eth_provider: state.engine.eth_provider.clone(),
        db_pool,
    };

    // ── Router ──────────────────────────────────────────────────────
    let api_key_state = config.api_key.clone();

    let app = Router::new()
        // Health check (no auth, no payment middleware — for load balancers)
        .route("/health", get(routes::health_handler))
        // Execution API — x402 middleware applied
        .route("/execute", post(routes::execute_handler))
        .route("/simulate", post(routes::simulate_handler))
        .route("/status/{id}", get(routes::status_handler))
        .layer(middleware::from_fn_with_state(
            payment_verifier,
            x402_middleware,
        ))
        // ── API Key auth (if API_KEY env var is set) ────────────────
        .layer(middleware::from_fn_with_state(
            api_key_state,
            api_key_middleware,
        ))
        .layer(TraceLayer::new_for_http())
        // ── Request body size limit (1 MB — prevents OOM from giant payloads)
        .layer(RequestBodyLimitLayer::new(1024 * 1024))
        // ── Global concurrency limit (prevents resource exhaustion) ─────────
        // MAX_CONCURRENT_REQUESTS controls max concurrent in-flight requests.
        // Default 50.  Set high to effectively disable.
        .layer(tower::limit::ConcurrencyLimitLayer::new(
            config.max_concurrent_requests.max(1) as usize,
        ))
        .layer({
            // In production set CORS_ORIGIN=https://yourdomain.com
            // Default (empty / unset) = permissive (for local dev)
            let cors = match std::env::var("CORS_ORIGIN") {
                Ok(origin) if !origin.is_empty() => {
                    CorsLayer::new()
                        .allow_origin(origin.parse::<axum::http::HeaderValue>().expect("invalid CORS_ORIGIN"))
                        .allow_methods(tower_http::cors::Any)
                        .allow_headers(tower_http::cors::Any)
                }
                _ => CorsLayer::permissive(),
            };
            cors
        })
        .with_state(state);

    // ── Serve with graceful shutdown ────────────────────────────────
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
