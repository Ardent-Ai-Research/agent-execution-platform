//! Paymaster Module — signs paymaster validation data for gas sponsorship
//! (EntryPoint v0.9 format).
//!
//! Our `VerifyingPaymaster` contract checks that each UserOperation carries a
//! signature from a trusted signer (our platform). This module produces that
//! signature after the x402 payment has been verified.
//!
//! Flow:
//! 1. Agent pays via x402 → platform verifies payment on-chain
//! 2. Platform builds the UserOperation
//! 3. This module signs the `(userOp, validUntil, validAfter)` tuple
//! 4. The signed `paymasterAndData` is attached to the UserOp
//! 5. When the bundler submits to the EntryPoint, the Paymaster's
//!    `validatePaymasterUserOp` checks our signature and agrees to pay gas
//!
//! ## v0.9 `paymasterAndData` layout
//!
//! ```text
//! paymaster_address     (20 bytes)
//! pmVerificationGasLimit (16 bytes, uint128 big-endian)
//! pmPostOpGasLimit       (16 bytes, uint128 big-endian)
//! abi.encode(validUntil, validAfter) (64 bytes)
//! signature              (65 bytes)
//! ```
//!
//! Total: 20 + 16 + 16 + 64 + 65 = 181 bytes

use anyhow::{Context, Result};
use ethers::abi::{self, Token};
use ethers::prelude::*;
use ethers::signers::LocalWallet;
use ethers::types::{Address, U256};
use ethers::utils::keccak256;
use tracing::info;

use crate::types::UserOperation;

// ──────────────────────── Paymaster Signer ───────────────────────────

/// Signs paymaster validation data for UserOperations (EntryPoint v0.9).
///
/// The `VerifyingPaymaster` contract expects `paymasterAndData` to be:
/// ```text
/// paymaster_address              (20 bytes)
/// pmVerificationGasLimit         (16 bytes, uint128 big-endian)
/// pmPostOpGasLimit               (16 bytes, uint128 big-endian)
/// abi.encode(validUntil, validAfter) (64 bytes)  ← paymasterData
/// signature                      (65 bytes)      ← appended to paymasterData
/// ```
///
/// Where `signature` is an ECDSA signature over:
/// ```text
/// keccak256(abi.encode(
///     userOp.sender, userOp.nonce,
///     keccak256(initCode), keccak256(callData),
///     accountGasLimits, preVerificationGas, gasFees,
///     chainId,
///     paymaster_address,
///     validUntil, validAfter
/// ))
/// ```
#[derive(Clone)]
pub struct PaymasterSigner {
    /// The paymaster contract address.
    paymaster_address: Address,
    /// The EOA that the Paymaster contract trusts as a signer.
    signing_key: LocalWallet,
    /// How long (seconds) a paymaster signature remains valid.
    validity_window_secs: u64,
    /// Paymaster verification gas limit (default: 100_000).
    pm_verification_gas_limit: U256,
    /// Paymaster post-op gas limit (default: 50_000).
    pm_post_op_gas_limit: U256,
}

impl PaymasterSigner {
    /// Create a new paymaster signer.
    ///
    /// `signing_key_hex` is the hex-encoded private key whose corresponding
    /// address is registered as the trusted signer in the Paymaster contract.
    pub fn new(
        paymaster_address: Address,
        signing_key_hex: &str,
        validity_window_secs: u64,
    ) -> Result<Self> {
        let signing_key: LocalWallet = signing_key_hex
            .parse()
            .context("invalid paymaster signing key")?;

        info!(
            paymaster = %paymaster_address,
            signer = %signing_key.address(),
            "paymaster signer initialized"
        );

        Ok(Self {
            paymaster_address,
            signing_key,
            validity_window_secs,
            pm_verification_gas_limit: U256::from(100_000u64),
            pm_post_op_gas_limit: U256::from(50_000u64),
        })
    }

    /// Build a dummy `paymasterAndData` with the correct byte length for gas
    /// estimation.  The bundler's `eth_estimateUserOperationGas` needs to see a
    /// realistically-sized `paymasterAndData` so that `preVerificationGas`
    /// (which depends on calldata size) is accurate.  The actual signature is
    /// irrelevant for estimation — only the length matters.
    ///
    /// Layout: paymaster(20) + pmVerifGas(16) + pmPostOpGas(16) + zeros(64+65)
    /// = 181 bytes, matching the real signed output.
    pub fn dummy_paymaster_and_data(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(20 + 16 + 16 + 64 + 65);

        // Paymaster address (20 bytes)
        buf.extend_from_slice(self.paymaster_address.as_bytes());

        // pmVerificationGasLimit (16 bytes, uint128 big-endian)
        let mut pm_verif_buf = [0u8; 32];
        self.pm_verification_gas_limit.to_big_endian(&mut pm_verif_buf);
        buf.extend_from_slice(&pm_verif_buf[16..32]);

        // pmPostOpGasLimit (16 bytes, uint128 big-endian)
        let mut pm_postop_buf = [0u8; 32];
        self.pm_post_op_gas_limit.to_big_endian(&mut pm_postop_buf);
        buf.extend_from_slice(&pm_postop_buf[16..32]);

        // Zero-filled paymasterData (abi.encode(validUntil, validAfter) + signature)
        buf.resize(buf.len() + 64 + 65, 0u8);

        buf
    }

