//! Application configuration loaded from environment variables.
//!
//! Supports **multi-chain** operation.  Each supported chain has its own
//! RPC URL, bundler URL, paymaster address, factory address, and native-
//! token price feed.  Chain-specific env vars use the prefix pattern:
//!
//! ```text
//! {CHAIN}_RPC_URL              e.g. ETHEREUM_RPC_URL, BASE_RPC_URL, BNB_RPC_URL
//! {CHAIN}_BUNDLER_RPC_URL      e.g. ETHEREUM_BUNDLER_RPC_URL
//! {CHAIN}_PAYMASTER_ADDRESS
//! {CHAIN}_FACTORY_ADDRESS
//! {CHAIN}_ENTRY_POINT_ADDRESS  (defaults to canonical v0.9 on all chains)
//! {CHAIN}_PRICE_FEED_URL       (native token / USD source)
//! {CHAIN}_ACCEPTED_TOKENS      (TOKEN=0xAddr pairs for payment verification)
//! {CHAIN}_TOKEN_DECIMALS       (TOKEN=N decimal mappings)
//! ```
//!
//! Legacy single-chain env vars (`BUNDLER_RPC_URL`, `PAYMASTER_ADDRESS`,
//! `ENTRY_POINT_ADDRESS`, `ACCOUNT_FACTORY_ADDRESS`, `ETH_PRICE_FEED_URL`)
//! are still accepted as fallbacks for the Ethereum chain.

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
    /// Address of the deployed VerifyingPaymaster contract on this chain.
    /// Empty string means paymaster is not configured (agents self-fund).
    pub paymaster_address: String,
    /// Address of the SimpleAccountFactory contract on this chain.
    pub factory_address: String,
    /// Address of the EntryPoint contract (default: canonical v0.9).
    pub entry_point_address: String,
    /// Native-token/USD source.
    ///
    /// Supported formats:
    /// - `chainlink://0x...` (on-chain Chainlink AggregatorV3 proxy)
    /// - `0x...` (same as above)
    /// - `https://...` JSON endpoint that returns a `usd` field
    pub price_feed_url: String,
    /// Accepted payment token symbols → contract addresses on this chain.
    /// e.g. `{"USDC": "0x833589fC...", "USDT": "0xfde4C96c..."}`
    pub accepted_tokens: HashMap<String, String>,
    /// Number of decimals for each accepted token on this chain.
    pub token_decimals: HashMap<String, u8>,
}

impl fmt::Debug for ChainConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChainConfig")
            .field("chain", &self.chain)
            .field("rpc_url", &self.rpc_url)
            .field("bundler_rpc_url", &self.bundler_rpc_url)
            .field("paymaster_address", &self.paymaster_address)
            .field("factory_address", &self.factory_address)
            .field("entry_point_address", &self.entry_point_address)
            .field("price_feed_url", &self.price_feed_url)
            .field("accepted_tokens", &self.accepted_tokens)
            .field("token_decimals", &self.token_decimals)
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

    // Pricing
    pub gas_price_markup_pct: f64,
    pub platform_fee_usd: f64,
    /// How long (seconds) to cache the native-token/USD price before re-fetching.
    pub price_cache_ttl_secs: u64,

    // Payment verification
    /// Platform treasury address that must be the recipient of payment transfers.
    pub payment_address: String,
    /// Minimum block confirmations required before accepting a payment.
    pub min_payment_confirmations: u64,

    // ERC-4337 Account Abstraction — per-chain
    /// Per-chain configuration.  Only chains present in this map are
    /// considered "supported" at runtime.
    pub chains: HashMap<Chain, ChainConfig>,

    /// Hex-encoded 32-byte AES-256 key for encrypting agent signing keys at rest.
    pub wallet_encryption_key: String,

    // Per-API-key rate limiting
    pub per_key_rate_limit_rps: f64,
    pub per_key_rate_limit_burst: f64,
}

