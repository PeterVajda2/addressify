ALTER TABLE customer_accounts
    ADD COLUMN IF NOT EXISTS email_confirmation_required BOOLEAN NOT NULL DEFAULT FALSE;

CREATE TABLE IF NOT EXISTS customer_email_actions (
    token_hash TEXT PRIMARY KEY,
    customer_id BIGINT NOT NULL REFERENCES customer_accounts(id) ON DELETE CASCADE,
    purpose TEXT NOT NULL CHECK (purpose IN ('confirm', 'reset')),
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_customer_email_actions_owner
    ON customer_email_actions(customer_id, purpose);
CREATE INDEX IF NOT EXISTS idx_customer_email_actions_expiry
    ON customer_email_actions(expires_at);