    /// Produce the `paymasterAndData` field for a UserOperation (v0.9 format).
    ///
    /// This is called AFTER the UserOp is built but BEFORE signing with the
    /// agent's EOA (since the agent's signature covers the full UserOp
    /// including `paymasterAndData`).
    ///
    /// v0.9 layout:
    /// ```text
    /// paymaster (20 bytes)
    /// pmVerificationGasLimit (16 bytes, uint128 BE)
    /// pmPostOpGasLimit (16 bytes, uint128 BE)
    /// paymasterData = abi.encode(validUntil, validAfter) + signature
    /// ```
    pub async fn sign_paymaster_data(
        &self,
        user_op: &UserOperation,
        chain_id: u64,
    ) -> Result<Vec<u8>> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // validAfter = now (immediately valid)
        // validUntil = now + validity_window (e.g. 5 minutes from now)
        let valid_after = U256::from(now);
        let valid_until = U256::from(now + self.validity_window_secs);

        // Compute the hash that the Paymaster contract will verify
        let hash = self.compute_paymaster_hash(user_op, chain_id, valid_until, valid_after)?;

        // Sign with the platform's paymaster signer key.
        // The VerifyingPaymaster contract wraps the hash with
        // `MessageHashUtils.toEthSignedMessageHash()` ("\x19Ethereum Signed Message:\n32" prefix)
        // before ECDSA.recover, so we must use `sign_message` (which adds the same prefix)
        // rather than `sign_hash` (which signs the raw hash).
        let signature = self
            .signing_key
            .sign_message(hash)
            .await?;

        // Encode time parameters as paymasterData prefix
        let time_params = abi::encode(&[
            Token::Uint(valid_until),
            Token::Uint(valid_after),
        ]);

        let sig_bytes = signature.to_vec();

        // v0.9 paymasterAndData:
        //   paymaster (20) + pmVerifGas (16) + pmPostOpGas (16) + paymasterData
        let mut paymaster_and_data = Vec::with_capacity(20 + 16 + 16 + 64 + 65);

        // Paymaster address (20 bytes)
        paymaster_and_data.extend_from_slice(self.paymaster_address.as_bytes());

        // pmVerificationGasLimit (16 bytes, uint128 big-endian)
        let mut pm_verif_buf = [0u8; 32];
        self.pm_verification_gas_limit.to_big_endian(&mut pm_verif_buf);
        paymaster_and_data.extend_from_slice(&pm_verif_buf[16..32]);

        // pmPostOpGasLimit (16 bytes, uint128 big-endian)
        let mut pm_postop_buf = [0u8; 32];
        self.pm_post_op_gas_limit.to_big_endian(&mut pm_postop_buf);
        paymaster_and_data.extend_from_slice(&pm_postop_buf[16..32]);

        // paymasterData = abi.encode(validUntil, validAfter) + signature
        paymaster_and_data.extend_from_slice(&time_params);
        paymaster_and_data.extend_from_slice(&sig_bytes);

        info!(
            valid_until = now + self.validity_window_secs,
            valid_after = now,
            pm_verif_gas = %self.pm_verification_gas_limit,
            pm_postop_gas = %self.pm_post_op_gas_limit,
            "paymaster data signed (v0.9 format)"
        );

        Ok(paymaster_and_data)
    }

    /// Compute the hash the VerifyingPaymaster will check (v0.9).
    ///
    /// This mirrors the `getHash()` function in the VerifyingPaymaster contract.
    /// It hashes the packed v0.9 UserOp fields (excluding signature and
    /// paymasterAndData) plus the paymaster context.
    fn compute_paymaster_hash(
        &self,
        op: &UserOperation,
        chain_id: u64,
        valid_until: U256,
        valid_after: U256,
    ) -> Result<[u8; 32]> {
        let sender: Address = op.sender.parse()?;
        let nonce = parse_hex_u256(&op.nonce)?;
        let init_code = parse_hex_bytes(&op.init_code)?;
        let call_data = parse_hex_bytes(&op.call_data)?;
        let account_gas_limits = parse_hex_bytes(&op.account_gas_limits)?;
        let pre_verification_gas = parse_hex_u256(&op.pre_verification_gas)?;
        let gas_fees = parse_hex_bytes(&op.gas_fees)?;

        // The Paymaster hashes the packed UserOp fields (v0.9) plus context.
        // Dynamic fields (initCode, callData) are keccak-hashed.
        // Packed fields (accountGasLimits, gasFees) are hashed as-is.
        let pack = abi::encode(&[
            Token::Address(sender),
            Token::Uint(nonce),
            Token::FixedBytes(keccak256(&init_code).to_vec()),
            Token::FixedBytes(keccak256(&call_data).to_vec()),
            Token::FixedBytes(keccak256(&account_gas_limits).to_vec()),
            Token::Uint(pre_verification_gas),
            Token::FixedBytes(keccak256(&gas_fees).to_vec()),
            Token::Uint(U256::from(chain_id)),
            Token::Address(self.paymaster_address),
            Token::Uint(valid_until),
            Token::Uint(valid_after),
        ]);

        Ok(keccak256(&pack))
    }
}

use crate::relayer::utils::{parse_hex_bytes, parse_hex_u256};
