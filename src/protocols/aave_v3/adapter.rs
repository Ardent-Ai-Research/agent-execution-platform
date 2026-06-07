//! Aave V3 typed action adapter.
//!
//! First supported market: Ethereum Sepolia.  The public API still uses
//! `chain: "ethereum"` because the hosted testnet environment maps that chain
//! label to Sepolia RPC/bundler infrastructure.

use anyhow::{anyhow, Result};
use ethers::abi::{self, Token};
use ethers::types::{Address, U256};
use ethers::utils::id;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::types::{BatchCall, ExecutionRequest};

const AAVE_V3_SEPOLIA_POOL: &str = "0x6Ae43d3271ff6888e7Fc43Fd7321a503ff738951";
const VARIABLE_RATE_MODE: u8 = 2;

fn erc20_approve_selector() -> [u8; 4] {
    selector("approve(address,uint256)")
}

fn erc20_balance_of_selector() -> [u8; 4] {
    selector("balanceOf(address)")
}

fn aave_pool_supply_selector() -> [u8; 4] {
    selector("supply(address,uint256,address,uint16)")
}

fn aave_pool_withdraw_selector() -> [u8; 4] {
    selector("withdraw(address,uint256,address)")
}

fn aave_pool_repay_selector() -> [u8; 4] {
    selector("repay(address,uint256,uint256,address)")
}

fn aave_pool_borrow_selector() -> [u8; 4] {
    selector("borrow(address,uint256,uint256,uint16,address)")
}

pub fn aave_pool_addresses_provider_selector() -> [u8; 4] {
    selector("ADDRESSES_PROVIDER()")
}

pub fn aave_addresses_provider_get_price_oracle_selector() -> [u8; 4] {
    selector("getPriceOracle()")
}

pub fn aave_oracle_get_asset_price_selector() -> [u8; 4] {
    selector("getAssetPrice(address)")
}

pub fn aave_pool_get_user_account_data_selector() -> [u8; 4] {
    selector("getUserAccountData(address)")
}

pub fn aave_pool_get_reserve_data_selector() -> [u8; 4] {
    selector("getReserveData(address)")
}

/// Typed request for supplying ERC-20 collateral/liquidity into Aave V3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AaveSupplyRequest {
    pub agent_id: String,
    /// For the hosted testnet environment, use `"ethereum"` for Sepolia.
    #[serde(default = "default_chain")]
    pub chain: String,
    /// Asset symbol on Aave V3 Sepolia, e.g. `USDC`, `WETH`, `DAI`.
    pub asset: String,
    /// Human-readable amount, e.g. `"100.5"` for USDC.
    #[serde(default)]
    pub amount: Option<String>,
    /// Raw token amount in smallest units. Use this for exact machine input.
    #[serde(default)]
    pub amount_raw: Option<String>,
    /// Optional Aave referral code. Defaults to 0.
    #[serde(default)]
    pub referral_code: Option<u16>,
    #[serde(default)]
    pub strategy_id: Option<String>,
    #[serde(default)]
    pub callback_url: Option<String>,
}

/// Typed request for withdrawing supplied liquidity from Aave V3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AaveWithdrawRequest {
    pub agent_id: String,
    #[serde(default = "default_chain")]
    pub chain: String,
    pub asset: String,
    /// Human-readable token amount, or `"max"` to withdraw all available aToken balance.
    #[serde(default)]
    pub amount: Option<String>,
    #[serde(default)]
    pub amount_raw: Option<String>,
    /// Optional recipient. Defaults to the agent smart wallet.
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub strategy_id: Option<String>,
    #[serde(default)]
    pub callback_url: Option<String>,
}

/// Typed request for repaying Aave V3 debt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AaveRepayRequest {
    pub agent_id: String,
    #[serde(default = "default_chain")]
    pub chain: String,
    pub asset: String,
    /// Human-readable token amount, or `"max"` to repay up to debt and wallet balance.
    #[serde(default)]
    pub amount: Option<String>,
    #[serde(default)]
    pub amount_raw: Option<String>,
    /// Aave interest rate mode. Defaults to 2 (variable).
    #[serde(default)]
    pub interest_rate_mode: Option<u8>,
    /// Optional debt owner. Defaults to the agent smart wallet.
    #[serde(default)]
    pub on_behalf_of: Option<String>,
    #[serde(default)]
    pub strategy_id: Option<String>,
    #[serde(default)]
    pub callback_url: Option<String>,
}

