# ETL

This project now includes the ETL entry points extracted from `/home/peter/address_wise`.

Available binaries:

- `etl_geojson`
- `etl_be_csv`
- `etl_hu_xlsx`

Helper scripts:

- `scripts/etl_geojson.sh`
- `scripts/etl_be_csv.sh`
- `scripts/etl_hu_xlsx.sh`

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
