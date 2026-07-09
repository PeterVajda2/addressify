use std::collections::HashSet;
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use addresswise::address_rules::{
    clean_thoroughfare, format_display_address, normalize_address_parts,
};
use addresswise::normalize::normalize_text;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sqlx::{PgPool, QueryBuilder};

const INSERT_BIND_PARAMS_PER_ROW: usize = 16;
const POSTGRES_MAX_BIND_PARAMS: usize = 65_535;
const MAX_SAFE_BATCH_SIZE: usize = POSTGRES_MAX_BIND_PARAMS / INSERT_BIND_PARAMS_PER_ROW;
const DEFAULT_BATCH_SIZE: usize = 4_000;

#[derive(Debug, Clone)]
struct Options {
    input_files: Vec<PathBuf>,
    database_url: Option<String>,
    country_override: Option<String>,
    batch_size: usize,
    truncate: bool,
    limit: Option<usize>,
    dry_run: bool,
}

#[derive(Debug, Default)]
struct Totals {
    files: usize,
    lines: usize,
    parsed: usize,
    inserted: usize,
    skipped_json: usize,
    skipped_invalid: usize,
}

#[derive(Debug, Default)]
struct FileStats {
    lines: usize,
    parsed: usize,
    inserted: usize,
    skipped_json: usize,
    skipped_invalid: usize,
}

#[derive(Debug, Deserialize)]
struct Feature {
    #[serde(default)]
    properties: Properties,
    #[serde(default)]
    geometry: Option<Geometry>,
}

#[derive(Debug, Default, Deserialize)]
struct Properties {
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    number: Option<String>,
    #[serde(default)]
    street: Option<String>,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    district: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    postcode: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Geometry {
    #[serde(default)]
    coordinates: Vec<f64>,
}

#[derive(Debug)]
struct AddressRow {
    source_hash: String,
    country_code: String,
    source_dataset: String,
    admin_area: Option<String>,
    locality: Option<String>,
    dependent_locality: Option<String>,
    thoroughfare: Option<String>,
    premise: Option<String>,
    premise_type: Option<String>,
    subpremise: Option<String>,
    postal_code: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    full_address: String,
    search_text: String,
    last_seen_run: i64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let opts = parse_args()?;

    let pool = if opts.dry_run {
        None
    } else {
        let db_url = opts
            .database_url
            .as_deref()
            .context("database URL missing; pass --database-url or set DATABASE_URL")?;
        Some(PgPool::connect(db_url).await.context("failed to connect to PostgreSQL")?)
    };

    if opts.truncate {
        if opts.dry_run {
            println!("skip truncate in dry-run mode");
        } else if let Some(pool) = &pool {
            sqlx::query("TRUNCATE TABLE addresses RESTART IDENTITY")
                .execute(pool)
                .await
                .context("failed to truncate addresses")?;
        }
    }

    let mut totals = Totals::default();
    let mut parsed_total_for_limit = 0usize;

    for file in &opts.input_files {
        let country = resolve_country_code(file, opts.country_override.as_deref())?;
        let source_dataset = resolve_source_dataset(file)?;
        let run_marker = current_run_marker()?;
        let stats = process_file(
            file,
            &country,
            &source_dataset,
            run_marker,
            &opts,
            pool.as_ref(),
            &mut parsed_total_for_limit,
        )
        .await?;

        totals.files += 1;
        totals.lines += stats.lines;
        totals.parsed += stats.parsed;
        totals.inserted += stats.inserted;
        totals.skipped_json += stats.skipped_json;
        totals.skipped_invalid += stats.skipped_invalid;

        println!(
            "file={} country={} lines={} parsed={} inserted={} skipped_json={} skipped_invalid={}",
            file.display(),
            country,
            stats.lines,
            stats.parsed,
            stats.inserted,
            stats.skipped_json,
            stats.skipped_invalid,
        );

        if let Some(limit) = opts.limit && parsed_total_for_limit >= limit {
            break;
        }
    }

