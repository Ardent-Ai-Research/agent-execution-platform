//! Transaction simulation.
//!
//! Standard simulation path:
//! * `eth_call` + `eth_estimateGas` against the chain RPC.
//!   Used during the `/execute` and `/simulate` API request path.

use anyhow::Result;
use ethers::prelude::*;
use ethers::types::{Bytes, TransactionRequest};
use std::sync::Arc;
use tracing::{info, warn};

use crate::types::{BatchCall, SimulationResult};

/// Simulate a transaction against the chain RPC.
///
/// Performs an `eth_call` (dry run) followed by `eth_estimateGas`.
pub async fn simulate_transaction(
    provider: Arc<Provider<Http>>,
    from: Address,
    to: Address,
    calldata: Bytes,
    value: U256,
) -> Result<SimulationResult> {
    let tx = TransactionRequest::new()
        .from(from)
        .to(to)
        .data(calldata.clone())
        .value(value);

    // 1. Dry-run via eth_call
    info!(
        target = %to,
        "simulating transaction via eth_call"
    );

    let call_result = provider.call(&tx.clone().into(), None).await;

    match call_result {
        Err(e) => {
            warn!(error = %e, "eth_call simulation failed");
            return Ok(SimulationResult {
                success: false,
                gas_estimate: 0,
                return_data: None,
                error: Some(format!("simulation reverted: {e}")),
            });
        }
        Ok(data) => {
            info!(return_bytes = data.len(), "eth_call succeeded");

            // 2. Estimate gas — if this fails, report it rather than silently
            //    defaulting to 21000 which would almost certainly cause a revert.
            let gas = match provider.estimate_gas(&tx.into(), None).await {
                Ok(g) => g,
                Err(e) => {
                    warn!(error = %e, "eth_estimateGas failed after successful eth_call");
                    return Ok(SimulationResult {
                        success: false,
                        gas_estimate: 0,
                        return_data: Some(format!("0x{}", hex::encode(data))),
                        error: Some(format!("gas estimation failed: {e}")),
                    });
                }
            };

            Ok(SimulationResult {
                success: true,
                gas_estimate: gas.as_u64(),
                return_data: Some(format!("0x{}", hex::encode(data))),
                error: None,
            })
        }
    }
}

/// Simulate a batch of calls against the chain RPC.
///
/// Each call is simulated independently using the smart wallet as `from`.
/// Gas estimates are summed (with overhead per extra call).  If any single
/// call reverts, the entire batch is rejected because `executeBatch` is
/// atomic on-chain — a revert in any leg reverts the whole UserOp.
///
/// v0.9 `BaseAccount.executeBatch(Call[])` supports per-call ETH values
/// natively, so each call is simulated with its specified value.
///
/// **Limitation**: each call is simulated against the *current* chain state,
/// not the post-state of previous calls. If call B depends on state changes
/// from call A (e.g. approve then swap), the simulation of B may be
/// inaccurate.
pub async fn simulate_batch(
    provider: Arc<Provider<Http>>,
    from: Address,
    batch_calls: &[BatchCall],
) -> Result<SimulationResult> {
    let mut total_gas: u64 = 0;
    let mut return_datas = Vec::with_capacity(batch_calls.len());

    for (i, call) in batch_calls.iter().enumerate() {
        let to: Address = call
            .target_contract
            .parse()
            .map_err(|e| anyhow::anyhow!("batch_calls[{i}]: invalid target_contract: {e}"))?;
        let calldata: Bytes = hex::decode(call.calldata.trim_start_matches("0x"))
            .map_err(|e| anyhow::anyhow!("batch_calls[{i}]: invalid calldata: {e}"))?
            .into();

        // v0.9 supports per-call ETH values via the Call struct
        let value = if call.value.trim().is_empty() || call.value.trim() == "0" {
            U256::zero()
        } else {
            U256::from_dec_str(call.value.trim())
                .map_err(|e| anyhow::anyhow!("batch_calls[{i}]: invalid value: {e}"))?
        };

        let sim = simulate_transaction(provider.clone(), from, to, calldata, value).await?;

        if !sim.success {
            return Ok(SimulationResult {
                success: false,
                gas_estimate: 0,
                return_data: None,
                error: Some(format!(
                    "batch_calls[{i}] reverted: {}",
                    sim.error.unwrap_or_default()
                )),
            });
        }

        total_gas = total_gas.saturating_add(sim.gas_estimate);
        if let Some(rd) = sim.return_data {
            return_datas.push(rd);
        }
    }

    // Add per-call overhead for the executeBatch dispatch (~2100 gas per
    // extra CALL opcode beyond the first, plus ABI decoding overhead).
    let batch_overhead = (batch_calls.len().saturating_sub(1) as u64) * 5_000;
    total_gas = total_gas.saturating_add(batch_overhead);

    info!(
        calls = batch_calls.len(),
        total_gas,
        "batch simulation succeeded"
    );

    Ok(SimulationResult {
        success: true,
        gas_estimate: total_gas,
        return_data: Some(format!("[{}]", return_datas.join(","))),
        error: None,
    })
}
