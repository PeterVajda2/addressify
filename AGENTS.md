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
- Embedded demo and admin pages: `static/index.html`, `static/admin.html`
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
AT BG CH CZ DE DK EE ES FI FR HR HU LT LU LV NL NO PT RS SI SK
```

GB and IE are intentionally disabled and their production data and indexes
were removed on 2026-08-22.

## Demo and API checks

The public demo is served from embedded `static/index.html`. When changing
country options, keep autocomplete and validation selectors consistent and
verify the live HTML after deployment.

Autocomplete endpoints require an API key and authorized origin; validation
requires an active API key. Never expose production keys in commands, logs,
screenshots, source, or this file.

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
