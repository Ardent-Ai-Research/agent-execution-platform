# AI Agent Blockchain Execution Platform

A production-hardened backend that allows **AI agents owning their own wallets** to request on-chain transaction execution through an API. The platform simulates transactions, calculates costs, requires payment via the **x402 protocol** with **real on-chain ERC-20 verification**, then sends transactions through a relayer network that abstracts gas payments.

---

## High-Level Architecture

```
AI Agent (wallet holder)
        │
        ▼
┌───────────────────────────────────────────────────────────┐
│                    Axum HTTP Server                        │
│  ┌─────────┐  ┌───────────┐  ┌────────┐  ┌────────────┐  │
│  │  CORS   │→ │Concurrency│→ │  Body  │→ │   Trace    │  │
│  │  Layer  │  │  Limit    │  │ Limit  │  │   Layer    │  │
│  └─────────┘  └───────────┘  └────────┘  └────────────┘  │
│       │                                                    │
│       ▼                                                    │
│  ┌─────────────┐    ┌──────────────────────────────┐      │
│  │ API Key Auth│ →  │   x402 Payment Middleware     │      │
│  │ (optional)  │    │ (on-chain ERC-20 verification)│      │
│  └─────────────┘    └──────────────────────────────┘      │
│       │                                                    │
│       ▼                                                    │
│  ┌──────────────────────────────────────────────────┐     │
│  │              Route Handlers                       │     │
│  │  /health  /execute  /simulate  /status/{id}       │     │
│  └──────────────────────────────────────────────────┘     │
└───────────────────────────────────────────────────────────┘
        │                                       ▲
        ▼                                       │ poll status
┌────────────────┐   ┌──────────────────┐       │
│ Execution      │   │   PostgreSQL     │───────┘
│ Engine         │   │  (state store)   │
│ validate →     │   └──────────────────┘
│ simulate →     │
│ price          │
└────────────────┘
        │
        ▼
┌────────────────┐   ┌──────────────────┐
│  Redis Queue   │ → │ Background       │ (supervised, auto-restart)
│  (BLMOVE       │   │ Workers          │
│   reliable)    │   └──────────────────┘
└────────────────┘           │
                             ▼
                    ┌────────────────────┐
                    │ Relayer            │
                    │ Orchestrator       │ (retry + backoff)
                    └────────────────────┘
                             │
                             ▼
                    ┌────────────────────┐
                    │ Ethereum Relayer   │
                    │ (EIP-1559, nonce   │
                    │  mutex, receipt    │
                    │  polling)          │
                    └────────────────────┘
                             │
                             ▼
                    ┌────────────────────┐
                    │    Blockchain      │
                    │    Network         │
                    └────────────────────┘
```

---

## Code-Level Architecture: Request Lifecycle

Below is the exact function-call trace from HTTP entry point to on-chain confirmation, referencing every file and function involved.

### Phase 1 — Boot Sequence (`src/main.rs` → `main()`)

```
main()
 ├── AppConfig::from_env()                          [config/mod.rs]
 │    ├── Reads .env via dotenvy
 │    ├── RELAYER_PRIVATE_KEY  → required (fail-hard)
 │    └── PAYMENT_ADDRESS      → required (fail-hard)
 │
 ├── db::create_pool()                              [db/mod.rs]
 ├── db::run_migrations()                           [db/mod.rs → migrations/]
 ├── queue::create_redis_connection()               [queue/mod.rs]
 ├── ExecutionEngine::new()                         [execution_engine/mod.rs]
 │    └── EthPriceCache::new()                      [execution_engine/pricing/mod.rs]
 │
 ├── EthereumRelayer::new()                         [relayer/ethereum/mod.rs]
 ├── RelayerOrchestrator::new().with_ethereum()     [relayer/orchestrator/mod.rs]
 │
 ├── for each worker_id:
 │    ├── queue::recover_stale_jobs()               [queue/mod.rs]  ← LMOVE processing → queue
 │    └── tokio::spawn(worker_supervisor())         [main.rs]
 │
 ├── Router::new()
 │    ├── /health   → routes::health_handler        [api/routes/mod.rs]
 │    ├── /execute  → routes::execute_handler       [api/routes/mod.rs]
 │    ├── /simulate → routes::simulate_handler      [api/routes/mod.rs]
 │    ├── /status/{id} → routes::status_handler     [api/routes/mod.rs]
 │    │
 │    ├── .layer(x402_middleware)                    [payments/mod.rs]
 │    ├── .layer(api_key_middleware)                 [main.rs]
 │    ├── .layer(TraceLayer)
 │    ├── .layer(RequestBodyLimitLayer 1MB)
 │    ├── .layer(ConcurrencyLimitLayer 50)
 │    └── .layer(CorsLayer)
 │
 └── axum::serve(listener, app)
      └── .with_graceful_shutdown(shutdown_signal())
```

