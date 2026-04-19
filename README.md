# AI Agent Blockchain Execution Platform

**Version 1.0** · Rust · ERC-4337 v0.9 · Multi-chain

A production-hardened Rust backend that enables **AI agents to execute on-chain transactions without owning wallets**. The platform auto-provisions **ERC-4337 smart wallets** per agent, simulates transactions, calculates costs, verifies payment via the **x402 protocol** with **real on-chain ERC-20 verification**, then submits transactions through a relayer network with paymaster-sponsored gas.

**Highlights:**
- **Multi-chain** — Ethereum, Base, and BNB Chain with per-chain bundler clients, paymaster signers, and native-token price feeds
- **EntryPoint v0.9** — `PackedUserOperation` fully integrated with batch transaction support (`executeBatch`)
- **Smart wallet provisioning** — on-chain `factory.getAddress()` for deterministic CREATE2 addresses
- **Webhook push notifications** — HMAC-SHA256 signed payloads delivered to agent callback URLs
- **60 integration tests** — covering API routes, middleware, DB, Redis queue, encryption, rate limiting, and on-chain RPC calls
- **Solidity contracts** — SimpleAccountFactory + VerifyingPaymaster, built with Foundry

---

## Table of Contents

- [High-Level Architecture](#high-level-architecture)
- [Key Design Decisions](#key-design-decisions)
- [Execution Path](#execution-path)
- [Project Structure](#project-structure)
- [Request Lifecycle](#request-lifecycle)
- [Tech Stack](#tech-stack)
- [Security & Reliability](#security--reliability)
- [Getting Started](#getting-started)
- [API Reference](#api-reference)
- [Configuration Reference](#configuration-reference)
- [x402 Payment Flow](#x402-payment-flow)
- [ERC-4337 Account Abstraction](#erc-4337-account-abstraction)
- [Webhook Notifications](#webhook-notifications)
- [Solidity Contracts](#solidity-contracts)
- [Database Schema](#database-schema)
- [Redis Queue Design](#redis-queue-design)
- [Extending for Multiple Chains](#extending-for-multiple-chains)
- [Known Limitations & TODO](#known-limitations--todo)
- [License](#license)

---

## High-Level Architecture

```
AI Agent (no wallet needed)
        │
        │  POST /execute { agent_id, chain, target, calldata }
        │  + X-API-Key: ak_xxxx
        ▼
┌───────────────────────────────────────────────────────────────────┐
│                       Axum HTTP Server                            │
│                                                                   │
│  ┌──────┐ ┌────────────┐ ┌──────────┐ ┌───────┐ ┌─────────────┐ │
│  │ CORS │→│ Concurrency│→│Body Limit│→│ Trace │→│  API Key    │ │
│  │      │ │   Limit    │ │  (1 MB)  │ │       │ │  Auth (DB)  │ │
│  └──────┘ └────────────┘ └──────────┘ └───────┘ └──────┬──────┘ │
│                                                         │        │
│                                                         ▼        │
│                                              ┌──────────────────┐│
│                                              │ x402 Payment     ││
│                                              │ Middleware        ││
│                                              │ (on-chain ERC-20 ││
│                                              │  verification)   ││
│                                              └────────┬─────────┘│
│                                                       │          │
│  ┌────────────────────────────────────────────────────▼────────┐ │
│  │                    Route Handlers                            │ │
│  │  GET /health    POST /execute    POST /simulate             │ │
│  │  GET /wallet    GET /status/:id                             │ │
│  └─────────────────────────┬──────────────────────────────────┘ │
└────────────────────────────┼────────────────────────────────────┘
                             │
               ┌─────────────┼──────────────────┐
               ▼             ▼                  ▼
     ┌──────────────┐ ┌────────────┐  ┌─────────────────────┐
     │  Execution   │ │ Agent      │  │    PostgreSQL        │
     │  Engine      │ │ Wallet     │  │  (state store)       │
     │  validate →  │ │ Registry   │  │  api_keys            │
     │  simulate →  │ │ (ERC-4337) │  │  agent_wallets       │
     │  price       │ │ get/create │  │  execution_requests  │
     └──────────────┘ └────────────┘  │  transactions        │
               │                      │  payments            │
               ▼                      └─────────────────────┘
     ┌──────────────┐                          ▲
     │  Redis Queue  │                          │ status updates
     │  (BLMOVE      │                          │
     │   reliable)   │                          │
     └───────┬──────┘                          │
             │                                  │
             ▼                                  │
     ┌────────────────────┐                     │
     │ Background Workers │ (supervised, auto-restart)
     │ (N concurrent)     │─────────────────────┘
     └────────┬───────────┘
              │
       ┌──────┼──────────────────────┐
       ▼                             ▼
     ┌────────────────────────┐  ┌───────────────────────┐
     │ ERC-4337 Bundler Client│  │ Webhook Delivery      │
     │ (UserOperations,       │  │ (HMAC-SHA256 signed,  │
     │  Paymaster signing,    │  │  exponential backoff, │
     │  receipt polling)      │  │  async non-blocking)  │
     └────────────┬───────────┘  └───────────┬───────────┘
                  │                          │
                  ▼                          ▼
┌──────────────────────────────┐  Agent's callback URL
│      Blockchain Network      │  (HTTPS endpoint)
│ (Ethereum, Base, BNB Chain)  │
└──────────────────────────────┘
```

---

## Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| **ERC-4337 over EIP-2771** | Agents get persistent smart wallet identity; paymaster pays gas; works with ANY target contract (no ERC-2771 support required) |
| **Agent-supplied IDs with API key namespacing** | Agents pick their own `agent_id`; the platform combines it with the API key (`{api_key_id}::{agent_id}`) to prevent cross-customer collisions |
| **Walletless agents** | Agents never handle private keys; the platform generates, encrypts (AES-256-GCM), and stores signing keys server-side |
| **x402 payment protocol** | Agents pay per-execution via on-chain ERC-20 transfers; the platform verifies Transfer logs on-chain before queuing |
| **Bundler-authoritative gas pricing** | Gas prices come exclusively from the ERC-4337 bundler (`rundler_getUserOperationGasPrice`) — no node-based EIP-1559 fallback |
| **Redis reliable queue (BLMOVE)** | Atomic dequeue into per-worker processing list; crash recovery via LMOVE; dead-letter queue for poison pills |
| **No gas bumping** | Bundler handles gas pricing for UserOperations; timeout + fresh retry is safer than replacement transactions |

---

## Execution Path

All jobs are executed through the ERC-4337 Account Abstraction pipeline:

```
Worker → load agent signing key from AgentWalletRegistry
       → BundlerClient.build_user_op()
       → PaymasterSigner.sign_paymaster_data() (if configured)
       → AgentWalletRegistry.decrypt_and_sign() + BundlerClient.apply_signature()
       → alchemy_simulateUserOperationAssetChanges() (Alchemy pre-submit check)
       → BundlerClient.submit_and_wait()
       → Bundler → EntryPoint → SimpleAccount.execute() → Target Contract
       → fire_webhook() (async, non-blocking — HMAC-SHA256 signed callback)
```
- `msg.sender` on-chain = agent's smart wallet (preserves agent identity)
- Paymaster pays gas; agent doesn't need native currency for gas
- Requires deployed SimpleAccountFactory + VerifyingPaymaster contracts

---

## Project Structure

```
agent-execution-platform/
├── Cargo.toml                            # Dependencies: axum, ethers, sqlx, redis, tokio, aes-gcm, hmac
├── docker-compose.yml                    # PostgreSQL 16 + Redis 7
├── .env                                  # Local dev configuration
├── contracts/
│   ├── foundry.toml                      # Foundry config: solc 0.8.28, optimizer, remappings
│   ├── SimpleAccountFactory.sol          # CREATE2 factory with SenderCreator guard (v0.9)
│   ├── VerifyingPaymaster.sol            # ECDSA paymaster — validates platform signature
│   └── lib/                              # Git submodules: account-abstraction, openzeppelin-contracts
├── migrations/
│   ├── 001_init.sql                      # Core schema: agents, execution_requests, transactions, payments
│   ├── 002_agent_wallets_and_api_keys.sql # ERC-4337: api_keys, agent_wallets, schema additions
│   ├── 003_webhook_url.sql               # Adds callback_url column for webhook notifications
│   └── 004_platform_keys.sql             # Platform-managed keys (auto-generated paymaster signer)
└── src/
    ├── main.rs                           # Boot sequence, middleware stack, worker supervisor
    ├── lib.rs                            # Module declarations
    │
    ├── config/mod.rs                     # AppConfig — all env vars with fail-hard on secrets
    ├── types/mod.rs                      # Shared types: ExecutionRequest, ExecutionJob,
    │                                     #   UserOperation, ApiKeyContext, PaymentProof, etc.
    │
    ├── agent_wallet/mod.rs               # ERC-4337 wallet registry: generate EOA, derive
    │                                     #   smart wallet via factory.getAddress(), AES-256-GCM encryption
    │
    ├── api/
    │   ├── mod.rs                        # Module re-exports
    │   ├── routes/mod.rs                 # Axum handlers: /execute, /simulate, /status/:id,
    │   │                                 #   /health, /wallet, /admin/api-keys + admin auth middleware
    │   ├── middleware/mod.rs             # Re-exports x402 middleware
    │   └── services/mod.rs              # Orchestration: validate → wallet → simulate → price → pay → queue
    │
    ├── execution_engine/
    │   ├── mod.rs                        # Engine: validate, simulate, price (holds provider + cache)
    │   ├── simulation/mod.rs             # eth_call + eth_estimateGas (no silent fallback)
    │   └── pricing/mod.rs               # Bundler gas price → USD cost (live native-token/USD cache via CoinGecko)
    │
    ├── payments/mod.rs                   # x402 middleware: parse proof header, fetch receipt,
    │                                     #   decode Transfer logs, verify amount/recipient/replay
    │
    ├── rate_limit/mod.rs                 # Per-API-key token-bucket rate limiter (DashMap-backed)
    │
    ├── relayer/
    │   ├── mod.rs                        # Submodule declarations
    │   ├── erc4337/mod.rs                # Bundler client: build/sign/submit UserOperations
    │   ├── paymaster/mod.rs              # Paymaster signer: VerifyingPaymaster signature generation
    │   └── utils.rs                      # Shared hex parsing utilities
    │
    ├── queue/mod.rs                      # Redis: BLMOVE reliable queue, ack, DLQ, stale recovery
    ├── webhook/mod.rs                    # Webhook delivery: HMAC-SHA256 signing, exponential backoff retries
    ├── worker/mod.rs                     # Supervised consumers: panic-safe, poison-pill, retry-bump, webhooks
    └── db/
        ├── mod.rs                        # PgPool + repository functions
        └── models/mod.rs                 # SQLx FromRow models
```

---

## Request Lifecycle

### Phase 1 — Boot Sequence (`main.rs`)

```
main()
 ├── AppConfig::from_env()                           [config/mod.rs]
 ├── db::create_pool() + db::run_migrations()        [db/mod.rs]
 ├── queue::create_redis_connection()                 [queue/mod.rs]
 ├── ExecutionEngine::new()                           [execution_engine/mod.rs]
 │    └── NativeTokenPriceCache::new() per chain       [pricing/mod.rs]
 ├── AgentWalletRegistry::new()                       [agent_wallet/mod.rs]
 ├── BundlerClient::new()                             [relayer/erc4337/mod.rs]
 │    └── validate_entry_point_supported()            (one-time bundler check)
 ├── PaymasterSigner (auto-generated, DB-backed)     [relayer/paymaster/mod.rs]
 ├── recover_stale_jobs() per worker                  [queue/mod.rs]
 ├── spawn worker_supervisor() per worker             [main.rs]
 └── axum::serve() with graceful_shutdown             [main.rs]
```

### Phase 2 — Inbound Request (`POST /execute`)

```
HTTP Request → CORS → ConcurrencyLimit → BodyLimit → Trace
    │
    ▼
api_key_middleware()                                  [main.rs]
 │  SHA-256 hash X-API-Key → lookup in api_keys table
 │  └── 401 if missing/invalid
 │  └── Inject ApiKeyContext { api_key_id, label }
 │
 ▼
x402_middleware()                                     [payments/mod.rs]
 │  if X-Payment-Proof header:
 │    verify_payment_on_chain():
 │      1. Parse JSON header
 │      2. Validate token is accepted
 │      3. Check DB replay (payment_tx_hash)
 │      4. Fetch tx receipt from chain
 │      5. Verify receipt.status == 1
 │      6. Verify block confirmations
 │      7. Decode Transfer logs (from, to, amount)
 │      8. Inject PaymentProof into request
 │  else: pass through
 │
 ▼
routes::execute_handler()                            [api/routes/mod.rs]
 │
 └── services::handle_execute()                      [api/services/mod.rs]
      ├── 1. engine.validate(req)                    — chain, agent_id, contract, calldata
      ├── 2. wallet_registry.get_or_create()         — provision or fetch smart wallet
      ├── 3. db::insert_execution_request()          — status: Pending
      ├── 4. engine.simulate(smart_wallet as from)   — eth_call + eth_estimateGas
      ├── 5. engine.estimate_cost() (+ 100k AA gas)  — bundler gas price × native token/USD + markup
      ├── 6. Payment check:
      │      None → HTTP 402 { amount, tokens, address }
      │      Some → cross-check amount, insert_payment (atomic replay protection)
      ├── 7. queue::enqueue_job(LPUSH)               — includes smart_wallet + eoa_address
      └── 8. Return { status: Queued, request_id }
```

### Phase 3 — Background Processing (`worker/mod.rs`)

```
worker_supervisor() → loop { spawn(run_worker), recover on panic }
    │
    ▼
run_worker() loop:
    ├── queue::dequeue_job(BLMOVE, 5s timeout)
    ├── Poison-pill: attempt_count ≥ 3 → move_to_dlq
    ├── db::update_status(Broadcasting)
    ├── tokio::spawn(execute_erc4337(&job))           — panic-safe
    │    └── BundlerClient::build_user_op()
    │    └── PaymasterSigner::sign_paymaster_data()
    │    └── decrypt_and_sign() + apply_signature()
    │    └── alchemy_simulateUserOperationAssetChanges()
    │    └── BundlerClient::submit_and_wait()
    │         └── Poll UserOperation receipt (120s timeout)
    │
    ├── Success → db::update_status(Confirmed), fire_webhook(), ack_job
    ├── Revert  → db::update_status(Reverted), fire_webhook(), ack_job (terminal)
    ├── DLQ     → db::update_status(Failed), fire_webhook() (exhausted retries)
    └── Failure → re_enqueue_with_bump(attempt+1)
```

### Phase 4 — Status Polling (`GET /status/:id`)

```
routes::status_handler()
 └── db::get_execution_request(id) → StatusResponse { status, tx_hash, cost_usd }
```

---

## Tech Stack

| Component          | Technology                   | Notes                                    |
|--------------------|------------------------------|------------------------------------------|
| Language           | Rust (edition 2021)          |                                          |
| Smart Contracts    | Solidity ^0.8.28 (Foundry)   | SimpleAccountFactory, VerifyingPaymaster |
| API Framework      | Axum 0.7                     | Tower middleware stack                   |
| Async Runtime      | Tokio (full features)        |                                          |
| Database           | PostgreSQL 16 (SQLx 0.7)     | Runtime migrations from `migrations/`    |
| Job Queue          | Redis 7 (redis 0.25)         | BLMOVE/LMOVE (Redis 6.2+ required)      |
| Blockchain         | ethers-rs 2                  | ERC-4337 v0.9 PackedUserOperations       |
| ERC-4337           | EntryPoint v0.9              | Packed gas fields, batch execute support |
| Price Feed         | CoinGecko API (reqwest 0.12) | TTL-cached, configurable feed URL        |
| Encryption         | AES-256-GCM (aes-gcm 0.10)  | Agent signing key encryption at rest     |
| Hashing            | SHA-256 (sha2 0.10)          | API key hashing                          |
| Webhook signing    | HMAC-SHA256 (hmac 0.12)      | Webhook payload authentication           |
| Logging            | tracing + tracing-subscriber | Structured, env-filter                   |

---

## Security & Reliability

| Feature                          | Implementation                                                      |
|----------------------------------|---------------------------------------------------------------------|
| **DB-backed API key auth**       | SHA-256 hashed keys, per-customer isolation, soft-disable support    |
| **Agent wallet encryption**      | AES-256-GCM, random nonces, keys never stored in plaintext, zeroized on drop |
| **Paymaster signer key**         | Auto-generated on first boot, encrypted at rest in DB — never in env vars    |
| **Secret redaction**             | Manual `Debug` impls on `AppConfig` and `AgentWallet` — secrets print as `[REDACTED]` |
| **Minimal key exposure**         | Signing keys decrypted only for the signing operation, then immediately dropped |
| **Payment verification**         | Real on-chain ERC-20 Transfer log decoding (not mocked)             |
| **Replay protection**            | `UNIQUE(payment_tx_hash)` + atomic `ON CONFLICT DO NOTHING`         |
| **Request body limit**           | `RequestBodyLimitLayer` — 1 MB max                                  |
| **Concurrency limit**            | `ConcurrencyLimitLayer` — default 200 concurrent requests           |
| **Per-API-key rate limiting**    | Token bucket (5 rps sustained, 10 burst) — `429` with `Retry-After`  |
| **Graceful shutdown**            | SIGINT / SIGTERM → drain in-flight requests                         |
| **No hardcoded secrets**         | `PAYMENT_ADDRESS` and encryption keys fail-hard on startup          |
| **Reliable queue**               | BLMOVE atomic dequeue into per-worker processing list               |
| **At-least-once delivery**       | Jobs survive crashes — recovered on restart via LMOVE               |
| **Poison-pill protection**       | Max 3 attempts → dead-letter queue + status marked Failed           |
| **Panic-safe workers**           | `tokio::spawn` boundary + supervisor auto-restart with cooldown     |
| **ERC-4337 execution**           | UserOperations submitted through bundler — no EOA nonce management  |
| **Alchemy preflight simulation** | `alchemy_simulateUserOperationAssetChanges` rejects bad UserOps before broadcast |
| **Bundler-native gas pricing**   | `rundler_getUserOperationGasPrice` for accurate fee estimation (no node fallback) |
| **Webhook HMAC signing**         | `X-Webhook-Signature: sha256=<hex>` HMAC-SHA256 over payload, keyed with API key hash |
| **Webhook security**             | HTTPS-only callbacks, no redirect following, exponential backoff (3 retries) |
| **Live price feed**              | CoinGecko native-token/USD with per-chain TTL cache (no hardcoded prices) |
| **Deep health check**            | Pings DB + Redis — returns 503 if either is degraded                |
| **Overflow protection**          | u128 arithmetic for gas cost calculation                            |
| **Namespaced agent isolation**   | `{api_key_id}::{agent_id}` prevents cross-customer wallet access    |

---

## Getting Started

This guide walks you through a complete setup — from zero to a running platform
that can accept API requests. Every step is explicit; nothing is assumed.

### Prerequisites

| Tool | Version | Install |
|------|---------|---------|
| **Rust** | 1.75+ | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| **Docker & Docker Compose** | latest | [docs.docker.com](https://docs.docker.com/get-docker/) |
| **Foundry** | latest | `curl -L https://foundry.paradigm.xyz \| bash && foundryup` |
| **An Alchemy account** | free tier | [alchemy.com](https://www.alchemy.com/) — needed for Sepolia RPC + bundler |

### Step 1 — Clone & Enter the Repo

```bash
git clone https://github.com/yourorg/agent-execution-platform.git
cd agent-execution-platform
```

### Step 2 — Start Infrastructure (PostgreSQL + Redis)

```bash
docker compose up -d
```

This starts:
- **PostgreSQL 16** on `localhost:5432` (user: `postgres`, pass: `postgres`, db: `agent_exec`)
- **Redis 7** on `localhost:6379`

Verify both containers are healthy:

```bash
docker compose ps        # STATUS should show "Up" for both
```

### Step 3 — Build Solidity Contracts


```bash
cd contracts
git submodule update --init --recursive   # REQUIRED: fetches contract dependencies
forge install                            # (optional, updates foundry.toml deps)
forge build                              # compiles SimpleAccountFactory.sol, VerifyingPaymaster.sol
cd ..
```

Build artifacts land in `contracts/out/`. You will need them if you deploy your
own factory / paymaster (see Step 6).

### Step 4 — Generate Secrets

Generate a 256-bit AES encryption key (used to encrypt all wallet private keys at rest):

```bash
openssl rand -hex 32
# Example output: 8492243f002e4d3e730e9e6f68f206438bb945fd037115ffb2417e942cbc6f39
```

Generate an admin bearer token (used to protect the `/admin/api-keys` endpoint):

```bash
openssl rand -hex 32
# Example output: a1b2c3d4e5f6...  (any strong random string works)
```

Save both values — you'll paste them into the `.env` file in the next step.

### Step 5 — Configure Environment

Create a `.env` file in the project root. The example below is configured for
**Ethereum Sepolia testnet** — the recommended starting point.

> **Important:** Replace every `YOUR_...` placeholder with your actual values.

```bash
# ── Required (server refuses to start without these) ─────────────
PAYMENT_ADDRESS=0xYourTreasuryAddress             # receives x402 payments
WALLET_ENCRYPTION_KEY=<output from openssl rand -hex 32>

# ── Admin ────────────────────────────────────────────────────────
# Protects POST /admin/api-keys.  If unset, admin endpoints return 403.
ADMIN_BEARER_TOKEN=<output from openssl rand -hex 32>

# ── Server ───────────────────────────────────────────────────────
HOST=0.0.0.0
PORT=8080

# ── Database & Redis ─────────────────────────────────────────────
DATABASE_URL=postgres://postgres:postgres@localhost:5432/agent_exec
REDIS_URL=redis://127.0.0.1:6379

# ── Blockchain (Sepolia testnet) ─────────────────────────────────
# Each chain is enabled by setting {CHAIN}_RPC_URL.
# At least one chain must be configured.
ETHEREUM_RPC_URL=https://eth-sepolia.g.alchemy.com/v2/YOUR_ALCHEMY_KEY

# ── ERC-4337 Account Abstraction (per-chain) ────────────────────
ETHEREUM_BUNDLER_RPC_URL=https://eth-sepolia.g.alchemy.com/v2/YOUR_ALCHEMY_KEY
ETHEREUM_ENTRY_POINT_ADDRESS=0x433709009B8330FDa32311DF1C2AFA402eD8D009
ETHEREUM_FACTORY_ADDRESS=0xYourDeployedFactoryAddress
ETHEREUM_PAYMASTER_ADDRESS=0xYourDeployedPaymasterAddress

# Uncomment to enable additional chains:
# BASE_RPC_URL=https://base-sepolia.g.alchemy.com/v2/YOUR_KEY
# BASE_BUNDLER_RPC_URL=https://base-sepolia.g.alchemy.com/v2/YOUR_KEY
# BASE_FACTORY_ADDRESS=0x...
# BASE_PAYMASTER_ADDRESS=0x...

# BNB_RPC_URL=https://data-seed-prebsc-1-s1.bnbchain.org:8545
# BNB_BUNDLER_RPC_URL=https://bnb-testnet.g.alchemy.com/v2/YOUR_KEY
# BNB_FACTORY_ADDRESS=0x...
# BNB_PAYMASTER_ADDRESS=0x...

# ── API Key Auth ─────────────────────────────────────────────────
# false = enforce API key on every request (recommended)
# true  = bypass auth entirely (local dev convenience only)
API_KEY_AUTH_DISABLED=false

# ── Pricing ──────────────────────────────────────────────────────
GAS_PRICE_MARKUP_PCT=10.0
PLATFORM_FEE_USD=0.01
# PRICE_CACHE_TTL_SECS=60

# ── Payment Verification ────────────────────────────────────────
MIN_PAYMENT_CONFIRMATIONS=3

# Per-chain accepted payment tokens (Sepolia testnet addresses shown)
ETHEREUM_ACCEPTED_TOKENS=USDC=0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238,USDT=0x33446002f23232873138b6d08003f009e5309323
ETHEREUM_TOKEN_DECIMALS=USDC=6,USDT=6

# ── Workers & Limits ────────────────────────────────────────────
NUM_WORKERS=2
MAX_CONCURRENT_REQUESTS=200
PER_KEY_RATE_LIMIT_RPS=5.0
PER_KEY_RATE_LIMIT_BURST=10.0
# CORS_ORIGIN=https://yourdomain.com
```

### Step 6 — Deploy Contracts (if you haven't already)

If you already have `SimpleAccountFactory` and `VerifyingPaymaster` deployed on
your target chain, skip to Step 7.

Deploy using Foundry's `forge create`:

```bash
# Set your deployer private key (an EOA with Sepolia ETH for gas)
export PRIVATE_KEY=0xYourDeployerPrivateKey
export RPC_URL=https://eth-sepolia.g.alchemy.com/v2/YOUR_ALCHEMY_KEY
export ENTRY_POINT=0x433709009B8330FDa32311DF1C2AFA402eD8D009

# Deploy SimpleAccountFactory
forge create contracts/src/SimpleAccountFactory.sol:SimpleAccountFactory \
  --constructor-args "$ENTRY_POINT" \
  --rpc-url "$RPC_URL" \
  --private-key "$PRIVATE_KEY"

# Deploy VerifyingPaymaster (verifyingSigner is set later — use any address for now)
forge create contracts/src/VerifyingPaymaster.sol:VerifyingPaymaster \
  --constructor-args "$ENTRY_POINT" "$(cast wallet address $PRIVATE_KEY)" \
  --rpc-url "$RPC_URL" \
  --private-key "$PRIVATE_KEY"
```

Copy the deployed addresses into your `.env`:

```bash
ETHEREUM_FACTORY_ADDRESS=0x<factory address from output>
ETHEREUM_PAYMASTER_ADDRESS=0x<paymaster address from output>
```

### Step 7 — Start the Platform

```bash
cargo run
```

On first boot you will see:

```
INFO  running database migrations…
INFO  generated NEW paymaster signer key — register this address in your
      VerifyingPaymaster contract by calling setVerifyingSigner(0x...)
INFO  HTTP server listening on 0.0.0.0:8080
```

The server is now running with:
- Database migrations applied automatically
- 2 supervised background workers (auto-restart on panic)
- Stale job recovery from any previous crash
- Agent wallet registry ready for smart wallet provisioning

> **Copy the paymaster signer address from the log output.** You need it in Step 8.

### Step 8 — Register the Paymaster Signer On-Chain

The platform auto-generates a paymaster signing ECDSA key on first boot:
1. Generated as a fresh keypair
2. Encrypted with `WALLET_ENCRYPTION_KEY` (AES-256-GCM)
3. Stored in the `platform_keys` table (purpose = `paymaster_signer`)

On subsequent boots the same key is loaded — no new key is generated unless you
delete the row from `platform_keys`.

Register the signer with your deployed `VerifyingPaymaster` contract:

```bash
cast send 0xYourPaymasterAddress \
  "setVerifyingSigner(address)" \
  "0xSignerAddressFromLog" \
  --rpc-url "$RPC_URL" \
  --private-key "$PRIVATE_KEY"
```

### Step 9 — Fund the Paymaster on the EntryPoint

The paymaster contract needs an ETH deposit on the EntryPoint to sponsor gas
for UserOperations. Without this deposit, all executions will fail.

```bash
cast send 0x433709009B8330FDa32311DF1C2AFA402eD8D009 \
  "depositTo(address)" \
  "0xYourPaymasterAddress" \
  --value 0.1ether \
  --rpc-url "$RPC_URL" \
  --private-key "$PRIVATE_KEY"
```

> **Note:** The paymaster *signer* EOA does not need any ETH. Gas deposits are
> made to the `EntryPoint` on behalf of the paymaster *contract* address.

### Step 10 — Create Your First API Key

API keys are created through the admin endpoint, which requires **both** your
`X-API-Key` header (if auth is enabled) and the `ADMIN_BEARER_TOKEN`.

If this is your very first key and auth is enabled, temporarily disable it:

```bash
# In .env, set:
API_KEY_AUTH_DISABLED=true
# Restart the server, then:
```

```bash
curl -s -X POST http://localhost:8080/admin/api-keys \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer YOUR_ADMIN_BEARER_TOKEN" \
  -d '{"label": "my-first-key"}' | jq .
```

Response:

```json
{
  "api_key_id": 1,
  "api_key": "ak_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
  "label": "my-first-key",
  "created_at": "2025-01-01T00:00:00Z",
  "message": "Store this API key securely — it will not be shown again."
}
```

**Save the `api_key` value** — it is shown exactly once. Then re-enable auth:

```bash
# In .env, set:
API_KEY_AUTH_DISABLED=false
# Restart the server
```

### Step 11 — Verify Everything Works

Check the health endpoint:

```bash
curl -s http://localhost:8080/health \
  -H "X-API-Key: ak_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx" | jq .
```

Expected response:

```json
{
  "status": "healthy",
  "version": "1.0.0",
  "database": "ok",
  "redis": "ok"
}
```

Simulate a transaction (no on-chain cost):

```bash
curl -s -X POST http://localhost:8080/simulate \
  -H "Content-Type: application/json" \
  -H "X-API-Key: ak_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx" \
  -d '{
    "agent_id": "test-agent",
    "chain": "ethereum",
    "target_contract": "0x0000000000000000000000000000000000000001",
    "calldata": "0x",
    "value": "0"
  }' | jq .
```

This returns a gas estimate and price quote (or a simulation error from the
bundler — either way, the platform is working end-to-end).

### Step 12 — Submit Your First Execution (End-to-End)

A real execution follows the **x402 payment flow**:

1. **First POST /execute** (no payment header) → returns `402 Payment Required`
   with the price, accepted tokens, and `payment_address`
2. **Pay on-chain** — send the required token amount to the `payment_address`
   on the correct chain
3. **Re-POST /execute** with `X-Payment-Proof` header containing the tx hash →
   returns `200` with `request_id` and `status: "queued"`
4. **Poll GET /status/:request_id** to track progress through
   `queued → processing → submitted → confirmed`

```bash
# Step 1: Get price quote
curl -s -X POST http://localhost:8080/execute \
  -H "Content-Type: application/json" \
  -H "X-API-Key: ak_yourkey" \
  -d '{
    "agent_id": "my-bot",
    "chain": "ethereum",
    "target_contract": "0xYourTargetContract",
    "calldata": "0xYourCalldata",
    "value": "0"
  }' | jq .
# → 402 response with amount_usd, accepted_tokens, payment_address

# Step 2: (pay on-chain using your wallet / script)

# Step 3: Re-submit with payment proof
curl -s -X POST http://localhost:8080/execute \
  -H "Content-Type: application/json" \
  -H "X-API-Key: ak_yourkey" \
  -H 'X-Payment-Proof: {"payer":"0xYourAddress","amount_usd":0.25,"token":"USDC","chain":"ethereum","tx_hash":"0xYourPaymentTxHash"}' \
  -d '{
    "agent_id": "my-bot",
    "chain": "ethereum",
    "target_contract": "0xYourTargetContract",
    "calldata": "0xYourCalldata",
    "value": "0"
  }' | jq .
# → 200 { "request_id": "...", "status": "queued" }

# Step 4: Poll status
curl -s http://localhost:8080/status/YOUR_REQUEST_ID \
  -H "X-API-Key: ak_yourkey" | jq .
```

---

## API Reference

### Authentication

**All endpoints** require an `X-API-Key` header (including `/health`):

```
X-API-Key: ak_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
```

API keys are per-customer, stored as SHA-256 hashes in the `api_keys` table. Set `API_KEY_AUTH_DISABLED=true` to bypass auth entirely in local development.

> **Note**: The `/admin/api-keys` endpoint requires **both** the `X-API-Key` header (outer middleware) **and** a `Authorization: Bearer <ADMIN_BEARER_TOKEN>` header (admin middleware).

---

### `POST /execute`

Submit a transaction for on-chain execution. Requires x402 payment.

**Request (single call):**
```json
{
  "agent_id": "my-trading-bot",
  "chain": "ethereum",
  "target_contract": "0x5FbDB2315678afecb367f032d93F642f64180aa3",
  "calldata": "0xa9059cbb000000000000000000000000f39fd6e51aad88f6f4ce6ab8827279cfffb92266000000000000000000000000000000000000000000000000000000000000000a",
  "value": "0",
  "strategy_id": "dca-eth-weekly",
  "callback_url": "https://myagent.example.com/webhook/execution"
}
```

**Request (batch — multiple calls in one UserOperation):**
```json
{
  "agent_id": "my-trading-bot",
  "chain": "ethereum",
  "batch_calls": [
    {
      "target_contract": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
      "value": "0",
      "calldata": "0x095ea7b3..."
    },
    {
      "target_contract": "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D",
      "value": "0",
      "calldata": "0x38ed1739..."
    }
  ],
  "strategy_id": "approve-and-swap",
  "callback_url": "https://myagent.example.com/webhook/execution"
}
```

> **Note:** Provide either `target_contract` + `calldata` (single call → `execute()`) or `batch_calls` (→ `executeBatch()`). Do not mix both.
```

**Response (402 — Payment Required):**
```json
{
  "error": "payment_required",
  "amount_usd": 0.25,
  "accepted_tokens": ["USDC", "USDT"],
  "payment_address": "0x742d35Cc6634C0532925a3b844Bc9e7595f2bD18",
  "smart_wallet_address": "0x1234...abcd",
  "chain": "ethereum",
  "request_id": "550e8400-e29b-41d4-a716-446655440000"
}
```

**Re-submit with payment proof:**
```bash
curl -X POST http://localhost:8080/execute \
  -H "Content-Type: application/json" \
  -H "X-API-Key: ak_yourkey" \
  -H 'X-Payment-Proof: {"payer":"0x70997970...","amount_usd":0.25,"token":"USDC","chain":"ethereum","tx_hash":"0xabc123..."}' \
  -d '{
    "agent_id": "my-trading-bot",
    "chain": "ethereum",
    "target_contract": "0x5FbDB2315678afecb367f032d93F642f64180aa3",
    "calldata": "0xa9059cbb...",
    "value": "0"
  }'
```

**Response (200 — Queued):**
```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "queued",
  "smart_wallet_address": "0x1234...abcd",
  "estimated_gas": 52000,
  "estimated_cost_usd": 0.25,
  "tx_hash": null,
  "message": "execution queued — webhook will deliver result to your callback URL"
}
```

---

### `POST /simulate`

Dry-run a transaction. No payment required, no execution queued.

```bash
curl -X POST http://localhost:8080/simulate \
  -H "Content-Type: application/json" \
  -H "X-API-Key: ak_yourkey" \
  -d '{
    "agent_id": "my-trading-bot",
    "chain": "ethereum",
    "target_contract": "0x5FbDB2315678afecb367f032d93F642f64180aa3",
    "calldata": "0x70a08231000000000000000000000000f39fd6e51aad88f6f4ce6ab8827279cfffb92266",
    "value": "0"
  }'
```

**Response:**
```json
{
  "request_id": "...",
  "status": "pending",
  "smart_wallet_address": "0x1234...abcd",
  "estimated_gas": 26000,
  "estimated_cost_usd": 0.12,
  "tx_hash": null,
  "message": "simulation succeeded"
}
```

---

### `GET /wallet`

Look up (or auto-provision) the agent's smart wallet address. Free — no payment or simulation. Use this to discover the wallet address before funding it with tokens.

```bash
curl "http://localhost:8080/wallet?agent_id=my-trading-bot&chain=ethereum" \
  -H "X-API-Key: ak_yourkey"
```

**Response:**
```json
{
  "agent_id": "my-trading-bot",
  "smart_wallet_address": "0x1234...abcd",
  "deployed": false,
  "message": "Wallet is not yet deployed (counterfactual). You can still safely send ERC-20 tokens and ETH to 0x1234...abcd — the address is deterministic via CREATE2. The wallet contract will be automatically deployed on your first transaction. Tokens sent now will be fully accessible after deployment."
}
```

- `smart_wallet_address` — fund this address with tokens your strategy needs (safe even before deployment — CREATE2 is deterministic)
- `deployed` — `false` means counterfactual; the wallet contract will be deployed on the first UserOperation
- `message` — context-appropriate funding guidance

---

### `GET /status/:request_id`

Poll execution status.

```bash
curl http://localhost:8080/status/550e8400-e29b-41d4-a716-446655440000 \
  -H "X-API-Key: ak_yourkey"
```

**Response:**
```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "confirmed",
  "chain": "ethereum",
  "tx_hash": "0xdef456...",
  "cost_usd": 0.25,
  "created_at": "2026-04-01T12:00:00Z",
  "updated_at": "2026-04-01T12:00:05Z"
}
```

**Status Values:** `pending` → `payment_required` → `payment_verified` → `queued` → `broadcasting` → `confirmed` | `reverted` | `failed`

---

### `GET /health`

Deep health check — pings PostgreSQL and Redis.

```bash
curl http://localhost:8080/health \
  -H "X-API-Key: ak_yourkey"
```

**Response (200):**
```json
{
  "status": "ok",
  "service": "agent-execution-platform",
  "version": "1.0.0",
  "checks": { "database": "ok", "redis": "ok" }
}
```

**Response (503 — Degraded):**
```json
{
  "status": "degraded",
  "service": "agent-execution-platform",
  "version": "1.0.0",
  "checks": { "database": "ok", "redis": "unreachable" }
}
```

---

### `POST /admin/api-keys`

Create a new API key. Requires **both** API key auth and bearer token auth.

**Authentication:** `X-API-Key` (outer middleware) + `Authorization: Bearer <ADMIN_BEARER_TOKEN>` (admin middleware)

```bash
curl -X POST http://localhost:8080/admin/api-keys \
  -H "Content-Type: application/json" \
  -H "X-API-Key: ak_yourkey" \
  -H "Authorization: Bearer your-admin-token" \
  -d '{ "label": "my-trading-bot" }'
```

**Response (201):**
```json
{
  "api_key_id": "a1b2c3d4-...",
  "api_key": "ak_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
  "label": "my-trading-bot",
  "created_at": "2026-04-01T12:00:00Z",
  "message": "Store this API key securely — it will not be shown again."
}
```

> **Note:** The raw `api_key` is returned exactly once. The platform only stores its SHA-256 hash. If the env var `ADMIN_BEARER_TOKEN` is not set, all admin endpoints return `403 Forbidden`.

---

## Configuration Reference

| Variable                    | Default                            | Required | Description                                |
|-----------------------------|------------------------------------|----------|--------------------------------------------|
| `PAYMENT_ADDRESS`           | —                                  | **Yes**  | Platform treasury address for payments     |
| `WALLET_ENCRYPTION_KEY`     | —                                  | **Yes**  | 64-char hex (32-byte AES-256 key)          |
| `HOST`                      | `0.0.0.0`                          | No       | Server bind address                        |
| `PORT`                      | `8080`                             | No       | Server port                                |
| `DATABASE_URL`              | `postgres://postgres:postgres@...` | No       | PostgreSQL connection string               |
| `REDIS_URL`                 | `redis://127.0.0.1:6379`          | No       | Redis connection string                    |
| **Per-chain (prefix: `ETHEREUM_`, `BASE_`, `BNB_`)** | | | |
| `{CHAIN}_RPC_URL`           | —                                  | **Yes¹** | Chain JSON-RPC endpoint (enables chain)    |
| `{CHAIN}_BUNDLER_RPC_URL`   | *(empty)*                          | No       | ERC-4337 bundler URL for chain             |
| `{CHAIN}_ENTRY_POINT_ADDRESS` | `0x43370900...` (v0.9)           | No       | EntryPoint v0.9 contract on chain          |
| `{CHAIN}_FACTORY_ADDRESS`   | *(empty)*                          | No       | SimpleAccountFactory on chain              |
| `{CHAIN}_PAYMASTER_ADDRESS` | *(empty)*                          | No       | VerifyingPaymaster on chain                |
| `{CHAIN}_PRICE_FEED_URL`    | CoinGecko (auto per token)         | No       | Native-token/USD price feed URL            |
| `{CHAIN}_ACCEPTED_TOKENS`   | *(empty)*                          | No       | `TOKEN=0xAddr,...` payment tokens on chain  |
| `{CHAIN}_TOKEN_DECIMALS`    | *(empty)*                          | No       | `TOKEN=N,...` decimals per token on chain   |
| **Legacy (Ethereum-only fallbacks)** | | | |
| `BUNDLER_RPC_URL`           | —                                  | No       | Fallback for `ETHEREUM_BUNDLER_RPC_URL`    |
| `ENTRY_POINT_ADDRESS`       | —                                  | No       | Fallback for `ETHEREUM_ENTRY_POINT_ADDRESS`|
| `ACCOUNT_FACTORY_ADDRESS`   | —                                  | No       | Fallback for `ETHEREUM_FACTORY_ADDRESS`    |
| `PAYMASTER_ADDRESS`         | —                                  | No       | Fallback for `ETHEREUM_PAYMASTER_ADDRESS`  |
| `ETH_PRICE_FEED_URL`        | —                                  | No       | Fallback for `ETHEREUM_PRICE_FEED_URL`     |
| **Global settings** | | | |
| `API_KEY_AUTH_DISABLED`     | `false`                            | No       | Set `true` to bypass API key auth (dev)    |
| `ADMIN_BEARER_TOKEN`        | *(empty)*                          | No       | Bearer token for `/admin/*` endpoints      |
| `MAX_CONCURRENT_REQUESTS`   | `200`                              | No       | Global concurrency limit                   |
| `PER_KEY_RATE_LIMIT_RPS`    | `5.0`                              | No       | Sustained requests/sec per API key (0=off) |
| `PER_KEY_RATE_LIMIT_BURST`  | `10.0`                             | No       | Burst capacity per API key                 |
| `GAS_PRICE_MARKUP_PCT`      | `10.0`                             | No       | Gas cost markup percentage                 |
| `PLATFORM_FEE_USD`          | `0.01`                             | No       | Flat platform fee per execution            |
| `PRICE_CACHE_TTL_SECS`      | `60`                               | No       | Price cache TTL in seconds                 |
| `NUM_WORKERS`               | `2`                                | No       | Background worker count                    |
| `MIN_PAYMENT_CONFIRMATIONS` | `1`                                | No       | Required block confirmations for payment   |
| `CORS_ORIGIN`               | *(unset = permissive)*             | No       | Restrict CORS to specific origin           |

> ¹ At least one chain must have `{CHAIN}_RPC_URL` set.

---

## x402 Payment Flow

```
Agent                          Platform                         Blockchain
  │                               │                                │
  │  POST /execute (no payment)   │                                │
  │──────────────────────────────▶│                                │
  │                               │── simulate (eth_call)  ───────▶│
  │                               │◀── gas estimate ──────────────│
  │                               │── price (bundler gas + native token/USD) │
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
```

---

## ERC-4337 Account Abstraction

### How It Works

1. **Agent Registration**: Agent sends `agent_id` (e.g. `"my-trading-bot"`) via the API. No wallet needed.
2. **Wallet Provisioning**: On first request, the platform:
   - Generates a random EOA signing key
   - Encrypts it with AES-256-GCM and stores in `agent_wallets` table
   - Derives the counterfactual `SimpleAccount` address via on-chain `factory.getAddress()`
3. **Namespaced Isolation**: The wallet key is `{api_key_id}::{agent_id}`, so different customers can use the same `agent_id` without collision.
4. **Execution** (EntryPoint v0.9):
   - Build a `PackedUserOperation` encoding `BaseAccount.execute(target, value, calldata)` (or `executeBatch(Call[])` for batches)
   - Gas limits are packed into `bytes32` fields: `accountGasLimits` and `gasFees`
   - Platform signs paymaster data (VerifyingPaymaster authorizes gas sponsorship)
   - Agent's EOA signs the EIP-712 typed data UserOp hash
   - Submit to bundler → EntryPoint v0.9 → SimpleAccount → Target Contract
5. **Gas Payment**: The VerifyingPaymaster contract pays gas on behalf of the agent. The agent pays the platform via x402 (ERC-20 stablecoin transfer).

### Contracts Required

| Contract | Purpose |
|----------|---------|
| **EntryPoint** (v0.9) | Canonical singleton — already deployed on all major chains at `0x433709009B8330FDa32311DF1C2AFA402eD8D009` |
| **SimpleAccountFactory** | Deploys SimpleAccount proxies via CREATE2 |
| **VerifyingPaymaster** | Checks platform signature, pays gas for approved UserOps |

### Wallet Funding & Asset Responsibility

The x402 payment covers **gas fees + platform margin only**. The agent's smart wallet must hold whatever tokens the transaction's calldata is spending:

| What the platform provides | What the agent provides |
|---------------------------|------------------------|
| Smart wallet (auto-provisioned) | Tokens the strategy needs (ERC-20s, NFTs, etc.) |
| Gas sponsorship (paymaster) | ETH if `msg.value > 0` (native transfers) |
| Transaction submission infra | Strategy logic / calldata |

**Why the platform can't prefund tokens:**
- Calldata is opaque — it could spend any token, any amount
- The platform can't source arbitrary assets (USDC, WETH, LP tokens, NFTs…)
- This matches how any wallet works: you need assets before you can spend them

**How agents fund their wallet:**
1. Call `GET /wallet?agent_id=my-bot` to get the smart wallet address
2. Transfer the needed tokens to that address (from the agent's treasury, a faucet, etc.)
3. Call `POST /execute` — simulation will verify the wallet has sufficient balance

**Safety net:** If the wallet lacks required tokens, `eth_call` simulation reverts *before* any x402 payment is charged. The agent gets a clear error message with their wallet address.

---

## Webhook Notifications

Agents can receive **push notifications** when their execution completes, instead of (or in addition to) polling `/status/{id}`.

### Setup

1. Include `callback_url` in your `POST /execute` request (must be `https://`)
2. When the execution reaches a terminal state (Confirmed, Reverted, or Failed), the platform POSTs a signed JSON payload to your callback URL
3. If delivery fails, the platform retries with exponential backoff (3 attempts: 2s → 4s → 8s)
4. Webhook delivery is **best-effort and non-blocking** — it never delays the worker from processing the next job. Agents should always be prepared to fall back to polling `/status/{id}`.

### Webhook Payload

```json
{
  "event_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "event_type": "execution.completed",
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "confirmed",
  "chain": "ethereum",
  "tx_hash": "0xdef456...",
  "cost_usd": 0.25,
  "error": null,
  "created_at": "2026-04-01T12:00:00Z",
  "completed_at": "2026-04-01T12:00:05Z"
}
```

### HMAC-SHA256 Verification

Every webhook request includes an `X-Webhook-Signature` header:

```
X-Webhook-Signature: sha256=<hex-encoded HMAC>
```

The HMAC is computed over the raw JSON payload body using the SHA-256 hash of your API key as the signing secret. To verify:

1. Compute `signing_secret = SHA256(your_raw_api_key)`
2. Compute `expected = HMAC-SHA256(signing_secret, request_body)`
3. Compare with the signature from the header (use constant-time comparison)

### Retry Behavior

| Attempt | Delay | Total elapsed |
|---------|-------|---------------|
| 1       | —     | immediate     |
| 2       | 2s    | ~2s           |
| 3       | 4s    | ~6s           |
| 4       | 8s    | ~14s          |

After 4 delivery attempts (1 initial + 3 retries), the webhook is abandoned. The execution status is always persisted in the database regardless of webhook delivery success.

### Security

- **HTTPS only**: `callback_url` must use the `https://` scheme (validated on submission)
- **No redirect following**: The webhook HTTP client does not follow redirects
- **Max URL length**: 2048 characters
- **Timeout**: 10s per delivery attempt
- **Non-blocking**: Delivery runs in a separate tokio task

---

## Solidity Contracts

The `contracts/` directory contains the platform's on-chain components, built with **Foundry** (solc 0.8.28, optimizer enabled, Cancun EVM).

### SimpleAccountFactory.sol

- Deploys `SimpleAccount` proxies via `CREATE2` (deterministic addressing)
- Includes an immutable `senderCreator` reference — only the EntryPoint's `SenderCreator` helper can call `createAccount()`, matching the v0.9 security model
- `getAddress(owner, salt)` returns the counterfactual address without deploying

### VerifyingPaymaster.sol

- Extends `BasePaymaster` from the v0.9 account-abstraction library
- Validates a platform-issued ECDSA signature over the UserOp hash using `toEthSignedMessageHash` + `ECDSA.recover`
- Sponsors gas for any UserOperation that carries a valid signature from the configured `verifyingSigner`

### Building


```bash
cd contracts
git submodule update --init --recursive   # REQUIRED: fetches contract dependencies
forge build                              # compile contracts → out/
forge test                               # run tests (if any)
forge script ...                         # deploy scripts
```

### Dependencies (git submodules)

| Library | Version | Path |
|---------|---------|------|
| `eth-infinitism/account-abstraction` | master (v0.9) | `contracts/lib/account-abstraction` |
| `OpenZeppelin/openzeppelin-contracts` | v5.6.1 | `contracts/lib/openzeppelin-contracts` |

---

## Database Schema

### State Machine

```
          ┌─────────┐
          │ Pending  │
          └────┬─────┘
               │ simulate + price
               ▼
     ┌──────────────────┐
     │ PaymentRequired  │ ← HTTP 402
     └────────┬─────────┘
              │ payment verified
              ▼
     ┌──────────────────┐
     │ PaymentVerified  │
     └────────┬─────────┘
              │ enqueued
              ▼
        ┌──────────┐
        │  Queued   │
        └─────┬─────┘
              │ worker picks up
              ▼
     ┌──────────────────┐
     │  Broadcasting    │
     └────────┬─────────┘
              │
    ┌─────────┼──────────┐
    ▼         ▼          ▼
┌─────────┐ ┌────────┐ ┌──────┐
│Confirmed│ │Reverted│ │Failed│
└─────────┘ └────────┘ └──────┘
```

### Tables

| Table | Purpose |
|-------|---------|
| **`api_keys`** | Per-customer API keys (SHA-256 hash stored, never plaintext) |
| **`agent_wallets`** | One row per `{api_key_id}::{agent_id}` — encrypted signing key, EOA, smart wallet address |
| **`execution_requests`** | Full lifecycle: status, gas/cost estimates, tx hash, errors, timestamps |
| **`transactions`** | On-chain tx records linked to execution requests |
| **`payments`** | x402 payment records with `UNIQUE(payment_tx_hash)` for replay protection |
| **`platform_keys`** | Platform-managed keys (e.g. auto-generated paymaster signer) — encrypted at rest |
| **`agents`** | Legacy table from migration 001 (pre-ERC-4337 wallet-based agents) |

---

## Redis Queue Design

```
                    ┌─────────────────────┐
    LPUSH ─────────▶│   execution_jobs     │◀── re-enqueue (retry)
                    │   (main queue)       │◀── LMOVE (stale recovery)
                    └──────────┬──────────┘
                               │
                        BLMOVE │ RIGHT→LEFT  (atomic)
                               ▼
                    ┌───────────────────────────────────┐
                    │ execution_jobs:processing:{wid}    │
                    │ (per-worker processing list)       │
                    └──────────┬────────────────────────┘
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

- **Enqueue**: `LPUSH` onto main queue
- **Dequeue**: `BLMOVE` atomically pops from queue tail → worker's processing list
- **Acknowledge**: `LREM` from processing list after success
- **Recover**: `LMOVE` stale jobs from processing lists back to main queue (at startup)
- **Dead Letter**: Poison-pill jobs (≥3 failures) sent to DLQ for inspection

---

## Extending for Multiple Chains

Multi-chain support is **built in**.  Ethereum, Base, and BNB Chain (BSC) are supported out of the box.

To enable a chain, set its `{CHAIN}_RPC_URL` environment variable (e.g. `BASE_RPC_URL`, `BNB_RPC_URL`).  The platform auto-configures providers, price caches, bundler clients, and paymaster signers for every chain that has an RPC URL.

To add a **new** chain beyond the built-in three:

1. Add a variant to `Chain` in `src/types/mod.rs` (with `chain_id()` and `from_str_loose()`)
2. Add a chain-parsing block in `AppConfig::parse_chains()` in `src/config/mod.rs`
3. Deploy `SimpleAccountFactory` + `VerifyingPaymaster` on the new chain
4. Set the chain's env vars: `{CHAIN}_RPC_URL`, `{CHAIN}_BUNDLER_RPC_URL`, `{CHAIN}_FACTORY_ADDRESS`, etc.

---

## Known Limitations & TODO

- [ ] **End-to-end on-chain test**: A full execution from `POST /execute` → UserOp submitted → on-chain confirmation requires a funded paymaster on a testnet. Currently tested manually, not automated. Tip: create custom stablecoin (i.e. USDT/USDC) for x402 payment verification.
- [ ] **Batch gas estimation**: Batch calls use the same gas estimation path as single calls — may need tuning for large batches.
- [ ] **Worker unit tests**: The background worker, bundler submission, paymaster signing, and webhook delivery paths are not yet covered by automated tests.

---

## License

GNU Affero General Public License v3.0 (`AGPL-3.0`)
