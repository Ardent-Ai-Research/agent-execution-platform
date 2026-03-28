//! Gas-cost → USD pricing — live edition.
//!
//! Fetches real gas prices from the chain RPC and ETH/USD from CoinGecko.
//! Falls back to hardcoded values if live feeds are unavailable.

use anyhow::Result;
use ethers::prelude::*;
use std::sync::Arc;
use tracing::{info, warn};

/// Fallback ETH/USD price if the API is unreachable.
const FALLBACK_ETH_USD: f64 = 2500.0;

/// Flat platform fee in USD.
const PLATFORM_FEE_USD: f64 = 0.01;

/// Markup percentage on gas cost.
const MARKUP_PCT: f64 = 10.0;

/// Fetch the current ETH/USD price from CoinGecko's free API.
/// Returns the fallback price on any error (network, parse, rate-limit).
pub async fn fetch_eth_usd() -> f64 {
    let url = "https://api.coingecko.com/api/v3/simple/price?ids=ethereum&vs_currencies=usd";

    let result: Result<f64, _> = async {
        let resp = reqwest::get(url).await?;
        let body: serde_json::Value = resp.json().await?;
        body["ethereum"]["usd"]
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("missing ethereum.usd in response"))
    }
    .await;

    match result {
        Ok(price) => {
            info!(eth_usd = price, "fetched live ETH/USD price");
            price
        }
        Err(e) => {
            warn!(
                error = %e,
                fallback = FALLBACK_ETH_USD,
                "failed to fetch live ETH/USD price, using fallback"
            );
            FALLBACK_ETH_USD
        }
    }
}

/// Fetch live EIP-1559 gas price from the node (effective gas price = base_fee + priority_fee).
/// Returns the effective gas price in wei.
pub async fn fetch_gas_price_wei(provider: &Arc<Provider<Http>>) -> Result<f64> {
    let latest = provider
        .get_block(BlockNumber::Latest)
        .await?
        .ok_or_else(|| anyhow::anyhow!("could not fetch latest block"))?;

    let base_fee = latest
        .base_fee_per_gas
        .unwrap_or_else(|| U256::from(20_000_000_000u64)); // 20 gwei fallback for non-1559 chains

    let priority_fee = match provider
        .request::<_, U256>("eth_maxPriorityFeePerGas", ())
        .await
    {
        Ok(fee) if fee > U256::zero() => fee,
        _ => U256::from(1_500_000_000u64), // 1.5 gwei fallback
    };

    let effective = base_fee + priority_fee;
    info!(
        base_fee_gwei = base_fee.as_u64() as f64 / 1e9,
        priority_fee_gwei = priority_fee.as_u64() as f64 / 1e9,
        effective_gwei = effective.as_u64() as f64 / 1e9,
        "fetched live gas price from node"
    );
    Ok(effective.as_u64() as f64)
}

/// Calculate execution cost in USD using live data.
///
/// `gas_estimate` — estimated gas units (already includes forwarder overhead).
/// `gas_price_wei` — effective gas price from the node.
/// `eth_usd` — live ETH/USD price.
pub fn calculate_cost(gas_estimate: u64, gas_price_wei: f64, eth_usd: f64) -> f64 {
    let gas_cost_eth = (gas_estimate as f64 * gas_price_wei) / 1e18;
    let base_cost_usd = gas_cost_eth * eth_usd;
    let markup = base_cost_usd * (MARKUP_PCT / 100.0);
    let total = base_cost_usd + markup + PLATFORM_FEE_USD;

    info!(
        gas_estimate,
        gas_price_gwei = gas_price_wei / 1e9,
        eth_usd,
        base_cost_usd,
        total,
        "cost calculation (live)"
    );

    total
}
