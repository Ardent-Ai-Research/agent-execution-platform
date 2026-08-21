//! Service layer — thin wrappers that coordinate between the execution engine,
//! agent wallet registry, database, and queue for each API endpoint.
//!
//! With ERC-4337, the flow is:
//!   validate → resolve smart wallet → simulate → enqueue

// Orchestration functions receive explicit shared dependencies from Axum state.
#![allow(clippy::too_many_arguments)]

use anyhow::Result;
use chrono::Utc;
use ethers::abi::{self, ParamType, Token};
use ethers::prelude::Middleware;
use ethers::types::{Address, Bytes, TransactionRequest, U256};
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use std::{collections::HashMap, net::IpAddr};
use tracing::info;
use uuid::Uuid;

use crate::agent_wallet::AgentWalletRegistry;
use crate::db;
use crate::execution_engine::ExecutionEngine;
use crate::queue;
use crate::relayer::erc4337::BundlerClient;
use crate::relayer::paymaster::PaymasterSigner;
use crate::types::*;

async fn simulate_request(
    engine: &ExecutionEngine,
    bundler_client: &BundlerClient,
    paymaster_signer: Option<&PaymasterSigner>,
    wallet_registry: &AgentWalletRegistry,
    req: &ExecutionRequest,
    chain: &Chain,
    agent_wallet: &crate::agent_wallet::AgentWallet,
    smart_wallet_address: Address,
    smart_wallet_str: &str,
) -> Result<SimulationResult> {
    if req.batch_calls.is_some() {
        if let Some(insufficient) =
            check_batch_native_value_balance(engine, req, chain, smart_wallet_address).await?
        {
            return Ok(insufficient);
        }

        simulate_batch_user_op(
            bundler_client,
            paymaster_signer,
            wallet_registry,
            req,
            chain,
            agent_wallet,
            smart_wallet_address,
            smart_wallet_str,
        )
        .await
    } else {
        engine.simulate(req, chain, smart_wallet_address).await
    }
}

async fn check_batch_native_value_balance(
    engine: &ExecutionEngine,
    req: &ExecutionRequest,
    chain: &Chain,
    smart_wallet_address: Address,
) -> Result<Option<SimulationResult>> {
    let required = batch_native_value_required(req)?;
    if required.is_zero() {
        return Ok(None);
    }

    let provider = engine.provider_for_chain(chain)?;
    let balance = provider.get_balance(smart_wallet_address, None).await?;
    if balance >= required {
        return Ok(None);
    }

    Ok(Some(SimulationResult {
        success: false,
        gas_estimate: 0,
        return_data: None,
        error: Some(format!(
            "insufficient native token balance for batched call value on {chain}: smart wallet {smart_wallet_address:?} has {balance} wei, requires {required} wei. Paymaster sponsorship covers gas only; GMX execution_fee_raw and other call values must be funded in the smart wallet."
        )),
    }))
}

fn batch_native_value_required(req: &ExecutionRequest) -> Result<U256> {
    let mut required = U256::zero();

    if let Some(batch_calls) = &req.batch_calls {
        for call in batch_calls {
            let value = parse_native_value(&call.value)?;
            required = required
                .checked_add(value)
                .ok_or_else(|| anyhow::anyhow!("batch call value overflow"))?;
        }
    }

    Ok(required)
}

fn parse_native_value(raw: &str) -> Result<U256> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "0" {
        return Ok(U256::zero());
    }

    U256::from_dec_str(trimmed).map_err(|_| anyhow::anyhow!("invalid batch call value: {raw}"))
}

