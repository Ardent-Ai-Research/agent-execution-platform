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

## Testnet payment tokens

Current accepted x402 payment tokens in the hosted testnet environment:

| Payload chain | USDC | aUSD |
| --- | --- | --- |
| `ethereum` | `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238` | `0xE9df660c675F6f649677Ae408FCf6665D4F0F5Be` |
| `base` | `0x036CbD53842c5426634e7929541eC2318f3dCF7e` | `0xE9df660c675F6f649677Ae408FCf6665D4F0F5Be` |
| `arbitrum` | `0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d` | `0xE9df660c675F6f649677Ae408FCf6665D4F0F5Be` |

Circle faucet USDC is available at `https://faucet.circle.com`; choose the Sepolia network that matches your request `chain`. Ethereum Sepolia may also accept `USDT` at `0xd077A400968890Eacc75cdc901F0356c943e4fDb` when enabled for your environment.

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
  "accepted_tokens": ["USDC", "USDT", "aUSD"],
  "required_amount_raw": {
    "USDC": "250000",
    "USDT": "250000",
    "aUSD": "250000"
  },
  "payment_address": "0x742d35Cc6634C0532925a3b844Bc9e7595f2bD18",
  "chain": "ethereum",
  "request_id": "your_request_id",
  "smart_wallet_address": "0x1234567890abcdef1234567890abcdef12345678"
}
```

## Re submit with payment proof

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

## Success response

Status code: `200 OK`

Example:

```json
{
  "request_id": "your_request_id",
  "status": "queued",
  "smart_wallet_address": "0xAgentSmartWallet",
  "estimated_gas": 52000,
  "estimated_cost_usd": 0.25,
  "tx_hash": null,
  "message": "execution queued"
}
```

## CLI equivalent

```bash
ardent execute \
  --agent-id my-agent-001 \
  --chain ethereum \
  --target-contract 0xTargetContract \
  --calldata 0xCalldata \
  --value 0
```

For manual payment re-submit after `402`:

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

See [Agent Integration](../agent-integration.md) for the one-line installer.

## Aave V3 Sepolia supply

For testnet Aave V3 supply actions, prefer the typed protocol endpoint instead
of manually encoding calldata.

For the full Aave command/reference page, including reserve balances and test asset funding, see [Aave V3 Sepolia](../aave-v3.md).

Simulate:

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

Execute:

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

CLI equivalent:

```bash
ardent aave-supply-simulate \
  --agent-id my-agent-001 \
  --asset USDC \
  --amount 1.25

ardent aave-supply \
  --agent-id my-agent-001 \
  --asset USDC \
  --amount 1.25
```

For manual payment re-submit after `402`:

```bash
ardent aave-supply \
  --agent-id my-agent-001 \
  --asset USDC \
  --amount 1.25 \
  --proof-request-id your_request_id \
  --proof-payer 0xYourPayer \
  --proof-token USDC \
  --proof-chain ethereum \
  --proof-tx-hash 0xYourPaymentTxHash
```

The adapter compiles the request into an atomic `approve -> Pool.supply` batch
for the agent's ERC-4337 smart wallet and uses full UserOperation simulation.
Use `amount_raw` instead of `amount` when the caller already has exact token
base units.

Withdraw:

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

CLI equivalent:

```bash
ardent aave-withdraw-simulate \
  --agent-id my-agent-001 \
  --asset USDC \
  --amount max

ardent aave-withdraw \
  --agent-id my-agent-001 \
  --asset USDC \
  --amount max
```

Repay:

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

CLI equivalent:

```bash
ardent aave-repay-simulate \
  --agent-id my-agent-001 \
  --asset USDC \
  --amount max

ardent aave-repay \
  --agent-id my-agent-001 \
  --asset USDC \
  --amount max
```

Repay `max` resolves to the smaller of selected-rate debt and the wallet's
underlying token balance before the normal approve/repay batch is simulated.

Borrow:

```bash
curl -X POST "$BASE_URL/protocols/aave-v3/borrow" \
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

CLI equivalent:

```bash
ardent aave-borrow-simulate \
  --agent-id my-agent-001 \
  --asset USDC \
  --amount max \
  --min-health-factor 1.10

ardent aave-borrow \
  --agent-id my-agent-001 \
  --asset USDC \
  --amount max \
  --min-health-factor 1.10
```

Borrow checks projected health factor using Aave account data and oracle price
before the normal execution simulation. The default minimum projected health
factor is `1.05`, and custom values must be at least `1.0`. Borrow `max`
resolves to the largest amount allowed by available borrows and that projected
health-factor floor.

Read position:

```bash
curl -X GET "$BASE_URL/protocols/aave-v3/position?agent_id=my-agent-001&chain=ethereum" \
  -H "X-API-Key: $API_KEY"
```

CLI equivalent:

