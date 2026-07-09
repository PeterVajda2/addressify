use std::collections::HashSet;
use std::env;
use std::fs::File;
use std::path::PathBuf;

use addresswise::address_rules::{
    clean_thoroughfare, format_display_address, normalize_address_parts,
};
use addresswise::normalize::normalize_text;
use anyhow::{Context, Result, bail};
use csv::ReaderBuilder;
use serde::Deserialize;
use sqlx::{PgPool, QueryBuilder};

const INSERT_BIND_PARAMS_PER_ROW: usize = 16;
const POSTGRES_MAX_BIND_PARAMS: usize = 65_535;
const MAX_SAFE_BATCH_SIZE: usize = POSTGRES_MAX_BIND_PARAMS / INSERT_BIND_PARAMS_PER_ROW;
const DEFAULT_BATCH_SIZE: usize = 4_000;

#[derive(Debug, Clone)]
struct Options {
    input: PathBuf,
    database_url: Option<String>,
    batch_size: usize,
    truncate: bool,
    limit: Option<usize>,
    dry_run: bool,
}

#[derive(Debug, Default)]
struct Totals {
    rows_parsed: usize,
    rows_inserted: usize,
    rows_skipped: usize,
    rows_deduped: usize,
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

#[derive(Debug, Deserialize)]
struct BeRecord {
    #[serde(default)]
    fed_address_id: String,
    #[serde(default)]
    best_id: String,
    #[serde(default)]
    housenumber: String,
    #[serde(default)]
    boxnumber: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    officially_assigned: String,
    #[serde(default)]
    postal_info_objectid: String,
    #[serde(default)]
    streetname_fr: String,
    #[serde(default)]
    streetname_nl: String,
    #[serde(default)]
    streetname_de: String,
    #[serde(default)]
    municipality_fr: String,
    #[serde(default)]
    municipality_nl: String,
    #[serde(default)]
    municipality_de: String,
    #[serde(default)]
    part_of_municipality_fr: String,
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

    let run_marker = current_run_marker()?;
    let source_dataset = opts
        .input
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("BE_source.csv")
        .to_string();

    let file = File::open(&opts.input)
        .with_context(|| format!("failed to open {}", opts.input.display()))?;
    let mut reader = ReaderBuilder::new().flexible(false).from_reader(file);

    let mut totals = Totals::default();
    let mut buffer = Vec::with_capacity(opts.batch_size);

    for record in reader.deserialize::<BeRecord>() {
        let record = record.with_context(|| {
            format!(
                "failed to parse CSV row near record {} in {}",
                totals.rows_parsed + totals.rows_skipped + 1,
                opts.input.display()
            )
        })?;

        match to_row(record, &source_dataset, run_marker) {
            Some(row) => {
                totals.rows_parsed += 1;
                buffer.push(row);
            }
            None => {
                totals.rows_skipped += 1;
                continue;
            }
        }

        if buffer.len() >= opts.batch_size {
            let (inserted, deduped) = flush_batch(pool.as_ref(), &mut buffer, opts.dry_run).await?;
            totals.rows_inserted += inserted;
            totals.rows_deduped += deduped;
        }

        if totals.rows_parsed % 100_000 == 0 {
            println!(
                "progress input={} parsed={} inserted={} deduped={} skipped={}",
                opts.input.display(),
                totals.rows_parsed,
                totals.rows_inserted,
                totals.rows_deduped,
                totals.rows_skipped
            );
        }

        if let Some(limit) = opts.limit && totals.rows_parsed >= limit {
            break;
        }
    }

    if !buffer.is_empty() {
        let (inserted, deduped) = flush_batch(pool.as_ref(), &mut buffer, opts.dry_run).await?;
        totals.rows_inserted += inserted;
        totals.rows_deduped += deduped;
    }

    if !opts.dry_run && opts.limit.is_none() {
        if let Some(pool) = pool.as_ref() {
            deactivate_missing_rows(pool, &source_dataset, run_marker).await?;
        }
    }

    println!(
        "done input={} parsed={} inserted={} deduped={} skipped={} mode={}",
        opts.input.display(),
        totals.rows_parsed,
        totals.rows_inserted,
        totals.rows_deduped,
        totals.rows_skipped,
        if opts.dry_run { "dry-run" } else { "import" }
    );

