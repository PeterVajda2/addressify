# Addresswise project and operations

Addresswise is a Rust/Tantivy address-autocomplete API. It loads per-country
Tantivy indexes built from PostgreSQL, currently serves `CZ` and `SK`, and
exposes `/search` and `/suggest` (plus `/health`). The optional bare
`street_only` query flag returns distinct street names. API-key/domain
authorization and usage tracking are backed by PostgreSQL.

## Local commands

- `cargo test` verifies the Rust project.
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
- Production has no usable `cargo` in its non-interactive shell. Build release
  binaries locally; do not rely on remote compilation.

Use `scripts/deploy_production.sh` to build locally, upload a staged binary,
and cut over the runtime bundle. Pass `--rebuild-indexes` for indexing schema
or search-behavior changes. That mode builds into a sibling index directory
while the service stays online, then swaps directories during the short service
restart. The source checkout is still kept on `master` for troubleshooting.
The service may take a brief systemd restart cycle after a binary cutover; wait
for the health endpoint instead of treating the first connection refusal as a
failed deployment.
Confirm `systemctl is-active addresswise` is `active` and
`curl --fail http://127.0.0.1:8080/health` succeeds before reporting
completion.

## Keeping this file current

Whenever work reveals a new or corrected project, deployment, service, or
operational fact, update this `AGENTS.md` in the same workstream and commit it.
Do not leave deployment knowledge only in conversation history.