/// Simulate a batch as the exact ERC-4337 `executeBatch` UserOperation shape.
///
/// The older batch simulation path ran each leg independently with `eth_call`,
/// which cannot observe state changes from earlier calls in the same batch
/// (for example `approve -> supply`).  Bundler estimation validates the full
/// smart-wallet callData atomically, so it catches cross-call dependencies
/// much closer to the eventual execution path.
async fn simulate_batch_user_op(
    bundler_client: &BundlerClient,
    paymaster_signer: Option<&PaymasterSigner>,
    wallet_registry: &AgentWalletRegistry,
    req: &ExecutionRequest,
    chain: &Chain,
    agent_wallet: &crate::agent_wallet::AgentWallet,
    smart_wallet: Address,
    smart_wallet_str: &str,
) -> Result<SimulationResult> {
    let job = ExecutionJob {
        request_id: Uuid::new_v4(),
        agent_id: req.agent_id.clone(),
        smart_wallet_address: smart_wallet_str.to_string(),
        eoa_address: format!("{:?}", agent_wallet.eoa_address),
        chain: chain.clone(),
        target_contract: req.target_contract.clone(),
        calldata: req.calldata.clone(),
        value: req.value.clone(),
        gas_limit: 0,
        created_at: Utc::now(),
        attempt_count: 0,
        batch_calls: req.batch_calls.clone(),
        callback_url: None,
        api_key_hash: None,
    };

    let chain_id = bundler_client.provider().get_chainid().await?.as_u64();

    let estimation_paymaster = match paymaster_signer {
        Some(signer) => {
            let mut draft_op = bundler_client
                .build_user_op_draft(&job, smart_wallet, Vec::new())
                .await?;
            bundler_client
                .apply_estimation_fee_hints(&mut draft_op)
                .await?;
            signer.sign_paymaster_data(&draft_op, chain_id).await?
        }
        None => Vec::new(),
    };

    let mut user_op = bundler_client
        .build_user_op_draft(&job, smart_wallet, estimation_paymaster)
        .await?;
    bundler_client
        .apply_estimation_fee_hints(&mut user_op)
        .await?;

    let draft_op_hash = bundler_client.user_op_hash(&user_op).await?;
    let draft_signature = wallet_registry.decrypt_and_sign(agent_wallet, draft_op_hash)?;
    user_op = bundler_client.apply_signature(user_op, draft_signature);

    match bundler_client.estimate_gas_for_user_op(&user_op).await {
        Ok((call_gas, verification_gas, pre_verification_gas)) => {
            let user_op_total_gas = call_gas
                .saturating_add(verification_gas)
                .saturating_add(pre_verification_gas);

            info!(
                call_gas = %call_gas,
                verification_gas = %verification_gas,
                pre_verification_gas = %pre_verification_gas,
                user_op_total_gas = %user_op_total_gas,
                "batch UserOperation simulation succeeded"
            );

            Ok(SimulationResult {
                // Preserve the public response's call-gas estimate convention.
                gas_estimate: call_gas.as_u64(),
                success: true,
                return_data: Some(format!(
                    "user_op_call_gas={call_gas};verification_gas={verification_gas};pre_verification_gas={pre_verification_gas};total_user_op_gas={user_op_total_gas}"
                )),
                error: None,
            })
        }
        Err(e) => {
            let mut error = format!("UserOperation simulation failed: {e:#}");
            if let Some(diagnostic) =
                diagnose_execute_batch_revert(bundler_client, req, smart_wallet).await?
            {
                error.push_str("; ");
                error.push_str(&diagnostic);
            }

            Ok(SimulationResult {
                success: false,
                gas_estimate: 0,
                return_data: None,
                error: Some(error),
            })
        }
    }
}

