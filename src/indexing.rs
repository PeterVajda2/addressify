use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value as JsonValue;
use tantivy::schema::{STORED, STRING, Schema, TEXT, TantivyDocument};
use tantivy::{Index, IndexWriter, ReloadPolicy};
use tempfile::TempDir;

use crate::AppResult;
use crate::models::Address;
use crate::normalize::normalize_text;
use crate::search::{AddressIndex, AddressIndexes, IndexFields, IndexStorage};

pub fn build_indices_from_postgres(
    country_codes: &[String],
) -> AppResult<(AddressIndexes, Vec<(String, usize)>)> {
    let mut by_country = HashMap::new();
    let mut indexed_counts = Vec::new();

    for country_code in country_codes {
        let country_started = Instant::now();
        let (index, indexed_count) = build_index_from_postgres(country_code)?;
        println!(
            "Built {country_code} index with {indexed_count} addresses in {:.2?}.",
            country_started.elapsed()
        );
        by_country.insert(country_code.clone(), Arc::new(index));
        indexed_counts.push((country_code.clone(), indexed_count));
    }

    Ok((AddressIndexes { by_country }, indexed_counts))
}

fn build_index_from_postgres(country_code: &str) -> AppResult<(AddressIndex, usize)> {
    let index_dir = TempDir::new()?;
    let (schema, fields) = address_schema();
    let index = Index::create_in_dir(&index_dir, schema)?;
    let mut writer = index.writer(100_000_000)?;
    let indexed_count = stream_postgres_addresses(country_code, &mut writer, fields)?;
    writer.commit()?;

    Ok((
        AddressIndex {
            _storage: IndexStorage::Temp {
                _temp_dir: index_dir,
            },
            reader: build_reader(&index)?,
            fields,
        },
        indexed_count,
    ))
}

pub fn build_indices_to_dir(
    country_codes: &[String],
    index_root: &Path,
) -> AppResult<Vec<(String, usize)>> {
    fs::create_dir_all(index_root)?;
    let mut indexed_counts = Vec::new();

    for country_code in country_codes {
        let country_started = Instant::now();
        let country_dir = country_index_dir(index_root, country_code);
        build_index_to_dir(country_code, &country_dir)?;
        let indexed_count = open_index_from_dir(country_code, &country_dir)?.doc_count() as usize;

        println!(
            "Built {country_code} index with {indexed_count} addresses at {} in {:.2?}.",
            country_dir.display(),
            country_started.elapsed()
        );
        indexed_counts.push((country_code.clone(), indexed_count));
    }

    Ok(indexed_counts)
}

pub fn load_indices_from_dir(
    country_codes: &[String],
    index_root: &Path,
) -> AppResult<AddressIndexes> {
    let mut by_country = HashMap::new();

    for country_code in country_codes {
        let country_dir = country_index_dir(index_root, country_code);
        let index = open_index_from_dir(country_code, &country_dir)?;
        by_country.insert(country_code.clone(), Arc::new(index));
    }

    Ok(AddressIndexes { by_country })
}

fn build_index_to_dir(country_code: &str, index_dir: &Path) -> AppResult<()> {
    if index_dir.exists() {
        fs::remove_dir_all(index_dir)?;
    }
    fs::create_dir_all(index_dir)?;

    let (schema, fields) = address_schema();
    let index = Index::create_in_dir(index_dir, schema)?;
    let mut writer = index.writer(100_000_000)?;
    let indexed_count = stream_postgres_addresses(country_code, &mut writer, fields)?;
    writer.commit()?;

    println!("Indexed {indexed_count} active {country_code} addresses.");
    Ok(())
}

fn open_index_from_dir(country_code: &str, index_dir: &Path) -> AppResult<AddressIndex> {
    if !index_dir.exists() {
        return Err(format!(
            "missing index for {country_code} at {}. Run `addresswise build-indexes` first.",
            index_dir.display()
        )
        .into());
    }

    let index = Index::open_in_dir(index_dir)?;
    let fields = index_fields(index.schema())?;

    Ok(AddressIndex {
        _storage: IndexStorage::Persistent {
            _path: index_dir.to_path_buf(),
        },
        reader: build_reader(&index)?,
        fields,
    })
}

pub fn address_schema() -> (Schema, IndexFields) {
    let mut schema_builder = Schema::builder();
    let country_code = schema_builder.add_text_field("country_code", STRING | STORED);
    let admin_area = schema_builder.add_text_field("admin_area", STORED);
    let locality = schema_builder.add_text_field("locality", STORED);
    let dependent_locality = schema_builder.add_text_field("dependent_locality", STORED);
    // Street-only autocomplete must not search the full address text: otherwise a
    // match in a locality or postal code can suggest an unrelated street.
    let thoroughfare = schema_builder.add_text_field("thoroughfare", TEXT | STORED);
    let premise = schema_builder.add_text_field("premise", STORED);
    let premise_type = schema_builder.add_text_field("premise_type", STORED);
    let subpremise = schema_builder.add_text_field("subpremise", STORED);
    let postal_code = schema_builder.add_text_field("postal_code", STORED);
    let full_address = schema_builder.add_text_field("full_address", STORED);
    let search_text = schema_builder.add_text_field("search_text", TEXT);
    let street_search_text = schema_builder.add_text_field("street_search_text", TEXT);
    let schema = schema_builder.build();

    (
        schema,
        IndexFields {
            country_code,
            admin_area,
            locality,
            dependent_locality,
            thoroughfare,
            premise,
            premise_type,
            subpremise,
            postal_code,
            full_address,
            search_text,
            street_search_text,
        },
    )
}

