//! Axum route handlers for the Execution API.

use axum::{
    extract::{Extension, Path, Query, Request, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use tracing::{error, info};
use uuid::Uuid;

use std::collections::HashMap;

use crate::agent_wallet::AgentWalletRegistry;
use crate::api::services;
use crate::config::AppConfig;
use crate::db;
use crate::execution_engine::ExecutionEngine;
use crate::payments::PaymentRequiredBody;
use crate::protocols::aave_v3::{
    service as aave_v3_service, AaveBalancesQuery, AaveBorrowRequest, AavePositionQuery,
    AaveRepayRequest, AaveSupplyRequest, AaveWithdrawRequest,
};
use crate::protocols::compound_v3::{
    service as compound_v3_service, CompoundBalancesQuery, CompoundBorrowRequest,
    CompoundPositionQuery, CompoundRepayRequest, CompoundSupplyRequest, CompoundWithdrawRequest,
};
use crate::protocols::gmx_v2::{
    service as gmx_v2_service, GmxAccountQuery, GmxCancelOrderRequest, GmxCancelRequest,
    GmxClaimRequest, GmxCreateDepositRequest, GmxCreateOrderRequest, GmxCreateWithdrawalRequest,
    GmxMarketsQuery, GmxUpdateOrderRequest,
};
use crate::relayer::erc4337::BundlerClient;
use crate::relayer::paymaster::PaymasterSigner;
use crate::types::*;

#[derive(Debug, serde::Deserialize)]
pub struct FeedQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Serialize)]
pub struct FeedResponse {
    pub items: Vec<FeedItem>,
}

#[derive(Debug, serde::Serialize)]
pub struct FeedItem {
    pub r#type: String,
    pub type_label: String,
    pub agent: String,
    pub detail: String,
    pub hash: String,
    pub status: String,
    pub confirm: String,
}

fn short_hash(value: &str) -> String {
    if value.len() <= 14 {
        return value.to_string();
    }
    let head = &value[..8];
    let tail = &value[value.len().saturating_sub(4)..];
    format!("{}...{}", head, tail)
}

fn chain_display(chain: &str) -> String {
    match chain.to_lowercase().as_str() {
        "ethereum" | "eth" => "Ethereum Sepolia".to_string(),
        "sepolia" => "Ethereum Sepolia".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => "Unknown".to_string(),
            }
        }
    }
}

fn format_amount_usd(amount: f64) -> String {
    if amount >= 100.0 {
        format!("${:.0}", amount)
    } else {
        format!("${:.2}", amount)
    }
}

fn usd_to_raw_amount_ceil(usd: f64, decimals: u8) -> Option<String> {
    if !usd.is_finite() || usd < 0.0 {
        return None;
    }
    let scaled = usd * 10f64.powi(decimals as i32);
    if !scaled.is_finite() || scaled < 0.0 || scaled > u128::MAX as f64 {
        return None;
    }
    Some((scaled.ceil() as u128).to_string())
}

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
    /// Per-chain paymaster signers for sponsored ERC-4337 operations.
    pub paymaster_signers: HashMap<Chain, PaymasterSigner>,
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
        &state.paymaster_signers,
        api_ctx.api_key_id,
        api_ctx.payment_mode.clone(),
        &req,
        proof_ref,
    )
    .await
    {
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
                                let raw = usd_to_raw_amount_ceil(quoted_usd, decimals)
                                    .unwrap_or_else(|| "0".to_string());
                                (symbol.clone(), raw)
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
                return (
                    StatusCode::PAYMENT_REQUIRED,
                    Json(serde_json::to_value(body).unwrap()),
                )
                    .into_response();
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
            (status, Json(serde_json::json!({ "error": err_str }))).into_response()
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
        &state.paymaster_signers,
        api_ctx.api_key_id,
        api_ctx.payment_mode.clone(),
        &req,
    )
    .await
    {
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
            (status, Json(serde_json::json!({ "error": err_str }))).into_response()
        }
    }
}

