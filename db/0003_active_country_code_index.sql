-- Supports the deployment helper's active-country discovery query.
CREATE INDEX IF NOT EXISTS idx_addresses_active_country_code
    ON addresses ((upper(trim(country_code))))
    WHERE is_active AND length(trim(country_code)) > 0;