async fn diagnose_execute_batch_revert(
    bundler_client: &BundlerClient,
    req: &ExecutionRequest,
    smart_wallet: Address,
) -> Result<Option<String>> {
    let Some(batch_calls) = req.batch_calls.as_ref() else {
        return Ok(None);
    };

    let provider = bundler_client.provider();
    let code = provider.get_code(smart_wallet, None).await?;
    if code.0.is_empty() {
        return Ok(Some(
            "direct executeBatch diagnostic skipped: smart wallet is not deployed yet".to_string(),
        ));
    }

    let calldata = bundler_client.encode_execute_batch_call(batch_calls)?;
    let tx = TransactionRequest::new()
        .from(bundler_client.entry_point())
        .to(smart_wallet)
        .data(Bytes::from(calldata));

    match provider.call(&tx.into(), None).await {
        Ok(_) => Ok(Some(
            "direct executeBatch diagnostic succeeded; protocol batch calldata is valid, so the remaining failure is specific to UserOperation validation, paymaster validation, or bundler gas estimation".to_string(),
        )),
        Err(error) => {
            let error_text = format!("{error:#}");
            let revert_hex = extract_revert_hex(&error_text);
            let decoded = revert_hex
                .as_deref()
                .and_then(decode_execute_error)
                .or_else(|| decode_truncated_execute_error(revert_hex.as_deref()));
            let call_summary = summarize_batch_calls(batch_calls);

            Ok(Some(match (revert_hex, decoded) {
                (Some(raw), Some(decoded)) => {
                    format!("direct executeBatch diagnostic: {decoded}; raw_revert={raw}; {call_summary}")
                }
                (Some(raw), None) => {
                    format!("direct executeBatch diagnostic reverted; raw_revert={raw}; {call_summary}")
                }
                (None, _) => {
                    format!("direct executeBatch diagnostic reverted: {error_text}; {call_summary}")
                }
            }))
        }
    }
}

fn summarize_batch_calls(batch_calls: &[BatchCall]) -> String {
    let calls = batch_calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            let selector = call
                .calldata
                .trim_start_matches("0x")
                .get(..8)
                .map(|raw| format!("0x{raw}"))
                .unwrap_or_else(|| "0x".to_string());
            format!(
                "batch_calls[{index}] target={} selector={} value={}",
                call.target_contract, selector, call.value
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("batch_call_summary=[{calls}]")
}

fn extract_revert_hex(error_text: &str) -> Option<String> {
    error_text
        .split(|c: char| c == '"' || c == '\'' || c == ',' || c.is_whitespace())
        .filter_map(|part| {
            let raw = part.strip_prefix("0x")?;
            if raw.len() >= 8 && raw.len() % 2 == 0 && raw.chars().all(|c| c.is_ascii_hexdigit()) {
                Some(format!("0x{raw}"))
            } else {
                None
            }
        })
        .max_by_key(|value| value.len())
}

fn decode_execute_error(raw: &str) -> Option<String> {
    let bytes = hex::decode(raw.trim_start_matches("0x")).ok()?;
    if bytes.len() < 4 || bytes[..4] != [0x5a, 0x15, 0x46, 0x75] {
        return decode_known_revert_selector(&bytes).map(str::to_string);
    }

    let decoded = abi::decode(&[ParamType::Uint(256), ParamType::Bytes], bytes.get(4..)?).ok()?;
    let index = match &decoded[0] {
        Token::Uint(value) => value.to_string(),
        _ => return None,
    };
    let inner = match &decoded[1] {
        Token::Bytes(value) => value.as_slice(),
        _ => return None,
    };
    let inner_label = decode_known_revert_selector(inner).unwrap_or("unknown inner error");
    let inner_hex = format!("0x{}", hex::encode(inner));
    Some(format!(
        "smart account ExecuteError at batch_calls[{index}], inner={inner_label}, inner_revert={inner_hex}"
    ))
}

fn decode_truncated_execute_error(raw: Option<&str>) -> Option<String> {
    let bytes = hex::decode(raw?.trim_start_matches("0x")).ok()?;
    if bytes.len() >= 10
        && bytes[..4] == [0x5a, 0x15, 0x46, 0x75]
        && bytes
            .windows(4)
            .any(|window| window == [0xcc, 0x34, 0x59, 0xff])
    {
        return Some(
            "smart account ExecuteError with truncated inner=UnexpectedMarket()".to_string(),
        );
    }
    None
}

fn decode_known_revert_selector(bytes: &[u8]) -> Option<&'static str> {
    match bytes.get(..4)? {
        [0x5a, 0x15, 0x46, 0x75] => Some("ExecuteError(uint256,bytes)"),
        [0xcc, 0x34, 0x59, 0xff] => Some("UnexpectedMarket()"),
        _ => None,
    }
}