/// Typed request for borrowing from Aave V3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AaveBorrowRequest {
    pub agent_id: String,
    #[serde(default = "default_chain")]
    pub chain: String,
    pub asset: String,
    /// Human-readable token amount, or `"max"` to borrow the maximum amount allowed by the health-factor guard.
    #[serde(default)]
    pub amount: Option<String>,
    #[serde(default)]
    pub amount_raw: Option<String>,
    /// Aave interest rate mode. Defaults to 2 (variable).
    #[serde(default)]
    pub interest_rate_mode: Option<u8>,
    /// Optional Aave referral code. Defaults to 0.
    #[serde(default)]
    pub referral_code: Option<u16>,
    /// Optional debt owner. Defaults to the agent smart wallet.
    #[serde(default)]
    pub on_behalf_of: Option<String>,
    /// Minimum projected health factor after this borrow. Defaults to 1.05.
    #[serde(default)]
    pub min_health_factor: Option<String>,
    #[serde(default)]
    pub strategy_id: Option<String>,
    #[serde(default)]
    pub callback_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AavePositionQuery {
    pub agent_id: String,
    #[serde(default = "default_chain")]
    pub chain: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AavePositionResponse {
    pub agent_id: String,
    pub chain: String,
    pub smart_wallet_address: String,
    pub pool_address: String,
    pub total_collateral_base: String,
    pub total_debt_base: String,
    pub available_borrows_base: String,
    pub current_liquidation_threshold_bps: String,
    pub ltv_bps: String,
    pub health_factor: String,
}

#[derive(Debug, Clone)]
pub struct AaveAccountData {
    pub total_collateral_base: U256,
    pub total_debt_base: U256,
    pub available_borrows_base: U256,
    pub current_liquidation_threshold_bps: U256,
    pub ltv_bps: U256,
    pub health_factor: U256,
}

#[derive(Debug, Clone)]
pub struct AaveReserveDebtTokens {
    pub stable_debt_token: Address,
    pub variable_debt_token: Address,
}

fn default_chain() -> String {
    "ethereum".to_string()
}

#[derive(Debug, Clone)]
struct AaveAsset {
    underlying: &'static str,
    decimals: u8,
}

fn sepolia_assets() -> HashMap<&'static str, AaveAsset> {
    HashMap::from([
        (
            "USDC",
            AaveAsset {
                underlying: "0x94a9D9AC8a22534E3FaCa9F4e7F2E2cf85d5E4C8",
                decimals: 6,
            },
        ),
        (
            "DAI",
            AaveAsset {
                underlying: "0xFF34B3d4Aee8ddCd6F9AFFFB6Fe49bD371b8a357",
                decimals: 18,
            },
        ),
        (
            "LINK",
            AaveAsset {
                underlying: "0xf8fF3713d459D7C1018BD0A49D19b4C44290ebe5",
                decimals: 18,
            },
        ),
        (
            "WBTC",
            AaveAsset {
                underlying: "0x29f2D40B0605204364af54EC677bD022dA425d03",
                decimals: 8,
            },
        ),
        (
            "WETH",
            AaveAsset {
                underlying: "0xC558DBdd856501FCd9aaF1E62eae57A9F0629a3c",
                decimals: 18,
            },
        ),
        (
            "USDT",
            AaveAsset {
                underlying: "0xaA8E23Fb1079EA71e0a56F48a2aA51851D8433D0",
                decimals: 6,
            },
        ),
        (
            "AAVE",
            AaveAsset {
                underlying: "0x88541670E55cC00bEEFD87eB59EDd1b7C511AC9a",
                decimals: 18,
            },
        ),
        (
            "EURS",
            AaveAsset {
                underlying: "0x6d906e526a4e2Ca02097BA9d0caA3c382F52278E",
                decimals: 2,
            },
        ),
        (
            "GHO",
            AaveAsset {
                underlying: "0xc4bF5CbDaBE595361438F8c6a187bDc330539c60",
                decimals: 18,
            },
        ),
    ])
}

pub fn compile_supply(
    req: &AaveSupplyRequest,
    smart_wallet_address: Address,
) -> Result<ExecutionRequest> {
    validate_supply_request(req)?;

    let symbol = req.asset.trim().to_uppercase();
    let assets = sepolia_assets();
    let asset = assets
        .get(symbol.as_str())
        .expect("validate_supply_request checked asset support");

    let amount = resolve_amount(
        req.amount.as_deref(),
        req.amount_raw.as_deref(),
        asset.decimals,
        false,
    )?;
    if amount.is_zero() {
        return Err(anyhow!("amount must be greater than zero"));
    }

    let token: Address = asset.underlying.parse()?;
    let pool: Address = AAVE_V3_SEPOLIA_POOL.parse()?;
    let referral_code = req.referral_code.unwrap_or(0);

    Ok(ExecutionRequest {
        agent_id: req.agent_id.clone(),
        chain: req.chain.clone(),
        target_contract: String::new(),
        calldata: String::new(),
        value: "0".to_string(),
        strategy_id: req
            .strategy_id
            .clone()
            .or_else(|| Some(format!("aave-v3-sepolia-supply-{symbol}"))),
        batch_calls: Some(vec![
            BatchCall {
                target_contract: format!("{token:?}"),
                calldata: encode_erc20_approve(pool, amount),
                value: "0".to_string(),
            },
            BatchCall {
                target_contract: format!("{pool:?}"),
                calldata: encode_aave_supply(token, amount, smart_wallet_address, referral_code),
                value: "0".to_string(),
            },
        ]),
        callback_url: req.callback_url.clone(),
    })
}

