//! Application configuration loaded from environment variables.
//!
//! Supports **multi-chain** operation.  Each supported chain has its own
//! RPC URL, bundler URL, and optional wallet-balance token map.
//! Shared ERC-4337 contracts can be configured once for all EVM chains:
//!
//! ```text
//! EVM_ENTRY_POINT_ADDRESS     (defaults to canonical v0.9)
//! EVM_FACTORY_ADDRESS         (SimpleAccountFactory deployed deterministically)
//! EVM_PAYMASTER_ADDRESS       (VerifyingPaymaster deployed deterministically)
//! ```
//!
//! Chain-specific env vars use the prefix pattern:
//!
//! ```text
//! {CHAIN}_RPC_URL              e.g. ETHEREUM_RPC_URL, BASE_RPC_URL, ARBITRUM_RPC_URL
//! {CHAIN}_BUNDLER_RPC_URL      e.g. ETHEREUM_BUNDLER_RPC_URL
//! {CHAIN}_PAYMASTER_ADDRESS    (optional override for EVM_PAYMASTER_ADDRESS)
//! {CHAIN}_FACTORY_ADDRESS      (optional override for EVM_FACTORY_ADDRESS)
//! {CHAIN}_ENTRY_POINT_ADDRESS  (optional override for EVM_ENTRY_POINT_ADDRESS)
//! {CHAIN}_TRACKED_TOKENS       (TOKEN=0xAddr pairs shown by /wallet/balance)
//! {CHAIN}_TRACKED_TOKEN_DECIMALS (TOKEN=N decimal mappings)
//! ```
//!
use anyhow::Result;
use std::collections::HashMap;
use std::fmt;

use crate::types::Chain;

// ──────────────────────── Per-chain config ───────────────────────────

/// Configuration for a single supported blockchain.
#[derive(Clone)]
pub struct ChainConfig {
    /// Which chain this config is for.
    pub chain: Chain,
    /// JSON-RPC URL of an Ethereum-compatible node for this chain.
    pub rpc_url: String,
    /// JSON-RPC URL of the ERC-4337 bundler for this chain.
    pub bundler_rpc_url: String,
    /// Address of the deployed VerifyingPaymaster contract for this chain.
    /// Empty string disables testnet gas sponsorship for the chain.
    pub paymaster_address: String,
    /// Address of the SimpleAccountFactory contract for this chain.
    pub factory_address: String,
    /// Address of the EntryPoint contract (default: canonical v0.9).
    pub entry_point_address: String,
    /// Token symbols and addresses exposed by the generic wallet balance endpoint.
    pub tracked_tokens: HashMap<String, String>,
    /// Number of decimals for each tracked token on this chain.
    pub tracked_token_decimals: HashMap<String, u8>,
}

impl fmt::Debug for ChainConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChainConfig")
            .field("chain", &self.chain)
            .field("rpc_url", &"[REDACTED]")
            .field("bundler_rpc_url", &"[REDACTED]")
            .field("paymaster_address", &self.paymaster_address)
            .field("factory_address", &self.factory_address)
            .field("entry_point_address", &self.entry_point_address)
            .field("tracked_tokens", &self.tracked_tokens)
            .field("tracked_token_decimals", &self.tracked_token_decimals)
            .finish()
    }
}

// ──────────────────────── Global config ──────────────────────────────

#[derive(Clone)]
pub struct AppConfig {
    // Server
    pub host: String,
    pub port: u16,

    // Database
    pub database_url: String,

    // Redis
    pub redis_url: String,

    // API security
    /// Global concurrency limit: max in-flight requests across all clients.
    /// Default 200.  Set to a high value to effectively disable.
    pub max_concurrent_requests: u64,

    // ERC-4337 Account Abstraction — per-chain
    /// Per-chain configuration.  Only chains present in this map are
    /// considered "supported" at runtime.
    pub chains: HashMap<Chain, ChainConfig>,

    /// Hex-encoded 32-byte AES-256 key for encrypting agent signing keys at rest.
    pub wallet_encryption_key: String,

    // Per-API-key rate limiting
    pub per_key_rate_limit_rps: f64,
    pub per_key_rate_limit_burst: f64,

    // Public API-key issuance
    pub public_api_key_limit: u64,
    pub public_api_key_window_secs: u64,
    /// Header trusted as the client IP only when explicitly configured behind
    /// a proxy that overwrites client-supplied values.
    pub public_api_key_client_ip_header: Option<String>,
}

