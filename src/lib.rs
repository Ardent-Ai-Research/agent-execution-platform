//! AI Agent Blockchain Execution Platform — library root.
//!
//! All modules are declared here so they can be imported with
//! `use agent_execution_platform::<module>`.

pub mod api;
pub mod config;
pub mod db;
pub mod execution_engine;
pub mod payments;
pub mod queue;
pub mod relayer;
pub mod types;
pub mod worker;
