# Test Suite — AI Agent Blockchain Execution Platform v1.0

> **60 integration & unit tests** in `tests/integration_tests.rs`
>
> All tests pass against a live PostgreSQL + Redis + Alchemy Sepolia stack.

---

## Running Tests

### Prerequisites

| Dependency | Why |
|---|---|
| **Docker Compose** | PostgreSQL 16 + Redis 7 must be running |
| **Alchemy Sepolia API key** | Simulation and chain-alias tests hit the live Sepolia bundler |
| **`.env` file** | Must be fully configured (see root `README.md` → Getting Started) |

### Commands

```bash
# Start infrastructure
docker compose up -d

# Run the full suite
cargo test -- --test-threads=1

# Run a single test
cargo test test_health_endpoint_returns_ok -- --test-threads=1

# Run a category (fuzzy match)
cargo test test_wallet -- --test-threads=1
```

> **`--test-threads=1`** is required because many tests share the same
> PostgreSQL database and Redis instance. Parallel execution causes data races.

---

## Test Coverage Map

### 1. Health Endpoint (2 tests)

| Test | What it verifies |
|---|---|
| `test_health_endpoint_returns_ok` | `GET /health` returns 200 with `status: "healthy"`, `version`, `database: "ok"`, `redis: "ok"` |
| `test_health_without_api_key_returns_401` | Health endpoint enforces API key middleware (returns 401 without header) |

**Source module:** `src/api/routes/mod.rs` → `health_handler`

### 2. API Key Authentication (3 tests)

| Test | What it verifies |
|---|---|
| `test_missing_api_key_returns_401` | Requests without `X-API-Key` header get 401 |
| `test_invalid_api_key_returns_401` | Requests with a non-existent API key get 401 |
| `test_valid_api_key_passes_auth` | Valid API key passes through middleware and reaches the handler |

**Source module:** `src/api/middleware/mod.rs` → `api_key_middleware`

### 3. Admin API Key Management (3 tests)

| Test | What it verifies |
|---|---|
| `test_admin_create_api_key_without_bearer_returns_error` | `POST /admin/api-keys` without `Authorization: Bearer` returns 403 or 401 |
| `test_admin_create_api_key_wrong_token_returns_401` | Wrong bearer token is rejected |
| `test_admin_create_api_key_success` | Correct bearer token creates a key; response includes `api_key_id`, `api_key`, `label` |

**Source module:** `src/api/routes/mod.rs` → `create_api_key_handler`, `admin_auth_middleware`

### 4. Wallet Provisioning — `GET /wallet` (6 tests)

| Test | What it verifies |
|---|---|
| `test_wallet_returns_smart_wallet_address` | Returns `eoa_address` and `smart_wallet_address` for a valid agent |
| `test_wallet_idempotent` | Same agent+chain returns the same wallet on repeated calls |
| `test_wallet_different_agents_different_addresses` | Different `agent_id` values get distinct wallets |
| `test_wallet_namespace_isolation_across_api_keys` | Different API keys with the same `agent_id` get isolated wallets |
| `test_wallet_unsupported_chain_returns_400` | Unknown chain name → 400 |
| `test_wallet_empty_agent_id_returns_400` | Empty `agent_id` → 400 |

**Source modules:** `src/api/routes/mod.rs` → `wallet_handler`, `src/agent_wallet/mod.rs`

### 5. Simulation — `POST /simulate` (7 tests)

| Test | What it verifies |
|---|---|
| `test_simulate_unsupported_chain_returns_400` | Unknown chain → 400 |
| `test_simulate_invalid_target_returns_400` | Non-address target → 400 |
| `test_simulate_empty_calldata_returns_400` | Missing/empty calldata → 400 |
| `test_simulate_empty_agent_id_returns_400` | Empty agent_id → 400 |
| `test_simulate_valid_call_against_sepolia` | Live call to Alchemy Sepolia bundler returns a gas estimate or simulation error |
| `test_simulate_batch_calls_empty_rejected` | Empty `batch_calls` array → 400 |
| `test_simulate_batch_calls_over_limit_rejected` | >10 batch calls → 400 |

**Source modules:** `src/api/routes/mod.rs` → `simulate_handler`, `src/execution_engine/simulation/mod.rs`

### 6. Execution — `POST /execute` (6 tests)

