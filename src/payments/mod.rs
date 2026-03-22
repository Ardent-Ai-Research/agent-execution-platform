//! x402 Payment Layer — on-chain ERC-20 payment verification.
//!
//! Flow:
//! 1. `/execute` determines payment is needed → responds HTTP 402 with amount,
//!    accepted tokens, and the platform treasury address.
//! 2. The AI agent pays the required stablecoin amount on-chain and re-submits
//!    with an `X-Payment-Proof` header containing the payment tx hash.
//! 3. This middleware intercepts the request, performs **real on-chain
//!    verification** (fetches the transaction receipt, decodes ERC-20
//!    `Transfer` event logs, validates recipient / amount / confirmations),
//!    checks for replay, and attaches a [`PaymentProof`] to request extensions.

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use ethers::prelude::*;
use ethers::types::{H160, H256, U256, U64};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::AppConfig;
use crate::db;
use crate::types::PaymentProof;

// ──────────────────────── Constants ──────────────────────────────────

/// ERC-20 `Transfer(address,address,uint256)` event topic.
/// keccak256("Transfer(address,address,uint256)")
const TRANSFER_EVENT_TOPIC: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

// ──────────────────────── 402 response body ──────────────────────────

#[derive(Debug, Serialize)]
pub struct PaymentRequiredBody {
    pub error: String,
    pub amount_usd: f64,
    pub accepted_tokens: Vec<String>,
    pub payment_address: String,
    pub chain: String,
    pub request_id: String,
}

// ──────────────────────── proof header DTO ───────────────────────────

/// Expected JSON payload inside the `X-Payment-Proof` header.
#[derive(Debug, Deserialize)]
pub struct PaymentProofHeader {
    pub payer: String,
    pub amount_usd: f64,
    pub token: String,
    pub chain: String,
    pub tx_hash: String,
}

// ──────────────────────── Payment verifier state ─────────────────────

/// Shared state injected into the x402 middleware layer so it can perform
/// on-chain queries and DB replay checks.
#[derive(Clone)]
pub struct PaymentVerifierState {
    pub config: AppConfig,
    pub eth_provider: Arc<Provider<Http>>,
    pub db_pool: PgPool,
}

// ──────────────────────── On-chain verification ──────────────────────