### Phase 2 — Inbound Request (`POST /execute`)

```
HTTP Request
 │
 ▼
CorsLayer → ConcurrencyLimitLayer → RequestBodyLimitLayer → TraceLayer
 │
 ▼
api_key_middleware()                                 [main.rs]
 │  Check X-API-Key header (if API_KEY env is set)
 │  └── 401 if missing/invalid
 │
 ▼
x402_middleware()                                    [payments/mod.rs]
 │  if X-Payment-Proof header present:
 │  └── verify_payment_on_chain()                   [payments/mod.rs]
 │       ├── 1. Parse PaymentProofHeader (JSON)
 │       ├── 2. Validate token is in config.accepted_tokens
 │       ├── 3. db::payment_tx_hash_exists()        [db/mod.rs]  ← replay check
 │       ├── 4. provider.get_transaction_receipt()   ← fetch receipt from chain
 │       ├── 5. Verify receipt.status == 1           ← tx succeeded
 │       ├── 6. Verify block confirmations ≥ min     ← finality check
 │       ├── 7. Decode ERC-20 Transfer logs:
 │       │    ├── log.address == expected token contract
 │       │    ├── topic[2] (to) == treasury address
 │       │    ├── topic[1] (from) == declared payer
 │       │    └── data (amount) ≥ required amount
 │       └── 8. Return PaymentProof → req.extensions_mut().insert()
 │  else: pass through (handler will return 402)
 │
 ▼
routes::execute_handler()                           [api/routes/mod.rs]
 │  Extracts: State(AppState), Option<Extension<PaymentProof>>, Json(req)
 │
 └── services::handle_execute()                     [api/services/mod.rs]
      │
      ├── 1. engine.validate(&req)                  [execution_engine/mod.rs]
      │    ├── Chain::from_str_loose()              [types/mod.rs]
      │    ├── Validate wallet address (0x, 42 chars)
      │    ├── Validate target contract (0x, 42 chars)
      │    └── Validate calldata (0x prefix, valid hex, ≥4-byte selector)
      │
      ├── 2. db::insert_execution_request()         [db/mod.rs]
      │    └── INSERT INTO execution_requests ... status='pending'
      │
      ├── 3. engine.simulate(&req, &chain)          [execution_engine/mod.rs]
      │    └── simulation::simulate_transaction()   [execution_engine/simulation/mod.rs]
      │         ├── provider.call() (eth_call)       ← dry run
      │         └── provider.estimate_gas()          ← gas estimate
      │              └── If fails → return SimulationResult { success: false }
      │
      ├── 4. engine.estimate_cost(&chain, gas)      [execution_engine/mod.rs]
      │    └── pricing::calculate_cost()            [execution_engine/pricing/mod.rs]
      │         ├── estimate_effective_gas_price()
      │         │    ├── provider.get_block(Latest)  ← base_fee_per_gas
      │         │    └── provider.request("eth_maxPriorityFeePerGas")
      │         │         → effective = 2 × base_fee + priority_fee
      │         ├── EthPriceCache::get_eth_usd()     ← live CoinGecko feed (TTL cache)
      │         └── total = (gas × eff_price × ETH_USD) × (1 + markup%) + platform_fee
      │
      ├── 5. Payment check:
      │    ├── None → return 402 (PaymentRequired) with amount, tokens, address
      │    └── Some(proof):
      │         ├── Cross-check: proof.amount_usd ≥ calculated cost
      │         ├── db::insert_payment()            [db/mod.rs]
      │         │    └── ON CONFLICT (payment_tx_hash) DO NOTHING  ← atomic replay block
      │         │         └── Returns None if duplicate → reject
      │         └── db::update_execution_status(PaymentVerified)
      │
      ├── 6. gas_limit = gas_estimate × 120%        ← 20% buffer for relayer
      │
      ├── 7. queue::enqueue_job()                   [queue/mod.rs]
      │    └── LPUSH execution_jobs <JSON>           ← Redis
      │
      └── 8. db::update_execution_status(Queued)
           └── Return ExecutionResponse { status: Queued }
```