pub fn compile_withdraw(
    req: &AaveWithdrawRequest,
    smart_wallet_address: Address,
) -> Result<ExecutionRequest> {
    validate_withdraw_request(req)?;

    let symbol = req.asset.trim().to_uppercase();
    let assets = sepolia_assets();
    let asset = assets
        .get(symbol.as_str())
        .expect("validate_withdraw_request checked asset support");

    let amount = resolve_amount(
        req.amount.as_deref(),
        req.amount_raw.as_deref(),
        asset.decimals,
        true,
    )?;
    if amount.is_zero() {
        return Err(anyhow!("amount must be greater than zero"));
    }

    let token: Address = asset.underlying.parse()?;
    let pool: Address = AAVE_V3_SEPOLIA_POOL.parse()?;
    let recipient = parse_optional_address(req.to.as_deref(), smart_wallet_address, "to")?;

    Ok(ExecutionRequest {
        agent_id: req.agent_id.clone(),
        chain: req.chain.clone(),
        target_contract: format!("{pool:?}"),
        calldata: encode_aave_withdraw(token, amount, recipient),
        value: "0".to_string(),
        strategy_id: req
            .strategy_id
            .clone()
            .or_else(|| Some(format!("aave-v3-sepolia-withdraw-{symbol}"))),
        batch_calls: None,
        callback_url: req.callback_url.clone(),
    })
}

pub fn compile_repay(
    req: &AaveRepayRequest,
    smart_wallet_address: Address,
) -> Result<ExecutionRequest> {
    validate_repay_request(req)?;

    let symbol = req.asset.trim().to_uppercase();
    let assets = sepolia_assets();
    let asset = assets
        .get(symbol.as_str())
        .expect("validate_repay_request checked asset support");

    let amount = resolve_amount(
        req.amount.as_deref(),
        req.amount_raw.as_deref(),
        asset.decimals,
        false,
    )?;
    if amount.is_zero() {
        return Err(anyhow!("amount must be greater than zero"));
    }

    let token: Address = asset.underlying.parse()?;
    let pool: Address = AAVE_V3_SEPOLIA_POOL.parse()?;
    let rate_mode = U256::from(req.interest_rate_mode.unwrap_or(VARIABLE_RATE_MODE));
    let on_behalf_of = parse_optional_address(
        req.on_behalf_of.as_deref(),
        smart_wallet_address,
        "on_behalf_of",
    )?;

    Ok(ExecutionRequest {
        agent_id: req.agent_id.clone(),
        chain: req.chain.clone(),
        target_contract: String::new(),
        calldata: String::new(),
        value: "0".to_string(),
        strategy_id: req
            .strategy_id
            .clone()
            .or_else(|| Some(format!("aave-v3-sepolia-repay-{symbol}"))),
        batch_calls: Some(vec![
            BatchCall {
                target_contract: format!("{token:?}"),
                calldata: encode_erc20_approve(pool, amount),
                value: "0".to_string(),
            },
            BatchCall {
                target_contract: format!("{pool:?}"),
                calldata: encode_aave_repay(token, amount, rate_mode, on_behalf_of),
                value: "0".to_string(),
            },
        ]),
        callback_url: req.callback_url.clone(),
    })
}

pub fn compile_borrow(
    req: &AaveBorrowRequest,
    smart_wallet_address: Address,
) -> Result<ExecutionRequest> {
    validate_borrow_request(req)?;

    let symbol = req.asset.trim().to_uppercase();
    let assets = sepolia_assets();
    let asset = assets
        .get(symbol.as_str())
        .expect("validate_borrow_request checked asset support");

    let amount = resolve_amount(
        req.amount.as_deref(),
        req.amount_raw.as_deref(),
        asset.decimals,
        false,
    )?;
    if amount.is_zero() {
        return Err(anyhow!("amount must be greater than zero"));
    }

    let token: Address = asset.underlying.parse()?;
    let pool: Address = AAVE_V3_SEPOLIA_POOL.parse()?;
    let rate_mode = U256::from(req.interest_rate_mode.unwrap_or(VARIABLE_RATE_MODE));
    let referral_code = req.referral_code.unwrap_or(0);
    let on_behalf_of = parse_optional_address(
        req.on_behalf_of.as_deref(),
        smart_wallet_address,
        "on_behalf_of",
    )?;

    Ok(ExecutionRequest {
        agent_id: req.agent_id.clone(),
        chain: req.chain.clone(),
        target_contract: format!("{pool:?}"),
        calldata: encode_aave_borrow(token, amount, rate_mode, referral_code, on_behalf_of),
        value: "0".to_string(),
        strategy_id: req
            .strategy_id
            .clone()
            .or_else(|| Some(format!("aave-v3-sepolia-borrow-{symbol}"))),
        batch_calls: None,
        callback_url: req.callback_url.clone(),
    })
}

