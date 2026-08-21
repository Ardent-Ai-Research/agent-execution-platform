# Getting Started

## 1. Generate an API key

API-key generation is public and self-service.

```bash
BASE_URL="https://api.ardentresearch.xyz"

curl -X POST "$BASE_URL/api-keys" \
  -H 'Content-Type: application/json' \
  -d '{"label":"my-test-agent"}'
```

Store the returned `api_key` immediately. It is shown once and only its SHA-256 hash is retained by the platform.

```bash
export ARDENT_API_KEY="ak_your_api_key"
```

## 2. Choose an agent ID

Choose any stable string, such as `trading-bot-01`. The first authenticated request for a new `(API key, agent_id)` pair provisions its smart wallet automatically.

## 3. First end-to-end path

1. Resolve the wallet with `GET /wallet`.
2. Fund it with assets required by the intended protocol action.
3. Run the matching simulation endpoint.
4. Submit the execution endpoint.
5. Poll `GET /status/:id` or provide a `callback_url`.

Testnet execution is free to API callers and UserOperation gas is sponsored by the configured paymaster. Assets spent by a protocol and protocol-native value such as GMX keeper fees still come from the agent wallet.

## 4. Supported chains

Use `ethereum`, `base`, or `arbitrum` for Ethereum Sepolia, Base Sepolia, and Arbitrum Sepolia respectively.

## 5. Terminal states

Requests terminate as `confirmed`, `reverted`, or `failed`.

See [Agent Integration](agent-integration.md) for CLI, MCP, and OpenAPI options.
