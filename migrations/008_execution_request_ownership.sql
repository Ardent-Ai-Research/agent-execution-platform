-- Scope execution requests to the API key that created them.
--
-- Historical rows can only be assigned safely when both the agent identifier
-- and smart-wallet address match a registered wallet. New V2 writes always
-- provide api_key_id at insertion time.

ALTER TABLE execution_requests
    ADD COLUMN IF NOT EXISTS api_key_id UUID REFERENCES api_keys(id);

UPDATE execution_requests AS request
SET api_key_id = wallet.api_key_id
FROM agent_wallets AS wallet
WHERE request.api_key_id IS NULL
  AND request.agent_id = wallet.agent_id
  AND lower(request.smart_wallet_address) = lower(wallet.smart_wallet_address);

CREATE INDEX IF NOT EXISTS idx_execution_requests_api_key
    ON execution_requests(api_key_id);