pub fn validate_supply_request(req: &AaveSupplyRequest) -> Result<()> {
    let asset = validate_common_action(&req.agent_id, &req.chain, &req.asset)?;

    let amount = resolve_amount(
        req.amount.as_deref(),
        req.amount_raw.as_deref(),
        asset.decimals,
        false,
    )?;
    if amount.is_zero() {
        return Err(anyhow!("amount must be greater than zero"));
    }

    Ok(())
}

pub fn validate_withdraw_request(req: &AaveWithdrawRequest) -> Result<()> {
    let asset = validate_common_action(&req.agent_id, &req.chain, &req.asset)?;
    let amount = resolve_amount(
        req.amount.as_deref(),
        req.amount_raw.as_deref(),
        asset.decimals,
        true,
    )?;
    if amount.is_zero() {
        return Err(anyhow!("amount must be greater than zero"));
    }
    if let Some(to) = req.to.as_deref() {
        parse_address(to, "to")?;
    }
    Ok(())
}

pub fn validate_repay_request(req: &AaveRepayRequest) -> Result<()> {
    let asset = validate_common_action(&req.agent_id, &req.chain, &req.asset)?;
    let amount = resolve_amount(
        req.amount.as_deref(),
        req.amount_raw.as_deref(),
        asset.decimals,
        true,
    )?;
    if amount.is_zero() {
        return Err(anyhow!("amount must be greater than zero"));
    }
    let mode = req.interest_rate_mode.unwrap_or(VARIABLE_RATE_MODE);
    if mode != 1 && mode != 2 {
        return Err(anyhow!(
            "interest_rate_mode must be 1 (stable) or 2 (variable)"
        ));
    }
    if let Some(on_behalf_of) = req.on_behalf_of.as_deref() {
        parse_address(on_behalf_of, "on_behalf_of")?;
    }
    Ok(())
}

pub fn validate_borrow_request(req: &AaveBorrowRequest) -> Result<()> {
    let asset = validate_common_action(&req.agent_id, &req.chain, &req.asset)?;
    let amount = resolve_amount(
        req.amount.as_deref(),
        req.amount_raw.as_deref(),
        asset.decimals,
        true,
    )?;
    if amount.is_zero() {
        return Err(anyhow!("amount must be greater than zero"));
    }
    let mode = req.interest_rate_mode.unwrap_or(VARIABLE_RATE_MODE);
    if mode != 1 && mode != 2 {
        return Err(anyhow!(
            "interest_rate_mode must be 1 (stable) or 2 (variable)"
        ));
    }
    if let Some(on_behalf_of) = req.on_behalf_of.as_deref() {
        parse_address(on_behalf_of, "on_behalf_of")?;
    }
    if let Some(min_health_factor) = req.min_health_factor.as_deref() {
        let min = parse_decimal_amount(min_health_factor, 18)?;
        if min < U256::exp10(18) {
            return Err(anyhow!("min_health_factor must be at least 1.0"));
        }
    }
    Ok(())
}

pub fn validate_position_query(query: &AavePositionQuery) -> Result<()> {
    validate_chain_and_agent(&query.agent_id, &query.chain)?;
    Ok(())
}

pub fn pool_address() -> &'static str {
    AAVE_V3_SEPOLIA_POOL
}

pub fn encode_get_user_account_data(user: Address) -> String {
    encode_call(
        aave_pool_get_user_account_data_selector(),
        &[Token::Address(user)],
    )
}

pub fn encode_balance_of(user: Address) -> String {
    encode_call(erc20_balance_of_selector(), &[Token::Address(user)])
}

pub fn encode_get_reserve_data(asset: Address) -> String {
    encode_call(
        aave_pool_get_reserve_data_selector(),
        &[Token::Address(asset)],
    )
}

pub fn encode_addresses_provider() -> String {
    encode_call(aave_pool_addresses_provider_selector(), &[])
}

pub fn encode_get_price_oracle() -> String {
    encode_call(aave_addresses_provider_get_price_oracle_selector(), &[])
}

pub fn encode_get_asset_price(asset: Address) -> String {
    encode_call(
        aave_oracle_get_asset_price_selector(),
        &[Token::Address(asset)],
    )
}

pub fn decode_address(raw: &[u8]) -> Result<Address> {
    let decoded = abi::decode(&[abi::ParamType::Address], raw)?;
    match decoded.first() {
        Some(Token::Address(value)) => Ok(*value),
        _ => Err(anyhow!("unexpected address return token")),
    }
}

pub fn decode_u256(raw: &[u8]) -> Result<U256> {
    let decoded = abi::decode(&[abi::ParamType::Uint(256)], raw)?;
    match decoded.first() {
        Some(Token::Uint(value)) => Ok(*value),
        _ => Err(anyhow!("unexpected uint256 return token")),
    }
}

