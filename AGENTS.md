# Addresswise agent guide

This is the repository-level operating guide for coding agents. Keep it short,
concrete, and current. User and system instructions take precedence. For work
in a deeper directory, check for a more-specific `AGENTS.md` or
`AGENTS.override.md` before editing files.

## Project at a glance

Addresswise is a Rust HTTP API for address autocomplete and validation.

- HTTP implementation: `src/http.rs`, `src/main.rs`
- Address model and display formatting: `src/models.rs`
- Search and ranking: `src/search.rs`
- Tantivy/PostgreSQL indexing: `src/indexing.rs`
- Country-specific rules: `src/address_rules.rs`, `src/normalize.rs`
- Database migrations: `db/`
- Shared landing page, usage dashboard and integration guide: `static/platform.html`, `static/account.html`, `static/docs.html`
- Address demo and shared key administration: `static/index.html`, `static/admin.html`
- Companywise gateway and shared usage API: `src/platform.rs`
- ETL binaries: `src/bin/etl_*.rs`
- ETL documentation: `ETL.md`
- Deployment documentation: `DEPLOY.md`

The application has one address and one street index per active country. The
runtime index root contains `<cc>/`, `<cc>_streets/`, and
`<cc>_postal_codes/` for each country. The
database is the source of truth for address records; Tantivy is built from
active database rows.

## Local development

Use the repository root as the working directory.

```sh
cargo test
scripts/check.sh
cargo build --release
```

`scripts/check.sh` is the normal pre-commit verification: formatting, Clippy
with warnings denied, tests, and a release build. Run narrower tests while
iterating, but run the full check for a completed Rust change.

The main binary modes are `serve`, `build-indexes`, `migrate`, and `dev`.
ETL binaries are `etl_geojson`, `etl_be_csv`, `etl_hu_xlsx`, and `etl_no_csv`.
Shell entry points are in `scripts/`.

Runtime configuration uses `DATABASE_URL`, `COUNTRY_CODES`, and `INDEX_DIR`.
`HOST`, `PORT`, `INDEX_LIMIT`, and `TOMTOM_API_KEY` are optional. Keep
`ADMIN_API_KEY`, `TOMTOM_API_KEY`, and all database credentials outside the
repository.

## Code and data conventions

- Preserve Unicode address characters in canonical values. Search aliases may
  use folded forms, but do not replace a country’s distinct letters in stored
  or displayed address data.
- Keep country-specific normalization exceptions in the address-rule layer,
  not scattered through HTTP or indexing code.
- Treat `source_hash` as source-object identity. PostgreSQL enforces
  uniqueness on `(country_code, source_hash)`; do not change that key without
  an explicit data-migration plan.
- Preserve the distinction between address records and street suggestions.
  Changes to street materialization or ranking affect every rebuilt country.
- The demo HTML is compiled into the Rust binary with `include_str!`; a static
  page change requires a release build and binary deployment.
- Avoid unrelated formatting, dependency, schema, or production changes.

## ETL safety

Before importing a large source:

1. Confirm the original source file and checksum.
2. Count eligible records from the original source, not an interrupted
   intermediate.
3. Check disk space, available memory, and database capacity.
4. Confirm the country code and source dataset name.
5. Use a streaming path; do not materialize millions of address objects in a
   single in-memory structure.

For OSM PBF imports use `scripts/etl_osm_pbf.sh`. It streams GeoJSON through a
FIFO and requires locality resolution by default (`OSM_REQUIRE_LOCALITY=true`).
Do not import locality-less records without a verified enrichment step.

Never use an interrupted or partial GeoJSON/PBF intermediate as the source of
truth. If counts disagree, compare source-object IDs, unique source hashes,
SQL active rows, and Tantivy counts as separate metrics.

## Indexing rules

Indexes are independent per country.

- A database change limited to one country normally requires only that
  country’s address and street indexes to be rebuilt.
- Do not delete the entire index root for a country-specific change.
- A full rebuild is required only when all index directories were deliberately
  removed or a global indexing behavior change requires it.
- Build into a sibling directory and atomically swap only affected country
  directories whenever possible.
