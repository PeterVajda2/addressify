# Deploying addresswise

## Runtime model

The binary now supports three modes:

- `addresswise build-indexes`
- `addresswise serve`
- `addresswise dev`
- `addresswise migrate`

Recommended production flow:

1. Build the indexes once with `build-indexes`.
2. Start the API with `serve`.

`serve` only loads Tantivy indexes from disk. It does not rebuild them and it does not shell into containers.
`serve` now requires PostgreSQL access because API-key auth and usage tracking are enforced on each request.

## Required environment

- `COUNTRY_CODES`
  Example: `CZ,SK`
- `INDEX_DIR`
  Example: `/opt/addresswise/data/indexes`
- `DATABASE_URL`
  Example: `postgres://address:password@127.0.0.1:5432/address_wise`

Optional:

- `HOST`
  Default: `127.0.0.1`
- `PORT`
  Default: `8080`
- `PSQL_BIN`
  Default: `psql`
- `INDEX_LIMIT`
  Limits rows during index builds for testing.

Production index rebuilds (`scripts/deploy_production.sh --rebuild-indexes`) derive
`COUNTRY_CODES` from every active, valid two-letter country code in the database.
They also install a systemd drop-in with that same list, so `serve` loads all of
the rebuilt country indexes. Run this deployment mode again after importing a new
country.

## Build and run

```bash
cargo build --release
```

Build indexes:

```bash
HOST=0.0.0.0 \
PORT=8080 \
COUNTRY_CODES=CZ,SK \
INDEX_DIR=/opt/addresswise/data/indexes \
DATABASE_URL=postgres://address:password@127.0.0.1:5432/address_wise \
./target/release/addresswise build-indexes
```

Apply schema migrations:

```bash
DATABASE_URL=postgres://address:password@127.0.0.1:5432/address_wise \
./target/release/addresswise migrate
```

Serve from existing indexes:

```bash
HOST=0.0.0.0 \
PORT=8080 \
COUNTRY_CODES=CZ,SK \
INDEX_DIR=/opt/addresswise/data/indexes \
DATABASE_URL=postgres://address:password@127.0.0.1:5432/address_wise \
./target/release/addresswise serve
```

Local all-in-one development mode:

```bash
COUNTRY_CODES=CZ,SK \
DATABASE_URL=postgres://address:password@127.0.0.1:5432/address_wise \
ADMIN_API_KEY=replace-with-a-long-private-admin-key \
./target/release/addresswise dev
```

## API key tables

The autocomplete endpoints `/search` and `/suggest` now require:

- `api_key` query parameter
- `Origin` or `Referer` header whose host matches a row in `api_key_domains`

`POST /validate?api_key=...` accepts the same JSON with `street`,
`house_number`, `postal_code`, `city`, and a two-letter `country`, but only
requires a valid active API key and does not enforce `Origin`/`Referer` domain
matching. It returns `valid`, a `confidence_ratio` from `0.0` to `1.0`, and
the best canonical `corrected_address` when the supplied address differs. When
the best match is below `0.90`, it also returns up to five ranked
`suggestions`.

Seed one key and one allowed domain:

```sql
insert into api_keys (api_key, label)
values ('replace-with-public-key', 'addresswise.eu browser key')
on conflict (api_key) do nothing;

insert into api_key_domains (api_key_id, domain)
select id, 'addresswise.eu'
from api_keys
where api_key = 'replace-with-public-key'
on conflict (api_key_id, domain) do nothing;
```

Usage is tracked in:

- `api_keys.total_requests`
- `api_key_usage_daily`

## API key administration

`/admin` is a browser interface for managing API keys, labels, allowed domains,
activation status, and usage data. Set `ADMIN_API_KEY` in the runtime
environment to enable it. Without that setting, the public API remains online
but administrative requests return a configuration error. The page stores this
key only in the browser's session storage and sends it in the `X-Admin-Key`
header to the administrative API. Generate a long, private value and keep it
in `/etc/addresswise.env`; never use a public autocomplete API key for this
setting.

## HTTP/3

The server binds both TCP and QUIC on the configured `HOST:PORT`.

If you expose it publicly, open both:

- `PORT/tcp`
- `PORT/udp`

If you put it behind a reverse proxy, make sure the proxy supports HTTP/3 passthrough or terminate HTTP/3 there and forward internally as needed.

## systemd

An example unit is included at:

- `deploy/addresswise.service.example`

The example rebuilds indexes before each start. For large datasets, a better long-term pattern is:

1. run `build-indexes` separately on deploy
2. restart `serve`

That keeps restarts fast and predictable.
