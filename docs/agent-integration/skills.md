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
ardent wallet --agent-id my-agent-001 --chain ethereum
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

The installer automatically registers Ardent in Claude Desktop, ChatGPT Desktop, Cursor, and Windsurf if any are detected. Just restart the app after install.

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

1. Resolve wallet first with `GET /wallet`.
2. Simulate with `POST /simulate`.
3. Execute with `POST /execute`.
4. If `402 payment_required`, pay exact `required_amount_raw` to `payment_address`, then resubmit with `X-Payment-Proof`.
5. Track completion with `GET /status/:id` (or callback webhook if configured).

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
5. Treat `GET /status/:id` as source of truth for terminal result (`confirmed`, `failed`, `reverted`).
