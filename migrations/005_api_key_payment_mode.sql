-- Migration 005: API key payment mode
--
-- Adds per-API-key billing policy:
--   - manual: caller must provide X-Payment-Proof (existing behavior)
--   - auto: reserved for platform-managed auto-debit flow
--   - sponsored: payment requirement is skipped

ALTER TABLE api_keys
    ADD COLUMN IF NOT EXISTS payment_mode TEXT NOT NULL DEFAULT 'manual';

-- Normalize any unexpected historical values to manual.
UPDATE api_keys
SET payment_mode = 'manual'
WHERE payment_mode NOT IN ('manual', 'auto', 'sponsored');
