-- Migration 003: Add optional webhook callback URL to execution requests.
--
-- When an agent provides a callback_url in the execute request, the platform
-- will POST the final status to that URL when the transaction reaches a
-- terminal state (confirmed, failed, reverted).

ALTER TABLE execution_requests
    ADD COLUMN IF NOT EXISTS callback_url TEXT;
