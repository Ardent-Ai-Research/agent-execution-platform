//! Comprehensive integration tests for the Agent Execution Platform.
//!
//! These tests spin up the full Axum server stack (including middleware layers)
//! against real PostgreSQL and Redis instances (from docker-compose).
//!
//! ## Prerequisites
//!   docker compose up -d   # Postgres on 5432, Redis on 6379
//!
//! ## Run
//!   cargo test --test integration_tests -- --test-threads=1
//!
//! Using `--test-threads=1` ensures tests that share DB / Redis state
//! don't interfere with each other.

use std::collections::HashMap;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use reqwest::Client;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use uuid::Uuid;

use agent_execution_platform::{
    agent_wallet::AgentWalletRegistry,
    api::routes::{self, AppState},
    config::AppConfig,
    db,
    execution_engine::ExecutionEngine,
    queue,
    rate_limit::RateLimiter,
    relayer::paymaster::PaymasterSigner,
    types::*,
};

// ═══════════════════════ Test infrastructure ═══════════════════════

/// Returns a test `AppConfig` from the local `.env`.
fn test_config() -> AppConfig {
    dotenvy::dotenv().ok();
    AppConfig::from_env().expect("failed to load config from .env")
}

/// Set up a fresh database pool and run migrations.
async fn setup_db(config: &AppConfig) -> PgPool {
    let pool = db::create_pool(&config.database_url)
        .await
        .expect("failed to create DB pool");
    db::run_migrations(&pool)
        .await
        .expect("failed to run migrations");
    pool
}

/// Set up a Redis connection.
async fn setup_redis(config: &AppConfig) -> redis::aio::ConnectionManager {
    queue::create_redis_connection(&config.redis_url)
        .await
        .expect("failed to connect to Redis")
}

/// Spin up the full application on a random available port.
/// Returns `(base_url, api_key, join_handle)`.
///
/// The middleware stack is identical to the production `main.rs`:
///   CORS → ConcurrencyLimit → BodySizeLimit →
///   [public routes] + [protected API key → rate limit routes]
async fn spawn_app() -> (String, String, tokio::task::JoinHandle<()>) {
    spawn_app_with_api_key_issuance(1_000_000, None).await
}

