//! Application configuration — hackathon edition.
//!
//! Uses Anvil default account #0 private key when RELAYER_PRIVATE_KEY is unset.
//! No database, no Redis, no payment verification config.

use anyhow::Result;

/// Anvil default account #0 private key (well-known, funded on local devnets).
const ANVIL_DEFAULT_KEY: &str =
    "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub ethereum_rpc_url: String,
    pub relayer_private_key: String,
}

impl AppConfig {
    /// Load configuration from environment (dotenv supported).
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        Ok(Self {
            host: std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: std::env::var("PORT")
                .unwrap_or_else(|_| "8080".into())
                .parse()?,
            ethereum_rpc_url: std::env::var("ETHEREUM_RPC_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8545".into()),
            relayer_private_key: std::env::var("RELAYER_PRIVATE_KEY")
                .unwrap_or_else(|_| ANVIL_DEFAULT_KEY.into()),
        })
    }
}
