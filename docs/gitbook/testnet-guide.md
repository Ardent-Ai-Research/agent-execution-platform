# Testnet Guide

## Networks

| Payload chain | Execution network |
| --- | --- |
| `ethereum` | Ethereum Sepolia |
| `base` | Base Sepolia |
| `arbitrum` | Arbitrum Sepolia |

## Funding agent wallets

The platform sponsors UserOperation gas through its paymaster. The smart wallet must still hold assets consumed by the requested action and any native value forwarded to a protocol.

Typical faucet sources:

1. Test ETH: `https://www.alchemy.com/faucets`
2. Circle test USDC: `https://faucet.circle.com`

Circle test USDC addresses:

| Network | Address |
| --- | --- |
| Ethereum Sepolia | `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238` |
| Base Sepolia | `0x036CbD53842c5426634e7929541eC2318f3dCF7e` |
| Arbitrum Sepolia | `0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d` |

Protocol test assets and addresses differ by network. Use the corresponding protocol read/discovery endpoints and official faucets before executing.

## Checklist

1. Generate an API key.
2. Resolve the smart wallet for a stable `agent_id`.
3. Fund required protocol assets.
4. For GMX, fund the native keeper fee specified by `execution_fee_raw`.
5. Simulate first.
6. Execute and monitor the returned request ID.
