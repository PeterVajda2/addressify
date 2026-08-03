#!/usr/bin/env bash
set -euo pipefail

# Stream OpenStreetMap address-tagged objects from a PBF into the existing
# GeoJSON importer. The FIFO keeps the multi-gigabyte intermediate data off
# disk. OSM nodes retain their coordinates; ways and relations are imported
# without a coordinate because address search does not require one.

if [[ $# -ne 1 ]]; then
    echo "Usage: $0 /path/to/country-latest.osm.pbf" >&2
    exit 2
fi

pbf_file="$1"
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
country_code="${OSM_COUNTRY_CODE:-DE}"
database_url="${DATABASE_URL:?set DATABASE_URL}"
etl_binary="${ETL_GEOJSON_BINARY:-$root_dir/target/release/etl_geojson}"
batch_size="${OSM_ETL_BATCH_SIZE:-4000}"
fifo="/tmp/${country_code}_osm_pbf_source.geojson"
filtered_pbf="$(mktemp /tmp/addresswise-osm-filtered.XXXXXX.pbf)"

[[ -r "$pbf_file" ]] || { echo "PBF is not readable: $pbf_file" >&2; exit 1; }
[[ -x "$etl_binary" ]] || { echo "ETL binary is not executable: $etl_binary" >&2; exit 1; }
command -v osmium >/dev/null || { echo "osmium is required" >&2; exit 1; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }
[[ ! -e "$fifo" ]] || { echo "temporary FIFO already exists: $fifo" >&2; exit 1; }

mkfifo "$fifo"
cleanup() {
    rm -f "$fifo"
    rm -f "$filtered_pbf"
}
trap cleanup EXIT

# tags-filter needs two passes over its input, so write its much smaller output
# to a temporary PBF. It keeps referenced nodes by default, allowing osmium to
# export address-bearing ways and relations as well as address nodes.
osmium tags-filter "$pbf_file" \
    nwr/addr:housenumber nwr/addr:street nwr/addr:place \
    -f pbf -o "$filtered_pbf"

(
    osmium export "$filtered_pbf" -f geojsonseq -a type,id \
    | jq --seq -c '
        . as $feature
        | $feature.properties as $p
        | select(
            (($p["addr:housenumber"] // "") | length > 0)
            or ((($p["addr:street"] // "") | length > 0) and (($p["addr:postcode"] // "") | length > 0))
          )
        | {
            type: "Feature",
            properties: {
              hash: ("osm:" + ($p["@type"] | tostring) + ":" + ($p["@id"] | tostring)),
              number: $p["addr:housenumber"],
              street: $p["addr:street"],
              unit: $p["addr:unit"],
              city: ($p["addr:city"] // $p["addr:place"]),
              district: ($p["addr:district"] // $p["addr:suburb"]),
              region: ($p["addr:state"] // $p["is_in:state"]),
              postcode: $p["addr:postcode"]
            },
            geometry: (if $feature.geometry.type == "Point" then $feature.geometry else null end)
          }
      ' | tr -d '\036' > "$fifo"
) &
producer_pid=$!

"$etl_binary" \
    --input "$fifo" \
    --country "$country_code" \
    --database-url "$database_url" \
    --batch-size "$batch_size"

wait "$producer_pid"