/// Perform full on-chain verification of a payment proof:
///
/// 1. Parse the `X-Payment-Proof` JSON header.
/// 2. Validate that the token is accepted and resolve its contract address.
/// 3. Check the DB for replay (same tx_hash already used).
/// 4. Fetch the transaction receipt from the chain RPC.
/// 5. Verify the receipt status is success (status = 1).
/// 6. Verify sufficient block confirmations.
/// 7. Decode ERC-20 `Transfer` event logs:
///    - The log must originate from the expected token contract address.
///    - `to` (topic[2]) must equal the platform treasury address.
///    - The transferred amount must meet or exceed the claimed amount.
/// 8. Return a fully populated [`PaymentProof`].
pub async fn verify_payment_on_chain(
    header_value: &str,
    state: &PaymentVerifierState,
) -> Result<PaymentProof, String> {
    // ── 1. Parse header ─────────────────────────────────────────────
    let proof_header: PaymentProofHeader = serde_json::from_str(header_value)
        .map_err(|e| format!("malformed X-Payment-Proof header: {e}"))?;

    if proof_header.tx_hash.is_empty() {
        return Err("tx_hash is required".into());
    }
    if proof_header.amount_usd <= 0.0 {
        return Err("amount must be positive".into());
    }
    if !proof_header.tx_hash.starts_with("0x") || proof_header.tx_hash.len() != 66 {
        return Err("tx_hash must be a 0x-prefixed 32-byte hex string".into());
    }
    if !proof_header.payer.starts_with("0x") || proof_header.payer.len() != 42 {
        return Err("payer must be a valid 0x-prefixed Ethereum address".into());
    }

    let token_upper = proof_header.token.to_uppercase();

    // ── 1b. Validate payment chain ──────────────────────────────────
    // Currently we only have an Ethereum provider for verification.
    // Reject if the declared chain doesn't match what we can actually query.
    let payment_chain = proof_header.chain.to_lowercase();
    let supported_payment_chains = ["ethereum", "eth", "mainnet"];
    if !supported_payment_chains.contains(&payment_chain.as_str()) {
        return Err(format!(
            "payment chain '{}' is not supported for verification (supported: ethereum)",
            proof_header.chain
        ));
    }

    // ── 2. Validate token is accepted ───────────────────────────────
    let expected_token_contract = state
        .config
        .accepted_tokens
        .get(&token_upper)
        .ok_or_else(|| format!("token {} is not accepted for payment", token_upper))?
        .clone();

    let token_decimals = state
        .config
        .token_decimals
        .get(&token_upper)
        .copied()
        .unwrap_or(6); // default to 6 for stablecoins

    let expected_token_addr: H160 = expected_token_contract
        .parse()
        .map_err(|_| "internal error: bad token contract address in config".to_string())?;

    let treasury_addr: H160 = state
        .config
        .payment_address
        .parse()
        .map_err(|_| "internal error: bad treasury address in config".to_string())?;

    // ── 3. Replay protection ────────────────────────────────────────
    let already_used = db::payment_tx_hash_exists(&state.db_pool, &proof_header.tx_hash)
        .await
        .map_err(|e| format!("database error checking replay: {e}"))?;

    if already_used {
        return Err(format!(
            "payment tx {} has already been used",
            proof_header.tx_hash
        ));
    }

    // ── 4. Fetch transaction receipt ────────────────────────────────
    let tx_hash: H256 = proof_header
        .tx_hash
        .parse()
        .map_err(|_| "invalid tx_hash hex".to_string())?;

    let receipt = state
        .eth_provider
        .get_transaction_receipt(tx_hash)
        .await
        .map_err(|e| format!("RPC error fetching receipt: {e}"))?
        .ok_or_else(|| {
            "transaction receipt not found — it may not be mined yet".to_string()
        })?;

    // ── 5. Verify receipt status ────────────────────────────────────
    match receipt.status {
        Some(status) if status == U64::from(1) => { /* success */ }
        Some(_) => return Err("transaction reverted on-chain".into()),
        None => return Err("transaction status unknown (pre-Byzantium receipt)".into()),
    }

    // ── 6. Verify block confirmations ───────────────────────────────
    let current_block: U64 = state
        .eth_provider
        .get_block_number()
        .await
        .map_err(|e| format!("RPC error fetching block number: {e}"))?;

    let tx_block = receipt
        .block_number
        .ok_or("receipt missing block number")?;

    let confirmations = current_block
        .saturating_sub(tx_block)
        .as_u64();

    if confirmations < state.config.min_payment_confirmations {
        return Err(format!(
            "insufficient confirmations: have {confirmations}, need {}",
            state.config.min_payment_confirmations
        ));
    }

    // ── 7. Decode Transfer logs ─────────────────────────────────────
    let transfer_topic: H256 = TRANSFER_EVENT_TOPIC
        .parse()
        .expect("constant transfer topic is valid");

    // Filter logs:
    //   - topic[0] == Transfer event signature
    //   - log.address == expected token contract
    //   - topic[2] (to) == treasury address
    let mut verified_amount: U256 = U256::zero();
    let mut found_matching_transfer = false;

    let payer_addr: H160 = proof_header
        .payer
        .parse()
        .map_err(|_| "invalid payer address".to_string())?;

    for log in &receipt.logs {
        // Must be from the correct token contract
        if log.address != expected_token_addr {
            continue;
        }

        // Must have the Transfer event topic and 3 topics total
        // topics: [Transfer sig, from, to]
        if log.topics.len() != 3 || log.topics[0] != transfer_topic {
            continue;
        }

        // Decode `from` (topic[1]) and `to` (topic[2]) — addresses are
        // zero-padded to 32 bytes in event topics
        let from = H160::from(log.topics[1]);
        let to = H160::from(log.topics[2]);

        // `to` must be the platform treasury
        if to != treasury_addr {
            continue;
        }

        // Verify `from` matches the declared payer
        if from != payer_addr {
            warn!(
                expected_from = %payer_addr,
                actual_from = %from,
                "Transfer.from does not match declared payer — skipping log"
            );
            continue;
        }

        // Decode amount from data (uint256, 32 bytes)
        if log.data.len() < 32 {
            continue;
        }
        let amount = U256::from_big_endian(&log.data[..32]);
        verified_amount = verified_amount.saturating_add(amount);
        found_matching_transfer = true;

        info!(
            from = %from,
            to = %to,
            amount = %amount,
            token = %token_upper,
            "found matching ERC-20 Transfer log"
        );
    }

    if !found_matching_transfer {
        return Err(format!(
            "no valid ERC-20 Transfer to treasury {} from {} in tx {}",
            state.config.payment_address, proof_header.payer, proof_header.tx_hash
        ));
    }

    // ── 8. Verify amount ────────────────────────────────────────────
    // Convert the claimed USD amount to the token's smallest unit.
    // For stablecoins: 1 USD ≈ 1 token unit, so amount_usd * 10^decimals.
    let required_amount_raw =
        U256::from((proof_header.amount_usd * 10f64.powi(token_decimals as i32)) as u128);

    if verified_amount < required_amount_raw {
        let verified_human =
            verified_amount.as_u128() as f64 / 10f64.powi(token_decimals as i32);
        return Err(format!(
            "underpayment: transferred {:.6} {}, required {:.6} {}",
            verified_human, token_upper, proof_header.amount_usd, token_upper
        ));
    }

    let verified_human = verified_amount.as_u128() as f64 / 10f64.powi(token_decimals as i32);

    info!(
        payer = %proof_header.payer,
        amount_usd = proof_header.amount_usd,
        verified_amount = %verified_human,
        token = %token_upper,
        confirmations,
        tx = %proof_header.tx_hash,
        "payment proof verified on-chain ✓"
    );

    Ok(PaymentProof {
        payment_id: Uuid::new_v4(),
        payer: proof_header.payer,
        amount_usd: proof_header.amount_usd,
        token: token_upper,
        chain: proof_header.chain,
        tx_hash: proof_header.tx_hash,
        verified: true,
        verified_at: Utc::now(),
        confirmed_amount_raw: Some(verified_amount.to_string()),
        block_confirmations: Some(confirmations),
        token_contract: Some(expected_token_contract),
    })
}

