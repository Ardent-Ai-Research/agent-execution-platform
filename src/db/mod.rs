//! Database access layer — connection pool + repository functions.

pub mod models;

use anyhow::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::Utc;
use rand::{rngs::OsRng, RngCore};
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
        r#"
        SELECT id, key_hash, label, is_active, created_at
        FROM api_keys
        WHERE key_hash = $1 AND is_active = TRUE
        "#,
    )
    .bind(&hash)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Insert a new API key. Returns the raw key (only time it's visible).
pub async fn create_api_key(pool: &PgPool, label: Option<&str>) -> Result<(ApiKeyRow, String)> {
    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut secret);
    let raw_key = format!("ak_{}", URL_SAFE_NO_PAD.encode(secret));
    let hash = sha256_hex(&raw_key);
    let row = sqlx::query_as::<_, ApiKeyRow>(
        r#"
        INSERT INTO api_keys (id, key_hash, label, is_active, created_at)
        VALUES ($1, $2, $3, TRUE, now())
        RETURNING id, key_hash, label, is_active, created_at
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
    api_key_id: Uuid,
    req: &crate::types::ExecutionRequest,
    status: &ExecutionStatus,
    smart_wallet_address: Option<&str>,
    callback_url: Option<&str>,
) -> Result<ExecutionRequestRow> {
    let now = Utc::now();
    let row = sqlx::query_as::<_, ExecutionRequestRow>(
        r#"
        INSERT INTO execution_requests
            (id, api_key_id, agent_wallet, chain, target_contract, calldata, value, strategy_id,
             status, created_at, updated_at, agent_id, smart_wallet_address, callback_url)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
        RETURNING *
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(api_key_id)
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
    let row =
        sqlx::query_as::<_, ExecutionRequestRow>("SELECT * FROM execution_requests WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row)
}

/// Look up a request only when it belongs to the authenticated API key.
pub async fn get_execution_request_for_api_key(
    pool: &PgPool,
    id: Uuid,
    api_key_id: Uuid,
) -> Result<Option<ExecutionRequestRow>> {
    let row = sqlx::query_as::<_, ExecutionRequestRow>(
        "SELECT * FROM execution_requests WHERE id = $1 AND api_key_id = $2",
    )
    .bind(id)
    .bind(api_key_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RecentFeedRow {
    pub id: Uuid,
    pub chain: String,
    pub status: String,
    pub tx_hash: Option<String>,
}

pub async fn get_recent_feed_rows(pool: &PgPool, limit: i64) -> Result<Vec<RecentFeedRow>> {
    let bounded = limit.clamp(1, 50);
    let rows = sqlx::query_as::<_, RecentFeedRow>(
        r#"
        SELECT
            er.id,
            er.chain,
            er.status,
            er.tx_hash
        FROM execution_requests er
        ORDER BY er.updated_at DESC
        LIMIT $1
        "#,
    )
    .bind(bounded)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn update_execution_status(
    pool: &PgPool,
    id: Uuid,
    status: &ExecutionStatus,
    tx_hash: Option<&str>,
    error_message: Option<&str>,
    gas_estimate: Option<i64>,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE execution_requests
        SET status = $2,
            tx_hash = COALESCE($3, tx_hash),
            error_message = COALESCE($4, error_message),
            gas_estimate = COALESCE($5, gas_estimate),
            updated_at = $6
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(status.to_string())
    .bind(tx_hash)
    .bind(error_message)
    .bind(gas_estimate)
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

/// Resolve the API key hash by API key id.
pub async fn get_api_key_hash_by_id(pool: &PgPool, api_key_id: Uuid) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        r#"
        SELECT key_hash
        FROM api_keys
        WHERE id = $1
        LIMIT 1
        "#,
    )
    .bind(api_key_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

// ──────────────────────── Platform Keys ──────────────────────────────

/// Retrieve a platform-managed key by its purpose (e.g. "paymaster_signer").
pub async fn get_platform_key(pool: &PgPool, purpose: &str) -> Result<Option<PlatformKeyRow>> {
    let row = sqlx::query_as::<_, PlatformKeyRow>("SELECT * FROM platform_keys WHERE purpose = $1")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ExecutionRequest;

    async fn setup_pool() -> PgPool {
        dotenvy::dotenv().ok();
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL env var");
        let pool = create_pool(&database_url).await.expect("create pool");
        run_migrations(&pool).await.expect("run migrations");
        pool
    }

    fn sample_request(agent_id: &str) -> ExecutionRequest {
        ExecutionRequest {
            agent_id: agent_id.into(),
            chain: "ethereum".into(),
            target_contract: "0x1234567890abcdef1234567890abcdef12345678".into(),
            calldata: "0xdeadbeef".into(),
            value: "0".into(),
            strategy_id: None,
            batch_calls: None,
            callback_url: None,
        }
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL"]
    async fn test_db_api_key_create_and_lookup() {
        let pool = setup_pool().await;

        let (row, raw_key) = create_api_key(&pool, Some("db-test"))
            .await
            .expect("create api key");
        assert!(raw_key.starts_with("ak_"));
        assert!(row.is_active);

        let found = get_api_key_by_raw(&pool, &raw_key)
            .await
            .expect("lookup api key")
            .expect("api key exists");
        assert_eq!(found.id, row.id);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL"]
    async fn test_db_api_key_wrong_key_returns_none() {
        let pool = setup_pool().await;
        let found = get_api_key_by_raw(&pool, "ak_does_not_exist")
            .await
            .expect("lookup non-existent key");
        assert!(found.is_none());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL"]
    async fn test_db_execution_request_lifecycle() {
        let pool = setup_pool().await;
        let req = sample_request("lifecycle");
        let (api_key, _) = create_api_key(&pool, Some("execution-owner"))
            .await
            .expect("create owner API key");

        let row = insert_execution_request(
            &pool,
            api_key.id,
            &req,
            &ExecutionStatus::Pending,
            Some("0xaaaa"),
            None,
        )
        .await
        .expect("insert request");
        assert_eq!(row.status, "pending");
        assert_eq!(row.api_key_id, Some(api_key.id));

        let fetched = get_execution_request(&pool, row.id)
            .await
            .expect("fetch request")
            .expect("request exists");
        assert_eq!(fetched.id, row.id);

        let owned = get_execution_request_for_api_key(&pool, row.id, api_key.id)
            .await
            .expect("fetch owned request");
        assert!(owned.is_some());
        let hidden = get_execution_request_for_api_key(&pool, row.id, Uuid::new_v4())
            .await
            .expect("fetch request with different owner");
        assert!(hidden.is_none());

        update_execution_status(
            &pool,
            row.id,
            &ExecutionStatus::Queued,
            None,
            None,
            Some(100_000),
        )
        .await
        .expect("update request status");

        let updated = get_execution_request(&pool, row.id)
            .await
            .expect("fetch updated request")
            .expect("request exists");
        assert_eq!(updated.status, "queued");
        assert_eq!(updated.gas_estimate, Some(100_000));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL"]
    async fn test_db_platform_keys() {
        let pool = setup_pool().await;
        let purpose = format!("test_key_{}", uuid::Uuid::new_v4());

        assert!(get_platform_key(&pool, &purpose)
            .await
            .expect("get missing key")
            .is_none());

        assert!(insert_platform_key(&pool, &purpose, "enc_data", "0xaabb")
            .await
            .expect("insert platform key")
            .is_some());

        let fetched = get_platform_key(&pool, &purpose)
            .await
            .expect("get platform key")
            .expect("platform key exists");
        assert_eq!(fetched.purpose, purpose);
        assert_eq!(fetched.address, "0xaabb");

        assert!(insert_platform_key(&pool, &purpose, "other", "0xccdd")
            .await
            .expect("duplicate platform key")
            .is_none());
    }
}