### Phase 3 — Background Processing (`src/worker/mod.rs`)

```
worker_supervisor()                                 [main.rs]
 │  loop:
 │  ├── tokio::spawn(worker::run_worker())
 │  ├── if panicked → queue::recover_stale_jobs()   [queue/mod.rs]
 │  └── sleep(2s) → restart
 │
 ▼
worker::run_worker()                                [worker/mod.rs]
 │  loop:
 │
 ├── queue::dequeue_job(timeout=5s, worker_id)      [queue/mod.rs]
 │    └── BLMOVE execution_jobs                     ← Redis 6.2+
 │         execution_jobs:processing:{worker_id}
 │         RIGHT LEFT <timeout>
 │    ├── Ok(Some(job)) → continue processing
 │    ├── Ok(None)      → timeout, loop again
 │    └── Err (corrupt payload) → LREM + push to DLQ
 │
 ├── Poison-pill guard:
 │    if job.attempt_count ≥ MAX_JOB_ATTEMPTS (3):
 │    ├── db::update_execution_status(Failed)
 │    └── queue::move_to_dlq()                      [queue/mod.rs]
 │         ├── LREM processing list
 │         └── LPUSH execution_jobs:dead_letter
 │
 ├── db::update_execution_status(Broadcasting)
 │
 ├── tokio::spawn(orchestrator.execute(&job))       ← panic-safe boundary
 │    │
 │    ▼
 │    RelayerOrchestrator::execute()                [relayer/orchestrator/mod.rs]
 │    │  for attempt in 1..=3:
 │    │
 │    └── EthereumRelayer::execute()                [relayer/ethereum/mod.rs]
 │         │
 │         ├── nonce_guard = self.nonce.lock()       ← Mutex (serialized access)
 │         │
 │         └── try_execute_locked()
 │              ├── provider.get_chainid()
 │              ├── next_nonce_locked()
 │              │    ├── Cached? → return n+1
 │              │    └── None? → provider.get_transaction_count(Pending)
 │              │
 │              ├── estimate_eip1559_fees()
 │              │    ├── provider.get_block(Latest) → base_fee
 │              │    ├── provider.request("eth_maxPriorityFeePerGas") → tip
 │              │    └── max_fee = 2 × base_fee + tip
 │              │
 │              ├── Build Eip1559TransactionRequest
 │              │    { to, data, value, gas, max_fee, priority_fee, nonce, chain_id }
 │              │
 │              ├── wallet.sign_transaction()
 │              ├── provider.send_raw_transaction()  ← broadcast to mempool
 │              │
 │              └── wait_for_receipt()               ← poll every 2s, timeout 90s
 │                   ├── receipt.status == 1 → Success (nonce++)
 │                   ├── receipt.status == 0 → Revert (nonce consumed, no retry)
 │                   └── timeout → nonce cache invalidated
 │
 │    Orchestrator retry logic:
 │    ├── success         → return result
 │    ├── "reverted"      → return immediately (no retry)
 │    └── transient error → backoff (500ms, 1s, 2s) → retry with fresh nonce
 │
 ├── On success:
 │    ├── db::update_execution_status(Confirmed, tx_hash)
 │    ├── db::insert_transaction()                  [db/mod.rs]
 │    └── queue::ack_job()                          [queue/mod.rs]
 │         └── LREM execution_jobs:processing:{worker_id}
 │
 ├── On revert:
 │    ├── db::update_execution_status(Reverted, tx_hash, error)
 │    └── queue::ack_job()                          ← terminal, no retry
 │
 └── On transient failure:
      └── re_enqueue_with_bump()                    [worker/mod.rs]
           ├── queue::ack_job()                     ← remove original from processing
           ├── job.attempt_count += 1
           ├── if count ≥ MAX_ATTEMPTS:
           │    ├── queue::push_to_dlq()            ← LPUSH to DLQ (already acked)
           │    └── db::update_execution_status(Failed, "exhausted N attempts")
           └── else:
                └── queue::enqueue_job(&bumped)     ← LPUSH back to main queue
```

