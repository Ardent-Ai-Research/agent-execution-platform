//! Core domain types — hackathon edition.
//!
//! EIP-2771 meta-transaction aware: every execution request includes a signed
//! ForwardRequest that the relayer wraps in a `Forwarder.execute()` call.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ──────────────────────────── Enumerations ────────────────────────────

/// Blockchain networks we support.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Chain {
    Ethereum,
    Base,
    Arbitrum,
    Optimism,
}

impl std::fmt::Display for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Chain::Ethereum => write!(f, "ethereum"),
            Chain::Base => write!(f, "base"),
            Chain::Arbitrum => write!(f, "arbitrum"),
            Chain::Optimism => write!(f, "optimism"),
        }
    }
}

impl Chain {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "ethereum" | "eth" | "mainnet" => Some(Chain::Ethereum),
            "base" => Some(Chain::Base),
            "arbitrum" | "arb" => Some(Chain::Arbitrum),
            "optimism" | "op" => Some(Chain::Optimism),
            _ => None,
        }
    }
}

/// Simplified lifecycle for hackathon demo.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Simulated,
    Broadcasting,
    Confirmed,
    Failed,
    Reverted,
}

impl std::fmt::Display for ExecutionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{:?}", self));
        write!(f, "{}", s)
    }
}

// ──────────────────────────── API DTOs ────────────────────────────────

/// Inbound request body for `POST /execute` and `POST /simulate`.
///
/// Every execution is a meta-transaction: the agent signs an EIP-712
/// `ForwardRequest` off-chain, and the relayer wraps it in a
/// `Forwarder.execute()` call so the target sees the agent as `_msgSender()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRequest {
    /// Agent's wallet address (the signer of the EIP-712 message).
    pub agent_wallet_address: String,
    pub chain: String,
    /// The target contract the agent wants to call (not the forwarder).
    pub target_contract: String,
    /// ABI-encoded calldata for the target contract function.
    pub calldata: String,
    #[serde(default)]
    pub value: String,
    pub strategy_id: Option<String>,

    // ── EIP-2771 meta-transaction fields ──
    /// The EIP-712 signature from the agent, hex-encoded with 0x prefix.
    pub signature: String,
    /// Address of the deployed MinimalForwarder contract.
    pub forwarder_address: String,
    /// Agent's nonce on the forwarder (for replay protection).
    pub forwarder_nonce: u64,
    /// Expiry timestamp (unix seconds). 0 = no expiry.
    #[serde(default)]
    pub deadline: u64,
    /// Gas limit the agent is authorizing for the inner call.
    #[serde(default = "default_meta_gas")]
    pub meta_gas: u64,
}

fn default_meta_gas() -> u64 {
    300_000
}

/// Response after an execution or simulation request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResponse {
    pub request_id: Uuid,
    pub status: ExecutionStatus,
    pub estimated_gas: Option<u64>,
    pub estimated_cost_usd: Option<f64>,
    pub tx_hash: Option<String>,
    pub message: String,
}

/// Response for `GET /status/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub request_id: Uuid,
    pub status: ExecutionStatus,
    pub chain: String,
    pub tx_hash: Option<String>,
    pub cost_usd: Option<f64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ──────────────────────────── Internal Models ─────────────────────────

/// Result returned by the relayer after broadcasting a transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayerResult {
    pub tx_hash: String,
    pub success: bool,
    pub error: Option<String>,
    pub block_number: Option<u64>,
    pub gas_used: Option<u64>,
}

/// Simulation output from the execution engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    pub success: bool,
    pub gas_estimate: u64,
    pub return_data: Option<String>,
    pub error: Option<String>,
}

/// Bundled meta-transaction parameters passed to the relayer.
#[derive(Debug, Clone)]
pub struct MetaTxParams {
    pub agent_address: String,
    pub target_contract: String,
    pub calldata: String,
    pub value: String,
    pub signature: String,
    pub forwarder_address: String,
    pub forwarder_nonce: u64,
    pub deadline: u64,
    pub meta_gas: u64,
}