| Test | What it verifies |
|---|---|
| `test_execute_without_payment_returns_402` | No `X-Payment-Proof` header → 402 with price quote |
| `test_execute_402_includes_accepted_tokens` | 402 response includes `accepted_tokens` list from config |
| `test_execute_unsupported_chain_returns_400` | Unknown chain → 400 |
| `test_execute_invalid_payment_proof_returns_402` | Fake tx hash in proof → 402 (payment not confirmed) |
| `test_execute_malformed_payment_proof_returns_402` | Garbage JSON in proof header → 402 |
| `test_execute_unsupported_token_in_proof_returns_402` | Token not in `ACCEPTED_TOKENS` → 402 |

**Source modules:** `src/api/routes/mod.rs` → `execute_handler`, `src/api/middleware/mod.rs` → `x402_middleware`, `src/payments/mod.rs`

### 7. Status — `GET /status/:id` (3 tests)

| Test | What it verifies |
|---|---|
| `test_status_invalid_uuid` | Non-UUID path → 400 |
| `test_status_nonexistent_returns_404` | Valid UUID that doesn't exist → 404 |
| `test_status_returns_existing_request` | Inserts a request via DB, fetches it, validates all fields |

**Source modules:** `src/api/routes/mod.rs` → `status_handler`, `src/db/mod.rs`

### 8. Calldata Validation (3 tests)

| Test | What it verifies |
|---|---|
| `test_calldata_odd_length_hex_returns_400` | Odd-length hex string → 400 |
| `test_calldata_too_short_returns_400` | Calldata shorter than 4-byte selector → 400 |
| `test_calldata_without_0x_prefix_returns_400` | Missing `0x` prefix → 400 |

**Source module:** `src/api/routes/mod.rs` (validation in handler)

### 9. Chain Aliases (1 test)

| Test | What it verifies |
|---|---|
| `test_chain_aliases_recognized` | `"eth"`, `"mainnet"`, `"sepolia"` all resolve to Ethereum chain |

**Source module:** `src/types/mod.rs` → `Chain::from_str_loose`

### 10. Body Size Limit (1 test)

| Test | What it verifies |
|---|---|
| `test_request_body_size_limit` | Payloads >1 MB are rejected (413 Payload Too Large) |

**Source module:** `src/main.rs` → `RequestBodyLimitLayer`

### 11. Chain Parsing & Display (4 unit tests)

| Test | What it verifies |
|---|---|
| `test_chain_from_str_loose` | Loose string parsing ("eth", "base", "bnb", "bsc", etc.) |
| `test_chain_display` | `Display` impl returns correct canonical names |
| `test_chain_ids` | `chain_id()` returns correct values (1, 8453, 56) |
| `test_execution_status_display` | `ExecutionStatus` Display for all variants |

**Source module:** `src/types/mod.rs`

### 12. Encryption (3 unit tests)

| Test | What it verifies |
|---|---|
| `test_encryption_round_trip` | Encrypt → decrypt with same key recovers original plaintext |
| `test_encryption_wrong_key_fails` | Decrypt with different key fails |
| `test_encryption_nonce_uniqueness` | Two encryptions of the same plaintext produce different ciphertexts (unique nonces) |

**Source module:** `src/agent_wallet/mod.rs` → `encrypt_key`, `decrypt_key`

### 13. Rate Limiter (4 unit tests)

| Test | What it verifies |
|---|---|
| `test_rate_limiter_allows_burst` | Burst capacity is respected (N requests pass, N+1 is rejected) |
| `test_rate_limiter_independent_keys` | Different API key IDs have independent rate limit buckets |
| `test_rate_limiter_retry_after` | Rejected response includes `Retry-After` header |
| `test_rate_limiter_evict_stale` | Stale entries are evicted from the rate limiter map |

**Source module:** `src/rate_limit/mod.rs`

### 14. Queue Operations (3 integration tests, real Redis)

| Test | What it verifies |
|---|---|
| `test_queue_enqueue_and_dequeue` | `enqueue` → `dequeue` round-trip; job data is preserved |
| `test_queue_recover_stale_jobs` | Stale jobs in processing lists are recovered back to main queue |
| `test_queue_dead_letter` | Jobs exceeding max attempts are moved to the dead-letter queue |

**Source module:** `src/queue/mod.rs`

### 15. Database Operations (5 integration tests, real PostgreSQL)

