# Endpoint Reference

This page gives a compact endpoint map for quick lookup.

This reference includes hosted user facing endpoints only.

For a CLI alternative to the curl examples below, see [Agent Integration](agent-integration.md).

## Quick setup

```bash
BASE_URL="https://api.ardentresearch.xyz"
API_KEY="your_api_key"
```

## Public endpoints

1. `GET /health`
2. `GET /feed/recent`

### `GET /health`

```bash
curl -X GET "$BASE_URL/health"
```

### `GET /feed/recent`

```bash
curl -X GET "$BASE_URL/feed/recent?limit=12"
```

## Protected endpoints

1. `GET /wallet`
2. `GET /wallet/balance`
3. `POST /simulate`
4. `POST /execute`
5. `GET /status/:id`

Authentication:

```bash
X-API-Key: <api_key>
```

Optional execution payment header (for manual API key users):

```bash
X-Payment-Proof: {"request_id":"your_request_id","payer":"0xYourPayer","token":"USDC","chain":"ethereum","tx_hash":"0xYourPaymentTxHash"}
```

### `GET /wallet`

```bash
curl -X GET "$BASE_URL/wallet?agent_id=my-agent-001&chain=ethereum" \
  -H "X-API-Key: $API_KEY"
```

### `GET /wallet/balance`

```bash
curl -X GET "$BASE_URL/wallet/balance?agent_id=my-agent-001&chain=ethereum" \
  -H "X-API-Key: $API_KEY"
```

### `POST /simulate`

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

### `POST /execute`

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

Manual payment mode re-submit (after `402 Payment Required`):

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

### `GET /status/:id`

```bash
curl -X GET "$BASE_URL/status/your_request_id" \
  -H "X-API-Key: $API_KEY"
```

## Standard content type

For JSON request bodies:

```bash
Content-Type: application/json
```

## Common status codes

1. `200 OK`
2. `400 Bad Request`
3. `401 Unauthorized`
4. `402 Payment Required`
5. `404 Not Found`
6. `429 Too Many Requests`
7. `500 Internal Server Error`
