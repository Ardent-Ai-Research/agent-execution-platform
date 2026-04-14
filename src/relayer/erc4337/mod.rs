//! ERC-4337 Bundler Client — builds, signs, and submits PackedUserOperations
//! to an ERC-4337 bundler via JSON-RPC (EntryPoint v0.9).
//!
//! Standard ERC-4337 endpoints:
//! * `eth_sendUserOperation` — submit a UserOp to the bundler mempool
//! * `eth_getUserOperationReceipt` — poll for on-chain inclusion
//! * `eth_estimateUserOperationGas` — gas estimation for UserOps
//! * `eth_getUserOperationByHash` — lookup a UserOp by its hash
//! * `eth_supportedEntryPoints` — discover supported EntryPoint addresses
//!
//! Alchemy-specific endpoints (auto-detected via URL):
//! * `rundler_getUserOperationGasPrice` — bundler-native fee recommendations
//! * `rundler_maxPriorityFeePerGas` — priority fee estimate (fallback)
//!
//! EntryPoint v0.9 changes from v0.6:
//! * PackedUserOperation — 9 fields with packed bytes32 gas limits/fees
//! * EIP-712 typed data hash (domain "ERC4337" version "1")
//! * `executeBatch(Call[])` where `Call = (address, uint256, bytes)`
//! * New paymasterAndData layout: paymaster(20) + pmVerifGas(16) + pmPostOp(16) + data
//!
//! The bundler is accessed via standard JSON-RPC (same as any Ethereum node)
//! so this works with Alchemy, Pimlico, Stackup, or a self-hosted rundler.

use anyhow::{anyhow, Context, Result};
use ethers::abi::{self, Token};
use ethers::prelude::*;
use ethers::types::{Address, Bytes, H256, U256};
use ethers::utils::keccak256;
use serde::Deserialize;
use std::time::Duration;
use tracing::{info, warn};

use crate::types::{ExecutionJob, UserOpResult, UserOperation};

// ──────────────────────── Constants ──────────────────────────────────

/// How long to poll the bundler for a UserOp receipt before giving up.
const USER_OP_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(120);

/// How often to poll for the UserOp receipt.
const USER_OP_POLL_INTERVAL: Duration = Duration::from_secs(3);

/// `execute(address dest, uint256 value, bytes calldata)` selector on BaseAccount.
/// keccak256("execute(address,uint256,bytes)")[:4]
const EXECUTE_SELECTOR: [u8; 4] = [0xb6, 0x1d, 0x27, 0xf6]; // 0xb61d27f6

/// `executeBatch((address,uint256,bytes)[])` selector on BaseAccount (v0.9).
/// keccak256("executeBatch((address,uint256,bytes)[])")[:4]
///
/// v0.9 uses `struct Call { address target; uint256 value; bytes data; }` which
/// ABI-encodes as `(address,uint256,bytes)[]`.  This replaces the v0.6 two-array
/// `executeBatch(address[],bytes[])` = 0x18dfb3c7.
const EXECUTE_BATCH_SELECTOR: [u8; 4] = [0x34, 0xfc, 0xd5, 0xbe]; // 0x34fcd5be

// ──────────────────────── EIP-712 Constants (EntryPoint v0.9) ────────

/// `keccak256("PackedUserOperation(address sender,uint256 nonce,bytes initCode,bytes callData,bytes32 accountGasLimits,uint256 preVerificationGas,bytes32 gasFees,bytes paymasterAndData)")`
const PACKED_USEROP_TYPEHASH: [u8; 32] = [
    0x29, 0xa0, 0xbc, 0xa4, 0xaf, 0x4b, 0xe3, 0x42, 0x13, 0x98, 0xda, 0x00, 0x29, 0x5e, 0x58, 0xe6,
    0xd7, 0xde, 0x38, 0xcb, 0x49, 0x22, 0x14, 0x75, 0x4c, 0xb6, 0xa4, 0x75, 0x07, 0xdd, 0x6f, 0x8e,
];

