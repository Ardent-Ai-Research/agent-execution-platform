//! Ethereum Relayer — signs and broadcasts transactions on Ethereum-compatible
//! chains using `ethers-rs`.
//!
//! Responsibilities:
//! * Serialized nonce management (lock held across sign → broadcast → confirm)
//! * EIP-1559 transaction construction (protocol handles fee market)
//! * Poll for on-chain confirmation with configurable timeout
//! * Detect reverts vs. successful inclusion
//!
//! Design rationale — no gas bumping:
//! EIP-1559 base fees adjust automatically per-block. Submitting at the node's
//! recommended `max_fee_per_gas` virtually never gets stuck. If a tx isn't
//! mined within the timeout it's because the call itself is problematic, not
//! because gas was too low. A simple "fail + fresh retry" is safer than
//! replacement txs for a platform handling user funds.

use anyhow::{anyhow, Result};
use ethers::prelude::*;
use ethers::signers::LocalWallet;
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers::types::{Bytes, U64};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::types::{ExecutionJob, RelayerResult};

/// How long to poll for a tx receipt before giving up.
const TX_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(90);

/// How often to poll for the receipt.
const TX_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Ethereum relayer holding a signing wallet, a provider, and a nonce tracker.
#[derive(Clone)]
pub struct EthereumRelayer {
    pub wallet: LocalWallet,
    pub provider: Arc<Provider<Http>>,
    /// Nonce state is protected by a Mutex. The lock is held across the entire
    /// nonce-acquire → sign → broadcast → confirm cycle to prevent races
    /// between concurrent workers.
    nonce: Arc<Mutex<Option<U256>>>,
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
            nonce: Arc::new(Mutex::new(None)),
        })
    }

    /// Execute an [`ExecutionJob`] with full lifecycle management:
    /// acquire nonce → sign EIP-1559 tx → broadcast → wait for confirmation.
    ///
    /// The nonce Mutex is held for the entire duration to prevent concurrent
    /// workers from creating nonce collisions.
    pub async fn execute(&self, job: &ExecutionJob) -> RelayerResult {
        // Hold the nonce lock for the entire sign → broadcast → confirm cycle.
        // This serializes all tx submissions through this relayer, which is
        // the correct behavior for a single-key relayer.
        let mut nonce_guard = self.nonce.lock().await;

        let result = self.try_execute_locked(job, &mut nonce_guard).await;

        match result {
            Ok(confirmed) => confirmed,
            Err(e) => {
                error!(request_id = %job.request_id, error = %e, "relayer execution failed");
                // On error, invalidate the nonce cache so the next attempt
                // re-fetches from the node. This is safe because we hold the lock.
                *nonce_guard = None;
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

    /// Inner execution logic, called while the nonce lock is held.
    async fn try_execute_locked(
        &self,
        job: &ExecutionJob,
        nonce_guard: &mut Option<U256>,
    ) -> Result<RelayerResult> {
        let to: Address = job.target_contract.parse()?;
        let calldata: Bytes = hex::decode(job.calldata.trim_start_matches("0x"))?.into();
        let value = if job.value.is_empty() || job.value == "0" {
            U256::zero()
        } else {
            U256::from_dec_str(&job.value)?
        };

        let chain_id = self.provider.get_chainid().await?;
        let nonce = self.next_nonce_locked(nonce_guard).await?;

        // ── EIP-1559 fee estimation ─────────────────────────────────
        // Query the node for current base fee + priority fee.  The node
        // returns sensible defaults; we add a 20% buffer on max_fee to
        // absorb up to ~2 blocks of base-fee increases.
        let (max_fee, priority_fee) = self.estimate_eip1559_fees().await?;

        // ── Build EIP-1559 tx ───────────────────────────────────────
        let tx = Eip1559TransactionRequest::new()
            .to(to)
            .data(calldata)
            .value(value)
            .gas(job.gas_limit)
            .max_fee_per_gas(max_fee)
            .max_priority_fee_per_gas(priority_fee)
            .nonce(nonce)
            .chain_id(chain_id.as_u64());

        let wallet = self.wallet.clone().with_chain_id(chain_id.as_u64());
        let typed_tx: TypedTransaction = tx.into();
        let signature = wallet.sign_transaction(&typed_tx).await?;
        let signed_tx = typed_tx.rlp_signed(&signature);

        // ── Broadcast ───────────────────────────────────────────────
        info!(
            request_id = %job.request_id,
            nonce = %nonce,
            max_fee_gwei = max_fee.as_u64() as f64 / 1e9,
            priority_fee_gwei = priority_fee.as_u64() as f64 / 1e9,
            "broadcasting EIP-1559 transaction"
        );

        let pending = self.provider.send_raw_transaction(signed_tx).await?;
        let tx_hash = pending.tx_hash();

        info!(
            request_id = %job.request_id,
            tx_hash = %tx_hash,
            "transaction broadcast, waiting for confirmation"
        );

        // ── Wait for confirmation ───────────────────────────────────
        match self.wait_for_receipt(tx_hash).await {
            Ok(receipt) => {
                let status_code = receipt.status.unwrap_or(U64::from(0));
                let block_num = receipt.block_number.map(|b| b.as_u64());
                let gas_used = receipt.gas_used.map(|g| g.as_u64());

                if status_code == U64::from(1) {
                    info!(
                        request_id = %job.request_id,
                        tx_hash = %tx_hash,
                        block = ?block_num,
                        gas_used = ?gas_used,
                        "transaction confirmed on-chain ✓"
                    );

                    Ok(RelayerResult {
                        tx_hash: format!("{tx_hash:?}"),
                        success: true,
                        error: None,
                        block_number: block_num,
                        gas_used,
                    })
                } else {
                    // On-chain revert — the nonce was consumed, don't roll back
                    warn!(
                        request_id = %job.request_id,
                        tx_hash = %tx_hash,
                        "transaction reverted on-chain"
                    );

                    Ok(RelayerResult {
                        tx_hash: format!("{tx_hash:?}"),
                        success: false,
                        error: Some("transaction reverted on-chain".into()),
                        block_number: block_num,
                        gas_used,
                    })
                }
            }
            Err(e) => {
                // Timeout waiting for receipt. The tx may still be in the
                // mempool but with EIP-1559 fees this is rare. Invalidate
                // the nonce cache so the next attempt re-syncs from the node
                // (which will skip past any nonce that did eventually land).
                warn!(
                    request_id = %job.request_id,
                    tx_hash = %tx_hash,
                    nonce = %nonce,
                    error = %e,
                    "tx not confirmed within timeout"
                );

                *nonce_guard = None;

                Ok(RelayerResult {
                    tx_hash: format!("{tx_hash:?}"),
                    success: false,
                    error: Some(format!("tx broadcast but not confirmed: {e}")),
                    block_number: None,
                    gas_used: None,
                })
            }
        }
    }

    /// Estimate EIP-1559 fee parameters from the node.
    ///
    /// Returns `(max_fee_per_gas, max_priority_fee_per_gas)`.
    ///
    /// Strategy:
    /// - `max_priority_fee_per_gas` = node's suggested tip (eth_maxPriorityFeePerGas)
    /// - `max_fee_per_gas` = 2 × latest_base_fee + priority_fee
    ///   This covers ~2 consecutive blocks of 100% full base-fee increases,
    ///   which is the standard heuristic (same as ethers.js / MetaMask).
    async fn estimate_eip1559_fees(&self) -> Result<(U256, U256)> {
        // Fetch latest block to get base_fee_per_gas
        let latest_block = self
            .provider
            .get_block(BlockNumber::Latest)
            .await?
            .ok_or_else(|| anyhow!("could not fetch latest block"))?;

        let base_fee = latest_block
            .base_fee_per_gas
            .ok_or_else(|| anyhow!("chain does not support EIP-1559 (no base_fee_per_gas)"))?;

        // eth_maxPriorityFeePerGas — falls back to 1.5 gwei if the node
        // doesn't support the call (some older nodes).
        let priority_fee = match self.provider.request::<_, U256>("eth_maxPriorityFeePerGas", ()).await {
            Ok(fee) if fee > U256::zero() => fee,
            _ => U256::from(1_500_000_000u64), // 1.5 gwei
        };

        // max_fee = 2 * base_fee + priority_fee
        let max_fee = base_fee * 2 + priority_fee;

        Ok((max_fee, priority_fee))
    }

    /// Get the next nonce from cache or node. Caller must hold `nonce_guard`.
    async fn next_nonce_locked(&self, nonce_guard: &mut Option<U256>) -> Result<U256> {
        match *nonce_guard {
            Some(n) => {
                let next = n + 1;
                *nonce_guard = Some(next);
                Ok(next)
            }
            None => {
                let n = self
                    .provider
                    .get_transaction_count(
                        self.wallet.address(),
                        Some(BlockNumber::Pending.into()),
                    )
                    .await?;
                *nonce_guard = Some(n);
                Ok(n)
            }
        }
    }

    /// Poll for a transaction receipt until confirmed or timeout.
    async fn wait_for_receipt(&self, tx_hash: TxHash) -> Result<TransactionReceipt> {
        let deadline = tokio::time::Instant::now() + TX_CONFIRMATION_TIMEOUT;

        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow!(
                    "timeout after {}s waiting for receipt of {tx_hash:?}",
                    TX_CONFIRMATION_TIMEOUT.as_secs()
                ));
            }

            match self.provider.get_transaction_receipt(tx_hash).await {
                Ok(Some(receipt)) => return Ok(receipt),
                Ok(None) => {
                    // Not mined yet, keep polling
                    tokio::time::sleep(TX_POLL_INTERVAL).await;
                }
                Err(e) => {
                    warn!(tx_hash = %tx_hash, error = %e, "RPC error polling receipt, retrying");
                    tokio::time::sleep(TX_POLL_INTERVAL).await;
                }
            }
        }
    }
}
