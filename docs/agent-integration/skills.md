# AI Agent Blockchain Execution Skills

This file covers everything needed to get started with AI Agent Blockchain Execution API — from initial install through to running autonomous agents. It serves as both a human setup guide and a runtime playbook for LLM agents (ChatGPT, Grok, Claude, etc.).

---

## Part 1 — Human Setup

### 1) Install CLI

```bash
curl -fsSL https://raw.githubusercontent.com/Ardent-Ai-Research/agent-execution-platform/master/docs/agent-integration/install.sh | bash
```

This installs an `ardent` command in `~/.local/bin` and downloads runtime files (MCP server, OpenAPI spec) into `~/.ardent/`.

### 2) Set credentials

```bash
export ARDENT_API_KEY="your_api_key"
```

### 3) Quick commands

```bash
ardent --version
ardent health
ardent feed                             # public activity feed
ardent wallet         --agent-id my-agent-001 --chain ethereum
ardent wallet-balance --agent-id my-agent-001 --chain ethereum
ardent aave-balances --agent-id my-agent-001
ardent gmx-create-order-simulate --agent-id my-agent-001 --body-file ./gmx-order.json
ardent simulate --agent-id my-agent-001 --chain ethereum --target-contract 0xTargetContract --calldata 0xCalldata --value 0
ardent execute --agent-id my-agent-001 --chain ethereum --target-contract 0xTargetContract --calldata 0xCalldata --value 0
ardent status --request-id your_request_id
```

### 4) Manual payment re-submit (after a `402 payment_required` response)

```bash
ardent execute \
  --agent-id my-agent-001 \
  --chain ethereum \
  --target-contract 0xTargetContract \
  --calldata 0xCalldata \
  --value 0 \
  --proof-request-id your_request_id \
  --proof-payer 0xYourPayer \
  --proof-token USDC \
  --proof-chain ethereum \
  --proof-tx-hash 0xYourPaymentTxHash
```

### 5) Update later

```bash
ardent self-update                # updates CLI only
ardent self-update --with-runtime # also refreshes ~/.ardent files
```

### Optional: MCP server for AI tool runtimes

The installer automatically registers Ardent in Codex, Claude Desktop, ChatGPT Desktop, Cursor, and Windsurf if any are detected. Just restart the app after install.

> **After install, open the patched config file and replace the `ARDENT_API_KEY` placeholder with your real key before restarting the app.**
> The installer prints the exact path of every file it patches.

To register manually (or for any other MCP-compatible AI), point it at:

```bash
python3 ~/.ardent/mcp_server.py
```

---

## Part 2 — Agent Playbook

_This section is written for LLM agents that have direct HTTP/tool access to Ardent API._

### Environment

- API base URL: `https://api.ardentresearch.xyz`
- Required auth header: `X-API-Key`
- Optional payment proof header (manual mode): `X-Payment-Proof`

### Variables

```bash
BASE_URL="https://api.ardentresearch.xyz"
API_KEY="your_api_key"
AGENT_ID="my-agent-001"
REQUEST_ID="your_request_id"
```

### Core Execution Flow

1. Resolve wallet with `GET /wallet`.
2. Confirm funding with `GET /wallet/balance` — verify the wallet holds enough tokens before executing.
3. Simulate with `POST /simulate`.
4. Execute with `POST /execute`.
5. If `402 payment_required`, pay exact `required_amount_raw` to `payment_address`, then resubmit with `X-Payment-Proof`.
6. Track completion with `GET /status/:id` (or callback webhook if configured).

### Typed Aave V3 Sepolia Flow

Use typed protocol tools for Aave V3 Sepolia actions instead of manually
encoding calldata. Before changing state, read balances and position:

```bash
curl -X GET "$BASE_URL/protocols/aave-v3/balances?agent_id=$AGENT_ID&chain=ethereum" \
  -H "X-API-Key: $API_KEY"
```

```bash
curl -X GET "$BASE_URL/protocols/aave-v3/position?agent_id=$AGENT_ID&chain=ethereum" \
  -H "X-API-Key: $API_KEY"
```

For supply, the API compiles the request into an atomic `approve -> Pool.supply`
ERC-4337 batch and runs full UserOperation simulation.

```bash
curl -X POST "$BASE_URL/protocols/aave-v3/supply/simulate" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $API_KEY" \
  -d '{
    "agent_id": "my-agent-001",
    "chain": "ethereum",
    "asset": "USDC",
    "amount": "1.25"
  }'
```

```bash
curl -X POST "$BASE_URL/protocols/aave-v3/supply" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $API_KEY" \
  -d '{
    "agent_id": "my-agent-001",
    "chain": "ethereum",
    "asset": "USDC",
    "amount": "1.25"
  }'
```

Use the same typed flow for withdrawing and repaying:

```bash
curl -X POST "$BASE_URL/protocols/aave-v3/withdraw" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $API_KEY" \
  -d '{
    "agent_id": "my-agent-001",
    "chain": "ethereum",
    "asset": "USDC",
    "amount": "max"
  }'
```

```bash
curl -X POST "$BASE_URL/protocols/aave-v3/repay" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $API_KEY" \
  -d '{
    "agent_id": "my-agent-001",
    "chain": "ethereum",
    "asset": "USDC",
    "amount": "max",
    "interest_rate_mode": 2
  }'
```

Read Aave account data before borrowing or making risk-sensitive decisions:

```bash
curl -X GET "$BASE_URL/protocols/aave-v3/position?agent_id=$AGENT_ID&chain=ethereum" \
  -H "X-API-Key: $API_KEY"
```

