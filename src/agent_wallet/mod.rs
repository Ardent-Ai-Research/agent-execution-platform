//! Agent Wallet Registry — provisions and manages ERC-4337 smart wallets
//! for AI agents.
//!
//! Each agent is identified by a namespaced ID (`{api_key_id}::{agent_id}`).
//! On first interaction the registry:
//!   1. Generates a fresh EOA signing key
//!   2. Encrypts it with AES-256-GCM and persists to Postgres
//!   3. Derives the counterfactual `SimpleAccount` address by calling
//!      `factory.getAddress(owner, salt)` on-chain
//!
//! On subsequent interactions the registry returns the existing wallet.
//!
//! **Security properties:**
//! - Signing keys are *never* held decrypted longer than the signing operation.
//! - The master encryption key is zeroized on drop.
//! - `Debug` impls redact all secret material.

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Context, Result};
use ethers::abi::{self, Token};
use ethers::prelude::*;
use ethers::signers::LocalWallet;
use ethers::types::{Address, Bytes, H256, U256};
use rand::RngCore;
use sqlx::PgPool;
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

// ──────────────────────── Constants ──────────────────────────────────

/// Canonical ERC-4337 EntryPoint v0.9 (deployed on all major EVM chains).
pub const ENTRY_POINT_V09: &str = "0x433709009B8330FDa32311DF1C2AFA402eD8D009";

/// `getAddress(address owner, uint256 salt)` selector on SimpleAccountFactory.
/// keccak256("getAddress(address,uint256)")[:4]
const GET_ADDRESS_SELECTOR: [u8; 4] = [0x8c, 0xb8, 0x4e, 0x18]; // 0x8cb84e18

// ──────────────────────── Public types ───────────────────────────────

/// A fully resolved agent wallet.
///
/// The signing key is stored **encrypted** — it is never held decrypted
/// in this struct.  Use [`AgentWalletRegistry::decrypt_and_sign`] to
/// perform a signing operation with minimal key exposure.
#[derive(Clone)]
pub struct AgentWallet {
    pub id: Uuid,
    pub api_key_id: Uuid,
    pub agent_id: String,
    pub namespaced_id: String,
    pub eoa_address: Address,
    pub smart_wallet_address: Address,
    /// The **encrypted** signing key blob (base64).  Never decrypted except
    /// inside [`AgentWalletRegistry::decrypt_and_sign`].
    pub signing_key_encrypted: String,
}

impl std::fmt::Debug for AgentWallet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentWallet")
            .field("id", &self.id)
            .field("api_key_id", &self.api_key_id)
            .field("agent_id", &self.agent_id)
            .field("namespaced_id", &self.namespaced_id)
            .field("eoa_address", &self.eoa_address)
            .field("smart_wallet_address", &self.smart_wallet_address)
            .field("signing_key_encrypted", &"[REDACTED]")
            .finish()
    }
}

// ──────────────────────── Registry ───────────────────────────────

/// The wallet registry manages creation and retrieval of agent wallets.
///
/// The master encryption key is zeroized when the registry is dropped.
///
/// Multi-chain note: smart wallet addresses are derived via CREATE2 and
/// are deterministic per (factory, owner, salt).  If the same factory
/// bytecode is deployed at the same address on every chain (standard
/// ERC-4337 practice), the smart wallet address is chain-independent.
/// The registry stores a single address per agent and uses one provider
/// for derivation.
pub struct AgentWalletRegistry {
    db_pool: PgPool,
    /// AES-256-GCM key (32 bytes) used to encrypt/decrypt signing keys at rest.
    /// Zeroized on drop to prevent key material from lingering in freed memory.
    encryption_key: EncryptionKey,
    /// Address of the SimpleAccountFactory contract (same on all chains via CREATE2).
    factory_address: Address,
    /// Provider used for `factory.getAddress()` calls during wallet provisioning.
    /// Any configured chain's provider works since the factory returns a
    /// deterministic address.
    provider: Arc<Provider<Http>>,
}

/// Wrapper around the raw 32-byte encryption key that zeroizes on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
struct EncryptionKey([u8; 32]);

