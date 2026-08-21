# Authentication and Keys

## Public generation

`POST /api-keys` requires no existing credential. It accepts an optional label of at most 100 characters and is rate-limited by client IP.

```bash
curl -X POST https://api.ardentresearch.xyz/api-keys \
  -H 'Content-Type: application/json' \
  -d '{"label":"research-agent"}'
```

The raw API key contains 256 bits of cryptographic randomness, is returned once with `Cache-Control: no-store`, and is never stored in plaintext.

## Protected requests

Send the key on wallet, status, simulation, execution, and protocol endpoints:

```text
X-API-Key: ak_your_api_key
```

Keep API keys in server-side secret storage or environment variables. Do not embed them in browser code, mobile binaries, prompts, logs, or source control.

```bash
export ARDENT_API_KEY="ak_your_api_key"
export ARDENT_BASE_URL="https://api.ardentresearch.xyz"
```

One key may own many agent wallets. Wallet identity is namespaced by the API-key ID and the stable `agent_id` supplied by the caller.

There is currently no recovery endpoint for a lost raw key. Generate a replacement key and migrate agents deliberately.