    println!(
        "done files={} lines={} parsed={} inserted={} skipped_json={} skipped_invalid={} mode={}",
        totals.files,
        totals.lines,
        totals.parsed,
        totals.inserted,
        totals.skipped_json,
        totals.skipped_invalid,
        if opts.dry_run { "dry-run" } else { "import" }
    );

    Ok(())
}

async fn process_file(
    file: &Path,
    country_code: &str,
    source_dataset: &str,
    run_marker: i64,
    opts: &Options,
    pool: Option<&PgPool>,
    parsed_total_for_limit: &mut usize,
) -> Result<FileStats> {
    let source = File::open(file)
        .with_context(|| format!("failed to open input file: {}", file.display()))?;
    let reader = BufReader::new(source);

    let mut stats = FileStats::default();
    let mut buffer: Vec<AddressRow> = Vec::with_capacity(opts.batch_size);

    for line in reader.lines() {
        let line = line?;
        stats.lines += 1;

        if line.trim().is_empty() {
            continue;
        }

        let feature: Feature = match serde_json::from_str(&line) {
            Ok(item) => item,
            Err(err) => {
                if stats.skipped_json < 5 {
                    eprintln!(
                        "skip invalid JSON file={} line={} err={}",
                        file.display(),
                        stats.lines,
                        err
                    );
                }
                stats.skipped_json += 1;
                continue;
            }
        };

        match to_row(feature, country_code, source_dataset, run_marker) {
            Some(row) => {
                stats.parsed += 1;
                *parsed_total_for_limit += 1;
                buffer.push(row);
            }
            None => stats.skipped_invalid += 1,
        }

        if buffer.len() >= opts.batch_size {
            stats.inserted += flush_batch(pool, &mut buffer, opts.dry_run).await?;
        }

        if stats.parsed % 100_000 == 0 {
            println!(
                "progress file={} parsed={} inserted={} skipped_json={} skipped_invalid={}",
                file.display(),
                stats.parsed,
                stats.inserted,
                stats.skipped_json,
                stats.skipped_invalid,
            );
        }

        if let Some(limit) = opts.limit && *parsed_total_for_limit >= limit {
            break;
        }
    }

    if !buffer.is_empty() {
        stats.inserted += flush_batch(pool, &mut buffer, opts.dry_run).await?;
    }

    if !opts.dry_run && opts.limit.is_none() {
        if let Some(pool) = pool {
            deactivate_missing_rows(pool, source_dataset, run_marker).await?;
        }
    }

    Ok(stats)
}

async fn flush_batch(pool: Option<&PgPool>, rows: &mut Vec<AddressRow>, dry_run: bool) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }

    if dry_run {
        let count = rows.len();
        rows.clear();
        return Ok(count);
    }

    let Some(pool) = pool else {
        bail!("internal error: pool missing in import mode");
    };

    let deduped = dedupe_batch_by_conflict_key(rows);
    if deduped > 0 {
        eprintln!("deduped {deduped} duplicate rows in insert batch");
    }

    let inserted = insert_batch(pool, rows).await?;
    rows.clear();
    Ok(inserted)
}

fn dedupe_batch_by_conflict_key(rows: &mut Vec<AddressRow>) -> usize {
    if rows.len() < 2 {
        return 0;
    }

    let original_len = rows.len();
    let mut seen: HashSet<(String, String)> = HashSet::with_capacity(original_len);
    rows.reverse();
    rows.retain(|row| seen.insert((row.country_code.clone(), row.source_hash.clone())));
    rows.reverse();
    original_len - rows.len()
}

