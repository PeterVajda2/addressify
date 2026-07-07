use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use addressify::address_rules::{
    clean_thoroughfare, format_display_address, normalize_address_parts,
};
use addressify::normalize::normalize_text;
use anyhow::{Context, Result, bail};
use calamine::{Data, Range, Reader, Sheets, open_workbook_auto};
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
    datasets: usize,
}

#[derive(Debug, Default, Clone)]
struct Settlement {
    locality: String,
    dependent_locality: Option<String>,
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

    let run_marker = current_run_marker()?;
    let mut workbook = open_workbook_auto(&opts.input)
        .with_context(|| format!("failed to open {}", opts.input.display()))?;

    let settlements = load_settlements(&mut workbook)?;
    let mut all_rows = Vec::new();
    all_rows.extend(build_settlement_rows(&opts.input, run_marker, &settlements));
    all_rows.extend(build_street_rows(
        &mut workbook,
        &opts.input,
        run_marker,
        &settlements,
    )?);

    if let Some(limit) = opts.limit && all_rows.len() > limit {
        all_rows.truncate(limit);
    }

    let parsed = all_rows.len();
    let mut totals = Totals {
        rows_parsed: parsed,
        ..Default::default()
    };

    let mut buffer = Vec::with_capacity(opts.batch_size);
    let mut datasets_seen = HashSet::new();

    for row in all_rows {
        datasets_seen.insert(row.source_dataset.clone());
        buffer.push(row);
        if buffer.len() >= opts.batch_size {
            let (inserted, deduped) = flush_batch(pool.as_ref(), &mut buffer, opts.dry_run).await?;
            totals.rows_inserted += inserted;
            totals.rows_deduped += deduped;
        }
    }

    if !buffer.is_empty() {
        let (inserted, deduped) = flush_batch(pool.as_ref(), &mut buffer, opts.dry_run).await?;
        totals.rows_inserted += inserted;
        totals.rows_deduped += deduped;
    }

    if !opts.dry_run && opts.limit.is_none() {
        if let Some(pool) = pool.as_ref() {
            for source_dataset in &datasets_seen {
                deactivate_missing_rows(pool, source_dataset, run_marker).await?;
            }
        }
    }

    totals.datasets = datasets_seen.len();
    totals.rows_skipped = totals
        .rows_parsed
        .saturating_sub(totals.rows_inserted + totals.rows_deduped);

    println!(
        "done input={} parsed={} inserted={} deduped={} skipped={} datasets={} mode={}",
        opts.input.display(),
        totals.rows_parsed,
        totals.rows_inserted,
        totals.rows_deduped,
        totals.rows_skipped,
        totals.datasets,
        if opts.dry_run { "dry-run" } else { "import" }
    );

    Ok(())
}

fn load_settlements(workbook: &mut Sheets<BufReader<File>>) -> Result<HashMap<String, Vec<Settlement>>> {
    let range = read_sheet(workbook, "Települések")?;
    let mut by_postal: HashMap<String, Vec<Settlement>> = HashMap::new();

    for row in range.rows().skip(1) {
        let postal_code = clean_postal_code(row.get(0).map(cell_to_string));
        let locality = to_opt_string(row.get(1).map(cell_to_string));
        let dependent_locality = to_opt_string(row.get(2).map(cell_to_string));
        let (Some(postal_code), Some(locality)) = (postal_code, locality) else {
            continue;
        };
        by_postal.entry(postal_code).or_default().push(Settlement {
            locality,
            dependent_locality,
        });
    }

    Ok(by_postal)
}

fn build_settlement_rows(
    input_path: &Path,
    run_marker: i64,
    settlements: &HashMap<String, Vec<Settlement>>,
) -> Vec<AddressRow> {
    let base_name = input_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("HU_data.xlsx");
    let source_dataset = format!("{base_name}#Telepulesek");

    let mut rows = Vec::new();
    for (postal_code, options) in settlements {
        for settlement in options {
            let full_address = format_display_address(
                "HU",
                None,
                None,
                None,
                Some(settlement.locality.as_str()),
                settlement.dependent_locality.as_deref(),
                None,
                Some(postal_code.as_str()),
            );
            if full_address.is_empty() {
                continue;
            }

            let source_hash = format!(
                "hu|telepulesek|{}|{}|{}",
                postal_code,
                settlement.locality,
                settlement.dependent_locality.clone().unwrap_or_default()
            );

            rows.push(AddressRow {
                source_hash,
                country_code: "HU".to_string(),
                source_dataset: source_dataset.clone(),
                admin_area: None,
                locality: Some(settlement.locality.clone()),
                dependent_locality: settlement.dependent_locality.clone(),
                thoroughfare: None,
                premise: None,
                premise_type: None,
                subpremise: None,
                postal_code: Some(postal_code.clone()),
                latitude: None,
                longitude: None,
                full_address: full_address.clone(),
                search_text: normalize_text(&full_address),
                last_seen_run: run_marker,
            });
        }
    }

    rows
}

