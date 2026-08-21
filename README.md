# Agent Execution Platform V2 (Testnet)

An open-source testnet backend from **Ardent AI Research** for giving AI agents persistent ERC-4337 smart wallets and safely executing onchain actions.

This repository is the deployed Testnet and a public preview of the execution research behind **Jusso**, Ardent AI Research's first product. Jusso is a new generation of autonomous infrastructure for onchain action, and its Beta is coming soon on Base. The existing `ardent` CLI and API identifiers remain unchanged for Testnet compatibility.

This `V2` branch contains the V2 platform. The original release is preserved separately on the `V1` branch.

The platform provisions a wallet for each `(API key, agent_id)` pair, simulates every action, submits it through an ERC-4337 bundler, and sponsors UserOperation gas through a VerifyingPaymaster. Testnet API users are not charged an execution fee.

## What It Includes

- Public, self-service API-key generation with one-time secret disclosure
- Per-key wallet isolation and encrypted signing-key storage
- Counterfactual ERC-4337 smart wallets with first-transaction deployment
- Stateful full-bundle simulation through `eth_estimateUserOperationGas`
- Paymaster-sponsored testnet gas
- Redis-backed execution queue with retries and dead-letter handling
- Signed webhook callbacks
- Per-key request throttling and separate public key-issuance throttling
- Typed integrations for Aave V3, Compound III, GMX V2, Balancer V3, Morpho Blue, and Uniswap V4
- Ethereum Sepolia, Base Sepolia, and Arbitrum Sepolia runtime profiles

## Request Flow

```text
Client
  |
  | POST /api-keys (public, independently rate-limited)
  v
API key stored by client; only SHA-256 hash stored by platform
  |
  | X-API-Key + stable agent_id
  v
Resolve/provision smart wallet
  |
  v
Validate request -> simulate exact execution shape
  |
  v
Queue -> build and sign UserOperation -> paymaster approval
  |
  v
Bundler -> EntryPoint -> agent smart wallet -> target protocol
```

The paymaster covers UserOperation gas. Assets spent by a protocol action still come from the agent smart wallet. Protocol-required native value, such as GMX keeper fees, must also be present in that wallet.

## Quick Start

### 1. Requirements

- Rust stable
- PostgreSQL 16+
- Redis 7+
- Foundry for contract tests and deployments
- RPC and ERC-4337 bundler endpoints for each enabled chain

### 2. Start local infrastructure

```bash
docker compose up -d
```

Install the pinned Solidity dependencies once before compiling contracts:

```bash
cd contracts
forge install --no-git --shallow \
  OpenZeppelin/openzeppelin-contracts@v5.4.0 \
  eth-infinitism/account-abstraction@v0.9.0
cd ..
```

### 3. Configure the environment

Create `.env` locally. It is ignored by Git.

```dotenv
HOST=0.0.0.0
PORT=8080
DATABASE_URL=postgres://postgres:postgres@localhost:5432/agent_exec
REDIS_URL=redis://127.0.0.1:6379

# Generate with: openssl rand -hex 32
WALLET_ENCRYPTION_KEY=<64-hex-character-key>

# Shared deterministic ERC-4337 contracts
EVM_ENTRY_POINT_ADDRESS=0x433709009B8330FDa32311DF1C2AFA402eD8D009
EVM_FACTORY_ADDRESS=0xYourSimpleAccountFactory
EVM_PAYMASTER_ADDRESS=0xYourVerifyingPaymaster

# Enable at least one chain
ETHEREUM_RPC_URL=https://your-sepolia-rpc
ETHEREUM_BUNDLER_RPC_URL=https://your-sepolia-bundler

# Optional additional chains
BASE_RPC_URL=https://your-base-sepolia-rpc
BASE_BUNDLER_RPC_URL=https://your-base-sepolia-bundler
ARBITRUM_RPC_URL=https://your-arbitrum-sepolia-rpc
ARBITRUM_BUNDLER_RPC_URL=https://your-arbitrum-sepolia-bundler

# Optional tokens returned by GET /wallet/balance
ETHEREUM_TRACKED_TOKENS=USDC=0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238
ETHEREUM_TRACKED_TOKEN_DECIMALS=USDC=6
BASE_TRACKED_TOKENS=USDC=0x036CbD53842c5426634e7929541eC2318f3dCF7e
BASE_TRACKED_TOKEN_DECIMALS=USDC=6
ARBITRUM_TRACKED_TOKENS=USDC=0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d
ARBITRUM_TRACKED_TOKEN_DECIMALS=USDC=6

# Authenticated request limits
PER_KEY_RATE_LIMIT_RPS=5
PER_KEY_RATE_LIMIT_BURST=10

# Public key issuance: five keys per client IP per hour by default
PUBLIC_API_KEY_LIMIT=5
PUBLIC_API_KEY_WINDOW_SECS=3600

# Set only behind a trusted proxy that overwrites this header
# PUBLIC_API_KEY_CLIENT_IP_HEADER=x-forwarded-for

MAX_CONCURRENT_REQUESTS=200
NUM_WORKERS=2
CORS_ORIGIN=https://ardentresearch.xyz,https://www.ardentresearch.xyz
```