- Verify `<cc>/`, `<cc>_streets/`, and `<cc>_postal_codes/` after a country rebuild.

## Production deployment

Production is:

- Host: `peter@31.220.81.20`
- Source checkout: `/home/peter/addresswise-src`
- Runtime bundle: `/home/peter/addresswise-deploy`
- Binary: `/home/peter/addresswise-deploy/addresswise`
- Indexes: `/home/peter/addresswise-deploy/data/indexes`
- Service: `addresswise`
- Health endpoint: `http://127.0.0.1:8080/health`
- Secrets: `/etc/addresswise.env` on the host; never print them

Build release binaries locally; production is not a build machine. The normal
deployment helper is `scripts/deploy_production.sh`.

- No arguments: binary-only deployment; use this for static/API changes that
  do not alter indexes.
- `--rebuild-indexes`: rebuilds every active country; use only for global
  indexing/schema changes or after all indexes were intentionally removed.
- `--add-countries CC,...`: preserves other indexes and rebuilds only the
  requested active country directories.

For a country-specific data change, do not run a full rebuild. Confirm the
requested country is active in PostgreSQL, rebuild its two indexes, and verify
the service. If the helper fails before cutover, verify that production was
unchanged; local SSH configuration may require an explicit known-good config.

Before destructive production actions, identify exact targets and confirm the
user requested them. Never recursively delete a broad directory. For country
removal, stop the service, delete only that country’s SQL rows and index/source
paths, remove it from the service country configuration, then restart and
health-check.

After every production deployment verify:

```sh
systemctl is-active addresswise
curl --fail http://127.0.0.1:8080/health
df -h /
free -h
```

Do not report success based on the first transient connection refusal during a
restart; wait for the final health response. The current active production
country set is:

```text
AT BE BG CH CZ DE DK EE ES FI FR HR HU IT LT LU LV NL NO PL PT RS SI SK
```

GB and IE are intentionally disabled and their production data and indexes
were removed on 2026-08-22.

## Demo and API checks

The root URL serves the shared two-product landing page. The address demo is
served at `/addresswise` from embedded `static/index.html`. When changing
country options, keep autocomplete and validation selectors consistent and
verify the live HTML after deployment.

Address entry is street-first only. The full tool has no flow selector or
editable postal-code lookup field: choose a country, select/type a street,
then enter the house number and choose the full address. City and postal code
are read-only derived fields. Keep the postal indexes and existing
`postal_only`/`postal_code` API parameters for compatibility; the UI does not
use them. Removing a UI flow does not require rebuilding country indexes.

The homepage embeds compact live demos: address suggestions call `/search`
and a selected address calls `/validate`; company lookups use the existing
Companywise gateway with `minimal=true`. The landing handler injects the
existing public demo key and current index country codes at runtime. Do not
hardcode keys or execute paid demo requests automatically on page load.

Autocomplete endpoints require an API key and authorized origin; validation
requires an active API key. Never expose production keys in commands, logs,
screenshots, source, or this file.

## Public website design

- The homepage, account page and developer guide share a white/slate palette,
  blue address accent and teal company accent, responsive layouts and focus states.
- Keep the homepage order explicit: product explanation and real demos, simple
  integration examples, package pricing, credit examples/calculator, then FAQs.
- Code examples offer cURL/JavaScript and copy controls. Match the actual API:
  autocomplete requires a query key, allowed origin and bare `street_only` flag;
  Companywise supports Bearer auth. Label abbreviated JSON as examples.
- Free access is issued by the team. Do not imply instant key delivery or online
  payment collection until those flows exist. Keep quota and billing copy exact.

## Shared Companywise platform

- Addresswise owns API keys for both products. Preserve existing address API
  handlers, authentication rules, responses and counters.
- Nginx routes `/companywise/api/` through Addresswise on port 8080. The gateway
  forwards authenticated subject lookups to the loopback Companywise service on
  port 3001. Other Companywise pages, docs and health still go directly to 3001.
- Companywise accepts a shared key in `api_key` or a Bearer header. The existing
  same-origin Companywise browser demo uses the public demo key with its allowed
  domain checks. Machine clients must supply a key.
