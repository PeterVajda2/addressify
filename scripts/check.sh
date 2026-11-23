#!/usr/bin/env bash
set -euo pipefail

python3 scripts/build_locales.py
python3 scripts/check_locales.py
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
