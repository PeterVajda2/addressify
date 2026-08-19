use std::collections::BTreeSet;
use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use quinn::crypto::rustls::QuicServerConfig;
use rand::RngCore;
use rcgen::generate_simple_self_signed;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use xitca_web::{
    App,
    handler::{handler_service, html::Html, json::Json, query::Query, state::StateRef},
    http::{StatusCode, WebRequest, WebResponse, header::CONTENT_TYPE},
    route::{get, post},
};

use crate::AppResult;
use crate::auth::{AuthState, ErrorResponse, error_status, normalize_domain};
use crate::models::{SearchResult, StructuredAddress};
use crate::normalize::normalize_text;
use crate::search::{AddressIndexes, search_indexes_async, validation_candidates_async};

const MAX_WORKERS: usize = 8;
const BLOCKING_THREADS_PER_WORKER: usize = 8;
pub const H3_CERT_PATH: &str = "/tmp/addresswise-h3-cert.der";

pub struct AppState {
    pub indexes: Arc<AddressIndexes>,
    pub auth: AuthState,
    pub demo_api_key: String,
    pub admin_api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: Option<String>,
    country: Option<String>,
    limit: Option<usize>,
    street_only: Option<String>,
    api_key: Option<String>,
}

#[derive(Debug, Serialize)]
struct SearchResponse {
    query: String,
    country: Option<String>,
    count: usize,
    results: Vec<SearchResult>,
}

const VALIDATION_SUGGESTION_THRESHOLD: f64 = 0.90;
const VALIDATION_CANDIDATE_LIMIT: usize = 100;
const VALIDATION_SUGGESTION_LIMIT: usize = 5;

