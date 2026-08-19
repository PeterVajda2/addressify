use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::File;
use std::path::PathBuf;

use addresswise::address_rules::{
    DisplayAddressParts, clean_thoroughfare, format_display_address, normalize_address_parts,
};
use addresswise::normalize::normalize_text;
use anyhow::{Context, Result, bail};
use csv::{ReaderBuilder, StringRecord};
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
    include_matrikkel: bool,
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

#[derive(Debug)]
struct SourceRecord {
    localid: String,
    adresse_id: String,
    uuid_adresse: String,
    kommunenavn: String,
    adressetype: String,
    adressetilleggsnavn: String,
    adressenavn: String,
    nummer: String,
    bokstav: String,
    undernummer: String,
    adresse_tekst: String,
    epsg_kode: String,
    nord: String,
    ost: String,
    postnummer: String,
    poststed: String,
    grunnkretsnavn: String,
    soknenavn: String,
    tettstednavn: String,
}

#[derive(Debug)]
struct Columns {
    localid: usize,
    kommunenavn: usize,
    adressetype: usize,
    adressetilleggsnavn: usize,
    adressenavn: usize,
    nummer: usize,
    bokstav: usize,
    undernummer: usize,
    adresse_tekst: usize,
    epsg_kode: usize,
    nord: usize,
    ost: usize,
    postnummer: usize,
    poststed: usize,
    grunnkretsnavn: usize,
    soknenavn: usize,
    tettstednavn: usize,
    adresse_id: usize,
    uuid_adresse: usize,
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
        Some(
            PgPool::connect(db_url)
                .await
                .context("failed to connect to PostgreSQL")?,
        )
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
        .unwrap_or("matrikkelenAdresse.csv")
        .to_string();

    let file = File::open(&opts.input)
        .with_context(|| format!("failed to open {}", opts.input.display()))?;
    let mut reader = ReaderBuilder::new()
        .delimiter(b';')
        .flexible(true)
        .from_reader(file);
    let headers = reader
        .headers()
        .context("failed to read CSV headers")?
        .clone();
    let columns = Columns::from_headers(&headers)?;

    let mut totals = Totals::default();
    let mut buffer = Vec::with_capacity(opts.batch_size);

