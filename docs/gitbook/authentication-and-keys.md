# Authentication and Keys

This page explains authentication for users integrating with the hosted API.

## 1. API key authentication

Protected endpoints require `X-API-Key`.

Header format:

```bash
X-API-Key: <api_key>
```

Protected endpoints:

1. `GET /wallet`
2. `POST /simulate`
3. `POST /execute`
4. `GET /status/:id`

## 2. Payment proof header

For manual payment flow, after you settle payment on-chain, include `X-Payment-Proof` with JSON payload.

Header format:

```bash
X-Payment-Proof: {"request_id":"your_request_id","payer":"0xYourPayer","token":"USDC","chain":"ethereum","tx_hash":"0xYourPaymentTxHash"}
```

Optional field:

1. `request_id` can be included when re-submitting against a specific quote context.

## 3. API key lifecycle

Hosted API users request keys through the onboarding channel.

Security behavior:

1. Keep key secret in server side configuration.
2. Do not hardcode keys in frontend code.
3. Request key rotation if exposure is suspected.

## 4. Recommended local shell setup

For raw curl usage:

```bash
BASE_URL="https://api.ardentresearch.xyz"
API_KEY="ak_your_key"
```

## 5. CLI and MCP server environment variables

The `ardent` CLI and the MCP server both resolve credentials from environment variables, not shell variables.

```bash
export ARDENT_API_KEY="ak_your_key"         # required for protected commands
export ARDENT_BASE_URL="https://api.ardentresearch.xyz"  # optional, shown for reference
```

`ARDENT_BASE_URL` defaults to `https://api.ardentresearch.xyz` when not set. Override it only when pointing at a self-hosted instance or a staging environment.

If `ARDENT_API_KEY` is not exported before running the CLI or starting the MCP server, protected commands will return an authentication error from the API.
