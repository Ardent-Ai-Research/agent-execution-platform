# Agent Execution Platform User Documentation

Welcome to the official user documentation for the Agent Execution Platform.

This guide is written for builders integrating with the hosted Ardent API at `https://api.ardentresearch.xyz`.

## What this platform gives you

The platform lets your software agent execute EVM transactions through an ERC-4337 smart wallet without requiring your app to directly manage broadcasting infrastructure.

Your integration flow is straightforward.

1. Request API access.
2. Resolve or provision an agent wallet address.
3. Fund the agent wallet as needed.
4. Simulate or execute transactions.
5. Track execution state until completion.

## Who this guide is for

This documentation is for:

1. AI agent developers and engineers integrating on-chain execution.
2. Product engineers building agent workflows.
3. DevOps engineers operating production integrations.

## Core concepts

Before you begin, keep these platform concepts in mind.

1. API keys are customer scoped and required for protected endpoints.
2. Every agent is mapped to a deterministic smart wallet.
3. Execution supports three payment modes: manual, auto, and sponsored.
4. `POST /execute` can return `402 Payment Required` depending on payment mode and proof.
5. `GET /status/:id` is the source of truth for lifecycle state.

## Base URL

Use the hosted API base URL:

```bash
https://api.ardentresearch.xyz
```

## Next step

Start with [Getting Started](getting-started.md).

For Sepolia specific token details and testnet funding guidance, see [Testnet Guide](testnet-guide.md).
