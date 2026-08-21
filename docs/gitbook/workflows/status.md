# Check Request Status

Use this endpoint to retrieve the latest state for an execution request.

## Endpoint

`GET /status/:id`

## Authentication

`X-API-Key` required.

The request is visible only to the API key that created it. A valid key from a different account receives `404 Not Found`.

## Command

```bash
curl "$BASE_URL/status/your_request_id" \
  -H "X-API-Key: $API_KEY"
```

## Success response

Status code: `200 OK`

Example:

```json
{
  "request_id": "your_request_id",
  "status": "broadcasting",
  "chain": "ethereum",
  "tx_hash": "0x1234...abcd",
  "created_at": "2026-05-05T10:00:00Z",
  "updated_at": "2026-05-05T10:00:12Z"
}
```

## CLI equivalent

```bash
ardent status --request-id your_request_id
```

See [Agent Integration](../agent-integration.md) for the one-line installer.

## Status lifecycle

Typical progression:

1. `pending`
2. `queued`
3. `broadcasting`
4. Terminal state
   1. `confirmed`
   2. `reverted`
   3. `failed`

## Polling recommendation

Use interval polling between 2 and 5 seconds until terminal state, then stop.
