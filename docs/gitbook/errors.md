# Errors and Troubleshooting

## Authentication

`401 missing X-API-Key header` means a protected endpoint was called without a key. `401 invalid API key` means the supplied key is unknown, inactive, or mistyped.

## API-key issuance

`429 api_key_issuance_rate_limit_exceeded` includes `retry_after_secs`. Wait before trying again. A `503` means the issuance limiter or client-address context is unavailable; generation fails closed.

## Validation

`400` responses cover unsupported chains, malformed addresses or calldata, missing amounts, invalid assets, unsafe health factors, and unsupported protocol state. Correct the payload and simulate again.

## Simulation

A simulation failure means the exact call or UserOperation is expected to revert. Common causes include insufficient wallet assets, missing approval, paused pools, unavailable liquidity, or insufficient native value for protocol-required calls.

## Operations

`no bundler configured` means the selected chain lacks a bundler URL. Queue, database, Redis, RPC, or bundler outages generally return `500` or `503` and may be retried with backoff.

Do not blindly retry validation failures or on-chain reverts. For transient infrastructure failures, use exponential backoff and retain the original request ID.