| Test | What it verifies |
|---|---|
| `test_db_api_key_create_and_lookup` | Create API key → look up by raw key → fields match |
| `test_db_api_key_wrong_key_returns_none` | Non-existent key lookup returns `None` |
| `test_db_execution_request_lifecycle` | Insert → update status → fetch → verify all fields |
| `test_db_payment_replay_protection` | Same `payment_tx_hash` cannot be inserted twice (UNIQUE constraint) |
| `test_db_platform_keys` | Insert and retrieve platform keys (paymaster signer storage) |

**Source module:** `src/db/mod.rs`

### 16. Serialization Round-Trips (3 unit tests)

| Test | What it verifies |
|---|---|
| `test_execution_job_serde_round_trip` | `ExecutionJob` survives JSON serialize → deserialize |
| `test_user_operation_serde` | `UserOperation` JSON representation is correct |
| `test_payment_proof_serde` | `PaymentProof` JSON representation is correct |

**Source modules:** `src/types/mod.rs`, `src/queue/mod.rs`

### 17. Config Loading (1 unit test)

| Test | What it verifies |
|---|---|
| `test_config_loads_correctly` | `AppConfig::from_env()` parses all env vars correctly; chain configs populated |

**Source module:** `src/config/mod.rs`

### 18. Webhook HMAC (2 unit tests)

| Test | What it verifies |
|---|---|
| `test_hmac_sha256_deterministic` | Same key + message produces same HMAC (deterministic) |
| `test_hmac_different_secrets` | Different keys produce different HMACs |

**Source module:** `src/webhook/mod.rs`

---

## What Is NOT Covered (Gaps)

### Critical Gaps

| Gap | Module | Risk |
|---|---|---|
| **Worker execution loop** | `src/worker/mod.rs` | The background worker that dequeues jobs and drives them through the execution pipeline is untested. A bug here silently drops requests. |
| **ERC-4337 UserOperation building** | `src/relayer/erc4337/mod.rs` | UserOp construction (nonce, calldata packing, gas fields) is not tested. Malformed UserOps are rejected by the bundler at runtime. |
| **Paymaster signing** | `src/relayer/paymaster/mod.rs` | The `signPaymasterData` flow (hash → ECDSA sign → pack into paymasterAndData) has no unit tests. A signing bug causes all executions to revert. |
| **Bundler RPC submission** | `src/relayer/erc4337/mod.rs` | `eth_sendUserOperation` call and response parsing are untested. |
| **Webhook delivery** | `src/webhook/mod.rs` | HTTP POST to callback URLs with HMAC signing, retry logic, HTTPS enforcement, and redirect blocking are untested (only HMAC computation is tested). |

### Moderate Gaps

| Gap | Module | Risk |
|---|---|---|
| **Payment verification (on-chain)** | `src/payments/mod.rs` | Token transfer receipt parsing and confirmation counting against a real/mock chain are untested. Only the "reject fake proof" path is tested via API. |
| **Pricing engine** | `src/execution_engine/pricing/mod.rs` | Gas cost → USD conversion, markup calculation, and CoinGecko price fetching have no unit tests. |
| **Simulation engine internals** | `src/execution_engine/simulation/mod.rs` | The Alchemy `simulateUserOperationAssetChanges` response parsing is not unit-tested (only tested end-to-end via `/simulate`). |
| **Concurrent request handling** | `src/main.rs` | No stress/load test for the `ConcurrencyLimitLayer` or concurrent worker dequeue behavior. |
| **Graceful shutdown** | `src/main.rs` | SIGTERM/SIGINT handling and in-flight request draining are untested. |
| **Relayer utilities** | `src/relayer/utils.rs` | Helper functions in the relayer utils module have no dedicated tests. |
| **Agent wallet creation edge cases** | `src/agent_wallet/mod.rs` | Wallet creation is tested via the `/wallet` API, but internal edge cases (concurrent creation race, DB encryption failures) are not. |

---

## How to Fill the Gaps

### 1. Worker Execution Loop (`src/worker/mod.rs`)

**Approach:** Mock-based unit tests.

