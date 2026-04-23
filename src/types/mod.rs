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
    Bnb,
}

impl std::fmt::Display for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Chain::Ethereum => write!(f, "ethereum"),
            Chain::Base => write!(f, "base"),
            Chain::Bnb => write!(f, "bnb"),
        }
    }
}

impl Chain {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "ethereum" | "eth" | "mainnet" => Some(Chain::Ethereum),
            "base" => Some(Chain::Base),
            "bnb" | "bsc" | "binance" => Some(Chain::Bnb),
            _ => None,
        }
    }

    /// Return the EVM chain ID.
    pub fn chain_id(&self) -> u64 {
        match self {
            Chain::Ethereum => 1,
            Chain::Base => 8453,
            Chain::Bnb => 56,
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

/// A single call within a batch transaction.
///
/// When sent inside `ExecutionRequest.batch_calls`, each entry becomes one
/// leg of a `BaseAccount.executeBatch(Call[])` call packed into a single
/// UserOperation (EntryPoint v0.9).
///
/// v0.9 `Call` struct supports per-call ETH values natively:
/// ```solidity
/// struct Call { address target; uint256 value; bytes data; }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchCall {
    pub target_contract: String,
    pub calldata: String,
    #[serde(default)]
    pub value: String,
}

/// Inbound request body for `POST /execute` and `POST /simulate`.
///
/// Supports two modes:
///   1. **Single call** — populate `target_contract` / `calldata` / `value`
///      directly. Maps to `BaseAccount.execute()`.
///   2. **Batch call** — populate `batch_calls` (2+). Maps to
///      `BaseAccount.executeBatch(Call[])` (v0.9). When `batch_calls` is
///      present, `target_contract` / `calldata` / `value` are ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRequest {
    /// Agent-supplied identifier. Combined with the API key to form a
    /// namespaced ID that maps to a unique smart wallet.
    pub agent_id: String,
    pub chain: String,
    /// Target contract for single-call mode. Ignored when `batch_calls` is set.
    #[serde(default)]
    pub target_contract: String,
    /// Calldata for single-call mode. Ignored when `batch_calls` is set.
    #[serde(default)]
    pub calldata: String,
    #[serde(default)]
    pub value: String,
    pub strategy_id: Option<String>,
    /// Optional batch of calls to execute atomically in a single UserOperation.
    /// When present (and non-empty), takes priority over the single-call fields.
    #[serde(default)]
    pub batch_calls: Option<Vec<BatchCall>>,
    /// Optional webhook callback URL.  When provided, the platform will POST
    /// the final execution result to this URL when the transaction reaches a
    /// terminal state (confirmed, failed, reverted).  The agent does not need
    /// to poll `/status/{id}` — the result will be pushed automatically.
    #[serde(default)]
    pub callback_url: Option<String>,
}

/// Response after an execution or simulation request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResponse {
    pub request_id: Uuid,
    pub status: ExecutionStatus,
    /// The agent's ERC-4337 smart wallet address.
    /// Always included so the agent knows where to send tokens before executing.
    pub smart_wallet_address: Option<String>,
    pub estimated_gas: Option<u64>,
    pub estimated_cost_usd: Option<f64>,
    pub tx_hash: Option<String>,
    pub message: String,
}

/// Response for `GET /wallet` — returns the agent's smart wallet address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletResponse {
    /// The agent-supplied identifier.
    pub agent_id: String,
    /// The ERC-4337 smart wallet address (counterfactual or deployed).
    ///
    /// **This address can receive ERC-20 tokens and native currency even before the wallet
    /// contract is deployed.**  CREATE2 makes the address deterministic — tokens
    /// sent here are safe and will be accessible once the wallet is deployed
    /// (automatically on the first UserOperation).
    pub smart_wallet_address: String,
    /// Whether the smart wallet contract is already deployed on-chain.
    /// `false` means it's a counterfactual address — will be deployed on the first UserOperation.
    pub deployed: bool,
    /// Human-readable note explaining wallet funding.
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
    /// The agent-supplied ID (not namespaced — the API key scope is resolved before queuing).
    pub agent_id: String,
    /// The agent's ERC-4337 smart wallet address (acts as `sender` in UserOperation).
    pub smart_wallet_address: String,
    /// The EOA address that owns the smart wallet (for signing UserOperations).
    pub eoa_address: String,
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
    /// When present, the worker uses `executeBatch` instead of `execute`.
    /// Each entry maps to one leg of the batch.
    #[serde(default)]
    pub batch_calls: Option<Vec<BatchCall>>,
    /// Agent-supplied webhook URL.  The worker POSTs the result here on completion.
    #[serde(default)]
    pub callback_url: Option<String>,
    /// SHA-256 hash of the agent's API key (used as HMAC signing secret for webhooks).
    #[serde(default)]
    pub api_key_hash: Option<String>,
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
    /// Optional original quote request ID from a prior 402 response.
    /// When present, server can lock required payment to that quote.
    pub quote_request_id: Option<Uuid>,
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

// ──────────────────────── API Key Context ─────────────────────────────

/// Authenticated API key context, attached to requests by the API key middleware.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyContext {
    pub api_key_id: Uuid,
    pub label: Option<String>,
}

// ──────────────────────── ERC-4337 Types ──────────────────────────────

/// An ERC-4337 PackedUserOperation as defined by EntryPoint v0.9.
///
/// v0.9 packs gas limits and fees into `bytes32` fields to reduce calldata
/// costs.  All hex string fields use `0x`-prefixed encoding.
///
/// Serialized as JSON for the `eth_sendUserOperation` bundler RPC call.
///
/// ## Packing layout
/// - `accountGasLimits = bytes32(uint128(verificationGasLimit) || uint128(callGasLimit))`
/// - `gasFees = bytes32(uint128(maxPriorityFeePerGas) || uint128(maxFeePerGas))`
/// - `paymasterAndData = paymaster(20) || pmVerificationGasLimit(16) || pmPostOpGasLimit(16) || paymasterData`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserOperation {
    /// The smart wallet address submitting the operation.
    pub sender: String,
    /// Anti-replay nonce (managed by the EntryPoint per-sender).
    pub nonce: String,
    /// Factory + factory call data for first-time wallet deployment.
    /// Empty bytes (`"0x"`) after the wallet is already deployed.
    pub init_code: String,
    /// The ABI-encoded call the smart wallet should execute
    /// (e.g. `execute(target, value, calldata)` or `executeBatch(Call[])`).
    pub call_data: String,
    /// Packed gas limits: `bytes32(uint128(verificationGasLimit) || uint128(callGasLimit))`.
    pub account_gas_limits: String,
    /// Gas to compensate the bundler for pre-verification overhead.
    pub pre_verification_gas: String,
    /// Packed fee caps: `bytes32(uint128(maxPriorityFeePerGas) || uint128(maxFeePerGas))`.
    pub gas_fees: String,
    /// ABI-encoded paymaster address + gas limits + paymaster-specific data.
    /// Empty (`"0x"`) if the sender pays its own gas.
    pub paymaster_and_data: String,
    /// The ECDSA signature over the EIP-712 UserOperation hash.
    pub signature: String,
}

/// Result from submitting a UserOperation to the bundler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserOpResult {
    /// The bundler-returned UserOperation hash.
    pub user_op_hash: String,
    /// The eventual on-chain transaction hash (after bundling).
    pub tx_hash: Option<String>,
    /// Whether the operation succeeded.
    pub success: bool,
    /// Error message if failed.
    pub error: Option<String>,
    /// Block number of inclusion.
    pub block_number: Option<u64>,
    /// Actual gas used.
    pub gas_used: Option<u64>,
}
