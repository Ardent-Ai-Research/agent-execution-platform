# Agent Integration

The agent integration pack ships everything needed to connect to the AI agent blockchain execution platform — whether you are a developer scripting from the terminal, a team using AI coding tools, or an agent framework that needs a typed API contract.

It includes a one-line installer, a zero-dependency CLI, an MCP server for AI desktop tools, and an OpenAPI 3.1 spec.

Run the installer once to get everything:

```bash
curl -fsSL https://raw.githubusercontent.com/Ardent-Ai-Research/agent-execution-platform/master/docs/agent-integration/install.sh | bash
```

Then set your key:

```bash
export ARDENT_API_KEY="your_api_key"
```

The installer adds `~/.local/bin` to your shell profile when needed. Open a new terminal after install. If `ardent` is still not found, run:
>
> ```bash
> export PATH="${HOME}/.local/bin:${PATH}"
> ```

## Who each integration is for

### Developers and engineers — the `ardent` CLI

**Best for:** engineers who want to test, debug, or script against the platform from the terminal without writing HTTP clients.

The CLI is a single Python file with no external dependencies. Install once, run anywhere Python 3.8+ is available.

**When to use it:**

- Smoke testing a new API key before writing integration code.
- Running simulate and execute manually during contract development.
- Scripting execution flows from shell scripts, Makefiles, or CI pipelines.
- Quickly checking execution status without opening a browser or writing curl.

#### Example: verify a new key, resolve a wallet, run a simulation

```bash
ardent health
ardent wallet --agent-id my-agent-001 --chain ethereum
ardent simulate \
  --agent-id my-agent-001 \
  --chain ethereum \
  --target-contract 0xTokenContract \
  --calldata 0xTransferCalldata \
  --value 0
```

#### Example: execute and poll status in a shell script

```bash
ardent execute \
  --agent-id deploy-bot \
  --chain base \
  --target-contract 0xFactory \
  --calldata 0xDeployCalldata \
  --value 0

ardent status --request-id your_request_id
```

**Example: manual payment mode re-submit after `402 payment_required`**

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

**Full command reference:**

```bash
ardent --version
ardent health
ardent feed                             # public activity feed
ardent wallet         --agent-id <id> --chain <chain>
ardent wallet-balance --agent-id <id> --chain <chain>   # native + ERC-20 token balances
ardent simulate --agent-id <id> --chain <chain> --target-contract <addr> --calldata <hex> --value <wei>
ardent execute  --agent-id <id> --chain <chain> --target-contract <addr> --calldata <hex> --value <wei>
ardent aave-balances --agent-id <id>                   # Aave reserve wallet/aToken/debt balances
ardent aave-position --agent-id <id>                   # Aave account data and health factor
ardent status   --request-id <id>
ardent self-update
ardent self-update --with-runtime       # also refreshes ~/.ardent files
```

---

### AI assistant users — the MCP server

**Best for:** developers and power users who run Codex, Claude Desktop, ChatGPT Desktop, Cursor, or Windsurf and want their AI tool to call the Ardent platform directly during a session.

The MCP server (`~/.ardent/mcp_server.py`) speaks the Model Context Protocol over stdio. Once registered, the AI assistant can invoke Ardent tools in response to natural language without you writing any code or curl commands.

The installer automatically patches the config file for any supported app it detects.

