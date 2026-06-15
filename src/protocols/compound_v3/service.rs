use anyhow::Result;
use ethers::prelude::Middleware;
use ethers::types::{Address, Bytes, TransactionRequest, U256};
use ethers::utils::format_units;
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

use super::adapter as compound_v3;
use super::adapter::{
    CompoundAssetBalance, CompoundBalancesQuery, CompoundBalancesResponse, CompoundBorrowRequest,
    CompoundCollateralBalance, CompoundPositionQuery, CompoundPositionResponse,
    CompoundRepayRequest, CompoundSupplyRequest, CompoundWithdrawRequest,
};
use crate::agent_wallet::AgentWalletRegistry;
use crate::api::services::{handle_execute, handle_simulate, resolve_chain_smart_wallet_address};
use crate::execution_engine::ExecutionEngine;
use crate::relayer::erc4337::BundlerClient;
use crate::relayer::paymaster::PaymasterSigner;
use crate::types::{Chain, ExecutionResponse, PaymentMode, PaymentProof};

pub async fn handle_supply(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &CompoundSupplyRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    compound_v3::validate_supply_request(req)?;
    let chain = parse_chain(&req.chain)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let _smart_wallet_address =
        resolve_chain_smart_wallet_address(engine, &chain, &agent_wallet).await?;
    let resolved_req = resolve_supply_amount(engine, &chain, req, _smart_wallet_address).await?;
    let execution_req = compound_v3::compile_supply(&resolved_req)?;

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
    .map_err(|e| anyhow::anyhow!("Compound III supply on {} failed: {}", chain, e))
}

pub async fn handle_supply_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &CompoundSupplyRequest,
) -> Result<ExecutionResponse> {
    compound_v3::validate_supply_request(req)?;
    let chain = parse_chain(&req.chain)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let smart_wallet_address =
        resolve_chain_smart_wallet_address(engine, &chain, &agent_wallet).await?;
    let resolved_req = resolve_supply_amount(engine, &chain, req, smart_wallet_address).await?;
    let execution_req = compound_v3::compile_supply(&resolved_req)?;

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

pub async fn handle_withdraw(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &CompoundWithdrawRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    compound_v3::validate_withdraw_request(req)?;
    let chain = parse_chain(&req.chain)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let smart_wallet_address =
        resolve_chain_smart_wallet_address(engine, &chain, &agent_wallet).await?;
    let resolved_req = resolve_withdraw_amount(engine, &chain, req, smart_wallet_address).await?;
    let execution_req = compound_v3::compile_withdraw(&resolved_req)?;

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
    .map_err(|e| anyhow::anyhow!("Compound III withdraw on {} failed: {}", chain, e))
}

pub async fn handle_withdraw_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &CompoundWithdrawRequest,
) -> Result<ExecutionResponse> {
    compound_v3::validate_withdraw_request(req)?;
    let chain = parse_chain(&req.chain)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let smart_wallet_address =
        resolve_chain_smart_wallet_address(engine, &chain, &agent_wallet).await?;
    let resolved_req = resolve_withdraw_amount(engine, &chain, req, smart_wallet_address).await?;
    let execution_req = compound_v3::compile_withdraw(&resolved_req)?;

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

pub async fn handle_repay(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &CompoundRepayRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    compound_v3::validate_repay_request(req)?;
    let chain = parse_chain(&req.chain)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let smart_wallet_address =
        resolve_chain_smart_wallet_address(engine, &chain, &agent_wallet).await?;
    let resolved_req = resolve_repay_amount(engine, &chain, req, smart_wallet_address).await?;
    let execution_req = compound_v3::compile_repay(&resolved_req)?;

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
    .map_err(|e| anyhow::anyhow!("Compound III repay on {} failed: {}", chain, e))
}

pub async fn handle_repay_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &CompoundRepayRequest,
) -> Result<ExecutionResponse> {
    compound_v3::validate_repay_request(req)?;
    let chain = parse_chain(&req.chain)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let smart_wallet_address =
        resolve_chain_smart_wallet_address(engine, &chain, &agent_wallet).await?;
    let resolved_req = resolve_repay_amount(engine, &chain, req, smart_wallet_address).await?;
    let execution_req = compound_v3::compile_repay(&resolved_req)?;

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

pub async fn handle_borrow(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &CompoundBorrowRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    compound_v3::validate_borrow_request(req)?;
    let chain = parse_chain(&req.chain)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let smart_wallet_address =
        resolve_chain_smart_wallet_address(engine, &chain, &agent_wallet).await?;
    let resolved_req = resolve_borrow_amount(engine, &chain, req, smart_wallet_address).await?;
    let execution_req = compound_v3::compile_borrow(&resolved_req)?;

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
    .map_err(|e| anyhow::anyhow!("Compound III borrow on {} failed: {}", chain, e))
}

pub async fn handle_borrow_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &CompoundBorrowRequest,
) -> Result<ExecutionResponse> {
    compound_v3::validate_borrow_request(req)?;
    let chain = parse_chain(&req.chain)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let smart_wallet_address =
        resolve_chain_smart_wallet_address(engine, &chain, &agent_wallet).await?;
    let resolved_req = resolve_borrow_amount(engine, &chain, req, smart_wallet_address).await?;
    let execution_req = compound_v3::compile_borrow(&resolved_req)?;

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

pub async fn handle_position(
    engine: &ExecutionEngine,
    wallet_registry: &AgentWalletRegistry,
    api_key_id: Uuid,
    query: &CompoundPositionQuery,
) -> Result<CompoundPositionResponse> {
    compound_v3::validate_position_query(query)?;
    let chain = parse_chain(&query.chain)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &query.agent_id)
        .await?;
    let smart_wallet_address =
        resolve_chain_smart_wallet_address(engine, &chain, &agent_wallet).await?;
    let market = compound_v3::market_from_query(query.market.as_deref())?;
    let comet = market.comet();
    let base = call_address(engine, &chain, comet, compound_v3::encode_base_token()).await?;
    let (base_symbol, base_decimals, base_supply_balance, base_borrow_balance, collateral_assets) =
        tokio::try_join!(
            token_symbol(engine, &chain, base),
            token_decimals(engine, &chain, base),
            call_u256(
                engine,
                &chain,
                comet,
                compound_v3::encode_balance_of(smart_wallet_address)
            ),
            call_u256(
                engine,
                &chain,
                comet,
                compound_v3::encode_borrow_balance_of(smart_wallet_address)
            ),
            collateral_balances(engine, &chain, comet, smart_wallet_address),
        )?;

    Ok(CompoundPositionResponse {
        agent_id: query.agent_id.clone(),
        chain: query.chain.clone(),
        smart_wallet_address: format!("{smart_wallet_address:?}"),
        comet_address: format!("{comet:?}"),
        base_token_address: format!("{base:?}"),
        base_token_symbol: base_symbol,
        base_token_decimals: base_decimals,
        base_supply_balance_raw: base_supply_balance.to_string(),
        base_supply_balance_formatted: format_token_units(base_supply_balance, base_decimals)?,
        base_borrow_balance_raw: base_borrow_balance.to_string(),
        base_borrow_balance_formatted: format_token_units(base_borrow_balance, base_decimals)?,
        collateral_assets,
    })
}

