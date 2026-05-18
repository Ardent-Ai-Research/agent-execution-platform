# Getting Started

This section helps you complete a first successful integration quickly.

## 1. Prepare required values

Set these shell variables in your terminal.

```bash
BASE_URL="https://api.ardentresearch.xyz"
API_KEY="ak_your_api_key"
```

If you do not have an API key yet, request access first using the onboarding channel.

See [Request API Key](workflows/request-api-key.md).

## 2. Understand endpoint groups

The hosted API has two endpoint groups for users.

1. Public endpoints.
   1. `GET /health`
   2. `GET /feed/recent`
2. Protected API key endpoints.
   1. `GET /wallet`
   2. `POST /simulate`
   3. `POST /execute`
   4. `GET /status/:id`

## 3. Understand your payment mode

Payment mode is assigned to your key by Ardent Research during access provisioning.

1. `manual`: requires payment proof for payable execution.
2. `auto`: platform performs an internal token transfer if proof is not provided.
3. `sponsored`: no external payment requirement for execution.

Detailed behavior is in [Payment Modes](payment-modes.md).

## 4. First end to end path

Use this sequence for first validation.

1. Request API key access.
1. Get wallet address for your agent ID.
1. Fund wallet if your strategy needs tokens.
1. Simulate execution.
1. Execute transaction.
1. Poll status until terminal state.

You can run every step with raw curl or with the `ardent` CLI. See [Agent Integration](agent-integration.md) for the one-line installer and CLI reference.

## 5. Terminal states

A request eventually reaches one of these terminal states.

1. `confirmed`
2. `reverted`
3. `failed`

## 6. Supported chain names in request payloads

Use one of:

1. `ethereum`
2. `base`
3. `bnb`

Aliases like `eth` and `bsc` are accepted by server side chain parsing, but using canonical names is recommended for clean client code.

## 7. Testnet token details for Sepolia users

Current x402 payment tokens on Ethereum Sepolia:

1. `USDC` at `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238`
2. `USDT` at `0xd077A400968890Eacc75cdc901F0356c943e4fDb`
3. `aUSD` at `0x112a19d6236016fc4dda49257c724E63a3CE5bEA`

For approved testers, `aUSD` (Ardent USD) is accepted in the hosted testnet payment flow and is requested through a separate faucet flow.

For exact payable amounts, always use `required_amount_raw` from the live `402 Payment Required` response.

For complete testnet instructions, see [Testnet Guide](testnet-guide.md).
