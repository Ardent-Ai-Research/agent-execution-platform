# FAQ

## Do I need to create a wallet first?

No. `GET /wallet`, simulation, and execution automatically resolve or provision the wallet for the authenticated `(API key, agent_id)` pair.

## How do I get an API key?

Call public `POST /api-keys` or run `ardent api-key-create --label my-agent`. Store the returned key immediately because it is shown once.

## Can one API key have many agents?

Yes. Use a different stable `agent_id` for each logical agent. Changing an ID creates a different wallet.

## Are testnet transactions free?

The API does not charge an execution fee and the configured paymaster sponsors UserOperation gas. The agent wallet must still supply assets spent by the action and protocol-required native value such as GMX keeper fees.

## Can I execute batches?

Yes. Use `batch_calls`; the full bundle is simulated atomically before queueing.

## How do I monitor completion?

Poll `GET /status/:id`, provide a `callback_url`, or use both. Persist request IDs for audit and recovery.

## Should frontend code hold API keys?

No. Keep keys in a trusted backend or agent runtime secret store.

## Which integrations are available?

Aave V3, Compound III, GMX V2, Balancer V3, Morpho Blue, and Uniswap V4 testnet actions and reads are exposed through HTTP, CLI, MCP, and OpenAPI tooling.

## Is there a CLI?

```bash
curl -fsSL https://raw.githubusercontent.com/ardentairesearch/agent-execution-platform/V2/docs/agent-integration/install.sh | bash
```

Run `ardent --help` after installation.

## Is the recent activity feed public?

Yes. `GET /feed/recent?limit=12` requires no API key.
