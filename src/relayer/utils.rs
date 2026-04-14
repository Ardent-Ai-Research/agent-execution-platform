//! Shared utility functions for the relayer subsystem.

use anyhow::{Context, Result};
use ethers::types::U256;

/// Parse a hex-encoded string (with optional `0x` prefix) into a `U256`.
pub fn parse_hex_u256(s: &str) -> Result<U256> {
    let stripped = s.trim_start_matches("0x");
    if stripped.is_empty() {
        return Ok(U256::zero());
    }
    U256::from_str_radix(stripped, 16).context("invalid hex U256")
}

/// Parse a hex-encoded string (with optional `0x` prefix) into raw bytes.
pub fn parse_hex_bytes(s: &str) -> Result<Vec<u8>> {
    let stripped = s.trim_start_matches("0x");
    if stripped.is_empty() {
        return Ok(Vec::new());
    }
    hex::decode(stripped).context("invalid hex bytes")
}