/// `keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)")`
const EIP712_DOMAIN_TYPEHASH: [u8; 32] = [
    0x8b, 0x73, 0xc3, 0xc6, 0x9b, 0xb8, 0xfe, 0x3d, 0x51, 0x2e, 0xcc, 0x4c, 0xf7, 0x59, 0xcc, 0x79,
    0x23, 0x9f, 0x7b, 0x17, 0x9b, 0x0f, 0xfa, 0xca, 0xa9, 0xa7, 0x5d, 0x52, 0x2b, 0x39, 0x40, 0x0f,
];

/// `keccak256("ERC4337")` — the domain name used in EntryPoint v0.9's EIP-712 constructor.
const EIP712_NAME_HASH: [u8; 32] = [
    0x36, 0x4d, 0xa2, 0x8a, 0x5c, 0x92, 0xbc, 0xc8, 0x7f, 0xe9, 0x7c, 0x88, 0x13, 0xa6, 0xc6, 0xb8,
    0xa3, 0xa0, 0x49, 0xb0, 0xea, 0x0a, 0x32, 0x8f, 0xcb, 0x0b, 0x4f, 0x0e, 0x00, 0x33, 0x75, 0x86,
];

/// `keccak256("1")` — the domain version used in EntryPoint v0.9's EIP-712 constructor.
const EIP712_VERSION_HASH: [u8; 32] = [
    0xc8, 0x9e, 0xfd, 0xaa, 0x54, 0xc0, 0xf2, 0x0c, 0x7a, 0xdf, 0x61, 0x28, 0x82, 0xdf, 0x09, 0x50,
    0xf5, 0xa9, 0x51, 0x63, 0x7e, 0x03, 0x07, 0xcd, 0xcb, 0x4c, 0x67, 0x2f, 0x29, 0x8b, 0x8b, 0xc6,
];

// ──────────────────────── Bundler Client ─────────────────────────────

/// Client for interacting with an ERC-4337 bundler.
#[derive(Clone)]
pub struct BundlerClient {
    /// The bundler JSON-RPC endpoint URL.
    rpc_url: String,
    /// HTTP client for JSON-RPC calls.
    http: reqwest::Client,
    /// The EntryPoint contract address.
    entry_point: Address,
    /// The SimpleAccountFactory contract address.
    factory: Address,
    /// The Ethereum RPC provider (for nonce queries and chain ID).
    provider: std::sync::Arc<Provider<Http>>,
}



impl BundlerClient {
    pub fn new(
        rpc_url: String,
        entry_point: Address,
        factory: Address,
        provider: std::sync::Arc<Provider<Http>>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");

        Self {
            rpc_url,
            http,
            entry_point,
            factory,
            provider,
        }
    }

    /// Return a reference to the underlying Ethereum provider.
    pub fn provider(&self) -> &Provider<Http> {
        &self.provider
    }

    /// Return the configured bundler RPC URL.
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    /// Return the configured EntryPoint address.
    pub fn entry_point(&self) -> Address {
        self.entry_point
    }

    /// Whether the configured bundler endpoint points to Alchemy.
    pub fn is_alchemy_endpoint(&self) -> bool {
        self.rpc_url.contains("alchemy.com")
    }