### Phase 4 — Status Polling (`GET /status/{id}`)

```
routes::status_handler()                            [api/routes/mod.rs]
 ├── Parse UUID from path
 └── db::get_execution_request()                    [db/mod.rs]
      └── SELECT * FROM execution_requests WHERE id = $1
           └── Return StatusResponse { status, tx_hash, cost_usd, ... }
```

### Phase 5 — Health Check (`GET /health`)

```
routes::health_handler()                            [api/routes/mod.rs]
 ├── sqlx::query("SELECT 1").execute(&pool)         ← ping Postgres
 ├── redis::cmd("PING").query_async()               ← ping Redis
 └── Return:
      ├── 200 { status: "ok", checks: { database: "ok", redis: "ok" } }
      └── 503 { status: "degraded", checks: { ... } }  ← if either fails
```

---

### Data Flow Diagram (Redis Queue)

```
                        ┌─────────────────────┐
    LPUSH ─────────────▶│   execution_jobs     │◀──── re-enqueue (retry)
                        │   (main queue)       │◀──── LMOVE (stale recovery)
                        └──────────┬──────────┘
                                   │
                            BLMOVE │ RIGHT→LEFT  (atomic)
                                   ▼
                        ┌─────────────────────────────────┐
                        │ execution_jobs:processing:{wid}  │
                        │ (per-worker processing list)     │
                        └──────────┬──────────────────────┘
                                   │
                     ┌─────────────┼─────────────┐
                     │             │             │
                   LREM          LREM          LREM
                  (ack)     + LPUSH DLQ    + LPUSH main
                     │        (poison)      (retry bump)
                     ▼             │             │
                  ✓ Done           ▼             ▼
                        ┌─────────────────────┐
                        │ execution_jobs:      │
                        │ dead_letter (DLQ)    │
                        └─────────────────────┘
```

### Database State Machine

```
                ┌─────────┐
                │ Pending  │
                └────┬─────┘
                     │ simulate + price
                     ▼
           ┌──────────────────┐
           │ PaymentRequired  │ ← HTTP 402 returned to agent
           └────────┬─────────┘
                    │ X-Payment-Proof verified on-chain
                    ▼
           ┌──────────────────┐
           │ PaymentVerified  │
           └────────┬─────────┘
                    │ enqueued to Redis
                    ▼
              ┌──────────┐
              │  Queued   │
              └─────┬─────┘
                    │ worker picks up job
                    ▼
           ┌──────────────────┐
           │  Broadcasting    │
           └────────┬─────────┘
                    │
          ┌─────────┼──────────┐
          │         │          │
          ▼         ▼          ▼
     ┌─────────┐ ┌────────┐ ┌────────────┐
     │Confirmed│ │Reverted│ │  Failed    │
     │  (✓)    │ │(on-chain│ │(exhausted  │
     │         │ │ revert) │ │ retries /  │
     │         │ │         │ │ DLQ)       │
     └─────────┘ └────────┘ └────────────┘
```

