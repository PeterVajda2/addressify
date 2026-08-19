# ETL

This project now includes the ETL entry points extracted from `/home/peter/address_wise`.

Available binaries:

- `etl_geojson`
- `etl_be_csv`
- `etl_hu_xlsx`
- `etl_no_csv`

Helper scripts:

- `scripts/etl_geojson.sh`
- `scripts/etl_be_csv.sh`
- `scripts/etl_hu_xlsx.sh`
- `scripts/etl_no_csv.sh`
- `scripts/etl_osm_pbf.sh`

All ETL tools write into the `addresses` table defined in:

- `db/0001_address_matching.sql`

Examples:

```bash
DATABASE_URL=postgres://address:address@127.0.0.1:5432/address_wise \
cargo run --release --bin etl_geojson -- --input-dir ./address_data
```

```bash
DATABASE_URL=postgres://address:address@127.0.0.1:5432/address_wise \
cargo run --release --bin etl_be_csv -- --input ./address_data/BE_source.csv
```

```bash
DATABASE_URL=postgres://address:address@127.0.0.1:5432/address_wise \
cargo run --release --bin etl_hu_xlsx -- --input ./address_data/HU_data.xlsx
```

```bash
DATABASE_URL=postgres://address:address@127.0.0.1:5432/address_wise \
cargo run --release --bin etl_no_csv -- --input ./norway_data/matrikkelenAdresse.csv
```

## Norway CSV imports

`etl_no_csv` imports the Norwegian matrikkel address CSV. By default it imports
only `vegadresse` rows, which are normal street addresses suitable for
autocomplete. Pass `--include-matrikkel` to also ingest `matrikkeladresse`
rows, which are cadastral-only addresses and may have no street name.

The importer reads semicolon-delimited CSV with the source headers from
`matrikkelenAdresse.csv`, converts projected `EPSG:258xx` coordinates to WGS84
latitude/longitude, normalizes uppercase locality/admin names for display, and
stores country code `NO`.

## OSM PBF imports

`scripts/etl_osm_pbf.sh` streams address-tagged OSM objects through the
GeoJSON importer. It resolves a locality from `addr:city`, `addr:town`,
`addr:village`, `addr:municipality`, `addr:place`, `is_in:city`, or
`is_in:municipality`.

Production imports require a locality by default (`OSM_REQUIRE_LOCALITY=true`).
This deliberately skips address objects with no locality on the object itself:
they must first be enriched by a spatial join against OSM administrative
boundaries or place data. Do not disable this guard for production data.