pub fn decode_reserve_debt_tokens(raw: &[u8]) -> Result<AaveReserveDebtTokens> {
    let decoded = abi::decode(
        &[abi::ParamType::Tuple(vec![
            abi::ParamType::Tuple(vec![abi::ParamType::Uint(256)]),
            abi::ParamType::Uint(128),
            abi::ParamType::Uint(128),
            abi::ParamType::Uint(128),
            abi::ParamType::Uint(128),
            abi::ParamType::Uint(128),
            abi::ParamType::Uint(40),
            abi::ParamType::Uint(16),
            abi::ParamType::Address,
            abi::ParamType::Address,
            abi::ParamType::Address,
            abi::ParamType::Address,
            abi::ParamType::Uint(128),
            abi::ParamType::Uint(128),
            abi::ParamType::Uint(128),
        ])],
        raw,
    )?;

    let tokens = match decoded.first() {
        Some(Token::Tuple(tokens)) => tokens,
        _ => return Err(anyhow!("unexpected getReserveData return token")),
    };

    match (tokens.get(9), tokens.get(10)) {
        (Some(Token::Address(stable)), Some(Token::Address(variable))) => {
            Ok(AaveReserveDebtTokens {
                stable_debt_token: *stable,
                variable_debt_token: *variable,
            })
        }
        _ => Err(anyhow!("unexpected getReserveData debt token fields")),
    }
}

pub fn asset_address_and_decimals(symbol: &str) -> Result<(Address, u8)> {
    let assets = sepolia_assets();
    let asset = assets
        .get(symbol.trim().to_uppercase().as_str())
        .ok_or_else(|| anyhow!("unsupported Aave V3 Sepolia asset '{}'", symbol))?;
    Ok((asset.underlying.parse()?, asset.decimals))
}

pub fn borrow_amount(req: &AaveBorrowRequest) -> Result<U256> {
    let assets = sepolia_assets();
    let asset = assets
        .get(req.asset.trim().to_uppercase().as_str())
        .ok_or_else(|| anyhow!("unsupported Aave V3 Sepolia asset '{}'", req.asset))?;
    resolve_amount(
        req.amount.as_deref(),
        req.amount_raw.as_deref(),
        asset.decimals,
        true,
    )
}

pub fn is_amount_max(amount: Option<&str>, amount_raw: Option<&str>) -> bool {
    amount
        .map(|v| v.trim().eq_ignore_ascii_case("max"))
        .unwrap_or(false)
        || amount_raw
            .map(|v| v.trim().eq_ignore_ascii_case("max"))
            .unwrap_or(false)
}

pub fn repay_with_amount_raw(req: &AaveRepayRequest, amount: U256) -> AaveRepayRequest {
    let mut resolved = req.clone();
    resolved.amount = None;
    resolved.amount_raw = Some(amount.to_string());
    resolved
}

pub fn borrow_with_amount_raw(req: &AaveBorrowRequest, amount: U256) -> AaveBorrowRequest {
    let mut resolved = req.clone();
    resolved.amount = None;
    resolved.amount_raw = Some(amount.to_string());
    resolved
}

pub fn min_health_factor_ray(req: &AaveBorrowRequest) -> Result<U256> {
    parse_decimal_amount(req.min_health_factor.as_deref().unwrap_or("1.05"), 18)
}

pub fn decode_user_account_data_values(raw: &[u8]) -> Result<AaveAccountData> {
    let decoded = abi::decode(
        &[
            abi::ParamType::Uint(256),
            abi::ParamType::Uint(256),
            abi::ParamType::Uint(256),
            abi::ParamType::Uint(256),
            abi::ParamType::Uint(256),
            abi::ParamType::Uint(256),
        ],
        raw,
    )?;

    let mut values = Vec::with_capacity(decoded.len());
    for token in decoded {
        match token {
            Token::Uint(value) => values.push(value),
            _ => return Err(anyhow!("unexpected getUserAccountData return token")),
        }
    }

    Ok(AaveAccountData {
        total_collateral_base: values[0],
        total_debt_base: values[1],
        available_borrows_base: values[2],
        current_liquidation_threshold_bps: values[3],
        ltv_bps: values[4],
        health_factor: values[5],
    })
}

pub fn decode_user_account_data(
    raw: &[u8],
    agent_id: String,
    chain: String,
    smart_wallet_address: Address,
) -> Result<AavePositionResponse> {
    let values = decode_user_account_data_values(raw)?;

    Ok(AavePositionResponse {
        agent_id,
        chain,
        smart_wallet_address: format!("{smart_wallet_address:?}"),
        pool_address: AAVE_V3_SEPOLIA_POOL.to_string(),
        total_collateral_base: values.total_collateral_base.to_string(),
        total_debt_base: values.total_debt_base.to_string(),
        available_borrows_base: values.available_borrows_base.to_string(),
        current_liquidation_threshold_bps: values.current_liquidation_threshold_bps.to_string(),
        ltv_bps: values.ltv_bps.to_string(),
        health_factor: values.health_factor.to_string(),
    })
}

