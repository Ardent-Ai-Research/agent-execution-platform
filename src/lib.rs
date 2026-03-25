//! AI Agent Blockchain Execution Platform — library root (hackathon edition).
//!
//! Stripped-down: no DB, no Redis, no payments, no workers.
//! Synchronous inline execution with in-memory state.

pub mod api;
pub mod config;
pub mod execution_engine;
pub mod relayer;
pub mod types;
