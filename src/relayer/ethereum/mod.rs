//! Ethereum Relayer — EIP-2771 meta-transaction edition.
//!
//! Instead of signing transactions directly to the target contract,
//! the relayer ABI-encodes a `Forwarder.execute(ForwardRequest, signature)`
//! call so that the on-chain `_msgSender()` resolves to the agent's address.

use anyhow::{anyhow, Result};
use ethers::prelude::*;
use ethers::signers::LocalWallet;
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers::types::{Bytes, U64};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use crate::execution_engine::simulation::build_forwarder_execute_calldata;
use crate::types::{MetaTxParams, RelayerResult};

/// Ethereum relayer holding a signing wallet and a provider.
/// The relayer pays gas; the agent's identity is preserved via EIP-2771.
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

        let provider = Provider::<Http>::try_from(rpc_url)?
            .interval(Duration::from_millis(500));

        info!(relayer_address = %wallet.address(), "ethereum relayer initialized");

        Ok(Self {
            wallet,
            provider: Arc::new(provider),
        })
    }

    /// Execute a meta-transaction through the MinimalForwarder.
    ///
    /// Builds an EIP-1559 tx from the relayer to the Forwarder contract,
    /// calling `execute(ForwardRequest, signature)`.
    pub async fn execute_meta_tx(&self, params: &MetaTxParams) -> RelayerResult {
        match self.try_execute_meta_tx(params).await {
            Ok(result) => result,
            Err(e) => {
                error!(error = %e, "relayer meta-tx execution failed");
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

    async fn try_execute_meta_tx(&self, params: &MetaTxParams) -> Result<RelayerResult> {
        let forwarder_addr: Address = params.forwarder_address.parse()?;
        let agent_addr: Address = params.agent_address.parse()?;
        let target_addr: Address = params.target_contract.parse()?;
        let inner_calldata: Vec<u8> = hex::decode(params.calldata.trim_start_matches("0x"))?;
        let signature_bytes: Vec<u8> = hex::decode(params.signature.trim_start_matches("0x"))?;

        let value = if params.value.is_empty() || params.value == "0" {
            U256::zero()
        } else {
            U256::from_dec_str(&params.value)?
        };

        // ── Build the Forwarder.execute() calldata (shared with simulation) ──

        let forwarder_calldata = build_forwarder_execute_calldata(
            agent_addr,
            target_addr,
            value,
            params.meta_gas,
            params.forwarder_nonce,
            params.deadline,
            inner_calldata,
            signature_bytes,
        );

        // ── Build and send the relayer's EIP-1559 tx to the forwarder ──
        let chain_id = self.provider.get_chainid().await?;
        let nonce = self
            .provider
            .get_transaction_count(self.wallet.address(), Some(BlockNumber::Pending.into()))
            .await?;

        let (max_fee, priority_fee) = self.estimate_eip1559_fees().await?;

        // Gas for the outer call: inner gas + overhead for forwarder verification (~80k).
        let outer_gas_limit = params.meta_gas + 80_000;

        let tx = Eip1559TransactionRequest::new()
            .to(forwarder_addr)
            .data(Bytes::from(forwarder_calldata))
            .value(value) // forward ETH if the agent's request includes value
            .gas(outer_gas_limit)
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
            forwarder = %forwarder_addr,
            agent = %agent_addr,
            target = %target_addr,
            "broadcasting meta-tx via forwarder"
        );

        let pending = self.provider.send_raw_transaction(signed_tx).await?;
        let tx_hash = pending.tx_hash();

        info!(tx_hash = %tx_hash, "meta-tx broadcast, waiting for confirmation");

        // Wait for confirmation
        match pending.await {
            Ok(Some(receipt)) => {
                let status_code = receipt.status.unwrap_or(U64::from(0));
                let block_num = receipt.block_number.map(|b| b.as_u64());
                let gas_used = receipt.gas_used.map(|g| g.as_u64());

                if status_code == U64::from(1) {
                    info!(tx_hash = %tx_hash, block = ?block_num, "meta-tx confirmed ✓");
                    Ok(RelayerResult {
                        tx_hash: format!("{tx_hash:?}"),
                        success: true,
                        error: None,
                        block_number: block_num,
                        gas_used,
                    })
                } else {
                    warn!(tx_hash = %tx_hash, "meta-tx reverted on-chain");
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