#[derive(Debug, Deserialize)]
struct ValidateParams {
    api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ValidateRequest {
    street: String,
    house_number: String,
    postal_code: String,
    city: String,
    country: String,
}

#[derive(Debug, Clone, Serialize)]
struct ValidationMatch {
    confidence_ratio: f64,
    address: StructuredAddress,
}

#[derive(Debug, Serialize)]
struct ValidationResponse {
    valid: bool,
    confidence_ratio: f64,
    corrected_address: Option<StructuredAddress>,
    suggestions: Vec<ValidationMatch>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
    countries: Vec<String>,
}

#[derive(Debug, Serialize)]
struct AdminApiKey {
    id: i64,
    api_key: String,
    label: Option<String>,
    domains: Vec<String>,
    is_active: bool,
    total_requests: i64,
    last_used_at: Option<String>,
    last_used_domain: Option<String>,
    last_used_ip: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct CreateApiKeyRequest {
    label: Option<String>,
    domains: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateApiKeyRequest {
    id: i64,
    label: Option<String>,
    domains: Vec<String>,
    is_active: bool,
}

#[derive(Debug, Deserialize)]
struct DeleteApiKeyRequest {
    id: i64,
}

pub fn serve_with_state(addr: String, state: Arc<AppState>) -> AppResult<()> {
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .clamp(1, MAX_WORKERS);
    let socket_addr = socket_addr(&addr);
    let h3_config = quic_config()?;

    App::new()
        .with_state(state)
        .at("/admin", get(handler_service(admin_home)))
        .at("/admin/api/keys", get(handler_service(admin_list_keys)))
        .at(
            "/admin/api/keys/create",
            post(handler_service(admin_create_key)),
        )
        .at(
            "/admin/api/keys/update",
            post(handler_service(admin_update_key)),
        )
        .at(
            "/admin/api/keys/delete",
            post(handler_service(admin_delete_key)),
        )
        .at("/health", get(handler_service(health)))
        .at("/search", get(handler_service(search)))
        .at("/suggest", get(handler_service(search)))
        .at("/validate", post(handler_service(validate)))
        .at("/", get(handler_service(home)))
        .serve()
        .worker_threads(workers)
        .worker_max_blocking_threads(BLOCKING_THREADS_PER_WORKER)
        .h2c_prior_knowledge()
        .bind(socket_addr)?
        .bind_h3(socket_addr, h3_config)?
        .run()
        .wait()?;

    Ok(())
}

async fn home(StateRef(state): StateRef<'_, Arc<AppState>>) -> Html<String> {
    let demo_api_key = serde_json::to_string(&state.demo_api_key)
        .expect("serializing a demo API key must succeed");
    Html(include_str!("../static/index.html").replace("__DEMO_API_KEY__", &demo_api_key))
}

async fn admin_home() -> Html<&'static str> {
    Html(include_str!("../static/admin.html"))
}

async fn admin_list_keys(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    req: &WebRequest<()>,
) -> WebResponse {
    if let Err((status, message)) = authorize_admin(state, req) {
        return admin_error(status, message);
    }
    match list_admin_keys(&state.auth).await {
        Ok(keys) => json_ok(keys),
        Err(error) => admin_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn admin_create_key(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Json(request): Json<CreateApiKeyRequest>,
    req: &WebRequest<()>,
) -> WebResponse {
    if let Err((status, message)) = authorize_admin(state, req) {
        return admin_error(status, message);
    }
    let domains = match normalize_domains(request.domains) {
        Ok(domains) => domains,
        Err(error) => return admin_error(StatusCode::BAD_REQUEST, error),
    };
    let label = match normalize_label(request.label) {
        Ok(label) => label,
        Err(error) => return admin_error(StatusCode::BAD_REQUEST, error),
    };
    let Some(pool) = state.auth.pool() else {
        return admin_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "database-backed authentication is disabled",
        );
    };

    let api_key = generate_api_key();
    let mut transaction = match pool.begin().await {
        Ok(transaction) => transaction,
        Err(error) => return database_error(error),
    };
    let id = match sqlx::query_scalar::<_, i64>(
        "insert into api_keys (api_key, label) values ($1, $2) returning id",
    )
    .bind(&api_key)
    .bind(&label)
    .fetch_one(&mut *transaction)
    .await
    {
        Ok(id) => id,
        Err(error) => return database_error(error),
    };
    if let Err(error) = replace_domains(&mut transaction, id, &domains).await {
        return database_error(error);
    }
    if let Err(error) = transaction.commit().await {
        return database_error(error);
    }
    state.auth.clear_authorization_cache();
    match admin_key_by_id(&state.auth, id).await {
        Ok(key) => json_response(StatusCode::CREATED, &key),
        Err(error) => admin_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn admin_update_key(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Json(request): Json<UpdateApiKeyRequest>,
    req: &WebRequest<()>,
) -> WebResponse {
    if let Err((status, message)) = authorize_admin(state, req) {
        return admin_error(status, message);
    }
    let domains = match normalize_domains(request.domains) {
        Ok(domains) => domains,
        Err(error) => return admin_error(StatusCode::BAD_REQUEST, error),
    };
    let label = match normalize_label(request.label) {
        Ok(label) => label,
        Err(error) => return admin_error(StatusCode::BAD_REQUEST, error),
    };
    let Some(pool) = state.auth.pool() else {
        return admin_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "database-backed authentication is disabled",
        );
    };
    let mut transaction = match pool.begin().await {
        Ok(transaction) => transaction,
        Err(error) => return database_error(error),
    };
    let result = match sqlx::query(
        "update api_keys set label = $2, is_active = $3, updated_at = now() where id = $1",
    )
    .bind(request.id)
    .bind(&label)
    .bind(request.is_active)
    .execute(&mut *transaction)
    .await
    {
        Ok(result) => result,
        Err(error) => return database_error(error),
    };
    if result.rows_affected() == 0 {
        return admin_error(StatusCode::NOT_FOUND, "API key was not found");
    }
    if let Err(error) = replace_domains(&mut transaction, request.id, &domains).await {
        return database_error(error);
    }
    if let Err(error) = transaction.commit().await {
        return database_error(error);
    }
    state.auth.clear_authorization_cache();
    match admin_key_by_id(&state.auth, request.id).await {
        Ok(key) => json_ok(key),
        Err(error) => admin_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn admin_delete_key(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Json(request): Json<DeleteApiKeyRequest>,
    req: &WebRequest<()>,
) -> WebResponse {
    if let Err((status, message)) = authorize_admin(state, req) {
        return admin_error(status, message);
    }
    let Some(pool) = state.auth.pool() else {
        return admin_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "database-backed authentication is disabled",
        );
    };
    match sqlx::query("delete from api_keys where id = $1")
        .bind(request.id)
        .execute(pool)
        .await
    {
        Ok(result) if result.rows_affected() == 0 => {
            admin_error(StatusCode::NOT_FOUND, "API key was not found")
        }
        Ok(_) => {
            state.auth.clear_authorization_cache();
            json_ok(serde_json::json!({ "deleted": true }))
        }
        Err(error) => database_error(error),
    }
}

fn authorize_admin(
    state: &AppState,
    req: &WebRequest<()>,
) -> Result<(), (StatusCode, &'static str)> {
    let Some(configured_key) = state.admin_api_key.as_deref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "administration is not configured; set ADMIN_API_KEY and restart the service",
        ));
    };
    let supplied = req
        .headers()
        .get("x-admin-key")
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            req.headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
        });
    if supplied.is_some_and(|value| value == configured_key) {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "a valid admin key is required"))
    }
}

