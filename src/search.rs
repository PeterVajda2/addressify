use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, FuzzyTermQuery, Occur, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, TantivyDocument, Value};
use tantivy::{DocAddress, IndexReader, Searcher, Term};
use tempfile::TempDir;
use tokio::task::spawn_blocking;

use crate::AppResult;
use crate::models::{Address, SearchResult, StructuredAddress};
use crate::normalize::normalize_text;

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
    pub street_search_text: Field,
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
        let normalized_query = normalize_text(user_input);
        if normalized_query.is_empty() {
            return Ok(Vec::new());
        }

        let searcher = self.reader.searcher();
        if is_single_character(&normalized_query) {
            return first_indexed_addresses(&searcher, self.fields, limit);
        }

        let query = autocomplete_query(self.fields.search_text, &normalized_query);
        search_tantivy(&searcher, &query, self.fields, limit)
    }

    pub fn search_streets(
        &self,
        user_input: &str,
        limit: usize,
    ) -> tantivy::Result<Vec<SearchResult>> {
        let normalized_query = normalize_text(user_input);
        if normalized_query.is_empty() {
            return Ok(Vec::new());
        }

        let searcher = self.reader.searcher();
        if is_single_character(&normalized_query) {
            return first_indexed_streets(&searcher, self.fields, limit);
        }

        let query = autocomplete_query(self.fields.street_search_text, &normalized_query);
        // Fetch every matching address before deduplicating: a populous street can
        // otherwise consume the entire address result limit with house numbers.
        let candidate_limit = usize::try_from(searcher.num_docs()).unwrap_or(usize::MAX);
        search_tantivy_streets(&searcher, &query, self.fields, candidate_limit, limit)
    }

    pub fn doc_count(&self) -> u64 {
        self.reader.searcher().num_docs()
    }
}

fn is_single_character(query: &str) -> bool {
    query.chars().count() == 1
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
    street_only: bool,
) -> AppResult<Vec<SearchResult>> {
    if let Some(country) = country {
        let index = indexes
            .by_country
            .get(&country)
            .cloned()
            .ok_or_else(|| format!("unknown country {country}"))?;
        return if street_only {
            search_streets_async(index, query, limit).await
        } else {
            search_async(index, query, limit).await
        };
    }

    let mut results = Vec::new();
    for index in indexes.by_country.values() {
        let country_results = if street_only {
            search_streets_async(Arc::clone(index), query.clone(), limit).await?
        } else {
            search_async(Arc::clone(index), query.clone(), limit).await?
        };
        results.extend(country_results);
    }
    if street_only {
        sort_streets_alphabetically(&mut results);
    } else {
        results.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.formatted.cmp(&right.formatted))
                .then_with(|| left.country_code.cmp(&right.country_code))
        });
    }
    results.truncate(limit);

    Ok(results)
}

