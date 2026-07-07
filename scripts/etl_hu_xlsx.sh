#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DB_URL_DEFAULT="postgres://address:address@localhost:5432/address_wise"
DATABASE_URL="${DATABASE_URL:-$DB_URL_DEFAULT}"

cd "$ROOT_DIR"

cargo run --release --bin etl_hu_xlsx -- \
  --input "$ROOT_DIR/address_data/HU_data.xlsx" \
  --database-url "$DATABASE_URL" \
  "$@"