pub async fn aave_supply_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    payment_proof: Option<Extension<PaymentProof>>,
    Json(req): Json<AaveSupplyRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, "POST /protocols/aave-v3/supply");

    let proof_ref = payment_proof.as_ref().map(|p| &p.0);
    let mut redis = state.redis_conn.clone();

    match aave_v3_service::handle_supply(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        api_ctx.payment_mode.clone(),
        &req,
        proof_ref,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(&state, &req.chain, resp),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn aave_supply_simulate_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<AaveSupplyRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, "POST /protocols/aave-v3/supply/simulate");

    match aave_v3_service::handle_supply_simulate(
        &state.engine,
        &state.db_pool,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        api_ctx.payment_mode.clone(),
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn aave_withdraw_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    payment_proof: Option<Extension<PaymentProof>>,
    Json(req): Json<AaveWithdrawRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, "POST /protocols/aave-v3/withdraw");

    let proof_ref = payment_proof.as_ref().map(|p| &p.0);
    let mut redis = state.redis_conn.clone();

    match aave_v3_service::handle_withdraw(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        api_ctx.payment_mode.clone(),
        &req,
        proof_ref,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(&state, &req.chain, resp),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn aave_withdraw_simulate_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<AaveWithdrawRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, "POST /protocols/aave-v3/withdraw/simulate");

    match aave_v3_service::handle_withdraw_simulate(
        &state.engine,
        &state.db_pool,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        api_ctx.payment_mode.clone(),
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn aave_repay_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    payment_proof: Option<Extension<PaymentProof>>,
    Json(req): Json<AaveRepayRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, "POST /protocols/aave-v3/repay");

    let proof_ref = payment_proof.as_ref().map(|p| &p.0);
    let mut redis = state.redis_conn.clone();

    match aave_v3_service::handle_repay(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        api_ctx.payment_mode.clone(),
        &req,
        proof_ref,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(&state, &req.chain, resp),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn aave_repay_simulate_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<AaveRepayRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, "POST /protocols/aave-v3/repay/simulate");

    match aave_v3_service::handle_repay_simulate(
        &state.engine,
        &state.db_pool,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        api_ctx.payment_mode.clone(),
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn aave_borrow_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    payment_proof: Option<Extension<PaymentProof>>,
    Json(req): Json<AaveBorrowRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, "POST /protocols/aave-v3/borrow");

    let proof_ref = payment_proof.as_ref().map(|p| &p.0);
    let mut redis = state.redis_conn.clone();

    match aave_v3_service::handle_borrow(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        api_ctx.payment_mode.clone(),
        &req,
        proof_ref,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(&state, &req.chain, resp),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn aave_borrow_simulate_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<AaveBorrowRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, "POST /protocols/aave-v3/borrow/simulate");

    match aave_v3_service::handle_borrow_simulate(
        &state.engine,
        &state.db_pool,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        api_ctx.payment_mode.clone(),
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn aave_position_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<AavePositionQuery>,
) -> impl IntoResponse {
    info!(agent_id = %query.agent_id, chain = %query.chain, "GET /protocols/aave-v3/position");

    match aave_v3_service::handle_position(
        &state.engine,
        &state.wallet_registry,
        api_ctx.api_key_id,
        &query,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn aave_balances_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<AaveBalancesQuery>,
) -> impl IntoResponse {
    info!(agent_id = %query.agent_id, chain = %query.chain, "GET /protocols/aave-v3/balances");

    match aave_v3_service::handle_balances(
        &state.engine,
        &state.wallet_registry,
        api_ctx.api_key_id,
        &query,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

macro_rules! compound_execute_handler {
    ($fn_name:ident, $req_ty:ty, $service_fn:path, $log_name:literal) => {
        pub async fn $fn_name(
            State(state): State<AppState>,
            Extension(api_ctx): Extension<ApiKeyContext>,
            payment_proof: Option<Extension<PaymentProof>>,
            Json(req): Json<$req_ty>,
        ) -> impl IntoResponse {
            info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, $log_name);
            let proof_ref = payment_proof.as_ref().map(|p| &p.0);
            let mut redis = state.redis_conn.clone();
            match $service_fn(
                &state.engine,
                &state.db_pool,
                &mut redis,
                &state.wallet_registry,
                &state.bundler_clients,
                &state.paymaster_signers,
                api_ctx.api_key_id,
                api_ctx.payment_mode.clone(),
                &req,
                proof_ref,
            )
            .await
            {
                Ok(resp) => execution_response_to_http(&state, &req.chain, resp),
                Err(e) => protocol_error_to_http(e),
            }
        }
    };
}

macro_rules! compound_simulate_handler {
    ($fn_name:ident, $req_ty:ty, $service_fn:path, $log_name:literal) => {
        pub async fn $fn_name(
            State(state): State<AppState>,
            Extension(api_ctx): Extension<ApiKeyContext>,
            Json(req): Json<$req_ty>,
        ) -> impl IntoResponse {
            info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, $log_name);
            match $service_fn(
                &state.engine,
                &state.db_pool,
                &state.wallet_registry,
                &state.bundler_clients,
                &state.paymaster_signers,
                api_ctx.api_key_id,
                api_ctx.payment_mode.clone(),
                &req,
            )
            .await
            {
                Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
                Err(e) => protocol_error_to_http(e),
            }
        }
    };
}

compound_execute_handler!(
    compound_supply_handler,
    CompoundSupplyRequest,
    compound_v3_service::handle_supply,
    "POST /protocols/compound-v3/supply"
);
compound_simulate_handler!(
    compound_supply_simulate_handler,
    CompoundSupplyRequest,
    compound_v3_service::handle_supply_simulate,
    "POST /protocols/compound-v3/supply/simulate"
);
compound_execute_handler!(
    compound_withdraw_handler,
    CompoundWithdrawRequest,
    compound_v3_service::handle_withdraw,
    "POST /protocols/compound-v3/withdraw"
);
compound_simulate_handler!(
    compound_withdraw_simulate_handler,
    CompoundWithdrawRequest,
    compound_v3_service::handle_withdraw_simulate,
    "POST /protocols/compound-v3/withdraw/simulate"
);
compound_execute_handler!(
    compound_repay_handler,
    CompoundRepayRequest,
    compound_v3_service::handle_repay,
    "POST /protocols/compound-v3/repay"
);
compound_simulate_handler!(
    compound_repay_simulate_handler,
    CompoundRepayRequest,
    compound_v3_service::handle_repay_simulate,
    "POST /protocols/compound-v3/repay/simulate"
);
compound_execute_handler!(
    compound_borrow_handler,
    CompoundBorrowRequest,
    compound_v3_service::handle_borrow,
    "POST /protocols/compound-v3/borrow"
);
compound_simulate_handler!(
    compound_borrow_simulate_handler,
    CompoundBorrowRequest,
    compound_v3_service::handle_borrow_simulate,
    "POST /protocols/compound-v3/borrow/simulate"
);

pub async fn compound_position_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<CompoundPositionQuery>,
) -> impl IntoResponse {
    info!(agent_id = %query.agent_id, chain = %query.chain, "GET /protocols/compound-v3/position");
    match compound_v3_service::handle_position(
        &state.engine,
        &state.wallet_registry,
        api_ctx.api_key_id,
        &query,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn compound_balances_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<CompoundBalancesQuery>,
) -> impl IntoResponse {
    info!(agent_id = %query.agent_id, chain = %query.chain, "GET /protocols/compound-v3/balances");
    match compound_v3_service::handle_balances(
        &state.engine,
        &state.wallet_registry,
        api_ctx.api_key_id,
        &query,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn gmx_create_order_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    payment_proof: Option<Extension<PaymentProof>>,
    Json(req): Json<GmxCreateOrderRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, order_type = %req.order_type, "POST /protocols/gmx-v2/orders");

    let proof_ref = payment_proof.as_ref().map(|p| &p.0);
    let mut redis = state.redis_conn.clone();

    match gmx_v2_service::handle_create_order(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        api_ctx.payment_mode.clone(),
        &req,
        proof_ref,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(&state, &req.chain, resp),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn gmx_create_order_simulate_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<GmxCreateOrderRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, order_type = %req.order_type, "POST /protocols/gmx-v2/orders/simulate");

    match gmx_v2_service::handle_create_order_simulate(
        &state.engine,
        &state.db_pool,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        api_ctx.payment_mode.clone(),
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn gmx_cancel_order_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    payment_proof: Option<Extension<PaymentProof>>,
    Json(req): Json<GmxCancelOrderRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, order_key = %req.order_key, "POST /protocols/gmx-v2/orders/cancel");

    let proof_ref = payment_proof.as_ref().map(|p| &p.0);
    let mut redis = state.redis_conn.clone();

    match gmx_v2_service::handle_cancel_order(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        api_ctx.payment_mode.clone(),
        &req,
        proof_ref,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(&state, &req.chain, resp),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn gmx_cancel_order_simulate_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<GmxCancelOrderRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, order_key = %req.order_key, "POST /protocols/gmx-v2/orders/cancel/simulate");

    match gmx_v2_service::handle_cancel_order_simulate(
        &state.engine,
        &state.db_pool,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        api_ctx.payment_mode.clone(),
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn gmx_markets_handler(
    State(state): State<AppState>,
    Query(query): Query<GmxMarketsQuery>,
) -> impl IntoResponse {
    info!(chain = %query.chain, "GET /protocols/gmx-v2/markets");
    match gmx_v2_service::handle_markets(&state.engine, &query).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn gmx_positions_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<GmxAccountQuery>,
) -> impl IntoResponse {
    info!(agent_id = %query.agent_id, chain = %query.chain, "GET /protocols/gmx-v2/positions");
    match gmx_v2_service::handle_positions(
        &state.engine,
        &state.wallet_registry,
        api_ctx.api_key_id,
        &query,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn gmx_orders_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<GmxAccountQuery>,
) -> impl IntoResponse {
    info!(agent_id = %query.agent_id, chain = %query.chain, "GET /protocols/gmx-v2/orders");
    match gmx_v2_service::handle_orders(
        &state.engine,
        &state.wallet_registry,
        api_ctx.api_key_id,
        &query,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn gmx_balances_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<GmxAccountQuery>,
) -> impl IntoResponse {
    info!(agent_id = %query.agent_id, chain = %query.chain, "GET /protocols/gmx-v2/balances");
    match gmx_v2_service::handle_balances(
        &state.engine,
        &state.wallet_registry,
        api_ctx.api_key_id,
        &query,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

macro_rules! gmx_execute_handler {
    ($fn_name:ident, $req_ty:ty, $service_fn:path, $log_name:literal) => {
        pub async fn $fn_name(
            State(state): State<AppState>,
            Extension(api_ctx): Extension<ApiKeyContext>,
            payment_proof: Option<Extension<PaymentProof>>,
            Json(req): Json<$req_ty>,
        ) -> impl IntoResponse {
            info!(agent_id = %req.agent_id, chain = %req.chain, $log_name);
            let proof_ref = payment_proof.as_ref().map(|p| &p.0);
            let mut redis = state.redis_conn.clone();
            match $service_fn(
                &state.engine,
                &state.db_pool,
                &mut redis,
                &state.wallet_registry,
                &state.bundler_clients,
                &state.paymaster_signers,
                api_ctx.api_key_id,
                api_ctx.payment_mode.clone(),
                &req,
                proof_ref,
            )
            .await
            {
                Ok(resp) => execution_response_to_http(&state, &req.chain, resp),
                Err(e) => protocol_error_to_http(e),
            }
        }
    };
}

macro_rules! gmx_simulate_handler {
    ($fn_name:ident, $req_ty:ty, $service_fn:path, $log_name:literal) => {
        pub async fn $fn_name(
            State(state): State<AppState>,
            Extension(api_ctx): Extension<ApiKeyContext>,
            Json(req): Json<$req_ty>,
        ) -> impl IntoResponse {
            info!(agent_id = %req.agent_id, chain = %req.chain, $log_name);
            match $service_fn(
                &state.engine,
                &state.db_pool,
                &state.wallet_registry,
                &state.bundler_clients,
                &state.paymaster_signers,
                api_ctx.api_key_id,
                api_ctx.payment_mode.clone(),
                &req,
            )
            .await
            {
                Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
                Err(e) => protocol_error_to_http(e),
            }
        }
    };
}

gmx_execute_handler!(
    gmx_update_order_handler,
    GmxUpdateOrderRequest,
    gmx_v2_service::handle_update_order,
    "POST /protocols/gmx-v2/orders/update"
);
gmx_simulate_handler!(
    gmx_update_order_simulate_handler,
    GmxUpdateOrderRequest,
    gmx_v2_service::handle_update_order_simulate,
    "POST /protocols/gmx-v2/orders/update/simulate"
);
gmx_execute_handler!(
    gmx_create_deposit_handler,
    GmxCreateDepositRequest,
    gmx_v2_service::handle_create_deposit,
    "POST /protocols/gmx-v2/deposits"
);
gmx_simulate_handler!(
    gmx_create_deposit_simulate_handler,
    GmxCreateDepositRequest,
    gmx_v2_service::handle_create_deposit_simulate,
    "POST /protocols/gmx-v2/deposits/simulate"
);
gmx_execute_handler!(
    gmx_create_withdrawal_handler,
    GmxCreateWithdrawalRequest,
    gmx_v2_service::handle_create_withdrawal,
    "POST /protocols/gmx-v2/withdrawals"
);
gmx_simulate_handler!(
    gmx_create_withdrawal_simulate_handler,
    GmxCreateWithdrawalRequest,
    gmx_v2_service::handle_create_withdrawal_simulate,
    "POST /protocols/gmx-v2/withdrawals/simulate"
);
gmx_execute_handler!(
    gmx_cancel_handler,
    GmxCancelRequest,
    gmx_v2_service::handle_cancel,
    "POST /protocols/gmx-v2/requests/cancel"
);
gmx_simulate_handler!(
    gmx_cancel_simulate_handler,
    GmxCancelRequest,
    gmx_v2_service::handle_cancel_simulate,
    "POST /protocols/gmx-v2/requests/cancel/simulate"
);
gmx_execute_handler!(
    gmx_claim_handler,
    GmxClaimRequest,
    gmx_v2_service::handle_claim,
    "POST /protocols/gmx-v2/claims"
);
gmx_simulate_handler!(
    gmx_claim_simulate_handler,
    GmxClaimRequest,
    gmx_v2_service::handle_claim_simulate,
    "POST /protocols/gmx-v2/claims/simulate"
);

fn execution_response_to_http(
    state: &AppState,
    chain: &str,
    resp: ExecutionResponse,
) -> axum::response::Response {
    if resp.status == ExecutionStatus::PaymentRequired {
        let quoted_usd = resp.estimated_cost_usd.unwrap_or(0.0);
        let (accepted, required_amount_raw) = Chain::from_str_loose(chain)
            .and_then(|c| state.config.chains.get(&c))
            .map(|cfg| {
                let accepted = cfg.accepted_tokens.keys().cloned().collect::<Vec<_>>();
                let required_amount_raw = cfg
                    .accepted_tokens
                    .keys()
                    .map(|symbol| {
                        let decimals = cfg.token_decimals.get(symbol).copied().unwrap_or(6);
                        let raw = usd_to_raw_amount_ceil(quoted_usd, decimals)
                            .unwrap_or_else(|| "0".to_string());
                        (symbol.clone(), raw)
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
            chain: chain.to_string(),
            request_id: resp.request_id.to_string(),
            smart_wallet_address: resp.smart_wallet_address.clone().unwrap_or_default(),
        };
        return (
            StatusCode::PAYMENT_REQUIRED,
            Json(serde_json::to_value(body).unwrap()),
        )
            .into_response();
    }

    (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response()
}

fn protocol_error_to_http(e: anyhow::Error) -> axum::response::Response {
    error!(error = %e, "protocol action failed");
    let err_str = e.to_string();
    let is_client_error = err_str.contains("unsupported")
        || err_str.contains("required")
        || err_str.contains("amount")
        || err_str.contains("asset")
        || err_str.contains("provide either")
        || err_str.contains("chain")
        || err_str.contains("rejected")
        || err_str.contains("exceeds")
        || err_str.contains("collateral")
        || err_str.contains("must be");
    let status = if is_client_error {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (status, Json(serde_json::json!({ "error": err_str }))).into_response()
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
                status: serde_json::from_value(serde_json::Value::String(row.status.clone()))
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

pub async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
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

// ────────────────────── GET /feed/recent ────────────────────────────

pub async fn public_feed_handler(
    State(state): State<AppState>,
    Query(params): Query<FeedQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(12).clamp(1, 50) as i64;

    match db::get_recent_feed_rows(&state.db_pool, limit).await {
        Ok(rows) => {
            let items = rows
                .into_iter()
                .map(|row| {
                    let normalized_status = row.status.to_lowercase();
                    let (feed_type, type_label) = match normalized_status.as_str() {
                        "payment_required" | "payment_verified" => ("pay", "PAY"),
                        "pending" => ("reg", "REG"),
                        "broadcasting" => ("relay", "RELAY"),
                        _ => ("exec", "EXEC"),
                    };

                    let status_ok =
                        matches!(normalized_status.as_str(), "confirmed" | "payment_verified");

                    let confirm = match normalized_status.as_str() {
                        "confirmed" | "payment_verified" => "Confirmed",
                        "failed" | "reverted" => "Failed",
                        "payment_required" => "Payment Required",
                        _ => "Pending...",
                    };

                    let hash = row
                        .tx_hash
                        .as_deref()
                        .map(short_hash)
                        .unwrap_or_else(|| short_hash(&row.id.to_string()));

                    let agent = format!("agent-{}", row.id.to_string()[..8].to_string());

                    let display_chain =
                        chain_display(row.payment_chain.as_deref().unwrap_or(&row.chain));

                    let detail = match normalized_status.as_str() {
                        "payment_verified" => {
                            let amount = row
                                .payment_amount_usd
                                .map(format_amount_usd)
                                .unwrap_or_else(|| "$0.00".to_string());
                            let token = row
                                .payment_token
                                .clone()
                                .unwrap_or_else(|| "token".to_string());
                            format!("x402 verify · {} {} · {}", amount, token, display_chain)
                        }
                        "payment_required" => {
                            format!("x402 quote required · {}", display_chain)
                        }
                        "broadcasting" => {
                            format!("bundler relay · {}", display_chain)
                        }
                        "pending" | "queued" => {
                            format!("request queued · {}", display_chain)
                        }
                        "failed" | "reverted" => {
                            format!("execution failed · {}", display_chain)
                        }
                        _ => {
                            format!("contract execution · {}", display_chain)
                        }
                    };

                    FeedItem {
                        r#type: feed_type.to_string(),
                        type_label: type_label.to_string(),
                        agent,
                        detail,
                        hash,
                        status: if status_ok {
                            "ok".to_string()
                        } else {
                            "pending".to_string()
                        },
                        confirm: confirm.to_string(),
                    }
                })
                .collect::<Vec<_>>();

            (
                StatusCode::OK,
                Json(serde_json::to_value(FeedResponse { items }).unwrap()),
            )
                .into_response()
        }
        Err(e) => {
            error!(error = %e, "failed to load recent feed activity");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "failed to load recent activity" })),
            )
                .into_response()
        }
    }
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
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
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
            (status, Json(serde_json::json!({ "error": err_str }))).into_response()
        }
    }
}

// ────────────────────── GET /wallet/balance ─────────────────────────────

/// Query parameters for `GET /wallet/balance`.
#[derive(Debug, serde::Deserialize)]
pub struct WalletBalanceQuery {
    pub agent_id: String,
    #[serde(default = "default_chain")]
    pub chain: String,
}

/// Return native + ERC-20 token balances for the agent's smart wallet.
pub async fn wallet_balance_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Query(params): Query<WalletBalanceQuery>,
) -> impl IntoResponse {
    info!(agent_id = %params.agent_id, chain = %params.chain, "GET /wallet/balance");

    match services::handle_get_wallet_balance(
        &state.engine,
        &state.wallet_registry,
        api_ctx.api_key_id,
        &params.agent_id,
        &params.chain,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => {
            error!(error = %e, "wallet balance lookup failed");
            let err_str = e.to_string();
            let is_client_error = err_str.contains("unsupported chain")
                || err_str.contains("not configured")
                || err_str.contains("agent_id");
            let status = if is_client_error {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(serde_json::json!({ "error": err_str }))).into_response()
        }
    }
}

// ────────────────────── POST /admin/api-keys ─────────────────────────

/// Request body for API key creation.
#[derive(Debug, serde::Deserialize)]
pub struct CreateApiKeyRequest {
    /// Optional human-readable label for the API key.
    pub label: Option<String>,
    /// Optional billing mode for the API key: manual, auto, sponsored.
    pub payment_mode: Option<String>,
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

    let payment_mode = match body.payment_mode.as_deref() {
        None => PaymentMode::Manual,
        Some(value) => match PaymentMode::from_str_loose(value) {
            Some(mode) => mode,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": "invalid payment_mode; expected one of: manual, auto, sponsored"
                    })),
                )
                    .into_response();
            }
        },
    };

    match db::create_api_key(&state.db_pool, body.label.as_deref(), Some(payment_mode)).await {
        Ok((row, raw_key)) => {
            info!(api_key_id = %row.id, "new API key created");
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "api_key_id": row.id,
                    "api_key": raw_key,
                    "label": row.label,
                    "payment_mode": row.payment_mode,
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
