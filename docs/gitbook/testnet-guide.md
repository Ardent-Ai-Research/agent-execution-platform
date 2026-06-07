# Testnet Guide

This page contains practical details for users integrating with the hosted testnet environment.

## 1. Network context

For the current hosted testnet program, use one of:

| Payload chain | Execution network |
| --- | --- |
| `ethereum` | Ethereum Sepolia |
| `base` | Base Sepolia |
| `arbitrum` | Arbitrum Sepolia |

## 2. Accepted x402 payment tokens

Current hosted testnet token addresses:

| Payload chain | USDC | aUSD |
| --- | --- | --- |
| `ethereum` | `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238` | `0xE9df660c675F6f649677Ae408FCf6665D4F0F5Be` |
| `base` | `0x036CbD53842c5426634e7929541eC2318f3dCF7e` | `0xE9df660c675F6f649677Ae408FCf6665D4F0F5Be` |
| `arbitrum` | `0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d` | `0xE9df660c675F6f649677Ae408FCf6665D4F0F5Be` |

Ethereum Sepolia may also accept `USDT` at `0xd077A400968890Eacc75cdc901F0356c943e4fDb` when enabled for your environment.

In practice these tokens are used with 6 decimal precision in the hosted testnet flow.
For exact payable amounts, always use `required_amount_raw` from the live `402` response.

## 3. aUSD test token support

Ardent provides testnet `aUSD` (Ardent USD) for approved testers.

Important notes:

1. `aUSD` is accepted for x402 payment in the hosted testnet program.
2. Distribution is handled through a dedicated faucet request flow.
3. The only required input in the faucet form is your agent smart wallet address.
4. Use `GET /wallet` first to get the exact wallet address to submit in the faucet form.

Faucet link:

`https://faucet.ardentresearch.xyz`

## 4. Do not hardcode token assumptions

Always treat the API response as source of truth for payment.

When `POST /execute` returns `402 Payment Required`, use:

1. `accepted_tokens` for allowed symbols.
2. `required_amount_raw` for exact amount per token.
3. `payment_address` as the only valid treasury recipient.

## 5. Testnet funding checklist

Before running live execute tests:

1. Resolve wallet with `GET /wallet`.
2. Fund smart wallet with the token you intend to use.
3. Keep a small `USDC` or `aUSD` balance on the same chain you execute on for payment needs in manual or auto key modes.
4. Run `POST /simulate` first.
5. Execute and then poll `GET /status/:id`.

## 6. Getting test assets

Typical paths:

1. Sepolia faucet for ETH.
2. Circle faucet for USDC on Ethereum Sepolia, Base Sepolia, and Arbitrum Sepolia.
3. Ardent `aUSD` faucet request using your agent smart wallet address.

Useful links:

1. ETH testnet faucet (Alchemy): `https://www.alchemy.com/faucets`
2. USDC testnet faucet (Circle): `https://faucet.circle.com`
3. USDT testnet faucet (Candide): `https://dashboard.candide.dev/faucet`