    Ok(())
}

fn to_row(record: BeRecord, source_dataset: &str, run_marker: i64) -> Option<AddressRow> {
    if !record.status.trim().is_empty() && !record.status.eq_ignore_ascii_case("current") {
        return None;
    }
    if record.officially_assigned.eq_ignore_ascii_case("f") {
        return None;
    }

    let source_hash = first_non_empty([record.fed_address_id.as_str(), record.best_id.as_str()])?;
    let thoroughfare = clean_thoroughfare(
        compose_multilingual_name([
            &record.streetname_fr,
            &record.streetname_nl,
            &record.streetname_de,
        ])
        .as_deref(),
    );
    let locality = compose_multilingual_name([
        &record.municipality_fr,
        &record.municipality_nl,
        &record.municipality_de,
    ]);
    let dependent_locality = to_opt_string(&record.part_of_municipality_fr)
        .filter(|value| Some(value.as_str()) != locality.as_deref());
    let postal_code = clean_postal_code(&record.postal_info_objectid);
    let raw_house_number = to_opt_string(&record.housenumber);
    let unit = to_opt_string(&record.boxnumber);

    let parsed = normalize_address_parts("BE", raw_house_number.as_deref(), unit.as_deref());
    let premise = parsed.house_number;
    let premise_type = parsed.house_number_type;
    let subpremise = parsed.unit;

    let full_address = format_display_address(
        "BE",
        thoroughfare.as_deref(),
        premise.as_deref(),
        subpremise.as_deref(),
        locality.as_deref(),
        dependent_locality.as_deref(),
        None,
        postal_code.as_deref(),
    );
    if full_address.is_empty() {
        return None;
    }

    Some(AddressRow {
        source_hash,
        country_code: "BE".to_string(),
        source_dataset: source_dataset.to_string(),
        admin_area: None,
        locality,
        dependent_locality,
        thoroughfare,
        premise,
        premise_type,
        subpremise,
        postal_code,
        latitude: None,
        longitude: None,
        full_address: full_address.clone(),
        search_text: normalize_text(&full_address),
        last_seen_run: run_marker,
    })
}

fn compose_multilingual_name<'a>(values: [&'a str; 3]) -> Option<String> {
    let names = values.into_iter().filter_map(to_opt_string).fold(Vec::<String>::new(), |mut acc, value| {
        if !acc.iter().any(|existing| existing.eq_ignore_ascii_case(&value)) {
            acc.push(value);
        }
        acc
    });

    match names.as_slice() {
        [] => None,
        [only] => Some(only.clone()),
        [first, rest @ ..] => Some(format!("{first} ({})", rest.join(", "))),
    }
}

fn first_non_empty<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<String> {
    values.into_iter().find_map(to_opt_string)
}

fn to_opt_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn clean_postal_code(value: &str) -> Option<String> {
    let compact = value.chars().filter(|c| !c.is_ascii_whitespace()).collect::<String>();
    (!compact.is_empty()).then_some(compact)
}

async fn flush_batch(pool: Option<&PgPool>, rows: &mut Vec<AddressRow>, dry_run: bool) -> Result<(usize, usize)> {
    if rows.is_empty() {
        return Ok((0, 0));
    }

    let deduped = dedupe_batch_by_conflict_key(rows);
    if dry_run {
        let count = rows.len();
        rows.clear();
        return Ok((count, deduped));
    }

    let Some(pool) = pool else {
        bail!("internal error: pool missing in import mode");
    };

    let inserted = insert_batch(pool, rows).await?;
    rows.clear();
    Ok((inserted, deduped))
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

fn parse_args() -> Result<Options> {
    let mut input = PathBuf::from("address_data/BE_source.csv");
    let mut database_url = env::var("DATABASE_URL").ok();
    let mut batch_size = DEFAULT_BATCH_SIZE;
    let mut truncate = false;
    let mut limit = None;
    let mut dry_run = false;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => input = PathBuf::from(args.next().context("missing value for --input")?),
            "--database-url" => database_url = Some(args.next().context("missing value for --database-url")?),
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
    if !input.exists() {
        bail!("input file not found: {}", input.display());
    }
    if !dry_run && database_url.is_none() {
        bail!("database URL missing; pass --database-url or set DATABASE_URL");
    }

    Ok(Options {
        input,
        database_url,
        batch_size,
        truncate,
        limit,
        dry_run,
    })
}

fn print_help() {
    println!(
        "Usage: cargo run --bin etl_be_csv -- [options]\n\n\
Options:\n\
  --input <path>          Input CSV file (default: address_data/BE_source.csv)\n\
  --database-url <url>    PostgreSQL DSN (or set DATABASE_URL env var)\n\
  --batch-size <n>        Rows per INSERT batch (default: 4000, max: 4095)\n\
  --limit <n>             Stop after n valid parsed rows\n\
  --truncate              Truncate addresses before import\n\
  --dry-run               Parse and transform only, no DB connection/writes\n\
  -h, --help              Show help\n"
    );
}

fn current_run_marker() -> Result<i64> {
    use std::time::{SystemTime, UNIX_EPOCH};

    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_secs() as i64)
}
