//! AI Agent Blockchain Execution Platform — entry point.
//!
//! Boots:
//! 1. Configuration from env / .env
//! 2. PostgreSQL connection pool + migrations
//! 3. Redis connection
//! 4. Execution engine (providers)
//! 5. Agent wallet registry (ERC-4337 smart wallet provisioning)
//! 6. ERC-4337 bundler client + paymaster signer
//! 7. Background worker(s)
//! 8. Axum HTTP server with routing + middleware

use std::net::SocketAddr;

use anyhow::Context;
use axum::{
    extract::{Request, State},
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

use ethers::signers::{LocalWallet, Signer};
use zeroize::Zeroize;

use agent_execution_platform::{
    agent_wallet::AgentWalletRegistry,
    api::{
        middleware::{x402_middleware, PaymentVerifierState},
        routes::{self, AppState},
    },
    config::AppConfig,
    db,
    execution_engine::ExecutionEngine,
    queue,
    rate_limit::{self, RateLimiter},
    relayer::{
        erc4337::BundlerClient,
        paymaster::PaymasterSigner,
    },
    types::ApiKeyContext,
    worker::{self, WorkerContext},
};

/// Supervisor that restarts a worker if it panics.  Each restart recovers
/// stale jobs from that worker's processing list first, so nothing is lost.
async fn worker_supervisor(
    redis_conn: redis::aio::ConnectionManager,
    ctx: WorkerContext,
    worker_id: u32,
) {
    loop {
        let rc = redis_conn.clone();
        let context = ctx.clone();

        let handle = tokio::spawn(async move {
            worker::run_worker(rc, context, worker_id).await;
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

/// API key authentication middleware (database-backed).
///
/// Every request must carry a valid `X-API-Key` header. The key is hashed
/// and looked up in the `api_keys` table. On success, an [`ApiKeyContext`]
/// is attached to the request extensions so downstream handlers know which
/// customer is calling.
///
/// If `API_KEY_AUTH_DISABLED` env var is set to "true", auth is bypassed
/// (local dev only — a default API key context is injected).
async fn api_key_middleware(
    axum::extract::State(db_pool): axum::extract::State<sqlx::PgPool>,
    mut req: Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    // Dev bypass — if auth is explicitly disabled
    let auth_disabled = std::env::var("API_KEY_AUTH_DISABLED")
        .map(|v| v == "true")
        .unwrap_or(false);

    if auth_disabled {
        // Inject a synthetic API key context for local dev
        req.extensions_mut().insert(ApiKeyContext {
            api_key_id: uuid::Uuid::nil(),
            label: Some("dev-bypass".into()),
        });
        return next.run(req).await.into_response();
    }

    let provided = req
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    match provided {
        None => (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({ "error": "missing X-API-Key header" })),
        )
            .into_response(),
        Some(raw_key) => {
            match db::get_api_key_by_raw(&db_pool, &raw_key).await {
                Ok(Some(api_key_row)) => {
                    req.extensions_mut().insert(ApiKeyContext {
                        api_key_id: api_key_row.id,
                        label: api_key_row.label,
                    });
                    next.run(req).await.into_response()
                }
                Ok(None) => (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({ "error": "invalid API key" })),
                )
                    .into_response(),
                Err(_) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({ "error": "authentication service error" })),
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
    let supported: Vec<_> = config.chains.keys().map(|c| c.to_string()).collect();
    info!(chains = ?supported, "execution engine initialized");

    // ── Agent Wallet Registry (ERC-4337) ────────────────────────────
    // Uses the first configured chain's factory + provider for address
    // derivation (deterministic via CREATE2, chain-independent).
    let first_chain = config.chains.keys().next()
        .ok_or_else(|| anyhow::anyhow!("no chains configured"))?;
    let first_chain_cfg = config.chain_config(first_chain)?;
    let factory_address: ethers::types::Address = first_chain_cfg
        .factory_address
        .parse()
        .unwrap_or_else(|_| ethers::types::Address::zero());
    let first_provider = engine.provider_for_chain(first_chain)?;
    let wallet_registry = AgentWalletRegistry::new(
        db_pool.clone(),
        &config.wallet_encryption_key,
        factory_address,
        first_provider,
    )
    .context("failed to initialize agent wallet registry")?;
    info!("agent wallet registry initialized");

    // ── Per-chain ERC-4337 Bundler Clients ──────────────────────────
    let mut bundler_clients = std::collections::HashMap::new();
    for (chain, chain_cfg) in &config.chains {
        if chain_cfg.bundler_rpc_url.is_empty() {
            tracing::warn!(chain = %chain, "no bundler URL configured — skipping bundler for this chain");
            continue;
        }
        let ep: ethers::types::Address = chain_cfg
            .entry_point_address
            .parse()
            .unwrap_or_else(|_| "0x433709009B8330FDa32311DF1C2AFA402eD8D009".parse().unwrap());
        let fa: ethers::types::Address = chain_cfg
            .factory_address
            .parse()
            .unwrap_or_else(|_| ethers::types::Address::zero());
        let provider = engine.provider_for_chain(chain)?;
        let bc = BundlerClient::new(
            chain_cfg.bundler_rpc_url.clone(),
            ep,
            fa,
            provider,
        );
        info!(
            chain = %chain,
            bundler_url = %chain_cfg.bundler_rpc_url,
            "ERC-4337 bundler client initialized"
        );

        // Validate entry point at startup (best-effort)
        if let Err(e) = bc.validate_entry_point_supported().await {
            tracing::warn!(
                chain = %chain,
                error = %e,
                "could not validate entry point against bundler (bundler may be unreachable)"
            );
        }

        bundler_clients.insert(chain.clone(), bc);
    }

    if bundler_clients.is_empty() {
        tracing::warn!("no bundler clients configured — execution will fail until a bundler is set up");
    }

    // ── ERC-4337 Paymaster Signer (auto-generated, DB-backed) ─────────
    // One signing key shared across all chains.  Per-chain paymaster
    // contract addresses determine which chains have sponsorship enabled.
    let mut paymaster_signers = std::collections::HashMap::new();
    let any_paymaster_configured = config.chains.values().any(|c| !c.paymaster_address.is_empty());

    if any_paymaster_configured {
        // Parse the wallet encryption key
        let enc_key_bytes = hex::decode(&config.wallet_encryption_key)
            .context("WALLET_ENCRYPTION_KEY must be valid hex")?;
        if enc_key_bytes.len() != 32 {
            anyhow::bail!("WALLET_ENCRYPTION_KEY must be exactly 32 bytes (64 hex chars)");
        }
        let mut enc_key = [0u8; 32];
        enc_key.copy_from_slice(&enc_key_bytes);

        // Load or generate the paymaster signing key (shared across chains)
        let existing = db::get_platform_key(&db_pool, "paymaster_signer").await?;

        let (encrypted_b64, signer_address_str) = match existing {
            Some(row) => {
                info!(
                    signer = %row.address,
                    "loaded existing paymaster signer from database"
                );
                (row.encrypted_key, row.address)
            }
            None => {
                // First boot — generate a fresh EOA for paymaster signing.
                let wallet = LocalWallet::new(&mut rand::thread_rng());
                let address = wallet.address();
                let mut key_hex = hex::encode(wallet.signer().to_bytes());

                let encrypted = agent_execution_platform::agent_wallet::encrypt_key_hex(
                    &enc_key, &key_hex,
                )?;

                key_hex.zeroize();
                drop(wallet);

                let addr_str = format!("{address:?}");

                let inserted = db::insert_platform_key(
                    &db_pool,
                    "paymaster_signer",
                    &encrypted,
                    &addr_str,
                )
                .await?;

                match inserted {
                    Some(row) => {
                        info!(
                            signer = %row.address,
                            "generated NEW paymaster signer key — \
                             activate sponsorship by calling \
                             VerifyingPaymaster.setVerifyingSigner({})",
                            row.address
                        );
                        (row.encrypted_key, row.address)
                    }
                    None => {
                        let row = db::get_platform_key(&db_pool, "paymaster_signer")
                            .await?
                            .context("platform key disappeared after insert race")?;
                        info!(
                            signer = %row.address,
                            "loaded paymaster signer from database (concurrent boot)"
                        );
                        (row.encrypted_key, row.address)
                    }
                }
            }
        };

        // Decrypt the shared signing key
        let mut key_hex = agent_execution_platform::agent_wallet::decrypt_key_hex(
            &enc_key, &encrypted_b64,
        )?;
        enc_key.zeroize();

        // Create one PaymasterSigner per chain that has a paymaster address
        for (chain, chain_cfg) in &config.chains {
            if chain_cfg.paymaster_address.is_empty() {
                tracing::warn!(
                    chain = %chain,
                    "no paymaster address for this chain — sponsorship disabled"
                );
                continue;
            }
            let pm_address: ethers::types::Address = chain_cfg
                .paymaster_address
                .parse()
                .context(format!("invalid {}_PAYMASTER_ADDRESS", chain.to_string().to_uppercase()))?;

            let signer = PaymasterSigner::new(
                pm_address,
                &key_hex,
                300, // 5-minute validity window
            )?;

            info!(
                chain = %chain,
                paymaster = %pm_address,
                signer = %signer_address_str,
                "paymaster signer ready for chain"
            );
            paymaster_signers.insert(chain.clone(), signer);
        }

        key_hex.zeroize();
    } else {
        tracing::warn!(
            "no PAYMASTER_ADDRESS set on any chain — \
             paymaster sponsorship disabled (agents must self-fund gas)"
        );
    }

    // ── Worker Context (ERC-4337-aware) ─────────────────────────────
    let worker_ctx = WorkerContext {
        db_pool: db_pool.clone(),
        wallet_registry: wallet_registry.clone(),
        bundler_clients: bundler_clients.clone(),
        paymaster_signers,
        webhook_client: agent_execution_platform::webhook::build_http_client(),
    };
    info!("worker context assembled");

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
        let ctx_clone = worker_ctx.clone();
        tokio::spawn(worker_supervisor(redis_clone, ctx_clone, wid));
    }
    info!(workers = num_workers, "background workers spawned with supervisors");

    // ── App State ───────────────────────────────────────────────────
    let state = AppState {
        db_pool: db_pool.clone(),
        redis_conn,
        engine,
        config: config.clone(),
        wallet_registry,
        bundler_clients: bundler_clients.clone(),
    };

    // ── Per-API-Key Rate Limiter ────────────────────────────────────
    let rate_limiter = if config.per_key_rate_limit_rps > 0.0 {
        let rl = RateLimiter::new(
            config.per_key_rate_limit_rps,
            config.per_key_rate_limit_burst,
        );
        // Periodic eviction of stale buckets to prevent unbounded memory growth
        let rl_evict = rl.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                rl_evict.evict_stale();
            }
        });
        info!(
            rps = config.per_key_rate_limit_rps,
            burst = config.per_key_rate_limit_burst,
            "per-API-key rate limiter enabled"
        );
        Some(rl)
    } else {
        info!("per-API-key rate limiting disabled (PER_KEY_RATE_LIMIT_RPS = 0)");
        None
    };

    // ── Payment Verifier State (for x402 middleware) ────────────────
    // Build per-chain provider map from the engine
    let mut pv_providers = std::collections::HashMap::new();
    for chain in config.chains.keys() {
        if let Ok(p) = state.engine.provider_for_chain(chain) {
            pv_providers.insert(chain.clone(), p);
        }
    }
    let payment_verifier = PaymentVerifierState {
        config: config.clone(),
        providers: pv_providers,
        db_pool: db_pool.clone(),
    };

    // ── Router ──────────────────────────────────────────────────────
    let api_key_db_pool = db_pool.clone();

    // Admin sub-router — separate auth (bearer token, not API key)
    let admin_router = Router::new()
        .route("/api-keys", post(routes::create_api_key_handler))
        .layer(middleware::from_fn(routes::admin_auth_middleware))
        .with_state(state.clone());

    let app = Router::new()
        // Health check (no auth, no payment middleware — for load balancers)
        .route("/health", get(routes::health_handler))
        // Admin endpoints (bearer-token auth, no x402 or API key middleware)
        .nest("/admin", admin_router)
        // Execution API — x402 middleware applied
        .route("/execute", post(routes::execute_handler))
        .route("/simulate", post(routes::simulate_handler))
        .route("/status/:id", get(routes::status_handler))
        .route("/wallet", get(routes::wallet_handler))
        .layer(middleware::from_fn_with_state(
            payment_verifier,
            x402_middleware,
        ))
        // ── Per-API-key rate limiting (after auth, before business logic) ──
        .layer({
            let rl = rate_limiter.clone();
            middleware::from_fn(move |req: Request, next: axum::middleware::Next| {
                let limiter = rl.clone();
                async move {
                    match limiter {
                        Some(ref l) => rate_limit::rate_limit_middleware(
                            State(l.clone()), req, next
                        ).await.into_response(),
                        None => next.run(req).await.into_response(),
                    }
                }
            })
        })
        // ── API Key auth (DB-backed, per-customer keys) ─────────────
        .layer(middleware::from_fn_with_state(
            api_key_db_pool,
            api_key_middleware,
        ))
        .layer(TraceLayer::new_for_http())
        // ── Request body size limit (1 MB — prevents OOM from giant payloads)
        .layer(RequestBodyLimitLayer::new(1024 * 1024))
        // ── Global concurrency limit (prevents resource exhaustion) ─────────
        // MAX_CONCURRENT_REQUESTS controls max concurrent in-flight requests.
        // Default 200.  Set high to effectively disable.
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
