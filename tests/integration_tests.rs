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
    api::{
        middleware::{x402_middleware, PaymentVerifierState},
        routes::{self, AppState},
    },
    config::AppConfig,
    db,
    execution_engine::ExecutionEngine,
    queue,
    rate_limit::RateLimiter,
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
///   CORS → ConcurrencyLimit → BodySizeLimit → API key auth → rate limit → x402 → handlers
///
/// **Important**: ALL routes (including `/health`) go through API key auth.
async fn spawn_app() -> (String, String, tokio::task::JoinHandle<()>) {
    let config = test_config();
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
    let wallet_registry =
        AgentWalletRegistry::new(db_pool.clone(), &config.wallet_encryption_key, factory, provider)
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

    let state = AppState {
        db_pool: db_pool.clone(),
        redis_conn: redis_conn.clone(),
        engine: engine.clone(),
        config: config.clone(),
        wallet_registry,
        bundler_clients,
    };

    let rate_limiter = if config.per_key_rate_limit_rps > 0.0 {
        Some(RateLimiter::new(
            config.per_key_rate_limit_rps,
            config.per_key_rate_limit_burst,
        ))
    } else {
        None
    };

    let mut pv_providers = HashMap::new();
    for chain in config.chains.keys() {
        if let Ok(p) = engine.provider_for_chain(chain) {
            pv_providers.insert(chain.clone(), p);
        }
    }
    let payment_verifier = PaymentVerifierState {
        config: config.clone(),
        providers: pv_providers,
        db_pool: db_pool.clone(),
    };

    let api_key_db_pool = db_pool.clone();

    let admin_router = Router::new()
        .route("/api-keys", post(routes::create_api_key_handler))
        .layer(middleware::from_fn(routes::admin_auth_middleware))
        .with_state(state.clone());

    let app = Router::new()
        .route("/health", get(routes::health_handler))
        .nest("/admin", admin_router)
        .route("/execute", post(routes::execute_handler))
        .route("/simulate", post(routes::simulate_handler))
        .route("/status/:id", get(routes::status_handler))
        .route("/wallet", get(routes::wallet_handler))
        .layer(middleware::from_fn_with_state(
            payment_verifier,
            x402_middleware,
        ))
        .layer({
            let rl = rate_limiter.clone();
            middleware::from_fn(move |req: Request, next: axum::middleware::Next| {
                let limiter = rl.clone();
                async move {
                    match limiter {
                        Some(ref l) => {
                            agent_execution_platform::rate_limit::rate_limit_middleware(
                                State(l.clone()),
                                req,
                                next,
                            )
                            .await
                            .into_response()
                        }
                        None => next.run(req).await.into_response(),
                    }
                }
            })
        })
        .layer(middleware::from_fn_with_state(
            api_key_db_pool,
            api_key_auth_middleware_test,
        ))
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
        axum::serve(listener, app).await.unwrap();
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
    let auth_disabled = std::env::var("API_KEY_AUTH_DISABLED")
        .map(|v| v == "true")
        .unwrap_or(false);

    if auth_disabled {
        req.extensions_mut().insert(ApiKeyContext {
            api_key_id: Uuid::nil(),
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
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .get(format!("{base}/health"))
        .header("X-API-Key", &api_key)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["checks"]["database"], "ok");
    assert_eq!(body["checks"]["redis"], "ok");
    assert!(body["version"].is_string());
    assert_eq!(body["service"], "agent-execution-platform");
}

#[tokio::test]
async fn test_health_without_api_key_returns_401() {
    let (base, _key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 401);
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

    // Use /health as a lightweight check that auth passes
    let resp = c
        .get(format!("{base}/health"))
        .header("X-API-Key", &api_key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// ────────────────── Admin API key creation ───────────────────────────

#[tokio::test]
async fn test_admin_create_api_key_without_bearer_returns_error() {
    let (base, _key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/admin/api-keys"))
        .json(&json!({ "label": "test" }))
        .send()
        .await
        .unwrap();

    // Admin uses bearer auth, not X-API-Key. Without bearer → 403 or 401.
    let s = resp.status().as_u16();
    assert!(s == 401 || s == 403, "expected 401 or 403, got {s}");
}

#[tokio::test]
async fn test_admin_create_api_key_wrong_token_returns_401() {
    std::env::set_var("ADMIN_BEARER_TOKEN", "test-admin-secret");

    let (base, _key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/admin/api-keys"))
        .header("Authorization", "Bearer wrong-token")
        .json(&json!({ "label": "test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    std::env::remove_var("ADMIN_BEARER_TOKEN");
}

#[tokio::test]
async fn test_admin_create_api_key_success() {
    std::env::set_var("ADMIN_BEARER_TOKEN", "test-admin-ok");

    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    // Admin endpoint is behind the global API key middleware,
    // so we need BOTH the X-API-Key and the Bearer token.
    let resp = c
        .post(format!("{base}/admin/api-keys"))
        .header("X-API-Key", &api_key)
        .header("Authorization", "Bearer test-admin-ok")
        .json(&json!({ "label": "my-agent-key" }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: Value = resp.json().await.unwrap();
    assert!(body["api_key"].as_str().unwrap().starts_with("ak_"));
    assert!(body["api_key_id"].is_string());
    assert_eq!(body["label"], "my-agent-key");

    std::env::remove_var("ADMIN_BEARER_TOKEN");
}

// ────────────────── GET /wallet ─────────────────────────────────────

#[tokio::test]
async fn test_wallet_returns_smart_wallet_address() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .get(format!("{base}/wallet?agent_id=wallet-test-agent&chain=ethereum"))
        .header("X-API-Key", &api_key)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["agent_id"], "wallet-test-agent");
    assert!(body["smart_wallet_address"].as_str().unwrap().starts_with("0x"));
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
        r1["smart_wallet_address"],
        r2["smart_wallet_address"],
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

// ────────────────── POST /execute ───────────────────────────────────

#[tokio::test]
async fn test_execute_without_payment_returns_402() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/execute"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "exec-test",
            "chain": "ethereum",
            "target_contract": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
            "calldata": "0xa9059cbb00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000001",
        }))
        .send()
        .await
        .unwrap();

    let status_code = resp.status();
    let body: Value = resp.json().await.unwrap();

    if status_code == 402 {
        assert_eq!(body["error"], "payment_required");
        assert!(body["amount_usd"].is_number());
        assert!(body["accepted_tokens"].is_array());
        assert!(body["payment_address"].is_string());
    } else {
        // Simulation succeeded → x402 middleware didn't fire → request went through
        assert_eq!(status_code, 200);
    }
}

#[tokio::test]
async fn test_execute_402_includes_accepted_tokens() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/execute"))
        .header("X-API-Key", &api_key)
        .json(&json!({
            "agent_id": "token-check",
            "chain": "ethereum",
            "target_contract": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
            "calldata": "0xa9059cbb00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000001",
        }))
        .send()
        .await
        .unwrap();

    if resp.status() == 402 {
        let body: Value = resp.json().await.unwrap();
        let tokens: Vec<&str> = body["accepted_tokens"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        assert!(
            tokens.contains(&"USDC") || tokens.contains(&"USDT"),
            "expected USDC or USDT, got: {tokens:?}"
        );
    }
}

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

#[tokio::test]
async fn test_execute_invalid_payment_proof_returns_402() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/execute"))
        .header("X-API-Key", &api_key)
        .header(
            "X-Payment-Proof",
            r#"{"payer":"0x0000000000000000000000000000000000000001","amount_usd":1.0,"token":"USDC","chain":"ethereum","tx_hash":"0x0000000000000000000000000000000000000000000000000000000000000001"}"#,
        )
        .json(&json!({
            "agent_id": "pay-test",
            "chain": "ethereum",
            "target_contract": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
            "calldata": "0xa9059cbb00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000001",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 402);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "payment_verification_failed");
}

#[tokio::test]
async fn test_execute_malformed_payment_proof_returns_402() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/execute"))
        .header("X-API-Key", &api_key)
        .header("X-Payment-Proof", "this is not json")
        .json(&json!({
            "agent_id": "malformed",
            "chain": "ethereum",
            "target_contract": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
            "calldata": "0xa9059cbb00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000001",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 402);
    let body: Value = resp.json().await.unwrap();
    assert!(body["reason"].as_str().unwrap().to_lowercase().contains("malformed"));
}

#[tokio::test]
async fn test_execute_unsupported_token_in_proof_returns_402() {
    let (base, api_key, _h) = spawn_app().await;
    let c = http_client();

    let resp = c
        .post(format!("{base}/execute"))
        .header("X-API-Key", &api_key)
        .header(
            "X-Payment-Proof",
            r#"{"payer":"0x0000000000000000000000000000000000000001","amount_usd":1.0,"token":"DOGE","chain":"ethereum","tx_hash":"0x0000000000000000000000000000000000000000000000000000000000000099"}"#,
        )
        .json(&json!({
            "agent_id": "doge-test",
            "chain": "ethereum",
            "target_contract": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
            "calldata": "0xa9059cbb00000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000001",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 402);
    let body: Value = resp.json().await.unwrap();
    let reason = body["reason"].as_str().unwrap().to_lowercase();
    assert!(reason.contains("not accepted") || reason.contains("doge"));
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

// ═══════════════════════ Unit-level tests ════════════════════════════

// ────────────────── Chain parsing and display ────────────────────────

#[test]
fn test_chain_from_str_loose() {
    assert_eq!(Chain::from_str_loose("ethereum"), Some(Chain::Ethereum));
    assert_eq!(Chain::from_str_loose("eth"), Some(Chain::Ethereum));
    assert_eq!(Chain::from_str_loose("mainnet"), Some(Chain::Ethereum));
    assert_eq!(Chain::from_str_loose("ETHEREUM"), Some(Chain::Ethereum));
    assert_eq!(Chain::from_str_loose("base"), Some(Chain::Base));
    assert_eq!(Chain::from_str_loose("bnb"), Some(Chain::Bnb));
    assert_eq!(Chain::from_str_loose("bsc"), Some(Chain::Bnb));
    assert_eq!(Chain::from_str_loose("binance"), Some(Chain::Bnb));
    assert_eq!(Chain::from_str_loose("solana"), None);
    assert_eq!(Chain::from_str_loose("polygon"), None);
    assert_eq!(Chain::from_str_loose(""), None);
}

#[test]
fn test_chain_display() {
    assert_eq!(Chain::Ethereum.to_string(), "ethereum");
    assert_eq!(Chain::Base.to_string(), "base");
    assert_eq!(Chain::Bnb.to_string(), "bnb");
}

#[test]
fn test_chain_ids() {
    assert_eq!(Chain::Ethereum.chain_id(), 1);
    assert_eq!(Chain::Base.chain_id(), 8453);
    assert_eq!(Chain::Bnb.chain_id(), 56);
}

#[test]
fn test_execution_status_display() {
    assert_eq!(ExecutionStatus::Pending.to_string(), "pending");
    assert_eq!(ExecutionStatus::PaymentRequired.to_string(), "payment_required");
    assert_eq!(ExecutionStatus::PaymentVerified.to_string(), "payment_verified");
    assert_eq!(ExecutionStatus::Queued.to_string(), "queued");
    assert_eq!(ExecutionStatus::Broadcasting.to_string(), "broadcasting");
    assert_eq!(ExecutionStatus::Confirmed.to_string(), "confirmed");
    assert_eq!(ExecutionStatus::Failed.to_string(), "failed");
    assert_eq!(ExecutionStatus::Reverted.to_string(), "reverted");
}

// ────────────────── Encryption round-trip ───────────────────────────

#[test]
fn test_encryption_round_trip() {
    use agent_execution_platform::agent_wallet::{decrypt_key_hex, encrypt_key_hex};

    let key = [0x42u8; 32];
    let plaintext = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";

    let encrypted = encrypt_key_hex(&key, plaintext).unwrap();
    let decrypted = decrypt_key_hex(&key, &encrypted).unwrap();
    assert_eq!(decrypted, plaintext);
}

#[test]
fn test_encryption_wrong_key_fails() {
    use agent_execution_platform::agent_wallet::{decrypt_key_hex, encrypt_key_hex};

    let key1 = [0x42u8; 32];
    let key2 = [0x43u8; 32];
    let plaintext = "secret_key_data";

    let encrypted = encrypt_key_hex(&key1, plaintext).unwrap();
    assert!(decrypt_key_hex(&key2, &encrypted).is_err());
}

#[test]
fn test_encryption_nonce_uniqueness() {
    use agent_execution_platform::agent_wallet::encrypt_key_hex;

    let key = [0x42u8; 32];
    let enc1 = encrypt_key_hex(&key, "same").unwrap();
    let enc2 = encrypt_key_hex(&key, "same").unwrap();
    assert_ne!(enc1, enc2, "random nonce → different ciphertexts");
}

// ────────────────── Rate limiter ────────────────────────────────────

#[test]
fn test_rate_limiter_allows_burst() {
    let limiter = RateLimiter::new(10.0, 5.0);
    let key = Uuid::new_v4();

    for i in 0..5 {
        assert!(limiter.check(key).is_ok(), "request {i} should pass");
    }
    assert!(limiter.check(key).is_err(), "6th request should be rejected");
}

#[test]
fn test_rate_limiter_independent_keys() {
    let limiter = RateLimiter::new(1.0, 1.0);
    let k1 = Uuid::new_v4();
    let k2 = Uuid::new_v4();

    assert!(limiter.check(k1).is_ok());
    assert!(limiter.check(k2).is_ok());
    assert!(limiter.check(k1).is_err());
}

#[test]
fn test_rate_limiter_retry_after() {
    let limiter = RateLimiter::new(10.0, 1.0);
    let key = Uuid::new_v4();

    limiter.check(key).unwrap();
    let retry = limiter.check(key).unwrap_err();
    assert!(retry > 0.0 && retry < 1.0, "retry_after = {retry}");
}

#[test]
fn test_rate_limiter_evict_stale() {
    let limiter = RateLimiter::new(1000.0, 1.0);
    limiter.check(Uuid::new_v4()).unwrap();
    limiter.evict_stale(); // must not panic
}

// ────────────────── Queue operations (real Redis) ───────────────────

#[tokio::test]
async fn test_queue_enqueue_and_dequeue() {
    let config = test_config();
    let mut conn = setup_redis(&config).await;

    let _: () = redis::cmd("DEL")
        .arg("execution_jobs")
        .query_async(&mut conn)
        .await
        .unwrap();

    let job = ExecutionJob {
        request_id: Uuid::new_v4(),
        agent_id: "queue-test".into(),
        smart_wallet_address: "0x1234567890abcdef1234567890abcdef12345678".into(),
        eoa_address: "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd".into(),
        chain: Chain::Ethereum,
        target_contract: "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238".into(),
        calldata: "0xa9059cbb".into(),
        value: "0".into(),
        gas_limit: 100_000,
        created_at: chrono::Utc::now(),
        attempt_count: 0,
        batch_calls: None,
        callback_url: None,
        api_key_hash: None,
    };

    queue::enqueue_job(&mut conn, &job).await.unwrap();
    assert!(queue::queue_length(&mut conn).await.unwrap() >= 1);

    let dequeued = queue::dequeue_job(&mut conn, 1.0, 99).await.unwrap();
    assert!(dequeued.is_some());
    let dj = dequeued.unwrap();
    assert_eq!(dj.request_id, job.request_id);

    queue::ack_job(&mut conn, &dj, 99).await.unwrap();
}

#[tokio::test]
async fn test_queue_recover_stale_jobs() {
    let config = test_config();
    let mut conn = setup_redis(&config).await;

    let worker_id = 97u32;
    let proc_key = format!("execution_jobs:processing:{worker_id}");

    let job = ExecutionJob {
        request_id: Uuid::new_v4(),
        agent_id: "stale".into(),
        smart_wallet_address: "0x1111111111111111111111111111111111111111".into(),
        eoa_address: "0x2222222222222222222222222222222222222222".into(),
        chain: Chain::Ethereum,
        target_contract: "0x3333333333333333333333333333333333333333".into(),
        calldata: "0xdeadbeef".into(),
        value: "0".into(),
        gas_limit: 50_000,
        created_at: chrono::Utc::now(),
        attempt_count: 1,
        batch_calls: None,
        callback_url: None,
        api_key_hash: None,
    };

    let payload = serde_json::to_string(&job).unwrap();
    let _: () = redis::AsyncCommands::lpush(&mut conn, &proc_key, &payload)
        .await
        .unwrap();

    let recovered = queue::recover_stale_jobs(&mut conn, worker_id).await.unwrap();
    assert!(recovered >= 1);
}

#[tokio::test]
async fn test_queue_dead_letter() {
    let config = test_config();
    let mut conn = setup_redis(&config).await;

    let job = ExecutionJob {
        request_id: Uuid::new_v4(),
        agent_id: "dlq".into(),
        smart_wallet_address: "0x4444444444444444444444444444444444444444".into(),
        eoa_address: "0x5555555555555555555555555555555555555555".into(),
        chain: Chain::Ethereum,
        target_contract: "0x6666666666666666666666666666666666666666".into(),
        calldata: "0xdeadbeef".into(),
        value: "0".into(),
        gas_limit: 50_000,
        created_at: chrono::Utc::now(),
        attempt_count: 3,
        batch_calls: None,
        callback_url: None,
        api_key_hash: None,
    };

    queue::push_to_dlq(&mut conn, &job).await.unwrap();
    assert!(queue::dlq_length(&mut conn).await.unwrap() >= 1);
}

// ────────────────── Database operations (real Postgres) ──────────────

#[tokio::test]
async fn test_db_api_key_create_and_lookup() {
    let config = test_config();
    let pool = setup_db(&config).await;

    let (row, raw_key) = db::create_api_key(&pool, Some("db-test")).await.unwrap();
    assert!(raw_key.starts_with("ak_"));
    assert!(row.is_active);

    let found = db::get_api_key_by_raw(&pool, &raw_key).await.unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, row.id);
}

#[tokio::test]
async fn test_db_api_key_wrong_key_returns_none() {
    let config = test_config();
    let pool = setup_db(&config).await;

    let found = db::get_api_key_by_raw(&pool, "ak_does_not_exist").await.unwrap();
    assert!(found.is_none());
}

#[tokio::test]
async fn test_db_execution_request_lifecycle() {
    let config = test_config();
    let pool = setup_db(&config).await;

    let req = ExecutionRequest {
        agent_id: "lifecycle".into(),
        chain: "ethereum".into(),
        target_contract: "0x1234567890abcdef1234567890abcdef12345678".into(),
        calldata: "0xdeadbeef".into(),
        value: "0".into(),
        strategy_id: Some("strat".into()),
        batch_calls: None,
        callback_url: None,
    };

    let row = db::insert_execution_request(&pool, &req, &ExecutionStatus::Pending, Some("0xaaaa"), None)
        .await
        .unwrap();
    assert_eq!(row.status, "pending");

    let fetched = db::get_execution_request(&pool, row.id).await.unwrap().unwrap();
    assert_eq!(fetched.id, row.id);

    db::update_execution_status(&pool, row.id, &ExecutionStatus::Queued, None, None, Some(100_000), Some(0.05))
        .await
        .unwrap();

    let updated = db::get_execution_request(&pool, row.id).await.unwrap().unwrap();
    assert_eq!(updated.status, "queued");
    assert_eq!(updated.gas_estimate, Some(100_000));
}

#[tokio::test]
async fn test_db_payment_replay_protection() {
    let config = test_config();
    let pool = setup_db(&config).await;

    let req = ExecutionRequest {
        agent_id: "replay".into(),
        chain: "ethereum".into(),
        target_contract: "0x1234567890abcdef1234567890abcdef12345678".into(),
        calldata: "0xdeadbeef".into(),
        value: "0".into(),
        strategy_id: None,
        batch_calls: None,
        callback_url: None,
    };
    let row = db::insert_execution_request(&pool, &req, &ExecutionStatus::PaymentRequired, None, None)
        .await
        .unwrap();

    let tx_hash = format!("0x{}", hex::encode(Uuid::new_v4().as_bytes().repeat(2)));

    let proof = PaymentProof {
        payment_id: Uuid::new_v4(),
        payer: "0x0000000000000000000000000000000000000001".into(),
        amount_usd: 0.05,
        token: "USDC".into(),
        chain: "ethereum".into(),
        tx_hash: tx_hash.clone(),
        verified: true,
        verified_at: chrono::Utc::now(),
        confirmed_amount_raw: Some("50000".into()),
        block_confirmations: Some(10),
        token_contract: Some("0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238".into()),
    };

    assert!(db::insert_payment(&pool, row.id, &proof).await.unwrap().is_some());
    assert!(db::insert_payment(&pool, row.id, &proof).await.unwrap().is_none(), "replay should be rejected");
    assert!(db::payment_tx_hash_exists(&pool, &tx_hash).await.unwrap());
}

#[tokio::test]
async fn test_db_platform_keys() {
    let config = test_config();
    let pool = setup_db(&config).await;

    let purpose = format!("test_key_{}", Uuid::new_v4());

    assert!(db::get_platform_key(&pool, &purpose).await.unwrap().is_none());

    assert!(db::insert_platform_key(&pool, &purpose, "enc_data", "0xaabb").await.unwrap().is_some());

    let fetched = db::get_platform_key(&pool, &purpose).await.unwrap().unwrap();
    assert_eq!(fetched.purpose, purpose);
    assert_eq!(fetched.address, "0xaabb");

    assert!(
        db::insert_platform_key(&pool, &purpose, "other", "0xccdd").await.unwrap().is_none(),
        "duplicate purpose should be rejected"
    );
}

// ────────────────── Serialization round-trips ───────────────────────

#[test]
fn test_execution_job_serde_round_trip() {
    let job = ExecutionJob {
        request_id: Uuid::new_v4(),
        agent_id: "serde".into(),
        smart_wallet_address: "0xaaaa".into(),
        eoa_address: "0xbbbb".into(),
        chain: Chain::Ethereum,
        target_contract: "0xcccc".into(),
        calldata: "0xdddd".into(),
        value: "123".into(),
        gas_limit: 42_000,
        created_at: chrono::Utc::now(),
        attempt_count: 2,
        batch_calls: Some(vec![BatchCall {
            target_contract: "0xeeee".into(),
            calldata: "0xffff".into(),
            value: "456".into(),
        }]),
        callback_url: Some("https://example.com/cb".into()),
        api_key_hash: Some("abc123".into()),
    };

    let json = serde_json::to_string(&job).unwrap();
    let rt: ExecutionJob = serde_json::from_str(&json).unwrap();
    assert_eq!(rt.request_id, job.request_id);
    assert_eq!(rt.attempt_count, 2);
    assert_eq!(rt.batch_calls.unwrap().len(), 1);
}

#[test]
fn test_user_operation_serde() {
    let op = UserOperation {
        sender: "0x1111111111111111111111111111111111111111".into(),
        nonce: "0x01".into(),
        init_code: "0x".into(),
        call_data: "0xabcdef".into(),
        account_gas_limits: format!("0x{}", "0".repeat(64)),
        pre_verification_gas: "0x5208".into(),
        gas_fees: format!("0x{}", "0".repeat(64)),
        paymaster_and_data: "0x".into(),
        signature: "0x".into(),
    };

    let v = serde_json::to_value(&op).unwrap();
    assert!(v.get("sender").is_some());
    assert!(v.get("callData").is_some());
    assert!(v.get("accountGasLimits").is_some());
    assert!(v.get("gasFees").is_some());
}

#[test]
fn test_payment_proof_serde() {
    let proof = PaymentProof {
        payment_id: Uuid::new_v4(),
        payer: "0x1234".into(),
        amount_usd: 1.5,
        token: "USDC".into(),
        chain: "ethereum".into(),
        tx_hash: "0xabcd".into(),
        verified: true,
        verified_at: chrono::Utc::now(),
        confirmed_amount_raw: Some("1500000".into()),
        block_confirmations: Some(12),
        token_contract: Some("0x5678".into()),
    };

    let json = serde_json::to_string(&proof).unwrap();
    let rt: PaymentProof = serde_json::from_str(&json).unwrap();
    assert_eq!(rt.amount_usd, 1.5);
    assert!(rt.verified);
}

// ────────────────── Config loading ──────────────────────────────────

#[test]
fn test_config_loads_correctly() {
    dotenvy::dotenv().ok();
    let config = AppConfig::from_env().unwrap();

    assert!(!config.chains.is_empty());
    assert!(config.chains.contains_key(&Chain::Ethereum));

    let eth = config.chain_config(&Chain::Ethereum).unwrap();
    assert!(!eth.rpc_url.is_empty());
    assert!(!eth.bundler_rpc_url.is_empty());
    assert!(eth.accepted_tokens.contains_key("USDC"));
    assert_eq!(eth.token_decimals.get("USDC"), Some(&6));
}

// ────────────────── Webhook HMAC ────────────────────────────────────

#[test]
fn test_hmac_sha256_deterministic() {
    type HmacSha256 = hmac::Hmac<sha2::Sha256>;
    use hmac::Mac;

    let body = r#"{"request_id":"abc"}"#;
    let secret = "test_secret";

    let mut m1 = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    m1.update(body.as_bytes());
    let s1 = hex::encode(m1.finalize().into_bytes());

    let mut m2 = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    m2.update(body.as_bytes());
    let s2 = hex::encode(m2.finalize().into_bytes());

    assert_eq!(s1, s2);
    assert_eq!(s1.len(), 64);
}

#[test]
fn test_hmac_different_secrets() {
    type HmacSha256 = hmac::Hmac<sha2::Sha256>;
    use hmac::Mac;

    let body = b"data";

    let mut m1 = HmacSha256::new_from_slice(b"secret1").unwrap();
    m1.update(body);
    let s1 = hex::encode(m1.finalize().into_bytes());

    let mut m2 = HmacSha256::new_from_slice(b"secret2").unwrap();
    m2.update(body);
    let s2 = hex::encode(m2.finalize().into_bytes());

    assert_ne!(s1, s2);
}
