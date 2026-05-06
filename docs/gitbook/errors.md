# Errors and Troubleshooting

This section lists common errors and clear corrective actions.

## 1. Authentication errors

### Missing API key

Response:

```json
{ "error": "missing X-API-Key header" }
```

Action:

1. Add `X-API-Key` header to protected endpoint requests.

### Invalid API key

Response:

```json
{ "error": "invalid API key" }
```

Action:

1. Verify key value.
2. Ensure key is active.
3. Request key rotation or re-issue through Ardent support if needed.

## 2. Payment errors

### Payment required

Response status: `402`

Response body includes payable amount, accepted tokens, `required_amount_raw`, and treasury address.

Action:

1. Settle payment transfer on-chain.
2. Re-submit execute with `X-Payment-Proof`.

### Payment verification failed

Response:

```json
{
  "error": "payment_verification_failed",
  "reason": "..."
}
```

Action:

1. Confirm token is accepted for chain.
2. Confirm transfer recipient equals platform treasury.
3. Confirm payer matches header value.
4. Confirm transaction confirmations satisfy policy.
5. Confirm tx hash has not already been used.

## 3. Validation errors

Examples include unsupported chain, malformed calldata, invalid target contract, or missing required fields.

Action:

1. Validate request payload schema client side.
2. Use canonical chain names.
3. Ensure calldata is `0x` prefixed and ABI encoded correctly.

## 4. Operational errors

### No bundler configured

Action:

1. Retry once after short delay.
2. If repeated, contact Ardent support with timestamp, chain, and request ID.

### Queue or infrastructure failures

Action:

1. Retry request with backoff.
2. If persistent, contact Ardent support with request ID and failure window.

## 5. Recommended client retry policy

1. Retry idempotent reads such as status polling with jitter.
2. Avoid blind retries for execute on unknown state. Query status first using known request ID.
3. Handle `402` explicitly rather than generic retry.
