//! Service layer — EIP-2771 meta-transaction edition.
//!
//! Inline synchronous execution: validate → simulate → price → broadcast → return.
//! All executions go through the MinimalForwarder so `_msgSender()` = agent.

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
/// validate → simulate → price → build MetaTxParams → broadcast via forwarder → return.
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

    // 2. Simulate the exact forwarder.execute() call (100% accurate gas)
    let sim = engine.simulate_full(req, &chain).await?;
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

    // 3. Price (full_simulation = true, gas already includes forwarder overhead)
    let cost = engine.estimate_cost(&chain, sim.gas_estimate, true).await?;

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

    // 5. Build MetaTxParams and execute via forwarder
    let params = MetaTxParams {
        agent_address: req.agent_wallet_address.clone(),
        target_contract: req.target_contract.clone(),
        calldata: req.calldata.clone(),
        value: req.value.clone(),
        signature: req.signature.clone(),
        forwarder_address: req.forwarder_address.clone(),
        forwarder_nonce: req.forwarder_nonce,
        deadline: req.deadline,
        meta_gas: req.meta_gas,
    };

    let result = orchestrator.execute(&chain, &params).await;

    // 6. Update store with result
    let (final_status, tx_hash, message) = if result.success {
        (
            ExecutionStatus::Confirmed,
            Some(result.tx_hash.clone()),
            format!("meta-tx confirmed in block {:?}", result.block_number),
        )
    } else if result.error.as_deref() == Some("transaction reverted on-chain") {
        (
            ExecutionStatus::Reverted,
            Some(result.tx_hash.clone()),
            "meta-tx reverted on-chain".into(),
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

    info!(request_id = %request_id, status = %final_status, "meta-tx execution complete");

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
/// Uses inner-call simulation (agent → target) since a valid EIP-712
/// signature may not be available. Forwarder overhead is added in pricing.
pub async fn handle_simulate(
    engine: &ExecutionEngine,
    req: &ExecutionRequest,
) -> Result<ExecutionResponse> {
    let request_id = Uuid::new_v4();

    // 1. Validate
    let chain = engine.validate(req)?;

    // 2. Simulate inner call only (no forwarder)
    let sim = engine.simulate_inner(req, &chain).await?;
    let cost = if sim.success {
        // full_simulation = false → pricing adds ~80k forwarder overhead
        Some(engine.estimate_cost(&chain, sim.gas_estimate, false).await?)
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
