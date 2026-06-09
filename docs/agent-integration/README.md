# AI Agent Blockchain Execution Integration

Everything needed to connect developers, AI agents, and LLM platforms to the AI Agent Blockchain Execution API.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Ardent-Ai-Research/agent-execution-platform/master/docs/agent-integration/install.sh | bash
```

The installer:

- Places the `ardent` CLI in `~/.local/bin`
- Adds `~/.local/bin` to your shell profile when needed so new terminals can find `ardent`
- Downloads MCP server + runtime files into `~/.ardent/`
- Auto-patches MCP configs for **Codex, Claude Desktop, ChatGPT Desktop, Cursor, and Windsurf** if any are detected
- Preserves any existing `ARDENT_API_KEY` already set in those configs

## Set credentials

```bash
export ARDENT_API_KEY="your_api_key"
```

> If `ardent` is not found after install, add `~/.local/bin` to your PATH:
>
> ```bash
> export PATH="${HOME}/.local/bin:${PATH}"
> ```

## CLI commands

### General

```bash
ardent --version
ardent health
ardent feed                             # public activity feed
ardent wallet         --agent-id my-agent-001 --chain ethereum
ardent wallet-balance --agent-id my-agent-001 --chain ethereum   # native + ERC-20 balances
ardent simulate --agent-id my-agent-001 --chain ethereum --target-contract 0xTargetContract --calldata 0xCalldata --value 0
ardent execute --agent-id my-agent-001 --chain ethereum --target-contract 0xTargetContract --calldata 0xCalldata --value 0
ardent status --request-id your_request_id
```

### Aave V3 Sepolia

```bash
ardent aave-supply-simulate --agent-id my-agent-001 --asset USDC --amount 1.25
ardent aave-supply --agent-id my-agent-001 --asset USDC --amount 1.25
ardent aave-withdraw-simulate --agent-id my-agent-001 --asset USDC --amount max
ardent aave-withdraw --agent-id my-agent-001 --asset USDC --amount max
ardent aave-repay-simulate --agent-id my-agent-001 --asset USDC --amount max
ardent aave-repay --agent-id my-agent-001 --asset USDC --amount max
ardent aave-borrow-simulate --agent-id my-agent-001 --asset USDC --amount max --min-health-factor 1.10
ardent aave-borrow --agent-id my-agent-001 --asset USDC --amount max --min-health-factor 1.10
ardent aave-position --agent-id my-agent-001
ardent aave-balances --agent-id my-agent-001
```

### GMX V2 Arbitrum Sepolia

Use raw GMX values for order sizing. Market increase `size_delta_usd_raw` and
`acceptable_price_raw` use GMX 30-decimal precision.

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
ardent gmx-markets --start 0 --end 50
ardent gmx-positions --agent-id my-agent-001
ardent gmx-orders --agent-id my-agent-001
ardent gmx-balances --agent-id my-agent-001
ardent gmx-cancel-order-simulate --agent-id my-agent-001 --order-key 0xYourBytes32OrderKey
ardent gmx-cancel-order --agent-id my-agent-001 --order-key 0xYourBytes32OrderKey
ardent gmx-update-order-simulate --agent-id my-agent-001 --body-file ./gmx-update.json
ardent gmx-create-deposit-simulate --agent-id my-agent-001 --body-file ./gmx-deposit.json
ardent gmx-create-withdrawal-simulate --agent-id my-agent-001 --body-file ./gmx-withdrawal.json
ardent gmx-cancel-simulate --agent-id my-agent-001 --request-type deposit --key 0xYourBytes32RequestKey
ardent gmx-claim-simulate --agent-id my-agent-001 --claim-type funding_fees --market 0xYourGmxMarketToken --token 0xClaimToken
```

`gmx-markets` includes token symbols when ERC-20 metadata is available.
`gmx-balances` returns GM/market LP token balances plus underlying GMX market
asset token balances for the smart wallet.

Manual payment re-submit (after a `402` response):

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

Update CLI and runtime files:

```bash
ardent self-update
ardent self-update --with-runtime
```

## Files in this folder

| File | Purpose | Used by |
| --- | --- | --- |
| `install.sh` | One-line installer | Everyone |
| `ardent_cli.py` | Zero-dependency CLI | Developers, scripts, CI |
| `mcp_server.py` | stdio MCP server | Codex, Claude, ChatGPT, Cursor, Windsurf |
| `mcp-tools.json` | MCP tool definitions | `mcp_server.py` |
| `openapi.yaml` | Generated bundled OpenAPI 3.1 spec | ChatGPT custom actions, code generators |
| `openapi/` | Split OpenAPI source + bundler | Maintainers |
| `skills.md` | Setup guide + agent playbook | Humans and LLM system prompts |

When editing the API spec, update files under `openapi/`, then run:

```bash
ruby docs/agent-integration/openapi/bundle.rb
```

## MCP — supported methods

- `initialize`
- `tools/list`
- `tools/call`

## Notes

- MCP tools available: `ardent_health`, `ardent_feed_recent`, `ardent_get_wallet`, `ardent_wallet_balance`, `ardent_simulate`, `ardent_execute`, Aave tools (`ardent_aave_supply_simulate`, `ardent_aave_supply_execute`, `ardent_aave_withdraw_simulate`, `ardent_aave_withdraw_execute`, `ardent_aave_repay_simulate`, `ardent_aave_repay_execute`, `ardent_aave_borrow_simulate`, `ardent_aave_borrow_execute`, `ardent_aave_position`, `ardent_aave_balances`), GMX tools (`ardent_gmx_create_order_simulate`, `ardent_gmx_create_order_execute`, `ardent_gmx_cancel_order_simulate`, `ardent_gmx_cancel_order_execute`), and `ardent_status`