fn validate_common_action(agent_id: &str, chain: &str, asset: &str) -> Result<AaveAsset> {
    validate_chain_and_agent(agent_id, chain)?;
    let symbol = asset.trim().to_uppercase();
    let assets = sepolia_assets();
    assets.get(symbol.as_str()).cloned().ok_or_else(|| {
        anyhow!(
            "unsupported Aave V3 Sepolia asset '{}'; supported: {}",
            asset,
            supported_asset_list(&assets)
        )
    })
}

fn validate_chain_and_agent(agent_id: &str, chain: &str) -> Result<()> {
    let chain = crate::types::Chain::from_str_loose(chain)
        .ok_or_else(|| anyhow!("unsupported chain for Aave V3: {}", chain))?;

    if chain != crate::types::Chain::Ethereum {
        return Err(anyhow!(
            "Aave V3 typed adapter currently supports Ethereum Sepolia only; use chain \"ethereum\""
        ));
    }

    if agent_id.trim().is_empty() {
        return Err(anyhow!("agent_id is required"));
    }

    Ok(())
}

fn supported_asset_list(assets: &HashMap<&'static str, AaveAsset>) -> String {
    let mut symbols = assets.keys().copied().collect::<Vec<_>>();
    symbols.sort_unstable();
    symbols.join(", ")
}

fn resolve_amount(
    amount: Option<&str>,
    amount_raw: Option<&str>,
    decimals: u8,
    allow_max: bool,
) -> Result<U256> {
    match (amount, amount_raw) {
        (Some(_), Some(_)) => Err(anyhow!("provide either amount or amount_raw, not both")),
        (Some(raw), None) if allow_max && raw.trim().eq_ignore_ascii_case("max") => Ok(U256::MAX),
        (Some(amount), None) => parse_decimal_amount(amount, decimals),
        (None, Some(raw)) if allow_max && raw.trim().eq_ignore_ascii_case("max") => Ok(U256::MAX),
        (None, Some(raw)) => U256::from_dec_str(raw.trim())
            .map_err(|e| anyhow!("amount_raw must be a base-10 integer: {e}")),
        (None, None) => Err(anyhow!("amount or amount_raw is required")),
    }
}

fn parse_optional_address(raw: Option<&str>, fallback: Address, field: &str) -> Result<Address> {
    match raw {
        Some(value) => parse_address(value, field),
        None => Ok(fallback),
    }
}

fn parse_address(raw: &str, field: &str) -> Result<Address> {
    raw.parse()
        .map_err(|e| anyhow!("{field} must be a valid EVM address: {e}"))
}

fn parse_decimal_amount(raw: &str, decimals: u8) -> Result<U256> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("amount is required"));
    }
    if trimmed.starts_with('-') {
        return Err(anyhow!("amount cannot be negative"));
    }

    let parts = trimmed.split('.').collect::<Vec<_>>();
    if parts.len() > 2 {
        return Err(anyhow!("amount has too many decimal points"));
    }

    let whole = parts[0];
    let fractional = parts.get(1).copied().unwrap_or("");

    if whole.is_empty() && fractional.is_empty() {
        return Err(anyhow!("amount is required"));
    }
    if !whole.chars().all(|c| c.is_ascii_digit()) || !fractional.chars().all(|c| c.is_ascii_digit())
    {
        return Err(anyhow!("amount must be a positive decimal number"));
    }
    if fractional.len() > decimals as usize {
        return Err(anyhow!(
            "amount has too many decimal places for asset decimals {}",
            decimals
        ));
    }

    let mut normalized = String::new();
    normalized.push_str(if whole.is_empty() { "0" } else { whole });
    normalized.push_str(fractional);
    normalized.push_str(&"0".repeat(decimals as usize - fractional.len()));

    let trimmed_zeroes = normalized.trim_start_matches('0');
    if trimmed_zeroes.is_empty() {
        Ok(U256::zero())
    } else {
        U256::from_dec_str(trimmed_zeroes).map_err(|e| anyhow!("invalid amount: {e}"))
    }
}

fn encode_erc20_approve(spender: Address, amount: U256) -> String {
    let tokens = vec![Token::Address(spender), Token::Uint(amount)];
    encode_call(erc20_approve_selector(), &tokens)
}

fn encode_aave_supply(
    asset: Address,
    amount: U256,
    on_behalf_of: Address,
    referral_code: u16,
) -> String {
    let tokens = vec![
        Token::Address(asset),
        Token::Uint(amount),
        Token::Address(on_behalf_of),
        Token::Uint(U256::from(referral_code)),
    ];
    encode_call(aave_pool_supply_selector(), &tokens)
}

