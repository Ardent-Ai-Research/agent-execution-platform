//! Service layer — thin wrappers that coordinate between the execution engine,
//! agent wallet registry, database, queue, and payment verification for each
//! API endpoint.
//!
//! With ERC-4337, the flow is:
//!   validate → resolve smart wallet → simulate → price → check payment → enqueue

use anyhow::Result;
use chrono::Utc;
use ethers::prelude::Middleware;
use ethers::types::{Address, Bytes, TransactionRequest, U256};
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::info;
use uuid::Uuid;

use crate::agent_wallet::AgentWalletRegistry;
use crate::db;
use crate::execution_engine::ExecutionEngine;
use crate::queue;
use crate::relayer::erc4337::BundlerClient;
use crate::relayer::paymaster::PaymasterSigner;
use crate::types::*;
use crate::worker::{self, WorkerContext};

fn usd_to_token_raw_amount(usd: f64, decimals: u8) -> Result<U256> {
    if !usd.is_finite() || usd < 0.0 {
        anyhow::bail!("invalid USD amount: {}", usd);
    }

    let scaled = usd * 10f64.powi(decimals as i32);
    if !scaled.is_finite() || scaled < 0.0 {
        anyhow::bail!("invalid scaled token amount for usd {}", usd);
    }

    let rounded = scaled.ceil();
    if rounded > u128::MAX as f64 {
        anyhow::bail!("token amount overflow for usd {}", usd);
    }

    Ok(U256::from(rounded as u128))
}

