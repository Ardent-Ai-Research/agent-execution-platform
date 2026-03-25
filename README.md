# AI Agent Blockchain Execution Platform

A backend that allows **AI agents owning their own wallets** to request on-chain transaction execution through an API. The platform simulates transactions, estimates costs, then sends transactions through a relayer that abstracts gas payments.

> **Hackathon edition** — synchronous inline execution, in-memory state, no external dependencies (no DB, no Redis). Just `anvil` + `cargo run`.

---

## Architecture

```
AI Agent (wallet holder)
        │
        ▼
┌───────────────────────────────────────────────────┐
│              Axum HTTP Server (:8080)              │
│                                                    │
│  POST /execute  → validate → simulate → price     │
│                    → sign EIP-1559 tx → broadcast  │
│                    → wait for receipt → return      │
│                                                    │
│  POST /simulate → validate → simulate → price     │
│                    → return estimate                │
│                                                    │
│  GET /status/{id} → lookup in-memory store         │
│  GET /health      → { status: "ok" }               │
└───────────────────────────────────────────────────┘
        │
        ▼
┌───────────────────────────────────────────────────┐
│            Relayer Orchestrator                    │
│  Routes to chain-specific relayer (Ethereum)       │
└───────────────────────────────────────────────────┘
        │
        ▼
┌───────────────────────────────────────────────────┐
│            Ethereum Relayer                        │
│  EIP-1559 tx construction → sign → broadcast      │
│  → wait for on-chain confirmation                  │
└───────────────────────────────────────────────────┘
        │
        ▼
   Blockchain (Anvil / Testnet / Mainnet)
```

### Request Lifecycle (`POST /execute`)

```
validate(req)           →  chain resolution, address/calldata checks
simulate(req, chain)    →  eth_call (dry run) + eth_estimateGas
estimate_cost(gas)      →  hardcoded: gas × 20 gwei × $3500 + 10% markup + $0.01
build_eip1559_tx()      →  nonce, max_fee = 2×base_fee + priority_fee
sign_and_broadcast()    →  wallet.sign_transaction() → send_raw_transaction()
wait_for_receipt()      →  poll until mined
return ExecutionResponse { request_id, status, tx_hash, cost_usd }
```

---

## Quick Start

### Prerequisites

- **Rust** (1.75+): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Foundry** (for Anvil local node): `curl -L https://foundry.paradigm.xyz | bash && foundryup`

### 1. Start a Local Ethereum Node

```bash
anvil
```

This gives you a local Ethereum node on `http://127.0.0.1:8545` with 10 pre-funded accounts (10,000 ETH each).

### 2. Run the Platform

```bash
cargo run
```

That's it. No `.env` file needed — defaults to:
- Anvil default account #0 as relayer (`0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266`)
- RPC at `http://127.0.0.1:8545`
- Server on `http://0.0.0.0:8080`

### 3. Test It

**Health check:**
```bash
curl http://localhost:8080/health
```

**Simulate a transaction (no execution):**
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

**Execute a transaction (sends on-chain via Anvil):**
```bash
curl -X POST http://localhost:8080/execute \
  -H "Content-Type: application/json" \
  -d '{
    "agent_wallet_address": "0x70997970C51812dc3A010C7d01b50e0d17dc79C8",
    "chain": "ethereum",
    "target_contract": "0x5FbDB2315678afecb367f032d93F642f64180aa3",
    "calldata": "0xa9059cbb000000000000000000000000f39fd6e51aad88f6f4ce6ab8827279cfffb92266000000000000000000000000000000000000000000000000000000000000000a",
    "value": "0"
  }'
```

**Check status:**
```bash
curl http://localhost:8080/status/\<request_id_from_above\>
```

---

## API Endpoints

| Method | Path            | Description                          |
|--------|-----------------|--------------------------------------|
| GET    | `/health`       | Health check                         |
| POST   | `/execute`      | Validate → simulate → execute on-chain |
| POST   | `/simulate`     | Validate → simulate → return estimate  |
| GET    | `/status/{id}`  | Look up execution status by UUID     |

### Request Body (`/execute` and `/simulate`)

```json
{
  "agent_wallet_address": "0x...",
  "chain": "ethereum",
  "target_contract": "0x...",
  "calldata": "0x...",
  "value": "0",
  "strategy_id": "optional-label"
}
```

### Response

```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "confirmed",
  "estimated_gas": 52000,
  "estimated_cost_usd": 0.014,
  "tx_hash": "0xabc123...",
  "message": "transaction confirmed in block Some(42)"
}
```

---

## Configuration

All via environment variables (`.env` supported):

| Variable              | Default                          | Description                    |
|-----------------------|----------------------------------|--------------------------------|
| `HOST`                | `0.0.0.0`                        | Server bind address            |
| `PORT`                | `8080`                           | Server port                    |
| `ETHEREUM_RPC_URL`    | `http://127.0.0.1:8545`         | Ethereum JSON-RPC endpoint     |
| `RELAYER_PRIVATE_KEY` | Anvil account #0                 | Hex-encoded signing key        |

---

## Project Structure

```
src/
├── main.rs                      # Entry point — boot + serve
├── lib.rs                       # Module declarations
├── config/mod.rs                # AppConfig (4 env vars)
├── types/mod.rs                 # Domain types, DTOs
├── api/
│   ├── mod.rs
│   ├── routes/mod.rs            # Axum handlers + in-memory store
│   └── services/mod.rs          # Inline: validate → simulate → price → execute
├── execution_engine/
│   ├── mod.rs                   # Engine: validate, simulate, price
│   ├── simulation/mod.rs        # eth_call + eth_estimateGas
│   └── pricing/mod.rs           # Hardcoded ETH/USD + gas price
└── relayer/
    ├── mod.rs
    ├── ethereum/mod.rs          # EIP-1559 tx, sign, broadcast, confirm
    └── orchestrator/mod.rs      # Routes to chain-specific relayer
```

---

## What's Different from Production

This hackathon branch strips the production version (`master`) to its core:

| Feature                  | Production (`master`)            | Hackathon (`hackathon`)        |
|--------------------------|----------------------------------|--------------------------------|
| State store              | PostgreSQL                       | In-memory HashMap              |
| Job queue                | Redis (BLMOVE reliable queue)    | None (inline execution)        |
| Background workers       | Supervised, auto-restart         | None                           |
| Payment verification     | On-chain ERC-20 log decoding     | None (free execution)          |
| Pricing                  | Live CoinGecko + EIP-1559 fees   | Hardcoded ($3500/ETH, 20 gwei) |
| Nonce management         | Mutex-serialized across workers  | Direct `get_transaction_count`  |
| Retry logic              | 3 attempts + exponential backoff | Single attempt                 |
| Dead-letter queue        | Redis DLQ + poison-pill guard    | None                           |
| API authentication       | Optional X-API-Key               | None                           |
| Request limits           | Body limit + concurrency limit   | None                           |
| Health check             | Deep (DB + Redis ping)           | Shallow (always ok)            |
| Infrastructure           | Docker Compose (PG + Redis)      | Just Anvil                     |

---

## License

MIT
