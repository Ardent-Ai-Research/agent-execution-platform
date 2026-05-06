# Testnet Guide

This page contains practical details for users integrating with the hosted testnet environment.

## 1. Network context

For the current hosted testnet program, use:

1. `chain: "ethereum"` in request payloads.
2. Ethereum Sepolia as the execution network context.

## 2. Accepted x402 payment tokens on Ethereum Sepolia

1. `USDC` at `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238`
2. `USDT` at `0xd077A400968890Eacc75cdc901F0356c943e4fDb`
3. `aUSD` at `0x112a19d6236016fc4dda49257c724E63a3CE5bEA`

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
3. Keep a small Sepolia `USDC`, `USDT`, or `aUSD` balance for payment needs in manual or auto key modes.
4. Run `POST /simulate` first.
5. Execute and then poll `GET /status/:id`.

## 6. Getting test assets

Typical paths:

1. Sepolia faucet for ETH.
2. Existing Sepolia token faucets or bridge sources for USDC and USDT when available.
3. Ardent `aUSD` faucet request using your agent smart wallet address.

Useful links:

1. ETH testnet faucet (Alchemy): `https://www.alchemy.com/faucets`
2. USDT testnet faucet (Candide): `https://dashboard.candide.dev/faucet`
