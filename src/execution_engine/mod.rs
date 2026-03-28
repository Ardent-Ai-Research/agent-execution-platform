//! Execution Engine — EIP-2771 meta-transaction edition.
//!
//! Orchestrates: validation → simulation → pricing.
//! Validates meta-transaction fields (signature, forwarder, nonce).
//!
//! Two simulation modes:
//! - `simulate_full()`: runs the exact `Forwarder.execute()` call the relayer
//!   will broadcast — 100% accurate gas including forwarder overhead.
//! - `simulate_inner()`: runs only the inner call (agent → target) for quick
//!   dry-runs where a valid signature is not yet available.

pub mod pricing;
pub mod simulation;

use anyhow::{anyhow, Result};
use ethers::prelude::*;
use ethers::types::Bytes;
use std::sync::Arc;
use tracing::info;

use crate::config::AppConfig;
use crate::types::{Chain, ExecutionRequest, SimulationResult};

/// Shared state for the execution engine.
#[derive(Clone)]
pub struct ExecutionEngine {
    pub config: AppConfig,
    pub eth_provider: Arc<Provider<Http>>,
    /// The relayer's address — needed for forwarder simulation (from = relayer).
    pub relayer_address: Address,
}

impl ExecutionEngine {
    pub fn new(config: AppConfig) -> Result<Self> {
        let eth_provider = Provider::<Http>::try_from(&config.ethereum_rpc_url)?;

        // Derive the relayer address from the private key.
        let wallet: LocalWallet = config
            .relayer_private_key
            .parse::<LocalWallet>()
            .map_err(|e| anyhow!("invalid relayer key in engine: {e}"))?;
        let relayer_address = wallet.address();

        Ok(Self {
            config,
            eth_provider: Arc::new(eth_provider),
            relayer_address,
        })
    }

    /// Resolve the provider for a given chain.
    pub fn provider_for_chain(&self, chain: &Chain) -> Result<Arc<Provider<Http>>> {
        match chain {
            Chain::Ethereum => Ok(self.eth_provider.clone()),
            other => Err(anyhow!("chain {} not yet supported", other)),
        }
    }

    // ──────────────────── Validation ────────────────────────────────

    /// Validate an inbound execution request including meta-tx fields.
    pub fn validate(&self, req: &ExecutionRequest) -> Result<Chain> {
        let chain = Chain::from_str_loose(&req.chain)
            .ok_or_else(|| anyhow!("unsupported chain: {}", req.chain))?;

        // Agent wallet
        if !req.agent_wallet_address.starts_with("0x") || req.agent_wallet_address.len() != 42 {
            return Err(anyhow!("invalid agent wallet address"));
        }
        // Target contract
        if !req.target_contract.starts_with("0x") || req.target_contract.len() != 42 {
            return Err(anyhow!("invalid target contract address"));
        }
        // Calldata
        if !req.calldata.starts_with("0x") {
            return Err(anyhow!("calldata must be hex-encoded with 0x prefix"));
        }
        let calldata_hex = req.calldata.trim_start_matches("0x");
        if calldata_hex.is_empty() {
            return Err(anyhow!("calldata is empty"));
        }
        if calldata_hex.len() % 2 != 0 {
            return Err(anyhow!("calldata has odd-length hex"));
        }
        if hex::decode(calldata_hex).is_err() {
            return Err(anyhow!("calldata contains invalid hex characters"));
        }
        if calldata_hex.len() < 8 {
            return Err(anyhow!("calldata too short — need at least 4-byte selector"));
        }

        // ── EIP-2771 meta-tx field validation ──

        // Forwarder address
        if !req.forwarder_address.starts_with("0x") || req.forwarder_address.len() != 42 {
            return Err(anyhow!("invalid forwarder address"));
        }

        // Signature (65 bytes = 130 hex chars + 0x prefix)
        let sig_hex = req.signature.trim_start_matches("0x");
        if sig_hex.len() != 130 {
            return Err(anyhow!(
                "invalid signature length: expected 65 bytes (130 hex chars), got {} hex chars",
                sig_hex.len()
            ));
        }
        if hex::decode(sig_hex).is_err() {
            return Err(anyhow!("signature contains invalid hex characters"));
        }

        info!(
            chain = %chain,
            agent = %req.agent_wallet_address,
            forwarder = %req.forwarder_address,
            nonce = req.forwarder_nonce,
            "meta-tx request validated"
        );
        Ok(chain)
    }

