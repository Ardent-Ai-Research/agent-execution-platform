//! Service layer — hackathon edition.
//!
//! Inline synchronous execution: validate → simulate → price → broadcast → return.
//! No payment gate, no queue, no database.

use anyhow::Result;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;
use uuid::Uuid;

use crate::api::routes::RequestRecord;
use crate::execution_engine::ExecutionEngine;
use crate::relayer::orchestrator::RelayerOrchestrator;
use crate::types::*;

/// Handle a full execution request inline:
/// validate → simulate → price → broadcast → return result.
pub async fn handle_execute(
    engine: &ExecutionEngine,
    orchestrator: &RelayerOrchestrator,
    store: &Arc<Mutex<HashMap<Uuid, RequestRecord>>>,
    req: &ExecutionRequest,
) -> Result<ExecutionResponse> {
    let request_id = Uuid::new_v4();
    let now = Utc::now();

    // 1. Validate
    let chain = engine.validate(req)?;

    // 2. Simulate
    let sim = engine.simulate(req, &chain).await?;
    if !sim.success {
        // Store failed record
        let record = RequestRecord {
            request_id,
            status: ExecutionStatus::Failed,
            chain: chain.to_string(),
            tx_hash: None,
            cost_usd: None,
            created_at: now,
            updated_at: Utc::now(),
        };
        store.lock().await.insert(request_id, record);

        return Ok(ExecutionResponse {
            request_id,
            status: ExecutionStatus::Failed,
            estimated_gas: None,
            estimated_cost_usd: None,
            tx_hash: None,
            message: format!("simulation failed: {}", sim.error.unwrap_or_default()),
        });
    }

    // 3. Price
    let cost = engine.estimate_cost(&chain, sim.gas_estimate).await?;

    // 4. Store initial record
    {
        let record = RequestRecord {
            request_id,
            status: ExecutionStatus::Broadcasting,
            chain: chain.to_string(),
            tx_hash: None,
            cost_usd: Some(cost),
            created_at: now,
            updated_at: Utc::now(),
        };
        store.lock().await.insert(request_id, record);
    }

    // 5. Execute via relayer (inline, synchronous)
    let gas_limit = sim.gas_estimate.saturating_mul(120) / 100; // 20% buffer
    let result = orchestrator
        .execute(&chain, &req.target_contract, &req.calldata, &req.value, gas_limit)
        .await;

    // 6. Update store with result
    let (final_status, tx_hash, message) = if result.success {
        (
            ExecutionStatus::Confirmed,
            Some(result.tx_hash.clone()),
            format!("transaction confirmed in block {:?}", result.block_number),
        )
    } else if result.error.as_deref() == Some("transaction reverted on-chain") {
        (
            ExecutionStatus::Reverted,
            Some(result.tx_hash.clone()),
            "transaction reverted on-chain".into(),
        )
    } else {
        (
            ExecutionStatus::Failed,
            if result.tx_hash.is_empty() { None } else { Some(result.tx_hash.clone()) },
            format!("execution failed: {}", result.error.unwrap_or_default()),
        )
    };

    {
        let mut s = store.lock().await;
        if let Some(record) = s.get_mut(&request_id) {
            record.status = final_status.clone();
            record.tx_hash = tx_hash.clone();
            record.updated_at = Utc::now();
        }
    }

    info!(request_id = %request_id, status = %final_status, "execution complete");

    Ok(ExecutionResponse {
        request_id,
        status: final_status,
        estimated_gas: Some(sim.gas_estimate),
        estimated_cost_usd: Some(cost),
        tx_hash,
        message,
    })
}

/// Handle a simulation-only request.
pub async fn handle_simulate(
    engine: &ExecutionEngine,
    req: &ExecutionRequest,
) -> Result<ExecutionResponse> {
    let request_id = Uuid::new_v4();

    // 1. Validate
    let chain = engine.validate(req)?;

    // 2. Simulate
    let sim = engine.simulate(req, &chain).await?;
    let cost = if sim.success {
        Some(engine.estimate_cost(&chain, sim.gas_estimate).await?)
    } else {
        None
    };

    Ok(ExecutionResponse {
        request_id,
        status: if sim.success {
            ExecutionStatus::Simulated
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
