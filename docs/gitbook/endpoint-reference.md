# Endpoint Reference

This page gives a compact endpoint map for quick lookup.

This reference includes hosted user facing endpoints only.

## Public endpoints

1. `GET /health`
2. `GET /feed/recent`

## Protected endpoints

1. `GET /wallet`
2. `POST /simulate`
3. `POST /execute`
4. `GET /status/:id`

Authentication:

```bash
X-API-Key: <api_key>
```

Optional execution payment header (for manual API key users):

```bash
X-Payment-Proof: {"payer":"0x...","token":"USDC","chain":"ethereum","tx_hash":"0x..."}
```

## Standard content type

For JSON request bodies:

```bash
Content-Type: application/json
```

## Common status codes

1. `200 OK`
2. `400 Bad Request`
3. `401 Unauthorized`
4. `402 Payment Required`
5. `404 Not Found`
6. `429 Too Many Requests`
7. `500 Internal Server Error`