fn index_fields(schema: Schema) -> AppResult<IndexFields> {
    Ok(IndexFields {
        country_code: schema.get_field("country_code")?,
        admin_area: schema.get_field("admin_area")?,
        locality: schema.get_field("locality")?,
        dependent_locality: schema.get_field("dependent_locality")?,
        thoroughfare: schema.get_field("thoroughfare")?,
        premise: schema.get_field("premise")?,
        premise_type: schema.get_field("premise_type")?,
        subpremise: schema.get_field("subpremise")?,
        postal_code: schema.get_field("postal_code")?,
        full_address: schema.get_field("full_address")?,
        search_text: schema.get_field("search_text")?,
        street_search_text: schema.get_field("street_search_text")?,
    })
}

fn build_reader(index: &Index) -> AppResult<tantivy::IndexReader> {
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    reader.reload()?;
    Ok(reader)
}

fn stream_postgres_addresses(
    country_code: &str,
    writer: &mut IndexWriter,
    fields: IndexFields,
) -> AppResult<usize> {
    let sql = address_copy_sql(country_code);
    let mut child = postgres_command(&sql)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let stdout = child.stdout.take().ok_or("failed to capture psql stdout")?;
    let reader = BufReader::new(stdout);
    let mut indexed_count = 0usize;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let address = address_from_json_line(&line)?;
        writer.add_document(tantivy_document(&address, fields))?;
        indexed_count += 1;

        if indexed_count % 100_000 == 0 {
            println!("indexed {indexed_count} addresses...");
        }
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(format!("psql exited with status {status}").into());
    }

    Ok(indexed_count)
}

fn postgres_command(sql: &str) -> Command {
    let psql_bin = env::var("PSQL_BIN").unwrap_or_else(|_| String::from("psql"));
    let mut command = Command::new(psql_bin);
    command.args(["-qAt", "-c", sql]);

    if let Ok(database_url) = env::var("DATABASE_URL") {
        command.args(["-d", database_url.as_str()]);
    }

    command
}

fn country_index_dir(index_root: &Path, country_code: &str) -> PathBuf {
    index_root.join(country_code.to_lowercase())
}

fn address_copy_sql(country_code: &str) -> String {
    let limit_clause = env::var("INDEX_LIMIT")
        .ok()
        .and_then(|limit| limit.parse::<u64>().ok())
        .map(|limit| format!(" limit {limit}"))
        .unwrap_or_default();

    format!(
        "copy (
            select json_build_object(
                'country_code', trim(country_code),
                'admin_area', admin_area,
                'locality', locality,
                'dependent_locality', dependent_locality,
                'thoroughfare', thoroughfare,
                'premise', premise,
                'premise_type', premise_type,
                'subpremise', subpremise,
                'postal_code', postal_code,
                'full_address', full_address,
                'search_text', search_text
            )::text
            from addresses
            where country_code = '{}' and is_active
            order by id
            {limit_clause}
        ) to stdout",
        sql_literal(country_code)
    )
}

fn sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn address_from_json_line(line: &str) -> AppResult<Address> {
    let value: JsonValue = serde_json::from_str(line)?;
    let country_code = json_required_string(&value, "country_code")?;
    let full_address = json_required_string(&value, "full_address")?;
    let search_text = json_required_string(&value, "search_text")?;

    Ok(Address::from_parts(
        crate::models::StructuredAddress {
            country_code,
            admin_area: json_optional_string(&value, "admin_area"),
            locality: json_optional_string(&value, "locality"),
            dependent_locality: json_optional_string(&value, "dependent_locality"),
            thoroughfare: json_optional_string(&value, "thoroughfare"),
            premise: json_optional_string(&value, "premise"),
            premise_type: json_optional_string(&value, "premise_type"),
            subpremise: json_optional_string(&value, "subpremise"),
            postal_code: json_optional_string(&value, "postal_code"),
            full_address,
        },
        search_text,
    ))
}

fn json_required_string(value: &JsonValue, key: &str) -> AppResult<String> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .map(String::from)
        .ok_or_else(|| format!("address row missing string field {key}").into())
}

fn json_optional_string(value: &JsonValue, key: &str) -> Option<String> {
    value.get(key).and_then(JsonValue::as_str).map(String::from)
}

fn tantivy_document(address: &Address, fields: IndexFields) -> TantivyDocument {
    let mut document = TantivyDocument::default();
    document.add_text(fields.country_code, &address.country_code);
    add_optional_text(&mut document, fields.admin_area, &address.admin_area);
    add_optional_text(&mut document, fields.locality, &address.locality);
    add_optional_text(
        &mut document,
        fields.dependent_locality,
        &address.dependent_locality,
    );
    add_optional_text(&mut document, fields.thoroughfare, &address.thoroughfare);
    add_optional_text(&mut document, fields.premise, &address.premise);
    add_optional_text(&mut document, fields.premise_type, &address.premise_type);
    add_optional_text(&mut document, fields.subpremise, &address.subpremise);
    add_optional_text(&mut document, fields.postal_code, &address.postal_code);
    document.add_text(fields.full_address, address.formatted());
    document.add_text(fields.search_text, &address.search_text);
    if let Some(thoroughfare) = &address.thoroughfare {
        document.add_text(fields.street_search_text, normalize_text(thoroughfare));
    }
    document
}

fn add_optional_text(
    document: &mut TantivyDocument,
    field: tantivy::schema::Field,
    value: &Option<String>,
) {
    if let Some(value) = value {
        document.add_text(field, value);
    }
}
