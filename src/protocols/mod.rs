//! Protocol-specific typed action adapters.
//!
//! These modules translate higher-level DeFi actions into the raw
//! `ExecutionRequest`/`BatchCall` shape consumed by the generic ERC-4337
//! executor.

pub mod aave_v3;
pub mod balancer_v3;
pub mod compound_v3;
pub mod gmx_v2;