// Manual Clone for AgentWalletRegistry since EncryptionKey doesn't auto-derive
// all the traits that #[derive(Clone)] on the outer struct expects.
impl Clone for AgentWalletRegistry {
    fn clone(&self) -> Self {
        Self {
            db_pool: self.db_pool.clone(),
            encryption_key: self.encryption_key.clone(),
            factory_address: self.factory_address,
            provider: self.provider.clone(),
        }
    }
}

impl std::fmt::Debug for AgentWalletRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentWalletRegistry")
            .field("factory_address", &self.factory_address)
            .field("encryption_key", &"[REDACTED]")
            .finish()
    }
}

impl AgentWalletRegistry {
    /// Create a new registry.
    ///
    /// `encryption_key_hex` must be a 64-char hex string (32 bytes).
    /// The `provider` is required — smart wallet address derivation calls
    /// `factory.getAddress(owner, salt)` on-chain, which requires an RPC
    /// connection.
    pub fn new(
        db_pool: PgPool,
        encryption_key_hex: &str,
        factory_address: Address,
        provider: Arc<Provider<Http>>,
    ) -> Result<Self> {
        let key_bytes = hex::decode(encryption_key_hex)
            .context("WALLET_ENCRYPTION_KEY must be valid hex")?;
        if key_bytes.len() != 32 {
            return Err(anyhow!(
                "WALLET_ENCRYPTION_KEY must be exactly 32 bytes (64 hex chars), got {}",
                key_bytes.len()
            ));
        }
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&key_bytes);