```bash
ardent aave-balances --agent-id my-agent-001
ardent aave-position --agent-id my-agent-001
```

## Balancer V3 Ethereum Sepolia

Balancer V3 swaps and liquidity actions should use the typed endpoints instead
of manually encoding Permit2 and Router calls. See
[Balancer V3 Ethereum Sepolia](../balancer-v3.md) for the full reference.

Inspect the pool and request a quote:

```bash
curl -X GET "$BASE_URL/protocols/balancer-v3/pool?chain=ethereum&pool=0xYourBalancerV3Pool" \
  -H "X-API-Key: $API_KEY"

curl -X POST "$BASE_URL/protocols/balancer-v3/swap/quote" \
  -H "Content-Type: application/json" \
  -H "X-API-Key: $API_KEY" \
  -d '{
    "agent_id": "my-agent-001",
    "chain": "ethereum",
    "pool": "0xYourBalancerV3Pool",
    "token_in": "0xFirstPoolToken",
    "token_out": "0xSecondPoolToken",
    "swap_kind": "exact_in",
    "amount_raw": "1000000",
    "slippage_bps": 100
  }'
```

CLI equivalents:

```bash
ardent balancer-pool --pool 0xYourBalancerV3Pool

ardent balancer-quote \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --token-in 0xFirstPoolToken \
  --token-out 0xSecondPoolToken \
  --swap-kind exact_in \
  --amount-raw 1000000

ardent balancer-swap-simulate \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --token-in 0xFirstPoolToken \
  --token-out 0xSecondPoolToken \
  --swap-kind exact_in \
  --amount-raw 1000000

ardent balancer-swap \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --token-in 0xFirstPoolToken \
  --token-out 0xSecondPoolToken \
  --swap-kind exact_in \
  --amount-raw 1000000

ardent balancer-add-liquidity-simulate \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --amount-in 0xFirstPoolToken=1000000 \
  --amount-in 0xSecondPoolToken=1000000

ardent balancer-add-liquidity \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --amount-in 0xFirstPoolToken=1000000 \
  --amount-in 0xSecondPoolToken=1000000

ardent balancer-remove-liquidity-simulate \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --bpt-amount-in-raw 1000000000000000000

ardent balancer-remove-liquidity \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --bpt-amount-in-raw 1000000000000000000
```

The adapter resets existing approval, grants bounded ERC-20 and Permit2
allowances, executes the Router swap, then clears both allowance layers in one
atomic batch. The server derives the limit from a live Router quote when
`limit_raw` is omitted, then runs the normal full UserOperation simulation.
Liquidity additions use the same bounded approval and cleanup pattern.
They support up to three deposited token addresses per operation. Proportional
removals burn BPT directly and require no approval.

## GMX V2 Arbitrum Sepolia

For GMX V2, prefer the typed protocol endpoints instead of manually encoding
the `ExchangeRouter` calldata. For the full GMX command/reference page, see
[GMX V2 Arbitrum Sepolia](../gmx-v2.md).

Create-order simulation:

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

CLI equivalent:

```bash
ardent gmx-create-order-simulate \
  --agent-id my-agent-001 \
  --order-type market_increase \
  --market 0xYourGmxMarketToken \
  --initial-collateral-token 0xYourCollateralToken \
  --initial-collateral-delta-amount-raw 1000000 \
  --size-delta-usd-raw 50000000000000000000000000000000000 \
  --acceptable-price-raw 30000000000000000000000000000000000000000 \
  --execution-fee-raw 1000000000000000 \
  --long

ardent gmx-create-order --agent-id my-agent-001 --body-file ./gmx-order.json
```

Cancel-order CLI equivalent:

```bash
ardent gmx-cancel-order-simulate \
  --agent-id my-agent-001 \
  --order-key 0xYourBytes32OrderKey

ardent gmx-cancel-order \
  --agent-id my-agent-001 \
  --order-key 0xYourBytes32OrderKey
```

The adapter compiles create-order requests into an atomic
`approve -> ExchangeRouter.multicall(sendWnt, sendTokens, createOrder)` bundle
for the agent's ERC-4337 smart wallet. `execution_fee_raw` is paid as ETH value
to the GMX router call, so the smart wallet must hold enough Arbitrum Sepolia
ETH for the GMX keeper fee.

## Notes by payment mode

1. Manual mode often requires two calls if proof is absent on the first call.
2. Auto mode can proceed in one call when auto transfer succeeds.
3. Sponsored mode proceeds without any external payment proof (i.e. execution is sponsored).

## Testnet execution tips

1. Use `chain: "ethereum"` for Sepolia testnet execution flow.
2. Confirm token symbol in `X-Payment-Proof` matches one of the server returned `accepted_tokens`.
3. Send payment to the exact `payment_address` returned by `402` response.
4. Keep payment and execute requests tied to the same `agent_id` context.