    /// Build a `PackedUserOperation` (v0.9) for an execution job.
    ///
    /// * **Single call** (default): encodes `BaseAccount.execute(target, value, calldata)`.
    /// * **Batch call** (`batch_calls` is `Some` with entries): encodes
    ///   `BaseAccount.executeBatch(Call[])` (v0.9) so multiple target calls
    ///   happen atomically in a single UserOp.
    ///
    /// Gas limits and fees are packed into `bytes32` fields per v0.9 spec:
    /// - `accountGasLimits = uint128(verificationGasLimit) || uint128(callGasLimit)`
    /// - `gasFees = uint128(maxPriorityFeePerGas) || uint128(maxFeePerGas)`
    pub async fn build_user_op(
        &self,
        job: &ExecutionJob,
        smart_wallet: Address,
        paymaster_and_data: Vec<u8>,
    ) -> Result<UserOperation> {
        // Choose encoding based on whether batch_calls is populated
        let call_data = if let Some(ref batch_calls) = job.batch_calls {
            if batch_calls.is_empty() {
                return Err(anyhow!("batch_calls is present but empty"));
            }
            self.encode_execute_batch_call(batch_calls)?
        } else {
            let target: Address = job.target_contract.parse()?;
            let calldata_bytes = hex::decode(job.calldata.trim_start_matches("0x"))?;
            let value = if job.value.is_empty() || job.value == "0" {
                U256::zero()
            } else {
                U256::from_dec_str(&job.value)?
            };
            self.encode_execute_call(target, value, &calldata_bytes)
        };

        // Get the smart wallet's nonce from the EntryPoint
        let nonce = self.get_sender_nonce(smart_wallet).await?;

        // Determine if we need initCode (first-time deployment)
        let init_code = self
            .get_init_code_if_needed(smart_wallet, job.eoa_address.parse()?)
            .await?;

        // Build a dummy op for gas estimation (zero gas fields)
        let dummy_account_gas_limits = format!("0x{}", "0".repeat(64));
        let dummy_gas_fees = format!("0x{}", "0".repeat(64));

        let (call_gas, verification_gas, pre_verification_gas) = self
            .estimate_gas(&UserOperation {
                sender: format!("{smart_wallet:?}"),
                nonce: format!("{nonce:#x}"),
                init_code: format!("0x{}", hex::encode(&init_code)),
                call_data: format!("0x{}", hex::encode(&call_data)),
                account_gas_limits: dummy_account_gas_limits,
                pre_verification_gas: "0x0".into(),
                gas_fees: dummy_gas_fees,
                paymaster_and_data: format!("0x{}", hex::encode(&paymaster_and_data)),
                signature: format!("0x{}", hex::encode([0u8; 65])), // dummy sig for estimation
            })
            .await
            .context("bundler gas estimation failed")?;

        // Get current gas prices
        let (max_fee, priority_fee) = self.get_gas_prices().await?;

        // Pack gas fields into bytes32 per v0.9 spec
        let account_gas_limits = pack_two_uint128(verification_gas, call_gas);
        let gas_fees = pack_two_uint128(priority_fee, max_fee);

        Ok(UserOperation {
            sender: format!("{smart_wallet:?}"),
            nonce: format!("{nonce:#x}"),
            init_code: format!("0x{}", hex::encode(&init_code)),
            call_data: format!("0x{}", hex::encode(&call_data)),
            account_gas_limits: format!("0x{}", hex::encode(account_gas_limits)),
            pre_verification_gas: format!("{pre_verification_gas:#x}"),
            gas_fees: format!("0x{}", hex::encode(gas_fees)),
            paymaster_and_data: format!("0x{}", hex::encode(&paymaster_and_data)),
            signature: "0x".into(), // placeholder — will be set after signing
        })
    }

    /// Compute the ERC-4337 v0.9 UserOperation hash (EIP-712 typed data hash).
    ///
    /// This is the hash that needs to be signed by the agent's EOA key.
    /// Uses the EntryPoint v0.9 EIP-712 domain: name="ERC4337", version="1".
    pub async fn user_op_hash(&self, user_op: &UserOperation) -> Result<H256> {
        let chain_id = self.provider.get_chainid().await?.as_u64();
        let raw = self.compute_user_op_hash(user_op, chain_id)?;
        Ok(H256::from(raw))
    }

    /// Apply a pre-computed signature to a UserOperation.
    pub fn apply_signature(
        &self,
        mut user_op: UserOperation,
        signature: ethers::types::Signature,
    ) -> UserOperation {
        user_op.signature = format!("0x{}", hex::encode(signature.to_vec()));
        user_op
    }

    /// Submit a signed UserOperation to the bundler and wait for inclusion.
    pub async fn submit_and_wait(
        &self,
        user_op: &UserOperation,
    ) -> Result<UserOpResult> {
        // 1. Submit to bundler
        let user_op_hash = self.send_user_operation(user_op).await?;
        info!(user_op_hash = %user_op_hash, "UserOperation submitted to bundler");

        // 2. Poll for receipt
        self.wait_for_receipt(&user_op_hash).await
    }