| App | Config file patched |
| --- | --- |
| Codex | `~/.codex/config.toml` |
| Claude Desktop | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| ChatGPT Desktop | `~/Library/Application Support/com.openai.chat/mcp_servers.json` |
| Cursor | `~/.cursor/mcp.json` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` |

> **You must insert your API key manually after install.**
> The installer writes `your_api_key_here` as a placeholder in the `env` block of each patched config. Open the file the installer reports and replace that value with your real key, then fully quit and reopen the app (Cmd+Q — not just close the window).
>
> The patched entry looks like this: `"env": { "ARDENT_API_KEY": "your_api_key_here" }`
>
> Replace `your_api_key_here` with your actual `ARDENT_API_KEY`. If you re-run the installer after already setting a real key, it preserves the existing value.

**When to use it:**

- Asking Claude to simulate a specific contract call and tell you the estimated cost before you commit to running it.
- Using Cursor to execute a transaction mid-session while building a contract integration, without leaving the editor.
- Asking ChatGPT to fetch recent platform activity or check whether an earlier request confirmed.
- Letting Windsurf handle the full simulate → execute → status loop in a guided workflow.

**Example conversations once the MCP server is active:**

> *"What are the current token balances for agent-001 on Base?"*

> *"Show my Aave wallet, supplied, and debt balances for agent-001."*

> *"Simulate a transfer of 100 USDC from my agent wallet on Base and tell me the gas cost."*

> *"Execute the approve and swap sequence for agent-001 on Ethereum. Use the calldata I just generated."*

> *"What is the current status of request ID abc-123?"*

> *"Show me the last 10 executions on the public feed."*

The assistant calls the appropriate tool (`ardent_simulate`, `ardent_execute`, `ardent_status`, `ardent_feed_recent`) and returns the result directly in the conversation.

**Manual MCP registration** (for any other MCP-compatible AI):

```bash
python3 ~/.ardent/mcp_server.py
```

**MCP tools available:**

| Tool | What it does |
| --- | --- |
| `ardent_health` | Check API reachability |
| `ardent_feed_recent` | Fetch public execution activity |
| `ardent_get_wallet` | Resolve or provision an agent smart wallet |
| `ardent_wallet_balance` | Get native + ERC-20 token balances for an agent wallet |
| `ardent_aave_balances` | Read Aave reserve wallet, supplied aToken, and debt balances |
| `ardent_aave_position` | Read Aave account data, borrowing capacity, and health factor |
| `ardent_simulate` | Simulate a transaction and return estimated cost |
| `ardent_execute` | Submit a transaction for execution |
| `ardent_status` | Poll execution request status |

---

### Product teams and framework builders — the OpenAPI spec

**Best for:** teams building their own agents or products on top of the AI agent blockchain execution layer, and anyone who needs a typed API contract to generate clients or configure tool-calling frameworks.

The spec (`~/.ardent/openapi.yaml`) is OpenAPI 3.1 and covers all user-facing endpoints.

**When to use it:**

- Generating a typed HTTP client in any language using `openapi-generator` or similar tooling.
- Configuring a ChatGPT custom action so your GPT can call Ardent endpoints directly from a product.
- Loading the spec into an agent framework (LangChain, AutoGen, etc.) as a tool schema source.
- Mocking the Ardent API in integration tests.
- Sharing exact request and response shapes with the rest of your team.

#### Example: generate a Python client

```bash
openapi-generator generate \
  -i ~/.ardent/openapi.yaml \
  -g python \
  -o ./ardent-client
```

#### Example: load into a ChatGPT custom action

In your GPT configuration, upload `openapi.yaml` under Actions. The GPT will automatically have access to all Ardent endpoints as callable actions with correct schemas.

---

### LLM agents with direct HTTP access — the agent playbook

**Best for:** autonomous AI agents that already have direct HTTP tool access and need a machine-readable reference for the correct execution flow, guardrails, and canonical request patterns.

The `skills.md` file is designed to be loaded as a system prompt or injected as context. It covers:

1. The canonical simulate → execute → status loop.
2. How to handle `402 payment_required` correctly.
3. Guardrails that prevent common agent mistakes (inventing payment amounts, changing `request_id` between calls, executing without simulating).

Full skills file:
`https://raw.githubusercontent.com/Ardent-Ai-Research/agent-execution-platform/master/docs/agent-integration/skills.md`

---

## Integration files reference

| File | Purpose | Used by |
| --- | --- | --- |
| `install.sh` | One-line installer | Everyone |
| `ardent_cli.py` | Zero-dependency CLI | Developers, scripts, CI |
| `mcp_server.py` | stdio MCP server | Codex, Claude, ChatGPT, Cursor, Windsurf |
| `mcp-tools.json` | MCP tool definitions | `mcp_server.py` |
| `openapi.yaml` | OpenAPI 3.1 spec | ChatGPT custom actions, code generators, framework builders |
| `skills.md` | Setup guide and agent playbook | Humans and LLM system prompts |
