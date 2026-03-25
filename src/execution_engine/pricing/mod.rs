//! Gas-cost → USD pricing — hackathon edition.
//!
//! Uses hardcoded values instead of live price feeds.
//! Good enough for demos on Anvil / testnets.

use tracing::info;

/// Hardcoded ETH/USD price for hackathon demo.
const ETH_USD: f64 = 3500.0;

/// Hardcoded gas price in gwei for hackathon demo.
const GAS_PRICE_GWEI: f64 = 20.0;

/// Flat platform fee.
const PLATFORM_FEE_USD: f64 = 0.01;

/// Markup percentage on gas cost.
const MARKUP_PCT: f64 = 10.0;

/// Calculate execution cost using hardcoded constants.
pub fn calculate_cost_hardcoded(gas_estimate: u64) -> f64 {
    let gas_price_wei = GAS_PRICE_GWEI * 1e9;
    let gas_cost_eth = (gas_estimate as f64 * gas_price_wei) / 1e18;
    let base_cost_usd = gas_cost_eth * ETH_USD;
    let markup = base_cost_usd * (MARKUP_PCT / 100.0);
    let total = base_cost_usd + markup + PLATFORM_FEE_USD;

    info!(
        gas_estimate,
        gas_price_gwei = GAS_PRICE_GWEI,
        eth_usd = ETH_USD,
        total,
        "cost calculation (hardcoded)"
    );

    total
}
