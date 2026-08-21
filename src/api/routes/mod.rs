//! Axum route handlers for the Execution API.

use axum::{
    extract::{ConnectInfo, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use redis::aio::ConnectionManager;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::{error, info};
use uuid::Uuid;

use std::{collections::HashMap, net::SocketAddr};

use crate::agent_wallet::AgentWalletRegistry;
use crate::api::services;
use crate::config::AppConfig;
use crate::db;
use crate::execution_engine::ExecutionEngine;
use crate::protocols::aave_v3::{
    service as aave_v3_service, AaveBalancesQuery, AaveBorrowRequest, AavePositionQuery,
    AaveRepayRequest, AaveSupplyRequest, AaveWithdrawRequest,
};
use crate::protocols::balancer_v3::{
    service as balancer_v3_service, BalancerAddLiquidityRequest, BalancerBalancesQuery,
    BalancerPoolQuery, BalancerPoolsQuery, BalancerRemoveLiquidityRequest, BalancerSwapRequest,
};
use crate::protocols::compound_v3::{
    service as compound_v3_service, CompoundBalancesQuery, CompoundBorrowCapacityQuery,
    CompoundBorrowRequest, CompoundMarketsQuery, CompoundPositionQuery, CompoundRepayRequest,
    CompoundSupplyRequest, CompoundWithdrawRequest,
};
use crate::protocols::gmx_v2::{
    service as gmx_v2_service, GmxAccountQuery, GmxCancelOrderRequest, GmxCancelRequest,
    GmxClaimRequest, GmxCreateDepositRequest, GmxCreateOrderRequest, GmxCreateWithdrawalRequest,
    GmxMarketsQuery, GmxUpdateOrderRequest,
};
use crate::protocols::morpho::{
    service as morpho_service, MorphoActionRequest, MorphoMarketQuery, MorphoMarketsQuery,
    MorphoPositionQuery,
};
use crate::protocols::uniswap_v4::{
    service as uniswap_v4_service, UniswapBalancesQuery, UniswapPoolQuery, UniswapPoolsQuery,
    UniswapSwapRequest,
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
    format!("{head}...{tail}")
}

fn chain_display(chain: &str) -> String {
    match chain.to_lowercase().as_str() {
        "ethereum" | "eth" => "Ethereum Sepolia".to_string(),
        "sepolia" => "Ethereum Sepolia".to_string(),
        "base" => "Base Sepolia".to_string(),
        "arbitrum" | "arb" => "Arbitrum Sepolia".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => "Unknown".to_string(),
            }
        }
    }
}

#[cfg(test)]
mod feed_tests {
    use super::chain_display;