    // ──────────────────── JSON-RPC calls ─────────────────────────────

    /// `eth_sendUserOperation(userOp, entryPoint)` → userOpHash
    async fn send_user_operation(&self, user_op: &UserOperation) -> Result<String> {
        let response: JsonRpcResponse<String> = self
            .rpc_call(
                "eth_sendUserOperation",
                serde_json::json!([user_op, format!("{:?}", self.entry_point)]),
            )
            .await?;

        response.result.ok_or_else(|| {
            anyhow!(
                "bundler rejected UserOperation: {}",
                response.error.map(|e| e.message).unwrap_or_default()
            )
        })
    }

    /// `eth_getUserOperationReceipt(userOpHash)` → receipt
    async fn get_user_op_receipt(
        &self,
        user_op_hash: &str,
    ) -> Result<Option<UserOpReceiptResponse>> {
        let response: JsonRpcResponse<UserOpReceiptResponse> = self
            .rpc_call(
                "eth_getUserOperationReceipt",
                serde_json::json!([user_op_hash]),
            )
            .await?;

        Ok(response.result)
    }

    /// `eth_getUserOperationByHash(userOpHash)` → operation details
    pub async fn get_user_operation_by_hash(
        &self,
        user_op_hash: &str,
    ) -> Result<Option<UserOpByHashResponse>> {
        let response: JsonRpcResponse<UserOpByHashResponse> = self
            .rpc_call(
                "eth_getUserOperationByHash",
                serde_json::json!([user_op_hash]),
            )
            .await?;

        Ok(response.result)
    }

    /// Validate that our configured EntryPoint address is supported by the
    /// bundler. Call once at startup — not on every send.
    pub async fn validate_entry_point_supported(&self) -> Result<()> {
        let supported = self.supported_entry_points().await?;
        let configured = format!("{:?}", self.entry_point).to_lowercase();
        let is_supported = supported
            .iter()
            .any(|addr| addr.to_lowercase() == configured);

        if !is_supported {
            return Err(anyhow!(
                "configured ENTRY_POINT_ADDRESS {} is not supported by bundler; supported: {:?}",
                configured,
                supported
            ));
        }
        info!(entry_point = %configured, "bundler entry point validated ✓");
        Ok(())
    }

    /// `eth_supportedEntryPoints()` → list of supported entry points
    pub async fn supported_entry_points(&self) -> Result<Vec<String>> {
        let response: JsonRpcResponse<Vec<String>> = self
            .rpc_call("eth_supportedEntryPoints", serde_json::json!([]))
            .await?;

        response.result.ok_or_else(|| {
            anyhow!(
                "failed to fetch supported entry points: {}",
                response.error.map(|e| e.message).unwrap_or_default()
            )
        })
    }

    /// `eth_estimateUserOperationGas(userOp, entryPoint[, stateOverride])` → gas estimates
    ///
    /// The optional `state_override` parameter allows modifying contract state
    /// before estimation (e.g. setting token balances for simulation).  Alchemy
    /// and most modern bundlers support this as the third JSON-RPC parameter.
    async fn estimate_gas(
        &self,
        user_op: &UserOperation,
    ) -> Result<(U256, U256, U256)> {
        let params = vec![
            serde_json::to_value(user_op)?,
            serde_json::json!(format!("{:?}", self.entry_point)),
        ];

        let response: JsonRpcResponse<GasEstimateResponse> = self
            .rpc_call(
                "eth_estimateUserOperationGas",
                serde_json::Value::Array(params),
            )
            .await?;

        let estimate = response.result.ok_or_else(|| {
            anyhow!(
                "gas estimation failed: {}",
                response.error.map(|e| e.message).unwrap_or_default()
            )
        })?;

        let call_gas = U256::from_str_radix(estimate.call_gas_limit.trim_start_matches("0x"), 16)
            .context("invalid callGasLimit from bundler")?;

        let verification_gas = U256::from_str_radix(
            estimate.verification_gas_limit.trim_start_matches("0x"),
            16,
        )
        .context("invalid verificationGasLimit from bundler")?;

        let pre_verification_gas = U256::from_str_radix(
            estimate.pre_verification_gas.trim_start_matches("0x"),
            16,
        )
        .context("invalid preVerificationGas from bundler")?;

        Ok((call_gas, verification_gas, pre_verification_gas))
    }