/// Resolve the smart wallet address that must be used as the ERC-4337 sender
/// on a specific chain.
///
/// Existing production wallets originally had one persisted smart-wallet
/// address, usually derived from the first configured chain.  On newer
/// deterministic multi-chain deployments, non-Ethereum chains must derive the
/// counterfactual sender from that chain's configured factory; otherwise the
/// UserOp `sender` can disagree with `initCode`, causing EntryPoint AA14.
pub async fn resolve_chain_smart_wallet_address(
    engine: &ExecutionEngine,
    chain: &Chain,
    agent_wallet: &crate::agent_wallet::AgentWallet,
) -> Result<Address> {
    if chain == &Chain::Ethereum {
        return Ok(agent_wallet.smart_wallet_address);
    }

    let chain_cfg = engine.config.chain_config(chain)?;
    let factory_address: Address = chain_cfg
        .factory_address
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid factory address for {chain}: {e}"))?;
    if factory_address == Address::zero() {
        anyhow::bail!("factory address is not configured for {chain}");
    }

    let provider = engine.provider_for_chain(chain)?;
    let resolved = AgentWalletRegistry::compute_smart_wallet_address_with(
        provider,
        factory_address,
        agent_wallet.eoa_address,
    )
    .await?;

    if resolved != agent_wallet.smart_wallet_address {
        info!(
            chain = %chain,
            agent_id = %agent_wallet.agent_id,
            stored_smart_wallet = %agent_wallet.smart_wallet_address,
            chain_smart_wallet = %resolved,
            "using chain-specific smart wallet address"
        );
    }

    Ok(resolved)
}

