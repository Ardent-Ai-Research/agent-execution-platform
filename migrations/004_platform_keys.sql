-- Migration 004: Platform-managed signing keys
--
-- Stores auto-generated keys used by the platform itself (not per-agent).
-- Currently: the paymaster signer key that authorizes gas sponsorship.
--
-- Keys are encrypted at rest with the same WALLET_ENCRYPTION_KEY used for
-- agent wallets (AES-256-GCM, base64-encoded nonce || ciphertext).

CREATE TABLE IF NOT EXISTS platform_keys (
    id              UUID PRIMARY KEY,
    purpose         TEXT NOT NULL UNIQUE,      -- e.g. 'paymaster_signer'
    encrypted_key   TEXT NOT NULL,             -- AES-256-GCM encrypted hex private key
    address         TEXT NOT NULL,             -- derived 0x-prefixed Ethereum address
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