    /// Fetch gas price recommendations from the bundler via
    /// `rundler_getUserOperationGasPrice`.
    ///
    /// Returns `(maxFeePerGas, maxPriorityFeePerGas)` from the bundler's
    /// `suggested` tier.  All UserOperations go through the bundler, so
    /// the bundler is the authoritative source for fee recommendations.
    async fn fetch_gas_prices_from_bundler(&self) -> Result<(U256, U256)> {
        let response: JsonRpcResponse<RundlerGasPriceResponse> = self
            .rpc_call("rundler_getUserOperationGasPrice", serde_json::json!([]))
            .await?;

        let result = response.result.ok_or_else(|| {
            anyhow!(
                "rundler_getUserOperationGasPrice returned no result: {}",
                response.error.map(|e| e.message).unwrap_or_default()
            )
        })?;

        info!(
            base_fee = %result.base_fee,
            block_number = %result.block_number,
            current_priority_fee = %result.current_priority_fee,
            "bundler gas price snapshot"
        );

        let max_fee = U256::from_str_radix(
            result.suggested.max_fee_per_gas.trim_start_matches("0x"),
            16,
        )
        .context("invalid rundler suggested maxFeePerGas")?;

        let max_priority = U256::from_str_radix(
            result
                .suggested
                .max_priority_fee_per_gas
                .trim_start_matches("0x"),
            16,
        )
        .context("invalid rundler suggested maxPriorityFeePerGas")?;

        if max_fee.is_zero() {
            return Err(anyhow!("bundler returned zero maxFeePerGas"));
        }

        Ok((max_fee, max_priority))
    }

    // ──────────────────── Internal helpers ────────────────────────────

    /// Encode `BaseAccount.execute(address dest, uint256 value, bytes data)`.
    fn encode_execute_call(&self, target: Address, value: U256, calldata: &[u8]) -> Vec<u8> {
        let tokens = vec![
            Token::Address(target),
            Token::Uint(value),
            Token::Bytes(calldata.to_vec()),
        ];
        let encoded_params = abi::encode(&tokens);
        let mut result = Vec::with_capacity(4 + encoded_params.len());
        result.extend_from_slice(&EXECUTE_SELECTOR);
        result.extend_from_slice(&encoded_params);
        result
    }

    /// Encode `BaseAccount.executeBatch(Call[] calldata calls)` (v0.9).
    ///
    /// Each `Call` is a tuple `(address target, uint256 value, bytes data)`.
    /// v0.9 supports per-call ETH values natively via the `Call` struct.
    fn encode_execute_batch_call(
        &self,
        batch_calls: &[crate::types::BatchCall],
    ) -> Result<Vec<u8>> {
        let mut call_tuples = Vec::with_capacity(batch_calls.len());

        for call in batch_calls {
            let target: Address = call
                .target_contract
                .parse()
                .context("invalid target_contract in batch call")?;
            let calldata_bytes =
                hex::decode(call.calldata.trim_start_matches("0x"))
                    .context("invalid calldata hex in batch call")?;
            let value = if call.value.trim().is_empty() || call.value.trim() == "0" {
                U256::zero()
            } else {
                U256::from_dec_str(call.value.trim())
                    .context("invalid value in batch call")?
            };

            call_tuples.push(Token::Tuple(vec![
                Token::Address(target),
                Token::Uint(value),
                Token::Bytes(calldata_bytes),
            ]));
        }

        let tokens = vec![Token::Array(call_tuples)];
        let encoded_params = abi::encode(&tokens);
        let mut result = Vec::with_capacity(4 + encoded_params.len());
        result.extend_from_slice(&EXECUTE_BATCH_SELECTOR);
        result.extend_from_slice(&encoded_params);
        Ok(result)
    }

