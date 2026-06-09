use anyhow::Result;
use ethers::prelude::Middleware;
use ethers::types::{Address, Bytes, TransactionRequest, U256};
use ethers::utils::format_units;
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

use super::adapter as gmx_v2;
use super::adapter::{
    GmxAccountQuery, GmxBalancesResponse, GmxCancelOrderRequest, GmxCancelRequest, GmxClaimRequest,
    GmxCreateDepositRequest, GmxCreateOrderRequest, GmxCreateWithdrawalRequest, GmxMarketBalance,
    GmxMarketsQuery, GmxMarketsResponse, GmxOrdersResponse, GmxPositionsResponse,
    GmxUpdateOrderRequest,
};
use crate::agent_wallet::AgentWalletRegistry;
use crate::api::services::{handle_execute, handle_simulate};
use crate::execution_engine::ExecutionEngine;
use crate::relayer::erc4337::BundlerClient;
use crate::relayer::paymaster::PaymasterSigner;
use crate::types::{Chain, ExecutionResponse, PaymentMode, PaymentProof};

async fn call_reader(engine: &ExecutionEngine, chain: &Chain, calldata: String) -> Result<Bytes> {
    let provider = engine.provider_for_chain(chain)?;
    let reader: Address = gmx_v2::reader_address().parse()?;
    let data: Bytes = hex::decode(calldata.trim_start_matches("0x"))?.into();
    let tx = TransactionRequest::new().to(reader).data(data);
    Ok(provider.call(&tx.into(), None).await?)
}

async fn call_u256(
    engine: &ExecutionEngine,
    chain: &Chain,
    to: Address,
    calldata: String,
) -> Result<U256> {
    let provider = engine.provider_for_chain(chain)?;
    let data: Bytes = hex::decode(calldata.trim_start_matches("0x"))?.into();
    let tx = TransactionRequest::new().to(to).data(data);
    let raw = provider.call(&tx.into(), None).await?;
    let decoded = ethers::abi::decode(&[ethers::abi::ParamType::Uint(256)], &raw.0)?;
    match &decoded[0] {
        ethers::abi::Token::Uint(value) => Ok(*value),
        other => anyhow::bail!("expected uint256 response, got {other:?}"),
    }
}

pub async fn handle_markets(
    engine: &ExecutionEngine,
    query: &GmxMarketsQuery,
) -> Result<GmxMarketsResponse> {
    let (start, end) = gmx_v2::validate_markets_query(query)?;
    let chain = Chain::from_str_loose(&query.chain)
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {}", query.chain))?;
    let data_store: Address = gmx_v2::data_store_address().parse()?;
    let raw = call_reader(
        engine,
        &chain,
        gmx_v2::encode_get_markets(data_store, U256::from(start), U256::from(end)),
    )
    .await?;
    let markets = gmx_v2::decode_markets(&raw.0)?;
    Ok(GmxMarketsResponse {
        chain: query.chain.clone(),
        reader_address: gmx_v2::reader_address().to_string(),
        data_store_address: gmx_v2::data_store_address().to_string(),
        start,
        end,
        markets,
    })
}

pub async fn handle_positions(
    engine: &ExecutionEngine,
    wallet_registry: &AgentWalletRegistry,
    api_key_id: Uuid,
    query: &GmxAccountQuery,
) -> Result<GmxPositionsResponse> {
    let (start, end) = gmx_v2::validate_account_query(query)?;
    let chain = Chain::from_str_loose(&query.chain)
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {}", query.chain))?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &query.agent_id)
        .await?;
    let data_store: Address = gmx_v2::data_store_address().parse()?;
    let raw = call_reader(
        engine,
        &chain,
        gmx_v2::encode_get_account_positions(
            data_store,
            agent_wallet.smart_wallet_address,
            U256::from(start),
            U256::from(end),
        ),
    )
    .await?;
    let positions = gmx_v2::decode_positions(&raw.0)?;
    Ok(GmxPositionsResponse {
        agent_id: query.agent_id.clone(),
        chain: query.chain.clone(),
        smart_wallet_address: format!("{:?}", agent_wallet.smart_wallet_address),
        start,
        end,
        positions,
    })
}

pub async fn handle_orders(
    engine: &ExecutionEngine,
    wallet_registry: &AgentWalletRegistry,
    api_key_id: Uuid,
    query: &GmxAccountQuery,
) -> Result<GmxOrdersResponse> {
    let (start, end) = gmx_v2::validate_account_query(query)?;
    let chain = Chain::from_str_loose(&query.chain)
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {}", query.chain))?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &query.agent_id)
        .await?;
    let data_store: Address = gmx_v2::data_store_address().parse()?;
    let raw = call_reader(
        engine,
        &chain,
        gmx_v2::encode_get_account_orders(
            data_store,
            agent_wallet.smart_wallet_address,
            U256::from(start),
            U256::from(end),
        ),
    )
    .await?;
    let orders = gmx_v2::decode_orders(&raw.0)?;
    Ok(GmxOrdersResponse {
        agent_id: query.agent_id.clone(),
        chain: query.chain.clone(),
        smart_wallet_address: format!("{:?}", agent_wallet.smart_wallet_address),
        start,
        end,
        orders,
    })
}

