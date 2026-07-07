#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DB_URL_DEFAULT="postgres://address:address@localhost:5432/address_wise"
DATABASE_URL="${DATABASE_URL:-$DB_URL_DEFAULT}"

cd "$ROOT_DIR"

cargo run --release --bin etl_geojson -- \
  --input-dir "$ROOT_DIR/address_data" \
  --database-url "$DATABASE_URL" \
  "$@"
