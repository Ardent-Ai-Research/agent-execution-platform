//! Database access layer — connection pool + repository functions.

pub mod models;

use anyhow::Result;
use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

use crate::types::ExecutionStatus;
use models::*;

/// Create a connection pool with sensible defaults.
pub async fn create_pool(database_url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(database_url)
        .await?;
    Ok(pool)
}

/// Run embedded migrations (from the `migrations/` folder).
pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

// ──────────────────────── API Keys ───────────────────────────────────

/// Look up an API key by its raw value (hashed before query).
pub async fn get_api_key_by_raw(pool: &PgPool, raw_key: &str) -> Result<Option<ApiKeyRow>> {
    let hash = sha256_hex(raw_key);
    let row = sqlx::query_as::<_, ApiKeyRow>(
        "SELECT * FROM api_keys WHERE key_hash = $1 AND is_active = TRUE",
    )
    .bind(&hash)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Insert a new API key. Returns the raw key (only time it's visible).
pub async fn create_api_key(pool: &PgPool, label: Option<&str>) -> Result<(ApiKeyRow, String)> {
    let raw_key = format!("ak_{}", Uuid::new_v4().to_string().replace('-', ""));
    let hash = sha256_hex(&raw_key);
    let row = sqlx::query_as::<_, ApiKeyRow>(
        r#"
        INSERT INTO api_keys (id, key_hash, label, is_active, created_at)
        VALUES ($1, $2, $3, TRUE, now())
        RETURNING *
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(&hash)
    .bind(label)
    .fetch_one(pool)
    .await?;
    Ok((row, raw_key))
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

// ──────────────────────── Execution Requests ─────────────────────────

pub async fn insert_execution_request(
    pool: &PgPool,
    req: &crate::types::ExecutionRequest,
    status: &ExecutionStatus,
    smart_wallet_address: Option<&str>,
    callback_url: Option<&str>,
) -> Result<ExecutionRequestRow> {
    let now = Utc::now();
    let row = sqlx::query_as::<_, ExecutionRequestRow>(
        r#"
        INSERT INTO execution_requests
            (id, agent_wallet, chain, target_contract, calldata, value, strategy_id,
             status, created_at, updated_at, agent_id, smart_wallet_address, callback_url)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
        RETURNING *
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(&req.agent_id) // Legacy `agent_wallet` column — stores agent_id for backward compat with migration 001 schema
    .bind(&req.chain)
    .bind(&req.target_contract)
    .bind(&req.calldata)
    .bind(&req.value)
    .bind(&req.strategy_id)
    .bind(status.to_string())
    .bind(now)
    .bind(now)
    .bind(&req.agent_id)
    .bind(smart_wallet_address)
    .bind(callback_url)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn get_execution_request(pool: &PgPool, id: Uuid) -> Result<Option<ExecutionRequestRow>> {
    let row = sqlx::query_as::<_, ExecutionRequestRow>(
        "SELECT * FROM execution_requests WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn update_execution_status(
    pool: &PgPool,
    id: Uuid,
    status: &ExecutionStatus,
    tx_hash: Option<&str>,
    error_message: Option<&str>,
    gas_estimate: Option<i64>,
    cost_usd: Option<f64>,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE execution_requests
        SET status = $2,
            tx_hash = COALESCE($3, tx_hash),
            error_message = COALESCE($4, error_message),
            gas_estimate = COALESCE($5, gas_estimate),
            cost_usd = COALESCE($6, cost_usd),
            updated_at = $7
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(status.to_string())
    .bind(tx_hash)
    .bind(error_message)
    .bind(gas_estimate)
    .bind(cost_usd)
    .bind(Utc::now())
    .execute(pool)
    .await?;
    Ok(())
}

// ──────────────────────── Transactions ───────────────────────────────

pub async fn insert_transaction(
    pool: &PgPool,
    request_id: Uuid,
    chain: &str,
    tx_hash: &str,
    from_addr: &str,
    to_addr: &str,
    status: &str,
) -> Result<TransactionRow> {
    let row = sqlx::query_as::<_, TransactionRow>(
        r#"
        INSERT INTO transactions
            (id, request_id, chain, tx_hash, from_address, to_address, status, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING *
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(request_id)
    .bind(chain)
    .bind(tx_hash)
    .bind(from_addr)
    .bind(to_addr)
    .bind(status)
    .bind(Utc::now())
    .fetch_one(pool)
    .await?;
    Ok(row)
}

// ──────────────────────── Payments ───────────────────────────────────

/// Atomically insert a payment record.
///
/// Uses `ON CONFLICT (payment_tx_hash) DO NOTHING` so that if two concurrent
/// requests race with the same tx_hash, exactly one succeeds and the other
/// gets `None` back — eliminating the TOCTOU window.
pub async fn insert_payment(
    pool: &PgPool,
    request_id: Uuid,
    proof: &crate::types::PaymentProof,
) -> Result<Option<PaymentRow>> {
    let row = sqlx::query_as::<_, PaymentRow>(
        r#"
        INSERT INTO payments
            (id, request_id, payer, amount_usd, token, payment_chain, payment_tx_hash, verified, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ON CONFLICT (payment_tx_hash) DO NOTHING
        RETURNING *
        "#,
    )
    .bind(proof.payment_id)
    .bind(request_id)
    .bind(&proof.payer)
    .bind(proof.amount_usd)
    .bind(&proof.token)
    .bind(&proof.chain)
    .bind(&proof.tx_hash)
    .bind(proof.verified)
    .bind(Utc::now())
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Check whether a payment tx hash has already been used (replay protection).
/// NOTE: This is a best-effort check. The real enforcement is the UNIQUE
/// constraint in `insert_payment` above.
pub async fn payment_tx_hash_exists(pool: &PgPool, tx_hash: &str) -> Result<bool> {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM payments WHERE payment_tx_hash = $1)",
    )
    .bind(tx_hash)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Resolve the API key hash for a given execution request ID.
///
/// Joins `execution_requests → agent_wallets → api_keys` to find the
/// key_hash that the worker needs for webhook HMAC signing.
///
/// The join uses `smart_wallet_address` (unique per wallet) rather than
/// `agent_id` (which can collide across different API keys).
pub async fn get_api_key_hash_for_request(pool: &PgPool, request_id: Uuid) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        r#"
        SELECT ak.key_hash
        FROM execution_requests er
        JOIN agent_wallets aw ON aw.smart_wallet_address = er.smart_wallet_address
        JOIN api_keys ak ON ak.id = aw.api_key_id
        WHERE er.id = $1
        LIMIT 1
        "#,
    )
    .bind(request_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

// ──────────────────────── Platform Keys ──────────────────────────────

/// Retrieve a platform-managed key by its purpose (e.g. "paymaster_signer").
pub async fn get_platform_key(
    pool: &PgPool,
    purpose: &str,
) -> Result<Option<PlatformKeyRow>> {
    let row = sqlx::query_as::<_, PlatformKeyRow>(
        "SELECT * FROM platform_keys WHERE purpose = $1",
    )
    .bind(purpose)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Persist a new platform-managed key.
///
/// Uses `ON CONFLICT (purpose) DO NOTHING` so that concurrent boot races
/// cannot create duplicates.  Returns the inserted row, or `None` if the
/// purpose already existed (caller should re-fetch).
pub async fn insert_platform_key(
    pool: &PgPool,
    purpose: &str,
    encrypted_key: &str,
    address: &str,
) -> Result<Option<PlatformKeyRow>> {
    let row = sqlx::query_as::<_, PlatformKeyRow>(
        r#"
        INSERT INTO platform_keys (id, purpose, encrypted_key, address, created_at)
        VALUES ($1, $2, $3, $4, now())
        ON CONFLICT (purpose) DO NOTHING
        RETURNING *
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(purpose)
    .bind(encrypted_key)
    .bind(address)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}
