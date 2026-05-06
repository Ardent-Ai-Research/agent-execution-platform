# Execute Transaction

This endpoint submits a transaction request for execution.

## Endpoint

`POST /execute`

## Authentication

1. `X-API-Key` required.
2. `X-Payment-Proof` required only when needed by payment mode and payment state.

## Request shapes

Two request shapes are supported.

### Single call shape

```json
{
  "agent_id": "my-agent-001",
  "chain": "ethereum",
  "target_contract": "0xTargetContract",
  "calldata": "0xCalldata",
  "value": "0",
  "strategy_id": "optional-strategy-id",
  "callback_url": "https://your-system.example.com/webhook/execution"
}
```

### Batch call shape

```json
{
  "agent_id": "my-agent-001",
  "chain": "ethereum",
  "batch_calls": [
    {
      "target_contract": "0xToken",
      "value": "0",
      "calldata": "0xApproveCalldata"
    },
    {
      "target_contract": "0xDex",
      "value": "0",
      "calldata": "0xSwapCalldata"
    }
  ],
  "strategy_id": "approve-and-swap",
  "callback_url": "https://your-system.example.com/webhook/execution"
}
```

Use one shape at a time for clean client behavior.

## Testnet payment tokens on Ethereum Sepolia

Current accepted x402 payment tokens in the hosted testnet environment:

1. `USDC` at `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238`
2. `USDT` at `0xd077A400968890Eacc75cdc901F0356c943e4fDb`
3. `aUSD` at `0x112a19d6236016fc4dda49257c724E63a3CE5bEA`

Always trust live `402` response fields (`accepted_tokens`, `required_amount_raw`, and `payment_address`) over static assumptions.
The exact token list in a `402` response can vary by your assigned environment and policy.

## First execute command

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

## Manual mode payment required response

Status code: `402 Payment Required`

Example:

```json
{
  "error": "payment_required",
  "amount_usd": 0.25,
  "accepted_tokens": ["USDC", "USDT"],
  "required_amount_raw": {
    "USDC": "250000",
    "USDT": "250000"
  },
  "payment_address": "0xTreasuryAddress",
  "chain": "ethereum",
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "smart_wallet_address": "0xAgentSmartWallet"
}
```

## Re submit with payment proof

```bash
curl -X POST "$BASE_URL/execute" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $API_KEY" \
  -H 'X-Payment-Proof: {"request_id":"550e8400-e29b-41d4-a716-446655440000","payer":"0xYourPayer","amount_usd":0.25,"token":"USDC","chain":"ethereum","tx_hash":"0xYourPaymentTxHash"}' \
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
  "status": "queued",
  "smart_wallet_address": "0xAgentSmartWallet",
  "estimated_gas": 52000,
  "estimated_cost_usd": 0.25,
  "tx_hash": null,
  "message": "execution queued"
}
```

## Notes by payment mode

1. Manual mode often requires two calls if proof is absent on the first call.
2. Auto mode can proceed in one call when auto transfer succeeds.
3. Sponsored mode proceeds without any external payment proof (i.e. execution is sponsored).

## Testnet execution tips

1. Use `chain: "ethereum"` for Sepolia testnet execution flow.
2. Confirm token symbol in `X-Payment-Proof` matches one of the server returned `accepted_tokens`.
3. Send payment to the exact `payment_address` returned by `402` response.
4. Keep payment and execute requests tied to the same `agent_id` context.