---

## Project Structure

```
agent-execution-platform/
├── Cargo.toml
├── docker-compose.yml               # PostgreSQL 16 + Redis 7
├── .env                             # Local dev configuration
├── migrations/
│   ├── 001_init.sql                 # Schema: agents, execution_requests, transactions, payments
│   └── 002_unique_payment_tx_hash.sql # UNIQUE constraint for replay protection
└── src/
    ├── main.rs                      # Entry point — boot, wiring, middleware stack, supervisor
    ├── lib.rs                       # Module declarations
    ├── config/mod.rs                # AppConfig from env vars (fail-hard on secrets)
    ├── types/mod.rs                 # Shared domain types, enums, DTOs
    ├── api/
    │   ├── mod.rs
    │   ├── routes/mod.rs            # Axum handlers: /execute, /simulate, /status/{id}, /health
    │   ├── middleware/mod.rs         # Re-exports x402 middleware
    │   └── services/mod.rs          # Orchestration: validate → simulate → price → pay → enqueue
    ├── execution_engine/
    │   ├── mod.rs                   # Engine: validate → simulate → price (holds provider + cache)
    │   ├── simulation/mod.rs        # eth_call + eth_estimateGas (no silent 21k fallback)
    │   └── pricing/mod.rs           # EIP-1559 fee estimation + live ETH/USD cache (CoinGecko)
    ├── payments/mod.rs              # x402 middleware: real on-chain ERC-20 Transfer verification
    ├── relayer/
    │   ├── mod.rs
    │   ├── ethereum/mod.rs          # EIP-1559 tx, serialized nonce (Mutex), receipt polling
    │   └── orchestrator/mod.rs      # Routes to chain relayer, retries (revert=abort, else=retry)
    ├── queue/mod.rs                 # Redis BLMOVE reliable queue, ack, DLQ, stale recovery
    ├── worker/mod.rs                # Supervised consumers: panic-safe, poison-pill, retry-bump
    └── db/
        ├── mod.rs                   # PgPool + repository functions (atomic insert, COALESCE update)
        └── models/mod.rs            # SQLx FromRow models
```

---

## Tech Stack

| Component          | Technology                | Notes                                  |
|--------------------|---------------------------|----------------------------------------|
| Language           | Rust (edition 2021)       |                                        |
| API Framework      | Axum 0.7                  | Tower middleware stack                 |
| Async Runtime      | Tokio (full features)     |                                        |
| Database           | PostgreSQL 16 (SQLx 0.7)  | Compile-time migrations                |
| Job Queue          | Redis 7 (redis 0.25)      | BLMOVE/LMOVE (Redis 6.2+ required)    |
| Blockchain         | ethers-rs 2               | EIP-1559 transactions                 |
| Price Feed         | CoinGecko API (reqwest)   | TTL-cached, configurable feed URL      |
| Logging            | tracing + tracing-subscriber | Structured, env-filter              |
| HTTP Client        | reqwest 0.12              | rustls-tls, JSON                       |

---

## Security & Reliability Features

