use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, FuzzyTermQuery, Occur, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, TantivyDocument, Value};
use tantivy::{IndexReader, Searcher, Term};
use tempfile::TempDir;
use tokio::task::spawn_blocking;

use crate::AppResult;
use crate::models::{Address, SearchResult, StructuredAddress};

#[derive(Clone, Copy)]
pub struct IndexFields {
    pub country_code: Field,
    pub admin_area: Field,
    pub locality: Field,
    pub dependent_locality: Field,
    pub thoroughfare: Field,
    pub premise: Field,
    pub premise_type: Field,
    pub subpremise: Field,
    pub postal_code: Field,
    pub full_address: Field,
    pub search_text: Field,
}

pub struct AddressIndex {
    pub _storage: IndexStorage,
    pub reader: IndexReader,
    pub fields: IndexFields,
}

pub enum IndexStorage {
    Temp { _temp_dir: TempDir },
    Persistent { _path: PathBuf },
}

pub struct AddressIndexes {
    pub by_country: HashMap<String, Arc<AddressIndex>>,
}

impl AddressIndexes {
    pub fn has_country(&self, country: &str) -> bool {
        self.by_country.contains_key(country)
    }

    pub fn country_codes(&self) -> Vec<&str> {
        let mut country_codes = self
            .by_country
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        country_codes.sort_unstable();
        country_codes
    }
}

impl AddressIndex {
    pub fn search(&self, user_input: &str, limit: usize) -> tantivy::Result<Vec<SearchResult>> {
        let normalized_query = normalize_query(user_input);
        if normalized_query.is_empty() {
            return Ok(Vec::new());
        }

        let query = autocomplete_query(self.fields.search_text, &normalized_query);
        let searcher = self.reader.searcher();
        search_tantivy(&searcher, &query, self.fields, limit)
    }

    pub fn doc_count(&self) -> u64 {
        self.reader.searcher().num_docs()
    }
}

pub async fn search_async(
    index: Arc<AddressIndex>,
    query: String,
    limit: usize,
) -> AppResult<Vec<SearchResult>> {
    let result = spawn_blocking(move || index.search(&query, limit))
        .await
        .map_err(|error| format!("blocking search task failed: {error}"))?;
    result.map_err(Into::into)
}

pub async fn search_indexes_async(
    indexes: Arc<AddressIndexes>,
    country: Option<String>,
    query: String,
    limit: usize,
) -> AppResult<Vec<SearchResult>> {
    if let Some(country) = country {
        let index = indexes
            .by_country
            .get(&country)
            .cloned()
            .ok_or_else(|| format!("unknown country {country}"))?;
        return search_async(index, query, limit).await;
    }

    let mut results = Vec::new();
    for index in indexes.by_country.values() {
        results.extend(search_async(Arc::clone(index), query.clone(), limit).await?);
    }
    results.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.formatted.cmp(&right.formatted))
            .then_with(|| left.country_code.cmp(&right.country_code))
    });
    results.truncate(limit);

    Ok(results)
}

fn autocomplete_query(search_field: Field, normalized_query: &str) -> Box<dyn Query> {
    let tokens = normalized_query.split_whitespace().collect::<Vec<_>>();
    let mut subqueries = Vec::new();

    for token in tokens.iter().take(tokens.len().saturating_sub(1)) {
        subqueries.push((
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(search_field, token),
                IndexRecordOption::WithFreqs,
            )) as Box<dyn Query>,
        ));
    }

    if let Some(prefix) = tokens.last() {
        subqueries.push((
            Occur::Must,
            Box::new(FuzzyTermQuery::new_prefix(
                Term::from_field_text(search_field, prefix),
                0,
                true,
            )) as Box<dyn Query>,
        ));
    }

    Box::new(BooleanQuery::new(subqueries))
}

fn search_tantivy(
    searcher: &Searcher,
    query: &dyn Query,
    fields: IndexFields,
    limit: usize,
) -> tantivy::Result<Vec<SearchResult>> {
    let top_docs = searcher.search(query, &TopDocs::with_limit(limit).order_by_score())?;
    let mut results = Vec::with_capacity(top_docs.len());

    for (score, doc_address) in top_docs {
        let retrieved_doc: TantivyDocument = searcher.doc(doc_address)?;
        let Some(address) = address_from_tantivy_doc(&retrieved_doc, fields) else {
            continue;
        };

        results.push(SearchResult {
            formatted: address.formatted().to_string(),
            score,
            country_code: address.country_code.clone(),
            address: address.structured(),
        });
    }

    Ok(results)
}

fn address_from_tantivy_doc(document: &TantivyDocument, fields: IndexFields) -> Option<Address> {
    let country_code = document_string(document, fields.country_code)?;
    let full_address = document_string(document, fields.full_address)?;

    Some(Address::from_parts(
        StructuredAddress {
            country_code,
            admin_area: document_optional_string(document, fields.admin_area),
            locality: document_optional_string(document, fields.locality),
            dependent_locality: document_optional_string(document, fields.dependent_locality),
            thoroughfare: document_optional_string(document, fields.thoroughfare),
            premise: document_optional_string(document, fields.premise),
            premise_type: document_optional_string(document, fields.premise_type),
            subpremise: document_optional_string(document, fields.subpremise),
            postal_code: document_optional_string(document, fields.postal_code),
            full_address,
        },
        String::new(),
    ))
}

fn document_string(document: &TantivyDocument, field: Field) -> Option<String> {
    document
        .get_first(field)
        .and_then(|value| value.as_str())
        .map(String::from)
}

fn document_optional_string(document: &TantivyDocument, field: Field) -> Option<String> {
    document_string(document, field)
}

fn normalize_query(query: &str) -> String {
    query
        .chars()
        .map(fold_char)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn fold_char(ch: char) -> char {
    match ch.to_lowercase().next().unwrap_or(ch) {
        'á' | 'ä' | 'à' | 'â' | 'å' => 'a',
        'č' | 'ć' => 'c',
        'ď' => 'd',
        'é' | 'ě' | 'è' | 'ê' => 'e',
        'í' | 'ì' | 'î' => 'i',
        'ľ' | 'ĺ' => 'l',
        'ň' => 'n',
        'ó' | 'ô' | 'ò' | 'ö' => 'o',
        'ŕ' | 'ř' => 'r',
        'š' => 's',
        'ť' => 't',
        'ú' | 'ů' | 'ù' | 'ü' => 'u',
        'ý' => 'y',
        'ž' => 'z',
        ch if ch.is_ascii_alphanumeric() => ch,
        _ => ' ',
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_query;

    #[test]
    fn normalize_query_folds_accents_and_symbols() {
        assert_eq!(normalize_query("Banská-Bystrica 15"), "banska bystrica 15");
    }
}