    /// Get the smart wallet's nonce from the EntryPoint.
    /// Uses `getNonce(address sender, uint192 key)` where key = 0.
    async fn get_sender_nonce(&self, sender: Address) -> Result<U256> {
        // getNonce(address,uint192) selector = 0x35567e1a
        let selector = hex::decode("35567e1a")?;
        let params = abi::encode(&[
            Token::Address(sender),
            Token::Uint(U256::zero()), // key = 0
        ]);
        let mut calldata = Vec::with_capacity(4 + params.len());
        calldata.extend_from_slice(&selector);
        calldata.extend_from_slice(&params);

        let tx = ethers::types::TransactionRequest::new()
            .to(self.entry_point)
            .data(Bytes::from(calldata));

        let result = self.provider.call(&tx.into(), None).await?;
        let nonce = U256::from_big_endian(&result);
        Ok(nonce)
    }

    /// Check if the smart wallet is already deployed. If not, return the
    /// initCode (factory address + createAccount calldata).
    async fn get_init_code_if_needed(
        &self,
        smart_wallet: Address,
        owner: Address,
    ) -> Result<Vec<u8>> {
        let code = self.provider.get_code(smart_wallet, None).await?;
        if !code.is_empty() {
            // Already deployed — no initCode needed
            return Ok(Vec::new());
        }

        // First-time deployment: initCode = factory_address ++ createAccount(owner, 0)
        let params = abi::encode(&[
            Token::Address(owner),
            Token::Uint(U256::zero()), // salt = 0
        ]);
        let mut factory_calldata = Vec::with_capacity(4 + params.len());
        factory_calldata.extend_from_slice(&[0x5f, 0xbf, 0xb9, 0xcf]); // createAccount selector
        factory_calldata.extend_from_slice(&params);

        let mut init_code = Vec::with_capacity(20 + factory_calldata.len());
        init_code.extend_from_slice(self.factory.as_bytes());
        init_code.extend_from_slice(&factory_calldata);

        info!(
            smart_wallet = %smart_wallet,
            "smart wallet not yet deployed — including initCode"
        );

        Ok(init_code)
    }

    /// Get current gas prices from the bundler.
    ///
    /// All UserOperations are submitted through the bundler, so it is the
    /// single source of truth for fee recommendations.  This is also used
    /// by the pricing module for cost estimation.
    pub async fn get_gas_prices(&self) -> Result<(U256, U256)> {
        self.fetch_gas_prices_from_bundler().await
    }

