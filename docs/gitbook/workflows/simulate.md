# Simulate Transaction

Use simulation to validate execution and estimate payable cost before submitting live execution.

## Endpoint

`POST /simulate`

## Authentication

`X-API-Key` required.

## Command

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

## Success response

Status code: `200 OK`

Example:

```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "pending",
  "smart_wallet_address": "0xAgentSmartWallet",
  "estimated_gas": 52000,
  "estimated_cost_usd": 0.23,
  "tx_hash": null,
  "message": "simulation succeeded"
}
```

## Cost semantics by payment mode

1. Manual includes platform fee.
2. Auto excludes platform fee.
3. Sponsored returns zero payable cost.

## Typical use

1. Call simulate first.
2. Validate gas and cost constraints.
3. Continue to execute only when output is acceptable.
