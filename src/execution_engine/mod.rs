//! Execution Engine — the brain of the platform.
//!
//! Orchestrates: validation → simulation → pricing → payment check → queue.
//! Now supports ERC-4337 Account Abstraction — agents are identified by
//! `agent_id` and execute through platform-managed smart wallets.

pub mod pricing;
pub mod simulation;

use anyhow::{anyhow, Result};
use ethers::prelude::*;
use ethers::types::Bytes;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

use crate::config::AppConfig;
use crate::relayer::erc4337::BundlerClient;
use crate::types::{Chain, ExecutionRequest, SimulationResult};
use pricing::NativeTokenPriceCache;

/// Shared state for the execution engine, holding per-chain providers
/// and native-token/USD price caches.
#[derive(Clone)]
pub struct ExecutionEngine {
    pub config: AppConfig,
    /// Per-chain JSON-RPC providers.
    providers: HashMap<Chain, Arc<Provider<Http>>>,
    /// Per-chain native-token/USD price caches.
    price_caches: HashMap<Chain, Arc<NativeTokenPriceCache>>,
}

impl ExecutionEngine {
    /// Boot the execution engine for all configured chains.
    pub fn new(config: AppConfig) -> Result<Self> {
        let mut providers = HashMap::new();
        let mut price_caches = HashMap::new();

        for (chain, chain_cfg) in &config.chains {
            let provider = Provider::<Http>::try_from(&chain_cfg.rpc_url)
                .map_err(|e| anyhow!("failed to create provider for {}: {}", chain, e))?;
            providers.insert(chain.clone(), Arc::new(provider));

            let cache = Arc::new(NativeTokenPriceCache::new(
                chain_cfg.price_feed_url.clone(),
                config.price_cache_ttl_secs,
            ));
            price_caches.insert(chain.clone(), cache);

            info!(chain = %chain, rpc = %chain_cfg.rpc_url, "provider initialized");
        }

        Ok(Self {
            config,
            providers,
            price_caches,
        })
    }

    /// Resolve the provider for a given chain.
    pub fn provider_for_chain(&self, chain: &Chain) -> Result<Arc<Provider<Http>>> {
        self.providers
            .get(chain)
            .cloned()
            .ok_or_else(|| anyhow!("chain {} is not configured", chain))
    }

    /// Resolve the price cache for a given chain.
    pub fn price_cache_for_chain(&self, chain: &Chain) -> Result<Arc<NativeTokenPriceCache>> {
        self.price_caches
            .get(chain)
            .cloned()
            .ok_or_else(|| anyhow!("price cache not configured for chain {}", chain))
    }

    // ────────────────────── Validation ────────────────────────────────

    /// Validate an inbound execution request.
    ///
    /// With the ERC-4337 flow, `agent_id` replaces `agent_wallet_address`.
    /// The agent does NOT need to supply a wallet — the platform resolves
    /// the smart wallet from the namespaced agent_id.
    ///
    /// Supports two modes:
    ///   * **Single call** — `target_contract` + `calldata` are validated
    ///     directly.
    ///   * **Batch call** — `batch_calls` is present and each entry is
    ///     validated independently.
    pub fn validate(&self, req: &ExecutionRequest) -> Result<Chain> {
        // Chain resolution
        let chain = Chain::from_str_loose(&req.chain)
            .ok_or_else(|| anyhow!("unsupported chain: {}", req.chain))?;

        // Verify the chain is actually configured (has an RPC URL)
        if !self.providers.contains_key(&chain) {
            return Err(anyhow!(
                "chain '{}' is recognized but not configured — set {}_RPC_URL to enable it",
                chain,
                chain.to_string().to_uppercase()
            ));
        }

        // Agent ID must be non-empty
        if req.agent_id.trim().is_empty() {
            return Err(anyhow!("agent_id is required"));
        }
        // Agent ID length check (reasonable limit to prevent abuse)
        if req.agent_id.len() > 256 {
            return Err(anyhow!("agent_id too long (max 256 characters)"));
        }

        // Decide between batch and single-call validation
        if let Some(ref batch_calls) = req.batch_calls {
            if batch_calls.is_empty() {
                return Err(anyhow!("batch_calls is present but empty — provide at least one call"));
            }
            if batch_calls.len() > 16 {
                return Err(anyhow!(
                    "batch_calls has {} entries — max 16 per UserOperation to stay within block gas limits",
                    batch_calls.len()
                ));
            }
            for (i, call) in batch_calls.iter().enumerate() {
                Self::validate_call_fields(
                    &call.target_contract,
                    &call.calldata,
                )
                .map_err(|e| anyhow!("batch_calls[{i}]: {e}"))?;
            }
        } else {
            // Single-call mode — original validation
            Self::validate_call_fields(&req.target_contract, &req.calldata)?;
        }

        info!(chain = %chain, agent_id = %req.agent_id, "request validated");
        Ok(chain)
    }