    /// Compute the v0.9 UserOperation hash using EIP-712 typed data hashing.
    ///
    /// ```text
    /// userOpHash = keccak256("\x19\x01" || domainSeparator || structHash)
    /// ```
    ///
    /// Where:
    /// - `domainSeparator = keccak256(abi.encode(DOMAIN_TYPEHASH, nameHash, versionHash, chainId, entryPoint))`
    /// - `structHash = keccak256(abi.encode(PACKED_USEROP_TYPEHASH, sender, nonce, keccak256(initCode), keccak256(callData), accountGasLimits, preVerificationGas, gasFees, keccak256(paymasterAndData)))`
    fn compute_user_op_hash(&self, op: &UserOperation, chain_id: u64) -> Result<[u8; 32]> {
        let sender: Address = op.sender.parse()?;
        let nonce = parse_hex_u256(&op.nonce)?;
        let init_code = parse_hex_bytes(&op.init_code)?;
        let call_data = parse_hex_bytes(&op.call_data)?;
        let account_gas_limits = parse_hex_bytes32(&op.account_gas_limits)?;
        let pre_verification_gas = parse_hex_u256(&op.pre_verification_gas)?;
        let gas_fees = parse_hex_bytes32(&op.gas_fees)?;
        let paymaster_and_data = parse_hex_bytes(&op.paymaster_and_data)?;

        // Struct hash per PACKED_USEROP_TYPEHASH
        let struct_data = abi::encode(&[
            Token::FixedBytes(PACKED_USEROP_TYPEHASH.to_vec()),
            Token::Address(sender),
            Token::Uint(nonce),
            Token::FixedBytes(keccak256(&init_code).to_vec()),
            Token::FixedBytes(keccak256(&call_data).to_vec()),
            Token::FixedBytes(account_gas_limits.to_vec()),
            Token::Uint(pre_verification_gas),
            Token::FixedBytes(gas_fees.to_vec()),
            Token::FixedBytes(keccak256(&paymaster_and_data).to_vec()),
        ]);
        let struct_hash = keccak256(&struct_data);

        // Domain separator
        let domain_data = abi::encode(&[
            Token::FixedBytes(EIP712_DOMAIN_TYPEHASH.to_vec()),
            Token::FixedBytes(EIP712_NAME_HASH.to_vec()),
            Token::FixedBytes(EIP712_VERSION_HASH.to_vec()),
            Token::Uint(U256::from(chain_id)),
            Token::Address(self.entry_point),
        ]);
        let domain_separator = keccak256(&domain_data);

        // EIP-712: keccak256("\x19\x01" || domainSeparator || structHash)
        let mut eip712_msg = Vec::with_capacity(2 + 32 + 32);
        eip712_msg.push(0x19);
        eip712_msg.push(0x01);
        eip712_msg.extend_from_slice(&domain_separator);
        eip712_msg.extend_from_slice(&struct_hash);

        Ok(keccak256(&eip712_msg))
    }

    /// Poll the bundler for a UserOperation receipt until it appears or timeout.
    async fn wait_for_receipt(&self, user_op_hash: &str) -> Result<UserOpResult> {
        let deadline = tokio::time::Instant::now() + USER_OP_CONFIRMATION_TIMEOUT;

        loop {
            if tokio::time::Instant::now() >= deadline {
                return Ok(UserOpResult {
                    user_op_hash: user_op_hash.to_string(),
                    tx_hash: None,
                    success: false,
                    error: Some(format!(
                        "timeout after {}s waiting for UserOp receipt",
                        USER_OP_CONFIRMATION_TIMEOUT.as_secs()
                    )),
                    block_number: None,
                    gas_used: None,
                });
            }

            match self.get_user_op_receipt(user_op_hash).await {
                Ok(Some(receipt)) => {
                    let success = receipt.success;
                    let tx_hash = receipt.receipt.as_ref().and_then(|r| {
                        r.get("transactionHash")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                    });
                    let block_number = receipt.receipt.as_ref().and_then(|r| {
                        r.get("blockNumber")
                            .and_then(|v| v.as_str())
                            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                    });
                    let gas_used = receipt.actual_gas_used.as_ref().and_then(|s| {
                        u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()
                    });

                    if success {
                        info!(
                            user_op_hash,
                            tx_hash = ?tx_hash,
                            block = ?block_number,
                            gas_used = ?gas_used,
                            "UserOperation confirmed on-chain ✓"
                        );
                    } else {
                        warn!(
                            user_op_hash,
                            "UserOperation failed on-chain"
                        );
                    }

                    return Ok(UserOpResult {
                        user_op_hash: user_op_hash.to_string(),
                        tx_hash,
                        success,
                        error: if success { None } else { Some("UserOp reverted on-chain".into()) },
                        block_number,
                        gas_used,
                    });
                }
                Ok(None) => {
                    // Not yet included, keep polling
                    tokio::time::sleep(USER_OP_POLL_INTERVAL).await;
                }
                Err(e) => {
                    warn!(error = %e, "error polling UserOp receipt, retrying");
                    tokio::time::sleep(USER_OP_POLL_INTERVAL).await;
                }
            }
        }
    }

