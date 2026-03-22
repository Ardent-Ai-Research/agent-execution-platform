//! Gas-cost → USD pricing module.
//!
//! Converts a gas estimate + current EIP-1559 fees into a platform cost
//! denominated in USD.  Uses a live price feed (CoinGecko by default) with
//! a configurable TTL cache.
//!
//! Design principles:
//! * **No silent fallbacks** — if we can't fetch gas fees or the ETH price,
//!   the pricing call fails and the request is rejected rather than
//!   under-quoting and operating at a loss.
//! * **EIP-1559 aligned** — uses the same `2 × base_fee + priority_fee`
//!   heuristic as the relayer, so the quoted price matches what we actually
//!   spend.
//! * **Cached price feed** — avoids hammering the API on every request while
//!   staying reasonably fresh (default TTL: 60 s).

use anyhow::{anyhow, Context, Result};
use ethers::prelude::*;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::info;

// ──────────────────────── Price cache ────────────────────────────────

/// Cached ETH/USD price with expiry.
struct CachedPrice {
    price_usd: f64,
    fetched_at: Instant,
}

/// Thread-safe, TTL-based price cache shared across all pricing calls.
pub struct EthPriceCache {
    inner: RwLock<Option<CachedPrice>>,
    feed_url: String,
    ttl: Duration,
    http: reqwest::Client,
}

impl EthPriceCache {
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

    /// Get the current ETH/USD price, using the cache if fresh.
    pub async fn get_eth_usd(&self) -> Result<f64> {
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

    /// Fetch the ETH/USD price from the configured feed.
    ///
    /// Expected JSON shape (CoinGecko simple/price):
    /// ```json
    /// { "ethereum": { "usd": 3500.42 } }
    /// ```
    /// Also supports a flat `{ "usd": 3500.42 }` shape for custom feeds.
    async fn fetch_price(&self) -> Result<f64> {
        let resp = self
            .http
            .get(&self.feed_url)
            .send()
            .await
            .context("ETH price feed request failed")?;

        if !resp.status().is_success() {
            return Err(anyhow!(
                "ETH price feed returned HTTP {}",
                resp.status()
            ));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse ETH price feed response")?;

        // Try CoinGecko shape first: { "ethereum": { "usd": N } }
        let price = body
            .get("ethereum")
            .and_then(|e| e.get("usd"))
            .and_then(|v| v.as_f64())
            // Fallback: flat { "usd": N }
            .or_else(|| body.get("usd").and_then(|v| v.as_f64()))
            .ok_or_else(|| {
                anyhow!(
                    "could not extract USD price from feed response: {}",
                    body
                )
            })?;

        if price <= 0.0 {
            return Err(anyhow!("ETH price feed returned non-positive price: {price}"));
        }

        info!(eth_usd = price, "ETH/USD price refreshed");
        Ok(price)
    }
}

// ──────────────────────── Fee estimation ─────────────────────────────

/// Estimate the effective EIP-1559 gas price for cost calculation.
///
/// Uses the same `2 × base_fee + priority_fee` heuristic as the relayer so
/// the quoted cost matches what we'll actually spend on-chain.
async fn estimate_effective_gas_price(provider: &Provider<Http>) -> Result<U256> {
    let latest_block = provider
        .get_block(BlockNumber::Latest)
        .await?
        .ok_or_else(|| anyhow!("could not fetch latest block for fee estimation"))?;

    let base_fee = latest_block
        .base_fee_per_gas
        .ok_or_else(|| anyhow!("chain does not support EIP-1559 (no base_fee_per_gas)"))?;

    let priority_fee = match provider
        .request::<_, U256>("eth_maxPriorityFeePerGas", ())
        .await
    {
        Ok(fee) if fee > U256::zero() => fee,
        _ => U256::from(1_500_000_000u64), // 1.5 gwei fallback for priority only
    };

    // Same formula the relayer uses: max_fee = 2 × base_fee + priority_fee
    let effective = base_fee * 2 + priority_fee;
    Ok(effective)
}

// ──────────────────────── Public API ─────────────────────────────────

/// Calculate the execution cost in USD.
///
/// Formula:
///   effective_gas_price = 2 × base_fee + priority_fee  (EIP-1559)
///   gas_cost_eth        = gas_estimate × effective_gas_price
///   cost_usd            = gas_cost_eth × live_ETH_USD + markup% + platform_fee
pub async fn calculate_cost(
    provider: Arc<Provider<Http>>,
    gas_estimate: u64,
    markup_pct: f64,
    platform_fee: f64,
    price_cache: &EthPriceCache,
) -> Result<f64> {
    // ── 1. EIP-1559 effective gas price (no silent fallback) ────────
    let gas_price = estimate_effective_gas_price(&provider)
        .await
        .context("failed to estimate gas price for cost calculation")?;

    // ── 2. Live ETH/USD price (no hardcoded constant) ──────────────
    let eth_usd = price_cache
        .get_eth_usd()
        .await
        .context("failed to fetch ETH/USD price for cost calculation")?;

    // ── 3. Calculate cost ──────────────────────────────────────────
    // Use u128 arithmetic to avoid U256 truncation risk:
    //   gas_cost_wei can be at most ~u64::MAX * ~100 gwei ≈ 1.8e27
    //   which fits comfortably in u128 (max ~3.4e38).
    let gas_cost_wei: u128 = (gas_estimate as u128)
        .checked_mul(gas_price.as_u128())
        .ok_or_else(|| anyhow!("gas cost overflow: estimate={gas_estimate}, price={gas_price}"))?;

    let gas_cost_eth = gas_cost_wei as f64 / 1e18;
    let base_cost_usd = gas_cost_eth * eth_usd;
    let markup = base_cost_usd * (markup_pct / 100.0);
    let total = base_cost_usd + markup + platform_fee;

    info!(
        gas_estimate,
        gas_price_gwei = gas_price.as_u64() as f64 / 1e9,
        eth_usd,
        base_cost_usd,
        markup,
        platform_fee,
        total,
        "cost calculation complete"
    );

    Ok(total)
}