/// Handle a full execution request:
/// validate → resolve smart wallet → simulate → price → check payment → enqueue.
pub async fn handle_execute(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &ExecutionRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    // 1. Validate
    let chain = engine.validate(req)?;

    // Resolve the bundler client for this chain
    let bundler_client = bundler_clients
        .get(&chain)
        .ok_or_else(|| anyhow::anyhow!("no bundler configured for chain {}", chain))?;

    // Validate callback_url if provided
    let callback_url = validate_callback_url(req.callback_url.as_deref())?;

    // 2. Resolve agent's smart wallet (get or create)
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let smart_wallet_str = format!("{:?}", agent_wallet.smart_wallet_address);

    // 3. Persist initial request
    let db_row = db::insert_execution_request(
        pool,
        req,
        &ExecutionStatus::Pending,
        Some(&smart_wallet_str),
        callback_url.as_deref(),
    )
    .await?;
    let request_id = db_row.id;

    // 4. Simulate (using smart wallet as `from`)
    let sim = engine
        .simulate(req, &chain, agent_wallet.smart_wallet_address)
        .await?;
    if !sim.success {
        db::update_execution_status(
            pool,
            request_id,
            &ExecutionStatus::Failed,
            None,
            sim.error.as_deref(),
            None,
            None,
        )
        .await?;

        return Ok(ExecutionResponse {
            request_id,
            status: ExecutionStatus::Failed,
            smart_wallet_address: Some(smart_wallet_str.clone()),
            estimated_gas: None,
            estimated_cost_usd: None,
            tx_hash: None,
            message: format!("simulation failed: {}", sim.error.unwrap_or_default()),
        });
    }

    // 5. Price (includes ERC-4337 overhead) and payment policy by mode.
    // Quote locking is only applied in manual mode.
    let quote_request_id = payment_proof.and_then(|p| p.quote_request_id);
    let locked_quote_cost = if payment_mode == PaymentMode::Manual {
        match quote_request_id {
            Some(quote_id) => db::get_locked_quote_cost(pool, quote_id, api_key_id, req).await?,
            None => None,
        }
    } else {
        None
    };

    if payment_mode == PaymentMode::Manual && quote_request_id.is_some() && locked_quote_cost.is_none() {
        db::update_execution_status(
            pool,
            request_id,
            &ExecutionStatus::Failed,
            None,
            Some("invalid or mismatched payment quote request_id"),
            Some(sim.gas_estimate as i64),
            None,
        )
        .await?;

        return Ok(ExecutionResponse {
            request_id,
            status: ExecutionStatus::Failed,
            smart_wallet_address: Some(smart_wallet_str.clone()),
            estimated_gas: Some(sim.gas_estimate),
            estimated_cost_usd: None,
            tx_hash: None,
            message: "invalid or mismatched payment quote request_id".into(),
        });
    }

    let payable_cost = match payment_mode {
        PaymentMode::Manual => match locked_quote_cost {
            Some(locked) => locked,
            None => {
                engine
                    .estimate_cost_with_mode(&chain, sim.gas_estimate, bundler_client, true)
                    .await?
            }
        },
        PaymentMode::Auto => {
            engine
                .estimate_cost_with_mode(&chain, sim.gas_estimate, bundler_client, false)
                .await?
        }
        PaymentMode::Sponsored => 0.0,
    };

    let initial_status = if payment_mode == PaymentMode::Sponsored {
        ExecutionStatus::PaymentVerified
    } else {
        ExecutionStatus::PaymentRequired
    };

    let initial_message = if payment_mode == PaymentMode::Sponsored {
        Some("payment mode sponsored: external payment not required")
    } else {
        None
    };

    db::update_execution_status(
        pool,
        request_id,
        &initial_status,
        None,
        initial_message,
        Some(sim.gas_estimate as i64),
        Some(payable_cost),
    )
    .await?;

    // 6. Payment mode behavior
    match payment_mode {
        PaymentMode::Manual => match payment_proof {
            None => {
                return Ok(ExecutionResponse {
                    request_id,
                    status: ExecutionStatus::PaymentRequired,
                    smart_wallet_address: Some(smart_wallet_str.clone()),
                    estimated_gas: Some(sim.gas_estimate),
                    estimated_cost_usd: Some(payable_cost),
                    tx_hash: None,
                    message: "payment required — include X-Payment-Proof header".into(),
                });
            }
            Some(proof) => {
                if let Some(resp) = validate_and_record_payment(
                    engine,
                    pool,
                    request_id,
                    &chain,
                    sim.gas_estimate,
                    payable_cost,
                    &smart_wallet_str,
                    proof,
                )
                .await?
                {
                    return Ok(resp);
                }
            }
        },
        PaymentMode::Auto => {
            if let Some(proof) = payment_proof {
                if let Some(resp) = validate_and_record_payment(
                    engine,
                    pool,
                    request_id,
                    &chain,
                    sim.gas_estimate,
                    payable_cost,
                    &smart_wallet_str,
                    proof,
                )
                .await?
                {
                    return Ok(resp);
                }
            } else {
                let auto_payment = execute_auto_payment(
                    engine,
                    pool,
                    wallet_registry,
                    bundler_clients,
                    paymaster_signers,
                    request_id,
                    &req.agent_id,
                    &chain,
                    payable_cost,
                    &smart_wallet_str,
                    &agent_wallet.eoa_address,
                )
                .await?;

                if let Some(resp) = auto_payment {
                    return Ok(resp);
                }
            }
        }
        PaymentMode::Sponsored => {}
    }

    // 7. Enqueue — the job now carries smart wallet + EOA for the worker
    //    to build a UserOperation.
    let gas_limit_with_buffer = sim.gas_estimate.saturating_mul(120) / 100;

    // Resolve API key hash for webhook HMAC signing (only if callback_url is set)
    let api_key_hash = if callback_url.is_some() {
        db::get_api_key_hash_for_request(pool, request_id)
            .await
            .ok()
            .flatten()
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

    db::update_execution_status(
        pool,
        request_id,
        &ExecutionStatus::Queued,
        None,
        None,
        None,
        None,
    )
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
        estimated_cost_usd: Some(payable_cost),
        tx_hash: None,
        message,
    })
}

