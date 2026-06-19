use anyhow::{anyhow, Result};
use chrono::Utc;
use ethers::abi::{self, ParamType};
use ethers::prelude::Middleware;
use ethers::types::{Address, Bytes, TransactionRequest, U256};
use ethers::utils::format_units;
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

use super::adapter as balancer_v3;
use super::adapter::{
    BalancerAddLiquidityQuoteResponse, BalancerAddLiquidityRequest, BalancerBalancesQuery,
    BalancerBalancesResponse, BalancerLiquidityAmount, BalancerPoolQuery, BalancerPoolResponse,
    BalancerPoolToken, BalancerQuoteResponse, BalancerRemoveLiquidityQuoteResponse,
    BalancerRemoveLiquidityRequest, BalancerSwapKind, BalancerSwapRequest, BalancerTokenAmount,
    BalancerTokenBalance,
};
use crate::agent_wallet::AgentWalletRegistry;
use crate::api::services::{handle_execute, handle_simulate, resolve_chain_smart_wallet_address};
use crate::execution_engine::ExecutionEngine;
use crate::relayer::erc4337::BundlerClient;
use crate::relayer::paymaster::PaymasterSigner;
use crate::types::{Chain, ExecutionResponse, PaymentMode, PaymentProof};

const DEFAULT_DEADLINE_SECS: u64 = 20 * 60;
const BPS_SCALE: u64 = 10_000;

pub async fn handle_swap(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &BalancerSwapRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    let resolved = resolve_swap(engine, wallet_registry, api_key_id, req).await?;
    let execution_req = balancer_v3::compile_swap(&resolved)?;

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
    .map_err(|e| anyhow!("Balancer V3 swap on Ethereum Sepolia failed: {e}"))
}

