# Agent Integration Pack (`skills.md` + OpenAPI + MCP + CLI)

This folder provides a standard install-style integration for Ardent API, similar to commonly used agent skill integrations.

## One-liner setup (no repo clone required)

Read setup and agent playbook remotely:

```bash
curl -sL https://raw.githubusercontent.com/Ardent-Ai-Research/agent-execution-platform/master/docs/agent-integration/skills.md
```

Install CLI + runtime files:

```bash
curl -fsSL https://raw.githubusercontent.com/Ardent-Ai-Research/agent-execution-platform/master/docs/agent-integration/install.sh | bash
```

Then set credentials:

```bash
export ARDENT_API_KEY="your_api_key"
export ARDENT_BASE_URL="https://api.ardentresearch.xyz"
```

## Most common commands

```bash
ardent --version
ardent health
ardent wallet --agent-id my-agent-001 --chain ethereum
ardent simulate --agent-id my-agent-001 --chain ethereum --target-contract 0xTargetContract --calldata 0xCalldata --value 0
ardent execute --agent-id my-agent-001 --chain ethereum --target-contract 0xTargetContract --calldata 0xCalldata --value 0
ardent status --request-id your_request_id
```

Update later:

```bash
ardent self-update
ardent self-update --with-runtime
```

Manual payment mode re-submit:

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

## Integration files

- `skills.md` — human setup guide + behavior and guardrails playbook for LLM agents
- `install.sh` — one-line installer (CLI + runtime files)
- `ardent_cli.py` — zero-dependency CLI wrapper for Ardent endpoints
- `mcp_server.py` — minimal stdio MCP server for LLM tool runtimes
- `mcp-tools.json` — MCP tool map and schemas
- `openapi.yaml` — machine-readable API contract for action/tool generation

## MCP methods supported

- `initialize`
- `tools/list`
- `tools/call`

## Notes

- Installer places `ardent` in `~/.local/bin` and runtime files in `~/.ardent`.
- Add `~/.local/bin` to your `PATH` if needed.
- For production, add stricter validation, retries/backoff, auth rotation, and structured logging.