fn build_street_rows(
    workbook: &mut Sheets<BufReader<File>>,
    input_path: &Path,
    run_marker: i64,
    settlements: &HashMap<String, Vec<Settlement>>,
) -> Result<Vec<AddressRow>> {
    let base_name = input_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("HU_data.xlsx");

    let city_by_sheet = [
        ("Bp.u.", "Budapest"),
        ("Miskolc u.", "Miskolc"),
        ("Debrecen u.", "Debrecen"),
        ("Szeged u.", "Szeged"),
        ("Pécs u.", "Pécs"),
        ("Győr u.", "Győr"),
    ];

    let mut rows = Vec::new();
    for (sheet_name, fallback_city) in city_by_sheet {
        let range = read_sheet(workbook, sheet_name)?;
        let source_dataset = format!("{base_name}#{sheet_name}");

        for row in range.rows().skip(1) {
            let postal_code = clean_postal_code(row.get(0).map(cell_to_string));
            let street_name = to_opt_string(row.get(1).map(cell_to_string));
            let street_type = to_opt_string(row.get(2).map(cell_to_string));
            let row_district = to_opt_string(row.get(3).map(cell_to_string));
            let num1 = to_opt_string(row.get(4).map(cell_to_string));
            let jel1 = to_opt_string(row.get(5).map(cell_to_string));
            let num2 = to_opt_string(row.get(6).map(cell_to_string));
            let jel2 = to_opt_string(row.get(7).map(cell_to_string));
            let ker = to_opt_string(row.get(8).map(cell_to_string));

            let (Some(postal_code), Some(street_name)) = (postal_code, street_name) else {
                continue;
            };

            let thoroughfare = clean_thoroughfare(
                join_parts([Some(street_name.as_str()), street_type.as_deref()]).as_deref(),
            );
            let (raw_premise, side_hint) = render_house_spec(
                num1.as_deref(),
                jel1.as_deref(),
                num2.as_deref(),
                jel2.as_deref(),
            );
            let parsed =
                normalize_address_parts("HU", raw_premise.as_deref(), side_hint.as_deref());
            let premise = parsed.house_number;
            let premise_type = parsed.house_number_type;
            let unit = parsed.unit;

            let locality = resolve_locality(settlements.get(&postal_code), fallback_city);
            let dependent_locality = row_district
                .clone()
                .or_else(|| resolve_dependent_locality(settlements.get(&postal_code), &locality));
            let admin_area = ker.clone();

            let full_address = format_display_address(
                "HU",
                thoroughfare.as_deref(),
                premise.as_deref(),
                unit.as_deref(),
                Some(locality.as_str()),
                dependent_locality.as_deref(),
                admin_area.as_deref(),
                Some(postal_code.as_str()),
            );
            if full_address.is_empty() {
                continue;
            }

            let source_hash = format!(
                "hu|{sheet_name}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
                postal_code,
                locality,
                thoroughfare.clone().unwrap_or_default(),
                dependent_locality.clone().unwrap_or_default(),
                admin_area.clone().unwrap_or_default(),
                num1.clone().unwrap_or_default(),
                jel1.clone().unwrap_or_default(),
                num2.clone().unwrap_or_default(),
                jel2.clone().unwrap_or_default()
            );

            rows.push(AddressRow {
                source_hash,
                country_code: "HU".to_string(),
                source_dataset: source_dataset.clone(),
                admin_area,
                locality: Some(locality),
                dependent_locality,
                thoroughfare,
                premise,
                premise_type,
                subpremise: unit,
                postal_code: Some(postal_code),
                latitude: None,
                longitude: None,
                full_address: full_address.clone(),
                search_text: normalize_text(&full_address),
                last_seen_run: run_marker,
            });
        }
    }

    Ok(rows)
}

