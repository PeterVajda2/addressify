ALTER TABLE api_credit_months ALTER COLUMN credit_limit DROP NOT NULL;

ALTER TABLE customer_accounts DROP CONSTRAINT IF EXISTS customer_accounts_requested_plan_check;
ALTER TABLE customer_accounts ADD CONSTRAINT customer_accounts_requested_plan_check
    CHECK (requested_plan IN ('free','starter','growth','unlimited','custom'));

ALTER TABLE api_access_requests DROP CONSTRAINT IF EXISTS api_access_requests_plan_check;
ALTER TABLE api_access_requests ADD CONSTRAINT api_access_requests_plan_check
    CHECK (plan IN ('free','starter','growth','unlimited','custom'));

-- A request for an individual quote is not a selection of the new paid plan.
UPDATE customer_accounts a SET requested_plan = coalesce(
    (SELECT k.billing_plan FROM api_keys k WHERE k.id = a.api_key_id), 'free'
) WHERE a.requested_plan = 'custom';
