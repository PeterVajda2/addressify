#!/usr/bin/env bash
set -euo pipefail

# Build locally, upload a new binary, then make the production cutover. Building
# indexes into a sibling directory keeps the live service available throughout
# the expensive part of an index-changing deployment.
remote_host="${ADDRESSWISE_HOST:-peter@31.220.81.20}"
source_dir="${ADDRESSWISE_SOURCE_DIR:-/home/peter/addresswise-src}"
runtime_dir="${ADDRESSWISE_RUNTIME_DIR:-/home/peter/addresswise-deploy}"
rebuild_indexes=false

usage() {
    echo "Usage: $0 [--rebuild-indexes]" >&2
}

case "${1:-}" in
    "") ;;
    --rebuild-indexes) rebuild_indexes=true ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
esac

cargo build --release

next_binary="$runtime_dir/addresswise.next"
scp target/release/addresswise "$remote_host:$next_binary"

ssh "$remote_host" bash -s -- "$runtime_dir" "$source_dir" "$rebuild_indexes" <<'REMOTE_SCRIPT'
set -euo pipefail

runtime_dir="$1"
source_dir="$2"
rebuild_indexes="$3"
next_binary="$runtime_dir/addresswise.next"
git -C "$source_dir" pull --ff-only origin master

test -x "$next_binary"

if [[ "$rebuild_indexes" == true ]]; then
    next_indexes="$runtime_dir/data/indexes.next.$$"
    sudo sh -c '
        set -a
        . /etc/addresswise.env
        set +a
        export COUNTRY_CODES=CZ,SK INDEX_DIR="$1"
        exec runuser -u peter -- "$2" build-indexes
    ' sh "$next_indexes" "$next_binary"
fi

timestamp="$(date +%Y%m%d%H%M%S)"
previous_binary="$runtime_dir/addresswise.$timestamp"
previous_indexes=""
cutover_started=false
rollback() {
    status="$?"
    if [[ "$status" -ne 0 && "$cutover_started" == true ]]; then
        mv "$previous_binary" "$runtime_dir/addresswise" || true
        if [[ "$rebuild_indexes" == true && -n "$previous_indexes" ]]; then
            mv "$runtime_dir/data/indexes" "$runtime_dir/data/indexes.failed.$timestamp" || true
            mv "$previous_indexes" "$runtime_dir/data/indexes" || true
        fi
        sudo systemctl start addresswise || true
    fi
    exit "$status"
}
trap rollback EXIT

sudo systemctl stop addresswise
cutover_started=true
cp "$runtime_dir/addresswise" "$previous_binary"
mv "$next_binary" "$runtime_dir/addresswise"

if [[ "$rebuild_indexes" == true ]]; then
    previous_indexes="$runtime_dir/data/indexes.$timestamp"
    mv "$runtime_dir/data/indexes" "$previous_indexes"
    mv "$next_indexes" "$runtime_dir/data/indexes"
fi

sudo systemctl start addresswise
sudo systemctl is-active --quiet addresswise
curl --fail --silent --show-error http://127.0.0.1:8080/health
cutover_started=false
REMOTE_SCRIPT
