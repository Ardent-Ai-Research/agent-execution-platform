//! Axum route handlers for the Execution API.

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use uuid::Uuid;
use tracing::{error, info};

use crate::api::services;
use crate::config::AppConfig;
use crate::db;
use crate::execution_engine::ExecutionEngine;
use crate::payments::PaymentRequiredBody;
use crate::types::*;

/// Shared application state injected into every handler.
#[derive(Clone)]
pub struct AppState {
    pub db_pool: PgPool,
    pub redis_conn: ConnectionManager,
    pub engine: ExecutionEngine,
    pub config: AppConfig,
}

// ────────────────────── POST /execute ────────────────────────────────

pub async fn execute_handler(
    State(state): State<AppState>,
    payment_proof: Option<Extension<PaymentProof>>,
    Json(req): Json<ExecutionRequest>,
) -> impl IntoResponse {
    info!(agent = %req.agent_wallet_address, chain = %req.chain, "POST /execute");

    let proof_ref = payment_proof.as_ref().map(|p| &p.0);
    let mut redis = state.redis_conn.clone();

    match services::handle_execute(&state.engine, &state.db_pool, &mut redis, &req, proof_ref).await {
        Ok(resp) => {
            // If payment is required, return 402
            if resp.status == ExecutionStatus::PaymentRequired {
                let body = PaymentRequiredBody {
                    error: "payment_required".into(),
                    amount_usd: resp.estimated_cost_usd.unwrap_or(0.0),
                    accepted_tokens: state.config.accepted_tokens.keys().cloned().collect(),
                    payment_address: state.config.payment_address.clone(),
                    chain: req.chain.clone(),
                    request_id: resp.request_id.to_string(),
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
                || err_str.contains("invalid agent wallet")
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
    Json(req): Json<ExecutionRequest>,
) -> impl IntoResponse {
    info!(agent = %req.agent_wallet_address, chain = %req.chain, "POST /simulate");

    match services::handle_simulate(&state.engine, &state.db_pool, &req).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => {
            error!(error = %e, "simulate failed");
            let err_str = e.to_string();
            let is_client_error = err_str.contains("unsupported chain")
                || err_str.contains("invalid agent wallet")
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
