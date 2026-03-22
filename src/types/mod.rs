//! Core domain types shared across the entire platform.
//!
//! Every module imports from here so changes propagate cleanly.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ──────────────────────────── Enumerations ────────────────────────────

/// Blockchain networks we support.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(type_name = "TEXT")]
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

/// Lifecycle of an execution request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "TEXT")]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Pending,
    PaymentRequired,
    PaymentVerified,
    Queued,
    Executing,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRequest {
    pub agent_wallet_address: String,
    pub chain: String,
    pub target_contract: String,
    pub calldata: String,
    #[serde(default)]
    pub value: String,
    pub strategy_id: Option<String>,
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

/// A fully validated execution job ready for the queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionJob {
    pub request_id: Uuid,
    pub agent_wallet: String,
    pub chain: Chain,
    pub target_contract: String,
    pub calldata: String,
    pub value: String,
    pub gas_limit: u64,
    pub created_at: DateTime<Utc>,
    /// Number of times this job has been attempted (for poison-pill protection).
    /// Defaults to 0 for newly enqueued jobs.
    #[serde(default)]
    pub attempt_count: u32,
}

/// Result returned by a relayer after broadcasting a transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayerResult {
    pub tx_hash: String,
    pub success: bool,
    pub error: Option<String>,
    /// Block number in which the tx was mined (if confirmed).
    pub block_number: Option<u64>,
    /// Actual gas used by the on-chain transaction.
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

/// Payment metadata attached after x402 verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentProof {
    pub payment_id: Uuid,
    pub payer: String,
    pub amount_usd: f64,
    pub token: String,
    pub chain: String,
    pub tx_hash: String,
    pub verified: bool,
    pub verified_at: DateTime<Utc>,
    /// The on-chain amount transferred (in token-native units, e.g. 6-decimal USDC).
    pub confirmed_amount_raw: Option<String>,
    /// Block confirmations at verification time.
    pub block_confirmations: Option<u64>,
    /// The token contract address that was verified on-chain.
    pub token_contract: Option<String>,
}