pub async fn handle_balances(
    engine: &ExecutionEngine,
    wallet_registry: &AgentWalletRegistry,
    api_key_id: Uuid,
    query: &GmxAccountQuery,
) -> Result<GmxBalancesResponse> {
    let (start, end) = gmx_v2::validate_account_query(query)?;
    let markets = handle_markets(
        engine,
        &GmxMarketsQuery {
            chain: query.chain.clone(),
            start: Some(start),
            end: Some(end),
        },
    )
    .await?
    .markets;
    let chain = Chain::from_str_loose(&query.chain)
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {}", query.chain))?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &query.agent_id)
        .await?;
    let mut balances = Vec::new();
    for market in markets {
        let token: Address = market.market_token.parse()?;
        let (balance, decimals_u256) = tokio::try_join!(
            call_u256(
                engine,
                &chain,
                token,
                gmx_v2::encode_balance_of(agent_wallet.smart_wallet_address)
            ),
            call_u256(engine, &chain, token, gmx_v2::encode_decimals())
        )?;
        let decimals = decimals_u256.as_u32().min(u8::MAX as u32) as u8;
        balances.push(GmxMarketBalance {
            market_token: market.market_token,
            balance_raw: balance.to_string(),
            balance_formatted: format_units(balance, decimals as usize)?,
            decimals,
        });
    }
    Ok(GmxBalancesResponse {
        agent_id: query.agent_id.clone(),
        chain: query.chain.clone(),
        smart_wallet_address: format!("{:?}", agent_wallet.smart_wallet_address),
        balances,
    })
}

pub async fn handle_create_order(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &GmxCreateOrderRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    let chain = Chain::from_str_loose(&req.chain)
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {}", req.chain))?;
    gmx_v2::validate_create_order_request(req)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let execution_req = gmx_v2::compile_create_order(req, agent_wallet.smart_wallet_address)?;

    handle_execute(
        engine,
        pool,
        redis_conn,
        wallet_registry,
        bundler_clients,
        paymaster_signers,
        api_key_id,
        payment_mode,
        &execution_req,
        payment_proof,
    )
    .await
    .map_err(|e| anyhow::anyhow!("GMX V2 create order on {} failed: {}", chain, e))
}

pub async fn handle_create_order_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &GmxCreateOrderRequest,
) -> Result<ExecutionResponse> {
    gmx_v2::validate_create_order_request(req)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let execution_req = gmx_v2::compile_create_order(req, agent_wallet.smart_wallet_address)?;

    handle_simulate(
        engine,
        pool,
        wallet_registry,
        bundler_clients,
        paymaster_signers,
        api_key_id,
        payment_mode,
        &execution_req,
    )
    .await
}

pub async fn handle_cancel_order(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &GmxCancelOrderRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    let chain = Chain::from_str_loose(&req.chain)
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {}", req.chain))?;
    gmx_v2::validate_cancel_order_request(req)?;
    wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let execution_req = gmx_v2::compile_cancel_order(req)?;

    handle_execute(
        engine,
        pool,
        redis_conn,
        wallet_registry,
        bundler_clients,
        paymaster_signers,
        api_key_id,
        payment_mode,
        &execution_req,
        payment_proof,
    )
    .await
    .map_err(|e| anyhow::anyhow!("GMX V2 cancel order on {} failed: {}", chain, e))
}

pub async fn handle_cancel_order_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &GmxCancelOrderRequest,
) -> Result<ExecutionResponse> {
    gmx_v2::validate_cancel_order_request(req)?;
    wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let execution_req = gmx_v2::compile_cancel_order(req)?;

    handle_simulate(
        engine,
        pool,
        wallet_registry,
        bundler_clients,
        paymaster_signers,
        api_key_id,
        payment_mode,
        &execution_req,
    )
    .await
}

pub async fn handle_update_order(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &GmxUpdateOrderRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    let chain = Chain::from_str_loose(&req.chain)
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {}", req.chain))?;
    gmx_v2::validate_update_order_request(req)?;
    wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let execution_req = gmx_v2::compile_update_order(req)?;
    handle_execute(
        engine,
        pool,
        redis_conn,
        wallet_registry,
        bundler_clients,
        paymaster_signers,
        api_key_id,
        payment_mode,
        &execution_req,
        payment_proof,
    )
    .await
    .map_err(|e| anyhow::anyhow!("GMX V2 update order on {} failed: {}", chain, e))
}