/// Handle a full execution request:
/// validate → resolve smart wallet → simulate → enqueue.
pub async fn handle_execute(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    req: &ExecutionRequest,
) -> Result<ExecutionResponse> {
    // 1. Validate
    let chain = engine.validate(req)?;

    // Resolve the bundler client for this chain
    let bundler_client = bundler_clients
        .get(&chain)
        .ok_or_else(|| anyhow::anyhow!("no bundler configured for chain {chain}"))?;
    let paymaster_signer = paymaster_signers.get(&chain).ok_or_else(|| {
        anyhow::anyhow!("testnet gas sponsorship is not configured for chain {chain}")
    })?;

    // Validate callback_url if provided
    let callback_url = validate_callback_url(req.callback_url.as_deref())?;

    // 2. Resolve agent's smart wallet (get or create)
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let smart_wallet_address =
        resolve_chain_smart_wallet_address(engine, &chain, &agent_wallet).await?;
    let smart_wallet_str = format!("{smart_wallet_address:?}");

    // 3. Persist initial request
    let db_row = db::insert_execution_request(
        pool,
        api_key_id,
        req,
        &ExecutionStatus::Pending,
        Some(&smart_wallet_str),
        callback_url.as_deref(),
    )
    .await?;
    let request_id = db_row.id;

    // 4. Simulate (using smart wallet as `from`)
    let sim = simulate_request(
        engine,
        bundler_client,
        Some(paymaster_signer),
        wallet_registry,
        req,
        &chain,
        &agent_wallet,
        smart_wallet_address,
        &smart_wallet_str,
    )
    .await?;
    if !sim.success {
        db::update_execution_status(
            pool,
            request_id,
            &ExecutionStatus::Failed,
            None,
            sim.error.as_deref(),
            None,
        )
        .await?;

        return Ok(ExecutionResponse {
            request_id,
            status: ExecutionStatus::Failed,
            smart_wallet_address: Some(smart_wallet_str.clone()),
            estimated_gas: None,
            tx_hash: None,
            message: format!("simulation failed: {}", sim.error.unwrap_or_default()),
        });
    }

    // Persist the simulation estimate before queueing. Testnet execution is free
    // to callers; the configured paymaster sponsors UserOperation gas.
    db::update_execution_status(
        pool,
        request_id,
        &ExecutionStatus::Pending,
        None,
        None,
        Some(sim.gas_estimate as i64),
    )
    .await?;

    // 5. Enqueue — the job now carries smart wallet + EOA for the worker
    //    to build a UserOperation.
    let gas_limit_with_buffer = sim.gas_estimate.saturating_mul(120) / 100;

    // Resolve API key hash for webhook HMAC signing (only if callback_url is set)
    let api_key_hash = if callback_url.is_some() {
        Some(
            db::get_api_key_hash_by_id(pool, api_key_id)
                .await
                .map_err(|error| anyhow::anyhow!("failed to load webhook signing secret: {error}"))?
                .ok_or_else(|| anyhow::anyhow!("authenticated API key is unavailable"))?,
        )
    } else {
        None
    };

    let job = ExecutionJob {
        request_id,
        agent_id: req.agent_id.clone(),
        smart_wallet_address: smart_wallet_str.clone(),
        eoa_address: format!("{:?}", agent_wallet.eoa_address),
        chain,
        target_contract: req.target_contract.clone(),
        calldata: req.calldata.clone(),
        value: req.value.clone(),
        gas_limit: gas_limit_with_buffer,
        created_at: Utc::now(),
        attempt_count: 0,
        batch_calls: req.batch_calls.clone(),
        callback_url,
        api_key_hash,
    };
    queue::enqueue_job(redis_conn, &job).await?;

    db::update_execution_status(pool, request_id, &ExecutionStatus::Queued, None, None, None)
        .await?;

    info!(
        request_id = %request_id,
        agent_id = %req.agent_id,
        has_callback = req.callback_url.is_some(),
        "execution request queued"
    );

    let message = if req.callback_url.is_some() {
        "execution queued — result will be POSTed to your callback URL".into()
    } else {
        "execution queued".into()
    };

    Ok(ExecutionResponse {
        request_id,
        status: ExecutionStatus::Queued,
        smart_wallet_address: Some(smart_wallet_str),
        estimated_gas: Some(sim.gas_estimate),
        tx_hash: None,
        message,
    })
}

async fn fetch_erc20_balance_of(
    provider: &ethers::providers::Provider<ethers::providers::Http>,
    token_contract: &str,
    wallet: Address,
) -> Result<U256> {
    let token_addr: Address = token_contract
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid token contract address {token_contract}: {e}"))?;

    let mut data = Vec::with_capacity(4 + 32);
    data.extend_from_slice(&[0x70, 0xa0, 0x82, 0x31]);
    let mut addr_slot = [0u8; 32];
    addr_slot[12..32].copy_from_slice(wallet.as_bytes());
    data.extend_from_slice(&addr_slot);

    let call = TransactionRequest::new()
        .to(token_addr)
        .data(Bytes::from(data));

    let raw = provider.call(&call.into(), None).await?;
    if raw.0.len() < 32 {
        anyhow::bail!("short balanceOf return data length {}", raw.0.len());
    }

    let mut amount_slot = [0u8; 32];
    amount_slot.copy_from_slice(&raw.0[raw.0.len() - 32..]);
    Ok(U256::from_big_endian(&amount_slot))
}

