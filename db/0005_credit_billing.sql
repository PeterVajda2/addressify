-- Existing request counters are preserved. Historical address requests cannot
-- be reliably split into autocomplete/validation and are not back-billed.
ALTER TABLE api_keys
    ADD COLUMN IF NOT EXISTS billing_plan TEXT NOT NULL DEFAULT 'free',
    ADD COLUMN IF NOT EXISTS custom_credit_limit BIGINT;

CREATE TABLE IF NOT EXISTS credit_metering_config (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
INSERT INTO credit_metering_config(singleton) VALUES (TRUE) ON CONFLICT DO NOTHING;

-- Shared public demo fair-use budget; independent from customer invoices.
CREATE TABLE IF NOT EXISTS api_demo_allowance (
    usage_date DATE PRIMARY KEY,
    credits BIGINT NOT NULL CHECK (credits >= 0 AND credits <= 10000)
);

-- Deliberately independent of the key FK: deleting a credential must not erase
-- financial history. API keys retain their existing deletion behavior.
CREATE TABLE IF NOT EXISTS api_credit_months (
    api_key_id BIGINT NOT NULL,
    period DATE NOT NULL,
    key_label TEXT,
    plan_id TEXT NOT NULL,
    credit_limit BIGINT NOT NULL,
    monthly_price_eur BIGINT,
    used_credits BIGINT NOT NULL DEFAULT 0 CHECK (used_credits >= 0),
    reserved_credits BIGINT NOT NULL DEFAULT 0 CHECK (reserved_credits >= 0),
    PRIMARY KEY (api_key_id, period)
);
CREATE TABLE IF NOT EXISTS api_credit_reservations (
    id BIGSERIAL PRIMARY KEY,
    api_key_id BIGINT NOT NULL,
    period DATE NOT NULL,
    operation TEXT NOT NULL CHECK (operation IN ('autocomplete', 'validation', 'company')),
    credits SMALLINT NOT NULL CHECK (credits > 0),
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'charged', 'refunded')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finalized_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_credit_reservations_pending
    ON api_credit_reservations (api_key_id, created_at) WHERE state = 'pending';
CREATE TABLE IF NOT EXISTS api_credit_usage_daily (
    api_key_id BIGINT NOT NULL,
    usage_date DATE NOT NULL,
    operation TEXT NOT NULL CHECK (operation IN ('autocomplete', 'validation', 'company')),
    request_count BIGINT NOT NULL CHECK (request_count >= 0),
    credits BIGINT NOT NULL CHECK (credits >= 0),
    PRIMARY KEY (api_key_id, usage_date, operation)
);

CREATE TABLE IF NOT EXISTS api_access_requests (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT NOT NULL,
    domain TEXT NOT NULL,
    plan TEXT NOT NULL CHECK (plan IN ('free', 'starter', 'growth', 'custom')),
    message TEXT NOT NULL DEFAULT '',
    api_key_id BIGINT,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'handled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    handled_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_access_requests_email ON api_access_requests (email, created_at);

CREATE UNIQUE INDEX IF NOT EXISTS uq_access_request_pending
    ON api_access_requests (email, domain, (coalesce(api_key_id,0))) WHERE state='pending';
