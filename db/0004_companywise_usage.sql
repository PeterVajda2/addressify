-- Companywise shares the existing API keys. Addresswise's counters and
-- authentication remain unchanged; the unified usage API reads both ledgers.
CREATE TABLE IF NOT EXISTS companywise_usage_daily (
    api_key_id BIGINT NOT NULL REFERENCES api_keys(id) ON DELETE CASCADE,
    usage_date DATE NOT NULL DEFAULT CURRENT_DATE,
    request_count BIGINT NOT NULL DEFAULT 0 CHECK (request_count >= 0),
    last_request_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (api_key_id, usage_date)
);
CREATE INDEX IF NOT EXISTS idx_companywise_usage_daily_date
    ON companywise_usage_daily (usage_date, api_key_id);
