//! Application configuration loaded from environment variables.

use anyhow::Result;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct AppConfig {
    // Server
    pub host: String,
    pub port: u16,

    // Database
    pub database_url: String,

    // Redis
    pub redis_url: String,

    // Ethereum RPC
    pub ethereum_rpc_url: String,

    // Relayer private key (hex-encoded, no 0x prefix stored).
    // REQUIRED in production — the service refuses to start without it.
    pub relayer_private_key: String,

    // API security
    /// Optional API key.  If set, all requests must include `X-API-Key` header.
    /// Leave unset to disable API key auth (local dev only).
    pub api_key: Option<String>,
    /// Global concurrency limit: max in-flight requests across all clients.
    /// Default 50.  Set to a high value to effectively disable.
    pub max_concurrent_requests: u64,

    // Pricing
    pub gas_price_markup_pct: f64,
    pub platform_fee_usd: f64,
    /// URL for fetching ETH/USD price. Default: CoinGecko v3 simple/price.
    pub eth_price_feed_url: String,
    /// How long (seconds) to cache the ETH/USD price before re-fetching.
    pub eth_price_cache_ttl_secs: u64,

    // Payment verification
    /// Platform treasury address that must be the recipient of payment transfers.
    pub payment_address: String,
    /// Minimum block confirmations required before accepting a payment.
    pub min_payment_confirmations: u64,
    /// Mapping of accepted token symbols → contract addresses (checksummed).
    /// e.g. "USDC" → "0xA0b8..." , "USDT" → "0xdAC1..."
    pub accepted_tokens: HashMap<String, String>,
    /// Number of decimals for each accepted token (e.g. USDC=6, USDT=6).
    pub token_decimals: HashMap<String, u8>,
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
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/agent_exec".into()),
            redis_url: std::env::var("REDIS_URL")
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".into()),
            ethereum_rpc_url: std::env::var("ETHEREUM_RPC_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8545".into()),
            relayer_private_key: std::env::var("RELAYER_PRIVATE_KEY")
                .map_err(|_| anyhow::anyhow!(
                    "RELAYER_PRIVATE_KEY env var is required — refusing to start with a default key"
                ))?,
            api_key: std::env::var("API_KEY").ok().filter(|s| !s.is_empty()),
            max_concurrent_requests: std::env::var("MAX_CONCURRENT_REQUESTS")
                .unwrap_or_else(|_| "50".into())
                .parse()?,
            gas_price_markup_pct: std::env::var("GAS_PRICE_MARKUP_PCT")
                .unwrap_or_else(|_| "10.0".into())
                .parse()?,
            platform_fee_usd: std::env::var("PLATFORM_FEE_USD")
                .unwrap_or_else(|_| "0.01".into())
                .parse()?,
            eth_price_feed_url: std::env::var("ETH_PRICE_FEED_URL").unwrap_or_else(|_| {
                "https://api.coingecko.com/api/v3/simple/price?ids=ethereum&vs_currencies=usd"
                    .into()
            }),
            eth_price_cache_ttl_secs: std::env::var("ETH_PRICE_CACHE_TTL_SECS")
                .unwrap_or_else(|_| "60".into())
                .parse()?,

            // Payment verification
            payment_address: std::env::var("PAYMENT_ADDRESS")
                .map_err(|_| anyhow::anyhow!(
                    "PAYMENT_ADDRESS env var is required — refusing to start with a default address"
                ))?,

            min_payment_confirmations: std::env::var("MIN_PAYMENT_CONFIRMATIONS")
                .unwrap_or_else(|_| "1".into())
                .parse()?,
            accepted_tokens: Self::parse_accepted_tokens()?,
            token_decimals: Self::parse_token_decimals()?,
        })
    }

    /// Parse ACCEPTED_TOKENS env var.
    /// Format: "USDC=0xA0b8...,USDT=0xdAC1..."
    /// Falls back to well-known Ethereum mainnet stablecoin addresses.
    fn parse_accepted_tokens() -> Result<HashMap<String, String>> {
        let raw = std::env::var("ACCEPTED_TOKENS").unwrap_or_else(|_| {
            // Default: well-known Ethereum mainnet addresses
            "USDC=0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48,USDT=0xdAC17F958D2ee523a2206206994597C13D831ec7".into()
        });
        let mut map = HashMap::new();
        for pair in raw.split(',') {
            let parts: Vec<&str> = pair.splitn(2, '=').collect();
            if parts.len() == 2 {
                map.insert(parts[0].trim().to_uppercase(), parts[1].trim().to_string());
            }
        }
        if map.is_empty() {
            anyhow::bail!("ACCEPTED_TOKENS must contain at least one TOKEN=ADDRESS pair");
        }
        Ok(map)
    }

    /// Parse TOKEN_DECIMALS env var.
    /// Format: "USDC=6,USDT=6"  Falls back to 6 for known stablecoins.
    fn parse_token_decimals() -> Result<HashMap<String, u8>> {
        let raw = std::env::var("TOKEN_DECIMALS").unwrap_or_else(|_| {
            "USDC=6,USDT=6".into()
        });
        let mut map = HashMap::new();
        for pair in raw.split(',') {
            let parts: Vec<&str> = pair.splitn(2, '=').collect();
            if parts.len() == 2 {
                map.insert(
                    parts[0].trim().to_uppercase(),
                    parts[1].trim().parse::<u8>()?,
                );
            }
        }
        Ok(map)
    }
}