pub async fn handle_update_order_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &GmxUpdateOrderRequest,
) -> Result<ExecutionResponse> {
    gmx_v2::validate_update_order_request(req)?;
    wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let execution_req = gmx_v2::compile_update_order(req)?;
    handle_simulate(
        engine,
        pool,
        wallet_registry,
        bundler_clients,
        paymaster_signers,
        api_key_id,
        payment_mode,
        &execution_req,
    )
    .await
}

pub async fn handle_create_deposit(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &GmxCreateDepositRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    let chain = Chain::from_str_loose(&req.chain)
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {}", req.chain))?;
    gmx_v2::validate_create_deposit_request(req)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let execution_req = gmx_v2::compile_create_deposit(req, agent_wallet.smart_wallet_address)?;
    handle_execute(
        engine,
        pool,
        redis_conn,
        wallet_registry,
        bundler_clients,
        paymaster_signers,
        api_key_id,
        payment_mode,
        &execution_req,
        payment_proof,
    )
    .await
    .map_err(|e| anyhow::anyhow!("GMX V2 create deposit on {} failed: {}", chain, e))
}

pub async fn handle_create_deposit_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &GmxCreateDepositRequest,
) -> Result<ExecutionResponse> {
    gmx_v2::validate_create_deposit_request(req)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let execution_req = gmx_v2::compile_create_deposit(req, agent_wallet.smart_wallet_address)?;
    handle_simulate(
        engine,
        pool,
        wallet_registry,
        bundler_clients,
        paymaster_signers,
        api_key_id,
        payment_mode,
        &execution_req,
    )
    .await
}

pub async fn handle_create_withdrawal(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &GmxCreateWithdrawalRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    let chain = Chain::from_str_loose(&req.chain)
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {}", req.chain))?;
    gmx_v2::validate_create_withdrawal_request(req)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let execution_req = gmx_v2::compile_create_withdrawal(req, agent_wallet.smart_wallet_address)?;
    handle_execute(
        engine,
        pool,
        redis_conn,
        wallet_registry,
        bundler_clients,
        paymaster_signers,
        api_key_id,
        payment_mode,
        &execution_req,
        payment_proof,
    )
    .await
    .map_err(|e| anyhow::anyhow!("GMX V2 create withdrawal on {} failed: {}", chain, e))
}

pub async fn handle_create_withdrawal_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &GmxCreateWithdrawalRequest,
) -> Result<ExecutionResponse> {
    gmx_v2::validate_create_withdrawal_request(req)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let execution_req = gmx_v2::compile_create_withdrawal(req, agent_wallet.smart_wallet_address)?;
    handle_simulate(
        engine,
        pool,
        wallet_registry,
        bundler_clients,
        paymaster_signers,
        api_key_id,
        payment_mode,
        &execution_req,
    )
    .await
}

pub async fn handle_cancel(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &GmxCancelRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    let chain = Chain::from_str_loose(&req.chain)
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {}", req.chain))?;
    gmx_v2::validate_cancel_request(req)?;
    wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let execution_req = gmx_v2::compile_cancel(req)?;
    handle_execute(
        engine,
        pool,
        redis_conn,
        wallet_registry,
        bundler_clients,
        paymaster_signers,
        api_key_id,
        payment_mode,
        &execution_req,
        payment_proof,
    )
    .await
    .map_err(|e| anyhow::anyhow!("GMX V2 cancel request on {} failed: {}", chain, e))
}

pub async fn handle_cancel_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &GmxCancelRequest,
) -> Result<ExecutionResponse> {
    gmx_v2::validate_cancel_request(req)?;
    wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let execution_req = gmx_v2::compile_cancel(req)?;
    handle_simulate(
        engine,
        pool,
        wallet_registry,
        bundler_clients,
        paymaster_signers,
        api_key_id,
        payment_mode,
        &execution_req,
    )
    .await
}

pub async fn handle_claim(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &GmxClaimRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    let chain = Chain::from_str_loose(&req.chain)
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {}", req.chain))?;
    gmx_v2::validate_claim_request(req)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let execution_req = gmx_v2::compile_claim(req, agent_wallet.smart_wallet_address)?;
    handle_execute(
        engine,
        pool,
        redis_conn,
        wallet_registry,
        bundler_clients,
        paymaster_signers,
        api_key_id,
        payment_mode,
        &execution_req,
        payment_proof,
    )
    .await
    .map_err(|e| anyhow::anyhow!("GMX V2 claim on {} failed: {}", chain, e))
}

pub async fn handle_claim_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &GmxClaimRequest,
) -> Result<ExecutionResponse> {
    gmx_v2::validate_claim_request(req)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let execution_req = gmx_v2::compile_claim(req, agent_wallet.smart_wallet_address)?;
    handle_simulate(
        engine,
        pool,
        wallet_registry,
        bundler_clients,
        paymaster_signers,
        api_key_id,
        payment_mode,
        &execution_req,
    )
    .await
}