fn parse_args() -> Result<Options> {
    let mut input_files: Vec<PathBuf> = Vec::new();
    let mut input_dir = PathBuf::from("address_data");
    let mut database_url = env::var("DATABASE_URL").ok();
    let mut country_override = None;
    let mut batch_size = DEFAULT_BATCH_SIZE;
    let mut truncate = false;
    let mut limit = None;
    let mut dry_run = false;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => input_files.push(PathBuf::from(args.next().context("missing value for --input")?)),
            "--input-dir" => input_dir = PathBuf::from(args.next().context("missing value for --input-dir")?),
            "--database-url" => database_url = Some(args.next().context("missing value for --database-url")?),
            "--country" => country_override = Some(args.next().context("missing value for --country")?),
            "--batch-size" => {
                let value = args.next().context("missing value for --batch-size")?;
                batch_size = value.parse::<usize>().with_context(|| format!("invalid --batch-size: {value}"))?;
            }
            "--limit" => {
                let value = args.next().context("missing value for --limit")?;
                limit = Some(value.parse::<usize>().with_context(|| format!("invalid --limit: {value}"))?);
            }
            "--truncate" => truncate = true,
            "--dry-run" => dry_run = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other}"),
        }
    }

    if batch_size == 0 {
        bail!("--batch-size must be > 0");
    }
    if batch_size > MAX_SAFE_BATCH_SIZE {
        bail!("--batch-size is too large for PostgreSQL parameter limits (max: {MAX_SAFE_BATCH_SIZE})");
    }

    if input_files.is_empty() {
        input_files = collect_input_files(&input_dir)?;
    }
    if input_files.is_empty() {
        bail!("no input files found");
    }
    input_files.sort();

    let country_override = country_override.map(|cc| cc.to_uppercase());
    if !dry_run && database_url.is_none() {
        bail!("database URL missing; pass --database-url or set DATABASE_URL");
    }

    Ok(Options {
        input_files,
        database_url,
        country_override,
        batch_size,
        truncate,
        limit,
        dry_run,
    })
}

fn print_help() {
    println!(
        "Usage: cargo run --bin etl_geojson -- [options]\n\n\
Options:\n\
  --input <path>          Import one file; repeatable\n\
  --input-dir <path>      Import all *_source.geojson from directory (default: address_data)\n\
  --database-url <url>    PostgreSQL DSN (or set DATABASE_URL env var)\n\
  --country <CC>          Override country code for all input files\n\
  --batch-size <n>        Rows per INSERT batch (default: 4000, max: 4095)\n\
  --limit <n>             Stop after n valid parsed rows (across all files)\n\
  --truncate              Truncate addresses before import\n\
  --dry-run               Parse and transform only, no DB connection/writes\n\
  -h, --help              Show help\n"
    );
}

fn collect_input_files(input_dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(input_dir)
        .with_context(|| format!("failed to read input dir: {}", input_dir.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.is_file()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| name.ends_with("_source.geojson"))
        {
            files.push(path);
        }
    }
    Ok(files)
}

fn resolve_country_code(file: &Path, override_code: Option<&str>) -> Result<String> {
    if let Some(code) = override_code {
        if is_country_code(code) {
            return Ok(code.to_string());
        }
        bail!("invalid --country value: {code}");
    }

    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("invalid file name: {}", file.display()))?;
    let candidate = name.split('_').next().unwrap_or_default().to_uppercase();
    if is_country_code(&candidate) {
        Ok(candidate)
    } else {
        bail!("cannot infer country code from file name: {} (expected like CZ_source.geojson)", file.display())
    }
}

fn resolve_source_dataset(file: &Path) -> Result<String> {
    file.file_name()
        .and_then(|n| n.to_str())
        .map(ToString::to_string)
        .with_context(|| format!("invalid file name: {}", file.display()))
}

fn is_country_code(value: &str) -> bool {
    value.len() == 2 && value.chars().all(|c| c.is_ascii_alphabetic())
}