    // ──────────────────── Simulation ────────────────────────────────

    /// Simulate the **exact** `Forwarder.execute(request, signature)` call
    /// the relayer will broadcast. Uses `from = relayer`, `to = forwarder`,
    /// and the real ABI-encoded calldata. The gas estimate includes forwarder
    /// overhead natively — no hardcoded +80k needed.
    ///
    /// Use this for `/execute` where we have a real signature.
    pub async fn simulate_full(
        &self,
        req: &ExecutionRequest,
        chain: &Chain,
    ) -> Result<SimulationResult> {
        let provider = self.provider_for_chain(chain)?;

        let agent: Address = req.agent_wallet_address.parse()?;
        let target: Address = req.target_contract.parse()?;
        let forwarder: Address = req.forwarder_address.parse()?;
        let inner_calldata = hex::decode(req.calldata.trim_start_matches("0x"))?;
        let signature_bytes = hex::decode(req.signature.trim_start_matches("0x"))?;
        let value = if req.value.is_empty() || req.value == "0" {
            U256::zero()
        } else {
            U256::from_dec_str(&req.value)?
        };

        simulation::simulate_via_forwarder(
            provider,
            self.relayer_address,
            forwarder,
            agent,
            target,
            value,
            req.meta_gas,
            req.forwarder_nonce,
            req.deadline,
            inner_calldata,
            signature_bytes,
        )
        .await
    }

    /// Simulate only the inner call (agent → target) without the forwarder.
    /// Useful for `/simulate` where a valid EIP-712 signature may not be
    /// available. The gas estimate covers only the inner call — forwarder
    /// overhead is added during pricing.
    pub async fn simulate_inner(
        &self,
        req: &ExecutionRequest,
        chain: &Chain,
    ) -> Result<SimulationResult> {
        let provider = self.provider_for_chain(chain)?;

        let from: Address = req.agent_wallet_address.parse()?;
        let to: Address = req.target_contract.parse()?;
        let calldata: Bytes = hex::decode(req.calldata.trim_start_matches("0x"))?.into();
        let value = if req.value.is_empty() || req.value == "0" {
            U256::zero()
        } else {
            U256::from_dec_str(&req.value)?
        };

        simulation::simulate_inner_call(provider, from, to, calldata, value).await
    }

    // ──────────────────── Pricing ──────────────────────────────────

    /// Calculate execution cost in USD using live gas price + live ETH/USD.
    ///
    /// `gas_estimate` should be the raw gas from simulation.
    /// `full_simulation` indicates whether the estimate already includes
    /// forwarder overhead (true for `simulate_full`, false for `simulate_inner`).
    pub async fn estimate_cost(
        &self,
        chain: &Chain,
        gas_estimate: u64,
        full_simulation: bool,
    ) -> Result<f64> {
        let total_gas = if full_simulation {
            // Gas from simulate_full already includes forwarder overhead
            gas_estimate
        } else {
            // Gas from simulate_inner needs ~80k added for forwarder
            gas_estimate + 80_000
        };

        let provider = self.provider_for_chain(chain)?;
        let gas_price_wei = pricing::fetch_gas_price_wei(&provider).await?;
        let eth_usd = pricing::fetch_eth_usd().await;

        Ok(pricing::calculate_cost(total_gas, gas_price_wei, eth_usd))
    }
}
