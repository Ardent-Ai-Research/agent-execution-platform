# AI Agent Blockchain Execution Integration

Everything needed to connect developers, AI agents, and LLM platforms to the AI Agent Blockchain Execution API.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Ardent-Ai-Research/agent-execution-platform/master/docs/agent-integration/install.sh | bash
```

The installer:

- Places the `ardent` CLI in `~/.local/bin`
- Downloads MCP server + runtime files into `~/.ardent/`
- Auto-patches MCP configs for **Claude Desktop, ChatGPT Desktop, Cursor, and Windsurf** if any are detected
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
| `mcp_server.py` | stdio MCP server | Claude, ChatGPT, Cursor, Windsurf |
| `mcp-tools.json` | MCP tool definitions | `mcp_server.py` |
| `openapi.yaml` | OpenAPI 3.1 spec | ChatGPT custom actions, code generators |
| `skills.md` | Setup guide + agent playbook | Humans and LLM system prompts |

## MCP — supported methods

- `initialize`
- `tools/list`
- `tools/call`

## Notes

- MCP tools available: `ardent_health`, `ardent_feed_recent`, `ardent_get_wallet`, `ardent_wallet_balance`, `ardent_simulate`, `ardent_execute`, `ardent_status`
