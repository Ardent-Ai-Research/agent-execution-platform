# FAQ

## 1. Do I need to create a wallet before execute

No separate wallet creation endpoint is required. `GET /wallet` resolves or provisions it for your agent. `POST /execute` also resolves wallet context as part of processing.

## 2. Can I use one API key for many agents

Yes. Agent identity is namespaced under API key scope. Use stable `agent_id` values so each agent maps consistently to its smart wallet.

## 3. What if I lose my API key

Request key rotation through Ardent Research support or onboarding. Update your client configuration immediately after rotation.

## 4. Is `chain` required on wallet endpoint

No. If omitted, default is `ethereum`.

## 5. Should frontend code call protected endpoints directly

For production systems, server side calls are recommended because API keys are secrets.

## 6. How do I monitor execution completion

You can combine both methods.

1. Poll `GET /status/:id`.
2. Provide `callback_url` on execute for push updates.

## 7. Why did manual execute return payment required even after payment

Most common causes are:

1. Payment proof tx hash mismatch.
2. Wrong token or wrong chain in header.
3. Transfer recipient does not match platform treasury.
4. Insufficient amount transferred.
5. Confirmation threshold not yet met.

## 8. Can I execute batch transactions

Yes. Use `batch_calls` with each call containing `target_contract`, `value`, and `calldata`.

## 9. How do I keep integration predictable in production

Use this operating pattern:

1. Simulate first.
2. Execute second.
3. Persist every request ID.
4. Track status until terminal state.
5. Store webhook events and status snapshots for audit.

## 10. Which x402 tokens are accepted on Sepolia testnet

Current hosted testnet accepted tokens:

1. `USDC` at `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238`
2. `USDT` at `0xd077A400968890Eacc75cdc901F0356c943e4fDb`
3. `aUSD` at `0x112a19d6236016fc4dda49257c724E63a3CE5bEA`

## 11. How do I get aUSD for testnet

Request `aUSD` allocation through the dedicated faucet flow.

The only required field is your agent smart wallet address.

Faucet link:

`https://faucet.ardentresearch.xyz`

## 12. Is there a CLI instead of raw curl

Yes. Run the one-line installer:

```bash
curl -fsSL https://raw.githubusercontent.com/Ardent-Ai-Research/agent-execution-platform/master/docs/agent-integration/install.sh | bash
```

This installs an `ardent` command with subcommands for every platform operation: `health`, `feed`, `wallet`, `simulate`, `execute`, `status`, and `self-update`.

See [Agent Integration](agent-integration.md).

## 13. Can I use Ardent tools inside Claude, ChatGPT, Cursor, or Windsurf

Yes. The installer auto-patches MCP configs for all four apps if they are detected on your machine.

After install, restart the desktop app and all Ardent tools (`ardent_health`, `ardent_feed_recent`, `ardent_get_wallet`, `ardent_simulate`, `ardent_execute`, `ardent_status`) will appear in the tool list.

For manual or other MCP-compatible runtimes, point them at:

```bash
python3 ~/.ardent/mcp_server.py
```

## 14. What is the public activity feed endpoint

`GET /feed/recent` returns recent execution activity and requires no authentication.

```bash
curl -X GET "https://api.ardentresearch.xyz/feed/recent?limit=12"
# or
ardent feed
```

Use it to confirm the API is reachable or to monitor platform activity.