Borrow uses a projected health-factor guard before simulation/execution. The
default minimum projected health factor is `1.05`; callers can set a stricter
floor with `min_health_factor`, which must be at least `1.0`.

```bash
curl -X POST "$BASE_URL/protocols/aave-v3/borrow/simulate" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $API_KEY" \
  -d '{
    "agent_id": "my-agent-001",
    "chain": "ethereum",
    "asset": "USDC",
    "amount": "max",
    "interest_rate_mode": 2,
    "min_health_factor": "1.10"
  }'
```

Supported assets: `AAVE`, `DAI`, `EURS`, `GHO`, `LINK`, `USDC`, `USDT`,
`WBTC`, `WETH`. Use `amount_raw` instead of `amount` for exact base units.
Withdraw, repay, and borrow support `amount: "max"`. Repay max resolves to the
smaller of selected-rate debt and wallet token balance. Borrow max resolves to
the largest amount allowed by available borrows and the projected health-factor
floor.

### Typed GMX V2 Arbitrum Sepolia Flow

Use typed protocol tools for GMX V2 Arbitrum Sepolia orders instead of manually
encoding `ExchangeRouter` calldata. For create-order actions, the API compiles
`approve -> ExchangeRouter.multicall(sendWnt, sendTokens, createOrder)` and
runs full UserOperation simulation.

Create a market increase simulation:

```bash
curl -X POST "$BASE_URL/protocols/gmx-v2/orders/simulate" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $API_KEY" \
  -d '{
    "agent_id": "my-agent-001",
    "chain": "arbitrum",
    "order_type": "market_increase",
    "market": "0xYourGmxMarketToken",
    "initial_collateral_token": "0xYourCollateralToken",
    "initial_collateral_delta_amount_raw": "1000000",
    "size_delta_usd_raw": "50000000000000000000000000000000000",
    "acceptable_price_raw": "30000000000000000000000000000000000000000",
    "execution_fee_raw": "1000000000000000",
    "is_long": true
  }'
```

Create a market swap simulation:

For `market_swap`, `market` is treated as the single GMX swap-path market. The
platform encodes the on-chain GMX order `market` field as `address(0)`, which is
required for GMX swap orders.

```bash
curl -X POST "$BASE_URL/protocols/gmx-v2/orders/simulate" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $API_KEY" \
  -d '{
    "agent_id": "my-agent-001",
    "chain": "arbitrum",
    "order_type": "market_swap",
    "market": "0xYourGmxMarketToken",
    "initial_collateral_token": "0xYourInputToken",
    "initial_collateral_delta_amount_raw": "1000000",
    "min_output_amount_raw": "1",
    "execution_fee_raw": "1000000000000000"
  }'
```

Cancel an order:

```bash
curl -X POST "$BASE_URL/protocols/gmx-v2/orders/cancel/simulate" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $API_KEY" \
  -d '{
    "agent_id": "my-agent-001",
    "chain": "arbitrum",
    "order_key": "0xYourBytes32OrderKey"
  }'
```

Agents should treat GMX values as raw integers: token amounts use token base
units, while size and price values use GMX 30-decimal precision. The agent smart
wallet must hold the collateral/input token and enough Arbitrum Sepolia ETH for
the GMX execution fee.

### Canonical curl Commands

#### Health

```bash
curl -X GET "$BASE_URL/health"
```

#### Get Wallet

```bash
curl -X GET "$BASE_URL/wallet?agent_id=$AGENT_ID&chain=ethereum" \
  -H "X-API-Key: $API_KEY"
```

#### Get Wallet Balance

```bash
curl -X GET "$BASE_URL/wallet/balance?agent_id=$AGENT_ID&chain=ethereum" \
  -H "X-API-Key: $API_KEY"
```

#### Simulate

```bash
curl -X POST "$BASE_URL/simulate" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $API_KEY" \
  -d '{
    "agent_id": "my-agent-001",
    "chain": "ethereum",
    "target_contract": "0xTargetContract",
    "calldata": "0xCalldata",
    "value": "0"
  }'
```

#### Execute

```bash
curl -X POST "$BASE_URL/execute" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $API_KEY" \
  -d '{
    "agent_id": "my-agent-001",
    "chain": "ethereum",
    "target_contract": "0xTargetContract",
    "calldata": "0xCalldata",
    "value": "0"
  }'
```

#### Execute Re-submit (Manual Mode After 402)

```bash
curl -X POST "$BASE_URL/execute" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $API_KEY" \
  -H 'X-Payment-Proof: {"request_id":"your_request_id","payer":"0xYourPayer","token":"USDC","chain":"ethereum","tx_hash":"0xYourPaymentTxHash"}' \
  -d '{
    "agent_id": "my-agent-001",
    "chain": "ethereum",
    "target_contract": "0xTargetContract",
    "calldata": "0xCalldata",
    "value": "0"
  }'
```

#### Status

```bash
curl -X GET "$BASE_URL/status/$REQUEST_ID" \
  -H "X-API-Key: $API_KEY"
```

### Guardrails For Agents

1. Always use placeholders like `your_request_id` in generated examples.
2. Never invent token amounts for manual payment; always read `required_amount_raw` from the `402` response.
3. Never change `request_id`, `payment_address`, or `chain` between `402` and re-submit.
4. Prefer `simulate` before `execute` when action safety is uncertain.
5. Use `GET /wallet/balance` to verify sufficient token balance before attempting execution — do not assume the wallet is funded.
6. Treat `GET /status/:id` as source of truth for terminal result (`confirmed`, `failed`, `reverted`).