/// Manual `Debug` impl that redacts secret fields.
impl fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database_url", &"[REDACTED]")
            .field("redis_url", &"[REDACTED]")
            .field("max_concurrent_requests", &self.max_concurrent_requests)
            .field("chains", &self.chains)
            .field("wallet_encryption_key", &"[REDACTED]")
            .field("per_key_rate_limit_rps", &self.per_key_rate_limit_rps)
            .field("per_key_rate_limit_burst", &self.per_key_rate_limit_burst)
            .field("public_api_key_limit", &self.public_api_key_limit)
            .field(
                "public_api_key_window_secs",
                &self.public_api_key_window_secs,
            )
            .field(
                "public_api_key_client_ip_header",
                &self.public_api_key_client_ip_header,
            )
            .finish()
    }
}

impl AppConfig {
    /// Load configuration from environment (dotenv supported).
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        let chains = Self::parse_chains()?;
        let per_key_rate_limit_rps: f64 = std::env::var("PER_KEY_RATE_LIMIT_RPS")
            .unwrap_or_else(|_| "5.0".into())
            .parse()?;
        let per_key_rate_limit_burst: f64 = std::env::var("PER_KEY_RATE_LIMIT_BURST")
            .unwrap_or_else(|_| "10.0".into())
            .parse()?;
        Self::validate_rate_limits(per_key_rate_limit_rps, per_key_rate_limit_burst)?;