/// Manual `Debug` impl that redacts secret fields.
impl fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database_url", &"[REDACTED]")
            .field("redis_url", &self.redis_url)
            .field("max_concurrent_requests", &self.max_concurrent_requests)
            .field("gas_price_markup_pct", &self.gas_price_markup_pct)
            .field("platform_fee_usd", &self.platform_fee_usd)
            .field("price_cache_ttl_secs", &self.price_cache_ttl_secs)
            .field("payment_address", &self.payment_address)
            .field("min_payment_confirmations", &self.min_payment_confirmations)
            .field("chains", &self.chains)
            .field("wallet_encryption_key", &"[REDACTED]")
            .field("per_key_rate_limit_rps", &self.per_key_rate_limit_rps)
            .field("per_key_rate_limit_burst", &self.per_key_rate_limit_burst)
            .finish()
    }
}

impl AppConfig {
    /// Load configuration from environment (dotenv supported).
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        let chains = Self::parse_chains()?;

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
            gas_price_markup_pct: std::env::var("GAS_PRICE_MARKUP_PCT")
                .unwrap_or_else(|_| "10.0".into())
                .parse()?,
            platform_fee_usd: std::env::var("PLATFORM_FEE_USD")
                .unwrap_or_else(|_| "0.01".into())
                .parse()?,
            price_cache_ttl_secs: std::env::var("PRICE_CACHE_TTL_SECS")
                .ok()
                .or_else(|| std::env::var("ETH_PRICE_CACHE_TTL_SECS").ok()) // legacy
                .unwrap_or_else(|| "60".into())
                .parse()?,

            // Payment verification
            payment_address: std::env::var("PAYMENT_ADDRESS")
                .map_err(|_| anyhow::anyhow!(
                    "PAYMENT_ADDRESS env var is required — refusing to start with a default address"
                ))?,
            min_payment_confirmations: std::env::var("MIN_PAYMENT_CONFIRMATIONS")
                .unwrap_or_else(|_| "1".into())
                .parse()?,

            chains,

            wallet_encryption_key: std::env::var("WALLET_ENCRYPTION_KEY")
                .map_err(|_| anyhow::anyhow!(
                    "WALLET_ENCRYPTION_KEY env var is required — it guards every agent signing key. \
                     Generate one with: openssl rand -hex 32"
                ))?,

