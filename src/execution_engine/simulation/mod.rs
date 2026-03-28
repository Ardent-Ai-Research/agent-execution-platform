//! Transaction simulation — calls `eth_call` + `eth_estimateGas` against the
//! target RPC and returns a [`SimulationResult`].
//!
//! Two modes:
//! - **Forwarder simulation** (`simulate_via_forwarder`): Simulates the exact
//!   `Forwarder.execute(request, signature)` call the relayer will broadcast.
//!   This gives 100% accurate gas estimation including forwarder overhead.
//! - **Inner-call simulation** (`simulate_inner_call`): Simulates only the
//!   inner call (`from=agent → target`) for quick dry-runs where a valid
//!   signature may not be available yet.

use anyhow::Result;
use ethers::abi::{encode, Token};
use ethers::prelude::*;
use ethers::types::{Bytes, TransactionRequest};
use std::sync::Arc;
use tracing::{info, warn};

use crate::types::SimulationResult;

/// Build the ABI-encoded calldata for `Forwarder.execute(ForwardRequest, signature)`.
///
/// This is the same encoding the relayer uses for the real transaction,
/// extracted here so simulation can mirror it exactly.
pub fn build_forwarder_execute_calldata(
    agent: Address,
    target: Address,
    value: U256,
    meta_gas: u64,
    forwarder_nonce: u64,
    deadline: u64,
    inner_calldata: Vec<u8>,
    signature_bytes: Vec<u8>,
) -> Vec<u8> {
    let forward_request_tuple = Token::Tuple(vec![
        Token::Address(agent),
        Token::Address(target),
        Token::Uint(value),
        Token::Uint(U256::from(meta_gas)),
        Token::Uint(U256::from(forwarder_nonce)),
        Token::Uint(U256::from(deadline)),
        Token::Bytes(inner_calldata),
    ]);

    let encoded_args = encode(&[forward_request_tuple, Token::Bytes(signature_bytes)]);

    // Selector: keccak256("execute((address,address,uint256,uint256,uint256,uint48,bytes),bytes)")
    let selector = &ethers::utils::keccak256(
        b"execute((address,address,uint256,uint256,uint256,uint48,bytes),bytes)",
    )[..4];

    let mut calldata = Vec::with_capacity(4 + encoded_args.len());
    calldata.extend_from_slice(selector);
    calldata.extend_from_slice(&encoded_args);
    calldata
}

/// Simulate the full `Forwarder.execute()` call as the relayer would send it.
///
/// `from` = relayer address (the actual `msg.sender` on-chain).
/// `to`   = forwarder address.
/// `data` = ABI-encoded `execute(ForwardRequest, signature)`.
///
/// This is 100% accurate: the EVM executes the forwarder's signature
/// verification, nonce check, and inner call — identical to a real broadcast.
pub async fn simulate_via_forwarder(
    provider: Arc<Provider<Http>>,
    relayer: Address,
    forwarder: Address,
    agent: Address,
    target: Address,
    value: U256,
    meta_gas: u64,
    forwarder_nonce: u64,
    deadline: u64,
    inner_calldata: Vec<u8>,
    signature_bytes: Vec<u8>,
) -> Result<SimulationResult> {
    let forwarder_calldata = build_forwarder_execute_calldata(
        agent,
        target,
        value,
        meta_gas,
        forwarder_nonce,
        deadline,
        inner_calldata,
        signature_bytes,
    );

    let tx = TransactionRequest::new()
        .from(relayer)
        .to(forwarder)
        .data(Bytes::from(forwarder_calldata))
        .value(value);

    info!(
        relayer = %relayer,
        forwarder = %forwarder,
        agent = %agent,
        target = %target,
        "simulating full forwarder.execute() via eth_call"
    );

    let call_result = provider.call(&tx.clone().into(), None).await;

    match call_result {
        Err(e) => {
            warn!(error = %e, "forwarder simulation failed (eth_call)");
            Ok(SimulationResult {
                success: false,
                gas_estimate: 0,
                return_data: None,
                error: Some(format!("forwarder simulation reverted: {e}")),
            })
        }
        Ok(data) => {
            info!(return_bytes = data.len(), "forwarder simulation succeeded");

            let gas = match provider.estimate_gas(&tx.into(), None).await {
                Ok(g) => g,
                Err(e) => {
                    warn!(error = %e, "eth_estimateGas failed after successful forwarder eth_call");
                    return Ok(SimulationResult {
                        success: false,
                        gas_estimate: 0,
                        return_data: Some(format!("0x{}", hex::encode(&data))),
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

/// Simulate only the inner call (agent → target) without going through
/// the forwarder. Useful for `/simulate` where a valid EIP-712 signature
/// may not be available.
pub async fn simulate_inner_call(
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

    info!(
        target = %to,
        "simulating inner call via eth_call (no forwarder)"
    );

    let call_result = provider.call(&tx.clone().into(), None).await;

    match call_result {
        Err(e) => {
            warn!(error = %e, "inner-call simulation failed");
            Ok(SimulationResult {
                success: false,
                gas_estimate: 0,
                return_data: None,
                error: Some(format!("simulation reverted: {e}")),
            })
        }
        Ok(data) => {
            info!(return_bytes = data.len(), "inner-call eth_call succeeded");

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