| Feature                          | Implementation                                                 |
|----------------------------------|----------------------------------------------------------------|
| **Payment verification**         | Real on-chain ERC-20 `Transfer` log decoding (not mocked)     |
| **Replay protection**            | DB `UNIQUE(payment_tx_hash)` + atomic `ON CONFLICT DO NOTHING`|
| **API authentication**           | Optional `X-API-Key` header (via `API_KEY` env var)            |
| **Request body limit**           | `RequestBodyLimitLayer` — 1 MB max                             |
| **Concurrency limit**            | `ConcurrencyLimitLayer` — default 50 concurrent requests       |
| **Graceful shutdown**            | Catches SIGINT / SIGTERM, drains in-flight requests            |
| **No hardcoded secrets**         | `RELAYER_PRIVATE_KEY` and `PAYMENT_ADDRESS` required           |
| **Reliable queue**               | BLMOVE atomic dequeue into per-worker processing list          |
| **At-least-once delivery**       | Jobs survive crashes — recovered on restart via LMOVE          |
| **Poison-pill protection**       | Max 3 attempts → dead-letter queue                             |
| **Panic-safe workers**           | `tokio::spawn` boundary + supervisor auto-restart              |
| **Serialized nonce**             | Mutex held across sign → broadcast → confirm cycle             |
| **EIP-1559 transactions**        | Protocol-level fee market — no manual gas bumping              |
| **Live price feed**              | CoinGecko ETH/USD with configurable TTL cache                 |
| **Deep health check**            | Pings DB + Redis — returns 503 if degraded                     |
| **Overflow protection**          | u128 arithmetic for gas cost calculation                       |

---

## Getting Started

### Prerequisites

- **Rust** (1.75+): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Docker & Docker Compose** (for PostgreSQL + Redis)
- **Foundry** (optional, for a local Ethereum node): `curl -L https://foundry.paradigm.xyz | bash && foundryup`

### 1. Start Infrastructure

```bash
docker compose up -d
```

This starts:
- PostgreSQL on `localhost:5432` (user: `postgres`, password: `postgres`, db: `agent_exec`)
- Redis 7 on `localhost:6379`

### 2. Configure Environment

Create a `.env` file (or export vars):

```bash
# Required — service will not start without these
RELAYER_PRIVATE_KEY=your_hex_private_key_no_0x_prefix
PAYMENT_ADDRESS=0xYourTreasuryAddress

# Optional — defaults shown
HOST=0.0.0.0
PORT=8080
DATABASE_URL=postgres://postgres:postgres@localhost:5432/agent_exec
REDIS_URL=redis://127.0.0.1:6379
ETHEREUM_RPC_URL=http://127.0.0.1:8545
GAS_PRICE_MARKUP_PCT=10.0
PLATFORM_FEE_USD=0.01
NUM_WORKERS=2
MAX_CONCURRENT_REQUESTS=50
# API_KEY=your-secret-api-key           # Uncomment to enable auth
# CORS_ORIGIN=https://yourdomain.com    # Uncomment for production CORS
# ETH_PRICE_FEED_URL=https://api.coingecko.com/api/v3/simple/price?ids=ethereum&vs_currencies=usd
# ETH_PRICE_CACHE_TTL_SECS=60
# MIN_PAYMENT_CONFIRMATIONS=1
# ACCEPTED_TOKENS=USDC=0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48,USDT=0xdAC17F958D2ee523a2206206994597C13D831ec7
# TOKEN_DECIMALS=USDC=6,USDT=6
```

### 3. Start a Local Ethereum Node (optional)

```bash
# Using Foundry's Anvil
anvil
```

This gives you a local Ethereum node on `http://127.0.0.1:8545` with pre-funded accounts.

### 4. Run the Platform

```bash
cargo run
```

The server starts on `http://0.0.0.0:8080` with:
- 2 background workers (supervised, auto-restart on panic)
- Stale job recovery from any previous crash
- Ethereum relayer connected to the configured RPC

---

## API Endpoints

### `POST /execute`

Submit a transaction for on-chain execution. Requires payment via x402.

**Request:**
```json
{
  "agent_wallet_address": "0x70997970C51812dc3A010C7d01b50e0d17dc79C8",
  "chain": "ethereum",
  "target_contract": "0x5FbDB2315678afecb367f032d93F642f64180aa3",
  "calldata": "0xa9059cbb000000000000000000000000f39fd6e51aad88f6f4ce6ab8827279cfffb92266000000000000000000000000000000000000000000000000000000000000000a",
  "value": "0",
  "strategy_id": "dca-eth-weekly"
}
```