async fn list_admin_keys(auth: &AuthState) -> Result<Vec<AdminApiKey>, String> {
    let pool = auth
        .pool()
        .ok_or_else(|| String::from("database-backed authentication is disabled"))?;
    let rows = sqlx::query(
        "select id, api_key, label, is_active, total_requests,
                to_char(last_used_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as last_used_at,
                last_used_domain, last_used_ip,
                to_char(created_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as created_at,
                to_char(updated_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as updated_at
         from api_keys order by created_at desc, id desc",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| format!("database operation failed: {error}"))?;
    let mut keys = Vec::with_capacity(rows.len());
    for row in rows {
        keys.push(admin_key_from_row(auth, row).await?);
    }
    Ok(keys)
}

async fn admin_key_by_id(auth: &AuthState, id: i64) -> Result<AdminApiKey, String> {
    let pool = auth
        .pool()
        .ok_or_else(|| String::from("database-backed authentication is disabled"))?;
    let row = sqlx::query(
        "select id, api_key, label, is_active, total_requests,
                to_char(last_used_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as last_used_at,
                last_used_domain, last_used_ip,
                to_char(created_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as created_at,
                to_char(updated_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as updated_at
         from api_keys where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("database operation failed: {error}"))?
    .ok_or_else(|| String::from("API key was not found"))?;
    admin_key_from_row(auth, row).await
}

async fn admin_key_from_row(
    auth: &AuthState,
    row: sqlx::postgres::PgRow,
) -> Result<AdminApiKey, String> {
    let pool = auth
        .pool()
        .ok_or_else(|| String::from("database-backed authentication is disabled"))?;
    let id: i64 = row.get("id");
    let domains = sqlx::query_scalar::<_, String>(
        "select domain from api_key_domains where api_key_id = $1 order by domain",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .map_err(|error| format!("database operation failed: {error}"))?;
    Ok(AdminApiKey {
        id,
        api_key: row.get("api_key"),
        label: row.get("label"),
        domains,
        is_active: row.get("is_active"),
        total_requests: row.get("total_requests"),
        last_used_at: row.get("last_used_at"),
        last_used_domain: row.get("last_used_domain"),
        last_used_ip: row.get("last_used_ip"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

async fn replace_domains(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    api_key_id: i64,
    domains: &[String],
) -> Result<(), sqlx::Error> {
    sqlx::query("delete from api_key_domains where api_key_id = $1")
        .bind(api_key_id)
        .execute(&mut **transaction)
        .await?;
    for domain in domains {
        sqlx::query("insert into api_key_domains (api_key_id, domain) values ($1, $2)")
            .bind(api_key_id)
            .bind(domain)
            .execute(&mut **transaction)
            .await?;
    }
    Ok(())
}

fn normalize_domains(domains: Vec<String>) -> Result<Vec<String>, &'static str> {
    let domains = domains
        .into_iter()
        .map(|domain| normalize_domain(&domain).ok_or("each allowed domain must be non-empty"))
        .collect::<Result<BTreeSet<_>, _>>()?
        .into_iter()
        .collect::<Vec<_>>();
    if domains.is_empty() {
        Err("at least one allowed domain is required")
    } else {
        Ok(domains)
    }
}

fn normalize_label(label: Option<String>) -> Result<Option<String>, &'static str> {
    let label = label.and_then(|label| {
        let trimmed = label.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    });
    if label.as_ref().is_some_and(|label| label.len() > 200) {
        Err("label must be at most 200 characters")
    } else {
        Ok(label)
    }
}

fn generate_api_key() -> String {
    let mut bytes = [0_u8; 24];
    rand::rng().fill_bytes(&mut bytes);
    let token = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("aw_live_{token}")
}

fn admin_error(status: StatusCode, message: impl Into<String>) -> WebResponse {
    json_error(
        status,
        ErrorResponse {
            error: "admin_error",
            message: message.into(),
        },
    )
}

fn database_error(error: sqlx::Error) -> WebResponse {
    admin_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("database operation failed: {error}"),
    )
}

async fn health(StateRef(state): StateRef<'_, Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        countries: state
            .indexes
            .country_codes()
            .into_iter()
            .map(String::from)
            .collect(),
    })
}

async fn search(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Query(params): Query<SearchParams>,
    req: &WebRequest<()>,
    remote_addr: SocketAddr,
) -> WebResponse {
    let query = params.q.unwrap_or_default();
    let country = normalize_country(params.country.as_deref());
    let limit = params.limit.unwrap_or(10).clamp(1, 50);
    let street_only = is_street_only(params.street_only.as_deref());

    if let Some(country_code) = country.as_deref()
        && !state.indexes.has_country(country_code)
    {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorResponse {
                error: "invalid_country",
                message: format!("country `{country_code}` is not indexed"),
            },
        );
    }

    if let Err(error) = state
        .auth
        .authorize(req, remote_addr, params.api_key.as_deref())
        .await
    {
        return json_error(error_status(&error), error);
    }

    match search_indexes_async(
        state.indexes.clone(),
        country.clone(),
        query.clone(),
        limit,
        street_only,
    )
    .await
    {
        Ok(results) => json_ok(SearchResponse {
            query,
            country,
            count: results.len(),
            results,
        }),
        Err(error) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorResponse {
                error: "search_failed",
                message: format!("search failed: {error}"),
            },
        ),
    }
}

async fn validate(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Query(params): Query<ValidateParams>,
    Json(request): Json<ValidateRequest>,
    req: &WebRequest<()>,
    remote_addr: SocketAddr,
) -> WebResponse {
    let country = normalize_country(Some(&request.country));
    let Some(country) = country else {
        return validation_error(
            "invalid_request",
            "field `country` must be a two-letter country code",
        );
    };
    if !state.indexes.has_country(&country) {
        return validation_error(
            "invalid_country",
            &format!("country `{country}` is not indexed"),
        );
    }
    if let Err(message) = validate_request_fields(&request) {
        return validation_error("invalid_request", &message);
    }
    if let Err(error) = state
        .auth
        .authorize_without_domain(req, remote_addr, params.api_key.as_deref())
        .await
    {
        return json_error(error_status(&error), error);
    }

    let query = validation_query_text(&request);
    let index = Arc::clone(
        state
            .indexes
            .by_country
            .get(&country)
            .expect("indexed country was checked"),
    );
    let candidates =
        match validation_candidates_async(index, query, VALIDATION_CANDIDATE_LIMIT).await {
            Ok(candidates) => candidates,
            Err(error) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorResponse {
                        error: "validation_failed",
                        message: format!("address validation failed: {error}"),
                    },
                );
            }
        };

    let matches = ranked_validation_matches(&request, candidates);
    let Some(best) = matches.first() else {
        return json_ok(ValidationResponse {
            valid: false,
            confidence_ratio: 0.0,
            corrected_address: None,
            suggestions: Vec::new(),
        });
    };
    let valid = address_matches_request(&request, &best.address);
    let confidence_ratio = best.confidence_ratio;
    let corrected_address = (!valid).then(|| best.address.clone());
    let suggestions = if confidence_ratio < VALIDATION_SUGGESTION_THRESHOLD {
        matches
            .into_iter()
            .take(VALIDATION_SUGGESTION_LIMIT)
            .collect()
    } else {
        Vec::new()
    };

    json_ok(ValidationResponse {
        valid,
        confidence_ratio,
        corrected_address,
        suggestions,
    })
}

fn validation_error(error: &'static str, message: &str) -> WebResponse {
    json_error(
        StatusCode::BAD_REQUEST,
        ErrorResponse {
            error,
            message: String::from(message),
        },
    )
}

fn validate_request_fields(request: &ValidateRequest) -> Result<(), String> {
    for (name, value) in [
        ("street", &request.street),
        ("house_number", &request.house_number),
        ("postal_code", &request.postal_code),
        ("city", &request.city),
    ] {
        if value.trim().is_empty() {
            return Err(format!("field `{name}` is required"));
        }
    }
    Ok(())
}

fn validation_query_text(request: &ValidateRequest) -> String {
    format!(
        "{} {} {} {}",
        request.street, request.house_number, request.postal_code, request.city
    )
}

fn ranked_validation_matches(
    request: &ValidateRequest,
    candidates: Vec<SearchResult>,
) -> Vec<ValidationMatch> {
    let mut seen = BTreeSet::new();
    let mut matches = candidates
        .into_iter()
        .filter_map(|candidate| {
            let address = candidate.address;
            let dedup_key = format!("{}\u{0}{}", address.country_code, address.full_address);
            seen.insert(dedup_key).then(|| ValidationMatch {
                confidence_ratio: validation_confidence(request, &address),
                address,
            })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        right
            .confidence_ratio
            .total_cmp(&left.confidence_ratio)
            .then_with(|| left.address.full_address.cmp(&right.address.full_address))
    });
    matches
}

fn validation_confidence(request: &ValidateRequest, address: &StructuredAddress) -> f64 {
    let street = text_similarity(&request.street, address.thoroughfare.as_deref());
    let house_number = house_number_similarity(&request.house_number, address);
    let postal_code = compact_similarity(&request.postal_code, address.postal_code.as_deref());
    let city = [
        text_similarity(&request.city, address.locality.as_deref()),
        text_similarity(&request.city, address.dependent_locality.as_deref()),
    ]
    .into_iter()
    .fold(0.0, f64::max);
    0.45 * street + 0.20 * house_number + 0.20 * postal_code + 0.15 * city
}

fn house_number_similarity(input: &str, address: &StructuredAddress) -> f64 {
    let full_number = match (&address.premise, &address.subpremise) {
        (Some(premise), Some(subpremise)) => Some(format!("{premise}/{subpremise}")),
        _ => None,
    };
    [
        text_similarity(input, address.premise.as_deref()),
        text_similarity(input, address.subpremise.as_deref()),
        text_similarity(input, full_number.as_deref()),
    ]
    .into_iter()
    .fold(0.0, f64::max)
}

fn address_matches_request(request: &ValidateRequest, address: &StructuredAddress) -> bool {
    normalize_text(&request.street) == normalize_optional(address.thoroughfare.as_deref())
        && normalize_text(&request.house_number) == normalize_optional(address.premise.as_deref())
        && compact(&request.postal_code) == compact_optional(address.postal_code.as_deref())
        && normalize_text(&request.city) == normalize_optional(address.locality.as_deref())
}

fn text_similarity(input: &str, candidate: Option<&str>) -> f64 {
    let input = normalize_text(input);
    let candidate = normalize_optional(candidate);
    normalized_similarity(&input, &candidate)
}

fn compact_similarity(input: &str, candidate: Option<&str>) -> f64 {
    normalized_similarity(&compact(input), &compact_optional(candidate))
}

fn normalize_optional(value: Option<&str>) -> String {
    value.map(normalize_text).unwrap_or_default()
}

fn compact_optional(value: Option<&str>) -> String {
    value.map(compact).unwrap_or_default()
}

fn compact(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn normalized_similarity(left: &str, right: &str) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    if left == right {
        return 1.0;
    }
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let max_len = left.len().max(right.len());
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    for (left_index, left_character) in left.iter().enumerate() {
        let mut current = vec![left_index + 1];
        for (right_index, right_character) in right.iter().enumerate() {
            let substitution =
                previous[right_index] + usize::from(left_character != right_character);
            let insertion = current[right_index] + 1;
            let deletion = previous[right_index + 1] + 1;
            current.push(substitution.min(insertion).min(deletion));
        }
        previous = current;
    }
    1.0 - (previous[right.len()] as f64 / max_len as f64)
}

fn normalize_country(country: Option<&str>) -> Option<String> {
    country
        .map(str::trim)
        .map(str::to_uppercase)
        .filter(|country| !country.is_empty())
}

/// Street-only search is enabled only by the bare `street_only` query flag.
fn is_street_only(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.is_empty())
}

fn socket_addr(addr: &str) -> SocketAddr {
    addr.parse()
        .unwrap_or_else(|_| "127.0.0.1:8080".parse().expect("default socket addr"))
}

fn quic_config() -> AppResult<quinn::ServerConfig> {
    let cert = generate_simple_self_signed(vec![String::from("localhost")])?;
    let cert_der = cert.cert.der().clone();
    let key_der = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());

    persist_cert(cert_der.as_ref())?;

    let mut crypto = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_no_client_auth()
    .with_single_cert(vec![cert_der], PrivateKeyDer::Pkcs8(key_der))?;
    crypto.alpn_protocols = vec![b"h3".to_vec()];
    crypto.max_early_data_size = u32::MAX;

    Ok(quinn::ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(crypto)?,
    )))
}

