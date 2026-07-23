CREATE TABLE IF NOT EXISTS api_keys (
    id BIGSERIAL PRIMARY KEY,
    api_key TEXT NOT NULL UNIQUE,
    label TEXT,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    total_requests BIGINT NOT NULL DEFAULT 0,
    last_used_at TIMESTAMPTZ,
    last_used_domain TEXT,
    last_used_ip TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS api_key_domains (
    id BIGSERIAL PRIMARY KEY,
    api_key_id BIGINT NOT NULL REFERENCES api_keys(id) ON DELETE CASCADE,
    domain TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT api_key_domains_domain_lowercase CHECK (domain = LOWER(domain))
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_api_key_domains_key_domain
    ON api_key_domains (api_key_id, domain);

CREATE INDEX IF NOT EXISTS idx_api_key_domains_domain
    ON api_key_domains (domain);

CREATE TABLE IF NOT EXISTS api_key_usage_daily (
    api_key_id BIGINT NOT NULL REFERENCES api_keys(id) ON DELETE CASCADE,
    usage_date DATE NOT NULL,
    request_domain TEXT NOT NULL,
    request_count BIGINT NOT NULL DEFAULT 0,
    last_request_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (api_key_id, usage_date, request_domain),
    CONSTRAINT api_key_usage_daily_domain_lowercase CHECK (request_domain = LOWER(request_domain))
);

CREATE INDEX IF NOT EXISTS idx_api_key_usage_daily_usage_date
    ON api_key_usage_daily (usage_date DESC, api_key_id);
