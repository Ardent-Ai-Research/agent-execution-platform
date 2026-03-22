//! Database access layer — connection pool + repository functions.

pub mod models;

use anyhow::Result;
use chrono::Utc;
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

// ──────────────────────── Agents ──────────────────────────────────────

pub async fn upsert_agent(pool: &PgPool, wallet: &str) -> Result<Agent> {
    let row = sqlx::query_as::<_, Agent>(
        r#"
        INSERT INTO agents (id, wallet_address, created_at)
        VALUES ($1, $2, $3)
        ON CONFLICT (wallet_address) DO UPDATE SET wallet_address = EXCLUDED.wallet_address
        RETURNING *
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(wallet)
    .bind(Utc::now())
    .fetch_one(pool)
    .await?;
    Ok(row)
}

// ──────────────────────── Execution Requests ─────────────────────────

pub async fn insert_execution_request(
    pool: &PgPool,
    req: &crate::types::ExecutionRequest,
    status: &ExecutionStatus,
) -> Result<ExecutionRequestRow> {
    let now = Utc::now();
    let row = sqlx::query_as::<_, ExecutionRequestRow>(
        r#"
        INSERT INTO execution_requests
            (id, agent_wallet, chain, target_contract, calldata, value, strategy_id, status, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        RETURNING *
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(&req.agent_wallet_address)
    .bind(&req.chain)
    .bind(&req.target_contract)
    .bind(&req.calldata)
    .bind(&req.value)
    .bind(&req.strategy_id)
    .bind(status.to_string())
    .bind(now)
    .bind(now)
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
