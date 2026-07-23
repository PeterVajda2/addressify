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

Deploy by pulling `master` in the source checkout, building the release binary,
and installing/copying it into the running bundle. Rebuild the Tantivy indexes
with the runtime environment when indexing schema or search behavior changes;
stop `addresswise` before replacing those indexes. Then restart the service.
Confirm `systemctl is-active addresswise` is `active` and
`curl --fail http://127.0.0.1:8080/health` succeeds before reporting
completion.