async fn spawn_app_with_api_key_issuance(
    public_api_key_limit: u64,
    client_ip_header: Option<&str>,
) -> (String, String, tokio::task::JoinHandle<()>) {
    let mut config = test_config();
    // Public issuance behavior is covered explicitly below. Keep repeated test
    // runs from exhausting the production-oriented per-IP allowance in Redis.
    config.public_api_key_limit = public_api_key_limit;
    config.public_api_key_client_ip_header = client_ip_header.map(str::to_string);
    let db_pool = setup_db(&config).await;
    let redis_conn = setup_redis(&config).await;

    let engine = ExecutionEngine::new(config.clone()).expect("engine init");

    // Create a test API key
    let (_, api_key) = db::create_api_key(&db_pool, Some("integration-test"))
        .await
        .expect("create API key");

    // Agent wallet registry
    let first_chain = config.chains.keys().next().unwrap();
    let first_cfg = config.chain_config(first_chain).unwrap();
    let factory: ethers::types::Address = first_cfg
        .factory_address
        .parse()
        .unwrap_or_else(|_| ethers::types::Address::zero());
    let provider = engine.provider_for_chain(first_chain).unwrap();
    let wallet_registry = AgentWalletRegistry::new(
        db_pool.clone(),
        &config.wallet_encryption_key,
        factory,
        provider,
    )
    .expect("wallet registry");

    // Bundler clients
    let mut bundler_clients = HashMap::new();
    for (chain, chain_cfg) in &config.chains {
        if chain_cfg.bundler_rpc_url.is_empty() {
            continue;
        }
        let ep: ethers::types::Address = chain_cfg.entry_point_address.parse().unwrap();
        let fa: ethers::types::Address = chain_cfg
            .factory_address
            .parse()
            .unwrap_or_else(|_| ethers::types::Address::zero());
        let p = engine.provider_for_chain(chain).unwrap();
        let bc = agent_execution_platform::relayer::erc4337::BundlerClient::new(
            chain_cfg.bundler_rpc_url.clone(),
            ep,
            fa,
            p,
        );
        bundler_clients.insert(chain.clone(), bc);
    }

    let mut paymaster_signers = HashMap::new();
    for (chain, chain_cfg) in &config.chains {
        let paymaster_address = chain_cfg
            .paymaster_address
            .parse()
            .expect("test paymaster address");
        // Deterministic test-only key; production keys are generated, encrypted,
        // and loaded by main.rs.
        let signer = PaymasterSigner::new(
            paymaster_address,
            "0000000000000000000000000000000000000000000000000000000000000001",
            300,
        )
        .expect("test paymaster signer");
        paymaster_signers.insert(chain.clone(), signer);
    }

    let state = AppState {
        db_pool: db_pool.clone(),
        redis_conn: redis_conn.clone(),
        engine: engine.clone(),
        config: config.clone(),
        wallet_registry,
        bundler_clients,
        paymaster_signers,
    };

    let rate_limiter = if config.per_key_rate_limit_rps > 0.0 {
        Some(RateLimiter::new(
            config.per_key_rate_limit_rps,
            config.per_key_rate_limit_burst,
        ))
    } else {
        None
    };

    let api_key_db_pool = db_pool.clone();

    let protected_api_router = Router::new()
        .route("/execute", post(routes::execute_handler))
        .route("/simulate", post(routes::simulate_handler))
        .route("/status/:id", get(routes::status_handler))
        .route("/wallet", get(routes::wallet_handler))
        .route(
            "/protocols/aave-v3/supply",
            post(routes::aave_supply_handler),
        )
        .route(
            "/protocols/aave-v3/supply/simulate",
            post(routes::aave_supply_simulate_handler),
        )
        .route(
            "/protocols/aave-v3/withdraw",
            post(routes::aave_withdraw_handler),
        )
        .route(
            "/protocols/aave-v3/withdraw/simulate",
            post(routes::aave_withdraw_simulate_handler),
        )
        .route("/protocols/aave-v3/repay", post(routes::aave_repay_handler))
        .route(
            "/protocols/aave-v3/repay/simulate",
            post(routes::aave_repay_simulate_handler),
        )
        .route(
            "/protocols/aave-v3/borrow",
            post(routes::aave_borrow_handler),
        )
        .route(
            "/protocols/aave-v3/borrow/simulate",
            post(routes::aave_borrow_simulate_handler),
        )
        .route(
            "/protocols/aave-v3/position",
            get(routes::aave_position_handler),
        )
        .layer({
            let rl = rate_limiter.clone();
            middleware::from_fn(move |req: Request, next: axum::middleware::Next| {
                let limiter = rl.clone();
                async move {
                    match limiter {
                        Some(ref l) => agent_execution_platform::rate_limit::rate_limit_middleware(
                            State(l.clone()),
                            req,
                            next,
                        )
                        .await
                        .into_response(),
                        None => next.run(req).await.into_response(),
                    }
                }
            })
        })
        .layer(middleware::from_fn_with_state(
            api_key_db_pool,
            api_key_auth_middleware_test,
        ));

    let app = Router::new()
        .route("/health", get(routes::health_handler))
        .route("/feed/recent", get(routes::public_feed_handler))
        .route("/api-keys", post(routes::create_api_key_handler))
        .merge(protected_api_router)
        .layer(RequestBodyLimitLayer::new(1024 * 1024))
        .layer(tower::limit::ConcurrencyLimitLayer::new(200))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");

    let handle = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (base_url, api_key, handle)
}

/// API-key auth middleware for tests — mirrors the real one in main.rs.
async fn api_key_auth_middleware_test(
    axum::extract::State(db_pool): axum::extract::State<sqlx::PgPool>,
    mut req: Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    let provided = req
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    match provided {
        None => (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({ "error": "missing X-API-Key header" })),
        )
            .into_response(),
        Some(raw_key) => match db::get_api_key_by_raw(&db_pool, &raw_key).await {
            Ok(Some(api_key_row)) => {
                req.extensions_mut().insert(ApiKeyContext {
                    api_key_id: api_key_row.id,
                    label: api_key_row.label,
                });
                next.run(req).await.into_response()
            }
            Ok(None) => (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({ "error": "invalid API key" })),
            )
                .into_response(),
            Err(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(json!({ "error": "auth error" })),
            )
                .into_response(),
        },
    }
}

