-- Initial schema for the AI Agent Execution Platform

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

-- ── agents ──────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS agents (
    id              UUID PRIMARY KEY,
    wallet_address  TEXT NOT NULL UNIQUE,
    label           TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_agents_wallet ON agents(wallet_address);

-- ── execution_requests ──────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS execution_requests (
    id              UUID PRIMARY KEY,
    agent_wallet    TEXT NOT NULL,
    chain           TEXT NOT NULL,
    target_contract TEXT NOT NULL,
    calldata        TEXT NOT NULL,
    value           TEXT NOT NULL DEFAULT '0',
    strategy_id     TEXT,
    gas_estimate    BIGINT,
    cost_usd        DOUBLE PRECISION,
    status          TEXT NOT NULL DEFAULT 'pending',
    tx_hash         TEXT,
    error_message   TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_exec_req_agent  ON execution_requests(agent_wallet);
CREATE INDEX idx_exec_req_status ON execution_requests(status);

-- ── transactions ────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS transactions (
    id              UUID PRIMARY KEY,
    request_id      UUID NOT NULL REFERENCES execution_requests(id),
    chain           TEXT NOT NULL,
    tx_hash         TEXT NOT NULL,
    from_address    TEXT NOT NULL,
    to_address      TEXT NOT NULL,
    gas_used        BIGINT,
    status          TEXT NOT NULL DEFAULT 'pending',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_tx_request ON transactions(request_id);

-- ── payments ────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS payments (
    id              UUID PRIMARY KEY,
    request_id      UUID NOT NULL REFERENCES execution_requests(id),
    payer           TEXT NOT NULL,
    amount_usd      DOUBLE PRECISION NOT NULL,
    token           TEXT NOT NULL,
    payment_chain   TEXT NOT NULL,
    payment_tx_hash TEXT NOT NULL,
    verified        BOOLEAN NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_payments_request ON payments(request_id);