// ──────────────────────── Axum Middleware ─────────────────────────────

/// Axum middleware layer that intercepts `X-Payment-Proof` headers, performs
/// **real on-chain verification**, checks for replay, and attaches
/// [`PaymentProof`] to request extensions.
///
/// Requires [`PaymentVerifierState`] to be available via Axum state (injected
/// as a request extension by the router layer).
///
/// If the header is absent, the request proceeds normally — individual route
/// handlers are responsible for returning 402 when payment is actually required.
pub async fn x402_middleware(
    State(pv_state): State<PaymentVerifierState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let headers: &HeaderMap = req.headers();

    if let Some(proof_value) = headers.get("X-Payment-Proof") {
        let proof_str = match proof_value.to_str() {
            Ok(s) => s,
            Err(_) => {
                warn!("X-Payment-Proof header is not valid UTF-8");
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "invalid payment proof header encoding" })),
                )
                    .into_response();
            }
        };

        match verify_payment_on_chain(proof_str, &pv_state).await {
            Ok(proof) => {
                info!(payment_id = %proof.payment_id, "payment verified on-chain, attaching to request");
                req.extensions_mut().insert(proof);
            }
            Err(reason) => {
                warn!(reason = %reason, "payment verification failed");
                return (
                    StatusCode::PAYMENT_REQUIRED,
                    Json(serde_json::json!({
                        "error": "payment_verification_failed",
                        "reason": reason,
                    })),
                )
                    .into_response();
            }
        }
    }

    next.run(req).await
}
