# Deploying addresswise

## Runtime model

The binary now supports three modes:

- `addresswise build-indexes`
- `addresswise serve`
- `addresswise dev`

Recommended production flow:

1. Build the indexes once with `build-indexes`.
2. Start the API with `serve`.

`serve` only loads Tantivy indexes from disk. It does not rebuild them and it does not shell into containers.

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

Serve from existing indexes:

```bash
HOST=0.0.0.0 \
PORT=8080 \
COUNTRY_CODES=CZ,SK \
INDEX_DIR=/opt/addresswise/data/indexes \
./target/release/addresswise serve
```

Local all-in-one development mode:

```bash
COUNTRY_CODES=CZ,SK \
DATABASE_URL=postgres://address:password@127.0.0.1:5432/address_wise \
./target/release/addresswise dev
```

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
