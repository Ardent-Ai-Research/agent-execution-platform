//! Relayer Orchestrator — hackathon edition.
//!
//! Single attempt, no retries. Routes to the correct chain relayer.

use ethers::signers::Signer;
use tracing::{error, info};

use crate::relayer::ethereum::EthereumRelayer;
use crate::types::{Chain, RelayerResult};

/// The orchestrator holds a registry of chain → relayer mappings.
#[derive(Clone)]
pub struct RelayerOrchestrator {
    ethereum_relayer: Option<EthereumRelayer>,
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

    /// Return the relayer wallet address for a given chain.
    pub fn relayer_address_for_chain(&self, chain: &Chain) -> Option<String> {
        match chain {
            Chain::Ethereum => self
                .ethereum_relayer
                .as_ref()
                .map(|r| format!("{:?}", r.wallet.address())),
            _ => None,
        }
    }

    /// Route to the appropriate relayer and execute (single attempt).
    pub async fn execute(
        &self,
        chain: &Chain,
        target_contract: &str,
        calldata: &str,
        value: &str,
        gas_limit: u64,
    ) -> RelayerResult {
        info!(chain = %chain, "orchestrator routing execution");

        match chain {
            Chain::Ethereum => {
                if let Some(ref relayer) = self.ethereum_relayer {
                    relayer.execute(target_contract, calldata, value, gas_limit).await
                } else {
                    error!("ethereum relayer not configured");
                    RelayerResult {
                        tx_hash: String::new(),
                        success: false,
                        error: Some("ethereum relayer not configured".into()),
                        block_number: None,
                        gas_used: None,
                    }
                }
            }
            other => {
                error!(chain = %other, "no relayer for chain");
                RelayerResult {
                    tx_hash: String::new(),
                    success: false,
                    error: Some(format!("no relayer for chain: {other}")),
                    block_number: None,
                    gas_used: None,
                }
            }
        }
    }
}
