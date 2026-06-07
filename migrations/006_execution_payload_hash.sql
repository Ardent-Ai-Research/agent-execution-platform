-- Migration 006: Persist a canonical execution payload hash for quote locking.
--
-- Batch requests store their actual calls in `batch_calls`, while legacy columns
-- `target_contract` and `calldata` are empty.  A hash lets payment quote
-- re-submit validate the exact execution payload, including batch calls.

ALTER TABLE execution_requests
    ADD COLUMN IF NOT EXISTS payload_hash TEXT;

CREATE INDEX IF NOT EXISTS idx_execution_requests_payload_hash
    ON execution_requests(payload_hash);