fn encode_aave_withdraw(asset: Address, amount: U256, to: Address) -> String {
    let tokens = vec![
        Token::Address(asset),
        Token::Uint(amount),
        Token::Address(to),
    ];
    encode_call(aave_pool_withdraw_selector(), &tokens)
}

fn encode_aave_repay(
    asset: Address,
    amount: U256,
    interest_rate_mode: U256,
    on_behalf_of: Address,
) -> String {
    let tokens = vec![
        Token::Address(asset),
        Token::Uint(amount),
        Token::Uint(interest_rate_mode),
        Token::Address(on_behalf_of),
    ];
    encode_call(aave_pool_repay_selector(), &tokens)
}

fn encode_aave_borrow(
    asset: Address,
    amount: U256,
    interest_rate_mode: U256,
    referral_code: u16,
    on_behalf_of: Address,
) -> String {
    let tokens = vec![
        Token::Address(asset),
        Token::Uint(amount),
        Token::Uint(interest_rate_mode),
        Token::Uint(U256::from(referral_code)),
        Token::Address(on_behalf_of),
    ];
    encode_call(aave_pool_borrow_selector(), &tokens)
}

fn encode_call(selector: [u8; 4], tokens: &[Token]) -> String {
    let encoded_params = abi::encode(tokens);
    let mut data = Vec::with_capacity(4 + encoded_params.len());
    data.extend_from_slice(&selector);
    data.extend_from_slice(&encoded_params);
    format!("0x{}", hex::encode(data))
}