    for record in reader.records() {
        let record = record.with_context(|| {
            format!(
                "failed to parse CSV row near record {} in {}",
                totals.rows_parsed + totals.rows_skipped + 1,
                opts.input.display()
            )
        })?;
        let source = SourceRecord::from_csv(&columns, &record);

        match to_row(source, &source_dataset, run_marker, opts.include_matrikkel) {
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

        if let Some(limit) = opts.limit
            && totals.rows_parsed >= limit
        {
            break;
        }
    }

    if !buffer.is_empty() {
        let (inserted, deduped) = flush_batch(pool.as_ref(), &mut buffer, opts.dry_run).await?;
        totals.rows_inserted += inserted;
        totals.rows_deduped += deduped;
    }

    if !opts.dry_run
        && opts.limit.is_none()
        && let Some(pool) = pool.as_ref()
    {
        deactivate_missing_rows(pool, &source_dataset, run_marker).await?;
    }

    println!(
        "done input={} parsed={} inserted={} deduped={} skipped={} mode={} matrikkel={}",
        opts.input.display(),
        totals.rows_parsed,
        totals.rows_inserted,
        totals.rows_deduped,
        totals.rows_skipped,
        if opts.dry_run { "dry-run" } else { "import" },
        opts.include_matrikkel
    );

    Ok(())
}

impl Columns {
    fn from_headers(headers: &StringRecord) -> Result<Self> {
        let positions = headers
            .iter()
            .enumerate()
            .map(|(index, header)| (strip_bom(header).to_string(), index))
            .collect::<HashMap<_, _>>();

        Ok(Self {
            localid: column_index(&positions, "lokalid")?,
            kommunenavn: column_index(&positions, "kommunenavn")?,
            adressetype: column_index(&positions, "adressetype")?,
            adressetilleggsnavn: column_index(&positions, "adressetilleggsnavn")?,
            adressenavn: column_index(&positions, "adressenavn")?,
            nummer: column_index(&positions, "nummer")?,
            bokstav: column_index(&positions, "bokstav")?,
            undernummer: column_index(&positions, "undernummer")?,
            adresse_tekst: column_index(&positions, "adresseTekst")?,
            epsg_kode: column_index(&positions, "EPSG-kode")?,
            nord: column_index(&positions, "Nord")?,
            ost: column_index(&positions, "Øst")?,
            postnummer: column_index(&positions, "postnummer")?,
            poststed: column_index(&positions, "poststed")?,
            grunnkretsnavn: column_index(&positions, "grunnkretsnavn")?,
            soknenavn: column_index(&positions, "soknenavn")?,
            tettstednavn: column_index(&positions, "tettstednavn")?,
            adresse_id: column_index(&positions, "adresseId")?,
            uuid_adresse: column_index(&positions, "uuidAdresse")?,
        })
    }
}

impl SourceRecord {
    fn from_csv(columns: &Columns, record: &StringRecord) -> Self {
        Self {
            localid: cell(record, columns.localid),
            kommunenavn: cell(record, columns.kommunenavn),
            adressetype: cell(record, columns.adressetype),
            adressetilleggsnavn: cell(record, columns.adressetilleggsnavn),
            adressenavn: cell(record, columns.adressenavn),
            nummer: cell(record, columns.nummer),
            bokstav: cell(record, columns.bokstav),
            undernummer: cell(record, columns.undernummer),
            adresse_tekst: cell(record, columns.adresse_tekst),
            epsg_kode: cell(record, columns.epsg_kode),
            nord: cell(record, columns.nord),
            ost: cell(record, columns.ost),
            postnummer: cell(record, columns.postnummer),
            poststed: cell(record, columns.poststed),
            grunnkretsnavn: cell(record, columns.grunnkretsnavn),
            soknenavn: cell(record, columns.soknenavn),
            tettstednavn: cell(record, columns.tettstednavn),
            adresse_id: cell(record, columns.adresse_id),
            uuid_adresse: cell(record, columns.uuid_adresse),
        }
    }
}

fn to_row(
    record: SourceRecord,
    source_dataset: &str,
    run_marker: i64,
    include_matrikkel: bool,
) -> Option<AddressRow> {
    let address_type = to_opt_string(&record.adressetype)?;
    let is_vegadresse = address_type.eq_ignore_ascii_case("vegadresse");
    let is_matrikkel = address_type.eq_ignore_ascii_case("matrikkeladresse");
    if !is_vegadresse && !(include_matrikkel && is_matrikkel) {
        return None;
    }

    let source_hash = first_non_empty([
        record.uuid_adresse.as_str(),
        record.adresse_id.as_str(),
        record.localid.as_str(),
    ])?;

    let admin_area = normalize_name_case_if_uppercase(&record.kommunenavn);
    let locality = normalize_name_case_if_uppercase(&record.poststed);
    let admin_area = admin_area.filter(|value| Some(value.as_str()) != locality.as_deref());
    let thoroughfare = if is_vegadresse {
        clean_thoroughfare(to_opt_string(&record.adressenavn).as_deref())
    } else {
        clean_thoroughfare(to_opt_string(&record.adressetilleggsnavn).as_deref())
    };

    let raw_premise = if is_vegadresse {
        compose_vegadresse_premise(&record.nummer, &record.bokstav)
    } else {
        to_opt_string(&record.adresse_tekst)
            .and_then(|value| value.split(',').next().map(str::trim).map(str::to_string))
    };
    let raw_subpremise = if is_matrikkel {
        to_opt_string(&record.undernummer)
    } else {
        None
    };
    let parsed = normalize_address_parts("NO", raw_premise.as_deref(), raw_subpremise.as_deref());
    let premise = parsed.house_number;
    let premise_type = parsed.house_number_type;
    let subpremise = parsed.unit;

    let dependent_locality = pick_dependent_locality(
        [
            record.tettstednavn.as_str(),
            record.grunnkretsnavn.as_str(),
            record.soknenavn.as_str(),
        ],
        locality.as_deref(),
        admin_area.as_deref(),
    );

    let postal_code = clean_postal_code(&record.postnummer);
    let latitude_longitude =
        project_to_wgs84(&record.epsg_kode, &record.nord, &record.ost).unwrap_or((None, None));

    let full_address = format_display_address(DisplayAddressParts {
        country_code: "NO",
        thoroughfare: thoroughfare.as_deref(),
        house_number: premise.as_deref(),
        unit: subpremise.as_deref(),
        locality: locality.as_deref(),
        dependent_locality: dependent_locality.as_deref(),
        admin_area: admin_area.as_deref(),
        postal_code: postal_code.as_deref(),
    });
    if full_address.is_empty() {
        return None;
    }

    Some(AddressRow {
        source_hash,
        country_code: "NO".to_string(),
        source_dataset: source_dataset.to_string(),
        admin_area,
        locality,
        dependent_locality,
        thoroughfare,
        premise,
        premise_type,
        subpremise,
        postal_code,
        latitude: latitude_longitude.0,
        longitude: latitude_longitude.1,
        full_address: full_address.clone(),
        search_text: normalize_text(&full_address),
        last_seen_run: run_marker,
    })
}

fn column_index(positions: &HashMap<String, usize>, name: &str) -> Result<usize> {
    positions
        .get(name)
        .copied()
        .with_context(|| format!("required column missing: {name}"))
}

fn cell(record: &StringRecord, index: usize) -> String {
    record.get(index).unwrap_or_default().to_string()
}

fn strip_bom(value: &str) -> &str {
    value.trim_start_matches('\u{feff}')
}

fn first_non_empty<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<String> {
    values.into_iter().find_map(to_opt_string)
}

fn to_opt_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn clean_postal_code(value: &str) -> Option<String> {
    let compact = value
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect::<String>();
    (!compact.is_empty()).then_some(compact)
}

fn compose_vegadresse_premise(number: &str, letter: &str) -> Option<String> {
    let number = to_opt_string(number)?;
    let letter = to_opt_string(letter);
    Some(match letter {
        Some(letter) => format!("{number}{letter}"),
        None => number,
    })
}

fn pick_dependent_locality<'a>(
    candidates: impl IntoIterator<Item = &'a str>,
    locality: Option<&str>,
    admin_area: Option<&str>,
) -> Option<String> {
    candidates
        .into_iter()
        .filter_map(to_opt_string)
        .map(|value| normalize_name_case_if_uppercase(&value).unwrap_or(value))
        .find(|value| Some(value.as_str()) != locality && Some(value.as_str()) != admin_area)
}

