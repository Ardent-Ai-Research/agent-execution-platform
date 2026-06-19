# Balancer V3 Ethereum Sepolia

Ardent exposes typed Balancer V3 swaps and liquidity actions on Ethereum
Sepolia using the `chain: "ethereum"` label.

## Supported actions

- Exact-input swap
- Exact-output swap
- Live swap quote with automatic slippage limit
- Unbalanced add liquidity, including balanced and single-token additions
- Proportional remove liquidity by exact BPT input
- Liquidity quotes with automatic slippage limits
- Pool state and registered-token read
- Agent BPT and pool-token balance read

Balancer pool addresses and token sets are not hardcoded. The API verifies the
selected pool against the Balancer V3 Vault and reads token registration order
directly from the Vault Explorer.

## Contracts

| Contract | Ethereum Sepolia address |
|---|---|
| Router V2 | `0x5e315f96389C1aaF9324D97d3512ae1e0Bf3C21a` |
| Vault | `0xbA1333333333a1BA1108E8412f11850A5C319bA9` |
| Vault Explorer V2 | `0xC82E329C832CAcc8DA65dbB57ac72B068e0CEb9B` |
| Permit2 | `0x000000000022D473030F116dDEE9F6B43aC78BA3` |

These addresses come from Balancer's official deployment repository.

## Ardent aUSD/USDC test pool

Ardent maintains an initialized Balancer V3 Stable Pool for testing swaps
between Circle USDC and Ardent USD:

| Asset | Ethereum Sepolia address |
|---|---|
| Stable Pool | `0x0c131e566752417dAA7d8a51D1E9ae8c95B52E99` |
| USDC | `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238` |
| aUSD | `0xE9df660c675F6f649677Ae408FCf6665D4F0F5Be` |

The pool uses 6-decimal tokens, an amplification parameter of `100`, and a
`0.1%` swap fee. This is testnet liquidity, so request a live quote and
simulate before every swap.

## Inspect a pool first

`balancer-pool` does not list every Balancer pool. It inspects one pool address
that you provide and verifies it against the V3 Vault.

```bash
ardent balancer-pool \
  --pool 0xYourBalancerV3Pool
```

The response includes:

- Pool registration, initialization, pause, and recovery-mode state
- BPT name, symbol, decimals, and total supply
- Registered token addresses in Vault order
- Token metadata and pool balances
- Static swap fee

A registered pool can still reject a swap because of pool-specific hooks or
rules. For example, an LBP may disable swaps outside its active sale window.
Always request a quote and simulate the action.

## Find Sepolia pools

The simplest option is Balancer's testnet application:

`https://test.balancer.fi/`

For programmatic discovery, query Balancer's official GraphQL API and filter for
Sepolia V3 pools:

```bash
curl -X POST "https://api-v3.balancer.fi/graphql" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "query Pools($where: GqlPoolFilter) { poolGetPools(first: 50, where: $where) { address name symbol type protocolVersion chain poolTokens { address symbol decimals index } dynamicData { totalLiquidity swapFee } } }",
    "variables": {
      "where": {
        "chainIn": ["SEPOLIA"],
        "protocolVersionIn": [3]
      }
    }
  }'
```

The deployment-address repository lists shared Balancer contracts and pool
factories, but it is not a complete list of pools created by those factories.

## Read wallet balances

```bash
ardent balancer-balances \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool
```

This returns the wallet's BPT balance and wallet balances for every registered
token in the selected pool.

## Add liquidity

Provide one `--amount-in TOKEN_ADDRESS=RAW_AMOUNT` value for each token you
want to deposit. The server reads the pool's canonical token order, quotes the
expected BPT output, derives a minimum, and fully simulates the approval and
Router batch.

Two-token example:

```bash
ardent balancer-add-liquidity-quote \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --amount-in 0xFirstPoolToken=1000000 \
  --amount-in 0xSecondPoolToken=1000000

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
```

You may supply only one registered token, but an unbalanced addition generally
has more price impact than depositing at the pool's current ratio. One
add-liquidity operation supports up to three deposited token addresses so its
approval, Router, and cleanup calls remain within the atomic batch limit.

## Remove liquidity

Proportional removal burns an exact BPT amount and returns every registered pool
token. Read `balancer-balances` first to obtain the wallet's raw BPT balance.

```bash
ardent balancer-remove-liquidity-quote \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --bpt-amount-in-raw 1000000000000000000

ardent balancer-remove-liquidity-simulate \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --bpt-amount-in-raw 1000000000000000000

ardent balancer-remove-liquidity \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --bpt-amount-in-raw 1000000000000000000
```

The quote derives a minimum for every output token. Advanced callers may supply
repeatable `--min-amount-out TOKEN_ADDRESS=RAW_AMOUNT` values instead.

## Quote an exact-input swap

All amounts are raw token base units.

```bash
ardent balancer-quote \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --token-in 0xFirstPoolToken \
  --token-out 0xSecondPoolToken \
  --swap-kind exact_in \
  --amount-raw 1000000 \
  --slippage-bps 100
```

The response returns `quoted_amount_raw`, `limit_raw`, and `deadline`.
For `exact_in`, `limit_raw` is the minimum output amount.

## Quote an exact-output swap

```bash
ardent balancer-quote \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --token-in 0xFirstPoolToken \
  --token-out 0xSecondPoolToken \
  --swap-kind exact_out \
  --amount-raw 1000000 \
  --slippage-bps 100
```

For `exact_out`, `amount_raw` is the exact desired output and `limit_raw` is
the maximum input amount.

## Simulate and execute

```bash
ardent balancer-swap-simulate \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --token-in 0xFirstPoolToken \
  --token-out 0xSecondPoolToken \
  --swap-kind exact_in \
  --amount-raw 1000000 \
  --slippage-bps 100

ardent balancer-swap \
  --agent-id my-agent-001 \
  --pool 0xYourBalancerV3Pool \
  --token-in 0xFirstPoolToken \
  --token-out 0xSecondPoolToken \
  --swap-kind exact_in \
  --amount-raw 1000000 \
  --slippage-bps 100
```

You may provide `--limit-raw` to bypass automatic slippage calculation. You may
also provide a future `--deadline` Unix timestamp; otherwise the server uses a
deadline twenty minutes from request handling.

## How token approval works

Balancer V3's retail Router pulls swap input through Permit2. Ardent compiles
one atomic smart-wallet batch:

1. Reset the ERC-20 Permit2 allowance to zero
2. `ERC20.approve(Permit2, maxInput)`
3. `Permit2.approve(tokenIn, Router, maxInput, deadline)`
4. `Router.swapSingleTokenExactIn(...)` or `swapSingleTokenExactOut(...)`
5. Clear the Router allowance in Permit2
6. Clear the ERC-20 Permit2 allowance

The complete batch is simulated as a UserOperation before execution. The
approval cleanup occurs atomically after a successful swap.

## Current scope

The Balancer V3 integration covers single-pool swaps, unbalanced liquidity
addition, and proportional liquidity removal. Multi-hop Batch Router swaps,
single-token removal, boosted pools, buffers, and gauge actions are not exposed
yet.
