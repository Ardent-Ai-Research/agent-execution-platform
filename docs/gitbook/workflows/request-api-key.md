# Generate API Key

## Endpoint

`POST /api-keys` is public and independently rate-limited.

```bash
curl -X POST https://api.ardentresearch.xyz/api-keys \
  -H 'Content-Type: application/json' \
  -d '{"label":"my-agent"}'
```

CLI equivalent:

```bash
ardent api-key-create --label my-agent
```

The response contains the raw API key once. Store it in a secret manager or protected environment variable:

```bash
export ARDENT_API_KEY="ak_your_api_key"
```

Verify it by resolving a wallet:

```bash
curl "https://api.ardentresearch.xyz/wallet?agent_id=my-agent&chain=ethereum" \
  -H "X-API-Key: $ARDENT_API_KEY"
```

Never commit the key or expose it in frontend code. If it is lost or exposed, generate a replacement and migrate intentionally; the original plaintext cannot be recovered from the server.