fn normalize_name_case_if_uppercase(value: &str) -> Option<String> {
    let value = to_opt_string(value)?;
    let has_lowercase = value.chars().any(|ch| ch.is_lowercase());
    if has_lowercase {
        return Some(value);
    }

    let mut result = String::with_capacity(value.len());
    let mut capitalize_next = true;
    for character in value.chars().flat_map(char::to_lowercase) {
        if capitalize_next && character.is_alphabetic() {
            for upper in character.to_uppercase() {
                result.push(upper);
            }
            capitalize_next = false;
        } else {
            result.push(character);
            capitalize_next = matches!(character, ' ' | '-' | '/' | '(');
        }
    }
    Some(result)
}

fn project_to_wgs84(
    epsg_code: &str,
    north: &str,
    east: &str,
) -> Option<(Option<f64>, Option<f64>)> {
    let epsg = to_opt_string(epsg_code)?;
    let zone = epsg
        .strip_prefix("258")?
        .parse::<u8>()
        .ok()
        .filter(|zone| (28..=38).contains(zone))?;
    let northing = to_opt_string(north)?.parse::<f64>().ok()?;
    let easting = to_opt_string(east)?.parse::<f64>().ok()?;
    let (latitude, longitude) = utm_to_wgs84(zone, easting, northing)?;
    Some((Some(latitude), Some(longitude)))
}