fn resolve_locality(matches: Option<&Vec<Settlement>>, fallback_city: &str) -> String {
    let Some(matches) = matches else {
        return fallback_city.to_string();
    };
    if matches.len() == 1 {
        return matches[0].locality.clone();
    }
    if let Some(found) = matches
        .iter()
        .find(|m| m.locality.eq_ignore_ascii_case(fallback_city))
    {
        return found.locality.clone();
    }
    matches[0].locality.clone()
}

fn resolve_dependent_locality(matches: Option<&Vec<Settlement>>, locality: &str) -> Option<String> {
    let matches = matches?;
    if let Some(found) = matches
        .iter()
        .find(|m| m.locality.eq_ignore_ascii_case(locality) && m.dependent_locality.is_some())
    {
        return found.dependent_locality.clone();
    }
    matches.iter().find_map(|m| m.dependent_locality.clone())
}

fn render_house_spec(
    num1: Option<&str>,
    jel1: Option<&str>,
    num2: Option<&str>,
    jel2: Option<&str>,
) -> (Option<String>, Option<String>) {
    let n1 = parse_i32(num1);
    let n2 = parse_i32(num2);
    let j1 = to_opt_string(jel1.map(|s| s.to_string()));
    let j2 = to_opt_string(jel2.map(|s| s.to_string()));

    match n1 {
        Some(0) | None => (None, None),
        Some(value) if value > 0 => {
            let start = format!("{}{}", value, j1.unwrap_or_default());
            let premise = match n2 {
                Some(end) if end > 0 => Some(format!("{start}-{end}{}", j2.unwrap_or_default())),
                _ => Some(format!("{start}+")),
            };
            (premise, None)
        }
        Some(-1) => (None, Some("odd side".to_string())),
        Some(-2) => (None, Some("even side".to_string())),
        Some(-3) => (None, Some("remaining numbers".to_string())),
        Some(other) => (None, Some(format!("code {other}"))),
    }
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
    let mut input = PathBuf::from("address_data/HU_data.xlsx");
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
        "Usage: cargo run --bin etl_hu_xlsx -- [options]\n\n\
Options:\n\
  --input <path>          Input XLSX file (default: address_data/HU_data.xlsx)\n\
  --database-url <url>    PostgreSQL DSN (or set DATABASE_URL env var)\n\
  --batch-size <n>        Rows per INSERT batch (default: 4000, max: 4095)\n\
  --limit <n>             Stop after n transformed rows\n\
  --truncate              Truncate addresses before import\n\
  --dry-run               Parse and transform only, no DB connection/writes\n\
  -h, --help              Show help\n"
    );
}

fn read_sheet(workbook: &mut Sheets<BufReader<File>>, name: &str) -> Result<Range<Data>> {
    let range = workbook
        .worksheet_range(name)
        .with_context(|| format!("failed reading sheet: {name}"))?;
    Ok(range)
}

fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.to_string(),
        Data::Float(f) => {
            if (f.fract()).abs() < f64::EPSILON {
                format!("{:.0}", f)
            } else {
                f.to_string()
            }
        }
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => b.to_string(),
        Data::DateTime(dt) => dt.to_string(),
        Data::DateTimeIso(s) => s.to_string(),
        Data::DurationIso(s) => s.to_string(),
        Data::Error(_) => String::new(),
    }
}

fn clean_postal_code(value: Option<String>) -> Option<String> {
    let value = to_opt_string(value)?;
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() == 4 {
        Some(digits)
    } else {
        None
    }
}

fn join_parts(parts: [Option<&str>; 2]) -> Option<String> {
    let mut out = Vec::new();
    for part in parts {
        if let Some(v) = to_opt_string(part.map(|s| s.to_string())) {
            out.push(v);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join(" "))
    }
}

fn parse_i32(value: Option<&str>) -> Option<i32> {
    let v = value?.trim();
    if v.is_empty() {
        return None;
    }
    v.parse::<i32>().ok()
}

fn to_opt_string(value: Option<String>) -> Option<String> {
    let raw = value?;
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
    }
}

fn current_run_marker() -> Result<i64> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?;
    let nanos = duration.as_nanos();
    i64::try_from(nanos).context("run marker overflowed i64")
}