fn selector(signature: &str) -> [u8; 4] {
    let hash = id(signature);
    [hash[0], hash[1], hash[2], hash[3]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_supply_builds_approve_then_supply_batch() {
        let req = AaveSupplyRequest {
            agent_id: "agent-aave".into(),
            chain: "ethereum".into(),
            asset: "USDC".into(),
            amount: Some("1.25".into()),
            amount_raw: None,
            referral_code: None,
            strategy_id: None,
            callback_url: None,
        };

        let wallet: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .expect("wallet");
        let execution = compile_supply(&req, wallet).expect("compile");
        let batch = execution.batch_calls.expect("batch");

        assert_eq!(batch.len(), 2);
        assert_eq!(
            &batch[0].calldata[..10],
            format!("0x{}", hex::encode(erc20_approve_selector()))
        );
        assert_eq!(
            &batch[1].calldata[..10],
            format!("0x{}", hex::encode(aave_pool_supply_selector()))
        );
        assert_eq!(
            batch[1].target_contract.to_lowercase(),
            AAVE_V3_SEPOLIA_POOL.to_lowercase()
        );
    }

    #[test]
    fn parse_decimal_amount_respects_decimals() {
        assert_eq!(
            parse_decimal_amount("1.25", 6).expect("amount"),
            U256::from(1_250_000u64)
        );
        assert!(parse_decimal_amount("1.0000001", 6).is_err());
    }

    #[test]
    fn compile_withdraw_uses_pool_withdraw_single_call() {
        let req = AaveWithdrawRequest {
            agent_id: "agent-aave".into(),
            chain: "ethereum".into(),
            asset: "USDC".into(),
            amount: Some("max".into()),
            amount_raw: None,
            to: None,
            strategy_id: None,
            callback_url: None,
        };
        let wallet: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .expect("wallet");
        let execution = compile_withdraw(&req, wallet).expect("compile");
        assert!(execution.batch_calls.is_none());
        assert_eq!(
            &execution.calldata[..10],
            format!("0x{}", hex::encode(aave_pool_withdraw_selector()))
        );
    }

    #[test]
    fn compile_repay_builds_approve_then_repay_batch() {
        let req = AaveRepayRequest {
            agent_id: "agent-aave".into(),
            chain: "ethereum".into(),
            asset: "USDC".into(),
            amount: Some("1".into()),
            amount_raw: None,
            interest_rate_mode: None,
            on_behalf_of: None,
            strategy_id: None,
            callback_url: None,
        };
        let wallet: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .expect("wallet");
        let execution = compile_repay(&req, wallet).expect("compile");
        let batch = execution.batch_calls.expect("batch");
        assert_eq!(batch.len(), 2);
        assert_eq!(
            &batch[1].calldata[..10],
            format!("0x{}", hex::encode(aave_pool_repay_selector()))
        );
    }

    #[test]
    fn compile_borrow_builds_pool_borrow_single_call() {
        let req = AaveBorrowRequest {
            agent_id: "agent-aave".into(),
            chain: "ethereum".into(),
            asset: "USDC".into(),
            amount: Some("1".into()),
            amount_raw: None,
            interest_rate_mode: None,
            referral_code: None,
            on_behalf_of: None,
            min_health_factor: None,
            strategy_id: None,
            callback_url: None,
        };
        let wallet: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .expect("wallet");
        let execution = compile_borrow(&req, wallet).expect("compile");
        assert!(execution.batch_calls.is_none());
        assert_eq!(
            &execution.calldata[..10],
            format!("0x{}", hex::encode(aave_pool_borrow_selector()))
        );
        assert_eq!(
            execution.target_contract.to_lowercase(),
            AAVE_V3_SEPOLIA_POOL.to_lowercase()
        );
    }

    #[test]
    fn borrow_min_health_factor_defaults_to_one_point_zero_five() {
        let req = AaveBorrowRequest {
            agent_id: "agent-aave".into(),
            chain: "ethereum".into(),
            asset: "USDC".into(),
            amount: Some("1".into()),
            amount_raw: None,
            interest_rate_mode: None,
            referral_code: None,
            on_behalf_of: None,
            min_health_factor: None,
            strategy_id: None,
            callback_url: None,
        };

        assert_eq!(
            min_health_factor_ray(&req).expect("min health factor"),
            U256::from_dec_str("1050000000000000000").expect("ray")
        );
    }

    #[test]
    fn borrow_rejects_min_health_factor_below_one() {
        let req = AaveBorrowRequest {
            agent_id: "agent-aave".into(),
            chain: "ethereum".into(),
            asset: "USDC".into(),
            amount: Some("1".into()),
            amount_raw: None,
            interest_rate_mode: None,
            referral_code: None,
            on_behalf_of: None,
            min_health_factor: Some("0.99".into()),
            strategy_id: None,
            callback_url: None,
        };

        assert!(validate_borrow_request(&req).is_err());
    }

    #[test]
    fn repay_accepts_max_for_service_layer_resolution() {
        let req = AaveRepayRequest {
            agent_id: "agent-aave".into(),
            chain: "ethereum".into(),
            asset: "USDC".into(),
            amount: Some("max".into()),
            amount_raw: None,
            interest_rate_mode: None,
            on_behalf_of: None,
            strategy_id: None,
            callback_url: None,
        };

        assert!(validate_repay_request(&req).is_ok());
        assert!(is_amount_max(
            req.amount.as_deref(),
            req.amount_raw.as_deref()
        ));
    }

    #[test]
    fn repay_with_amount_raw_replaces_max_marker() {
        let req = AaveRepayRequest {
            agent_id: "agent-aave".into(),
            chain: "ethereum".into(),
            asset: "USDC".into(),
            amount: Some("max".into()),
            amount_raw: None,
            interest_rate_mode: None,
            on_behalf_of: None,
            strategy_id: None,
            callback_url: None,
        };
        let resolved = repay_with_amount_raw(&req, U256::from(123u64));

        assert_eq!(resolved.amount, None);
        assert_eq!(resolved.amount_raw.as_deref(), Some("123"));
        assert!(!is_amount_max(
            resolved.amount.as_deref(),
            resolved.amount_raw.as_deref()
        ));
    }

    #[test]
    fn borrow_accepts_max_for_service_layer_resolution() {
        let req = AaveBorrowRequest {
            agent_id: "agent-aave".into(),
            chain: "ethereum".into(),
            asset: "USDC".into(),
            amount: Some("max".into()),
            amount_raw: None,
            interest_rate_mode: None,
            referral_code: None,
            on_behalf_of: None,
            min_health_factor: None,
            strategy_id: None,
            callback_url: None,
        };

        assert!(validate_borrow_request(&req).is_ok());
        assert!(is_amount_max(
            req.amount.as_deref(),
            req.amount_raw.as_deref()
        ));
    }

    #[test]
    fn decode_reserve_debt_tokens_reads_stable_and_variable_addresses() {
        let stable: Address = "0x2222222222222222222222222222222222222222"
            .parse()
            .expect("stable");
        let variable: Address = "0x3333333333333333333333333333333333333333"
            .parse()
            .expect("variable");
        let reserve_tuple = Token::Tuple(vec![
            Token::Tuple(vec![Token::Uint(U256::zero())]),
            Token::Uint(U256::zero()),
            Token::Uint(U256::zero()),
            Token::Uint(U256::zero()),
            Token::Uint(U256::zero()),
            Token::Uint(U256::zero()),
            Token::Uint(U256::zero()),
            Token::Uint(U256::zero()),
            Token::Address(
                "0x1111111111111111111111111111111111111111"
                    .parse()
                    .unwrap(),
            ),
            Token::Address(stable),
            Token::Address(variable),
            Token::Address(
                "0x4444444444444444444444444444444444444444"
                    .parse()
                    .unwrap(),
            ),
            Token::Uint(U256::zero()),
            Token::Uint(U256::zero()),
            Token::Uint(U256::zero()),
        ]);
        let raw = abi::encode(&[reserve_tuple]);
        let decoded = decode_reserve_debt_tokens(&raw).expect("decode");

        assert_eq!(decoded.stable_debt_token, stable);
        assert_eq!(decoded.variable_debt_token, variable);
    }
}