fn utm_to_wgs84(zone: u8, easting: f64, northing: f64) -> Option<(f64, f64)> {
    let a: f64 = 6_378_137.0;
    let f: f64 = 1.0 / 298.257_223_563;
    let k0: f64 = 0.9996;
    let e_sq: f64 = f * (2.0 - f);
    let e_prime_sq: f64 = e_sq / (1.0 - e_sq);

    let x = easting - 500_000.0;
    let y = northing;
    let m = y / k0;
    let mu = m / (a * (1.0 - e_sq / 4.0 - 3.0 * e_sq.powi(2) / 64.0 - 5.0 * e_sq.powi(3) / 256.0));

    let e1 = (1.0 - (1.0 - e_sq).sqrt()) / (1.0 + (1.0 - e_sq).sqrt());
    let j1 = 3.0 * e1 / 2.0 - 27.0 * e1.powi(3) / 32.0;
    let j2 = 21.0 * e1.powi(2) / 16.0 - 55.0 * e1.powi(4) / 32.0;
    let j3 = 151.0 * e1.powi(3) / 96.0;
    let j4 = 1097.0 * e1.powi(4) / 512.0;

    let fp = mu
        + j1 * (2.0 * mu).sin()
        + j2 * (4.0 * mu).sin()
        + j3 * (6.0 * mu).sin()
        + j4 * (8.0 * mu).sin();

    let sin_fp = fp.sin();
    let cos_fp = fp.cos();
    let tan_fp = fp.tan();

    let c1 = e_prime_sq * cos_fp.powi(2);
    let t1 = tan_fp.powi(2);
    let n1 = a / (1.0 - e_sq * sin_fp.powi(2)).sqrt();
    let r1 = a * (1.0 - e_sq) / (1.0 - e_sq * sin_fp.powi(2)).powf(1.5);
    let d = x / (n1 * k0);

    let q1 = n1 * tan_fp / r1;
    let q2 = d.powi(2) / 2.0;
    let q3 = (5.0 + 3.0 * t1 + 10.0 * c1 - 4.0 * c1.powi(2) - 9.0 * e_prime_sq) * d.powi(4) / 24.0;
    let q4 =
        (61.0 + 90.0 * t1 + 298.0 * c1 + 45.0 * t1.powi(2) - 252.0 * e_prime_sq - 3.0 * c1.powi(2))
            * d.powi(6)
            / 720.0;

    let latitude = fp - q1 * (q2 - q3 + q4);

    let q5 = d;
    let q6 = (1.0 + 2.0 * t1 + c1) * d.powi(3) / 6.0;
    let q7 = (5.0 - 2.0 * c1 + 28.0 * t1 - 3.0 * c1.powi(2) + 8.0 * e_prime_sq + 24.0 * t1.powi(2))
        * d.powi(5)
        / 120.0;
    let longitude =
        ((zone as f64 - 1.0) * 6.0 - 180.0 + 3.0).to_radians() + (q5 - q6 + q7) / cos_fp;

    Some((latitude.to_degrees(), longitude.to_degrees()))
}

async fn flush_batch(
    pool: Option<&PgPool>,
    rows: &mut Vec<AddressRow>,
    dry_run: bool,
) -> Result<(usize, usize)> {
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

    let result = qb
        .build()
        .execute(pool)
        .await
        .context("failed bulk insert")?;
    Ok(result.rows_affected() as usize)
}

async fn deactivate_missing_rows(
    pool: &PgPool,
    source_dataset: &str,
    run_marker: i64,
) -> Result<()> {
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
    let mut input = PathBuf::from("norway_data/matrikkelenAdresse.csv");
    let mut database_url = env::var("DATABASE_URL").ok();
    let mut batch_size = DEFAULT_BATCH_SIZE;
    let mut truncate = false;
    let mut limit = None;
    let mut dry_run = false;
    let mut include_matrikkel = false;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => input = PathBuf::from(args.next().context("missing value for --input")?),
            "--database-url" => {
                database_url = Some(args.next().context("missing value for --database-url")?)
            }
            "--batch-size" => {
                let value = args.next().context("missing value for --batch-size")?;
                batch_size = value
                    .parse::<usize>()
                    .with_context(|| format!("invalid --batch-size: {value}"))?;
            }
            "--limit" => {
                let value = args.next().context("missing value for --limit")?;
                limit = Some(
                    value
                        .parse::<usize>()
                        .with_context(|| format!("invalid --limit: {value}"))?,
                );
            }
            "--truncate" => truncate = true,
            "--dry-run" => dry_run = true,
            "--include-matrikkel" => include_matrikkel = true,
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
        bail!(
            "--batch-size is too large for PostgreSQL parameter limits (max: {MAX_SAFE_BATCH_SIZE})"
        );
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
        include_matrikkel,
    })
}

fn print_help() {
    println!(
        "Usage: cargo run --bin etl_no_csv -- [options]\n\n\
Options:\n\
  --input <path>              Input CSV file (default: norway_data/matrikkelenAdresse.csv)\n\
  --database-url <url>        PostgreSQL DSN (or set DATABASE_URL env var)\n\
  --batch-size <n>            Rows per INSERT batch (default: 4000, max: 4095)\n\
  --limit <n>                 Stop after n valid parsed rows\n\
  --truncate                  Truncate addresses before import\n\
  --dry-run                   Parse and transform only, no DB connection/writes\n\
  --include-matrikkel         Include cadastral-only matrikkeladresse rows\n\
  -h, --help                  Show help\n"
    );
}

