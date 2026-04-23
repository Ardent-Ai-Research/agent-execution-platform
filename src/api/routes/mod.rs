//! Axum route handlers for the Execution API.

use axum::{
    extract::{Extension, Path, Query, Request, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use uuid::Uuid;
use tracing::{error, info};

use std::collections::HashMap;

use crate::agent_wallet::AgentWalletRegistry;
use crate::api::services;
use crate::config::AppConfig;
use crate::db;
use crate::execution_engine::ExecutionEngine;
use crate::payments::PaymentRequiredBody;
use crate::relayer::erc4337::BundlerClient;
use crate::types::*;

/// Shared application state injected into every handler.
#[derive(Clone)]
pub struct AppState {
    pub db_pool: PgPool,
    pub redis_conn: ConnectionManager,
    pub engine: ExecutionEngine,
    pub config: AppConfig,
    pub wallet_registry: AgentWalletRegistry,
    /// Per-chain bundler clients.  Keyed by [`Chain`].
    pub bundler_clients: HashMap<Chain, BundlerClient>,
}

// ────────────────────── POST /execute ────────────────────────────────

pub async fn execute_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    payment_proof: Option<Extension<PaymentProof>>,
    Json(req): Json<ExecutionRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, "POST /execute");

    let proof_ref = payment_proof.as_ref().map(|p| &p.0);
    let mut redis = state.redis_conn.clone();

    match services::handle_execute(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        api_ctx.api_key_id,
        &req,
        proof_ref,
    ).await {
        Ok(resp) => {
            // If payment is required, return 402
            if resp.status == ExecutionStatus::PaymentRequired {
                let quoted_usd = resp.estimated_cost_usd.unwrap_or(0.0);
                // Resolve the chain to get per-chain accepted tokens
                let (accepted, required_amount_raw) = Chain::from_str_loose(&req.chain)
                    .and_then(|c| state.config.chains.get(&c))
                    .map(|cfg| {
                        let accepted = cfg.accepted_tokens.keys().cloned().collect::<Vec<_>>();
                        let required_amount_raw = cfg
                            .accepted_tokens
                            .keys()
                            .map(|symbol| {
                                let decimals = cfg.token_decimals.get(symbol).copied().unwrap_or(6);
                                let raw = (quoted_usd * 10f64.powi(decimals as i32)) as u128;
                                (symbol.clone(), raw.to_string())
                            })
                            .collect::<HashMap<_, _>>();
                        (accepted, required_amount_raw)
                    })
                    .unwrap_or_else(|| (Vec::new(), HashMap::new()));
                let body = PaymentRequiredBody {
                    error: "payment_required".into(),
                    amount_usd: quoted_usd,
                    accepted_tokens: accepted,
                    required_amount_raw,
                    payment_address: state.config.payment_address.clone(),
                    chain: req.chain.clone(),
                    request_id: resp.request_id.to_string(),
                    smart_wallet_address: resp.smart_wallet_address.clone().unwrap_or_default(),
                };
                return (StatusCode::PAYMENT_REQUIRED, Json(serde_json::to_value(body).unwrap())).into_response();
            }
            (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response()
        }
        Err(e) => {
            error!(error = %e, "execute failed");
            // Distinguish client errors from internal server errors.
            // Validation / parsing errors are client faults (400);
            // DB / RPC / queue errors are server faults (500).
            let err_str = e.to_string();
            let is_client_error = err_str.contains("unsupported chain")
                || err_str.contains("not configured")
                || err_str.contains("no bundler configured")
                || err_str.contains("agent_id")
                || err_str.contains("invalid target contract")
                || err_str.contains("calldata")
                || err_str.contains("malformed");
            let status = if is_client_error {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(serde_json::json!({ "error": err_str })),
            )
                .into_response()
        }
    }
}

// ──────────────────── POST /simulate ───────────────────────

pub async fn simulate_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<ExecutionRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, "POST /simulate");

    match services::handle_simulate(
        &state.engine,
        &state.db_pool,
        &state.wallet_registry,
        &state.bundler_clients,
        api_ctx.api_key_id,
        &req,
    ).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => {
            error!(error = %e, "simulate failed");
            let err_str = e.to_string();
            let is_client_error = err_str.contains("unsupported chain")
                || err_str.contains("not configured")
                || err_str.contains("no bundler configured")
                || err_str.contains("agent_id")
                || err_str.contains("invalid target contract")
                || err_str.contains("calldata")
                || err_str.contains("malformed");
            let status = if is_client_error {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(serde_json::json!({ "error": err_str })),
            )
                .into_response()
        }
    }
}

// ────────────────────── GET /status/:id ──────────────────────────────