pub async fn handle_balances(
    engine: &ExecutionEngine,
    wallet_registry: &AgentWalletRegistry,
    api_key_id: Uuid,
    query: &CompoundBalancesQuery,
) -> Result<CompoundBalancesResponse> {
    compound_v3::validate_balances_query(query)?;
    let chain = parse_chain(&query.chain)?;
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &query.agent_id)
        .await?;
    let smart_wallet_address =
        resolve_chain_smart_wallet_address(engine, &chain, &agent_wallet).await?;
    let market = compound_v3::market_from_query(query.market.as_deref())?;
    let comet = market.comet();
    let base = call_address(engine, &chain, comet, compound_v3::encode_base_token()).await?;
    let base_symbol = token_symbol(engine, &chain, base).await?;
    let base_decimals = token_decimals(engine, &chain, base).await?;
    let (base_wallet_balance, base_supply_balance) = tokio::try_join!(
        call_u256(
            engine,
            &chain,
            base,
            compound_v3::encode_erc20_balance_of(smart_wallet_address)
        ),
        call_u256(
            engine,
            &chain,
            comet,
            compound_v3::encode_balance_of(smart_wallet_address)
        ),
    )?;

    let mut assets = vec![CompoundAssetBalance {
        symbol: base_symbol,
        token_address: format!("{base:?}"),
        decimals: base_decimals,
        wallet_balance_raw: base_wallet_balance.to_string(),
        wallet_balance_formatted: format_token_units(base_wallet_balance, base_decimals)?,
        compound_balance_raw: base_supply_balance.to_string(),
        compound_balance_formatted: format_token_units(base_supply_balance, base_decimals)?,
        is_base_asset: true,
    }];

    for collateral in collateral_balances(engine, &chain, comet, smart_wallet_address).await? {
        let token: Address = collateral.token_address.parse()?;
        let wallet_balance = call_u256(
            engine,
            &chain,
            token,
            compound_v3::encode_erc20_balance_of(smart_wallet_address),
        )
        .await?;
        assets.push(CompoundAssetBalance {
            symbol: collateral.symbol,
            token_address: collateral.token_address,
            decimals: collateral.decimals,
            wallet_balance_raw: wallet_balance.to_string(),
            wallet_balance_formatted: format_token_units(wallet_balance, collateral.decimals)?,
            compound_balance_raw: collateral.collateral_balance_raw,
            compound_balance_formatted: collateral.collateral_balance_formatted,
            is_base_asset: false,
        });
    }

    Ok(CompoundBalancesResponse {
        agent_id: query.agent_id.clone(),
        chain: query.chain.clone(),
        smart_wallet_address: format!("{smart_wallet_address:?}"),
        comet_address: format!("{comet:?}"),
        assets,
    })
}