    /// Validate target contract address and calldata (shared between single
    /// and batch call modes).
    fn validate_call_fields(target_contract: &str, calldata: &str) -> Result<()> {
        if !target_contract.starts_with("0x") || target_contract.len() != 42 {
            return Err(anyhow!("invalid target contract address"));
        }
        if !calldata.starts_with("0x") {
            return Err(anyhow!("calldata must be hex-encoded with 0x prefix"));
        }
        let calldata_hex = calldata.trim_start_matches("0x");
        if calldata_hex.is_empty() {
            return Err(anyhow!("calldata is empty — must contain at least a 4-byte function selector"));
        }
        if calldata_hex.len() % 2 != 0 {
            return Err(anyhow!("calldata has odd-length hex — must be even number of hex characters"));
        }
        if hex::decode(calldata_hex).is_err() {
            return Err(anyhow!("calldata contains invalid hex characters"));
        }
        if calldata_hex.len() < 8 {
            return Err(anyhow!("calldata too short — must contain at least a 4-byte function selector (8 hex chars)"));
        }
        Ok(())
    }

    // ────────────────────── Simulation ────────────────────────────────

    /// Simulate the transaction using the agent's smart wallet as `from`.
    ///
    /// In the ERC-4337 model, the smart wallet is the `msg.sender` on-chain,
    /// so simulation must use the smart wallet address to get accurate gas
    /// estimates (especially for access-list-dependent contracts).
    ///
    /// For batch requests, each call is simulated independently and gas
    /// estimates are summed. If any call reverts, the entire batch is
    /// rejected (since `executeBatch` is atomic).
    pub async fn simulate(
        &self,
        req: &ExecutionRequest,
        chain: &Chain,
        smart_wallet_address: Address,
    ) -> Result<SimulationResult> {
        let provider = self.provider_for_chain(chain)?;

        if let Some(ref batch_calls) = req.batch_calls {
            // Batch mode: simulate each call individually, sum gas, fail on first revert
            simulation::simulate_batch(
                provider,
                smart_wallet_address,
                batch_calls,
            )
            .await
        } else {
            // Single-call mode
            let to: Address = req.target_contract.parse()?;
            let calldata: Bytes = hex::decode(req.calldata.trim_start_matches("0x"))?.into();
            let value = if req.value.is_empty() || req.value == "0" {
                U256::zero()
            } else {
                U256::from_dec_str(&req.value)?
            };

            simulation::simulate_transaction(provider, smart_wallet_address, to, calldata, value).await
        }
    }

    // ────────────────────── Pricing ──────────────────────────────────

    /// Calculate execution cost in USD based on gas estimate.
    ///
    /// Gas prices are fetched from the bundler via `rundler_getUserOperationGasPrice`.
    /// There is no node-based fallback — the bundler is the authoritative source
    /// for fee recommendations since all UserOperations go through it.
    ///
    /// For ERC-4337, the total gas includes the UserOp overhead (verification
    /// gas + pre-verification gas) on top of the call gas. We add a buffer
    /// to account for this.
    pub async fn estimate_cost(
        &self,
        chain: &Chain,
        gas_estimate: u64,
        bundler_client: &BundlerClient,
    ) -> Result<f64> {
        // Fetch gas price from the bundler (single source of truth)
        let (max_fee_per_gas, _max_priority_fee) = bundler_client.get_gas_prices().await?;

        // ERC-4337 overhead: ~100k gas for verification + pre-verification.
        let total_gas_with_aa_overhead = gas_estimate.saturating_add(100_000);

        let price_cache = self.price_cache_for_chain(chain)?;

        pricing::calculate_cost(
            max_fee_per_gas,
            total_gas_with_aa_overhead,
            self.config.gas_price_markup_pct,
            self.config.platform_fee_usd,
            &price_cache,
        )
        .await
    }
}
