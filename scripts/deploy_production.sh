#!/usr/bin/env bash
set -euo pipefail

# Build locally, upload a new binary, then make the production cutover. Building
# indexes into a sibling directory keeps the live service available throughout
# the expensive part of an index-changing deployment.
remote_host="${ADDRESSWISE_HOST:-peter@31.220.81.20}"
source_dir="${ADDRESSWISE_SOURCE_DIR:-/home/peter/addresswise-src}"
runtime_dir="${ADDRESSWISE_RUNTIME_DIR:-/home/peter/addresswise-deploy}"
index_mode=none
requested_countries=""

usage() {
    echo "Usage: $0 [--rebuild-indexes | --add-countries CC,...]" >&2
}

case "${1:-}" in
    "") ;;
    --rebuild-indexes) index_mode=all ;;
    --add-countries)
        requested_countries="${2:-}"
        [[ -n "$requested_countries" ]] || { usage; exit 2; }
        index_mode=add
        ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
esac

cargo build --release

next_binary="$runtime_dir/addresswise.next.$$.bin"
scp target/release/addresswise "$remote_host:$next_binary"

requested_countries_arg="${requested_countries:--}"
ssh "$remote_host" bash -s -- "$runtime_dir" "$source_dir" "$index_mode" "$requested_countries_arg" "$next_binary" <<'REMOTE_SCRIPT'
set -euo pipefail

runtime_dir="$1"
source_dir="$2"
index_mode="$3"
requested_countries="$4"
next_binary="$5"
git -C "$source_dir" pull --ff-only origin master

test -x "$next_binary"

if [[ "$index_mode" != none ]]; then
    next_indexes="$runtime_dir/data/indexes.next.$$"
    country_sql="select string_agg(country_code, chr(44) order by country_code) from (select distinct upper(trim(country_code)) as country_code from addresses where is_active and length(trim(country_code)) > 0) countries where country_code ~ '^[A-Z]{2}$';"
    country_codes="$(sudo sh -c '
        set -a
        . /etc/addresswise.env
        set +a
        exec psql -qAt -d "$DATABASE_URL" -c "$1"
    ' sh "$country_sql")"
    if [[ -z "$country_codes" ]]; then
        echo "No active country codes found in the production database." >&2
        exit 1
    fi
    build_codes="$country_codes"
    if [[ "$index_mode" == add ]]; then
        build_codes="$(printf '%s' "$requested_countries" | tr '[:lower:]' '[:upper:]')"
        if [[ ! "$build_codes" =~ ^[A-Z]{2}(,[A-Z]{2})*$ ]]; then
            echo "Invalid country list for --add-countries: $requested_countries" >&2
            exit 2
        fi
        IFS=',' read -r -a requested_array <<< "$build_codes"
        IFS=',' read -r -a active_array <<< "$country_codes"
        for requested_code in "${requested_array[@]}"; do
            found=false
            for active_code in "${active_array[@]}"; do
                if [[ "$requested_code" == "$active_code" ]]; then
                    found=true
                    break
                fi
            done
            if [[ "$found" != true ]]; then
                echo "Requested country $requested_code is not active in the production database." >&2
                exit 1
            fi
        done
        cp -al "$runtime_dir/data/indexes" "$next_indexes"
    fi
    sudo sh -c '
        set -a
        . /etc/addresswise.env
        set +a
        export COUNTRY_CODES="$1" INDEX_DIR="$2"
        exec runuser -u peter -- "$3" build-indexes
    ' sh "$build_codes" "$next_indexes" "$next_binary"

    sudo install -d -m 0755 /etc/systemd/system/addresswise.service.d
    printf '[Service]\nEnvironment=COUNTRY_CODES=%s\n' "$country_codes" \
        | sudo tee /etc/systemd/system/addresswise.service.d/country-codes.conf >/dev/null
    sudo systemctl daemon-reload
fi

timestamp="$(date +%Y%m%d%H%M%S)"
previous_binary="$runtime_dir/addresswise.$timestamp"
previous_indexes=""
cutover_started=false
rollback() {
    status="$?"
    if [[ "$status" -ne 0 && "$cutover_started" == true ]]; then
        mv "$previous_binary" "$runtime_dir/addresswise" || true
        if [[ "$index_mode" != none && -n "$previous_indexes" ]]; then
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

if [[ "$index_mode" != none ]]; then
    previous_indexes="$runtime_dir/data/indexes.$timestamp"
    mv "$runtime_dir/data/indexes" "$previous_indexes"
    mv "$next_indexes" "$runtime_dir/data/indexes"
fi

sudo systemctl start addresswise
for attempt in {1..15}; do
    if sudo systemctl is-active --quiet addresswise \
        && curl --fail --silent --show-error http://127.0.0.1:8080/health >/dev/null \
        && curl --fail --silent --show-error http://127.0.0.1:8080/ | grep -Fq '<title>addresswise</title>' \
        && curl --fail --silent --show-error http://127.0.0.1:8080/admin | grep -Fq '<title>addresswise — API key admin</title>'; then
        break
    fi
    if [[ "$attempt" == 15 ]]; then
        exit 1
    fi
    sleep 1
done
cutover_started=false
REMOTE_SCRIPT
