//! Ethereum Relayer — hackathon edition.
//!
//! Simplified: no nonce Mutex, no receipt polling loop.
//! Uses ethers PendingTransaction auto-confirmation.

use anyhow::{anyhow, Result};
use ethers::prelude::*;
use ethers::signers::LocalWallet;
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers::types::{Bytes, U64};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::types::RelayerResult;

/// Ethereum relayer holding a signing wallet and a provider.
#[derive(Clone)]
pub struct EthereumRelayer {
    pub wallet: LocalWallet,
    pub provider: Arc<Provider<Http>>,
}

impl EthereumRelayer {
    /// Create a new relayer from a hex-encoded private key and an RPC URL.
    pub fn new(private_key_hex: &str, rpc_url: &str) -> Result<Self> {
        let wallet: LocalWallet = private_key_hex
            .parse::<LocalWallet>()
            .map_err(|e| anyhow!("invalid relayer private key: {e}"))?;

        let provider = Provider::<Http>::try_from(rpc_url)?;

        info!(relayer_address = %wallet.address(), "ethereum relayer initialized");

        Ok(Self {
            wallet,
            provider: Arc::new(provider),
        })
    }

    /// Execute a transaction: build EIP-1559 tx → sign → broadcast → wait.
    pub async fn execute(
        &self,
        target_contract: &str,
        calldata_hex: &str,
        value_str: &str,
        gas_limit: u64,
    ) -> RelayerResult {
        match self.try_execute(target_contract, calldata_hex, value_str, gas_limit).await {
            Ok(result) => result,
            Err(e) => {
                error!(error = %e, "relayer execution failed");
                RelayerResult {
                    tx_hash: String::new(),
                    success: false,
                    error: Some(e.to_string()),
                    block_number: None,
                    gas_used: None,
                }
            }
        }
    }

    async fn try_execute(
        &self,
        target_contract: &str,
        calldata_hex: &str,
        value_str: &str,
        gas_limit: u64,
    ) -> Result<RelayerResult> {
        let to: Address = target_contract.parse()?;
        let calldata: Bytes = hex::decode(calldata_hex.trim_start_matches("0x"))?.into();
        let value = if value_str.is_empty() || value_str == "0" {
            U256::zero()
        } else {
            U256::from_dec_str(value_str)?
        };

        let chain_id = self.provider.get_chainid().await?;
        let nonce = self
            .provider
            .get_transaction_count(self.wallet.address(), Some(BlockNumber::Pending.into()))
            .await?;

        // EIP-1559 fee estimation
        let (max_fee, priority_fee) = self.estimate_eip1559_fees().await?;

        // Build EIP-1559 tx
        let tx = Eip1559TransactionRequest::new()
            .to(to)
            .data(calldata)
            .value(value)
            .gas(gas_limit)
            .max_fee_per_gas(max_fee)
            .max_priority_fee_per_gas(priority_fee)
            .nonce(nonce)
            .chain_id(chain_id.as_u64());

        let wallet = self.wallet.clone().with_chain_id(chain_id.as_u64());
        let typed_tx: TypedTransaction = tx.into();
        let signature = wallet.sign_transaction(&typed_tx).await?;
        let signed_tx = typed_tx.rlp_signed(&signature);

        info!(
            nonce = %nonce,
            max_fee_gwei = max_fee.as_u64() as f64 / 1e9,
            "broadcasting EIP-1559 transaction"
        );

        let pending = self.provider.send_raw_transaction(signed_tx).await?;
        let tx_hash = pending.tx_hash();

        info!(tx_hash = %tx_hash, "transaction broadcast, waiting for confirmation");

        // Wait for confirmation (ethers default: poll until mined)
        match pending.await {
            Ok(Some(receipt)) => {
                let status_code = receipt.status.unwrap_or(U64::from(0));
                let block_num = receipt.block_number.map(|b| b.as_u64());
                let gas_used = receipt.gas_used.map(|g| g.as_u64());

                if status_code == U64::from(1) {
                    info!(tx_hash = %tx_hash, block = ?block_num, "transaction confirmed ✓");
                    Ok(RelayerResult {
                        tx_hash: format!("{tx_hash:?}"),
                        success: true,
                        error: None,
                        block_number: block_num,
                        gas_used,
                    })
                } else {
                    warn!(tx_hash = %tx_hash, "transaction reverted on-chain");
                    Ok(RelayerResult {
                        tx_hash: format!("{tx_hash:?}"),
                        success: false,
                        error: Some("transaction reverted on-chain".into()),
                        block_number: block_num,
                        gas_used,
                    })
                }
            }
            Ok(None) => Ok(RelayerResult {
                tx_hash: format!("{tx_hash:?}"),
                success: false,
                error: Some("no receipt returned".into()),
                block_number: None,
                gas_used: None,
            }),
            Err(e) => Ok(RelayerResult {
                tx_hash: format!("{tx_hash:?}"),
                success: false,
                error: Some(format!("confirmation failed: {e}")),
                block_number: None,
                gas_used: None,
            }),
        }
    }

    /// Estimate EIP-1559 fee parameters from the node.
    async fn estimate_eip1559_fees(&self) -> Result<(U256, U256)> {
        let latest_block = self
            .provider
            .get_block(BlockNumber::Latest)
            .await?
            .ok_or_else(|| anyhow!("could not fetch latest block"))?;

        let base_fee = latest_block
            .base_fee_per_gas
            .ok_or_else(|| anyhow!("chain does not support EIP-1559"))?;

        let priority_fee = match self
            .provider
            .request::<_, U256>("eth_maxPriorityFeePerGas", ())
            .await
        {
            Ok(fee) if fee > U256::zero() => fee,
            _ => U256::from(1_500_000_000u64), // 1.5 gwei
        };

        let max_fee = base_fee * 2 + priority_fee;
        Ok((max_fee, priority_fee))
    }
}
