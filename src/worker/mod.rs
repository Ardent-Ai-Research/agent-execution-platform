//! Background worker that consumes jobs from the Redis queue, builds and
//! submits ERC-4337 UserOperations via the bundler, and updates the database.
//!
//! Reliability guarantees:
//!
//! * **No silent job loss**: Jobs live in a per-worker processing list until
//!   explicitly acknowledged.  Crash → stale-job reaper pushes them back.
//! * **Poison-pill protection**: After [`MAX_JOB_ATTEMPTS`] failures the job is
//!   moved to a dead-letter queue instead of looping forever.
//! * **Panic safety**: The execution call is wrapped in [`tokio::task::spawn`]
//!   so a panic in one job doesn't kill the worker loop.
//! * **Single status write**: Queued → Broadcasting.

use redis::aio::ConnectionManager;
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::{error, info, warn};

use crate::agent_wallet::AgentWalletRegistry;
use crate::db;
use crate::execution_engine::simulation;
use crate::queue::{self, MAX_JOB_ATTEMPTS};
use crate::relayer::erc4337::BundlerClient;
use crate::relayer::paymaster::PaymasterSigner;
use crate::types::{Chain, ExecutionJob, ExecutionStatus, RelayerResult};
use crate::webhook;

use ethers::prelude::Middleware;

// ──────────────────────── Worker Context ─────────────────────────────

/// Everything a background worker needs to process jobs.
///
/// Created once at startup and cloned into each worker task.
#[derive(Clone)]
pub struct WorkerContext {
    pub db_pool: PgPool,
    /// Agent wallet registry (for loading signing keys).
    pub wallet_registry: AgentWalletRegistry,
    /// Per-chain ERC-4337 bundler clients.
    pub bundler_clients: HashMap<Chain, BundlerClient>,
    /// Per-chain paymaster signers (same signing key, different paymaster
    /// contract addresses).  If empty, gas sponsorship is disabled.
    pub paymaster_signers: HashMap<Chain, PaymasterSigner>,
    /// Shared HTTP client for webhook delivery (connection pooling).
    pub webhook_client: reqwest::Client,
}

// ──────────────────────── Worker loop ────────────────────────────────

