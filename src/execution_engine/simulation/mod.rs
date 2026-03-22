//! Transaction simulation — calls `eth_call` + `eth_estimateGas` against the
//! target RPC and returns a [`SimulationResult`].

use anyhow::Result;
use ethers::prelude::*;
use ethers::types::{Bytes, TransactionRequest};
use std::sync::Arc;
use tracing::{info, warn};

use crate::types::SimulationResult;

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
