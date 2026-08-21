-- Remove the retired x402 execution-fee model while preserving safe upgrades
-- from databases that already ran migrations 001-006.

UPDATE execution_requests
SET status = 'failed',
    error_message = COALESCE(
        error_message,
        'Execution request expired when testnet execution fees were retired.'
    ),
    updated_at = now()
WHERE status IN ('payment_required', 'payment_verified');

DROP TABLE IF EXISTS payments;

ALTER TABLE api_keys
    DROP COLUMN IF EXISTS payment_mode;

DROP INDEX IF EXISTS idx_execution_requests_payload_hash;

ALTER TABLE execution_requests
    DROP COLUMN IF EXISTS cost_usd,
    DROP COLUMN IF EXISTS payload_hash;