- Successful Companywise lookups are persisted in `companywise_usage_daily`;
  they do not change legacy `api_keys.total_requests` or address usage rows.
- `/account/api/usage` returns only the supplied active key's usage.
  `/admin/api/usage` requires the existing admin secret. Both accept optional
  `month=YYYY-MM` and default to the database's current calendar month.
- The new ledger is additive migration `db/0004_companywise_usage.sql`.
  Deploy it before enabling the Companywise gateway.
- The initial unified deployment and source snapshot are at
  `/home/peter/addresswise-platform-releases/20261118-unified` on production.
  `deploy/rollout-platform.sh` verifies a candidate on port 8081 against live
  address responses before switching the binary and nginx Companywise snippet.
  It rolls back the binary/snippet on failure and retains the additive ledger.
- For subsequent binary-only releases, `deploy/rollout-binary.sh` performs the
  candidate/legacy-response checks without changing database schema, nginx or
  indexes. Use a new release directory so previous backups are never replaced.
- `scripts/verify_platform.py` uses private environment credentials and only
  creates/modifies/deletes its own temporary keys. Never log its credential env.
- Shared credits, package pricing, hard monthly limits and CSV export are
  configured. Online card collection and automatic invoicing are not configured.

## Credit billing

- `src/billing.rs` owns costs (autocomplete 1, validation 3, company 5), package
  limits (Free 1,000; Starter 100,000 for EUR 9; Growth 1,000,000 for EUR 19),
  UTC periods, reservations, reports, enquiries and admin activation.
- Limits are chosen packages, never automatic upgrades or overages. Custom
  limits require admin approval. Paid access is manually activated after the
  customer/payment arrangement is confirmed; no card is charged by this code.
- New and existing keys default to Free when migration 0005 is first applied.
  The existing public demo key is separate from customer credits, with a shared
  fair-use budget of 10,000 attempted credits/day (UTC); exhaustion returns
  `demo_limit_exceeded`. It cannot provide unlimited unmetered production use.
- Reserve atomically under the key row lock before work. Only 2xx responses
  charge credits; errors refund. Completed empty/needs-review/cached/repeated
  requests still count. Abandoned pending reservations expire after five minutes.
- Exhaustion returns HTTP 429 `credit_limit_exceeded`, including requested,
  used, reserved, remaining, limit and UTC reset, plus `Retry-After`. Lower-cost
  requests can still run if they fit. Do not change successful API payloads.
- `api_credit_months`, `api_credit_reservations`, `api_credit_usage_daily` are
  the authoritative financial ledger. Legacy address/company counters remain
  raw activity only and are never retrospectively converted to credits.
- Financial history survives credential deletion. Snapshot package prices per
  month; changing a package does not reset usage. Reject downgrades below used
  plus pending credits. An unrecorded historical month has no package charge.
- `/account/api/credits` and `/admin/api/credits` accept `month=YYYY-MM`.
  Admin `/admin/api/billing/plan` is the only package activation endpoint.
  Public `/account/api/access-requests` stores requests in the admin inbox;
  submission does not activate paid access or send external messages.
- Deploy new credit columns/tables using additive migration 0005 before the
  billing binary. `deploy/rollout-credits.sh` applies it transactionally, tests a
  candidate, checks costs/concurrent limits over HTTP, then switches with rollback.
  No index, source-data or nginx changes are required.
- Run `billing::tests::postgres_credit_accounting` explicitly against a fresh
  isolated `CREDIT_TEST_DATABASE_URL`. It verifies reservations, refunds,
  concurrency, expiry, period rollover, plan changes and archived history.
- `scripts/verify_credits.py` uses only temporary keys it creates. Rollout
  supplies private environment credentials; it cleans up only its own test
  financial records, never real customer history.

## Definition of done

For a code change:

- Make the smallest scoped change.
- Run relevant tests, then `scripts/check.sh` for a completed Rust change.
- Inspect the diff and confirm no secrets or generated artifacts were added.
- Update this file only when a durable project or operational fact changed.

For a production change:

- Verify exact deployed binary, config, and data paths.
- Monitor disk and memory for large imports or index builds.
- Verify service state and `/health` after cutover.
- Report what changed, what was verified, and any known limitation.