```rust
// Strategy:
// 1. Create a mock queue that returns a pre-built ExecutionJob on dequeue
// 2. Create a mock relayer that returns a fake tx hash on submit
// 3. Create a mock webhook client that records calls
// 4. Run one iteration of the worker loop
// 5. Assert: DB status updated to Confirmed, webhook called with correct payload

#[tokio::test]
async fn test_worker_processes_job_successfully() {
    // Setup: insert a "queued" execution request in the DB
    // Enqueue a matching job in Redis
    // Create mock relayer that returns Ok(tx_hash)
    // Run worker.process_one_job()
    // Assert: DB status = Confirmed, tx_hash populated
}

#[tokio::test]
async fn test_worker_retries_on_transient_failure() {
    // Mock relayer returns Err on first call, Ok on second
    // Assert: attempt count incremented, job re-enqueued
}

#[tokio::test]
async fn test_worker_dead_letters_after_max_retries() {
    // Mock relayer always returns Err
    // Assert: after 3 attempts, job is in DLQ, status = Failed
}
```

**Prerequisite:** The worker's `process_one_job` method needs to accept trait
objects (or be generic over) the relayer and webhook client. Currently it may
call concrete impls directly — you may need to extract traits first:

```rust
#[async_trait]
trait Relayer: Send + Sync {
    async fn submit_user_operation(&self, job: &ExecutionJob) -> Result<String>;
}

#[async_trait]
trait WebhookSender: Send + Sync {
    async fn send(&self, url: &str, payload: &WebhookPayload, secret: &str) -> Result<()>;
}
```

### 2. ERC-4337 UserOperation Building (`src/relayer/erc4337/mod.rs`)

**Approach:** Pure unit tests (no network needed).

```rust
#[test]
fn test_build_user_operation_single_call() {
    // Given: known sender, nonce, target, calldata, gas values
    // Build a UserOperation
    // Assert: callData field encodes execute(target, value, data)
    // Assert: sender matches smart wallet address
    // Assert: nonce is correctly formatted
}

#[test]
fn test_build_user_operation_batch_call() {
    // Given: multiple (target, value, calldata) tuples
    // Assert: callData encodes executeBatch(targets[], values[], datas[])
}

#[test]
fn test_user_operation_hash() {
    // Given: a known UserOp + entry point + chain ID
    // Compute the UserOp hash
    // Assert: matches expected keccak256 output (test vector)
}
```

### 3. Paymaster Signing (`src/relayer/paymaster/mod.rs`)

**Approach:** Unit tests with a known test private key.

```rust
#[test]
fn test_paymaster_signs_valid_hash() {
    // Use a deterministic test key (not random)
    // Build a UserOp hash
    // Sign it with the paymaster signer
    // Recover the signer address from the signature
    // Assert: recovered address matches the test key's address
}

#[test]
fn test_paymaster_and_data_encoding() {
    // Assert: paymasterAndData = paymaster_address ++ validUntil ++ validAfter ++ signature
    // Assert: length is correct (20 + 32 + 32 + 65 = 149 bytes)
}
```

### 4. Webhook Delivery (`src/webhook/mod.rs`)

**Approach:** Use `wiremock` or `httpmock` for HTTP assertions.

```rust
#[tokio::test]
async fn test_webhook_sends_post_with_hmac() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header_exists("X-Webhook-Signature"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;

    send_webhook(&mock_server.uri(), payload, secret).await.unwrap();
    // Assert: mock received exactly 1 request
    // Assert: X-Webhook-Signature header matches expected HMAC
}

#[tokio::test]
async fn test_webhook_retries_on_5xx() {
    // First 2 responses: 500, third: 200
    // Assert: 3 total requests made
}

#[tokio::test]
async fn test_webhook_rejects_http_url() {
    // callback_url = "http://..." (not https)
    // Assert: webhook is not sent (HTTPS-only enforcement)
}
```

### 5. Pricing Engine (`src/execution_engine/pricing/mod.rs`)

**Approach:** Unit tests with mocked price feeds.

```rust
#[test]
fn test_gas_cost_to_usd_conversion() {
    // Given: gas_used = 100_000, gas_price = 20 gwei, eth_price = $3000
    // Assert: cost_usd = 100_000 * 20e-9 * 3000 = $6.00
    // Assert: with 10% markup = $6.60
    // Assert: with $0.01 platform fee = $6.61
}

#[tokio::test]
async fn test_price_cache_ttl() {
    // Fetch price → cache hit → wait for TTL → cache miss → re-fetch
}
```

### 6. End-to-End On-Chain Execution

**Approach:** Testnet integration test (requires funded paymaster).

