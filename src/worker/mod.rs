//! Background worker that consumes jobs from the Redis queue, dispatches them
//! to the relayer orchestrator, and updates the database.
//!
//! Reliability guarantees:
//!
//! * **No silent job loss**: Jobs live in a per-worker processing list until
//!   explicitly acknowledged.  Crash → stale-job reaper pushes them back.
//! * **Poison-pill protection**: After [`MAX_JOB_ATTEMPTS`] failures the job is
//!   moved to a dead-letter queue instead of looping forever.
//! * **Panic safety**: The relayer call is wrapped in [`tokio::task::spawn`]
//!   so a panic in one job doesn't kill the worker loop.
//! * **Single status write**: Queued → Broadcasting (skips the redundant
//!   "Executing" transition that added no information).

use redis::aio::ConnectionManager;
use sqlx::PgPool;
use tracing::{error, info, warn};

use crate::db;
use crate::queue::{self, MAX_JOB_ATTEMPTS};
use crate::relayer::orchestrator::RelayerOrchestrator;
use crate::types::{ExecutionJob, ExecutionStatus};

/// Spawn a worker loop.  This function runs indefinitely (designed to be
/// `tokio::spawn`'d).
pub async fn run_worker(
    mut redis_conn: ConnectionManager,
    db_pool: PgPool,
    orchestrator: RelayerOrchestrator,
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
                &db_pool,
                request_id,
                &ExecutionStatus::Failed,
                None,
                Some(&format!("exceeded {} execution attempts", MAX_JOB_ATTEMPTS)),
                None,
                None,
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

        // ── Mark as Broadcasting (single transition, replaces old
        //    Executing → Broadcasting double-write) ──────────────────
        if let Err(e) = db::update_execution_status(
            &db_pool,
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

        // ── Execute inside a panic-safe boundary ────────────────────
        let orch_clone = orchestrator.clone();
        let job_clone = job.clone();
        let handle = tokio::spawn(async move {
            orch_clone.execute(&job_clone).await
        });

        let result = match handle.await {
            Ok(r) => r,
            Err(join_err) => {
                // The task panicked or was cancelled
                error!(
                    worker_id,
                    request_id = %request_id,
                    error = %join_err,
                    "PANIC in relayer execution — re-enqueuing job"
                );
                re_enqueue_with_bump(&mut redis_conn, &db_pool, &job, worker_id).await;
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
                &db_pool,
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

            // Insert transaction record with the relayer's actual address
            let relayer_address = orchestrator
                .relayer_address_for_chain(&job.chain)
                .unwrap_or_default();
            if let Err(e) = db::insert_transaction(
                &db_pool,
                request_id,
                &job.chain.to_string(),
                &result.tx_hash,
                &relayer_address,
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

            // ── Acknowledge: remove from processing list ────────────
            if let Err(e) = queue::ack_job(&mut redis_conn, &job, worker_id).await {
                error!(request_id = %request_id, error = %e, "failed to ack job");
            }
        } else {
            let err_msg = result.error.as_deref().unwrap_or("unknown error");
            let is_revert = err_msg.contains("reverted on-chain");

            error!(
                worker_id,
                request_id = %request_id,
                error = err_msg,
                is_revert,
                "execution failed"
            );

            if is_revert {
                // On-chain revert — the nonce was consumed and retrying would
                // revert again. Mark as terminal Reverted and ack from queue.
                let status = &ExecutionStatus::Reverted;
                if let Err(e) = db::update_execution_status(
                    &db_pool,
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
                if let Err(e) = queue::ack_job(&mut redis_conn, &job, worker_id).await {
                    error!(request_id = %request_id, error = %e, "failed to ack reverted job");
                }
            } else {
                // Transient failure — re-enqueue for retry.  Keep status as
                // Broadcasting (not Failed) so the DB reflects the job is
                // still being processed. It will be set to Failed only if
                // attempts are exhausted (DLQ path).
                re_enqueue_with_bump(&mut redis_conn, &db_pool, &job, worker_id).await;
            }
        }
    }
}

/// Remove from processing list and re-enqueue with `attempt_count + 1`.
///
/// If the incremented count reaches [`MAX_JOB_ATTEMPTS`], the job is sent to
/// the dead-letter queue and marked as Failed in the database.
async fn re_enqueue_with_bump(
    redis_conn: &mut ConnectionManager,
    db_pool: &PgPool,
    job: &ExecutionJob,
    worker_id: u32,
) {
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
        // push_to_dlq (not move_to_dlq) because we already acked the
        // original from the processing list above.
        if let Err(e) = queue::push_to_dlq(redis_conn, &bumped).await {
            error!(request_id = %job.request_id, error = %e, "failed to push job to DLQ");
        }
        // Mark as terminal Failed now that the job is truly exhausted.
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