/// Resolve the agent's smart wallet and return ERC-20 + native token balances.
pub async fn handle_get_wallet_balance(
    engine: &ExecutionEngine,
    wallet_registry: &AgentWalletRegistry,
    api_key_id: Uuid,
    agent_id: &str,
    chain_str: &str,
) -> Result<WalletBalanceResponse> {
    let chain = Chain::from_str_loose(chain_str)
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {chain_str}"))?;

    let chain_cfg = engine
        .config
        .chains
        .get(&chain)
        .ok_or_else(|| anyhow::anyhow!("chain {chain_str} is not configured"))?
        .clone();

    let provider = engine.provider_for_chain(&chain)?;

    // Resolve (or provision) the agent's smart wallet
    let agent_wallet = wallet_registry.get_or_create(api_key_id, agent_id).await?;
    let smart_wallet_address =
        resolve_chain_smart_wallet_address(engine, &chain, &agent_wallet).await?;
    let smart_wallet_str = format!("{smart_wallet_address:?}");

    // ── Native balance ────────────────────────────────────────────────
    let native_wei: U256 = provider.get_balance(smart_wallet_address, None).await?;
    let native_balance_wei = native_wei.to_string();
    // Format as native token amount with 18 decimals (ETH on Ethereum/Base/Arbitrum).
    // Convert via low 128 bits only; typical
    // wallet balances are far below 2^128 wei so precision is not lost.
    let native_formatted = {
        let divisor = 1_000_000_000_000_000_000u128 as f64; // 1e18
        let val = native_wei.low_u128() as f64;
        format!("{:.6}", val / divisor)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    };

    // ── ERC-20 balances ───────────────────────────────────────────────
    // Reuse the existing fetch_erc20_balance_of helper (reads last 32 bytes
    // of the call response, handles short-return gracefully).
    let mut tokens = Vec::new();
    for (symbol, contract_addr_str) in &chain_cfg.tracked_tokens {
        let decimals = chain_cfg
            .tracked_token_decimals
            .get(symbol)
            .copied()
            .unwrap_or(6);

        let raw_balance =
            fetch_erc20_balance_of(provider.as_ref(), contract_addr_str, smart_wallet_address)
                .await
                .unwrap_or(U256::zero()); // graceful zero on any RPC error

        let formatted = {
            let divisor = 10u128.pow(decimals as u32) as f64;
            let val = raw_balance.low_u128() as f64;
            format!("{:.prec$}", val / divisor, prec = decimals as usize)
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string()
        };

        tokens.push(TokenBalance {
            symbol: symbol.clone(),
            contract_address: contract_addr_str.clone(),
            raw: raw_balance.to_string(),
            formatted,
            decimals,
        });
    }

    // Sort tokens alphabetically for stable output
    tokens.sort_by(|a, b| a.symbol.cmp(&b.symbol));

    Ok(WalletBalanceResponse {
        agent_id: agent_id.to_string(),
        smart_wallet_address: smart_wallet_str,
        chain: chain.to_string(),
        native_balance_wei,
        native_balance_formatted: native_formatted,
        tokens,
    })
}

/// Handle a simulation-only request (no queue).
pub async fn handle_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    req: &ExecutionRequest,
) -> Result<ExecutionResponse> {
    let chain = engine.validate(req)?;

    let bundler_client = bundler_clients
        .get(&chain)
        .ok_or_else(|| anyhow::anyhow!("no bundler configured for chain {chain}"))?;
    let paymaster_signer = paymaster_signers.get(&chain).ok_or_else(|| {
        anyhow::anyhow!("testnet gas sponsorship is not configured for chain {chain}")
    })?;

    // Resolve agent's smart wallet
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let smart_wallet_address =
        resolve_chain_smart_wallet_address(engine, &chain, &agent_wallet).await?;
    let smart_wallet_str = format!("{smart_wallet_address:?}");

    let db_row = db::insert_execution_request(
        pool,
        api_key_id,
        req,
        &ExecutionStatus::Pending,
        Some(&smart_wallet_str),
        None,
    )
    .await?;
    let request_id = db_row.id;

    let sim = simulate_request(
        engine,
        bundler_client,
        Some(paymaster_signer),
        wallet_registry,
        req,
        &chain,
        &agent_wallet,
        smart_wallet_address,
        &smart_wallet_str,
    )
    .await?;
    db::update_execution_status(
        pool,
        request_id,
        if sim.success {
            &ExecutionStatus::Pending
        } else {
            &ExecutionStatus::Failed
        },
        None,
        sim.error.as_deref(),
        Some(sim.gas_estimate as i64),
    )
    .await?;

    Ok(ExecutionResponse {
        request_id,
        status: if sim.success {
            ExecutionStatus::Pending
        } else {
            ExecutionStatus::Failed
        },
        smart_wallet_address: Some(smart_wallet_str),
        estimated_gas: Some(sim.gas_estimate),
        tx_hash: None,
        message: if sim.success {
            "simulation succeeded".into()
        } else {
            format!("simulation failed: {}", sim.error.unwrap_or_default())
        },
    })
}

