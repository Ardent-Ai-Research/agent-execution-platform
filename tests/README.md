# Test Suite — Agent Execution Platform

This document reflects the **current** test organization and coverage after the test-layout refactor:

- `tests/integration_tests.rs` contains **external end-to-end API behavior tests only**.
- Internal behavior, serialization, helpers, and module-level logic are tested in per-module `#[cfg(test)]` blocks under `src/**`.

## Current Test Inventory

- **Integration tests (`tests/integration_tests.rs`)**: `35`
- **Module/unit tests (`src/**/mod.rs`)**: `37`
- **Total**: `72`

## How To Run

### Prerequisites

- Docker services up for shared infra:
  - PostgreSQL
  - Redis
- Valid `.env` (same env used by app startup)

### Commands

```bash
# Start infra
docker compose up -d

# Integration-only (external API behavior)
cargo test --test integration_tests -- --test-threads=1

# Module/unit tests (internal behavior)
cargo test --lib -- --test-threads=1

# Full suite
cargo test -- --test-threads=1
```

`--test-threads=1` is recommended because multiple tests share DB/Redis state.

## Integration Coverage (External Behavior Only)

Source: `tests/integration_tests.rs`

### HTTP API behavior

- **Health** (`2`)
  - `GET /health` success shape and auth behavior
- **API key auth** (`3`)
  - Missing/invalid/valid `X-API-Key`
- **Admin API key endpoints** (`3`)
  - Bearer auth + create key flow
- **Wallet endpoint** (`6`)
  - Deterministic wallet behavior, namespace isolation, input validation
- **Simulate endpoint** (`7`)
  - Validation failures + live Sepolia simulation path
- **Execute endpoint / x402 flow** (`6`)
  - 402 response behavior, accepted token metadata, proof rejection paths
- **Status endpoint** (`3`)
  - UUID validation, not-found, existing request retrieval
- **Calldata validation** (`3`)
  - Odd-length, too-short, missing `0x`
- **Chain alias route behavior** (`1`)
- **Request body limit behavior** (`1`)

## Module Coverage (Internal Behavior)

### `src/types/mod.rs` (`7`)

- Chain parsing/display/id logic
- `ExecutionStatus` display
- Serde round-trips for:
  - `ExecutionJob`
  - `UserOperation`
  - `PaymentProof`

### `src/agent_wallet/mod.rs` (`3`)

- Encryption/decryption round-trip
- Wrong-key decrypt failure
- Nonce uniqueness (ciphertext differs for same plaintext)

### `src/rate_limit/mod.rs` (`4`)

- Burst allowance
- Key isolation
- Retry-after behavior
- Stale bucket eviction

### `src/queue/mod.rs` (`3`)

- Enqueue/dequeue/ack flow
- Stale processing recovery
- Dead-letter queue push/length

### `src/db/mod.rs` (`5`)

- API key create + lookup
- Missing key lookup
- Execution request lifecycle updates
- Payment replay protection
- Platform key insert/get/duplicate handling

### `src/config/mod.rs` (`1`)

- Environment config load + Ethereum chain expectations

### `src/relayer/erc4337/mod.rs` (`3`)

- Packed gas field pack/unpack round-trip
- v0.9 paymasterAndData split invariants
- Canonical RPC payload field-shape via mocked JSON-RPC endpoint

### `src/relayer/paymaster/mod.rs` (`2`)

- v0.9 dummy paymasterAndData byte-length/layout invariant
- Signature generation + signer recovery correctness

### `src/webhook/mod.rs` (`5`)

- HMAC determinism/variance
- Delivery success and signature header presence
- Retry behavior across `5xx` responses

### `src/execution_engine/pricing/mod.rs` (`2`)

- Gas→USD conversion formula correctness
- Native price cache TTL refresh behavior

### `src/worker/mod.rs` (`2`)

- Re-enqueue attempt bump behavior
- DLQ transition + failed status update at max attempts

## Coverage Summary by Intent

- **External end-to-end API behavior**: covered in `tests/integration_tests.rs` only.
- **Internal module logic and helper invariants**: covered in per-module tests under `src/**`.

This split is intentional and enforced by structure.

## Remaining Gaps (Updated)

These are the most relevant areas still not deeply covered:

- **`run_worker` orchestration:** Current worker coverage focuses on retry/DLQ helpers; full loop iteration with controlled dependencies is still missing.
- **`src/payments/mod.rs` positive path:** API tests cover malformed/rejected proofs; positive receipt parsing and confirmation logic still need direct tests.
- **`src/relayer/erc4337/mod.rs` depth:** Payload shape/helpers are covered; broader mocked coverage for `estimate_gas`, polling timeouts, and error surfaces is still needed.
- **Webhook policy constraints:** Delivery retries and signature behavior are covered; explicit HTTPS-only callback enforcement tests are pending if policy is enforced.
- **`src/main.rs` lifecycle/load behavior:** Concurrency-limit stress and graceful-shutdown behavior remain untested.

## Recommended Next Tests (No Manual On-Chain E2E)

Per request, manual on-chain E2E remains out of scope here. Highest-value next additions:

- Add a worker harness test that executes one full loop with controllable fakes/mocks.
- Add mocked positive-path payment verification tests for receipt/token-transfer decoding.
- Expand bundler mocked scenarios for estimate/send/receipt error surfaces.
- Add explicit tests if HTTPS-only webhook policy is implemented.
- Add `main.rs` shutdown and concurrency-limit behavior tests.

---

If this README is used as a quality gate, keep it synchronized whenever test counts/categories change.
