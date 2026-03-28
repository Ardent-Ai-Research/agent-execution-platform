# AI Agent Blockchain Execution Platform - Lite Version

A backend that allows **AI agents owning their own wallets** to execute on-chain transactions **without paying gas** while preserving their on-chain identity. Agents sign EIP-712 typed data off-chain; the platform relays it through an [EIP-2771](https://eips.ethereum.org/EIPS/eip-2771) trusted forwarder so the target contract sees `_msgSender() == agent`, not the relayer.

> **Hackathon edition** — synchronous inline execution, in-memory state, no external dependencies (no DB, no Redis). Just `anvil` + `cargo run`.

---

## Architecture

```text
AI Agent (signs EIP-712, never pays gas)
        │
        ▼
┌───────────────────────────────────────────────────────┐
│               Axum HTTP Server (:8080)                │
│                                                       │
│  POST /execute  → validate → simulate → price         │
│                   → build Forwarder.execute() calldata │
│                   → relayer signs outer tx → broadcast  │
│                   → wait for receipt → return           │
│                                                       │
│  POST /simulate → validate → simulate → price         │
│                   → return estimate                    │
│                                                       │
│  GET /status/{id} → lookup in-memory store            │
│  GET /health      → { status: "ok" }                  │
└───────────────────────────────────────────────────────┘
        │
        ▼
┌───────────────────────────────────────────────────────┐
│             Relayer Orchestrator                       │
│  Routes to chain-specific relayer (Ethereum)           │
└───────────────────────────────────────────────────────┘
        │
        ▼
┌───────────────────────────────────────────────────────┐
│             Ethereum Relayer                           │
│  ABI-encode Forwarder.execute(ForwardRequest, sig)     │
│  → EIP-1559 outer tx → sign → broadcast → confirm     │
└───────────────────────────────────────────────────────┘
        │
        ▼
┌───────────────────────────────────────────────────────┐
│             MinimalForwarder (on-chain)                │
│  Verify EIP-712 signature → increment nonce           │
│  → call target with appended sender address            │
└───────────────────────────────────────────────────────┘
        │
        ▼
┌───────────────────────────────────────────────────────┐
│         Target Contract (ERC2771Context)               │
│  _msgSender() = agent address (not relayer)            │
└───────────────────────────────────────────────────────┘
        │
        ▼
   Blockchain (Anvil / Testnet / Mainnet)
```

### Request Lifecycle (`POST /execute`)

```text
validate(req)                →  chain resolution, address/calldata/signature checks
simulate_full(req, chain)    →  eth_call (from=relayer, to=forwarder) — 100% accurate gas
estimate_cost(gas, live)     →  live gas price from node × live ETH/USD + 10% markup + $0.01
build_forwarder_calldata()   →  ABI-encode execute(ForwardRequest, signature)
sign_outer_eip1559_tx()      →  relayer signs tx to forwarder contract
broadcast_and_confirm()      →  send_raw_transaction → poll until mined
return ExecutionResponse     →  { request_id, status, tx_hash, cost_usd }
```

---

## Quick Start

### Prerequisites

- **Rust** (1.75+): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Foundry** (for Anvil + cast + forge): `curl -L https://foundry.paradigm.xyz | bash && foundryup`

### 1. Start a Local Ethereum Node

```bash
anvil
```

This gives you a local Ethereum node on `http://127.0.0.1:8545` with 10 pre-funded accounts (10,000 ETH each).

### 2. Deploy Contracts

```bash
cd contracts

# Install OpenZeppelin (if not already)
forge install OpenZeppelin/openzeppelin-contracts

# Deploy MinimalForwarder
forge create --rpc-url http://127.0.0.1:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
  --broadcast src/MinimalForwarder.sol:MinimalForwarder

# Deploy SimpleStaking with forwarder address
forge create --rpc-url http://127.0.0.1:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
  --broadcast src/SimpleStaking.sol:SimpleStaking \
  --constructor-args <FORWARDER_ADDRESS>
```

### 3. Run the Platform

```bash
cargo run
```

No `.env` file needed — defaults to:

- Anvil default account #0 as relayer (`0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266`)
- RPC at `http://127.0.0.1:8545`
- Server on `http://0.0.0.0:8080`

### 4. Run the Integration Tests

```bash
bash tests/integration_test.sh
```

This automatically:

1. Starts Anvil
2. Deploys `MinimalForwarder` + `SimpleStaking`
3. Builds & starts the platform
4. Agent (Anvil account #1) signs EIP-712 meta-transactions
5. Submits stake/unstake via the API
6. **Proves on-chain that `stakes[agent] == 1 ETH` and `stakes[relayer] == 0`**
7. Tests error paths (bad signatures, reverts, invalid inputs)

---

## API Endpoints

| Method | Path            | Description                            |
|--------|-----------------|----------------------------------------|
| GET    | `/health`       | Health check                           |
| POST   | `/execute`      | Validate → simulate → meta-tx execute  |
| POST   | `/simulate`     | Validate → simulate → return estimate  |
| GET    | `/status/{id}`  | Look up execution status by UUID       |

### Request Body (`/execute` and `/simulate`)

```json
{
  "agent_wallet_address": "0x70997970C51812dc3A010C7d01b50e0d17dc79C8",
  "chain": "ethereum",
  "target_contract": "0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512",
  "calldata": "0x3a4b66f1",
  "value": "1000000000000000000",
  "signature": "0x<65-byte EIP-712 signature>",
  "forwarder_address": "0x5FbDB2315678afecb367f032d93F642f64180aa3",
  "forwarder_nonce": 0,
  "deadline": 0,
  "meta_gas": 200000,
  "strategy_id": "optional-label"
}
```

| Field                  | Required | Description                                        |
|------------------------|----------|----------------------------------------------------|
| `agent_wallet_address` | ✅       | Agent's EOA address (the EIP-712 signer)           |
| `chain`                | ✅       | `"ethereum"`, `"base"`, `"arbitrum"`, `"optimism"` |
| `target_contract`      | ✅       | The contract the agent wants to call               |
| `calldata`             | ✅       | ABI-encoded function call (hex, `0x`-prefixed)     |
| `value`                | ✅       | Wei to send (usually `"0"`)                        |
| `signature`            | ✅       | Agent's EIP-712 signature over the ForwardRequest  |
| `forwarder_address`    | ✅       | Deployed MinimalForwarder address                  |
| `forwarder_nonce`      | ✅       | Agent's current nonce on the forwarder             |
| `deadline`             | ✅       | Unix timestamp expiry (`0` = no expiry)            |
| `meta_gas`             | ✅       | Gas limit for the inner call (default: 300000)     |
| `strategy_id`          |          | Optional label for the request                     |

### Response

```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "confirmed",
  "estimated_gas": 67720,
  "estimated_cost_usd": 8.13,
  "tx_hash": "0xabc123...",
  "message": "meta-tx confirmed in block Some(42)"
}
```

### How the Agent Signs (EIP-712)

The agent signs a typed `ForwardRequest` struct:

```solidity
ForwardRequest {
    from:     address   // agent's address
    to:       address   // target contract
    value:    uint256   // ETH to forward (usually 0)
    gas:      uint256   // gas limit for inner call
    nonce:    uint256   // forwarder nonce for this agent
    deadline: uint48    // expiry (0 = no expiry)
    data:     bytes     // calldata for the target
}
```

With EIP-712 domain:

```solidity
EIP712Domain {
    name:              "MinimalForwarder"
    version:           "1"
    chainId:           <chain_id>
    verifyingContract: <forwarder_address>
}
```

Using `cast` (Foundry):

```bash
# Compute the digest and sign it
SIGNATURE=$(cast wallet sign --no-hash "$DIGEST" --private-key "$AGENT_KEY")
```

See `tests/integration_test.sh` for a complete `sign_forward_request()` bash function.

---

## Configuration

All via environment variables (`.env` supported):

| Variable              | Default                  | Description                     |
|-----------------------|--------------------------|---------------------------------|
| `HOST`                | `0.0.0.0`               | Server bind address             |
| `PORT`                | `8080`                   | Server port                     |
| `ETHEREUM_RPC_URL`    | `http://127.0.0.1:8545` | Ethereum JSON-RPC endpoint      |
| `RELAYER_PRIVATE_KEY` | Anvil account #0         | Hex-encoded relayer signing key |

---

## Project Structure

```text
contracts/
├── src/
│   ├── MinimalForwarder.sol     # EIP-2771 forwarder: verify EIP-712 sig → forward call
│   └── SimpleStaking.sol        # ERC2771Context staking contract (demo target)
├── lib/openzeppelin-contracts/  # OpenZeppelin v5 dependency
└── foundry.toml

src/
├── main.rs                      # Entry point — boot + serve
├── lib.rs                       # Module declarations
├── config/mod.rs                # AppConfig (4 env vars)
├── types/mod.rs                 # Domain types, DTOs, MetaTxParams
├── api/
│   ├── mod.rs
│   ├── routes/mod.rs            # Axum handlers + in-memory store
│   └── services/mod.rs          # Inline: validate → simulate → price → meta-tx execute
├── execution_engine/
│   ├── mod.rs                   # Engine: validate, simulate_full / simulate_inner, price
│   ├── simulation/mod.rs        # Full forwarder sim + inner-call sim, shared ABI encoder
│   └── pricing/mod.rs           # Live gas price (RPC) + live ETH/USD (CoinGecko)
└── relayer/
    ├── mod.rs
    ├── ethereum/mod.rs          # ABI-encode Forwarder.execute() → EIP-1559 → broadcast
    └── orchestrator/mod.rs      # Routes MetaTxParams to chain-specific relayer

tests/
└── integration_test.sh          # Full E2E: deploy, sign EIP-712, execute, verify on-chain
```

---

## How It Works (EIP-2771 Deep Dive)

1. **Agent** has a wallet (private key) but **zero ETH for gas**
2. Agent builds the calldata they want (e.g., `stake()`) and signs an EIP-712 `ForwardRequest`
3. Agent sends `{ calldata, signature, forwarder_nonce, ... }` to `POST /execute`
4. Platform **simulates** the exact `Forwarder.execute()` call via `eth_call` with `from=relayer` — 100% accurate gas including forwarder overhead
5. **Relayer** ABI-encodes `MinimalForwarder.execute(request, signature)` and signs its own EIP-1559 transaction to the forwarder
6. On-chain: **Forwarder** verifies the agent's EIP-712 signature, increments nonce, calls the target with `abi.encodePacked(calldata, agentAddress)`
7. Target contract inherits **`ERC2771Context`** — when called by the trusted forwarder, `_msgSender()` extracts the last 20 bytes = agent's address
8. Result: `stakes[agent] = 1 ETH`, `stakes[relayer] = 0` ✓

---

## License

MIT
