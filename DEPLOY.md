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
./target/release/addresswise dev
```

## API key tables

The autocomplete endpoints `/search` and `/suggest` now require:

- `api_key` query parameter
- `Origin` or `Referer` header whose host matches a row in `api_key_domains`

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