fn to_row(feature: Feature, country_code: &str, source_dataset: &str, run_marker: i64) -> Option<AddressRow> {
    let p = feature.properties;

    let source_hash = to_opt_string(p.hash.as_deref())?;
    let street = to_opt_string(p.street.as_deref());
    let number = to_opt_string(p.number.as_deref());
    let unit = to_opt_string(p.unit.as_deref());
    let city = to_opt_string(p.city.as_deref());
    let district = to_opt_string(p.district.as_deref());
    let region = to_opt_string(p.region.as_deref());
    let postcode = to_opt_string(p.postcode.as_deref());

    let (lon, lat) = match feature.geometry {
        Some(geometry) if geometry.coordinates.len() >= 2 => {
            (Some(geometry.coordinates[0]), Some(geometry.coordinates[1]))
        }
        _ => (None, None),
    };

    let thoroughfare = clean_thoroughfare(street.as_deref());
    let parsed = normalize_address_parts(country_code, number.as_deref(), unit.as_deref());
    let premise = parsed.house_number;
    let premise_type = parsed.house_number_type;
    let subpremise = parsed.unit;
    let locality = city.clone();

    let full_address = format_display_address(
        country_code,
        thoroughfare.as_deref(),
        premise.as_deref(),
        subpremise.as_deref(),
        locality.as_deref(),
        district.as_deref(),
        region.as_deref(),
        postcode.as_deref(),
    );
    if full_address.is_empty() {
        return None;
    }

    Some(AddressRow {
        source_hash,
        country_code: country_code.to_string(),
        source_dataset: source_dataset.to_string(),
        admin_area: region,
        locality,
        dependent_locality: district,
        thoroughfare,
        premise,
        premise_type,
        subpremise,
        postal_code: postcode,
        latitude: lat,
        longitude: lon,
        full_address: full_address.clone(),
        search_text: normalize_text(&full_address),
        last_seen_run: run_marker,
    })
}

async fn insert_batch(pool: &PgPool, rows: &[AddressRow]) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }

    let mut qb = QueryBuilder::new(
        "INSERT INTO addresses (\
            source_hash, country_code, source_dataset, admin_area, locality, dependent_locality,\
            thoroughfare, premise, premise_type, subpremise, postal_code, latitude, longitude,\
            full_address, search_text, last_seen_run\
        ) ",
    );

    qb.push_values(rows, |mut b, row| {
        b.push_bind(&row.source_hash)
            .push_bind(&row.country_code)
            .push_bind(&row.source_dataset)
            .push_bind(&row.admin_area)
            .push_bind(&row.locality)
            .push_bind(&row.dependent_locality)
            .push_bind(&row.thoroughfare)
            .push_bind(&row.premise)
            .push_bind(&row.premise_type)
            .push_bind(&row.subpremise)
            .push_bind(&row.postal_code)
            .push_bind(row.latitude)
            .push_bind(row.longitude)
            .push_bind(&row.full_address)
            .push_bind(&row.search_text)
            .push_bind(row.last_seen_run);
    });

    qb.push(
        " ON CONFLICT (country_code, source_hash) DO UPDATE SET \
            source_dataset = EXCLUDED.source_dataset,\
            admin_area = EXCLUDED.admin_area,\
            locality = EXCLUDED.locality,\
            dependent_locality = EXCLUDED.dependent_locality,\
            thoroughfare = EXCLUDED.thoroughfare,\
            premise = EXCLUDED.premise,\
            premise_type = EXCLUDED.premise_type,\
            subpremise = EXCLUDED.subpremise,\
            postal_code = EXCLUDED.postal_code,\
            latitude = EXCLUDED.latitude,\
            longitude = EXCLUDED.longitude,\
            full_address = EXCLUDED.full_address,\
            search_text = EXCLUDED.search_text,\
            last_seen_run = EXCLUDED.last_seen_run,\
            is_active = TRUE",
    );

    let result = qb.build().execute(pool).await.context("failed bulk insert")?;
    Ok(result.rows_affected() as usize)
}

async fn deactivate_missing_rows(pool: &PgPool, source_dataset: &str, run_marker: i64) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE addresses
        SET is_active = FALSE
        WHERE source_dataset = $1
          AND is_active = TRUE
          AND COALESCE(last_seen_run, 0) <> $2
        "#,
    )
    .bind(source_dataset)
    .bind(run_marker)
    .execute(pool)
    .await
    .with_context(|| format!("failed to deactivate stale rows for dataset {source_dataset}"))?;
    Ok(())
}

fn current_run_marker() -> Result<i64> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?;
    let nanos = duration.as_nanos();
    i64::try_from(nanos).context("run marker overflowed i64")
}

fn to_opt_string(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