/// Handle a wallet lookup — return the agent's smart wallet address.
///
/// This is a lightweight endpoint that lets agents discover their wallet
/// address so they can fund it with tokens before submitting execute requests.
pub async fn handle_get_wallet(
    engine: &ExecutionEngine,
    wallet_registry: &AgentWalletRegistry,
    api_key_id: Uuid,
    agent_id: &str,
    chain: &str,
) -> Result<crate::types::WalletResponse> {
    use crate::types::WalletResponse;

    // Validate chain
    let resolved_chain = crate::types::Chain::from_str_loose(chain)
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {chain}"))?;

    // Validate agent_id
    if agent_id.trim().is_empty() {
        return Err(anyhow::anyhow!("agent_id is required"));
    }
    if agent_id.len() > 256 {
        return Err(anyhow::anyhow!("agent_id too long (max 256 characters)"));
    }

    // Resolve or create the smart wallet
    let agent_wallet = wallet_registry.get_or_create(api_key_id, agent_id).await?;
    let smart_wallet_addr =
        resolve_chain_smart_wallet_address(engine, &resolved_chain, &agent_wallet).await?;

    // Check if the smart wallet is already deployed on-chain
    let provider = engine.provider_for_chain(&resolved_chain)?;
    let code: ethers::types::Bytes = provider
        .get_code(smart_wallet_addr, None)
        .await
        .unwrap_or_default();
    let deployed = !code.is_empty();

    let smart_wallet_str = format!("{smart_wallet_addr:?}");

    let message = if deployed {
        format!(
            "Wallet is deployed. Send any ERC-20 tokens or native currency to {smart_wallet_str} before executing transactions that spend them.",
        )
    } else {
        format!(
            "Wallet is not yet deployed (counterfactual). You can still safely send ERC-20 tokens and native currency to {smart_wallet_str} — \
             the address is deterministic via CREATE2. The wallet contract will be automatically deployed \
             on your first transaction. Tokens sent now will be fully accessible after deployment.",
        )
    };

    Ok(WalletResponse {
        agent_id: agent_id.to_string(),
        smart_wallet_address: smart_wallet_str,
        deployed,
        message,
    })
}

// ──────────────────────── Helpers ────────────────────────────────────

/// Validate an optional callback URL.
///
/// Rules:
///   - Must be `https://` (no plaintext HTTP — webhook payloads contain
///     sensitive execution data and HMAC signatures).
///   - Must be parseable as a URL.
///   - Must not contain credentials, fragments, localhost, or a non-public
///     literal IP address.
///   - Maximum length: 2048 characters.
///
/// Returns `Ok(None)` if the input is `None`.
fn validate_callback_url(url: Option<&str>) -> Result<Option<String>> {
    match url {
        None => Ok(None),
        Some(u) => {
            let trimmed = u.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.len() > 2048 {
                anyhow::bail!("callback_url too long (max 2048 characters)");
            }
            let parsed = reqwest::Url::parse(trimmed)
                .map_err(|_| anyhow::anyhow!("callback_url must be a valid absolute URL"))?;
            if parsed.scheme() != "https" {
                anyhow::bail!("callback_url must use HTTPS");
            }
            if !parsed.username().is_empty() || parsed.password().is_some() {
                anyhow::bail!("callback_url must not contain credentials");
            }
            if parsed.fragment().is_some() {
                anyhow::bail!("callback_url must not contain a fragment");
            }
            let host = parsed
                .host_str()
                .ok_or_else(|| anyhow::anyhow!("callback_url is missing a host"))?;
            if host.eq_ignore_ascii_case("localhost")
                || host.to_ascii_lowercase().ends_with(".localhost")
            {
                anyhow::bail!("callback_url must use a public host");
            }
            let ip_literal = host
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
                .unwrap_or(host);
            if ip_literal
                .parse::<IpAddr>()
                .is_ok_and(is_non_public_callback_ip)
            {
                anyhow::bail!("callback_url must use a public IP address");
            }
            Ok(Some(parsed.to_string()))
        }
    }
}

