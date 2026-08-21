# Get Wallet Balance

Use this endpoint to check the native token and configured ERC-20 balances of an agent's smart wallet.

## Endpoint

`GET /wallet/balance`

## Authentication

`X-API-Key` required.

## Query parameters

1. `agent_id` required string. The agent identifier (same as used in `/execute`).
2. `chain` optional string. Default is `ethereum`.

## Command

```bash
curl -G "$BASE_URL/wallet/balance" \
  -H "X-API-Key: $API_KEY" \
  --data-urlencode "agent_id=my-agent-001" \
  --data-urlencode "chain=ethereum"
```

## CLI equivalent

```bash
ardent wallet-balance --agent-id my-agent-001 --chain ethereum
```

## Success response

Status code: `200 OK`

Example:

```json
{
  "agent_id": "my-agent-001",
  "smart_wallet_address": "0x1234567890abcdef1234567890abcdef12345678",
  "chain": "ethereum",
  "native_balance_wei": "5000000000000000",
  "native_balance_formatted": "0.005",
  "tokens": [
    {
      "symbol": "USDC",
      "contract_address": "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
      "raw": "10000000",
      "formatted": "10",
      "decimals": 6
    },
    {
      "symbol": "USDT",
      "contract_address": "0xd077A400968890Eacc75cdc901F0356c943e4fDb",
      "raw": "0",
      "formatted": "0",
      "decimals": 6
    }
  ]
}
```

## Response fields

| Field | Type | Description |
| --- | --- | --- |
| `agent_id` | string | The agent identifier passed in the query |
| `smart_wallet_address` | string | The agent's ERC-4337 smart wallet address |
| `chain` | string | The chain queried |
| `native_balance_wei` | string | Native token balance in wei (ETH on Ethereum, Base, and Arbitrum) |
| `native_balance_formatted` | string | Native balance formatted in the chain's base unit (e.g. `"0.005"` ETH) |
| `tokens` | array | One entry per tracked token configured on the chain |
| `tokens[].symbol` | string | Token symbol, e.g. `"USDC"` |
| `tokens[].contract_address` | string | Token contract address on the queried chain |
| `tokens[].raw` | string | Raw on-chain balance in the token's smallest unit (e.g. `"10000000"` for 10 USDC at 6 decimals) |
| `tokens[].formatted` | string | Human-readable balance scaled by decimals (e.g. `"10"`) |
| `tokens[].decimals` | number | Number of decimals used for formatting |

## Practical usage

1. Call after `GET /wallet` to confirm the wallet holds assets needed by an action.
2. Use `tokens[].raw` when exact base-unit balances matter.
3. All configured tracked tokens for the chain are returned, even when the balance is zero.