```rust
#[tokio::test]
#[ignore] // Run manually: cargo test test_e2e -- --ignored
async fn test_e2e_execute_on_sepolia() {
    // Prerequisites:
    //   - Paymaster funded on EntryPoint (≥0.01 ETH deposit)
    //   - Paymaster signer registered
    //   - API key created
    //
    // 1. POST /simulate → get gas estimate
    // 2. POST /execute with valid payment proof (or auth disabled)
    // 3. Poll GET /status/:id until Confirmed or Failed
    // 4. Assert: status = Confirmed, tx_hash is present
    // 5. Verify tx on Etherscan/Alchemy
}
```

> Mark this test `#[ignore]` so it doesn't run in CI without explicit opt-in.
> It costs real testnet gas.

---

## Test Architecture

```
tests/integration_tests.rs
│
├── Helper: test_config()        — builds AppConfig from env vars
├── Helper: build_test_app()     — constructs full Axum router with real DB/Redis
├── Helper: create_test_api_key()— inserts an API key and returns the raw key
│
├── HTTP integration tests       — use axum::test to send requests through the full stack
│   ├── Health (2)
│   ├── Auth (3)
│   ├── Admin (3)
│   ├── Wallet (6)
│   ├── Simulate (7)
│   ├── Execute (6)
│   ├── Status (3)
│   ├── Calldata validation (3)
│   ├── Chain aliases (1)
│   └── Body size limit (1)
│
├── Unit tests (sync)            — no server, test individual functions
│   ├── Chain parsing (4)
│   ├── Encryption (3)
│   ├── Rate limiter (4)
│   ├── Serialization (3)
│   ├── Config (1)
│   └── HMAC (2)
│
└── Infrastructure tests (async) — test against real PostgreSQL / Redis
    ├── Queue (3)
    └── Database (5)
```

---

## Summary

| Category | Tests | Type |
|---|---|---|
| Health endpoint | 2 | HTTP integration |
| API key auth | 3 | HTTP integration |
| Admin management | 3 | HTTP integration |
| Wallet provisioning | 6 | HTTP integration |
| Simulation | 7 | HTTP integration |
| Execution (x402 flow) | 6 | HTTP integration |
| Status polling | 3 | HTTP integration |
| Calldata validation | 3 | HTTP integration |
| Chain aliases | 1 | HTTP integration |
| Body size limit | 1 | HTTP integration |
| Chain parsing/display | 4 | Unit |
| Encryption | 3 | Unit |
| Rate limiter | 4 | Unit |
| Queue (Redis) | 3 | Infrastructure |
| Database (Postgres) | 5 | Infrastructure |
| Serialization | 3 | Unit |
| Config loading | 1 | Unit |
| Webhook HMAC | 2 | Unit |
| **Total** | **60** | |

**Coverage by source module:**

| Source Module | Tested? | Notes |
|---|---|---|
| `src/api/routes/mod.rs` | ✅ Thorough | All handlers tested via HTTP |
| `src/api/middleware/mod.rs` | ✅ Thorough | API key + x402 middleware tested |
| `src/config/mod.rs` | ✅ Basic | Config parsing from env |
| `src/db/mod.rs` | ✅ Good | CRUD operations + constraints |
| `src/db/models/mod.rs` | ✅ Indirect | Serialization round-trips |
| `src/queue/mod.rs` | ✅ Good | Enqueue, dequeue, recovery, DLQ |
| `src/rate_limit/mod.rs` | ✅ Good | Burst, independence, eviction |
| `src/types/mod.rs` | ✅ Good | Parsing, display, serde |
| `src/agent_wallet/mod.rs` | ✅ Partial | Via `/wallet` API + encryption unit tests |
| `src/webhook/mod.rs` | ⚠️ Partial | Only HMAC computation — delivery untested |
| `src/payments/mod.rs` | ⚠️ Partial | Only rejection paths via API |
| `src/execution_engine/simulation/mod.rs` | ⚠️ Partial | Via `/simulate` API only |
| `src/execution_engine/pricing/mod.rs` | ❌ None | No tests |
| `src/worker/mod.rs` | ❌ None | No tests |
| `src/relayer/erc4337/mod.rs` | ❌ None | No tests |
| `src/relayer/paymaster/mod.rs` | ❌ None | No tests |
| `src/relayer/utils.rs` | ❌ None | No tests |
| `src/relayer/mod.rs` | ❌ None | No tests |
| `src/main.rs` | ⚠️ Partial | Router tested via integration; shutdown untested |