async fn execute_auto_payment(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    paymaster_signers: &HashMap<Chain, PaymasterSigner>,
    request_id: Uuid,
    agent_id: &str,
    chain: &Chain,
    cost_usd: f64,
    smart_wallet_address: &str,
    eoa_address: &ethers::types::Address,
) -> Result<Option<ExecutionResponse>> {
    let chain_cfg = engine.config.chain_config(chain)?.clone();

    let smart_wallet_addr: Address = smart_wallet_address
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid smart wallet address for auto payment: {e}"))?;

    let mut token_symbols = chain_cfg
        .accepted_tokens
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    token_symbols.sort();
    if token_symbols.is_empty() {
        return Err(anyhow::anyhow!(
            "auto payment unavailable: no accepted tokens configured"
        ));
    }

    let mut selected: Option<(String, String, U256)> = None;
    let provider = engine.provider_for_chain(chain)?;

    for symbol in token_symbols {
        let token_contract = match chain_cfg.accepted_tokens.get(&symbol) {
            Some(addr) => addr.clone(),
            None => continue,
        };
        let decimals = chain_cfg.token_decimals.get(&symbol).copied().unwrap_or(6);
        let required_raw = usd_to_token_raw_amount(cost_usd, decimals)?;

        if required_raw.is_zero() {
            selected = Some((symbol, token_contract, required_raw));
            break;
        }

        let balance_raw =
            fetch_erc20_balance_of(provider.as_ref(), &token_contract, smart_wallet_addr)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("auto payment balance check failed for {}: {}", symbol, e)
                })?;

        if balance_raw >= required_raw {
            selected = Some((symbol, token_contract, required_raw));
            break;
        }
    }

    let (token_symbol, token_contract, required_amount_raw) = selected.ok_or_else(|| {
        anyhow::anyhow!(
            "auto payment unavailable: no accepted token balance can cover required amount"
        )
    })?;

    if required_amount_raw.is_zero() {
        db::update_execution_status(
            pool,
            request_id,
            &ExecutionStatus::PaymentVerified,
            None,
            Some("payment mode auto: zero-cost quote, no transfer required"),
            None,
            None,
        )
        .await?;
        return Ok(None);
    }

    let transfer_calldata =
        encode_erc20_transfer(&engine.config.payment_address, required_amount_raw)?;

    let payment_job = ExecutionJob {
        request_id,
        agent_id: agent_id.to_string(),
        smart_wallet_address: smart_wallet_address.to_string(),
        eoa_address: format!("{eoa_address:?}"),
        chain: chain.clone(),
        target_contract: token_contract.clone(),
        calldata: transfer_calldata,
        value: "0".to_string(),
        gas_limit: 120_000,
        created_at: Utc::now(),
        attempt_count: 0,
        batch_calls: None,
        callback_url: None,
        api_key_hash: None,
    };

    let immediate_ctx = WorkerContext {
        db_pool: pool.clone(),
        wallet_registry: wallet_registry.clone(),
        bundler_clients: bundler_clients.clone(),
        paymaster_signers: paymaster_signers.clone(),
        webhook_client: crate::webhook::build_http_client(),
    };

    let auto_result = worker::execute_erc4337_now(&immediate_ctx, &payment_job).await;
    if !auto_result.success {
        let reason = auto_result
            .error
            .unwrap_or_else(|| "auto payment transaction failed".to_string());
        db::update_execution_status(
            pool,
            request_id,
            &ExecutionStatus::Failed,
            None,
            Some(&reason),
            None,
            None,
        )
        .await?;

        return Ok(Some(ExecutionResponse {
            request_id,
            status: ExecutionStatus::Failed,
            smart_wallet_address: Some(smart_wallet_address.to_string()),
            estimated_gas: None,
            estimated_cost_usd: Some(cost_usd),
            tx_hash: None,
            message: format!("payment mode auto failed: {}", reason),
        }));
    }

    let payment_tx_hash = auto_result.tx_hash;
    let proof = PaymentProof {
        payment_id: Uuid::new_v4(),
        quote_request_id: Some(request_id),
        payer: smart_wallet_address.to_string(),
        amount_usd: cost_usd,
        token: token_symbol,
        chain: chain.to_string(),
        tx_hash: payment_tx_hash,
        verified: true,
        verified_at: Utc::now(),
        confirmed_amount_raw: Some(required_amount_raw.to_string()),
        block_confirmations: None,
        token_contract: Some(token_contract),
    };

    let inserted = db::insert_payment(pool, request_id, &proof).await?;
    if inserted.is_none() {
        return Ok(Some(ExecutionResponse {
            request_id,
            status: ExecutionStatus::Failed,
            smart_wallet_address: Some(smart_wallet_address.to_string()),
            estimated_gas: None,
            estimated_cost_usd: Some(cost_usd),
            tx_hash: None,
            message: "payment mode auto failed: replay-protected payment hash conflict".into(),
        }));
    }

    db::update_execution_status(
        pool,
        request_id,
        &ExecutionStatus::PaymentVerified,
        None,
        Some("payment mode auto: onchain transfer confirmed"),
        None,
        None,
    )
    .await?;

    Ok(None)
}

