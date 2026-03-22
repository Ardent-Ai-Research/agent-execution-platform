//! Execution Engine — the brain of the platform.
//!
//! Orchestrates: validation → simulation → pricing → payment check → queue.

pub mod pricing;
pub mod simulation;

use anyhow::{anyhow, Result};
use ethers::prelude::*;
use ethers::types::Bytes;
use std::sync::Arc;
use tracing::info;

use crate::config::AppConfig;
use crate::types::{Chain, ExecutionRequest, SimulationResult};
use pricing::EthPriceCache;

/// Shared state for the execution engine, holding provider references
/// and the ETH/USD price cache.
#[derive(Clone)]
pub struct ExecutionEngine {
    pub config: AppConfig,
    pub eth_provider: Arc<Provider<Http>>,
    pub price_cache: Arc<EthPriceCache>,
}

impl ExecutionEngine {
    pub fn new(config: AppConfig) -> Result<Self> {
        let eth_provider = Provider::<Http>::try_from(&config.ethereum_rpc_url)?;
        let price_cache = Arc::new(EthPriceCache::new(
            config.eth_price_feed_url.clone(),
            config.eth_price_cache_ttl_secs,
        ));
        Ok(Self {
            config,
            eth_provider: Arc::new(eth_provider),
            price_cache,
        })
    }

    /// Resolve the provider for a given chain.  
    /// Currently only Ethereum is supported; extend here for multi-chain.
    pub fn provider_for_chain(&self, chain: &Chain) -> Result<Arc<Provider<Http>>> {
        match chain {
            Chain::Ethereum => Ok(self.eth_provider.clone()),
            other => Err(anyhow!("chain {} not yet supported", other)),
        }
    }

    // ────────────────────── Validation ────────────────────────────────

    /// Validate an inbound execution request.
    pub fn validate(&self, req: &ExecutionRequest) -> Result<Chain> {
        // Chain resolution
        let chain = Chain::from_str_loose(&req.chain)
            .ok_or_else(|| anyhow!("unsupported chain: {}", req.chain))?;

        // Basic address validation (must start with 0x, 42 chars)
        if !req.agent_wallet_address.starts_with("0x") || req.agent_wallet_address.len() != 42 {
            return Err(anyhow!("invalid agent wallet address"));
        }
        if !req.target_contract.starts_with("0x") || req.target_contract.len() != 42 {
            return Err(anyhow!("invalid target contract address"));
        }
        if !req.calldata.starts_with("0x") {
            return Err(anyhow!("calldata must be hex-encoded with 0x prefix"));
        }
        // Validate calldata is actually valid hex
        let calldata_hex = req.calldata.trim_start_matches("0x");
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

        info!(chain = %chain, agent = %req.agent_wallet_address, "request validated");
        Ok(chain)
    }

    // ────────────────────── Simulation ────────────────────────────────

    /// Simulate the transaction and return gas estimate + return data.
    pub async fn simulate(&self, req: &ExecutionRequest, chain: &Chain) -> Result<SimulationResult> {
        let provider = self.provider_for_chain(chain)?;

        let from: Address = req.agent_wallet_address.parse()?;
        let to: Address = req.target_contract.parse()?;
        let calldata: Bytes = hex::decode(req.calldata.trim_start_matches("0x"))?.into();
        let value = if req.value.is_empty() || req.value == "0" {
            U256::zero()
        } else {
            U256::from_dec_str(&req.value)?
        };

        simulation::simulate_transaction(provider, from, to, calldata, value).await
    }

    // ────────────────────── Pricing ──────────────────────────────────

    /// Calculate execution cost in USD based on gas estimate.
    pub async fn estimate_cost(&self, chain: &Chain, gas_estimate: u64) -> Result<f64> {
        let provider = self.provider_for_chain(chain)?;
        pricing::calculate_cost(
            provider,
            gas_estimate,
            self.config.gas_price_markup_pct,
            self.config.platform_fee_usd,
            &self.price_cache,
        )
        .await
    }
}
