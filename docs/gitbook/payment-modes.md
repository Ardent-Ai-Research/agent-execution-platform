# Payment Modes

Payment mode is assigned per API key at creation time.

## 1. Manual mode

Manual mode behavior:

1. Payable amount is gas cost plus platform fee.
2. If payment proof is missing, `POST /execute` returns `402 Payment Required`.
3. You submit payment on-chain to the configured treasury address.
4. You re-submit the same execute request with `X-Payment-Proof`.

## 2. Auto mode

Auto mode behavior:

1. Payable amount is gas cost only.
2. If proof is not supplied, platform attempts an internal ERC-20 transfer from the agent smart wallet.
3. Token selection uses the first accepted token in sorted symbol order that can cover required amount.
4. If auto transfer succeeds, execution continues without requiring external proof submission.

## 3. Sponsored mode

Sponsored mode behavior:

1. Payable amount is zero.
2. External payment proof is not required.
3. Execution can proceed directly to queueing when validation and simulation succeed.

## 4. Simulate behavior by mode

`POST /simulate` returns estimated cost that reflects mode policy.

1. Manual includes platform fee.
2. Auto excludes platform fee.
3. Sponsored returns zero payable cost.

## 5. Choosing the right mode

Use this guide:

1. Choose manual when your system wants explicit payment control and accounting certainty.
2. Choose auto when you want less client side payment orchestration and your wallet funding policy supports auto debit.
3. Choose sponsored for managed developer experience where platform absorbs payable amount policy.