            // Per-API-key rate limiting
            per_key_rate_limit_rps: std::env::var("PER_KEY_RATE_LIMIT_RPS")
                .unwrap_or_else(|_| "5.0".into())
                .parse()?,
            per_key_rate_limit_burst: std::env::var("PER_KEY_RATE_LIMIT_BURST")
                .unwrap_or_else(|_| "10.0".into())
                .parse()?,
        })
    }

    // ──────────────────── Per-chain parsing ──────────────────────────

    /// Parse chain configurations from environment variables.
    ///
    /// A chain is considered "configured" if its `{CHAIN}_RPC_URL` env var
    /// is set.  Legacy single-chain env vars (e.g. `BUNDLER_RPC_URL`) are
    /// accepted as fallbacks for the Ethereum chain.
    fn parse_chains() -> Result<HashMap<Chain, ChainConfig>> {
        /// Canonical EntryPoint v0.9 — deployed at the same address on every
        /// EVM chain via CREATE2.
        const CANONICAL_EP_V09: &str = "0x433709009B8330FDa32311DF1C2AFA402eD8D009";

        let mut chains = HashMap::new();

        // ── Ethereum ───────────────────────────────────────────────
        if let Ok(rpc_url) = std::env::var("ETHEREUM_RPC_URL") {
            chains.insert(Chain::Ethereum, ChainConfig {
                chain: Chain::Ethereum,
                rpc_url,
                bundler_rpc_url: std::env::var("ETHEREUM_BUNDLER_RPC_URL")
                    .or_else(|_| std::env::var("BUNDLER_RPC_URL")) // legacy
                    .unwrap_or_else(|_| "http://127.0.0.1:3000/rpc".into()),
                paymaster_address: std::env::var("ETHEREUM_PAYMASTER_ADDRESS")
                    .or_else(|_| std::env::var("PAYMASTER_ADDRESS")) // legacy
                    .unwrap_or_default(),
                factory_address: std::env::var("ETHEREUM_FACTORY_ADDRESS")
                    .or_else(|_| std::env::var("ACCOUNT_FACTORY_ADDRESS")) // legacy
                    .unwrap_or_default(),
                entry_point_address: std::env::var("ETHEREUM_ENTRY_POINT_ADDRESS")
                    .or_else(|_| std::env::var("ENTRY_POINT_ADDRESS")) // legacy
                    .unwrap_or_else(|_| CANONICAL_EP_V09.into()),
                price_feed_url: std::env::var("ETHEREUM_PRICE_FEED_URL")
                    .or_else(|_| std::env::var("ETH_PRICE_FEED_URL")) // legacy
                    .map_err(|_| anyhow::anyhow!(
                        "ETHEREUM_PRICE_FEED_URL is required when ETHEREUM_RPC_URL is set"
                    ))?,
                accepted_tokens: Self::parse_token_map(
                    &std::env::var("ETHEREUM_ACCEPTED_TOKENS").unwrap_or_default(),
                ),
                token_decimals: Self::parse_decimal_map(
                    &std::env::var("ETHEREUM_TOKEN_DECIMALS").unwrap_or_default(),
                ),
            });
        }

        // ── Base ───────────────────────────────────────────────────
        if let Ok(rpc_url) = std::env::var("BASE_RPC_URL") {
            chains.insert(Chain::Base, ChainConfig {
                chain: Chain::Base,
                rpc_url,
                bundler_rpc_url: std::env::var("BASE_BUNDLER_RPC_URL")
                    .unwrap_or_default(),
                paymaster_address: std::env::var("BASE_PAYMASTER_ADDRESS")
                    .unwrap_or_default(),
                factory_address: std::env::var("BASE_FACTORY_ADDRESS")
                    .unwrap_or_default(),
                entry_point_address: std::env::var("BASE_ENTRY_POINT_ADDRESS")
                    .unwrap_or_else(|_| CANONICAL_EP_V09.into()),
                // Base uses ETH as native gas token, but URL must still be configured explicitly.
                price_feed_url: std::env::var("BASE_PRICE_FEED_URL")
                    .map_err(|_| anyhow::anyhow!(
                        "BASE_PRICE_FEED_URL is required when BASE_RPC_URL is set"
                    ))?,
                accepted_tokens: Self::parse_token_map(
                    &std::env::var("BASE_ACCEPTED_TOKENS").unwrap_or_default(),
                ),
                token_decimals: Self::parse_decimal_map(
                    &std::env::var("BASE_TOKEN_DECIMALS").unwrap_or_default(),
                ),
            });
        }

        // ── BNB Chain (BSC) ────────────────────────────────────────
        if let Ok(rpc_url) = std::env::var("BNB_RPC_URL") {
            chains.insert(Chain::Bnb, ChainConfig {
                chain: Chain::Bnb,
                rpc_url,
                bundler_rpc_url: std::env::var("BNB_BUNDLER_RPC_URL")
                    .unwrap_or_default(),
                paymaster_address: std::env::var("BNB_PAYMASTER_ADDRESS")
                    .unwrap_or_default(),
                factory_address: std::env::var("BNB_FACTORY_ADDRESS")
                    .unwrap_or_default(),
                entry_point_address: std::env::var("BNB_ENTRY_POINT_ADDRESS")
                    .unwrap_or_else(|_| CANONICAL_EP_V09.into()),
                price_feed_url: std::env::var("BNB_PRICE_FEED_URL")
                    .map_err(|_| anyhow::anyhow!(
                        "BNB_PRICE_FEED_URL is required when BNB_RPC_URL is set"
                    ))?,
                accepted_tokens: Self::parse_token_map(
                    &std::env::var("BNB_ACCEPTED_TOKENS").unwrap_or_default(),
                ),
                token_decimals: Self::parse_decimal_map(
                    &std::env::var("BNB_TOKEN_DECIMALS").unwrap_or_default(),
                ),
            });
        }

        if chains.is_empty() {
            anyhow::bail!(
                "no chains configured — set at least ETHEREUM_RPC_URL, BASE_RPC_URL, or BNB_RPC_URL"
            );
        }

        Ok(chains)
    }

    // ──────────────────── Token parsing helpers ─────────────────────

    /// Parse a `KEY=VALUE,...` string into a `HashMap<String, String>`.
    ///
    /// Used for per-chain `{CHAIN}_ACCEPTED_TOKENS` env vars.
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