    #[test]
    fn chain_display_names_supported_testnets() {
        assert_eq!(chain_display("ethereum"), "Ethereum Sepolia");
        assert_eq!(chain_display("base"), "Base Sepolia");
        assert_eq!(chain_display("arbitrum"), "Arbitrum Sepolia");
    }
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
    Json(req): Json<ExecutionRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, "POST /execute");

    let mut redis = state.redis_conn.clone();

    match services::handle_execute(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
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
    Json(req): Json<AaveSupplyRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, "POST /protocols/aave-v3/supply");

    let mut redis = state.redis_conn.clone();

    match aave_v3_service::handle_supply(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(resp),
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
    Json(req): Json<AaveWithdrawRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, "POST /protocols/aave-v3/withdraw");

    let mut redis = state.redis_conn.clone();

    match aave_v3_service::handle_withdraw(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(resp),
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
    Json(req): Json<AaveRepayRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, "POST /protocols/aave-v3/repay");

    let mut redis = state.redis_conn.clone();

    match aave_v3_service::handle_repay(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(resp),
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
    Json(req): Json<AaveBorrowRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, "POST /protocols/aave-v3/borrow");

    let mut redis = state.redis_conn.clone();

    match aave_v3_service::handle_borrow(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(resp),
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
            Json(req): Json<$req_ty>,
        ) -> impl IntoResponse {
            info!(agent_id = %req.agent_id, chain = %req.chain, asset = %req.asset, $log_name);
            let mut redis = state.redis_conn.clone();
            match $service_fn(
                &state.engine,
                &state.db_pool,
                &mut redis,
                &state.wallet_registry,
                &state.bundler_clients,
                &state.paymaster_signers,
                api_ctx.api_key_id,
                &req,
            )
            .await
            {
                Ok(resp) => execution_response_to_http(resp),
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

pub async fn compound_borrow_capacity_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<CompoundBorrowCapacityQuery>,
) -> impl IntoResponse {
    info!(agent_id = %query.agent_id, chain = %query.chain, "GET /protocols/compound-v3/borrow-capacity");
    match compound_v3_service::handle_borrow_capacity(
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

pub async fn compound_markets_handler(
    State(state): State<AppState>,
    Extension(_api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<CompoundMarketsQuery>,
) -> impl IntoResponse {
    info!(
        chain = %query.chain,
        base_asset = ?query.base_asset,
        "GET /protocols/compound-v3/markets"
    );
    match compound_v3_service::handle_markets(&state.engine, &query).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(error) => protocol_error_to_http(error),
    }
}

macro_rules! morpho_execute_handler {
    ($fn_name:ident, $service_fn:path, $log_name:literal) => {
        pub async fn $fn_name(
            State(state): State<AppState>,
            Extension(api_ctx): Extension<ApiKeyContext>,
            Json(req): Json<MorphoActionRequest>,
        ) -> impl IntoResponse {
            info!(
                agent_id = %req.agent_id,
                chain = %req.chain,
                market_id = %req.market_id,
                $log_name
            );
            let mut redis = state.redis_conn.clone();
            match $service_fn(
                &state.engine,
                &state.db_pool,
                &mut redis,
                &state.wallet_registry,
                &state.bundler_clients,
                &state.paymaster_signers,
                api_ctx.api_key_id,
                &req,
            )
            .await
            {
                Ok(resp) => execution_response_to_http(resp),
                Err(error) => protocol_error_to_http(error),
            }
        }
    };
}

macro_rules! morpho_simulate_handler {
    ($fn_name:ident, $service_fn:path, $log_name:literal) => {
        pub async fn $fn_name(
            State(state): State<AppState>,
            Extension(api_ctx): Extension<ApiKeyContext>,
            Json(req): Json<MorphoActionRequest>,
        ) -> impl IntoResponse {
            info!(
                agent_id = %req.agent_id,
                chain = %req.chain,
                market_id = %req.market_id,
                $log_name
            );
            match $service_fn(
                &state.engine,
                &state.db_pool,
                &state.wallet_registry,
                &state.bundler_clients,
                &state.paymaster_signers,
                api_ctx.api_key_id,
                &req,
            )
            .await
            {
                Ok(resp) => {
                    (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response()
                }
                Err(error) => protocol_error_to_http(error),
            }
        }
    };
}

morpho_execute_handler!(
    morpho_supply_handler,
    morpho_service::handle_supply,
    "POST /protocols/morpho/supply"
);
morpho_simulate_handler!(
    morpho_supply_simulate_handler,
    morpho_service::handle_supply_simulate,
    "POST /protocols/morpho/supply/simulate"
);
morpho_execute_handler!(
    morpho_withdraw_handler,
    morpho_service::handle_withdraw,
    "POST /protocols/morpho/withdraw"
);
morpho_simulate_handler!(
    morpho_withdraw_simulate_handler,
    morpho_service::handle_withdraw_simulate,
    "POST /protocols/morpho/withdraw/simulate"
);
morpho_execute_handler!(
    morpho_supply_collateral_handler,
    morpho_service::handle_supply_collateral,
    "POST /protocols/morpho/supply-collateral"
);
morpho_simulate_handler!(
    morpho_supply_collateral_simulate_handler,
    morpho_service::handle_supply_collateral_simulate,
    "POST /protocols/morpho/supply-collateral/simulate"
);
morpho_execute_handler!(
    morpho_withdraw_collateral_handler,
    morpho_service::handle_withdraw_collateral,
    "POST /protocols/morpho/withdraw-collateral"
);
morpho_simulate_handler!(
    morpho_withdraw_collateral_simulate_handler,
    morpho_service::handle_withdraw_collateral_simulate,
    "POST /protocols/morpho/withdraw-collateral/simulate"
);
morpho_execute_handler!(
    morpho_borrow_handler,
    morpho_service::handle_borrow,
    "POST /protocols/morpho/borrow"
);
morpho_simulate_handler!(
    morpho_borrow_simulate_handler,
    morpho_service::handle_borrow_simulate,
    "POST /protocols/morpho/borrow/simulate"
);
morpho_execute_handler!(
    morpho_repay_handler,
    morpho_service::handle_repay,
    "POST /protocols/morpho/repay"
);
morpho_simulate_handler!(
    morpho_repay_simulate_handler,
    morpho_service::handle_repay_simulate,
    "POST /protocols/morpho/repay/simulate"
);

pub async fn morpho_market_handler(
    State(state): State<AppState>,
    Extension(_api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<MorphoMarketQuery>,
) -> impl IntoResponse {
    info!(chain = %query.chain, market_id = %query.market_id, "GET /protocols/morpho/market");
    match morpho_service::handle_market(&state.engine, &query).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(error) => protocol_error_to_http(error),
    }
}

pub async fn morpho_markets_handler(
    State(state): State<AppState>,
    Extension(_api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<MorphoMarketsQuery>,
) -> impl IntoResponse {
    info!(
        chain = %query.chain,
        loan_token = ?query.loan_token,
        collateral_token = ?query.collateral_token,
        "GET /protocols/morpho/markets"
    );
    match morpho_service::handle_markets(&state.engine, &query).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(error) => protocol_error_to_http(error),
    }
}

pub async fn morpho_position_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<MorphoPositionQuery>,
) -> impl IntoResponse {
    info!(
        agent_id = %query.agent_id,
        chain = %query.chain,
        market_id = %query.market_id,
        "GET /protocols/morpho/position"
    );
    match morpho_service::handle_position(
        &state.engine,
        &state.wallet_registry,
        api_ctx.api_key_id,
        &query,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(error) => protocol_error_to_http(error),
    }
}

pub async fn balancer_swap_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<BalancerSwapRequest>,
) -> impl IntoResponse {
    info!(
        agent_id = %req.agent_id,
        chain = %req.chain,
        pool = %req.pool,
        "POST /protocols/balancer-v3/swap"
    );
    let mut redis = state.redis_conn.clone();

    match balancer_v3_service::handle_swap(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(resp),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn balancer_swap_simulate_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<BalancerSwapRequest>,
) -> impl IntoResponse {
    info!(
        agent_id = %req.agent_id,
        chain = %req.chain,
        pool = %req.pool,
        "POST /protocols/balancer-v3/swap/simulate"
    );
    match balancer_v3_service::handle_swap_simulate(
        &state.engine,
        &state.db_pool,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn balancer_quote_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<BalancerSwapRequest>,
) -> impl IntoResponse {
    info!(
        agent_id = %req.agent_id,
        chain = %req.chain,
        pool = %req.pool,
        "POST /protocols/balancer-v3/swap/quote"
    );
    match balancer_v3_service::handle_quote(
        &state.engine,
        &state.wallet_registry,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn balancer_add_liquidity_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<BalancerAddLiquidityRequest>,
) -> impl IntoResponse {
    info!(
        agent_id = %req.agent_id,
        chain = %req.chain,
        pool = %req.pool,
        "POST /protocols/balancer-v3/liquidity/add"
    );
    let mut redis = state.redis_conn.clone();
    match balancer_v3_service::handle_add_liquidity(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(resp),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn balancer_add_liquidity_simulate_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<BalancerAddLiquidityRequest>,
) -> impl IntoResponse {
    info!(
        agent_id = %req.agent_id,
        chain = %req.chain,
        pool = %req.pool,
        "POST /protocols/balancer-v3/liquidity/add/simulate"
    );
    match balancer_v3_service::handle_add_liquidity_simulate(
        &state.engine,
        &state.db_pool,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn balancer_add_liquidity_quote_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<BalancerAddLiquidityRequest>,
) -> impl IntoResponse {
    match balancer_v3_service::handle_add_liquidity_quote(
        &state.engine,
        &state.wallet_registry,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn balancer_remove_liquidity_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<BalancerRemoveLiquidityRequest>,
) -> impl IntoResponse {
    info!(
        agent_id = %req.agent_id,
        chain = %req.chain,
        pool = %req.pool,
        "POST /protocols/balancer-v3/liquidity/remove"
    );
    let mut redis = state.redis_conn.clone();
    match balancer_v3_service::handle_remove_liquidity(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(resp),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn balancer_remove_liquidity_simulate_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<BalancerRemoveLiquidityRequest>,
) -> impl IntoResponse {
    match balancer_v3_service::handle_remove_liquidity_simulate(
        &state.engine,
        &state.db_pool,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn balancer_remove_liquidity_quote_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<BalancerRemoveLiquidityRequest>,
) -> impl IntoResponse {
    match balancer_v3_service::handle_remove_liquidity_quote(
        &state.engine,
        &state.wallet_registry,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn balancer_pool_handler(
    State(state): State<AppState>,
    Query(query): Query<BalancerPoolQuery>,
) -> impl IntoResponse {
    info!(
        chain = %query.chain,
        pool = %query.pool,
        "GET /protocols/balancer-v3/pool"
    );
    match balancer_v3_service::handle_pool(&state.engine, &query).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn balancer_pools_handler(
    State(state): State<AppState>,
    Extension(_api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<BalancerPoolsQuery>,
) -> impl IntoResponse {
    info!(
        chain = %query.chain,
        token_in = %query.token_in,
        token_out = %query.token_out,
        "GET /protocols/balancer-v3/pools"
    );
    match balancer_v3_service::handle_pools(&state.engine, &query).await {
        Ok(response) => (
            StatusCode::OK,
            Json(serde_json::to_value(response).unwrap()),
        )
            .into_response(),
        Err(error) => protocol_error_to_http(error),
    }
}

pub async fn balancer_balances_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<BalancerBalancesQuery>,
) -> impl IntoResponse {
    info!(
        agent_id = %query.agent_id,
        chain = %query.chain,
        pool = %query.pool,
        "GET /protocols/balancer-v3/balances"
    );
    match balancer_v3_service::handle_balances(
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

pub async fn uniswap_v4_swap_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<UniswapSwapRequest>,
) -> impl IntoResponse {
    info!(
        agent_id = %req.agent_id,
        chain = %req.chain,
        token_in = %req.token_in,
        token_out = %req.token_out,
        "POST /protocols/uniswap-v4/swap"
    );
    let mut redis = state.redis_conn.clone();
    match uniswap_v4_service::handle_swap(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(resp),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn uniswap_v4_swap_simulate_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<UniswapSwapRequest>,
) -> impl IntoResponse {
    info!(
        agent_id = %req.agent_id,
        chain = %req.chain,
        token_in = %req.token_in,
        token_out = %req.token_out,
        "POST /protocols/uniswap-v4/swap/simulate"
    );
    match uniswap_v4_service::handle_swap_simulate(
        &state.engine,
        &state.db_pool,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn uniswap_v4_quote_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Json(req): Json<UniswapSwapRequest>,
) -> impl IntoResponse {
    info!(
        agent_id = %req.agent_id,
        chain = %req.chain,
        token_in = %req.token_in,
        token_out = %req.token_out,
        "POST /protocols/uniswap-v4/swap/quote"
    );
    match uniswap_v4_service::handle_quote(
        &state.engine,
        &state.wallet_registry,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn uniswap_v4_pool_handler(
    State(state): State<AppState>,
    Query(query): Query<UniswapPoolQuery>,
) -> impl IntoResponse {
    info!(
        chain = %query.chain,
        token_a = %query.token_a,
        token_b = %query.token_b,
        "GET /protocols/uniswap-v4/pool"
    );
    match uniswap_v4_service::handle_pool(&state.engine, &query).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn uniswap_v4_pools_handler(
    State(state): State<AppState>,
    Query(query): Query<UniswapPoolsQuery>,
) -> impl IntoResponse {
    info!(
        chain = %query.chain,
        token_a = %query.token_a,
        token_b = %query.token_b,
        include_hooked_pools = query.include_hooked_pools,
        "GET /protocols/uniswap-v4/pools"
    );
    match uniswap_v4_service::handle_pools(&state.engine, &query).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => protocol_error_to_http(e),
    }
}

pub async fn uniswap_v4_balances_handler(
    State(state): State<AppState>,
    Extension(api_ctx): Extension<ApiKeyContext>,
    Query(query): Query<UniswapBalancesQuery>,
) -> impl IntoResponse {
    info!(
        agent_id = %query.agent_id,
        chain = %query.chain,
        token_a = %query.token_a,
        token_b = %query.token_b,
        "GET /protocols/uniswap-v4/balances"
    );
    match uniswap_v4_service::handle_balances(
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
    Json(req): Json<GmxCreateOrderRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, order_type = %req.order_type, "POST /protocols/gmx-v2/orders");

    let mut redis = state.redis_conn.clone();

    match gmx_v2_service::handle_create_order(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(resp),
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
    Json(req): Json<GmxCancelOrderRequest>,
) -> impl IntoResponse {
    info!(agent_id = %req.agent_id, chain = %req.chain, order_key = %req.order_key, "POST /protocols/gmx-v2/orders/cancel");

    let mut redis = state.redis_conn.clone();

    match gmx_v2_service::handle_cancel_order(
        &state.engine,
        &state.db_pool,
        &mut redis,
        &state.wallet_registry,
        &state.bundler_clients,
        &state.paymaster_signers,
        api_ctx.api_key_id,
        &req,
    )
    .await
    {
        Ok(resp) => execution_response_to_http(resp),
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
            Json(req): Json<$req_ty>,
        ) -> impl IntoResponse {
            info!(agent_id = %req.agent_id, chain = %req.chain, $log_name);
            let mut redis = state.redis_conn.clone();
            match $service_fn(
                &state.engine,
                &state.db_pool,
                &mut redis,
                &state.wallet_registry,
                &state.bundler_clients,
                &state.paymaster_signers,
                api_ctx.api_key_id,
                &req,
            )
            .await
            {
                Ok(resp) => execution_response_to_http(resp),
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

fn execution_response_to_http(resp: ExecutionResponse) -> axum::response::Response {
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
        || err_str.contains("not registered")
        || err_str.contains("not initialized")
        || err_str.contains("pool is paused")
        || err_str.contains("recovery mode")
        || err_str.contains("token_in")
        || err_str.contains("token_out")
        || err_str.contains("slippage")
        || err_str.contains("deadline")
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
    Extension(api_ctx): Extension<ApiKeyContext>,
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

    match db::get_execution_request_for_api_key(&state.db_pool, uuid, api_ctx.api_key_id).await {
        Ok(Some(row)) => {
            let resp = StatusResponse {
                request_id: row.id,
                status: serde_json::from_value(serde_json::Value::String(row.status.clone()))
                    .unwrap_or(ExecutionStatus::Pending),
                chain: row.chain,
                tx_hash: row.tx_hash,
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
                        "pending" => ("reg", "REG"),
                        "broadcasting" => ("relay", "RELAY"),
                        _ => ("exec", "EXEC"),
                    };

                    let status_ok = normalized_status == "confirmed";

                    let confirm = match normalized_status.as_str() {
                        "confirmed" => "Confirmed",
                        "failed" | "reverted" => "Failed",
                        _ => "Pending...",
                    };

                    let hash = row
                        .tx_hash
                        .as_deref()
                        .map(short_hash)
                        .unwrap_or_else(|| short_hash(&row.id.to_string()));

                    let agent = format!("agent-{}", &row.id.to_string()[..8]);

                    let display_chain = chain_display(&row.chain);

                    let detail = match normalized_status.as_str() {
                        "broadcasting" => {
                            format!("bundler relay · {display_chain}")
                        }
                        "pending" | "queued" => {
                            format!("request queued · {display_chain}")
                        }
                        "failed" | "reverted" => {
                            format!("execution failed · {display_chain}")
                        }
                        _ => {
                            format!("contract execution · {display_chain}")
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
/// No simulation or transaction submission is performed.
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

// ────────────────────── POST /api-keys ───────────────────────────────

/// Request body for API key creation.
#[derive(Debug, serde::Deserialize)]
pub struct CreateApiKeyRequest {
    /// Optional human-readable label for the API key.
    pub label: Option<String>,
}

fn api_key_client_identifier(
    headers: &HeaderMap,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    configured_header: Option<&str>,
) -> Result<String, &'static str> {
    if let Some(header_name) = configured_header {
        let raw = headers
            .get(header_name)
            .and_then(|value| value.to_str().ok())
            .ok_or("configured client IP header is missing or invalid")?;
        let first = raw
            .split(',')
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or("configured client IP header is empty")?;
        let ip = first
            .parse::<std::net::IpAddr>()
            .map_err(|_| "configured client IP header does not contain a valid IP address")?;
        return Ok(ip.to_string());
    }

    connect_info
        .map(|ConnectInfo(address)| address.ip().to_string())
        .ok_or("client network address is unavailable")
}

async fn enforce_api_key_issuance_limit(
    redis: &mut ConnectionManager,
    client_identifier: &str,
    limit: u64,
    window_secs: u64,
) -> Result<(), ApiKeyIssuanceLimitError> {
    if limit == 0 {
        return Ok(());
    }

    let fingerprint = hex::encode(Sha256::digest(client_identifier.as_bytes()));
    let redis_key = format!("rate_limit:public_api_key:{fingerprint}");
    let script = redis::Script::new(
        r#"
        local count = redis.call('INCR', KEYS[1])
        if count == 1 then
            redis.call('EXPIRE', KEYS[1], ARGV[1])
        end
        return {count, redis.call('TTL', KEYS[1])}
        "#,
    );

    let result: redis::RedisResult<(u64, i64)> = script
        .key(redis_key)
        .arg(window_secs.max(1))
        .invoke_async(redis)
        .await;

    match result {
        Ok((count, _ttl)) if count <= limit => Ok(()),
        Ok((_count, ttl)) => Err(ApiKeyIssuanceLimitError::Exceeded {
            limit,
            retry_after: ttl.max(1) as u64,
        }),
        Err(error) => Err(ApiKeyIssuanceLimitError::Unavailable(error)),
    }
}

enum ApiKeyIssuanceLimitError {
    Exceeded { limit: u64, retry_after: u64 },
    Unavailable(redis::RedisError),
}

fn normalize_api_key_label(label: Option<String>) -> Result<Option<String>, String> {
    let Some(label) = label else {
        return Ok(None);
    };
    let label = label.trim();
    if label.is_empty() {
        return Ok(None);
    }
    if label.chars().count() > 100 {
        return Err("label must be at most 100 characters".into());
    }
    if label.chars().any(char::is_control) {
        return Err("label must not contain control characters".into());
    }
    Ok(Some(label.to_string()))
}

/// Create a self-service API key. The raw key is returned exactly once and is
/// never stored in plaintext. Issuance is rate-limited independently from the
/// authenticated API request limit.
pub async fn create_api_key_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    Json(body): Json<CreateApiKeyRequest>,
) -> impl IntoResponse {
    info!("POST /api-keys");

    let label = match normalize_api_key_label(body.label) {
        Ok(label) => label,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": message })),
            )
                .into_response();
        }
    };

    let client_identifier = match api_key_client_identifier(
        &headers,
        connect_info,
        state.config.public_api_key_client_ip_header.as_deref(),
    ) {
        Ok(identifier) => identifier,
        Err(message) => {
            error!(message, "cannot identify API key requester");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "API key issuance is temporarily unavailable"
                })),
            )
                .into_response();
        }
    };

    let mut redis = state.redis_conn.clone();
    if let Err(rate_limit_error) = enforce_api_key_issuance_limit(
        &mut redis,
        &client_identifier,
        state.config.public_api_key_limit,
        state.config.public_api_key_window_secs,
    )
    .await
    {
        return match rate_limit_error {
            ApiKeyIssuanceLimitError::Exceeded { limit, retry_after } => (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", retry_after.to_string())],
                Json(serde_json::json!({
                    "error": "api_key_issuance_rate_limit_exceeded",
                    "message": format!("A maximum of {limit} API keys may be generated in this window."),
                    "retry_after_secs": retry_after
                })),
            )
                .into_response(),
            ApiKeyIssuanceLimitError::Unavailable(error) => {
                error!(error = %error, "API key issuance rate limiter unavailable");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "error": "API key issuance is temporarily unavailable"
                    })),
                )
                    .into_response()
            }
        };
    }

    match db::create_api_key(&state.db_pool, label.as_deref()).await {
        Ok((row, raw_key)) => {
            info!(api_key_id = %row.id, "new API key created");
            (
                StatusCode::CREATED,
                [("cache-control", "no-store")],
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
