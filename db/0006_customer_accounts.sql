-- Customer identities are separate from existing API credentials. No key is
-- claimed or reassigned based on an email address, domain or a public demo key.
CREATE TABLE IF NOT EXISTS customer_accounts (
    id BIGSERIAL PRIMARY KEY,
    email TEXT NOT NULL UNIQUE CHECK (email=lower(email)),
    name TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    domain TEXT NOT NULL CHECK (domain=lower(domain)),
    api_key_id BIGINT UNIQUE REFERENCES api_keys(id) ON DELETE SET NULL,
    requested_plan TEXT NOT NULL DEFAULT 'free' CHECK (requested_plan IN ('free','starter','growth','custom')),
    cancel_at TIMESTAMPTZ,
    terms_version TEXT NOT NULL,
    terms_language TEXT NOT NULL CHECK (terms_language IN ('cs','sk','en')),
    terms_accepted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS customer_sessions (
    token_hash TEXT PRIMARY KEY,
    customer_id BIGINT NOT NULL REFERENCES customer_accounts(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now()+interval '30 days'
);
CREATE INDEX IF NOT EXISTS idx_customer_sessions_owner ON customer_sessions(customer_id);
CREATE INDEX IF NOT EXISTS idx_customer_sessions_expiry ON customer_sessions(expires_at);
CREATE TABLE IF NOT EXISTS customer_auth_limits (
    bucket TEXT PRIMARY KEY,
    attempts INTEGER NOT NULL DEFAULT 1,
    resets_at TIMESTAMPTZ NOT NULL
);
