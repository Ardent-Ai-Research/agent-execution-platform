//! Axum route handlers — hackathon edition.
//!
//! In-memory state (HashMap), inline synchronous execution, no payment gate.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;
use tracing::{error, info};

use crate::api::services;
use crate::execution_engine::ExecutionEngine;
use crate::relayer::orchestrator::RelayerOrchestrator;
use crate::types::*;

/// Per-request state stored in memory.
#[derive(Debug, Clone)]
pub struct RequestRecord {
    pub request_id: Uuid,
    pub status: ExecutionStatus,
    pub chain: String,
    pub tx_hash: Option<String>,
    pub cost_usd: Option<f64>,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub engine: ExecutionEngine,
    pub orchestrator: RelayerOrchestrator,
    pub store: Arc<Mutex<HashMap<Uuid, RequestRecord>>>,
}

// ────────────────────── POST /execute ────────────────────────────────

pub async fn execute_handler(
    State(state): State<AppState>,
    Json(req): Json<ExecutionRequest>,
) -> impl IntoResponse {
    info!(agent = %req.agent_wallet_address, chain = %req.chain, "POST /execute");

    match services::handle_execute(&state.engine, &state.orchestrator, &state.store, &req).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => {
            error!(error = %e, "execute failed");
            let err_str = e.to_string();
            let is_client_error = err_str.contains("unsupported chain")
                || err_str.contains("invalid agent wallet")
                || err_str.contains("invalid target contract")
                || err_str.contains("calldata")
                || err_str.contains("invalid forwarder")
                || err_str.contains("invalid signature");
            let status = if is_client_error {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(serde_json::json!({ "error": err_str }))).into_response()
        }
    }
}

// ──────────────────── POST /simulate ───────────────────────

pub async fn simulate_handler(
    State(state): State<AppState>,
    Json(req): Json<ExecutionRequest>,
) -> impl IntoResponse {
    info!(agent = %req.agent_wallet_address, chain = %req.chain, "POST /simulate");

    match services::handle_simulate(&state.engine, &req).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => {
            error!(error = %e, "simulate failed");
            let err_str = e.to_string();
            let is_client_error = err_str.contains("unsupported chain")
                || err_str.contains("invalid agent wallet")
                || err_str.contains("invalid target contract")
                || err_str.contains("calldata")
                || err_str.contains("invalid forwarder")
                || err_str.contains("invalid signature");
            let status = if is_client_error {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(serde_json::json!({ "error": err_str }))).into_response()
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

    let store = state.store.lock().await;
    match store.get(&uuid) {
        Some(record) => {
            let resp = StatusResponse {
                request_id: record.request_id,
                status: record.status.clone(),
                chain: record.chain.clone(),
                tx_hash: record.tx_hash.clone(),
                cost_usd: record.cost_usd,
                created_at: record.created_at,
                updated_at: record.updated_at,
            };
            (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "request not found" })),
        )
            .into_response(),
    }
}

// ────────────────────── GET /health ──────────────────────────────────

pub async fn health_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
            "service": "agent-execution-platform",
            "version": env!("CARGO_PKG_VERSION"),
            "mode": "hackathon"
        })),
    )
}
