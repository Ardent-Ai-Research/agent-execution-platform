//! Gas-cost → USD pricing module.
//!
//! Converts a gas estimate + a bundler-provided gas price into a platform cost
//! denominated in USD.  Uses a live price feed (CoinGecko by default) with
//! a configurable TTL cache.
//!
//! Design principles:
//! * **Multi-chain** — each chain has its own `NativeTokenPriceCache` instance
//!   pointing at the correct native-token/USD feed (ETH for Ethereum & Base,
//!   BNB for BSC, etc.).
//! * **Bundler-authoritative fees** — all UserOperations are submitted through
//!   the ERC-4337 bundler, so `rundler_getUserOperationGasPrice` is the single
//!   source of truth for gas pricing.  There is no node-based fallback.
//! * **No silent fallbacks** — if we can't fetch the native token price, the
//!   pricing call fails and the request is rejected rather than under-quoting
//!   and operating at a loss.
//! * **Cached price feed** — avoids hammering the API on every request while
//!   staying reasonably fresh (default TTL: 60 s).

use anyhow::{anyhow, Context, Result};
use ethers::types::U256;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::info;

// ──────────────────────── Price cache ────────────────────────────────

/// Cached native-token/USD price with expiry.
struct CachedPrice {
    price_usd: f64,
    fetched_at: Instant,
}

/// Thread-safe, TTL-based native-token/USD price cache.
///
/// Each supported chain gets its own instance (ETH/USD for Ethereum & Base,
/// BNB/USD for BSC, etc.).
pub struct NativeTokenPriceCache {
    inner: RwLock<Option<CachedPrice>>,
    feed_url: String,
    ttl: Duration,
    http: reqwest::Client,
}

impl NativeTokenPriceCache {
    /// Create a new cache with the given feed URL and TTL.
    pub fn new(feed_url: String, ttl_secs: u64) -> Self {
        Self {
            inner: RwLock::new(None),
            feed_url,
            ttl: Duration::from_secs(ttl_secs),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("failed to build reqwest client"),
        }
    }

    /// Get the cached native-token/USD price, refreshing if stale.
    pub async fn get_native_token_usd(&self) -> Result<f64> {
        // Fast path: read lock, check cache
        {
            let guard = self.inner.read().await;
            if let Some(ref cached) = *guard {
                if cached.fetched_at.elapsed() < self.ttl {
                    return Ok(cached.price_usd);
                }
            }
        }

        // Slow path: write lock, re-check (another task may have refreshed),
        // then fetch.
        let mut guard = self.inner.write().await;
        if let Some(ref cached) = *guard {
            if cached.fetched_at.elapsed() < self.ttl {
                return Ok(cached.price_usd);
            }
        }

        let price = self.fetch_price().await?;
        *guard = Some(CachedPrice {
            price_usd: price,
            fetched_at: Instant::now(),
        });
        Ok(price)
    }

    /// Fetch the native-token/USD price from the configured feed.
    ///
    /// Supports two JSON shapes:
    /// * CoinGecko nested: `{ "<coin_id>": { "usd": 3500.42 } }`
    /// * Flat/custom:      `{ "usd": 3500.42 }`
    async fn fetch_price(&self) -> Result<f64> {
        let resp = self
            .http
            .get(&self.feed_url)
            .send()
            .await
            .context("native token price feed request failed")?;

        if !resp.status().is_success() {
            return Err(anyhow!(
                "native token price feed returned HTTP {}",
                resp.status()
            ));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse native token price feed response")?;

        // Try CoinGecko shape: { "<coin_id>": { "usd": N } }
        // Works for any coin_id (ethereum, binancecoin, etc.) by scanning
        // all top-level keys for a nested "usd" field.
        let price = body
            .as_object()
            .and_then(|obj| {
                obj.values()
                    .filter_map(|v| v.get("usd").and_then(|u| u.as_f64()))
                    .next()
            })
            // Fallback: flat { "usd": N }
            .or_else(|| body.get("usd").and_then(|v| v.as_f64()))
            .ok_or_else(|| {
                anyhow!(
                    "could not extract USD price from feed response: {}",
                    body
                )
            })?;

        if price <= 0.0 {
            return Err(anyhow!("native token price feed returned non-positive price: {price}"));
        }

        info!(native_token_usd = price, feed = %self.feed_url, "native token price refreshed");
        Ok(price)
    }
}

// ──────────────────────── Public API ─────────────────────────────────

/// Calculate the execution cost in USD.
///
/// The caller must supply the `gas_price` (maxFeePerGas) obtained from the
/// bundler via `BundlerClient::get_gas_prices()`.  This module does NOT
/// fetch gas prices itself — the bundler is the single source of truth.
///
/// Formula:
///   gas_cost_native = gas_estimate × gas_price  (in native token, e.g. ETH or BNB)
///   cost_usd        = gas_cost_native × live_native_token/USD + markup% + platform_fee
pub async fn calculate_cost(
    gas_price: U256,
    gas_estimate: u64,
    markup_pct: f64,
    platform_fee: f64,
    price_cache: &NativeTokenPriceCache,
) -> Result<f64> {
    // ── 1. Gas price provided by caller (from bundler) ────────────

    // ── 2. Live native-token/USD price ─────────────────────────────
    let native_usd = price_cache
        .get_native_token_usd()
        .await
        .context("failed to fetch native token/USD price for cost calculation")?;

    // ── 3. Calculate cost ──────────────────────────────────────────
    // Use u128 arithmetic to avoid U256 truncation risk:
    //   gas_cost_wei can be at most ~u64::MAX * ~100 gwei ≈ 1.8e27
    //   which fits comfortably in u128 (max ~3.4e38).
    let gas_cost_wei: u128 = (gas_estimate as u128)
        .checked_mul(gas_price.as_u128())
        .ok_or_else(|| anyhow!("gas cost overflow: estimate={gas_estimate}, price={gas_price}"))?;

    let gas_cost_native = gas_cost_wei as f64 / 1e18;
    let base_cost_usd = gas_cost_native * native_usd;
    let markup = base_cost_usd * (markup_pct / 100.0);
    let total = base_cost_usd + markup + platform_fee;

    info!(
        gas_estimate,
        gas_price_gwei = gas_price.as_u64() as f64 / 1e9,
        native_usd,
        base_cost_usd,
        markup,
        platform_fee,
        total,
        "cost calculation complete"
    );

    Ok(total)
}