    /// Make a JSON-RPC call to the bundler.
    async fn rpc_call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<JsonRpcResponse<T>> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        let resp = self
            .http
            .post(&self.rpc_url)
            .json(&body)
            .send()
            .await
            .context("bundler RPC request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("bundler returned HTTP {status}: {body_text}"));
        }

        let result: JsonRpcResponse<T> = resp.json().await.context("failed to parse bundler response")?;
        Ok(result)
    }
}

// ──────────────────────── JSON-RPC types ─────────────────────────────

#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserOpReceiptResponse {
    success: bool,
    actual_gas_used: Option<String>,
    receipt: Option<serde_json::Value>,
}

/// Gas estimation response from `eth_estimateUserOperationGas`.
///
/// v0.9 bundlers may also return `paymasterVerificationGasLimit` and
/// `paymasterPostOpGasLimit` when a paymaster is present.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct GasEstimateResponse {
    call_gas_limit: String,
    verification_gas_limit: String,
    pre_verification_gas: String,
    /// Paymaster verification gas (returned by v0.9 bundlers when paymaster is used).
    #[serde(default)]
    paymaster_verification_gas_limit: Option<String>,
    /// Paymaster post-op gas (returned by v0.9 bundlers when paymaster is used).
    #[serde(default)]
    paymaster_post_op_gas_limit: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RundlerGasPriceResponse {
    /// Current base fee from the latest block.
    #[serde(default)]
    base_fee: String,
    /// Latest block number the recommendation is based on.
    #[serde(default)]
    block_number: String,
    /// Current priority fee observed by the bundler.
    #[serde(default)]
    current_priority_fee: String,
    /// Suggested maxFeePerGas and maxPriorityFeePerGas with safety buffers.
    suggested: SuggestedGasPrice,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SuggestedGasPrice {
    max_priority_fee_per_gas: String,
    max_fee_per_gas: String,
}

/// Response from `eth_getUserOperationByHash` (v0.9 packed format).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserOpByHashResponse {
    pub sender: Option<String>,
    pub nonce: Option<serde_json::Value>,
    pub init_code: Option<String>,
    pub call_data: Option<String>,
    pub account_gas_limits: Option<String>,
    pub pre_verification_gas: Option<String>,
    pub gas_fees: Option<String>,
    pub paymaster_and_data: Option<String>,
    pub signature: Option<String>,
    pub entry_point: Option<String>,
    pub block_number: Option<String>,
    pub block_hash: Option<String>,
    pub transaction_hash: Option<String>,
}

use crate::relayer::utils::{parse_hex_bytes, parse_hex_u256};

// ──────────────────────── Packing helpers ────────────────────────────

/// Pack two `U256` values into a `bytes32` by treating each as `uint128`:
/// `result = uint128(high) || uint128(low)`
///
/// The high value occupies the upper 16 bytes, the low value the lower 16.
/// Both values MUST fit in 128 bits — this is enforced by the caller via
/// the bundler's gas estimation (which returns values well within uint128).
fn pack_two_uint128(high: U256, low: U256) -> [u8; 32] {
    let mut result = [0u8; 32];
    // high → bytes [0..16] (big-endian uint128)
    let mut high_buf = [0u8; 32];
    high.to_big_endian(&mut high_buf);
    result[..16].copy_from_slice(&high_buf[16..32]);
    // low → bytes [16..32] (big-endian uint128)
    let mut low_buf = [0u8; 32];
    low.to_big_endian(&mut low_buf);
    result[16..].copy_from_slice(&low_buf[16..32]);
    result
}

/// Parse a hex-encoded bytes32 string into a fixed 32-byte array.
fn parse_hex_bytes32(s: &str) -> Result<[u8; 32]> {
    let bytes = parse_hex_bytes(s)?;
    if bytes.is_empty() {
        return Ok([0u8; 32]);
    }
    // Left-pad to 32 bytes if shorter
    let mut result = [0u8; 32];
    if bytes.len() <= 32 {
        result[32 - bytes.len()..].copy_from_slice(&bytes);
    } else {
        return Err(anyhow!("bytes32 value too long: {} bytes", bytes.len()));
    }
    Ok(result)
}