        Ok(Self {
            db_pool,
            encryption_key: EncryptionKey(raw),
            factory_address,
            provider,
        })
    }

    /// Get or create an agent wallet for the given (api_key_id, agent_id) pair.
    ///
    /// This is the primary entry point — called on every `/execute` request.
    pub async fn get_or_create(
        &self,
        api_key_id: Uuid,
        agent_id: &str,
    ) -> Result<AgentWallet> {
        let namespaced_id = format!("{api_key_id}::{agent_id}");

        // Try to load existing wallet first
        if let Some(wallet) = self.load_wallet(&namespaced_id).await? {
            return Ok(wallet);
        }

        // First interaction — provision a new wallet
        self.provision_wallet(api_key_id, agent_id, &namespaced_id)
            .await
    }

    /// Load an existing wallet from the database.
    ///
    /// Returns the wallet with an **encrypted** signing key.  The key is
    /// never decrypted at load time.
    async fn load_wallet(&self, namespaced_id: &str) -> Result<Option<AgentWallet>> {
        let row = sqlx::query_as::<_, AgentWalletRow>(
            "SELECT * FROM agent_wallets WHERE namespaced_id = $1",
        )
        .bind(namespaced_id)
        .fetch_optional(&self.db_pool)
        .await?;

        match row {
            None => Ok(None),
            Some(row) => {
                let eoa_address: Address = row.eoa_address.parse()
                    .context("corrupt eoa_address in DB")?;
                let smart_wallet_address: Address = row.smart_wallet_address.parse()
                    .context("corrupt smart_wallet_address in DB")?;

                Ok(Some(AgentWallet {
                    id: row.id,
                    api_key_id: row.api_key_id,
                    agent_id: row.agent_id,
                    namespaced_id: row.namespaced_id,
                    eoa_address,
                    smart_wallet_address,
                    signing_key_encrypted: row.signing_key_encrypted,
                }))
            }
        }
    }

    /// Provision a brand-new wallet: generate EOA, derive smart wallet, persist.
    async fn provision_wallet(
        &self,
        api_key_id: Uuid,
        agent_id: &str,
        namespaced_id: &str,
    ) -> Result<AgentWallet> {
        // 1. Generate a fresh random EOA
        let signing_key = LocalWallet::new(&mut rand::thread_rng());
        let eoa_address = signing_key.address();

        // 2. Derive counterfactual SimpleAccount address
        let smart_wallet_address = self.compute_smart_wallet_address(eoa_address).await?;

        // 3. Encrypt the private key for storage
        let mut key_hex = hex::encode(signing_key.signer().to_bytes());
        let encrypted = self.encrypt_signing_key(&key_hex)?;
        // Zeroize the plaintext hex string immediately
        key_hex.zeroize();
        // Drop the LocalWallet — we don't keep it around
        drop(signing_key);

        // 4. Persist
        let wallet_id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO agent_wallets
                (id, api_key_id, agent_id, namespaced_id, signing_key_encrypted,
                 eoa_address, smart_wallet_address, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, now())
            ON CONFLICT (namespaced_id) DO NOTHING
            "#,
        )
        .bind(wallet_id)
        .bind(api_key_id)
        .bind(agent_id)
        .bind(namespaced_id)
        .bind(&encrypted)
        .bind(format!("{eoa_address:?}"))
        .bind(format!("{smart_wallet_address:?}"))
        .execute(&self.db_pool)
        .await?;

        // Handle race condition: if another request created the wallet first,
        // load that one instead.
        if let Some(existing) = self.load_wallet(namespaced_id).await? {
            return Ok(existing);
        }

        info!(
            agent_id,
            namespaced_id,
            eoa = %eoa_address,
            smart_wallet = %smart_wallet_address,
            "provisioned new agent wallet"
        );

        Ok(AgentWallet {
            id: wallet_id,
            api_key_id,
            agent_id: agent_id.to_string(),
            namespaced_id: namespaced_id.to_string(),
            eoa_address,
            smart_wallet_address,
            signing_key_encrypted: encrypted,
        })
    }

    /// Compute the counterfactual `SimpleAccount` address by calling
    /// `factory.getAddress(owner, 0)` on-chain.
    ///
    /// This is a view function (zero gas) that returns the deterministic
    /// address the factory would deploy to for the given (owner, salt) pair.
    /// Always accurate regardless of factory version or proxy init code.
    pub async fn compute_smart_wallet_address(&self, owner: Address) -> Result<Address> {
        let provider = &self.provider;
        // Encode: getAddress(address owner, uint256 salt)
        let params = abi::encode(&[
            Token::Address(owner),
            Token::Uint(U256::zero()), // salt = 0
        ]);
        let mut calldata = Vec::with_capacity(4 + params.len());
        calldata.extend_from_slice(&GET_ADDRESS_SELECTOR);
        calldata.extend_from_slice(&params);

        let tx = ethers::types::TransactionRequest::new()
            .to(self.factory_address)
            .data(Bytes::from(calldata));

        let result = provider
            .call(&tx.into(), None)
            .await
            .context("factory.getAddress() call failed — is ACCOUNT_FACTORY_ADDRESS correct?")?;

        // The return value is an ABI-encoded address (32 bytes, right-padded)
        if result.len() < 32 {
            return Err(anyhow!(
                "factory.getAddress() returned unexpected data length: {}",
                result.len()
            ));
        }

        let address = Address::from_slice(&result[12..32]);

        info!(
            owner = %owner,
            smart_wallet = %address,
            "computed smart wallet address via factory.getAddress()"
        );

        Ok(address)
    }

    // ──────────────────── Signing (minimal key exposure) ───────────────

    /// Decrypt the agent's signing key, sign a hash, and immediately zeroize
    /// the decrypted key material.
    ///
    /// This is the **only** path through which decrypted keys are used.
    /// The plaintext key exists in memory only for the duration of the
    /// signing operation.
    pub fn decrypt_and_sign(
        &self,
        wallet: &AgentWallet,
        hash: H256,
    ) -> Result<ethers::types::Signature> {
        let signing_key = self.decrypt_signing_key(&wallet.signing_key_encrypted)?;
        let signature = signing_key.sign_hash(hash)?;
        // `signing_key` (LocalWallet) is dropped here — Rust drops at end of scope.
        // The underlying k256 SecretKey implements ZeroizeOnDrop.
        Ok(signature)
    }

    // ──────────────────── Encryption helpers ─────────────────────────

    /// Encrypt a hex-encoded private key using AES-256-GCM.
    /// Returns: base64(nonce || ciphertext)
    fn encrypt_signing_key(&self, key_hex: &str) -> Result<String> {
        let cipher = Aes256Gcm::new_from_slice(&self.encryption_key.0)
            .map_err(|e| anyhow!("failed to init cipher: {e}"))?;

        // Generate random 96-bit nonce
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, key_hex.as_bytes())
            .map_err(|e| anyhow!("encryption failed: {e}"))?;

        // Prepend nonce to ciphertext
        let mut combined = Vec::with_capacity(12 + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);

        use base64::Engine;
        Ok(base64::engine::general_purpose::STANDARD.encode(&combined))
    }

    /// Decrypt a signing key that was encrypted with [`encrypt_signing_key`].
    ///
    /// The decrypted hex string is zeroized immediately after parsing into
    /// a `LocalWallet`.  The returned wallet should be used briefly and dropped.
    fn decrypt_signing_key(&self, encrypted_b64: &str) -> Result<LocalWallet> {
        use base64::Engine;
        let combined = base64::engine::general_purpose::STANDARD
            .decode(encrypted_b64)
            .context("invalid base64 in encrypted signing key")?;

        if combined.len() < 13 {
            return Err(anyhow!("encrypted key data too short"));
        }

        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);

        let cipher = Aes256Gcm::new_from_slice(&self.encryption_key.0)
            .map_err(|e| anyhow!("failed to init cipher: {e}"))?;

        let mut plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow!("decryption failed — wrong key or corrupt data: {e}"))?;

        let mut key_hex = String::from_utf8(plaintext.clone())
            .context("decrypted key is not valid UTF-8")?;

        // Zeroize the raw plaintext bytes immediately
        plaintext.zeroize();

        let wallet: LocalWallet = key_hex
            .parse()
            .context("decrypted key is not a valid private key")?;

        // Zeroize the hex string immediately after parsing
        key_hex.zeroize();

        Ok(wallet)
    }
}