async fn resolve_supply_amount(
    engine: &ExecutionEngine,
    chain: &Chain,
    req: &CompoundSupplyRequest,
    smart_wallet_address: Address,
) -> Result<CompoundSupplyRequest> {
    if !compound_v3::is_amount_max(req.amount.as_deref(), req.amount_raw.as_deref()) {
        return Ok(req.clone());
    }
    let market = compound_v3::market_from_action(&req.asset, req.market.as_deref())?;
    let asset = action_asset_address(&req.asset, market)?;
    let balance = call_u256(
        engine,
        chain,
        asset,
        compound_v3::encode_erc20_balance_of(smart_wallet_address),
    )
    .await?;
    if balance.is_zero() {
        anyhow::bail!("Compound III supply rejected: wallet has zero selected asset balance");
    }
    Ok(compound_v3::supply_with_amount_raw(req, balance))
}

async fn resolve_withdraw_amount(
    engine: &ExecutionEngine,
    chain: &Chain,
    req: &CompoundWithdrawRequest,
    smart_wallet_address: Address,
) -> Result<CompoundWithdrawRequest> {
    if !compound_v3::is_amount_max(req.amount.as_deref(), req.amount_raw.as_deref()) {
        return Ok(req.clone());
    }
    let market = compound_v3::market_from_action(&req.asset, req.market.as_deref())?;
    let comet = market.comet();
    let asset = action_asset_address(&req.asset, market)?;
    let base = call_address(engine, chain, comet, compound_v3::encode_base_token()).await?;
    let balance = if asset == base {
        call_u256(
            engine,
            chain,
            comet,
            compound_v3::encode_balance_of(smart_wallet_address),
        )
        .await?
    } else {
        call_u256(
            engine,
            chain,
            comet,
            compound_v3::encode_collateral_balance_of(smart_wallet_address, asset),
        )
        .await?
    };
    if balance.is_zero() {
        anyhow::bail!("Compound III withdraw rejected: no supplied balance for selected asset");
    }
    Ok(compound_v3::withdraw_with_amount_raw(req, balance))
}

async fn resolve_repay_amount(
    engine: &ExecutionEngine,
    chain: &Chain,
    req: &CompoundRepayRequest,
    smart_wallet_address: Address,
) -> Result<CompoundRepayRequest> {
    if !compound_v3::is_amount_max(req.amount.as_deref(), req.amount_raw.as_deref()) {
        return Ok(req.clone());
    }
    let market = compound_v3::market_from_action(&req.asset, req.market.as_deref())?;
    let comet = market.comet();
    let base = market.base_token();
    let (debt, wallet_balance) = tokio::try_join!(
        call_u256(
            engine,
            chain,
            comet,
            compound_v3::encode_borrow_balance_of(smart_wallet_address)
        ),
        call_u256(
            engine,
            chain,
            base,
            compound_v3::encode_erc20_balance_of(smart_wallet_address)
        )
    )?;
    if debt.is_zero() {
        anyhow::bail!("Compound III repay rejected: no base debt");
    }
    if wallet_balance.is_zero() {
        anyhow::bail!("Compound III repay rejected: wallet has zero base asset balance");
    }
    Ok(compound_v3::repay_with_amount_raw(
        req,
        min_u256(debt, wallet_balance),
    ))
}

async fn resolve_borrow_amount(
    _engine: &ExecutionEngine,
    _chain: &Chain,
    req: &CompoundBorrowRequest,
    _smart_wallet_address: Address,
) -> Result<CompoundBorrowRequest> {
    if compound_v3::is_amount_max(req.amount.as_deref(), req.amount_raw.as_deref()) {
        anyhow::bail!(
            "Compound III borrow amount max is not supported yet; provide amount or amount_raw"
        );
    }
    Ok(req.clone())
}