/// Spawn a worker loop.  This function runs indefinitely (designed to be
/// `tokio::spawn`'d).
pub async fn run_worker(
    mut redis_conn: ConnectionManager,
    ctx: WorkerContext,
    worker_id: u32,
) {
    info!(worker_id, "worker started, waiting for jobs");

    loop {
        // Block-wait for up to 5 seconds.  On dequeue the job is atomically
        // moved into this worker's processing list (BRPOPLPUSH).
        let job = match queue::dequeue_job(&mut redis_conn, 5.0, worker_id).await {
            Ok(Some(job)) => job,
            Ok(None) => continue, // timeout, loop again
            Err(e) => {
                error!(worker_id, error = %e, "failed to dequeue job");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
        };

        let request_id = job.request_id;

        // ── Poison-pill guard ───────────────────────────────────────
        if job.attempt_count >= MAX_JOB_ATTEMPTS {
            error!(
                worker_id,
                request_id = %request_id,
                attempts = job.attempt_count,
                "job exceeded max attempts — moving to dead-letter queue"
            );
            let _ = db::update_execution_status(
                &ctx.db_pool,
                request_id,
                &ExecutionStatus::Failed,
                None,
                Some(&format!("exceeded {} execution attempts", MAX_JOB_ATTEMPTS)),
                None,
                None,
            )
            .await;

            // Webhook notification for DLQ'd jobs
            fire_webhook(
                &ctx,
                &job,
                &ExecutionStatus::Failed,
                None,
                None,
                Some(&format!("exceeded {} execution attempts", MAX_JOB_ATTEMPTS)),
            )
            .await;

            if let Err(e) = queue::move_to_dlq(&mut redis_conn, &job, worker_id).await {
                error!(request_id = %request_id, error = %e, "failed to move job to DLQ");
            }
            continue;
        }

        info!(
            worker_id,
            request_id = %request_id,
            attempt = job.attempt_count + 1,
            "processing job"
        );

        // ── Mark as Broadcasting ────────────────────────────────────
        if let Err(e) = db::update_execution_status(
            &ctx.db_pool,
            request_id,
            &ExecutionStatus::Broadcasting,
            None,
            None,
            None,
            None,
        )
        .await
        {
            error!(request_id = %request_id, error = %e, "failed to update status to broadcasting");
        }

        // ── Execute via ERC-4337 bundler ─────────────────────────────
        let ctx_clone = ctx.clone();
        let job_clone = job.clone();
        let handle = tokio::spawn(async move {
            execute_erc4337(&ctx_clone, &job_clone).await
        });

        let result = match handle.await {
            Ok(r) => r,
            Err(join_err) => {
                // The task panicked or was cancelled
                error!(
                    worker_id,
                    request_id = %request_id,
                    error = %join_err,
                    "PANIC in execution — re-enqueuing job"
                );
                re_enqueue_with_bump(&mut redis_conn, &ctx, &job, worker_id).await;
                continue;
            }
        };

        if result.success {
            info!(
                worker_id,
                request_id = %request_id,
                tx_hash = %result.tx_hash,
                block_number = ?result.block_number,
                gas_used = ?result.gas_used,
                "execution confirmed on-chain ✓"
            );

            if let Err(e) = db::update_execution_status(
                &ctx.db_pool,
                request_id,
                &ExecutionStatus::Confirmed,
                Some(&result.tx_hash),
                None,
                result.block_number.map(|b| b as i64),
                result.gas_used.map(|g| g as f64),
            )
            .await
            {
                error!(
                    request_id = %request_id,
                    error = %e,
                    "CRITICAL: failed to update status to confirmed — request stuck in broadcasting"
                );
            }

            // Insert transaction record — smart wallet is always the sender
            let from_address = job.smart_wallet_address.clone();
            if let Err(e) = db::insert_transaction(
                &ctx.db_pool,
                request_id,
                &job.chain.to_string(),
                &result.tx_hash,
                &from_address,
                &job.target_contract,
                "confirmed",
            )
            .await
            {
                warn!(
                    request_id = %request_id,
                    error = %e,
                    "failed to insert transaction record"
                );
            }

            // ── Webhook notification ────────────────────────────────
            fire_webhook(
                &ctx,
                &job,
                &ExecutionStatus::Confirmed,
                Some(&result.tx_hash),
                result.gas_used.map(|g| g as f64),
                None,
            )
            .await;

            // ── Acknowledge: remove from processing list ────────────
            if let Err(e) = queue::ack_job(&mut redis_conn, &job, worker_id).await {
                error!(request_id = %request_id, error = %e, "failed to ack job");
            }
        } else {
            let err_msg = result.error.as_deref().unwrap_or("unknown error");
            let is_revert = err_msg.contains("reverted on-chain")
                || err_msg.contains("UserOp reverted");

            error!(
                worker_id,
                request_id = %request_id,
                error = err_msg,
                is_revert,
                "execution failed"
            );

            if is_revert {
                // On-chain revert — retrying would revert again. Mark as terminal.
                let status = &ExecutionStatus::Reverted;
                if let Err(e) = db::update_execution_status(
                    &ctx.db_pool,
                    request_id,
                    status,
                    if result.tx_hash.is_empty() { None } else { Some(&result.tx_hash) },
                    Some(err_msg),
                    None,
                    None,
                )
                .await
                {
                    error!(
                        request_id = %request_id,
                        error = %e,
                        "CRITICAL: failed to update status to reverted"
                    );
                }

                // ── Webhook notification ────────────────────────────
                fire_webhook(
                    &ctx,
                    &job,
                    &ExecutionStatus::Reverted,
                    if result.tx_hash.is_empty() { None } else { Some(&result.tx_hash) },
                    None,
                    Some(err_msg),
                )
                .await;

                if let Err(e) = queue::ack_job(&mut redis_conn, &job, worker_id).await {
                    error!(request_id = %request_id, error = %e, "failed to ack reverted job");
                }
            } else {
                // Transient failure — re-enqueue for retry.
                re_enqueue_with_bump(&mut redis_conn, &ctx, &job, worker_id).await;
            }
        }
    }
}

// ──────────────────────── ERC-4337 Execution ─────────────────────────

/// Execute a job through the ERC-4337 Account Abstraction path:
///   1. Load agent's signing key from the wallet registry
///   2. Build a UserOperation via the bundler client
///   3. Sign paymaster data (if paymaster is configured)
///   4. Sign the UserOperation with the agent's EOA
///   5. Submit to the bundler and wait for on-chain confirmation
async fn execute_erc4337(ctx: &WorkerContext, job: &ExecutionJob) -> RelayerResult {
    let request_id = job.request_id;

    // 0. Resolve the bundler client for this job's chain
    let bundler_client = match ctx.bundler_clients.get(&job.chain) {
        Some(bc) => bc,
        None => {
            return RelayerResult {
                tx_hash: String::new(),
                success: false,
                error: Some(format!("no bundler configured for chain {}", job.chain)),
                block_number: None,
                gas_used: None,
            };
        }
    };

    // 1. Parse addresses
    let smart_wallet: ethers::types::Address = match job.smart_wallet_address.parse() {
        Ok(a) => a,
        Err(e) => {
            return RelayerResult {
                tx_hash: String::new(),
                success: false,
                error: Some(format!("invalid smart_wallet_address: {e}")),
                block_number: None,
                gas_used: None,
            };
        }
    };

    let eoa: ethers::types::Address = match job.eoa_address.parse() {
        Ok(a) => a,
        Err(e) => {
            return RelayerResult {
                tx_hash: String::new(),
                success: false,
                error: Some(format!("invalid eoa_address: {e}")),
                block_number: None,
                gas_used: None,
            };
        }
    };

    // 2. Load the agent's signing key from the wallet registry
    //    We look up by eoa_address to find the right wallet.
    let agent_wallet = match load_agent_wallet_by_eoa(ctx, eoa).await {
        Ok(w) => w,
        Err(e) => {
            return RelayerResult {
                tx_hash: String::new(),
                success: false,
                error: Some(format!("failed to load agent wallet: {e}")),
                block_number: None,
                gas_used: None,
            };
        }
    };

    info!(
        request_id = %request_id,
        smart_wallet = %smart_wallet,
        eoa = %eoa,
        "executing via ERC-4337 bundler"
    );

    // 3. Build the UserOperation (once) with a correctly-sized paymaster
    //    placeholder for gas estimation, then sign paymaster data over the
    //    result.  This avoids a double gas-estimation round-trip that would
    //    produce different gas values from the ones the paymaster signed over.
    let paymaster_signer = ctx.paymaster_signers.get(&job.chain);
    let estimation_paymaster = match paymaster_signer {
        Some(signer) => signer.dummy_paymaster_and_data(),
        None => Vec::new(),
    };

    let mut user_op = match bundler_client
        .build_user_op(job, smart_wallet, estimation_paymaster)
        .await
    {
        Ok(op) => op,
        Err(e) => {
            return RelayerResult {
                tx_hash: String::new(),
                success: false,
                error: Some(format!("failed to build UserOperation: {e}")),
                block_number: None,
                gas_used: None,
            };
        }
    };

    // 4. Sign paymaster data over the built op and splice it in.
    //    The gas fields in user_op are now final — the paymaster signature
    //    will cover the exact same values the EntryPoint sees on-chain.
    if let Some(signer) = paymaster_signer {
        let chain_id: u64 = match bundler_client.provider().get_chainid().await {
            Ok(id) => id.as_u64(),
            Err(e) => {
                return RelayerResult {
                    tx_hash: String::new(),
                    success: false,
                    error: Some(format!("failed to get chain ID: {e}")),
                    block_number: None,
                    gas_used: None,
                };
            }
        };

        let signed_pm_data = match signer.sign_paymaster_data(&user_op, chain_id).await {
            Ok(data) => data,
            Err(e) => {
                return RelayerResult {
                    tx_hash: String::new(),
                    success: false,
                    error: Some(format!("paymaster signing failed: {e}")),
                    block_number: None,
                    gas_used: None,
                };
            }
        };

        // Replace the dummy paymasterAndData with the real signed version.
        // The byte length is identical (181 bytes) so gas estimation remains valid.
        user_op.paymaster_and_data = format!("0x{}", hex::encode(&signed_pm_data));
    }

    // 5. Sign the UserOperation with the agent's EOA key
    //    The signing key is decrypted only for this operation and zeroized immediately.
    let op_hash = match bundler_client.user_op_hash(&user_op).await {
        Ok(h) => h,
        Err(e) => {
            return RelayerResult {
                tx_hash: String::new(),
                success: false,
                error: Some(format!("failed to compute UserOp hash: {e}")),
                block_number: None,
                gas_used: None,
            };
        }
    };

    let signature = match ctx.wallet_registry.decrypt_and_sign(&agent_wallet, op_hash) {
        Ok(sig) => sig,
        Err(e) => {
            return RelayerResult {
                tx_hash: String::new(),
                success: false,
                error: Some(format!("failed to sign UserOperation: {e}")),
                block_number: None,
                gas_used: None,
            };
        }
    };

    let signed_op = bundler_client.apply_signature(user_op, signature);

    // 6. Alchemy-specific preflight simulation of the full UserOperation.
    if bundler_client.is_alchemy_endpoint() {
        match simulation::simulate_user_operation_asset_changes(
            bundler_client.rpc_url(),
            &signed_op,
            bundler_client.entry_point(),
            None,
        )
        .await
        {
            Ok(sim) if sim.success => {
                info!(
                    request_id = %request_id,
                    changes = sim.changes.len(),
                    "alchemy UserOperation simulation succeeded"
                );
            }
            Ok(sim) => {
                return RelayerResult {
                    tx_hash: String::new(),
                    success: false,
                    error: Some(format!(
                        "alchemy simulation rejected UserOperation: {}",
                        sim.error.unwrap_or_else(|| "unknown simulation error".into())
                    )),
                    block_number: None,
                    gas_used: None,
                };
            }
            Err(e) => {
                // Simulation is advisory — a transport/HTTP error should not
                // block submission.  Log a warning and proceed.
                warn!(
                    request_id = %request_id,
                    error = %e,
                    "alchemy simulation request failed, proceeding with submission"
                );
            }
        }
    }

    // 7. Submit to bundler and wait for receipt
    match bundler_client.submit_and_wait(&signed_op).await {
        Ok(result) => {
            let tx_hash = result.tx_hash.clone().unwrap_or_default();
            RelayerResult {
                tx_hash,
                success: result.success,
                error: result.error,
                block_number: result.block_number,
                gas_used: result.gas_used,
            }
        }
        Err(e) => RelayerResult {
            tx_hash: String::new(),
            success: false,
            error: Some(format!("bundler submission failed: {e}")),
            block_number: None,
            gas_used: None,
        },
    }
}

/// Load an agent wallet by its EOA address from the database.
///
/// We query agent_wallets by eoa_address since the worker only has the
/// address strings from the ExecutionJob (not the api_key_id + agent_id).
async fn load_agent_wallet_by_eoa(
    ctx: &WorkerContext,
    eoa: ethers::types::Address,
) -> anyhow::Result<crate::agent_wallet::AgentWallet> {
    let eoa_str = format!("{eoa:?}");
    let row = sqlx::query_as::<_, AgentWalletLookupRow>(
        "SELECT api_key_id, agent_id FROM agent_wallets WHERE eoa_address = $1 LIMIT 1",
    )
    .bind(&eoa_str)
    .fetch_optional(&ctx.db_pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("no agent wallet found for EOA {eoa_str}"))?;

    ctx.wallet_registry
        .get_or_create(row.api_key_id, &row.agent_id)
        .await
}

/// Minimal row type for the reverse lookup.
#[derive(sqlx::FromRow)]
struct AgentWalletLookupRow {
    api_key_id: uuid::Uuid,
    agent_id: String,
}

// ──────────────────────── Webhook helper ─────────────────────────────

/// Fire a webhook notification if the job has a `callback_url`.
///
/// This is a best-effort delivery — failure to deliver does not affect the
/// main execution flow.  The agent can always fall back to polling `/status/{id}`.
async fn fire_webhook(
    ctx: &WorkerContext,
    job: &ExecutionJob,
    status: &ExecutionStatus,
    tx_hash: Option<&str>,
    cost_usd: Option<f64>,
    error_msg: Option<&str>,
) {
    let callback_url = match &job.callback_url {
        Some(url) if !url.is_empty() => url.clone(),
        _ => return, // No callback — nothing to do
    };

    let signing_secret = job.api_key_hash.clone().unwrap_or_default();

    let payload = webhook::WebhookPayload {
        event_id: uuid::Uuid::new_v4(),
        event_type: "execution.completed".to_string(),
        request_id: job.request_id,
        status: status.clone(),
        chain: job.chain.to_string(),
        tx_hash: tx_hash.map(String::from),
        cost_usd,
        error: error_msg.map(String::from),
        created_at: job.created_at,
        completed_at: chrono::Utc::now(),
    };

    // Spawn as a separate task so webhook delivery doesn't block the worker
    // from picking up the next job.
    let client = ctx.webhook_client.clone();
    tokio::spawn(async move {
        webhook::deliver(&client, &callback_url, &payload, &signing_secret).await;
    });
}

// ──────────────────────── Re-enqueue helper ──────────────────────────

/// Remove from processing list and re-enqueue with `attempt_count + 1`.
///
/// If the incremented count reaches [`MAX_JOB_ATTEMPTS`], the job is sent to
/// the dead-letter queue, marked as Failed in the database, and a webhook
/// notification is fired (if the job has a `callback_url`).
async fn re_enqueue_with_bump(
    redis_conn: &mut ConnectionManager,
    ctx: &WorkerContext,
    job: &ExecutionJob,
    worker_id: u32,
) {
    let db_pool = &ctx.db_pool;
    // Remove the *original* job from this worker's processing list first.
    if let Err(e) = queue::ack_job(redis_conn, job, worker_id).await {
        warn!(request_id = %job.request_id, error = %e, "failed to ack job before re-enqueue");
    }

    let mut bumped = job.clone();
    bumped.attempt_count += 1;

    if bumped.attempt_count >= MAX_JOB_ATTEMPTS {
        warn!(
            request_id = %job.request_id,
            attempts = bumped.attempt_count,
            "job reached max attempts — sending to DLQ"
        );
        if let Err(e) = queue::push_to_dlq(redis_conn, &bumped).await {
            error!(request_id = %job.request_id, error = %e, "failed to push job to DLQ");
        }
        if let Err(e) = db::update_execution_status(
            db_pool,
            job.request_id,
            &ExecutionStatus::Failed,
            None,
            Some(&format!("exhausted {} attempts, moved to dead-letter queue", MAX_JOB_ATTEMPTS)),
            None,
            None,
        )
        .await
        {
            error!(
                request_id = %job.request_id,
                error = %e,
                "CRITICAL: failed to update status to failed after DLQ"
            );
        }

        // Webhook notification for exhausted retries
        fire_webhook(
            ctx,
            job,
            &ExecutionStatus::Failed,
            None,
            None,
            Some(&format!("exhausted {} attempts, moved to dead-letter queue", MAX_JOB_ATTEMPTS)),
        )
        .await;

        return;
    }

    if let Err(e) = queue::enqueue_job(redis_conn, &bumped).await {
        error!(
            request_id = %job.request_id,
            error = %e,
            "CRITICAL: failed to re-enqueue failed job — job may be lost"
        );
    } else {
        info!(
            request_id = %job.request_id,
            new_attempt = bumped.attempt_count,
            "job re-enqueued for retry"
        );
    }
}
