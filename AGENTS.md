# Addresswise project and operations

Addresswise is a Rust/Tantivy address-autocomplete API. It loads per-country
Tantivy indexes built from PostgreSQL, currently serves `CZ` and `SK`, and
exposes `/search` and `/suggest` (plus `/health`). The optional bare
`street_only` query flag returns distinct street names. API-key/domain
authorization and usage tracking are backed by PostgreSQL.

## Local commands

- `cargo test` verifies the Rust project.
- `scripts/check.sh` runs formatting, Clippy with warnings denied, tests, and a
  release build; run it before committing a code change.
- `cargo build --release` builds `target/release/addresswise`.
- The binary commands are `serve`, `build-indexes`, `migrate`, and `dev`.
- `scripts/public_benchmark.py` fails on HTTP errors. Supply its API key through
  `ADDRESSWISE_BENCHMARK_API_KEY`; use `--street-only --all-countries` to
  reproduce the API's cross-country autocomplete path. Each run writes
  revision-stamped JSON, including per-prefix percentiles, to
  `benchmark-results/` unless `--results-dir ''` disables it.
- `DEPLOY.md` documents runtime environment variables and API behavior.

## Production deployment

After completing and verifying a production-facing change, deploy it unless the
user explicitly asks not to. Commit the intended working-tree changes and push
`master` to `origin` first.

Production host: `peter@31.220.81.20`.

- The source checkout is `/home/peter/addresswise-src` (Git remote `origin`).
- The running bundle is `/home/peter/addresswise-deploy`; it is **not** a Git
  checkout.
- Systemd unit: `addresswise`, working directory
  `/home/peter/addresswise-deploy`, binary
  `/home/peter/addresswise-deploy/addresswise`.
- Runtime indexes live at `/home/peter/addresswise-deploy/data/indexes`.
- Runtime secrets, including `DATABASE_URL`, are in `/etc/addresswise.env` and
  must never be printed or committed.
- `ADMIN_API_KEY` enables and protects `/admin` and its API key-management
  endpoints; keep it only in `/etc/addresswise.env`. When absent, the public
  API remains available and admin requests return a configuration error.
- Production has no usable `cargo` in its non-interactive shell. Build release
  binaries locally; do not rely on remote compilation.

Use `scripts/deploy_production.sh` to build locally, upload a staged binary,
and cut over the runtime bundle. Pass `--rebuild-indexes` for indexing schema
or search-behavior changes. Street autocomplete materializes every normalized
street prefix as an index term, so changes to that behavior require this mode.
This mode derives every active two-letter country code from the production
database and persists it in a systemd drop-in, overriding the base service
unit's `COUNTRY_CODES` setting. Run it after importing addresses for a new
country.
It builds into a sibling index directory
while the service stays online, then swaps directories during the short service
restart. The source checkout is still kept on `master` for troubleshooting.
The service may take a brief systemd restart cycle after a binary cutover; wait
for the health endpoint instead of treating the first connection refusal as a
failed deployment.
Confirm `systemctl is-active addresswise` is `active` and
`curl --fail http://127.0.0.1:8080/health` succeeds before reporting
completion.

For OSM PBF address replacement imports, production has `osmium-tool` and
`jq` installed. Use `scripts/etl_osm_pbf.sh`; it streams tagged OSM addresses
through a FIFO and does not create a large GeoJSON intermediate file. The
script requires a locality by default, resolving standard `addr:*` and
`is_in:*` city/municipality tags; do not import locality-less records without
a spatial boundary/place enrichment pass.

### Active German OSM re-import (2026-08-04)

The previous DE OSM import was removed because it included duplicate
address-tagged OSM objects without a locality. The corrected importer skips
those objects (`OSM_REQUIRE_LOCALITY=true`), retaining the corresponding
objects that contain `addr:city`/another supported locality tag.

The source PBF is `/home/peter/germany-latest.osm.pbf` on production (SHA-256
`15f7a663ee428b8ab9e0cb30e6097ca30a97a06b4b102a0085208dcb41b9abfb`). The
import was launched in the background with log
`/home/peter/addresswise-deploy/imports/de_osm_reimport_20260804.log`:

```sh
sudo sh -c '
  set -euo pipefail
  set -a; . /etc/addresswise.env; set +a
  export OSM_COUNTRY_CODE=DE OSM_REQUIRE_LOCALITY=true
  export ETL_GEOJSON_BINARY=/home/peter/addresswise-src/etl_geojson
  exec runuser -u peter -- /home/peter/addresswise-src/scripts/etl_osm_pbf.sh \
    /home/peter/germany-latest.osm.pbf
'
```

Before treating the import as successful, wait for that process to exit and
the log to report zero skipped JSON/invalid rows. Then query a ten-row DE
sample and verify non-empty street, house number, postcode, locality, and
coordinates; confirm `Konrad-Adenauer-Allee 1-11` is `Bad Vilbel, 61118`.
Build replacement DE and `de_streets` indexes in a sibling directory, cut over
only those two directories with rollback protection, and confirm
`systemctl is-active addresswise` plus `/health`. Keep the live service online
while the import and index build run.

Completed 2026-08-04: the importer finished with `35,323,479` parsed and
`29,357,037` inserted rows, with `skipped_json=0` and `skipped_invalid=0`.
The replacement indexes built `19,575,650` active DE addresses and `369,715`
distinct DE streets in
`/home/peter/addresswise-deploy/data/indexes.de.next.20260804143840`. Only
`de` and `de_streets` were swapped into the live index root; the rollback
copies are `de.pre_de_reimport.20260804145634` and
`de_streets.pre_de_reimport.20260804145634`. An authenticated local API search
verified `Konrad-Adenauer-Allee 1-11, Bad Vilbel, 61118`; systemd and
`/health` were both healthy after the cutover.

## Keeping this file current

Whenever work reveals a new or corrected project, deployment, service, or
operational fact, update this `AGENTS.md` in the same workstream and commit it.
Do not leave deployment knowledge only in conversation history.
