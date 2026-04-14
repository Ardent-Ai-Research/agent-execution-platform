-- Migration 002: Agent wallet registry + per-customer API keys + ERC-4337 support
--
-- Changes:
-- 1. api_keys: per-customer API key management
-- 2. agent_wallets: platform-managed EOA + counterfactual smart wallet per agent
-- 3. Alter execution_requests to reference agent_id instead of raw wallet
-- 4. Add UNIQUE constraint on payments.payment_tx_hash (if missing)

-- ── api_keys ────────────────────────────────────────────────────────
-- Each customer (company / developer) gets one or more API keys.
-- The key itself is never stored — only its SHA-256 hash.
CREATE TABLE IF NOT EXISTS api_keys (
    id          UUID PRIMARY KEY,
    key_hash    TEXT NOT NULL UNIQUE,          -- SHA-256 of the raw API key
    label       TEXT,                          -- human-readable name ("production", "staging")
    is_active   BOOLEAN NOT NULL DEFAULT TRUE, -- soft-disable without deleting
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_api_keys_hash ON api_keys(key_hash);

-- ── agent_wallets ───────────────────────────────────────────────────
-- One row per unique (api_key_id, agent_id) pair.
-- The namespaced_id = "{api_key_id}::{agent_id}" guarantees cross-customer isolation.
-- signing_key_encrypted holds the AES-256-GCM encrypted hex private key of the EOA.
-- eoa_address is the EOA public address (derived from the signing key).
-- smart_wallet_address is the counterfactual ERC-4337 SimpleAccount address.
CREATE TABLE IF NOT EXISTS agent_wallets (
    id                      UUID PRIMARY KEY,
    api_key_id              UUID NOT NULL REFERENCES api_keys(id),
    agent_id                TEXT NOT NULL,               -- agent-supplied identifier
    namespaced_id           TEXT NOT NULL UNIQUE,         -- "{api_key_id}::{agent_id}"
    signing_key_encrypted   TEXT NOT NULL,                -- AES-256-GCM encrypted private key
    eoa_address             TEXT NOT NULL,                -- 0x-prefixed EOA address
    smart_wallet_address    TEXT NOT NULL,                -- counterfactual SimpleAccount address
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_agent_wallets_namespaced ON agent_wallets(namespaced_id);
CREATE INDEX IF NOT EXISTS idx_agent_wallets_api_key    ON agent_wallets(api_key_id);
CREATE INDEX IF NOT EXISTS idx_agent_wallets_smart      ON agent_wallets(smart_wallet_address);

-- ── execution_requests: add agent_id column ─────────────────────────
-- Keep agent_wallet for backward compat (old rows), add agent_id + smart_wallet_address
ALTER TABLE execution_requests
    ADD COLUMN IF NOT EXISTS agent_id             TEXT,
    ADD COLUMN IF NOT EXISTS smart_wallet_address  TEXT;

-- ── payments: ensure UNIQUE on payment_tx_hash ──────────────────────
-- The ON CONFLICT logic in insert_payment depends on this constraint.
-- Using DO NOTHING on the CREATE so it's idempotent.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'payments_payment_tx_hash_key'
    ) THEN
        ALTER TABLE payments ADD CONSTRAINT payments_payment_tx_hash_key UNIQUE (payment_tx_hash);
    END IF;
END
$$;
