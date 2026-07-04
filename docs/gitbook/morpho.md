# Morpho Blue Base Sepolia

Ardent exposes typed Morpho Blue lending and borrowing actions on Base Sepolia using `chain: "base"`.

Morpho markets are permissionless and isolated. A market is identified by the hash of its loan token, collateral token, oracle, interest-rate model, and LLTV. Always inspect a market before using it.

## Contracts and default market

| Item | Address or ID |
| --- | --- |
| Morpho Blue | `0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb` |
| Default USDC/WETH 86% LLTV market | `0xd39b446f5f67c78fa8be78efab5715845290d1cfb12874f5f0b245d638a153a8` |
| Loan token | Base Sepolia USDC, `0x036CbD53842c5426634e7929541eC2318f3dCF7e` |
| Collateral token | Base Sepolia WETH, `0x4200000000000000000000000000000000000006` |

The default market is provided for convenience and has a currently responsive oracle; it is not an official Morpho endorsement or allowlist. Pass `--market-id` to use any other created Base Sepolia market. Ardent resolves its immutable parameters directly from `Morpho.idToMarketParams`; callers do not supply token, oracle, IRM, or LLTV addresses separately.

Testnet liquidity is not guaranteed and can be very small. Check `ardent morpho-market` before borrowing. If liquidity is insufficient, a tester can supply USDC to the market first.

Base Sepolia USDC is available from Circle's faucet. WETH is obtained by wrapping Base Sepolia ETH through the canonical WETH contract shown above.

## Commands

```bash
ardent morpho-market
ardent morpho-position --agent-id my-agent-001

ardent morpho-supply-simulate --agent-id my-agent-001 --amount 10
ardent morpho-supply --agent-id my-agent-001 --amount 10
ardent morpho-withdraw-simulate --agent-id my-agent-001 --amount max

ardent morpho-supply-collateral-simulate --agent-id my-agent-001 --amount 0.01
ardent morpho-supply-collateral --agent-id my-agent-001 --amount 0.01
ardent morpho-borrow-simulate --agent-id my-agent-001 --amount 5 --min-health-factor 1.10
ardent morpho-borrow --agent-id my-agent-001 --amount 5 --min-health-factor 1.10
ardent morpho-repay-simulate --agent-id my-agent-001 --amount max
ardent morpho-repay --agent-id my-agent-001 --amount max
ardent morpho-withdraw-collateral-simulate --agent-id my-agent-001 --amount 0.001
```

Every execute command has a matching `-simulate` command and follows the normal UserOperation simulation path.

## Read market and position

`morpho-market` returns token metadata, immutable market parameters, oracle price, accrued supply and borrow totals, available liquidity, utilization, fee, and the annualized average rate that would be used for pending interest accrual at the read block.

`morpho-position` returns:

- wallet USDC and WETH balances
- supplied loan assets and supply shares
- borrowed assets and borrow shares
- supplied collateral
- collateral value in loan-asset units
- borrow capacity and available borrow
- current LTV, LLTV, health factor, and health status

Interest accrued since the market's last on-chain update is included using Morpho's IRM and canonical Taylor-compounding formula.

## Amount behavior

| Action | Action token | `amount=max` |
| --- | --- | --- |
| Supply | Loan token | Entire wallet balance |
| Withdraw | Loan token | All supply shares |
| Supply collateral | Collateral token | Entire wallet balance |
| Withdraw collateral | Collateral token | Only when borrow shares are zero |
| Borrow | Loan token | Not supported; use an explicit amount below available capacity |
| Repay | Loan token | All borrow shares |

Full withdrawals and repayments use Morpho shares rather than an estimated asset amount. This avoids residual supply or debt caused by interest and conversion rounding.

A full share repayment can revert if another transaction repays part of the same
position before this transaction is included. Refresh the position and retry
`amount=max` if that race occurs.

## Safety

- Read `morpho-position` immediately before borrowing or withdrawing collateral.
- Borrow and collateral withdrawal enforce a projected minimum health factor of `1.05` by default. `--min-health-factor` may raise it, but cannot lower it below `1.0`.
- Do not borrow the displayed maximum. Leave a meaningful safety margin below the market LLTV.
- A market being permissionlessly created does not make its tokens, oracle, or IRM trustworthy.
- Execution simulation remains the final pre-flight check, and Morpho enforces position health and market liquidity on-chain.
- ERC-20 allowances are reset to zero before use, set only for the action, and cleared to zero afterward in the same atomic UserOperation.
