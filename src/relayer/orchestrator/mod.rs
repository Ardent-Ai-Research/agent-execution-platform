//! Relayer Orchestrator — routes execution jobs to the correct chain-specific
//! relayer and manages retries.
//!
//! Retry strategy:
//! * If a tx reverted on-chain, do NOT retry — the nonce was consumed and the
//!   call would likely revert again.
//! * For any other failure (RPC error, timeout, signing error), retry with a
//!   fresh nonce. The relayer invalidates its nonce cache on failure so the
//!   next attempt re-fetches from the node automatically.
//!
//! No gas bumping — EIP-1559 handles fee-market dynamics at the protocol level.

use ethers::signers::Signer;
use tracing::{error, info, warn};

use crate::relayer::ethereum::EthereumRelayer;
use crate::types::{Chain, ExecutionJob, RelayerResult};

const MAX_RETRIES: u32 = 3;

/// The orchestrator holds a registry of chain → relayer mappings.
#[derive(Clone)]
pub struct RelayerOrchestrator {
    ethereum_relayer: Option<EthereumRelayer>,
    // Add more relayers here: base_relayer, arbitrum_relayer, etc.
}

impl RelayerOrchestrator {
    pub fn new() -> Self {
        Self {
            ethereum_relayer: None,
        }
    }

    /// Register an Ethereum relayer.
    pub fn with_ethereum(mut self, relayer: EthereumRelayer) -> Self {
        self.ethereum_relayer = Some(relayer);
        self
    }

    /// Return the relayer wallet address for a given chain (for audit/logging).
    pub fn relayer_address_for_chain(&self, chain: &Chain) -> Option<String> {
        match chain {
            Chain::Ethereum => self
                .ethereum_relayer
                .as_ref()
                .map(|r| format!("{:?}", r.wallet.address())),
            _ => None,
        }
    }

    /// Route a job to the appropriate relayer and execute with retries.
    pub async fn execute(&self, job: &ExecutionJob) -> RelayerResult {
        info!(
            request_id = %job.request_id,
            chain = %job.chain,
            "orchestrator routing job"
        );

        let mut last_result = RelayerResult {
            tx_hash: String::new(),
            success: false,
            error: Some("no relayer available".into()),
            block_number: None,
            gas_used: None,
        };

        for attempt in 1..=MAX_RETRIES {
            let result = match &job.chain {
                Chain::Ethereum => {
                    if let Some(ref relayer) = self.ethereum_relayer {
                        relayer.execute(job).await
                    } else {
                        return RelayerResult {
                            tx_hash: String::new(),
                            success: false,
                            error: Some("ethereum relayer not configured".into()),
                            block_number: None,
                            gas_used: None,
                        };
                    }
                }
                other => {
                    return RelayerResult {
                        tx_hash: String::new(),
                        success: false,
                        error: Some(format!("no relayer for chain: {other}")),
                        block_number: None,
                        gas_used: None,
                    };
                }
            };

            if result.success {
                return result;
            }

            let err_msg = result.error.as_deref().unwrap_or("unknown");

            // On-chain revert — nonce was consumed, retrying would likely
            // revert again. Return immediately.
            if err_msg.contains("reverted on-chain") {
                error!(
                    request_id = %job.request_id,
                    "transaction reverted on-chain, not retrying"
                );
                return result;
            }

            // Any other error — the relayer already invalidated its nonce
            // cache, so the next attempt will re-sync from the node.
            warn!(
                request_id = %job.request_id,
                attempt,
                error = err_msg,
                "relayer attempt failed, will retry"
            );

            last_result = result;

            // Exponential back-off: 500ms, 1s, 2s
            tokio::time::sleep(std::time::Duration::from_millis(
                500 * 2u64.pow(attempt - 1),
            ))
            .await;
        }

        error!(
            request_id = %job.request_id,
            "all {} relayer attempts exhausted",
            MAX_RETRIES
        );
        last_result
    }
}