pub async fn status_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    info!(request_id = %id, "GET /status");

    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid UUID" })),
            )
                .into_response();
        }
    };

    match db::get_execution_request(&state.db_pool, uuid).await {
        Ok(Some(row)) => {
            let resp = StatusResponse {
                request_id: row.id,
                status: serde_json::from_value(
                    serde_json::Value::String(row.status.clone()),
                )
                .unwrap_or(ExecutionStatus::Pending),
                chain: row.chain,
                tx_hash: row.tx_hash,
                cost_usd: row.cost_usd,
                created_at: row.created_at,
                updated_at: row.updated_at,
            };
            (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "request not found" })),
        )
            .into_response(),
        Err(e) => {
            error!(error = %e, "status lookup failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "internal server error" })),
            )
                .into_response()
        }
    }
}

// ────────────────────── GET /health ──────────────────────────────────

pub async fn health_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
    // Deep health check: verify DB and Redis are reachable.
    let db_ok = sqlx::query("SELECT 1")
        .execute(&state.db_pool)
        .await
        .is_ok();

    let mut redis = state.redis_conn.clone();
    let redis_ok = redis::cmd("PING")
        .query_async::<_, String>(&mut redis)
        .await
        .is_ok();

    let all_ok = db_ok && redis_ok;
    let status = if all_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        Json(serde_json::json!({
            "status": if all_ok { "ok" } else { "degraded" },
            "service": "agent-execution-platform",
            "version": env!("CARGO_PKG_VERSION"),
            "checks": {
                "database": if db_ok { "ok" } else { "unreachable" },
                "redis": if redis_ok { "ok" } else { "unreachable" },
            }
        })),
    )
}

// ────────────────────── GET /wallet ──────────────────────────────────

/// Query parameters for `GET /wallet`.
#[derive(Debug, serde::Deserialize)]
pub struct WalletQuery {
    /// The agent identifier (same as used in /execute).
    pub agent_id: String,
    /// The blockchain to check deployment status on (default: "ethereum").
    #[serde(default = "default_chain")]
    pub chain: String,
}

fn default_chain() -> String {
    "ethereum".to_string()
}

/// Look up (or provision) the agent's smart wallet address.
///
/// This is a lightweight, free endpoint that returns the agent's ERC-4337
/// smart wallet address. The agent should fund this address with whatever
/// tokens their strategy needs before calling `/execute`.
///
/// No payment or simulation is performed.
pub async fn wallet_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Query(params): Query<WalletQuery>,
) -> impl IntoResponse {
    info!(agent_id = %params.agent_id, chain = %params.chain, "GET /wallet");

    match services::handle_get_wallet(
        &state.engine,
        &state.wallet_registry,
        api_ctx.api_key_id,
        &params.agent_id,
        &params.chain,
    )
    .await
    {
        Ok(resp) => (
            StatusCode::OK,
            Json(serde_json::to_value(resp).unwrap()),
        )
            .into_response(),
        Err(e) => {
            error!(error = %e, "wallet lookup failed");
            let err_str = e.to_string();
            let is_client_error = err_str.contains("unsupported chain")
                || err_str.contains("not configured")
                || err_str.contains("agent_id");
            let status = if is_client_error {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(serde_json::json!({ "error": err_str })),
            )
                .into_response()
        }
    }
}

// ────────────────────── POST /admin/api-keys ─────────────────────────

/// Request body for API key creation.
#[derive(Debug, serde::Deserialize)]
pub struct CreateApiKeyRequest {
    /// Optional human-readable label for the API key.
    pub label: Option<String>,
}

/// Create a new API key (admin-only).
///
/// Protected by the `ADMIN_BEARER_TOKEN` env var — callers must send
/// `Authorization: Bearer <token>`.  Returns the raw API key exactly once;
/// it is never stored in plaintext.
pub async fn create_api_key_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateApiKeyRequest>,
) -> impl IntoResponse {
    info!("POST /admin/api-keys");

    match db::create_api_key(&state.db_pool, body.label.as_deref()).await {
        Ok((row, raw_key)) => {
            info!(api_key_id = %row.id, "new API key created");
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "api_key_id": row.id,
                    "api_key": raw_key,
                    "label": row.label,
                    "created_at": row.created_at,
                    "message": "Store this API key securely — it will not be shown again."
                })),
            )
                .into_response()
        }
        Err(e) => {
            error!(error = %e, "failed to create API key");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "failed to create API key" })),
            )
                .into_response()
        }
    }
}

/// Admin authentication middleware.
///
/// Checks the `Authorization: Bearer <token>` header against the
/// `ADMIN_BEARER_TOKEN` environment variable. If the env var is not set,
/// the admin endpoints are disabled (all requests get 403).
pub async fn admin_auth_middleware(
    req: Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    let expected = std::env::var("ADMIN_BEARER_TOKEN").unwrap_or_default();

    if expected.is_empty() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "admin endpoints disabled — set ADMIN_BEARER_TOKEN env var"
            })),
        )
            .into_response();
    }

    let provided = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    if provided != expected {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid admin token" })),
        )
            .into_response();
    }

    next.run(req).await.into_response()
}