/// Build a reqwest client with a generous timeout for RPC calls.
fn http_client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap()
}

// ═══════════════════════ HTTP API Tests ══════════════════════════════

// ────────────────── Health endpoint ──────────────────────────────────

#[tokio::test]
async fn test_health_endpoint_returns_ok() {
    let (base, _api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c.get(format!("{base}/health")).send().await.unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["checks"]["database"], "ok");
    assert_eq!(body["checks"]["redis"], "ok");
    assert!(body["version"].is_string());
    assert_eq!(body["service"], "agent-execution-platform");
}

#[tokio::test]
async fn test_health_without_api_key_returns_200() {
    let (base, _key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

// ────────────────── API Key Authentication ───────────────────────────

#[tokio::test]
async fn test_missing_api_key_returns_401() {
    let (base, _key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .get(format!("{base}/wallet?agent_id=test-agent&chain=ethereum"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_invalid_api_key_returns_401() {
    let (base, _key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .get(format!("{base}/wallet?agent_id=test-agent&chain=ethereum"))
        .header("X-API-Key", "ak_bogus_key_that_does_not_exist")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_valid_api_key_passes_auth() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    // Use a protected endpoint to verify API-key auth passes
    let resp = c
        .get(format!("{base}/wallet?agent_id=auth-check&chain=ethereum"))
        .header("X-API-Key", &api_key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// ────────────────── Public API key creation ──────────────────────────

#[tokio::test]
async fn test_public_create_api_key_success() {
    let (base, _key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/api-keys"))
        .json(&json!({ "label": "my-agent-key" }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    assert_eq!(resp.headers()["cache-control"], "no-store");
    let body: Value = resp.json().await.unwrap();
    let raw_key = body["api_key"].as_str().unwrap();
    assert!(raw_key.starts_with("ak_"));
    assert!(
        raw_key.len() >= 46,
        "API key should contain 256 bits of entropy"
    );
    assert!(body["api_key_id"].is_string());
    assert_eq!(body["label"], "my-agent-key");

    let authenticated = c
        .get(format!(
            "{base}/wallet?agent_id=public-key-auth-check&chain=ethereum"
        ))
        .header("X-API-Key", raw_key)
        .send()
        .await
        .unwrap();
    assert_eq!(authenticated.status(), 200);
}

#[tokio::test]
async fn test_public_create_api_key_rejects_oversized_label() {
    let (base, _api_key, _h) = spawn_app().await;
    let c = http_client();
    let resp = c
        .post(format!("{base}/api-keys"))
        .json(&json!({ "label": "x".repeat(101) }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_public_create_api_key_rate_limit_returns_429() {
    let client_ip_header = "x-test-client-ip";
    let (base, _api_key, _h) = spawn_app_with_api_key_issuance(1, Some(client_ip_header)).await;
    let c = http_client();
    let bytes = Uuid::new_v4().into_bytes();
    let client_ip = format!("10.{}.{}.{}", bytes[0], bytes[1], bytes[2]);

    let first = c
        .post(format!("{base}/api-keys"))
        .header(client_ip_header, &client_ip)
        .json(&json!({ "label": "first-key" }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 201);

    let second = c
        .post(format!("{base}/api-keys"))
        .header(client_ip_header, &client_ip)
        .json(&json!({ "label": "second-key" }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 429);
    assert!(second.headers().contains_key("retry-after"));
    let body: Value = second.json().await.unwrap();
    assert_eq!(body["error"], "api_key_issuance_rate_limit_exceeded");
}

// ────────────────── GET /wallet ─────────────────────────────────────

#[tokio::test]
async fn test_wallet_returns_smart_wallet_address() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .get(format!(
            "{base}/wallet?agent_id=wallet-test-agent&chain=ethereum"
        ))
        .header("X-API-Key", &api_key)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["agent_id"], "wallet-test-agent");
    assert!(body["smart_wallet_address"]
        .as_str()
        .unwrap()
        .starts_with("0x"));
    assert!(body["deployed"].is_boolean());
}

#[tokio::test]
async fn test_wallet_idempotent() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();
    let url = format!("{base}/wallet?agent_id=idem-agent&chain=ethereum");

    let r1: Value = c
        .get(&url)
        .header("X-API-Key", &api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let r2: Value = c
        .get(&url)
        .header("X-API-Key", &api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(r1["smart_wallet_address"], r2["smart_wallet_address"]);
}

#[tokio::test]
async fn test_wallet_different_agents_different_addresses() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let r1: Value = c
        .get(format!("{base}/wallet?agent_id=agent-alpha&chain=ethereum"))
        .header("X-API-Key", &api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let r2: Value = c
        .get(format!("{base}/wallet?agent_id=agent-beta&chain=ethereum"))
        .header("X-API-Key", &api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_ne!(r1["smart_wallet_address"], r2["smart_wallet_address"]);
}

#[tokio::test]
async fn test_wallet_namespace_isolation_across_api_keys() {
    let config = test_config();
    let db_pool = setup_db(&config).await;

    let (base, key1, _h) = spawn_app().await;
    let (_, key2) = db::create_api_key(&db_pool, Some("key-2")).await.unwrap();
    let c = http_client();

    let r1: Value = c
        .get(format!("{base}/wallet?agent_id=shared-name&chain=ethereum"))
        .header("X-API-Key", &key1)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let r2: Value = c
        .get(format!("{base}/wallet?agent_id=shared-name&chain=ethereum"))
        .header("X-API-Key", &key2)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_ne!(
        r1["smart_wallet_address"], r2["smart_wallet_address"],
        "different API keys + same agent_id must produce different wallets"
    );
}

#[tokio::test]
async fn test_wallet_unsupported_chain_returns_400() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .get(format!("{base}/wallet?agent_id=test&chain=solana"))
        .header("X-API-Key", &api_key)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_wallet_empty_agent_id_returns_400() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .get(format!("{base}/wallet?agent_id=%20&chain=ethereum"))
        .header("X-API-Key", &api_key)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

// ────────────────── POST /simulate ──────────────────────────────────

#[tokio::test]
async fn test_simulate_unsupported_chain_returns_400() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/simulate"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "test",
            "chain": "polygon",
            "target_contract": "0x1234567890abcdef1234567890abcdef12345678",
            "calldata": "0xa9059cbb0000000000000000000000000000000000000000000000000000000000000001",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_simulate_invalid_target_returns_400() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/simulate"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "test",
            "chain": "ethereum",
            "target_contract": "not-an-address",
            "calldata": "0xa9059cbb0000000000000000000000000000000000000000000000000000000000000001",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_simulate_empty_calldata_returns_400() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/simulate"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "test",
            "chain": "ethereum",
            "target_contract": "0x1234567890abcdef1234567890abcdef12345678",
            "calldata": "0x",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_simulate_empty_agent_id_returns_400() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/simulate"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": " ",
            "chain": "ethereum",
            "target_contract": "0x1234567890abcdef1234567890abcdef12345678",
            "calldata": "0xa9059cbb0000000000000000000000000000000000000000000000000000000000000001",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_simulate_valid_call_against_sepolia() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/simulate"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "sim-test-agent",
            "chain": "ethereum",
            "target_contract": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
            "calldata": "0xa9059cbb00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000001",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["request_id"].is_string());
    assert!(body["smart_wallet_address"].is_string());
}

#[tokio::test]
async fn test_simulate_batch_calls_empty_rejected() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/simulate"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "batch-test",
            "chain": "ethereum",
            "batch_calls": [],
        }))
        .send()
        .await
        .unwrap();

    // Should NOT be 200 — either 400 or 500 depending on error classification
    assert_ne!(resp.status(), 200, "empty batch_calls should not succeed");
}

#[tokio::test]
async fn test_simulate_batch_calls_over_limit_rejected() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let calls: Vec<Value> = (0..17)
        .map(|_| json!({
            "target_contract": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
            "calldata": "0xa9059cbb00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000001",
        }))
        .collect();

    let resp = c
        .post(format!("{base}/simulate"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "batch-max",
            "chain": "ethereum",
            "batch_calls": calls,
        }))
        .send()
        .await
        .unwrap();

    assert_ne!(resp.status(), 200, "17 batch calls should exceed limit");
}

#[tokio::test]
async fn test_aave_supply_simulate_unsupported_asset_returns_400() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/protocols/aave-v3/supply/simulate"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "aave-bad-asset",
            "chain": "ethereum",
            "asset": "NOT_A_RESERVE",
            "amount": "1"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

// ────────────────── POST /execute ───────────────────────────────────

#[tokio::test]
async fn test_execute_unsupported_chain_returns_400() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/execute"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "test",
            "chain": "avalanche",
            "target_contract": "0x1234567890abcdef1234567890abcdef12345678",
            "calldata": "0xa9059cbb0000000000000000000000000000000000000000000000000000000000000001",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

// ────────────────── GET /status/{id} ────────────────────────────────

#[tokio::test]
async fn test_status_invalid_uuid() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .get(format!("{base}/status/not-a-uuid"))
        .header("X-API-Key", &api_key)
        .send()
        .await
        .unwrap();

    // Should be 400 (invalid UUID) — though some routing layers might make it 404
    let s = resp.status().as_u16();
    assert!(s == 400 || s == 404, "expected 400 or 404, got {s}");
}

#[tokio::test]
async fn test_status_nonexistent_returns_404() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .get(format!("{base}/status/{}", Uuid::new_v4()))
        .header("X-API-Key", &api_key)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn test_status_returns_existing_request() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    // Create via simulate
    let sim_resp = c
        .post(format!("{base}/simulate"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "status-test",
            "chain": "ethereum",
            "target_contract": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
            "calldata": "0xa9059cbb00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000001",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(sim_resp.status(), 200);
    let sim_body: Value = sim_resp.json().await.unwrap();
    let request_id = sim_body["request_id"].as_str().unwrap();

    let resp = c
        .get(format!("{base}/status/{request_id}"))
        .header("X-API-Key", &api_key)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["request_id"], request_id);
    assert_eq!(body["chain"], "ethereum");
    assert!(body["created_at"].is_string());
    assert!(body["updated_at"].is_string());

    let second_key_resp = c
        .post(format!("{base}/api-keys"))
        .json(&json!({ "label": "status-isolation" }))
        .send()
        .await
        .unwrap();
    assert_eq!(second_key_resp.status(), 201);
    let second_key_body: Value = second_key_resp.json().await.unwrap();
    let second_key = second_key_body["api_key"].as_str().unwrap();

    let hidden = c
        .get(format!("{base}/status/{request_id}"))
        .header("X-API-Key", second_key)
        .send()
        .await
        .unwrap();
    assert_eq!(hidden.status(), 404);
}

// ────────────────── Calldata validation edge cases ──────────────────

#[tokio::test]
async fn test_calldata_odd_length_hex_returns_400() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/simulate"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "odd",
            "chain": "ethereum",
            "target_contract": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
            "calldata": "0xa9059cb",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_calldata_too_short_returns_400() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/simulate"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "short",
            "chain": "ethereum",
            "target_contract": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
            "calldata": "0xa905",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_calldata_without_0x_prefix_returns_400() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/simulate"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "noprefix",
            "chain": "ethereum",
            "target_contract": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
            "calldata": "a9059cbb0000000000000000000000000000000000000000000000000000000000000001",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

// ────────────────── Chain aliases ────────────────────────────────────

#[tokio::test]
async fn test_chain_aliases_recognized() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    for alias in &["eth", "mainnet", "ethereum"] {
        let resp = c
            .get(format!("{base}/wallet?agent_id=alias-test&chain={alias}"))
            .header("X-API-Key", &api_key)
            .send()
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            200,
            "alias '{alias}' should resolve to ethereum"
        );
    }
}

// ────────────────── Body size limit ─────────────────────────────────

#[tokio::test]
async fn test_request_body_size_limit() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let large_calldata = format!("0x{}", "ab".repeat(600_000));

    let resp = c
        .post(format!("{base}/execute"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "big",
            "chain": "ethereum",
            "target_contract": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
            "calldata": large_calldata,
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 413, "body > 1MB should be rejected");
}
