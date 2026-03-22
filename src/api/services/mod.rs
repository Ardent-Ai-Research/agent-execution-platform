//! Service layer — thin wrappers that coordinate between the execution engine,
//! database, queue, and payment verification for each API endpoint.

use anyhow::Result;
use chrono::Utc;
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use tracing::info;

use crate::db;
use crate::execution_engine::ExecutionEngine;
use crate::queue;
use crate::types::*;

/// Handle a full execution request:
/// validate → simulate → price → check payment → enqueue.
pub async fn handle_execute(
    engine: &ExecutionEngine,
    pool: &PgPool,
    redis_conn: &mut ConnectionManager,
    req: &ExecutionRequest,
    payment_proof: Option<&PaymentProof>,
) -> Result<ExecutionResponse> {
    // 1. Validate
    let chain = engine.validate(req)?;

    // 2. Persist initial request
    let db_row = db::insert_execution_request(pool, req, &ExecutionStatus::Pending).await?;
    let request_id = db_row.id;

    // 3. Simulate
    let sim = engine.simulate(req, &chain).await?;
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
            estimated_gas: None,
            estimated_cost_usd: None,
            tx_hash: None,
            message: format!("simulation failed: {}", sim.error.unwrap_or_default()),
        });
    }

    // 4. Price
    let cost = engine.estimate_cost(&chain, sim.gas_estimate).await?;
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

    // 5. Check payment
    match payment_proof {
        None => {
            // No payment yet — return 402-equivalent response
            return Ok(ExecutionResponse {
                request_id,
                status: ExecutionStatus::PaymentRequired,
                estimated_gas: Some(sim.gas_estimate),
                estimated_cost_usd: Some(cost),
                tx_hash: None,
                message: "payment required — include X-Payment-Proof header".into(),
            });
        }
        Some(proof) => {
            // Server-side amount cross-check: the on-chain verified payment
            // must cover the platform's calculated cost, regardless of what
            // the client claimed in the header.
            if proof.amount_usd < cost {
                db::update_execution_status(
                    pool,
                    request_id,
                    &ExecutionStatus::Failed,
                    None,
                    Some(&format!(
                        "underpayment: paid {:.6} USD, required {:.6} USD",
                        proof.amount_usd, cost
                    )),
                    None,
                    None,
                )
                .await?;

                return Ok(ExecutionResponse {
                    request_id,
                    status: ExecutionStatus::Failed,
                    estimated_gas: Some(sim.gas_estimate),
                    estimated_cost_usd: Some(cost),
                    tx_hash: None,
                    message: format!(
                        "underpayment: paid {:.6} USD, required {:.6} USD",
                        proof.amount_usd, cost
                    ),
                });
            }

            // Atomically record payment — returns None if tx_hash already used
            // (race-condition-safe via UNIQUE constraint + ON CONFLICT DO NOTHING)
            let inserted = db::insert_payment(pool, request_id, proof).await?;
            if inserted.is_none() {
                return Ok(ExecutionResponse {
                    request_id,
                    status: ExecutionStatus::Failed,
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

    // 6. Enqueue
    // Apply a 20% gas buffer for the relayer to prevent out-of-gas reverts.
    // Pricing was already calculated on the raw estimate, so the user isn't
    // overcharged — the buffer is a safety margin absorbed by the platform.
    let gas_limit_with_buffer = sim.gas_estimate.saturating_mul(120) / 100;

    let job = ExecutionJob {
        request_id,
        agent_wallet: req.agent_wallet_address.clone(),
        chain,
        target_contract: req.target_contract.clone(),
        calldata: req.calldata.clone(),
        value: req.value.clone(),
        gas_limit: gas_limit_with_buffer,
        created_at: Utc::now(),
        attempt_count: 0,
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

    info!(request_id = %request_id, "execution request queued");

    Ok(ExecutionResponse {
        request_id,
        status: ExecutionStatus::Queued,
        estimated_gas: Some(sim.gas_estimate),
        estimated_cost_usd: Some(cost),
        tx_hash: None,
        message: "execution queued".into(),
    })
}

/// Handle a simulation-only request (no payment, no queue).
pub async fn handle_simulate(
    engine: &ExecutionEngine,
    pool: &PgPool,
    req: &ExecutionRequest,
) -> Result<ExecutionResponse> {
    let chain = engine.validate(req)?;
    let db_row = db::insert_execution_request(pool, req, &ExecutionStatus::Pending).await?;
    let request_id = db_row.id;

    let sim = engine.simulate(req, &chain).await?;
    let cost = if sim.success {
        Some(engine.estimate_cost(&chain, sim.gas_estimate).await?)
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