**Response (402 — Payment Required):**
```json
{
  "error": "payment_required",
  "amount_usd": 0.25,
  "accepted_tokens": ["USDC", "USDT"],
  "payment_address": "0x742d35Cc6634C0532925a3b844Bc9e7595f2bD18",
  "chain": "ethereum",
  "request_id": "550e8400-e29b-41d4-a716-446655440000"
}
```

**Re-submit with payment proof:**
```bash
curl -X POST http://localhost:8080/execute \
  -H "Content-Type: application/json" \
  -H 'X-Payment-Proof: {"payer":"0x70997970C51812dc3A010C7d01b50e0d17dc79C8","amount_usd":0.25,"token":"USDC","chain":"ethereum","tx_hash":"0xabc123...def"}' \
  -d '{
    "agent_wallet_address": "0x70997970C51812dc3A010C7d01b50e0d17dc79C8",
    "chain": "ethereum",
    "target_contract": "0x5FbDB2315678afecb367f032d93F642f64180aa3",
    "calldata": "0xa9059cbb0000000000000000000000...",
    "value": "0"
  }'
```

**Response (200 — Queued):**
```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "queued",
  "estimated_gas": 52000,
  "estimated_cost_usd": 0.25,
  "tx_hash": null,
  "message": "execution queued"
}
```

### `POST /simulate`

Dry-run a transaction without payment or execution.

```bash
curl -X POST http://localhost:8080/simulate \
  -H "Content-Type: application/json" \
  -d '{
    "agent_wallet_address": "0x70997970C51812dc3A010C7d01b50e0d17dc79C8",
    "chain": "ethereum",
    "target_contract": "0x5FbDB2315678afecb367f032d93F642f64180aa3",
    "calldata": "0x70a08231000000000000000000000000f39fd6e51aad88f6f4ce6ab8827279cfffb92266",
    "value": "0"
  }'
```

### `GET /status/{request_id}`

Poll execution status.

```bash
curl http://localhost:8080/status/550e8400-e29b-41d4-a716-446655440000
```

**Response:**
```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "confirmed",
  "chain": "ethereum",
  "tx_hash": "0xdef456...",
  "cost_usd": 0.25,
  "created_at": "2026-03-16T12:00:00Z",
  "updated_at": "2026-03-16T12:00:05Z"
}
```

### `GET /health`

Deep health check — pings both PostgreSQL and Redis.

```bash
curl http://localhost:8080/health
```

**Response (200):**
```json
{
  "status": "ok",
  "service": "agent-execution-platform",
  "version": "0.1.0",
  "checks": {
    "database": "ok",
    "redis": "ok"
  }
}
```

**Response (503 — degraded):**
```json
{
  "status": "degraded",
  "service": "agent-execution-platform",
  "version": "0.1.0",
  "checks": {
    "database": "ok",
    "redis": "unreachable"
  }
}
```

---

## x402 Payment Flow

```
Agent                          Platform                         Blockchain
  │                               │                                │
  │  POST /execute (no payment)   │                                │
  │──────────────────────────────▶│                                │
  │                               │── simulate (eth_call)  ───────▶│
  │                               │◀── gas estimate ──────────────│
  │                               │── price (EIP-1559 fees +      │
  │                               │   live ETH/USD)               │
  │  HTTP 402 { amount, tokens }  │                                │
  │◀──────────────────────────────│                                │
  │                               │                                │
  │  ERC-20 transfer (USDC)       │                                │
  │───────────────────────────────┼───────────────────────────────▶│
  │                               │                                │
  │  POST /execute + X-Payment-Proof                               │
  │──────────────────────────────▶│                                │
  │                               │── get_transaction_receipt() ──▶│
  │                               │◀── receipt + Transfer logs ───│
  │                               │── verify: recipient, amount,   │
  │                               │   confirmations, replay        │
  │                               │── db::insert_payment (atomic)  │
  │                               │── queue::enqueue_job (Redis)   │
  │  HTTP 200 { status: queued }  │                                │
  │◀──────────────────────────────│                                │
  │                               │                                │
  │                          [Worker picks up job]                 │
  │                               │── sign EIP-1559 tx ───────────▶│
  │                               │── broadcast ──────────────────▶│
  │                               │◀── receipt (confirmed) ───────│
  │                               │── db: status = confirmed       │
  │                               │                                │
  │  GET /status/{id}             │                                │
  │──────────────────────────────▶│                                │
  │  { status: confirmed,         │                                │
  │    tx_hash: 0x... }           │                                │
  │◀──────────────────────────────│                                │
```