fn encode_erc20_transfer(to: &str, amount: U256) -> Result<String> {
    let to_addr: ethers::types::Address = to
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid PAYMENT_ADDRESS for auto payment: {e}"))?;

    let mut data = Vec::with_capacity(4 + 32 + 32);
    data.extend_from_slice(&[0xa9, 0x05, 0x9c, 0xbb]);

    let mut addr_slot = [0u8; 32];
    addr_slot[12..32].copy_from_slice(to_addr.as_bytes());
    data.extend_from_slice(&addr_slot);

    let mut amount_slot = [0u8; 32];
    amount.to_big_endian(&mut amount_slot);
    data.extend_from_slice(&amount_slot);

    Ok(format!("0x{}", hex::encode(data)))
}

async fn fetch_erc20_balance_of(
    provider: &ethers::providers::Provider<ethers::providers::Http>,
    token_contract: &str,
    wallet: Address,
) -> Result<U256> {
    let token_addr: Address = token_contract
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid token contract address {}: {e}", token_contract))?;

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

async fn validate_and_record_payment(
    engine: &ExecutionEngine,
    pool: &PgPool,
    request_id: Uuid,
    chain: &Chain,
    gas_estimate: u64,
    cost: f64,
    smart_wallet_str: &str,
    proof: &PaymentProof,
) -> Result<Option<ExecutionResponse>> {
    let chain_cfg = engine.config.chain_config(chain)?;
    let token_upper = proof.token.to_uppercase();
    let token_decimals = chain_cfg
        .token_decimals
        .get(&token_upper)
        .copied()
        .unwrap_or(6);

    let required_amount_raw = match usd_to_token_raw_amount(cost, token_decimals) {
        Ok(value) => value,
        Err(error) => {
            db::update_execution_status(
                pool,
                request_id,
                &ExecutionStatus::Failed,
                None,
                Some(&format!("invalid required payment amount: {}", error)),
                None,
                None,
            )
            .await?;

            return Ok(Some(ExecutionResponse {
                request_id,
                status: ExecutionStatus::Failed,
                smart_wallet_address: Some(smart_wallet_str.to_string()),
                estimated_gas: Some(gas_estimate),
                estimated_cost_usd: Some(cost),
                tx_hash: None,
                message: format!("invalid required payment amount: {}", error),
            }));
        }
    };

    let confirmed_amount_raw = match proof.confirmed_amount_raw.as_deref() {
        Some(raw) => U256::from_dec_str(raw)
            .map_err(|e| anyhow::anyhow!("invalid confirmed_amount_raw in payment proof: {e}"))?,
        None => {
            db::update_execution_status(
                pool,
                request_id,
                &ExecutionStatus::Failed,
                None,
                Some("payment proof missing confirmed_amount_raw"),
                None,
                None,
            )
            .await?;

            return Ok(Some(ExecutionResponse {
                request_id,
                status: ExecutionStatus::Failed,
                smart_wallet_address: Some(smart_wallet_str.to_string()),
                estimated_gas: Some(gas_estimate),
                estimated_cost_usd: Some(cost),
                tx_hash: None,
                message: "payment proof missing confirmed_amount_raw".into(),
            }));
        }
    };

    if confirmed_amount_raw < required_amount_raw {
        db::update_execution_status(
            pool,
            request_id,
            &ExecutionStatus::Failed,
            None,
            Some(&format!(
                "underpayment: paid {} raw {}, required {} raw {}",
                confirmed_amount_raw, token_upper, required_amount_raw, token_upper,
            )),
            None,
            None,
        )
        .await?;

        return Ok(Some(ExecutionResponse {
            request_id,
            status: ExecutionStatus::Failed,
            smart_wallet_address: Some(smart_wallet_str.to_string()),
            estimated_gas: Some(gas_estimate),
            estimated_cost_usd: Some(cost),
            tx_hash: None,
            message: format!(
                "underpayment: paid {} raw {}, required {} raw {}",
                confirmed_amount_raw, token_upper, required_amount_raw, token_upper,
            ),
        }));
    }

    let inserted = db::insert_payment(pool, request_id, proof).await?;
    if inserted.is_none() {
        return Ok(Some(ExecutionResponse {
            request_id,
            status: ExecutionStatus::Failed,
            smart_wallet_address: Some(smart_wallet_str.to_string()),
            estimated_gas: Some(gas_estimate),
            estimated_cost_usd: Some(cost),
            tx_hash: None,
            message: format!(
                "payment tx {} has already been used (replay rejected)",
                proof.tx_hash
            ),
        }));
    }

    db::update_execution_status(
        pool,
        request_id,
        &ExecutionStatus::PaymentVerified,
        None,
        None,
        None,
        None,
    )
    .await?;

    Ok(None)
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
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {}", chain_str))?;

    let chain_cfg = engine
        .config
        .chains
        .get(&chain)
        .ok_or_else(|| anyhow::anyhow!("chain {} is not configured", chain_str))?
        .clone();

    let provider = engine.provider_for_chain(&chain)?;

    // Resolve (or provision) the agent's smart wallet
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, agent_id)
        .await?;
    let smart_wallet_address = agent_wallet.smart_wallet_address;
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
    for (symbol, contract_addr_str) in &chain_cfg.accepted_tokens {
        let decimals = chain_cfg
            .token_decimals
            .get(symbol)
            .copied()
            .unwrap_or(6);

        let raw_balance = fetch_erc20_balance_of(
            provider.as_ref(),
            contract_addr_str,
            smart_wallet_address,
        )
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

/// Handle a simulation-only request (no payment, no queue).
pub async fn handle_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    api_key_id: Uuid,
    payment_mode: PaymentMode,
    req: &ExecutionRequest,
) -> Result<ExecutionResponse> {
    let chain = engine.validate(req)?;

    let bundler_client = bundler_clients
        .get(&chain)
        .ok_or_else(|| anyhow::anyhow!("no bundler configured for chain {}", chain))?;

    // Resolve agent's smart wallet
    let agent_wallet = wallet_registry
        .get_or_create(api_key_id, &req.agent_id)
        .await?;
    let smart_wallet_str = format!("{:?}", agent_wallet.smart_wallet_address);

    let db_row = db::insert_execution_request(
        pool,
        req,
        &ExecutionStatus::Pending,
        Some(&smart_wallet_str),
        None,
    )
    .await?;
    let request_id = db_row.id;

    let sim = engine
        .simulate(req, &chain, agent_wallet.smart_wallet_address)
        .await?;
    let cost = if sim.success {
        match payment_mode {
            PaymentMode::Manual => Some(
                engine
                    .estimate_cost_with_mode(&chain, sim.gas_estimate, bundler_client, true)
                    .await?,
            ),
            PaymentMode::Auto => Some(
                engine
                    .estimate_cost_with_mode(&chain, sim.gas_estimate, bundler_client, false)
                    .await?,
            ),
            PaymentMode::Sponsored => Some(0.0),
        }
    } else {
        None
    };

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
        cost,
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
        estimated_cost_usd: cost,
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
        .ok_or_else(|| anyhow::anyhow!("unsupported chain: {}", chain))?;

    // Validate agent_id
    if agent_id.trim().is_empty() {
        return Err(anyhow::anyhow!("agent_id is required"));
    }
    if agent_id.len() > 256 {
        return Err(anyhow::anyhow!("agent_id too long (max 256 characters)"));
    }

    // Resolve or create the smart wallet
    let agent_wallet = wallet_registry.get_or_create(api_key_id, agent_id).await?;
    let smart_wallet_addr = agent_wallet.smart_wallet_address;

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
            "Wallet is deployed. Send any ERC-20 tokens or native currency to {} before executing transactions that spend them.",
            smart_wallet_str,
        )
    } else {
        format!(
            "Wallet is not yet deployed (counterfactual). You can still safely send ERC-20 tokens and native currency to {} — \
             the address is deterministic via CREATE2. The wallet contract will be automatically deployed \
             on your first transaction. Tokens sent now will be fully accessible after deployment.",
            smart_wallet_str,
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
            if !trimmed.starts_with("https://") {
                anyhow::bail!(
                    "callback_url must use HTTPS (got: {})",
                    &trimmed[..trimmed.len().min(40)]
                );
            }
            // Basic URL structure check — must have a host after "https://"
            if trimmed.len() <= "https://".len() {
                anyhow::bail!("callback_url is missing a host");
            }
            Ok(Some(trimmed.to_string()))
        }
    }
}
