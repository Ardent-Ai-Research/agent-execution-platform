//! SQLx models that map directly to PostgreSQL tables.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

// ──────────────────────────── agents ──────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Agent {
    pub id: Uuid,
    pub wallet_address: String,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
}

// ──────────────────────────── execution_requests ─────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ExecutionRequestRow {
    pub id: Uuid,
    pub agent_wallet: String,
    pub chain: String,
    pub target_contract: String,
    pub calldata: String,
    pub value: String,
    pub strategy_id: Option<String>,
    pub gas_estimate: Option<i64>,
    pub cost_usd: Option<f64>,
    pub status: String,
    pub tx_hash: Option<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ──────────────────────────── transactions ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct TransactionRow {
    pub id: Uuid,
    pub request_id: Uuid,
    pub chain: String,
    pub tx_hash: String,
    pub from_address: String,
    pub to_address: String,
    pub gas_used: Option<i64>,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

// ──────────────────────────── payments ───────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PaymentRow {
    pub id: Uuid,
    pub request_id: Uuid,
    pub payer: String,
    pub amount_usd: f64,
    pub token: String,
    pub payment_chain: String,
    pub payment_tx_hash: String,
    pub verified: bool,
    pub created_at: DateTime<Utc>,
}