fn current_run_marker() -> Result<i64> {
    use std::time::{SystemTime, UNIX_EPOCH};

    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::{SourceRecord, normalize_name_case_if_uppercase, project_to_wgs84, to_row};

    #[test]
    fn converts_epsg_25833_to_wgs84() {
        let converted = project_to_wgs84("25833", "6587871.65", "196541.04")
            .expect("coordinates should convert");
        let latitude = converted.0.expect("latitude");
        let longitude = converted.1.expect("longitude");

        assert!((latitude - 59.32019870).abs() < 0.00001);
        assert!((longitude - 9.66478811).abs() < 0.00001);
    }

    #[test]
    fn converts_negative_easting_values() {
        let converted = project_to_wgs84("25833", "6524502.45", "-28442.41")
            .expect("coordinates should convert");
        let latitude = converted.0.expect("latitude");
        let longitude = converted.1.expect("longitude");

        assert!(
            (latitude - 58.53803109).abs() < 0.00001,
            "latitude={latitude}"
        );
        assert!(
            (longitude - 5.90585870).abs() < 0.0001,
            "longitude={longitude}"
        );
    }

    #[test]
    fn title_cases_uppercase_names() {
        assert_eq!(
            normalize_name_case_if_uppercase("NES I ÅDAL").as_deref(),
            Some("Nes I Ådal")
        );
    }

    #[test]
    fn builds_vegadresse_row_by_default() {
        let row = to_row(
            SourceRecord {
                localid: "1".to_string(),
                adresse_id: "1".to_string(),
                uuid_adresse: "uuid-1".to_string(),
                kommunenavn: "SILJAN".to_string(),
                adressetype: "vegadresse".to_string(),
                adressetilleggsnavn: String::new(),
                adressenavn: "Grorudveien".to_string(),
                nummer: "88".to_string(),
                bokstav: String::new(),
                undernummer: String::new(),
                adresse_tekst: "Grorudveien 88".to_string(),
                epsg_kode: "25833".to_string(),
                nord: "6587871.65".to_string(),
                ost: "196541.04".to_string(),
                postnummer: "3748".to_string(),
                poststed: "SILJAN".to_string(),
                grunnkretsnavn: "Opdalen".to_string(),
                soknenavn: "Siljan".to_string(),
                tettstednavn: "Siljan".to_string(),
            },
            "matrikkelenAdresse.csv",
            123,
            false,
        )
        .expect("vegadresse row");

        assert_eq!(row.country_code, "NO");
        assert_eq!(row.admin_area, None);
        assert_eq!(row.locality.as_deref(), Some("Siljan"));
        assert_eq!(row.dependent_locality.as_deref(), Some("Opdalen"));
        assert_eq!(row.thoroughfare.as_deref(), Some("Grorudveien"));
        assert_eq!(row.premise.as_deref(), Some("88"));
        assert_eq!(row.postal_code.as_deref(), Some("3748"));
        assert_eq!(
            row.full_address,
            "Grorudveien 88, Opdalen, Siljan, 3748, NO"
        );
    }

    #[test]
    fn skips_matrikkel_by_default() {
        let row = to_row(
            SourceRecord {
                localid: "1".to_string(),
                adresse_id: "1".to_string(),
                uuid_adresse: "uuid-1".to_string(),
                kommunenavn: "HÅ".to_string(),
                adressetype: "matrikkeladresse".to_string(),
                adressetilleggsnavn: "Ualand".to_string(),
                adressenavn: String::new(),
                nummer: String::new(),
                bokstav: String::new(),
                undernummer: "1".to_string(),
                adresse_tekst: "123/4-1".to_string(),
                epsg_kode: "25833".to_string(),
                nord: "6526419.93".to_string(),
                ost: "-31176.52".to_string(),
                postnummer: "4363".to_string(),
                poststed: "BRUSAND".to_string(),
                grunnkretsnavn: "Ogna".to_string(),
                soknenavn: "Ogna".to_string(),
                tettstednavn: String::new(),
            },
            "matrikkelenAdresse.csv",
            123,
            false,
        );

        assert!(row.is_none());
    }
}