fn is_non_public_callback_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 0 && c == 2)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113)
                || a >= 224
        }
        IpAddr::V6(ip) => {
            if let Some(ipv4) = ip.to_ipv4() {
                return is_non_public_callback_ip(IpAddr::V4(ipv4));
            }
            let segments = ip.segments();
            ip.is_unspecified()
                || ip.is_loopback()
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80
                || (segments[0] & 0xff00) == 0xff00
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_batch_request(values: &[&str]) -> ExecutionRequest {
        ExecutionRequest {
            agent_id: "agent-1".to_string(),
            chain: "arbitrum".to_string(),
            target_contract: String::new(),
            calldata: String::new(),
            value: "0".to_string(),
            strategy_id: None,
            batch_calls: Some(
                values
                    .iter()
                    .map(|value| BatchCall {
                        target_contract: "0x1111111111111111111111111111111111111111".to_string(),
                        calldata: "0xabcdef12".to_string(),
                        value: (*value).to_string(),
                    })
                    .collect(),
            ),
            callback_url: None,
        }
    }

    #[test]
    fn batch_native_value_required_sums_batch_call_values() {
        let req = sample_batch_request(&["0", "100", " 200 "]);

        let required = batch_native_value_required(&req).expect("sum batch values");

        assert_eq!(required, U256::from(300u64));
    }

    #[test]
    fn callback_url_accepts_and_normalizes_public_https_url() {
        let url = validate_callback_url(Some("  https://hooks.example.com/events  "))
            .expect("valid callback URL");
        assert_eq!(url.as_deref(), Some("https://hooks.example.com/events"));
    }

    #[test]
    fn callback_url_rejects_unsafe_targets() {
        for url in [
            "http://hooks.example.com/events",
            "https://localhost/events",
            "https://service.localhost/events",
            "https://127.0.0.1/events",
            "https://10.0.0.1/events",
            "https://[::1]/events",
            "https://user:password@example.com/events",
            "https://hooks.example.com/events#secret",
            "not a URL",
        ] {
            assert!(
                validate_callback_url(Some(url)).is_err(),
                "unsafe callback URL was accepted: {url}"
            );
        }
    }

    #[test]
    fn batch_native_value_required_rejects_invalid_value() {
        let req = sample_batch_request(&["1", "not-a-number"]);

        let err = batch_native_value_required(&req).expect_err("invalid value");

        assert!(format!("{err:#}").contains("invalid batch call value"));
    }

    #[test]
    fn decode_execute_error_reports_batch_index_and_inner_selector() {
        let inner = hex::decode("cc3459ff").expect("inner selector");
        let encoded = abi::encode(&[Token::Uint(U256::from(1u64)), Token::Bytes(inner)]);
        let raw = format!("0x5a154675{}", hex::encode(encoded));

        let decoded = decode_execute_error(&raw).expect("decode execute error");

        assert!(decoded.contains("batch_calls[1]"));
        assert!(decoded.contains("inner=UnexpectedMarket()"));
        assert!(decoded.contains("inner_revert=0xcc3459ff"));
    }

    #[test]
    fn decode_truncated_execute_error_reports_unexpected_market() {
        let decoded = decode_truncated_execute_error(Some("0x5a154675014004cc3459ff"))
            .expect("decode truncated execute error");

        assert!(decoded.contains("UnexpectedMarket()"));
    }
}