fn persist_cert(cert_der: &[u8]) -> AppResult<()> {
    let path = Path::new(H3_CERT_PATH);
    fs::write(path, cert_der)?;
    Ok(())
}

fn json_ok<T>(payload: T) -> WebResponse
where
    T: Serialize,
{
    json_response(StatusCode::OK, &payload)
}

fn json_error(status: StatusCode, payload: ErrorResponse) -> WebResponse {
    json_response(status, &payload)
}

fn json_response<T>(status: StatusCode, payload: &T) -> WebResponse
where
    T: Serialize,
{
    let body = serde_json::to_vec(payload).unwrap_or_else(|_| {
        Vec::from(
            br#"{"error":"internal_error","message":"failed to serialize response"}"#.as_slice(),
        )
    });
    let mut response = WebResponse::new(body.into());
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        xitca_web::http::HeaderValue::from_static("application/json"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::{
        AppState, ValidateRequest, address_matches_request, admin_home, health, home,
        is_street_only, normalize_country, ranked_validation_matches, search,
        validation_candidates_async, validation_confidence,
    };
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;

    use serde_json::Value as JsonValue;
    use tantivy::schema::{STORED, STRING, Schema, TEXT};
    use tantivy::{Index, ReloadPolicy};
    use tempfile::TempDir;
    use xitca_web::{
        App,
        handler::handler_service,
        http::{StatusCode, Uri, WebRequest},
        route::get,
        service::Service,
        test::collect_string_body,
    };

    use crate::auth::AuthState;
    use crate::models::{Address, StructuredAddress};
    use crate::search::{AddressIndex, AddressIndexes, IndexFields, IndexStorage};

    #[test]
    fn normalize_country_uppercases_and_trims() {
        assert_eq!(normalize_country(Some(" sk ")), Some(String::from("SK")));
    }

    #[test]
    fn street_only_flag_requires_bare_parameter() {
        assert!(is_street_only(Some("")));
        assert!(!is_street_only(Some("true")));
        assert!(!is_street_only(Some("1")));
        assert!(!is_street_only(Some("false")));
    }

    #[tokio::test]
    async fn search_endpoint_returns_structured_address_fields() {
        let indexes = Arc::new(AppState {
            indexes: Arc::new(test_indexes().expect("test index")),
            auth: AuthState::Disabled,
            demo_api_key: String::from("test-key"),
            admin_api_key: Some(String::from("test-admin-key")),
        });
        let service = App::new()
            .with_state(indexes)
            .at("/", get(handler_service(home)))
            .at("/health", get(handler_service(health)))
            .at("/search", get(handler_service(search)))
            .at("/suggest", get(handler_service(search)))
            .finish()
            .call(())
            .await
            .expect("app service");

        let mut req = WebRequest::default();
        *req.uri_mut() = Uri::from_static("/search?q=hlavna&country=SK&limit=1&api_key=test");
        req.headers_mut().insert(
            xitca_web::http::header::ORIGIN,
            xitca_web::http::HeaderValue::from_static("https://addresswise.eu"),
        );
        *req.body_mut().socket_addr_mut() = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234);

        let resp = service.call(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = collect_string_body(resp.into_body()).await.expect("body");
        let payload: JsonValue = serde_json::from_str(&body).expect("json body");

        assert_eq!(payload["query"], "hlavna");
        assert_eq!(payload["country"], "SK");
        assert_eq!(payload["count"], 1);
        assert_eq!(payload["results"][0]["country_code"], "SK");
        assert_eq!(payload["results"][0]["address"]["country_code"], "SK");
        assert_eq!(payload["results"][0]["address"]["thoroughfare"], "Hlavna");
        assert!(payload["results"][0]["address"]["premise"].is_string());
        assert_eq!(payload["results"][0]["address"]["postal_code"], "040 01");
        assert!(
            payload["results"][0]["address"]["full_address"]
                .as_str()
                .is_some_and(|address| address.starts_with("Hlavna "))
        );
    }

    #[tokio::test]
    async fn validation_finds_an_exact_structured_address() {
        let indexes = test_indexes().expect("test index");
        let index = Arc::clone(indexes.by_country.get("SK").expect("SK index"));
        let request = ValidateRequest {
            street: String::from("Hlavna"),
            house_number: String::from("68"),
            postal_code: String::from("040 01"),
            city: String::from("Kosice"),
            country: String::from("SK"),
        };

        let candidates = validation_candidates_async(
            index,
            super::validation_query_text(&request),
            super::VALIDATION_CANDIDATE_LIMIT,
        )
        .await
        .expect("validation candidates");
        let matches = ranked_validation_matches(&request, candidates);
        let best = matches.first().expect("matching address");

        assert!(address_matches_request(&request, &best.address));
        assert_eq!(best.confidence_ratio, 1.0);
    }

    #[tokio::test]
    async fn validation_corrects_a_wrong_postal_code_and_suggests_matches() {
        let indexes = test_indexes().expect("test index");
        let index = Arc::clone(indexes.by_country.get("SK").expect("SK index"));
        let request = ValidateRequest {
            street: String::from("Hlavna"),
            house_number: String::from("68"),
            postal_code: String::from("999 99"),
            city: String::from("Kosice"),
            country: String::from("SK"),
        };

        let candidates = validation_candidates_async(
            index,
            super::validation_query_text(&request),
            super::VALIDATION_CANDIDATE_LIMIT,
        )
        .await
        .expect("validation candidates");
        let matches = ranked_validation_matches(&request, candidates);
        let best = matches.first().expect("matching address");

        assert!(!address_matches_request(&request, &best.address));
        assert_eq!(best.address.postal_code.as_deref(), Some("040 01"));
        assert!(best.confidence_ratio < super::VALIDATION_SUGGESTION_THRESHOLD);
    }

    #[test]
    fn validation_scores_a_matching_subpremise_and_dependent_locality() {
        let request = ValidateRequest {
            street: String::from("Na pasekach"),
            house_number: String::from("20"),
            postal_code: String::from("83106"),
            city: String::from("bratislava"),
            country: String::from("SK"),
        };
        let address = StructuredAddress {
            country_code: String::from("SK"),
            admin_area: None,
            locality: None,
            dependent_locality: Some(String::from("Bratislava-Rača")),
            thoroughfare: Some(String::from("Na pasekách")),
            premise: Some(String::from("3085")),
            premise_type: None,
            subpremise: Some(String::from("20")),
            postal_code: Some(String::from("83106")),
            full_address: String::from("Na pasekách 3085/20, Bratislava-Rača, 83106, SK"),
        };

        assert!(!address_matches_request(&request, &address));
        assert!(validation_confidence(&request, &address) > 0.90);
    }

    #[tokio::test]
    async fn street_only_search_returns_distinct_streets_without_address_details() {
        let indexes = Arc::new(AppState {
            indexes: Arc::new(test_indexes().expect("test index")),
            auth: AuthState::Disabled,
            demo_api_key: String::from("test-key"),
            admin_api_key: Some(String::from("test-admin-key")),
        });
        let service = App::new()
            .with_state(indexes)
            .at("/search", get(handler_service(search)))
            .finish()
            .call(())
            .await
            .expect("app service");

        let mut req = WebRequest::default();
        *req.uri_mut() = Uri::from_static("/search?q=hl&country=SK&street_only");
        *req.body_mut().socket_addr_mut() = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234);

        let resp = service.call(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = collect_string_body(resp.into_body()).await.expect("body");
        let payload: JsonValue = serde_json::from_str(&body).expect("json body");

        assert_eq!(payload["count"], 2);
        assert_eq!(payload["results"][0]["formatted"], "Hlavna");
        assert_eq!(payload["results"][0]["address"]["thoroughfare"], "Hlavna");
        assert_eq!(payload["results"][1]["formatted"], "Hlinkova");
        assert!(payload["results"][0]["address"]["premise"].is_null());
        assert!(payload["results"][0]["address"]["locality"].is_null());
        assert_eq!(payload["results"][0]["address"]["full_address"], "Hlavna");
    }

    #[tokio::test]
    async fn street_only_search_falls_back_to_addresses_when_no_street_matches() {
        let state = Arc::new(AppState {
            indexes: Arc::new(test_indexes().expect("test index")),
            auth: AuthState::Disabled,
            demo_api_key: String::from("test-key"),
            admin_api_key: Some(String::from("test-admin-key")),
        });
        let service = App::new()
            .with_state(state)
            .at("/search", get(handler_service(search)))
            .finish()
            .call(())
            .await
            .expect("app service");

        let mut req = WebRequest::default();
        *req.uri_mut() = Uri::from_static("/search?q=hlavna%2068&country=SK&street_only");
        *req.body_mut().socket_addr_mut() = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234);

        let body = collect_string_body(service.call(req).await.expect("response").into_body())
            .await
            .expect("body");
        let payload: JsonValue = serde_json::from_str(&body).expect("json body");

        assert_eq!(payload["count"], 1);
        assert_eq!(payload["results"][0]["address"]["premise"], "68");
    }

    #[tokio::test]
    async fn home_endpoint_returns_html() {
        let indexes = Arc::new(AppState {
            indexes: Arc::new(test_indexes().expect("test index")),
            auth: AuthState::Disabled,
            demo_api_key: String::from("test-key"),
            admin_api_key: Some(String::from("test-admin-key")),
        });
        let service = App::new()
            .with_state(indexes)
            .at("/", get(handler_service(home)))
            .finish()
            .call(())
            .await
            .expect("app service");

        let mut req = WebRequest::default();
        *req.uri_mut() = Uri::from_static("/");

        let resp = service.call(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = collect_string_body(resp.into_body()).await.expect("body");
        assert!(body.contains("<title>addresswise</title>"));
        assert!(body.contains("id=\"search-form\""));
        assert!(body.contains("id=\"country-input\""));
        assert!(body.contains("Select a country"));
        assert!(body.contains("streetInput.disabled = !countryInput.value"));
        assert!(body.contains("label for=\"street-input\">Street &amp; house number</label>"));
        assert!(body.contains("A little help finding your way"));
        assert!(body.contains("class=\"brand-mark\""));
        assert!(body.contains("label for=\"city-input\">City</label>"));
        assert!(body.contains("label for=\"postal-code-input\">Postal code</label>"));
        assert!(body.contains("section class=\"panel\""));
        assert!(body.contains("const demoApiKey = \"test-key\""));
        assert!(!body.contains("api-key-input"));
        assert!(body.contains("&street_only"));
        assert!(body.contains("selectedStreet"));
        assert!(body.contains("focusStreetInputAtEnd"));
        assert!(body.contains("`${selectedStreet} `"));
        assert!(body.contains("fillStructuredFields(result)"));
        assert!(body.contains("latestResultsAreStreetOnly && !hasHouseNumber"));
        assert!(body.contains("Validate an address"));
        assert!(body.contains("id=\"validation-form\""));
        assert!(body.contains("/validate?${params.toString()}"));
        assert!(body.contains("confidence_ratio"));
    }

    #[tokio::test]
    async fn admin_page_is_available_without_exposing_key_data() {
        let state = Arc::new(AppState {
            indexes: Arc::new(test_indexes().expect("test index")),
            auth: AuthState::Disabled,
            demo_api_key: String::from("test-key"),
            admin_api_key: Some(String::from("test-admin-key")),
        });
        let service = App::new()
            .with_state(state)
            .at("/admin", get(handler_service(admin_home)))
            .at("/", get(handler_service(home)))
            .finish()
            .call(())
            .await
            .expect("app service");

        let mut req = WebRequest::default();
        *req.uri_mut() = Uri::from_static("/admin");
        let body = collect_string_body(service.call(req).await.expect("response").into_body())
            .await
            .expect("body");

        assert!(body.contains("API key administration"));
        assert!(body.contains("X-Admin-Key"));
        assert!(!body.contains("test-admin-key"));
    }

    fn test_indexes() -> tantivy::Result<AddressIndexes> {
        let index_dir = TempDir::new().expect("tempdir");
        let (index, fields) = build_test_index(&index_dir)?;
        let mut writer = index.writer(50_000_000)?;

        let address = Address::from_parts(
            StructuredAddress {
                country_code: String::from("SK"),
                admin_area: Some(String::from("Kosicky kraj")),
                locality: Some(String::from("Kosice")),
                dependent_locality: None,
                thoroughfare: Some(String::from("Hlavna")),
                premise: Some(String::from("68")),
                premise_type: Some(String::from("building")),
                subpremise: None,
                postal_code: Some(String::from("040 01")),
                full_address: String::from("Hlavna 68, Kosice, 040 01, SK"),
            },
            "hlavna 68 kosice 040 01 sk",
        );

        writer.add_document(test_document(&address, fields))?;
        let another_address = Address::from_parts(
            StructuredAddress {
                country_code: String::from("SK"),
                admin_area: Some(String::from("Kosicky kraj")),
                locality: Some(String::from("Kosice")),
                dependent_locality: None,
                thoroughfare: Some(String::from("Hlavna")),
                premise: Some(String::from("69")),
                premise_type: Some(String::from("building")),
                subpremise: None,
                postal_code: Some(String::from("040 01")),
                full_address: String::from("Hlavna 69, Kosice, 040 01, SK"),
            },
            "hlavna 69 kosice 040 01 sk",
        );
        writer.add_document(test_document(&another_address, fields))?;
        let hlinkova_address = Address::from_parts(
            StructuredAddress {
                country_code: String::from("SK"),
                admin_area: Some(String::from("Kosicky kraj")),
                locality: Some(String::from("Kosice")),
                dependent_locality: None,
                thoroughfare: Some(String::from("Hlinkova")),
                premise: Some(String::from("1")),
                premise_type: Some(String::from("building")),
                subpremise: None,
                postal_code: Some(String::from("040 01")),
                full_address: String::from("Hlinkova 1, Kosice, 040 01, SK"),
            },
            "hlinkova 1 kosice 040 01 sk",
        );
        writer.add_document(test_document(&hlinkova_address, fields))?;
        writer.commit()?;

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        reader.reload()?;

        let address_index = AddressIndex {
            _storage: IndexStorage::Temp {
                _temp_dir: index_dir,
            },
            reader: reader.clone(),
            street_reader: reader,
            fields,
        };

        Ok(AddressIndexes {
            by_country: HashMap::from([(String::from("SK"), Arc::new(address_index))]),
        })
    }

    fn build_test_index(index_dir: &TempDir) -> tantivy::Result<(Index, IndexFields)> {
        let mut schema_builder = Schema::builder();
        let country_code = schema_builder.add_text_field("country_code", STRING | STORED);
        let admin_area = schema_builder.add_text_field("admin_area", STORED);
        let locality = schema_builder.add_text_field("locality", STORED);
        let dependent_locality = schema_builder.add_text_field("dependent_locality", STORED);
        let thoroughfare = schema_builder.add_text_field("thoroughfare", TEXT | STORED);
        let premise = schema_builder.add_text_field("premise", STORED);
        let premise_type = schema_builder.add_text_field("premise_type", STORED);
        let subpremise = schema_builder.add_text_field("subpremise", STORED);
        let postal_code = schema_builder.add_text_field("postal_code", STORED);
        let full_address = schema_builder.add_text_field("full_address", STORED);
        let search_text = schema_builder.add_text_field("search_text", TEXT);
        let street_search_text = schema_builder.add_text_field("street_search_text", TEXT);
        let street_prefix_text = schema_builder.add_text_field("street_prefix_text", STRING);
        let schema = schema_builder.build();
        let index = Index::create_in_dir(index_dir, schema)?;

        Ok((
            index,
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
                street_prefix_text,
            },
        ))
    }

    fn test_document(address: &Address, fields: IndexFields) -> tantivy::schema::TantivyDocument {
        let mut document = tantivy::schema::TantivyDocument::default();
        document.add_text(fields.country_code, &address.country_code);
        if let Some(value) = &address.admin_area {
            document.add_text(fields.admin_area, value);
        }
        if let Some(value) = &address.locality {
            document.add_text(fields.locality, value);
        }
        if let Some(value) = &address.dependent_locality {
            document.add_text(fields.dependent_locality, value);
        }
        if let Some(value) = &address.thoroughfare {
            document.add_text(fields.thoroughfare, value);
        }
        if let Some(value) = &address.premise {
            document.add_text(fields.premise, value);
        }
        if let Some(value) = &address.premise_type {
            document.add_text(fields.premise_type, value);
        }
        if let Some(value) = &address.subpremise {
            document.add_text(fields.subpremise, value);
        }
        if let Some(value) = &address.postal_code {
            document.add_text(fields.postal_code, value);
        }
        document.add_text(fields.full_address, &address.full_address);
        document.add_text(fields.search_text, &address.search_text);
        if let Some(value) = &address.thoroughfare {
            let normalized_street = crate::normalize::normalize_text(value);
            document.add_text(fields.street_search_text, &normalized_street);
            for end in 1..=normalized_street.len() {
                document.add_text(fields.street_prefix_text, &normalized_street[..end]);
            }
        }
        document
    }
}