async fn search_streets_async(
    index: Arc<AddressIndex>,
    query: String,
    limit: usize,
) -> AppResult<Vec<SearchResult>> {
    let result = spawn_blocking(move || index.search_streets(&query, limit))
        .await
        .map_err(|error| format!("blocking search task failed: {error}"))?;
    result.map_err(Into::into)
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

/// Returns the first live documents in index order without evaluating a query.
/// This is intentionally used for one-character autocomplete, where relevance is
/// less useful than a response that is effectively immediate.
fn first_indexed_addresses(
    searcher: &Searcher,
    fields: IndexFields,
    limit: usize,
) -> tantivy::Result<Vec<SearchResult>> {
    let mut results = Vec::with_capacity(limit);

    for (segment_ord, segment_reader) in searcher.segment_readers().iter().enumerate() {
        for doc_id in 0..segment_reader.max_doc() {
            if segment_reader.is_deleted(doc_id) {
                continue;
            }
            let document: TantivyDocument =
                searcher.doc(DocAddress::new(segment_ord as u32, doc_id))?;
            let Some(address) = address_from_tantivy_doc(&document, fields) else {
                continue;
            };
            results.push(SearchResult {
                formatted: address.formatted().to_string(),
                score: 0.0,
                country_code: address.country_code.clone(),
                address: address.structured(),
            });
            if results.len() == limit {
                return Ok(results);
            }
        }
    }

    Ok(results)
}

fn first_indexed_streets(
    searcher: &Searcher,
    fields: IndexFields,
    limit: usize,
) -> tantivy::Result<Vec<SearchResult>> {
    let mut streets = Vec::with_capacity(limit);
    let mut seen = HashSet::new();

    for (segment_ord, segment_reader) in searcher.segment_readers().iter().enumerate() {
        for doc_id in 0..segment_reader.max_doc() {
            if segment_reader.is_deleted(doc_id) {
                continue;
            }
            let document: TantivyDocument =
                searcher.doc(DocAddress::new(segment_ord as u32, doc_id))?;
            let Some(address) = address_from_tantivy_doc(&document, fields) else {
                continue;
            };
            let Some(thoroughfare) = address
                .thoroughfare
                .filter(|street| !street.trim().is_empty())
            else {
                continue;
            };
            let key = (address.country_code.clone(), normalize_text(&thoroughfare));
            if !seen.insert(key) {
                continue;
            }

            streets.push(SearchResult {
                formatted: thoroughfare.clone(),
                score: 0.0,
                country_code: address.country_code.clone(),
                address: StructuredAddress {
                    country_code: address.country_code,
                    admin_area: None,
                    locality: None,
                    dependent_locality: None,
                    thoroughfare: Some(thoroughfare.clone()),
                    premise: None,
                    premise_type: None,
                    subpremise: None,
                    postal_code: None,
                    full_address: thoroughfare,
                },
            });
            if streets.len() == limit {
                return Ok(streets);
            }
        }
    }

    Ok(streets)
}

fn search_tantivy_streets(
    searcher: &Searcher,
    query: &dyn Query,
    fields: IndexFields,
    candidate_limit: usize,
    limit: usize,
) -> tantivy::Result<Vec<SearchResult>> {
    let top_docs = searcher.search(
        query,
        &TopDocs::with_limit(candidate_limit).order_by_score(),
    )?;
    let mut streets = Vec::with_capacity(limit);
    let mut seen = HashSet::new();

    for (score, doc_address) in top_docs {
        let retrieved_doc: TantivyDocument = searcher.doc(doc_address)?;
        let Some(address) = address_from_tantivy_doc(&retrieved_doc, fields) else {
            continue;
        };
        let Some(thoroughfare) = address
            .thoroughfare
            .filter(|street| !street.trim().is_empty())
        else {
            continue;
        };
        let key = (address.country_code.clone(), normalize_text(&thoroughfare));
        if !seen.insert(key) {
            continue;
        }

        streets.push(SearchResult {
            formatted: thoroughfare.clone(),
            score,
            country_code: address.country_code.clone(),
            address: StructuredAddress {
                country_code: address.country_code,
                admin_area: None,
                locality: None,
                dependent_locality: None,
                thoroughfare: Some(thoroughfare.clone()),
                premise: None,
                premise_type: None,
                subpremise: None,
                postal_code: None,
                full_address: thoroughfare,
            },
        });
    }

    sort_streets_alphabetically(&mut streets);
    streets.truncate(limit);

    Ok(streets)
}

fn sort_streets_alphabetically(streets: &mut [SearchResult]) {
    streets.sort_by(|left, right| {
        normalize_text(&left.formatted)
            .cmp(&normalize_text(&right.formatted))
            .then_with(|| left.formatted.cmp(&right.formatted))
            .then_with(|| left.country_code.cmp(&right.country_code))
    });
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tantivy::schema::TantivyDocument;
    use tantivy::{Index, ReloadPolicy};

    use super::{AddressIndex, IndexStorage};
    use crate::indexing::address_schema;
    use crate::normalize::normalize_text;

    #[test]
    fn normalize_query_folds_accents_and_symbols() {
        assert_eq!(normalize_text("Banská-Bystrica 15"), "banska bystrica 15");
    }

    #[test]
    fn street_autocomplete_does_not_match_a_locality() {
        let (schema, fields) = address_schema();
        let index = Index::create_in_ram(schema);
        let mut writer = index.writer(15_000_000).unwrap();

        for (street, locality) in [("Alpha Road", "Beta"), ("Baker Street", "Alpha City")] {
            let mut document = TantivyDocument::default();
            document.add_text(fields.country_code, "SK");
            document.add_text(fields.thoroughfare, street);
            document.add_text(fields.street_search_text, normalize_text(street));
            document.add_text(fields.locality, locality);
            document.add_text(fields.full_address, format!("{street}, {locality}, SK"));
            document.add_text(
                fields.search_text,
                format!("{street} {locality} sk").to_lowercase(),
            );
            writer.add_document(document).unwrap();
        }
        writer.commit().unwrap();

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        reader.reload().unwrap();
        let address_index = AddressIndex {
            _storage: IndexStorage::Persistent {
                _path: PathBuf::new(),
            },
            reader,
            fields,
        };

        let streets = address_index.search_streets("al", 10).unwrap();
        assert_eq!(streets.len(), 1);
        assert_eq!(streets[0].formatted, "Alpha Road");
    }

    #[test]
    fn single_character_street_autocomplete_uses_first_indexed_streets() {
        let (schema, fields) = address_schema();
        let index = Index::create_in_ram(schema);
        let mut writer = index.writer(15_000_000).unwrap();

        for street in ["Alpha Road", "Baker Street"] {
            let mut document = TantivyDocument::default();
            document.add_text(fields.country_code, "SK");
            document.add_text(fields.thoroughfare, street);
            document.add_text(fields.street_search_text, normalize_text(street));
            document.add_text(fields.full_address, format!("{street}, SK"));
            document.add_text(fields.search_text, normalize_text(street));
            writer.add_document(document).unwrap();
        }
        writer.commit().unwrap();

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        reader.reload().unwrap();
        let address_index = AddressIndex {
            _storage: IndexStorage::Persistent {
                _path: PathBuf::new(),
            },
            reader,
            fields,
        };

        let streets = address_index.search_streets("z", 2).unwrap();
        assert_eq!(streets.len(), 2);
        assert_eq!(streets[0].formatted, "Alpha Road");
        assert_eq!(streets[1].formatted, "Baker Street");
    }

    #[test]
    fn street_autocomplete_folds_diacritics() {
        let (schema, fields) = address_schema();
        let index = Index::create_in_ram(schema);
        let mut writer = index.writer(15_000_000).unwrap();
        let mut document = TantivyDocument::default();
        document.add_text(fields.country_code, "SK");
        document.add_text(fields.thoroughfare, "Na paseká");
        document.add_text(fields.street_search_text, "na paseka");
        document.add_text(fields.full_address, "Na paseká, SK");
        document.add_text(fields.search_text, "na paseka sk");
        writer.add_document(document).unwrap();
        writer.commit().unwrap();

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        reader.reload().unwrap();
        let address_index = AddressIndex {
            _storage: IndexStorage::Persistent {
                _path: PathBuf::new(),
            },
            reader,
            fields,
        };

        let streets = address_index.search_streets("Na paseka", 10).unwrap();
        assert_eq!(streets.len(), 1);
        assert_eq!(streets[0].formatted, "Na paseká");
    }
}