async fn collateral_balances(
    engine: &ExecutionEngine,
    chain: &Chain,
    comet: Address,
    smart_wallet_address: Address,
) -> Result<Vec<CompoundCollateralBalance>> {
    let count = call_u256(engine, chain, comet, compound_v3::encode_num_assets()).await?;
    if count > U256::from(u8::MAX) {
        anyhow::bail!("Compound III market reports too many collateral assets");
    }

    let mut out = Vec::new();
    for index in 0..count.as_u32() {
        let info = call_asset_info(engine, chain, comet, index as u8).await?;
        let (symbol, decimals, balance) = tokio::try_join!(
            token_symbol(engine, chain, info.asset),
            token_decimals(engine, chain, info.asset),
            call_u256(
                engine,
                chain,
                comet,
                compound_v3::encode_collateral_balance_of(smart_wallet_address, info.asset)
            ),
        )?;
        out.push(CompoundCollateralBalance {
            symbol,
            token_address: format!("{:?}", info.asset),
            decimals,
            collateral_balance_raw: balance.to_string(),
            collateral_balance_formatted: format_token_units(balance, decimals)?,
        });
    }
    Ok(out)
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
    compound_v3::decode_u256(&raw.0)
}

async fn call_address(
    engine: &ExecutionEngine,
    chain: &Chain,
    to: Address,
    calldata: String,
) -> Result<Address> {
    let provider = engine.provider_for_chain(chain)?;
    let data: Bytes = hex::decode(calldata.trim_start_matches("0x"))?.into();
    let tx = TransactionRequest::new().to(to).data(data);
    let raw = provider.call(&tx.into(), None).await?;
    compound_v3::decode_address(&raw.0)
}

async fn call_asset_info(
    engine: &ExecutionEngine,
    chain: &Chain,
    comet: Address,
    index: u8,
) -> Result<compound_v3::CompoundAssetInfo> {
    let provider = engine.provider_for_chain(chain)?;
    let data: Bytes =
        hex::decode(compound_v3::encode_get_asset_info(index).trim_start_matches("0x"))?.into();
    let tx = TransactionRequest::new().to(comet).data(data);
    let raw = provider.call(&tx.into(), None).await?;
    compound_v3::decode_asset_info(&raw.0)
}

async fn token_decimals(engine: &ExecutionEngine, chain: &Chain, token: Address) -> Result<u8> {
    let provider = engine.provider_for_chain(chain)?;
    let data: Bytes =
        hex::decode(compound_v3::encode_erc20_decimals().trim_start_matches("0x"))?.into();
    let tx = TransactionRequest::new().to(token).data(data);
    let raw = provider.call(&tx.into(), None).await?;
    compound_v3::decode_u8(&raw.0)
}

async fn token_symbol(engine: &ExecutionEngine, chain: &Chain, token: Address) -> Result<String> {
    let provider = engine.provider_for_chain(chain)?;
    let data: Bytes =
        hex::decode(compound_v3::encode_erc20_symbol().trim_start_matches("0x"))?.into();
    let tx = TransactionRequest::new().to(token).data(data);
    let raw = provider.call(&tx.into(), None).await?;
    compound_v3::decode_string(&raw.0).or_else(|_| Ok("UNKNOWN".to_string()))
}

fn action_asset_address(asset: &str, market: compound_v3::CompoundMarket) -> Result<Address> {
    match asset.trim().to_uppercase().as_str() {
        "BASE" => Ok(market.base_token()),
        "USDC" => "0x036CbD53842c5426634e7929541eC2318f3dCF7e"
            .parse()
            .map_err(Into::into),
        "WETH" => "0x4200000000000000000000000000000000000006"
            .parse()
            .map_err(Into::into),
        _ => asset
            .parse()
            .map_err(|e| anyhow::anyhow!("asset must be a symbol or token address: {e}")),
    }
}

fn parse_chain(chain: &str) -> Result<Chain> {
    Chain::from_str_loose(chain).ok_or_else(|| anyhow::anyhow!("unsupported chain: {}", chain))
}

fn min_u256(a: U256, b: U256) -> U256 {
    if a < b {
        a
    } else {
        b
    }
}

fn format_token_units(value: U256, decimals: u8) -> Result<String> {
    format_units(value, decimals as usize).map_err(Into::into)
}