        Ok(Self {
            host: std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: std::env::var("PORT")
                .unwrap_or_else(|_| "8080".into())
                .parse()?,
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/agent_exec".into()),
            redis_url: std::env::var("REDIS_URL")
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".into()),
            max_concurrent_requests: std::env::var("MAX_CONCURRENT_REQUESTS")
                .unwrap_or_else(|_| "200".into())
                .parse()?,
            chains,

            wallet_encryption_key: std::env::var("WALLET_ENCRYPTION_KEY")
                .map_err(|_| anyhow::anyhow!(
                    "WALLET_ENCRYPTION_KEY env var is required — it guards every agent signing key. \
                     Generate one with: openssl rand -hex 32"
                ))?,

            // Per-API-key rate limiting
            per_key_rate_limit_rps,
            per_key_rate_limit_burst,
            public_api_key_limit: std::env::var("PUBLIC_API_KEY_LIMIT")
                .unwrap_or_else(|_| "5".into())
                .parse()?,
            public_api_key_window_secs: std::env::var("PUBLIC_API_KEY_WINDOW_SECS")
                .unwrap_or_else(|_| "3600".into())
                .parse()?,
            public_api_key_client_ip_header: std::env::var(
                "PUBLIC_API_KEY_CLIENT_IP_HEADER",
            )
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty()),
        })
    }

    // ──────────────────── Per-chain parsing ──────────────────────────

    fn shared_contract_address(chain_specific: &str, shared: &str) -> String {
        std::env::var(chain_specific)
            .or_else(|_| std::env::var(shared))
            .unwrap_or_default()
    }

    fn shared_entry_point_address(chain_specific: &str, default_entry_point: &str) -> String {
        std::env::var(chain_specific)
            .or_else(|_| std::env::var("EVM_ENTRY_POINT_ADDRESS"))
            .unwrap_or_else(|_| default_entry_point.into())
    }

    fn validate_rate_limits(rate: f64, burst: f64) -> Result<()> {
        if !rate.is_finite() || rate < 0.0 {
            anyhow::bail!("PER_KEY_RATE_LIMIT_RPS must be finite and non-negative");
        }
        if !burst.is_finite() || burst < 1.0 {
            anyhow::bail!("PER_KEY_RATE_LIMIT_BURST must be finite and at least 1");
        }
        Ok(())
    }

    /// Parse chain configurations from environment variables.
    ///
    /// A chain is considered "configured" if its `{CHAIN}_RPC_URL` env var
    /// is set.
    fn parse_chains() -> Result<HashMap<Chain, ChainConfig>> {
        /// Canonical EntryPoint v0.9 — deployed at the same address on every
        /// EVM chain via CREATE2.
        const CANONICAL_EP_V09: &str = "0x433709009B8330FDa32311DF1C2AFA402eD8D009";

        let mut chains = HashMap::new();

        // ── Ethereum ───────────────────────────────────────────────
        if let Ok(rpc_url) = std::env::var("ETHEREUM_RPC_URL") {
            chains.insert(
                Chain::Ethereum,
                ChainConfig {
                    chain: Chain::Ethereum,
                    rpc_url,
                    bundler_rpc_url: std::env::var("ETHEREUM_BUNDLER_RPC_URL").unwrap_or_default(),
                    paymaster_address: Self::shared_contract_address(
                        "ETHEREUM_PAYMASTER_ADDRESS",
                        "EVM_PAYMASTER_ADDRESS",
                    ),
                    factory_address: Self::shared_contract_address(
                        "ETHEREUM_FACTORY_ADDRESS",
                        "EVM_FACTORY_ADDRESS",
                    ),
                    entry_point_address: Self::shared_entry_point_address(
                        "ETHEREUM_ENTRY_POINT_ADDRESS",
                        CANONICAL_EP_V09,
                    ),
                    tracked_tokens: Self::parse_token_map(
                        &std::env::var("ETHEREUM_TRACKED_TOKENS").unwrap_or_default(),
                    ),
                    tracked_token_decimals: Self::parse_decimal_map(
                        &std::env::var("ETHEREUM_TRACKED_TOKEN_DECIMALS").unwrap_or_default(),
                    ),
                },
            );
        }

        // ── Base ───────────────────────────────────────────────────
        if let Ok(rpc_url) = std::env::var("BASE_RPC_URL") {
            chains.insert(
                Chain::Base,
                ChainConfig {
                    chain: Chain::Base,
                    rpc_url,
                    bundler_rpc_url: std::env::var("BASE_BUNDLER_RPC_URL").unwrap_or_default(),
                    paymaster_address: Self::shared_contract_address(
                        "BASE_PAYMASTER_ADDRESS",
                        "EVM_PAYMASTER_ADDRESS",
                    ),
                    factory_address: Self::shared_contract_address(
                        "BASE_FACTORY_ADDRESS",
                        "EVM_FACTORY_ADDRESS",
                    ),
                    entry_point_address: Self::shared_entry_point_address(
                        "BASE_ENTRY_POINT_ADDRESS",
                        CANONICAL_EP_V09,
                    ),
                    tracked_tokens: Self::parse_token_map(
                        &std::env::var("BASE_TRACKED_TOKENS").unwrap_or_default(),
                    ),
                    tracked_token_decimals: Self::parse_decimal_map(
                        &std::env::var("BASE_TRACKED_TOKEN_DECIMALS").unwrap_or_default(),
                    ),
                },
            );
        }

        // ── Arbitrum ───────────────────────────────────────────────
        if let Ok(rpc_url) = std::env::var("ARBITRUM_RPC_URL") {
            chains.insert(
                Chain::Arbitrum,
                ChainConfig {
                    chain: Chain::Arbitrum,
                    rpc_url,
                    bundler_rpc_url: std::env::var("ARBITRUM_BUNDLER_RPC_URL").unwrap_or_default(),
                    paymaster_address: Self::shared_contract_address(
                        "ARBITRUM_PAYMASTER_ADDRESS",
                        "EVM_PAYMASTER_ADDRESS",
                    ),
                    factory_address: Self::shared_contract_address(
                        "ARBITRUM_FACTORY_ADDRESS",
                        "EVM_FACTORY_ADDRESS",
                    ),
                    entry_point_address: Self::shared_entry_point_address(
                        "ARBITRUM_ENTRY_POINT_ADDRESS",
                        CANONICAL_EP_V09,
                    ),
                    tracked_tokens: Self::parse_token_map(
                        &std::env::var("ARBITRUM_TRACKED_TOKENS").unwrap_or_default(),
                    ),
                    tracked_token_decimals: Self::parse_decimal_map(
                        &std::env::var("ARBITRUM_TRACKED_TOKEN_DECIMALS").unwrap_or_default(),
                    ),
                },
            );
        }

        if chains.is_empty() {
            anyhow::bail!(
                "no chains configured — set at least ETHEREUM_RPC_URL, BASE_RPC_URL, or ARBITRUM_RPC_URL"
            );
        }

        Ok(chains)
    }

    // ──────────────────── Token parsing helpers ─────────────────────

    /// Parse a `KEY=VALUE,...` string into a `HashMap<String, String>`.
    ///
    /// Used for per-chain `{CHAIN}_TRACKED_TOKENS` env vars.
    /// Format: `"USDC=0xA0b8...,USDT=0xdAC1..."`
    /// Returns an empty map if the input is empty.
    fn parse_token_map(raw: &str) -> HashMap<String, String> {
        let mut map = HashMap::new();
        if raw.trim().is_empty() {
            return map;
        }
        for pair in raw.split(',') {
            let parts: Vec<&str> = pair.splitn(2, '=').collect();
            if parts.len() == 2 {
                map.insert(parts[0].trim().to_uppercase(), parts[1].trim().to_string());
            }
        }
        map
    }

    /// Parse a `KEY=DECIMALS,...` string into a `HashMap<String, u8>`.
    ///
    /// Used for per-chain `{CHAIN}_TOKEN_DECIMALS` env vars.
    /// Format: `"USDC=6,USDT=6"`
    /// Returns an empty map if the input is empty.
    fn parse_decimal_map(raw: &str) -> HashMap<String, u8> {
        let mut map = HashMap::new();
        if raw.trim().is_empty() {
            return map;
        }
        for pair in raw.split(',') {
            let parts: Vec<&str> = pair.splitn(2, '=').collect();
            if parts.len() == 2 {
                if let Ok(d) = parts[1].trim().parse::<u8>() {
                    map.insert(parts[0].trim().to_uppercase(), d);
                }
            }
        }
        map
    }

    // ──────────────────── Convenience accessors ─────────────────────

    /// Get the chain config for a given chain, or error if not configured.
    pub fn chain_config(&self, chain: &Chain) -> Result<&ChainConfig> {
        self.chains.get(chain).ok_or_else(|| {
            anyhow::anyhow!(
                "chain {} is not configured — set {}_RPC_URL env var to enable it",
                chain,
                chain.to_string().to_uppercase()
            )
        })
    }

    /// List all configured chains.
    pub fn supported_chains(&self) -> Vec<&Chain> {
        self.chains.keys().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn shared_contract_address_prefers_chain_then_shared() {
        let _guard = env_lock().lock().expect("env test lock");
        let chain_var = "TEST_CHAIN_FACTORY_ADDRESS";
        let shared_var = "TEST_EVM_FACTORY_ADDRESS";

        std::env::remove_var(chain_var);
        std::env::remove_var(shared_var);
        assert_eq!(
            AppConfig::shared_contract_address(chain_var, shared_var),
            ""
        );

        std::env::set_var(shared_var, "0xshared");
        assert_eq!(
            AppConfig::shared_contract_address(chain_var, shared_var),
            "0xshared"
        );

        std::env::set_var(chain_var, "0xchain");
        assert_eq!(
            AppConfig::shared_contract_address(chain_var, shared_var),
            "0xchain"
        );

        std::env::remove_var(chain_var);
        std::env::remove_var(shared_var);
    }

    #[test]
    fn debug_output_redacts_connection_urls_and_encryption_key() {
        let config = AppConfig {
            host: "127.0.0.1".into(),
            port: 8080,
            database_url: "postgres://user:secret@localhost/db".into(),
            redis_url: "redis://:secret@localhost".into(),
            max_concurrent_requests: 10,
            chains: HashMap::from([(
                Chain::Base,
                ChainConfig {
                    chain: Chain::Base,
                    rpc_url: "https://rpc.example/secret".into(),
                    bundler_rpc_url: "https://bundler.example/secret".into(),
                    paymaster_address: String::new(),
                    factory_address: String::new(),
                    entry_point_address: String::new(),
                    tracked_tokens: HashMap::new(),
                    tracked_token_decimals: HashMap::new(),
                },
            )]),
            wallet_encryption_key: "super-secret".into(),
            per_key_rate_limit_rps: 5.0,
            per_key_rate_limit_burst: 10.0,
            public_api_key_limit: 5,
            public_api_key_window_secs: 3600,
            public_api_key_client_ip_header: None,
        };

        let debug = format!("{config:?}");
        assert!(!debug.contains("secret"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn rate_limit_configuration_rejects_non_finite_or_negative_values() {
        assert!(AppConfig::validate_rate_limits(5.0, 10.0).is_ok());
        assert!(AppConfig::validate_rate_limits(0.0, 1.0).is_ok());
        assert!(AppConfig::validate_rate_limits(-1.0, 10.0).is_err());
        assert!(AppConfig::validate_rate_limits(f64::NAN, 10.0).is_err());
        assert!(AppConfig::validate_rate_limits(5.0, f64::INFINITY).is_err());
        assert!(AppConfig::validate_rate_limits(5.0, 0.0).is_err());
    }
}
