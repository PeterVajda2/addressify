use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use quinn::crypto::rustls::QuicServerConfig;
use rcgen::generate_simple_self_signed;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use xitca_web::{
    App,
    handler::{handler_service, html::Html, json::Json, query::Query, state::StateRef},
    http::{StatusCode, WebRequest, WebResponse, header::CONTENT_TYPE},
    route::get,
};

use crate::AppResult;
use crate::auth::{AuthState, ErrorResponse, error_status};
use crate::models::SearchResult;
use crate::search::{AddressIndexes, search_indexes_async};

const MAX_WORKERS: usize = 8;
const BLOCKING_THREADS_PER_WORKER: usize = 8;
pub const H3_CERT_PATH: &str = "/tmp/addresswise-h3-cert.der";

pub struct AppState {
    pub indexes: Arc<AddressIndexes>,
    pub auth: AuthState,
    pub demo_api_key: String,
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

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
    countries: Vec<String>,
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
        .at("/", get(handler_service(home)))
        .at("/health", get(handler_service(health)))
        .at("/search", get(handler_service(search)))
        .at("/suggest", get(handler_service(search)))
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

    if let Some(country_code) = country.as_deref() {
        if !state.indexes.has_country(country_code) {
            return json_error(
                StatusCode::BAD_REQUEST,
                ErrorResponse {
                    error: "invalid_country",
                    message: format!("country `{country_code}` is not indexed"),
                },
            );
        }
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
    use super::{AppState, health, home, is_street_only, normalize_country, search};
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
    async fn street_only_search_returns_distinct_streets_without_address_details() {
        let indexes = Arc::new(AppState {
            indexes: Arc::new(test_indexes().expect("test index")),
            auth: AuthState::Disabled,
            demo_api_key: String::from("test-key"),
        });
        let service = App::new()
            .with_state(indexes)
            .at("/search", get(handler_service(search)))
            .finish()
            .call(())
            .await
            .expect("app service");

        let mut req = WebRequest::default();
        *req.uri_mut() = Uri::from_static("/search?q=hlavna&country=SK&street_only");
        *req.body_mut().socket_addr_mut() = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234);

        let resp = service.call(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = collect_string_body(resp.into_body()).await.expect("body");
        let payload: JsonValue = serde_json::from_str(&body).expect("json body");

        assert_eq!(payload["count"], 1);
        assert_eq!(payload["results"][0]["formatted"], "Hlavna");
        assert_eq!(payload["results"][0]["address"]["thoroughfare"], "Hlavna");
        assert!(payload["results"][0]["address"]["premise"].is_null());
        assert!(payload["results"][0]["address"]["locality"].is_null());
        assert_eq!(payload["results"][0]["address"]["full_address"], "Hlavna");
    }

    #[tokio::test]
    async fn home_endpoint_returns_html() {
        let indexes = Arc::new(AppState {
            indexes: Arc::new(test_indexes().expect("test index")),
            auth: AuthState::Disabled,
            demo_api_key: String::from("test-key"),
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
        assert!(body.contains("label for=\"street-input\">Street</label>"));
        assert!(body.contains("label for=\"city-input\">City</label>"));
        assert!(body.contains("label for=\"postal-code-input\">Postal code</label>"));
        assert!(body.contains("section class=\"panel\""));
        assert!(body.contains("const demoApiKey = \"test-key\""));
        assert!(!body.contains("api-key-input"));
        assert!(body.contains("&street_only"));
        assert!(body.contains("selectedStreet"));
        assert!(body.contains("fillStructuredFields(result)"));
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
            reader,
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
        let thoroughfare = schema_builder.add_text_field("thoroughfare", STORED);
        let premise = schema_builder.add_text_field("premise", STORED);
        let premise_type = schema_builder.add_text_field("premise_type", STORED);
        let subpremise = schema_builder.add_text_field("subpremise", STORED);
        let postal_code = schema_builder.add_text_field("postal_code", STORED);
        let full_address = schema_builder.add_text_field("full_address", STORED);
        let search_text = schema_builder.add_text_field("search_text", TEXT);
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
        document
    }
}