---

## Configuration

All configuration is via environment variables (`.env` supported):

| Variable                    | Default                            | Required | Description                           |
|-----------------------------|------------------------------------|----------|---------------------------------------|
| `RELAYER_PRIVATE_KEY`       | —                                  | **Yes**  | Hex-encoded relayer signing key       |
| `PAYMENT_ADDRESS`           | —                                  | **Yes**  | Platform treasury address             |
| `HOST`                      | `0.0.0.0`                          | No       | Server bind address                   |
| `PORT`                      | `8080`                             | No       | Server port                           |
| `DATABASE_URL`              | `postgres://postgres:postgres@...` | No       | PostgreSQL connection string          |
| `REDIS_URL`                 | `redis://127.0.0.1:6379`          | No       | Redis connection string               |
| `ETHEREUM_RPC_URL`          | `http://127.0.0.1:8545`           | No       | Ethereum JSON-RPC endpoint            |
| `API_KEY`                   | *(unset = auth disabled)*          | No       | API key for `X-API-Key` header auth   |
| `MAX_CONCURRENT_REQUESTS`   | `50`                               | No       | Global concurrency limit              |
| `GAS_PRICE_MARKUP_PCT`      | `10.0`                             | No       | Gas cost markup percentage            |
| `PLATFORM_FEE_USD`          | `0.01`                             | No       | Flat platform fee per tx              |
| `NUM_WORKERS`               | `2`                                | No       | Background worker count               |
| `ETH_PRICE_FEED_URL`        | CoinGecko simple/price             | No       | ETH/USD price feed URL                |
| `ETH_PRICE_CACHE_TTL_SECS`  | `60`                               | No       | Price cache TTL in seconds            |
| `MIN_PAYMENT_CONFIRMATIONS` | `1`                                | No       | Required block confirmations          |
| `ACCEPTED_TOKENS`           | USDC + USDT (mainnet)              | No       | `TOKEN=0xAddr,...` pairs              |
| `TOKEN_DECIMALS`            | `USDC=6,USDT=6`                   | No       | Decimals per token                    |
| `CORS_ORIGIN`               | *(unset = permissive)*             | No       | Restrict CORS to specific origin      |

---

## Extending for Multiple Chains

1. Add a new variant to `Chain` in `src/types/mod.rs`
2. Create a new relayer module in `src/relayer/<chain>/mod.rs`
3. Register it in `RelayerOrchestrator` (see `with_ethereum()` pattern)
4. Add a provider in `ExecutionEngine::provider_for_chain()`

---

## The `msg.sender` Problem
Right now, the relayer signs the transaction with its own private key, so on-chain `msg.sender` = relayer address, not the AI agent's wallet. This breaks the core premise — agents lose their on-chain identity. Three Viable Approaches; A. Pre-signed tx relay (but agent's wallet pays native gas), B. `EIP-2771` Meta-Transactions (relayer pays gas but only limited to target contracts with `ERC-2771` context), C. `ERC-4337` Account Abstraction (agent with smart wallet, bundler/paymaster pays gas, and target contract needs smart wallets + bundler context). 

## License

MIT