`PUBLIC_API_KEY_CLIENT_IP_HEADER` is intentionally unset by default. Never trust a forwarding header unless your ingress removes client-supplied values and writes the authoritative client IP.

### 4. Run

```bash
cargo run
```

Database migrations run automatically at startup.

## Generate an API Key

API-key creation is public and does not require an existing credential:

```bash
curl -X POST http://localhost:8080/api-keys \
  -H 'Content-Type: application/json' \
  -d '{"label":"my-test-agent"}'
```

Example response:

```json
{
  "api_key_id": "9abfd5d9-d5a8-4ba2-839f-d25fb0cd126f",
  "api_key": "ak_<one-time-secret>",
  "label": "my-test-agent",
  "created_at": "2026-08-16T12:00:00Z",
  "message": "Store this API key securely - it will not be shown again."
}
```

The raw key contains 256 bits of cryptographic randomness. The server stores only its SHA-256 hash and sends `Cache-Control: no-store` with the creation response.

## Use an Agent Wallet

```bash
export ARDENT_API_KEY='ak_<one-time-secret>'

curl 'http://localhost:8080/wallet?agent_id=research-agent-01&chain=ethereum' \
  -H "X-API-Key: $ARDENT_API_KEY"
```

The same API key can own multiple agents. Use a stable `agent_id` for each agent; changing it provisions a different wallet.

## Simulate and Execute

```bash
curl -X POST http://localhost:8080/simulate \
  -H 'Content-Type: application/json' \
  -H "X-API-Key: $ARDENT_API_KEY" \
  -d '{
    "agent_id":"research-agent-01",
    "chain":"ethereum",
    "target_contract":"0xTargetContract",
    "calldata":"0xEncodedFunctionCall",
    "value":"0"
  }'

curl -X POST http://localhost:8080/execute \
  -H 'Content-Type: application/json' \
  -H "X-API-Key: $ARDENT_API_KEY" \
  -d '{
    "agent_id":"research-agent-01",
    "chain":"ethereum",
    "target_contract":"0xTargetContract",
    "calldata":"0xEncodedFunctionCall",
    "value":"0"
  }'
```

Successful execution requests move through:

```text
pending -> queued -> broadcasting -> confirmed
                                 \-> failed/reverted
```

Every execution is simulated before it can enter the queue.

## Public Endpoints

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/health` | Database and Redis health |
| `GET` | `/feed/recent` | Sanitized recent activity |
| `POST` | `/api-keys` | Generate a self-service API key |

All wallet, simulation, execution, status, and protocol endpoints require `X-API-Key`.

## Important Security Properties

- Raw API keys are returned once and never stored.
- Agent signing keys are AES-256-GCM encrypted at rest.
- API-key namespaces prevent customers from sharing agent wallets accidentally.
- Public key issuance fails closed when Redis throttling is unavailable.
- Callback URLs are restricted to public HTTPS destinations before queueing.
- Webhooks are signed with HMAC-SHA256.
- Execution status is scoped to the API key that created the request.
- UserOperations are simulated before submission.
- Paymaster signing keys are encrypted in PostgreSQL and zeroized after use.

## Paymaster Operations

The platform generates or loads the shared paymaster signer during startup. Register the logged signer address in each deployed `VerifyingPaymaster`, then fund each paymaster's EntryPoint deposit. The signer EOA itself does not need native currency.

Execution is unavailable on a chain without a configured paymaster signer and funded EntryPoint deposit. Configure and fund the paymaster on every enabled chain for the hosted free testnet experience.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --lib

cd contracts
forge fmt --check
forge test
```

Integration tests require PostgreSQL and Redis:

```bash
docker compose up -d
cargo test --test integration_tests -- --test-threads=1
```

## Repository Layout

```text
src/api/                 HTTP routes and orchestration services
src/agent_wallet/        Encrypted agent wallet registry
src/execution_engine/    Validation and simulation
src/relayer/             ERC-4337 bundler and paymaster clients
src/protocols/           Typed protocol adapters and services
src/queue/               Redis queue and recovery
src/worker/              UserOperation execution workers
contracts/src/           SimpleAccountFactory and VerifyingPaymaster
migrations/              Forward-only PostgreSQL migrations
docs/agent-integration/  CLI, MCP tools, and OpenAPI bundles
```

Migrations `001` through `006` are retained as immutable deployment history. Migration `007` removes the retired billing model, and migration `008` adds API-key ownership to execution requests.

## License

AGPL-3.0-only. See [LICENSE](LICENSE).