// ──────────────────────── DB row type ────────────────────────────────

#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(dead_code)]
struct AgentWalletRow {
    id: Uuid,
    api_key_id: Uuid,
    agent_id: String,
    namespaced_id: String,
    signing_key_encrypted: String,
    eoa_address: String,
    smart_wallet_address: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

// ──────────── Standalone crypto helpers (reused by main.rs) ──────────

/// Encrypt a hex-encoded private key using AES-256-GCM.
///
/// `encryption_key` must be exactly 32 bytes.
/// Returns: `base64(nonce || ciphertext)`.
///
/// This is the same algorithm used by [`AgentWalletRegistry`] for agent
/// signing keys — extracted here so the platform bootstrap can encrypt the
/// auto-generated paymaster signer key without depending on the registry.
pub fn encrypt_key_hex(encryption_key: &[u8; 32], key_hex: &str) -> Result<String> {
    let cipher = Aes256Gcm::new_from_slice(encryption_key)
        .map_err(|e| anyhow!("failed to init cipher: {e}"))?;

    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, key_hex.as_bytes())
        .map_err(|e| anyhow!("encryption failed: {e}"))?;

    let mut combined = Vec::with_capacity(12 + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);

    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(&combined))
}

/// Decrypt a key that was encrypted with [`encrypt_key_hex`].
///
/// Returns the plaintext hex string.  **Caller must zeroize after use.**
pub fn decrypt_key_hex(encryption_key: &[u8; 32], encrypted_b64: &str) -> Result<String> {
    use base64::Engine;
    let combined = base64::engine::general_purpose::STANDARD
        .decode(encrypted_b64)
        .context("invalid base64 in encrypted key")?;

    if combined.len() < 13 {
        return Err(anyhow!("encrypted key data too short"));
    }

    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    let cipher = Aes256Gcm::new_from_slice(encryption_key)
        .map_err(|e| anyhow!("failed to init cipher: {e}"))?;

    let mut plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow!("decryption failed — wrong key or corrupt data: {e}"))?;

    let key_hex = String::from_utf8(plaintext.clone())
        .context("decrypted key is not valid UTF-8")?;

    plaintext.zeroize();

    Ok(key_hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encryption_round_trip() {
        let key = [0x42u8; 32];
        let plaintext = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";

        let encrypted = encrypt_key_hex(&key, plaintext).expect("encrypt");
        let decrypted = decrypt_key_hex(&key, &encrypted).expect("decrypt");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encryption_wrong_key_fails() {
        let key1 = [0x42u8; 32];
        let key2 = [0x43u8; 32];
        let plaintext = "secret_key_data";

        let encrypted = encrypt_key_hex(&key1, plaintext).expect("encrypt");
        assert!(decrypt_key_hex(&key2, &encrypted).is_err());
    }

    #[test]
    fn test_encryption_nonce_uniqueness() {
        let key = [0x42u8; 32];
        let encrypted_one = encrypt_key_hex(&key, "same").expect("encrypt one");
        let encrypted_two = encrypt_key_hex(&key, "same").expect("encrypt two");
        assert_ne!(encrypted_one, encrypted_two);
    }
}
