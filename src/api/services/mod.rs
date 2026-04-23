//! Service layer — thin wrappers that coordinate between the execution engine,
//! agent wallet registry, database, queue, and payment verification for each
//! API endpoint.
//!
//! With ERC-4337, the flow is:
//!   validate → resolve smart wallet → simulate → price → check payment → enqueue

use anyhow::Result;
use chrono::Utc;
use ethers::prelude::Middleware;
use ethers::types::U256;
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
use crate::types::*;

/// Handle a full execution request:
/// validate → resolve smart wallet → simulate → price → check payment → enqueue.
pub async fn handle_execute(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    api_key_id: Uuid,
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
    let agent_wallet = wallet_registry.get_or_create(api_key_id, &req.agent_id).await?;
    let smart_wallet_str = format!("{:?}", agent_wallet.smart_wallet_address);

    // 3. Persist initial request
    let db_row = db::insert_execution_request(
        pool, req, &ExecutionStatus::Pending, Some(&smart_wallet_str),
        callback_url.as_deref(),
    ).await?;
    let request_id = db_row.id;

    // 4. Simulate (using smart wallet as `from`)
    let sim = engine.simulate(req, &chain, agent_wallet.smart_wallet_address).await?;
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

    // 5. Price (includes ERC-4337 overhead), unless a locked quote is provided.
    let locked_quote_cost = match payment_proof.and_then(|p| p.quote_request_id) {
        Some(quote_request_id) => {
            db::get_locked_quote_cost(pool, quote_request_id, api_key_id, req).await?
        }
        None => None,
    };

    if payment_proof.and_then(|p| p.quote_request_id).is_some() && locked_quote_cost.is_none() {
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

    let cost = match locked_quote_cost {
        Some(locked) => locked,
        None => engine.estimate_cost(&chain, sim.gas_estimate, bundler_client).await?,
    };
    db::update_execution_status(
        pool,
        request_id,
        &ExecutionStatus::PaymentRequired,
        None,
        None,
        Some(sim.gas_estimate as i64),
        Some(cost),
    )
    .await?;

    // 6. Check payment
    match payment_proof {
        None => {
            return Ok(ExecutionResponse {
                request_id,
                status: ExecutionStatus::PaymentRequired,
                smart_wallet_address: Some(smart_wallet_str.clone()),
                estimated_gas: Some(sim.gas_estimate),
                estimated_cost_usd: Some(cost),
                tx_hash: None,
                message: "payment required — include X-Payment-Proof header".into(),
            });
        }
        Some(proof) => {
            // Server-side amount cross-check in raw token units.
            // Source of truth is on-chain transferred amount (`confirmed_amount_raw`),
            // not floating-point USD values from headers.
            let chain_cfg = engine.config.chain_config(&chain)?;
            let token_upper = proof.token.to_uppercase();
            let token_decimals = chain_cfg
                .token_decimals
                .get(&token_upper)
                .copied()
                .unwrap_or(6);

            let required_amount_raw = U256::from((cost * 10f64.powi(token_decimals as i32)) as u128);

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

                    return Ok(ExecutionResponse {
                        request_id,
                        status: ExecutionStatus::Failed,
                        smart_wallet_address: Some(smart_wallet_str.clone()),
                        estimated_gas: Some(sim.gas_estimate),
                        estimated_cost_usd: Some(cost),
                        tx_hash: None,
                        message: "payment proof missing confirmed_amount_raw".into(),
                    });
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
                        confirmed_amount_raw,
                        token_upper,
                        required_amount_raw,
                        token_upper,
                    )),
                    None,
                    None,
                )
                .await?;

                return Ok(ExecutionResponse {
                    request_id,
                    status: ExecutionStatus::Failed,
                    smart_wallet_address: Some(smart_wallet_str.clone()),
                    estimated_gas: Some(sim.gas_estimate),
                    estimated_cost_usd: Some(cost),
                    tx_hash: None,
                    message: format!(
                        "underpayment: paid {} raw {}, required {} raw {}",
                        confirmed_amount_raw,
                        token_upper,
                        required_amount_raw,
                        token_upper,
                    ),
                });
            }

            // Atomically record payment (replay protection via UNIQUE constraint)
            let inserted = db::insert_payment(pool, request_id, proof).await?;
            if inserted.is_none() {
                return Ok(ExecutionResponse {
                    request_id,
                    status: ExecutionStatus::Failed,
                    smart_wallet_address: Some(smart_wallet_str.clone()),
                    estimated_gas: Some(sim.gas_estimate),
                    estimated_cost_usd: Some(cost),
                    tx_hash: None,
                    message: format!(
                        "payment tx {} has already been used (replay rejected)",
                        proof.tx_hash
                    ),
                });
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
        }
    }

    // 7. Enqueue — the job now carries smart wallet + EOA for the worker
    //    to build a UserOperation.
    let gas_limit_with_buffer = sim.gas_estimate.saturating_mul(120) / 100;

    // Resolve API key hash for webhook HMAC signing (only if callback_url is set)
    let api_key_hash = if callback_url.is_some() {
        db::get_api_key_hash_for_request(pool, request_id).await.ok().flatten()
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
        estimated_cost_usd: Some(cost),
        tx_hash: None,
        message,
    })
}

/// Handle a simulation-only request (no payment, no queue).
pub async fn handle_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    wallet_registry: &AgentWalletRegistry,
    bundler_clients: &HashMap<Chain, BundlerClient>,
    api_key_id: Uuid,
    req: &ExecutionRequest,
) -> Result<ExecutionResponse> {
    let chain = engine.validate(req)?;

    let bundler_client = bundler_clients
        .get(&chain)
        .ok_or_else(|| anyhow::anyhow!("no bundler configured for chain {}", chain))?;

    // Resolve agent's smart wallet
    let agent_wallet = wallet_registry.get_or_create(api_key_id, &req.agent_id).await?;
    let smart_wallet_str = format!("{:?}", agent_wallet.smart_wallet_address);

    let db_row = db::insert_execution_request(
        pool, req, &ExecutionStatus::Pending, Some(&smart_wallet_str), None,
    ).await?;
    let request_id = db_row.id;

    let sim = engine.simulate(req, &chain, agent_wallet.smart_wallet_address).await?;
    let cost = if sim.success {
        Some(engine.estimate_cost(&chain, sim.gas_estimate, bundler_client).await?)
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
    let code: ethers::types::Bytes = provider.get_code(smart_wallet_addr, None).await.unwrap_or_default();
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
