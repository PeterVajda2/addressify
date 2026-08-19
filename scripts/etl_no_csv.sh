#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DB_URL_DEFAULT="postgres://address:address@localhost:5432/address_wise"
DATABASE_URL="${DATABASE_URL:-$DB_URL_DEFAULT}"

cd "$ROOT_DIR"

cargo run --release --bin etl_no_csv -- \
  --input "$ROOT_DIR/norway_data/matrikkelenAdresse.csv" \
  --database-url "$DATABASE_URL" \
  "$@"
