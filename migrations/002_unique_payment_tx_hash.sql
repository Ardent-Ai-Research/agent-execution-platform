-- Enforce uniqueness on payment_tx_hash to prevent replay attacks at the DB level.
-- This is the last line of defense: even if the application-level check has a
-- TOCTOU race, the DB will reject duplicate payment transaction hashes.

ALTER TABLE payments
    ADD CONSTRAINT uq_payments_tx_hash UNIQUE (payment_tx_hash);
