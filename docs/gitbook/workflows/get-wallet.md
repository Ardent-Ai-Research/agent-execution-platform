# Get Wallet

Use this endpoint to resolve or provision the smart wallet address for a specific agent.

## Endpoint

`GET /wallet`

## Authentication

`X-API-Key` required.

## Query parameters

1. `agent_id` required string.
2. `chain` optional string. Default is `ethereum`.

## Command

```bash
curl -G "$BASE_URL/wallet" \
  -H "X-API-Key: $API_KEY" \
  --data-urlencode "agent_id=my-agent-001" \
  --data-urlencode "chain=ethereum"
```

## Success response

Status code: `200 OK`

Example:

```json
{
  "agent_id": "my-agent-001",
  "smart_wallet_address": "0x1234567890abcdef1234567890abcdef12345678",
  "deployed": false,
  "message": "Wallet is not yet deployed (counterfactual). You can still safely send ERC-20 tokens and native currency to 0x1234567890abcdef1234567890abcdef12345678."
}
```

## CLI equivalent

```bash
ardent wallet --agent-id my-agent-001 --chain ethereum
```

See [Agent Integration](../agent-integration.md) for the one-line installer.

## Practical usage

1. Call this once per new `(api_key, agent_id)` pair.
2. Fund the returned wallet if strategy needs token transfers.
3. Use same `agent_id` consistently so the wallet mapping remains stable.