pub async fn handle_swap_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &BalancerSwapRequest,
) -> Result<ExecutionResponse> {
    let resolved = resolve_swap(engine, wallet_registry, api_key_id, req).await?;
    let execution_req = balancer_v3::compile_swap(&resolved)?;

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

pub async fn handle_add_liquidity(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &BalancerAddLiquidityRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    let resolved = resolve_add_liquidity(engine, wallet_registry, api_key_id, req).await?;
    let execution_req = balancer_v3::compile_add_liquidity(
        req,
        &resolved.tokens,
        &resolved.amounts_in,
        resolved.min_bpt_amount_out,
        resolved.deadline,
    )?;
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
    .map_err(|e| anyhow!("Balancer V3 add liquidity on Ethereum Sepolia failed: {e}"))
}

pub async fn handle_add_liquidity_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &BalancerAddLiquidityRequest,
) -> Result<ExecutionResponse> {
    let resolved = resolve_add_liquidity(engine, wallet_registry, api_key_id, req).await?;
    let execution_req = balancer_v3::compile_add_liquidity(
        req,
        &resolved.tokens,
        &resolved.amounts_in,
        resolved.min_bpt_amount_out,
        resolved.deadline,
    )?;
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

pub async fn handle_remove_liquidity(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &BalancerRemoveLiquidityRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    let resolved = resolve_remove_liquidity(engine, wallet_registry, api_key_id, req).await?;
    let execution_req = balancer_v3::compile_remove_liquidity(req, &resolved.min_amounts_out)?;
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
    .map_err(|e| anyhow!("Balancer V3 remove liquidity on Ethereum Sepolia failed: {e}"))
}

pub async fn handle_remove_liquidity_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &BalancerRemoveLiquidityRequest,
) -> Result<ExecutionResponse> {
    let resolved = resolve_remove_liquidity(engine, wallet_registry, api_key_id, req).await?;
    let execution_req = balancer_v3::compile_remove_liquidity(req, &resolved.min_amounts_out)?;
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

pub async fn handle_quote(
    engine: &ExecutionEngine,
    wallet_registry: &AgentWalletRegistry,
    api_key_id: Uuid,
    req: &BalancerSwapRequest,
) -> Result<BalancerQuoteResponse> {
    balancer_v3::validate_swap_request(req)?;
    let chain = parse_chain(&req.chain)?;
    let wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let smart_wallet_address = resolve_chain_smart_wallet_address(engine, &chain, &wallet).await?;
    validate_pool_for_swap(engine, &chain, req).await?;

    let quoted = quote_swap(engine, &chain, req, smart_wallet_address).await?;
    let limit = match balancer_v3::explicit_limit(req)? {
        Some(limit) => {
            validate_explicit_limit(req.swap_kind, quoted, limit)?;
            limit
        }
        None => limit_from_quote(req.swap_kind, quoted, req.slippage_bps)?,
    };
    let max_input = match req.swap_kind {
        BalancerSwapKind::ExactIn => balancer_v3::amount(req)?,
        BalancerSwapKind::ExactOut => limit,
    };
    balancer_v3::validate_permit2_amount(max_input, "Balancer swap maximum input")?;
    let deadline = resolve_deadline(req.deadline, "swap")?;

    Ok(BalancerQuoteResponse {
        agent_id: req.agent_id.clone(),
        chain: "ethereum".to_string(),
        smart_wallet_address: format!("{smart_wallet_address:?}"),
        pool_address: format!(
            "{:?}",
            balancer_v3::parse_request_address(&req.pool, "pool")?
        ),
        token_in: format!(
            "{:?}",
            balancer_v3::parse_request_address(&req.token_in, "token_in")?
        ),
        token_out: format!(
            "{:?}",
            balancer_v3::parse_request_address(&req.token_out, "token_out")?
        ),
        swap_kind: req.swap_kind,
        amount_raw: balancer_v3::amount(req)?.to_string(),
        quoted_amount_raw: quoted.to_string(),
        limit_raw: limit.to_string(),
        slippage_bps: req.slippage_bps,
        deadline,
    })
}

pub async fn handle_add_liquidity_quote(
    engine: &ExecutionEngine,
    wallet_registry: &AgentWalletRegistry,
    api_key_id: Uuid,
    req: &BalancerAddLiquidityRequest,
) -> Result<BalancerAddLiquidityQuoteResponse> {
    let resolved = resolve_add_liquidity(engine, wallet_registry, api_key_id, req).await?;
    Ok(BalancerAddLiquidityQuoteResponse {
        agent_id: req.agent_id.clone(),
        chain: "ethereum".to_string(),
        smart_wallet_address: format!("{:?}", resolved.smart_wallet_address),
        pool_address: format!(
            "{:?}",
            balancer_v3::parse_request_address(&req.pool, "pool")?
        ),
        amounts_in: liquidity_amounts(&resolved.tokens, &resolved.amounts_in),
        quoted_bpt_amount_out_raw: resolved.quoted_bpt_amount_out.to_string(),
        min_bpt_amount_out_raw: resolved.min_bpt_amount_out.to_string(),
        slippage_bps: req.slippage_bps,
        deadline: resolved.deadline,
    })
}

pub async fn handle_remove_liquidity_quote(
    engine: &ExecutionEngine,
    wallet_registry: &AgentWalletRegistry,
    api_key_id: Uuid,
    req: &BalancerRemoveLiquidityRequest,
) -> Result<BalancerRemoveLiquidityQuoteResponse> {
    let resolved = resolve_remove_liquidity(engine, wallet_registry, api_key_id, req).await?;
    Ok(BalancerRemoveLiquidityQuoteResponse {
        agent_id: req.agent_id.clone(),
        chain: "ethereum".to_string(),
        smart_wallet_address: format!("{:?}", resolved.smart_wallet_address),
        pool_address: format!(
            "{:?}",
            balancer_v3::parse_request_address(&req.pool, "pool")?
        ),
        bpt_amount_in_raw: req.bpt_amount_in_raw.clone(),
        quoted_amounts_out: liquidity_amounts(&resolved.tokens, &resolved.quoted_amounts_out),
        min_amounts_out: liquidity_amounts(&resolved.tokens, &resolved.min_amounts_out),
        slippage_bps: req.slippage_bps,
    })
}

pub async fn handle_pool(
    engine: &ExecutionEngine,
    query: &BalancerPoolQuery,
) -> Result<BalancerPoolResponse> {
    balancer_v3::validate_pool_query(query)?;
    let chain = parse_chain(&query.chain)?;
    let pool = balancer_v3::parse_request_address(&query.pool, "pool")?;
    let state = load_pool_state(engine, &chain, pool).await?;

    let (pool_name, pool_symbol, pool_decimals, total_supply) = tokio::try_join!(
        token_name(engine, &chain, pool),
        token_symbol(engine, &chain, pool),
        token_decimals(engine, &chain, pool),
        call_u256(
            engine,
            &chain,
            pool,
            balancer_v3::encode_total_supply(),
            "Balancer BPT total supply"
        ),
    )?;

    let mut tokens = Vec::with_capacity(state.tokens.len());
    for (index, token) in state.tokens.iter().copied().enumerate() {
        let decimals = token_decimals(engine, &chain, token).await?;
        let symbol = token_symbol_or_fallback(engine, &chain, token).await;
        let name = token_name_or_fallback(engine, &chain, token, &symbol).await;
        let raw_balance = state.raw_balances[index];
        tokens.push(BalancerPoolToken {
            index,
            address: format!("{token:?}"),
            symbol,
            name,
            decimals,
            pool_balance_raw: raw_balance.to_string(),
            pool_balance_formatted: format_token_units(raw_balance, decimals)?,
            live_balance_scaled_18_raw: state.live_balances[index].to_string(),
        });
    }

    Ok(BalancerPoolResponse {
        chain: "ethereum".to_string(),
        pool_address: format!("{pool:?}"),
        pool_name,
        pool_symbol,
        pool_decimals,
        total_supply_raw: total_supply.to_string(),
        total_supply_formatted: format_token_units(total_supply, pool_decimals)?,
        router_address: format!("{:?}", balancer_v3::router_address()),
        vault_address: format!("{:?}", balancer_v3::vault_address()),
        vault_explorer_address: format!("{:?}", balancer_v3::vault_explorer_address()),
        permit2_address: format!("{:?}", balancer_v3::permit2_address()),
        is_registered: true,
        is_initialized: state.is_initialized,
        is_paused: state.is_paused,
        is_in_recovery_mode: state.is_in_recovery_mode,
        static_swap_fee_percentage_raw: state.static_swap_fee_percentage.to_string(),
        tokens,
    })
}

pub async fn handle_balances(
    engine: &ExecutionEngine,
    wallet_registry: &AgentWalletRegistry,
    api_key_id: Uuid,
    query: &BalancerBalancesQuery,
) -> Result<BalancerBalancesResponse> {
    balancer_v3::validate_balances_query(query)?;
    let chain = parse_chain(&query.chain)?;
    let pool = balancer_v3::parse_request_address(&query.pool, "pool")?;
    let wallet = wallet_registry
        .get_or_create(api_key_id, &query.agent_id)
        .await?;
    let smart_wallet_address = resolve_chain_smart_wallet_address(engine, &chain, &wallet).await?;
    let state = load_pool_state(engine, &chain, pool).await?;

    let (pool_decimals, bpt_balance) = tokio::try_join!(
        token_decimals(engine, &chain, pool),
        call_u256(
            engine,
            &chain,
            pool,
            balancer_v3::encode_balance_of(smart_wallet_address),
            "Balancer BPT balance"
        ),
    )?;

    let mut tokens = Vec::with_capacity(state.tokens.len());
    for token in state.tokens {
        let (decimals, wallet_balance) = tokio::try_join!(
            token_decimals(engine, &chain, token),
            call_u256(
                engine,
                &chain,
                token,
                balancer_v3::encode_balance_of(smart_wallet_address),
                "Balancer token wallet balance"
            ),
        )?;
        let symbol = token_symbol_or_fallback(engine, &chain, token).await;
        let name = token_name_or_fallback(engine, &chain, token, &symbol).await;
        tokens.push(BalancerTokenBalance {
            address: format!("{token:?}"),
            symbol,
            name,
            decimals,
            wallet_balance_raw: wallet_balance.to_string(),
            wallet_balance_formatted: format_token_units(wallet_balance, decimals)?,
        });
    }

    Ok(BalancerBalancesResponse {
        agent_id: query.agent_id.clone(),
        chain: "ethereum".to_string(),
        smart_wallet_address: format!("{smart_wallet_address:?}"),
        pool_address: format!("{pool:?}"),
        bpt_balance_raw: bpt_balance.to_string(),
        bpt_balance_formatted: format_token_units(bpt_balance, pool_decimals)?,
        tokens,
    })
}

async fn resolve_swap(
    engine: &ExecutionEngine,
    wallet_registry: &AgentWalletRegistry,
    api_key_id: Uuid,
    req: &BalancerSwapRequest,
) -> Result<BalancerSwapRequest> {
    let quote = handle_quote(engine, wallet_registry, api_key_id, req).await?;
    let limit = U256::from_dec_str(&quote.limit_raw)
        .map_err(|_| anyhow!("failed to parse resolved Balancer swap limit"))?;
    Ok(balancer_v3::swap_with_resolved_limit(
        req,
        limit,
        quote.deadline,
    ))
}

struct ResolvedAddLiquidity {
    smart_wallet_address: Address,
    tokens: Vec<Address>,
    amounts_in: Vec<U256>,
    quoted_bpt_amount_out: U256,
    min_bpt_amount_out: U256,
    deadline: u64,
}

async fn resolve_add_liquidity(
    engine: &ExecutionEngine,
    wallet_registry: &AgentWalletRegistry,
    api_key_id: Uuid,
    req: &BalancerAddLiquidityRequest,
) -> Result<ResolvedAddLiquidity> {
    balancer_v3::validate_add_liquidity_request(req)?;
    let chain = parse_chain(&req.chain)?;
    let pool = balancer_v3::parse_request_address(&req.pool, "pool")?;
    let wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let smart_wallet_address = resolve_chain_smart_wallet_address(engine, &chain, &wallet).await?;
    let state = validate_pool_for_liquidity(engine, &chain, pool, "add").await?;
    let amounts_in = order_token_amounts(&state.tokens, &req.amounts_in, "amounts_in")?;
    let quoted_bpt_amount_out = call_u256(
        engine,
        &chain,
        balancer_v3::router_address(),
        balancer_v3::encode_query_add_liquidity_unbalanced(pool, &amounts_in, smart_wallet_address),
        "Balancer V3 add-liquidity quote",
    )
    .await
    .map_err(|e| {
        let raw = e.to_string();
        if raw.to_ascii_lowercase().contains("0xd4f5779c") {
            anyhow!(
                "Balancer V3 add liquidity rejected: the selected pool does not support unbalanced liquidity"
            )
        } else {
            anyhow!("Balancer V3 add liquidity rejected by Router quote: {raw}")
        }
    })?;
    if quoted_bpt_amount_out.is_zero() {
        return Err(anyhow!("Balancer V3 add-liquidity quote returned zero BPT"));
    }
    let min_bpt_amount_out = match req.min_bpt_amount_out_raw.as_deref() {
        Some(value) => {
            let explicit = U256::from_dec_str(value)
                .map_err(|_| anyhow!("min_bpt_amount_out_raw must be a raw unsigned integer"))?;
            if explicit > quoted_bpt_amount_out {
                return Err(anyhow!(
                    "Balancer min_bpt_amount_out_raw exceeds the current quoted BPT output"
                ));
            }
            explicit
        }
        None => downward_slippage(quoted_bpt_amount_out, req.slippage_bps)?,
    };

    Ok(ResolvedAddLiquidity {
        smart_wallet_address,
        tokens: state.tokens,
        amounts_in,
        quoted_bpt_amount_out,
        min_bpt_amount_out,
        deadline: resolve_deadline(req.deadline, "liquidity approval")?,
    })
}

struct ResolvedRemoveLiquidity {
    smart_wallet_address: Address,
    tokens: Vec<Address>,
    quoted_amounts_out: Vec<U256>,
    min_amounts_out: Vec<U256>,
}

async fn resolve_remove_liquidity(
    engine: &ExecutionEngine,
    wallet_registry: &AgentWalletRegistry,
    api_key_id: Uuid,
    req: &BalancerRemoveLiquidityRequest,
) -> Result<ResolvedRemoveLiquidity> {
    balancer_v3::validate_remove_liquidity_request(req)?;
    let chain = parse_chain(&req.chain)?;
    let pool = balancer_v3::parse_request_address(&req.pool, "pool")?;
    let bpt_amount_in = U256::from_dec_str(&req.bpt_amount_in_raw)
        .map_err(|_| anyhow!("bpt_amount_in_raw must be a raw unsigned integer"))?;
    let wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let smart_wallet_address = resolve_chain_smart_wallet_address(engine, &chain, &wallet).await?;
    let state = validate_pool_for_liquidity(engine, &chain, pool, "remove").await?;
    let bpt_balance = call_u256(
        engine,
        &chain,
        pool,
        balancer_v3::encode_balance_of(smart_wallet_address),
        "Balancer BPT balance",
    )
    .await?;
    if bpt_amount_in > bpt_balance {
        return Err(anyhow!(
            "Balancer remove liquidity requested BPT amount {bpt_amount_in} exceeds the agent wallet balance {bpt_balance}"
        ));
    }
    let quoted_amounts_out = call_u256_array(
        engine,
        &chain,
        balancer_v3::router_address(),
        balancer_v3::encode_query_remove_liquidity_proportional(
            pool,
            bpt_amount_in,
            smart_wallet_address,
        ),
        "Balancer V3 remove-liquidity quote",
    )
    .await
    .map_err(|e| anyhow!("Balancer V3 remove liquidity rejected by Router quote: {e}"))?;
    if quoted_amounts_out.len() != state.tokens.len() {
        return Err(anyhow!(
            "Balancer remove-liquidity quote returned an unexpected token count"
        ));
    }

    let min_amounts_out = match req.min_amounts_out.as_deref() {
        Some(explicit) => {
            let ordered = order_token_amounts(&state.tokens, explicit, "min_amounts_out")?;
            for (minimum, quote) in ordered.iter().zip(&quoted_amounts_out) {
                if minimum > quote {
                    return Err(anyhow!(
                        "Balancer min_amounts_out contains an amount above the current quote"
                    ));
                }
            }
            ordered
        }
        None => quoted_amounts_out
            .iter()
            .copied()
            .map(|quote| downward_slippage_allow_zero(quote, req.slippage_bps))
            .collect::<Result<Vec<_>>>()?,
    };

    Ok(ResolvedRemoveLiquidity {
        smart_wallet_address,
        tokens: state.tokens,
        quoted_amounts_out,
        min_amounts_out,
    })
}

async fn validate_pool_for_swap(
    engine: &ExecutionEngine,
    chain: &Chain,
    req: &BalancerSwapRequest,
) -> Result<()> {
    let pool = balancer_v3::parse_request_address(&req.pool, "pool")?;
    let token_in = balancer_v3::parse_request_address(&req.token_in, "token_in")?;
    let token_out = balancer_v3::parse_request_address(&req.token_out, "token_out")?;
    let state = load_pool_state(engine, chain, pool).await?;

    if !state.is_initialized {
        return Err(anyhow!(
            "Balancer V3 swap rejected: pool is not initialized"
        ));
    }
    if state.is_paused {
        return Err(anyhow!("Balancer V3 swap rejected: pool is paused"));
    }
    if state.is_in_recovery_mode {
        return Err(anyhow!(
            "Balancer V3 swap rejected: pool is in recovery mode"
        ));
    }
    if !state.tokens.contains(&token_in) {
        return Err(anyhow!(
            "Balancer V3 swap rejected: token_in is not registered in the selected pool"
        ));
    }
    if !state.tokens.contains(&token_out) {
        return Err(anyhow!(
            "Balancer V3 swap rejected: token_out is not registered in the selected pool"
        ));
    }
    Ok(())
}

async fn quote_swap(
    engine: &ExecutionEngine,
    chain: &Chain,
    req: &BalancerSwapRequest,
    sender: Address,
) -> Result<U256> {
    let calldata = balancer_v3::encode_query_swap(req, sender)?;
    call_u256(
        engine,
        chain,
        balancer_v3::router_address(),
        calldata,
        "Balancer V3 swap quote",
    )
    .await
    .map_err(|e| {
        let raw = e.to_string();
        if raw.to_ascii_lowercase().contains("0xfdf79845") {
            anyhow!("Balancer V3 swap rejected: swaps are disabled for the selected pool")
        } else {
            anyhow!("Balancer V3 swap rejected by Router quote: {raw}")
        }
    })
}

fn validate_explicit_limit(kind: BalancerSwapKind, quote: U256, limit: U256) -> Result<()> {
    match kind {
        BalancerSwapKind::ExactIn if limit > quote => Err(anyhow!(
            "Balancer exact-in limit_raw exceeds the current quoted output"
        )),
        BalancerSwapKind::ExactOut if limit < quote => Err(anyhow!(
            "Balancer exact-out limit_raw is below the current quoted input"
        )),
        _ => Ok(()),
    }
}

fn limit_from_quote(kind: BalancerSwapKind, quote: U256, slippage_bps: u16) -> Result<U256> {
    if quote.is_zero() {
        return Err(anyhow!("Balancer V3 swap quote returned zero"));
    }
    let scale = U256::from(BPS_SCALE);
    let bps = U256::from(slippage_bps);
    let limit = match kind {
        BalancerSwapKind::ExactIn => {
            quote
                .checked_mul(scale - bps)
                .ok_or_else(|| anyhow!("Balancer swap minimum output calculation overflowed"))?
                / scale
        }
        BalancerSwapKind::ExactOut => {
            let numerator = quote
                .checked_mul(scale + bps)
                .ok_or_else(|| anyhow!("Balancer swap maximum input calculation overflowed"))?;
            numerator
                .checked_add(scale - U256::one())
                .ok_or_else(|| anyhow!("Balancer swap maximum input calculation overflowed"))?
                / scale
        }
    };
    if limit.is_zero() {
        return Err(anyhow!(
            "Balancer V3 swap slippage limit rounded to zero; provide limit_raw explicitly"
        ));
    }
    Ok(limit)
}

fn resolve_deadline(requested: Option<u64>, context: &str) -> Result<u64> {
    let now = Utc::now().timestamp();
    if now < 0 {
        return Err(anyhow!("system clock is before the Unix epoch"));
    }
    let now = now as u64;
    let deadline = match requested {
        Some(deadline) => {
            if deadline <= now {
                return Err(anyhow!("Balancer {context} deadline must be in the future"));
            }
            deadline
        }
        None => now
            .checked_add(DEFAULT_DEADLINE_SECS)
            .ok_or_else(|| anyhow!("Balancer {context} deadline overflowed"))?,
    };
    if deadline > ((1u64 << 48) - 1) {
        return Err(anyhow!("Balancer {context} deadline exceeds uint48"));
    }
    Ok(deadline)
}

async fn validate_pool_for_liquidity(
    engine: &ExecutionEngine,
    chain: &Chain,
    pool: Address,
    action: &str,
) -> Result<PoolState> {
    let state = load_pool_state(engine, chain, pool).await?;
    if !state.is_initialized {
        return Err(anyhow!(
            "Balancer V3 {action} liquidity rejected: pool is not initialized"
        ));
    }
    if state.is_paused {
        return Err(anyhow!(
            "Balancer V3 {action} liquidity rejected: pool is paused"
        ));
    }
    if state.is_in_recovery_mode {
        return Err(anyhow!(
            "Balancer V3 {action} liquidity rejected: pool is in recovery mode"
        ));
    }
    Ok(state)
}

fn order_token_amounts(
    pool_tokens: &[Address],
    requested: &[BalancerTokenAmount],
    field: &str,
) -> Result<Vec<U256>> {
    let mut ordered = vec![U256::zero(); pool_tokens.len()];
    for entry in requested {
        let token = balancer_v3::parse_request_address(&entry.token, &format!("{field}.token"))?;
        let index = pool_tokens
            .iter()
            .position(|candidate| *candidate == token)
            .ok_or_else(|| {
                anyhow!("Balancer {field} token {token:?} is not registered in the selected pool")
            })?;
        ordered[index] = U256::from_dec_str(&entry.amount_raw)
            .map_err(|_| anyhow!("{field}.amount_raw must be a raw unsigned integer"))?;
    }
    Ok(ordered)
}

fn liquidity_amounts(tokens: &[Address], amounts: &[U256]) -> Vec<BalancerLiquidityAmount> {
    tokens
        .iter()
        .zip(amounts)
        .map(|(token, amount)| BalancerLiquidityAmount {
            token: format!("{token:?}"),
            amount_raw: amount.to_string(),
        })
        .collect()
}

fn downward_slippage(value: U256, slippage_bps: u16) -> Result<U256> {
    let adjusted = downward_slippage_allow_zero(value, slippage_bps)?;
    if adjusted.is_zero() {
        return Err(anyhow!(
            "Balancer slippage limit rounded to zero; provide an explicit minimum"
        ));
    }
    Ok(adjusted)
}

fn downward_slippage_allow_zero(value: U256, slippage_bps: u16) -> Result<U256> {
    value
        .checked_mul(U256::from(BPS_SCALE - u64::from(slippage_bps)))
        .ok_or_else(|| anyhow!("Balancer liquidity slippage calculation overflowed"))
        .map(|numerator| numerator / U256::from(BPS_SCALE))
}

struct PoolState {
    tokens: Vec<Address>,
    raw_balances: Vec<U256>,
    live_balances: Vec<U256>,
    is_initialized: bool,
    is_paused: bool,
    is_in_recovery_mode: bool,
    static_swap_fee_percentage: U256,
}

async fn load_pool_state(
    engine: &ExecutionEngine,
    chain: &Chain,
    pool: Address,
) -> Result<PoolState> {
    let explorer = balancer_v3::vault_explorer_address();
    let is_registered = call_bool(
        engine,
        chain,
        explorer,
        balancer_v3::encode_is_pool_registered(pool),
        "Balancer pool registration",
    )
    .await?;
    if !is_registered {
        return Err(anyhow!(
            "Balancer V3 pool is not registered in the Sepolia Vault"
        ));
    }

    let (token_info_raw, live_balances, is_initialized, is_paused, recovery, fee) = tokio::try_join!(
        call_raw(
            engine,
            chain,
            explorer,
            balancer_v3::encode_get_pool_token_info(pool)
        ),
        call_u256_array(
            engine,
            chain,
            explorer,
            balancer_v3::encode_get_current_live_balances(pool),
            "Balancer live pool balances"
        ),
        call_bool(
            engine,
            chain,
            explorer,
            balancer_v3::encode_is_pool_initialized(pool),
            "Balancer pool initialization"
        ),
        call_bool(
            engine,
            chain,
            explorer,
            balancer_v3::encode_is_pool_paused(pool),
            "Balancer pool paused state"
        ),
        call_bool(
            engine,
            chain,
            explorer,
            balancer_v3::encode_is_pool_in_recovery_mode(pool),
            "Balancer pool recovery mode"
        ),
        call_u256(
            engine,
            chain,
            explorer,
            balancer_v3::encode_get_static_swap_fee_percentage(pool),
            "Balancer static swap fee"
        ),
    )?;
    let (tokens, raw_balances) = balancer_v3::decode_pool_token_info(&token_info_raw)?;
    if tokens.len() != live_balances.len() {
        return Err(anyhow!(
            "Balancer pool returned mismatched token and live-balance lengths"
        ));
    }

    Ok(PoolState {
        tokens,
        raw_balances,
        live_balances,
        is_initialized,
        is_paused,
        is_in_recovery_mode: recovery,
        static_swap_fee_percentage: fee,
    })
}

async fn call_raw(
    engine: &ExecutionEngine,
    chain: &Chain,
    target: Address,
    calldata: String,
) -> Result<Bytes> {
    let provider = engine.provider_for_chain(chain)?;
    let tx = TransactionRequest::new()
        .to(target)
        .data(parse_calldata(&calldata)?);
    Ok(provider.call(&tx.into(), None).await?)
}

async fn call_u256(
    engine: &ExecutionEngine,
    chain: &Chain,
    target: Address,
    calldata: String,
    context: &str,
) -> Result<U256> {
    let raw = call_raw(engine, chain, target, calldata).await?;
    balancer_v3::decode_u256(&raw, context)
}

async fn call_u256_array(
    engine: &ExecutionEngine,
    chain: &Chain,
    target: Address,
    calldata: String,
    context: &str,
) -> Result<Vec<U256>> {
    let raw = call_raw(engine, chain, target, calldata).await?;
    balancer_v3::decode_u256_array(&raw, context)
}

async fn call_bool(
    engine: &ExecutionEngine,
    chain: &Chain,
    target: Address,
    calldata: String,
    context: &str,
) -> Result<bool> {
    let raw = call_raw(engine, chain, target, calldata).await?;
    balancer_v3::decode_bool(&raw, context)
}

async fn token_decimals(engine: &ExecutionEngine, chain: &Chain, token: Address) -> Result<u8> {
    let raw = call_raw(engine, chain, token, balancer_v3::encode_decimals()).await?;
    let value = balancer_v3::decode_u256(&raw, "ERC-20 decimals")?;
    if value > U256::from(u8::MAX) {
        return Err(anyhow!("ERC-20 decimals exceeds uint8"));
    }
    Ok(value.as_u32() as u8)
}

async fn token_symbol(engine: &ExecutionEngine, chain: &Chain, token: Address) -> Result<String> {
    token_string(engine, chain, token, balancer_v3::encode_symbol(), "symbol").await
}

async fn token_name(engine: &ExecutionEngine, chain: &Chain, token: Address) -> Result<String> {
    token_string(engine, chain, token, balancer_v3::encode_name(), "name").await
}

async fn token_symbol_or_fallback(
    engine: &ExecutionEngine,
    chain: &Chain,
    token: Address,
) -> String {
    token_symbol(engine, chain, token)
        .await
        .unwrap_or_else(|_| compact_address_label(token))
}

async fn token_name_or_fallback(
    engine: &ExecutionEngine,
    chain: &Chain,
    token: Address,
    symbol: &str,
) -> String {
    token_name(engine, chain, token)
        .await
        .unwrap_or_else(|_| symbol.to_string())
}

fn compact_address_label(address: Address) -> String {
    let raw = format!("{address:?}");
    format!("{}...{}", &raw[..8], &raw[raw.len() - 4..])
}

async fn token_string(
    engine: &ExecutionEngine,
    chain: &Chain,
    token: Address,
    calldata: String,
    context: &str,
) -> Result<String> {
    let raw = call_raw(engine, chain, token, calldata).await?;
    if let Ok(decoded) = abi::decode(&[ParamType::String], &raw) {
        if let Some(value) = decoded[0].clone().into_string() {
            return Ok(value);
        }
    }
    if raw.len() == 32 {
        let value = raw
            .iter()
            .copied()
            .take_while(|byte| *byte != 0)
            .collect::<Vec<_>>();
        return String::from_utf8(value)
            .map_err(|e| anyhow!("failed to decode ERC-20 {context}: {e}"));
    }
    Err(anyhow!("failed to decode ERC-20 {context}"))
}

fn parse_calldata(value: &str) -> Result<Bytes> {
    let value = value
        .strip_prefix("0x")
        .ok_or_else(|| anyhow!("calldata must start with 0x"))?;
    Ok(Bytes::from(
        hex::decode(value).map_err(|e| anyhow!("invalid calldata hex: {e}"))?,
    ))
}

fn format_token_units(amount: U256, decimals: u8) -> Result<String> {
    format_units(amount, decimals as usize)
        .map_err(|e| anyhow!("failed to format token amount: {e}"))
}

fn parse_chain(chain: &str) -> Result<Chain> {
    match chain.trim().to_ascii_lowercase().as_str() {
        "ethereum" | "eth" | "sepolia" => Ok(Chain::Ethereum),
        other => Err(anyhow!(
            "unsupported chain for Balancer V3: {other}; use ethereum"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_in_limit_applies_downward_slippage() {
        assert_eq!(
            limit_from_quote(BalancerSwapKind::ExactIn, U256::from(1_000u64), 100).unwrap(),
            U256::from(990u64)
        );
    }

    #[test]
    fn exact_out_limit_rounds_up() {
        assert_eq!(
            limit_from_quote(BalancerSwapKind::ExactOut, U256::from(1_001u64), 100).unwrap(),
            U256::from(1_012u64)
        );
    }

    #[test]
    fn rejects_explicit_limit_that_conflicts_with_quote() {
        assert!(validate_explicit_limit(
            BalancerSwapKind::ExactIn,
            U256::from(100u64),
            U256::from(101u64)
        )
        .is_err());
        assert!(validate_explicit_limit(
            BalancerSwapKind::ExactOut,
            U256::from(100u64),
            U256::from(99u64)
        )
        .is_err());
    }

    #[test]
    fn orders_liquidity_amounts_by_vault_token_order() {
        let token_a = Address::from_low_u64_be(1);
        let token_b = Address::from_low_u64_be(2);
        let requested = vec![
            BalancerTokenAmount {
                token: format!("{token_b:?}"),
                amount_raw: "22".to_string(),
            },
            BalancerTokenAmount {
                token: format!("{token_a:?}"),
                amount_raw: "11".to_string(),
            },
        ];
        assert_eq!(
            order_token_amounts(&[token_a, token_b], &requested, "amounts_in").unwrap(),
            vec![U256::from(11u64), U256::from(22u64)]
        );
    }

    #[test]
    fn rejects_liquidity_token_not_registered_in_pool() {
        let requested = vec![BalancerTokenAmount {
            token: format!("{:?}", Address::from_low_u64_be(2)),
            amount_raw: "1".to_string(),
        }];
        assert!(
            order_token_amounts(&[Address::from_low_u64_be(1)], &requested, "amounts_in")
                .unwrap_err()
                .to_string()
                .contains("not registered")
        );
    }

    #[test]
    fn liquidity_slippage_rounds_down() {
        assert_eq!(
            downward_slippage(U256::from(1_001u64), 100).unwrap(),
            U256::from(990u64)
        );
        assert_eq!(
            downward_slippage_allow_zero(U256::zero(), 100).unwrap(),
            U256::zero()
        );
    }
}
