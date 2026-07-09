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
    http::StatusCode,
    route::get,
};

use crate::AppResult;
use crate::models::SearchResult;
use crate::search::{AddressIndexes, search_indexes_async};

const MAX_WORKERS: usize = 8;
const BLOCKING_THREADS_PER_WORKER: usize = 8;
pub const H3_CERT_PATH: &str = "/tmp/addresswise-h3-cert.der";

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: Option<String>,
    country: Option<String>,
    limit: Option<usize>,
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

pub fn serve(addr: String, indexes: Arc<AddressIndexes>) -> AppResult<()> {
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .clamp(1, MAX_WORKERS);
    let socket_addr = socket_addr(&addr);
    let h3_config = quic_config()?;

    App::new()
        .with_state(indexes)
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

async fn home() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>addresswise</title>
  <style>
    :root {
      color-scheme: light;
      --bg: #f3efe7;
      --panel: #fffdf8;
      --ink: #18202b;
      --line: #d8d0c0;
      --accent: #145f4c;
      --accent-soft: #dceee8;
      --muted: #647083;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      font-family: Georgia, "Times New Roman", serif;
      color: var(--ink);
      background:
        radial-gradient(circle at top left, rgba(20, 95, 76, 0.18), transparent 26rem),
        radial-gradient(circle at bottom right, rgba(196, 145, 70, 0.12), transparent 24rem),
        var(--bg);
      min-height: 100vh;
      display: grid;
      place-items: center;
      padding: 1.5rem;
    }
    main {
      width: min(100%, 44rem);
      background: var(--panel);
      border: 1px solid var(--line);
      border-radius: 1.5rem;
      padding: 2rem;
      box-shadow: 0 1.5rem 4rem rgba(24, 32, 43, 0.08);
    }
    h1 {
      margin: 0 0 1rem;
      font-size: clamp(2.2rem, 5vw, 3.6rem);
      line-height: 1;
      letter-spacing: -0.04em;
    }
    form {
      display: grid;
      gap: 1rem;
    }
    input {
      width: 100%;
      padding: 1rem 1.1rem;
      border-radius: 1rem;
      border: 1px solid var(--line);
      background: #fff;
      color: var(--ink);
      font: inherit;
      font-size: 1.05rem;
    }
    input:focus {
      outline: 2px solid transparent;
      border-color: var(--accent);
      box-shadow: 0 0 0 0.2rem rgba(20, 95, 76, 0.16);
    }
    button {
      width: fit-content;
      padding: 0.9rem 1.2rem;
      border-radius: 999px;
      border: 0;
      background: var(--accent);
      color: white;
      font: inherit;
      font-weight: 600;
      cursor: pointer;
    }
    ul {
      list-style: none;
      padding: 0;
      margin: 0;
      display: grid;
      gap: 0.75rem;
    }
    li {
      padding: 0.95rem 1rem;
      border-radius: 1rem;
      background: var(--accent-soft);
      border: 1px solid rgba(20, 95, 76, 0.08);
      line-height: 1.45;
    }
    .muted {
      color: var(--muted);
      margin: 0;
      min-height: 1.5rem;
    }
  </style>
</head>
<body>
  <main>
    <h1>addresswise</h1>
    <form id="search-form">
      <input id="street-input" name="q" type="text" placeholder="Type a street or address" autocomplete="street-address" spellcheck="false">
      <button type="submit">Search</button>
    </form>
    <p class="muted" id="status"></p>
    <ul id="results"></ul>
  </main>
  <script>
    const form = document.getElementById("search-form");
    const input = document.getElementById("street-input");
    const status = document.getElementById("status");
    const results = document.getElementById("results");

    function escapeHtml(value) {
      return value
        .replaceAll("&", "&amp;")
        .replaceAll("<", "&lt;")
        .replaceAll(">", "&gt;")
        .replaceAll("\"", "&quot;")
        .replaceAll("'", "&#39;");
    }

    let activeController = null;
    let pendingTimer = null;
    let requestSequence = 0;

    async function runSearch(query) {
      if (activeController) {
        activeController.abort();
      }

      activeController = new AbortController();
      const params = new URLSearchParams({ q: query, limit: "10" });
      const response = await fetch(`/search?${params.toString()}`, {
        method: "GET",
        cache: "no-store",
        credentials: "same-origin",
        signal: activeController.signal,
      });

      if (!response.ok) {
        throw new Error(`Search failed with HTTP ${response.status}`);
      }

      return response.json();
    }

    function renderResults(payload) {
      status.textContent = `${payload.count} result(s)`;
      results.innerHTML = payload.results
        .map((result) => `<li>${escapeHtml(result.address.full_address)}</li>`)
        .join("");

      if (!payload.results.length) {
        status.textContent = "No results.";
      }
    }

    async function triggerSearch(query) {
      results.innerHTML = "";

      if (!query) {
        if (activeController) {
          activeController.abort();
          activeController = null;
        }
        status.textContent = "Start typing to see suggestions.";
        return;
      }

      const currentRequest = ++requestSequence;
      status.textContent = "Searching...";

      try {
        const payload = await runSearch(query);
        if (currentRequest === requestSequence) {
          renderResults(payload);
        }
      } catch (error) {
        if (error.name === "AbortError") {
          return;
        }
        if (currentRequest === requestSequence) {
          status.textContent = error.message;
        }
      }
    }

    function scheduleSearch() {
      if (pendingTimer) {
        clearTimeout(pendingTimer);
      }

      pendingTimer = setTimeout(() => {
        pendingTimer = null;
        void triggerSearch(input.value.trim());
      }, 120);
    }

    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      if (pendingTimer) {
        clearTimeout(pendingTimer);
        pendingTimer = null;
      }
      await triggerSearch(input.value.trim());
    });

    input.addEventListener("input", () => {
      scheduleSearch();
    });

    status.textContent = "Start typing to see suggestions.";
  </script>
</body>
</html>"#,
    )
}

async fn health(StateRef(indexes): StateRef<'_, Arc<AddressIndexes>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        countries: indexes
            .country_codes()
            .into_iter()
            .map(String::from)
            .collect(),
    })
}

async fn search(
    StateRef(indexes): StateRef<'_, Arc<AddressIndexes>>,
    Query(params): Query<SearchParams>,
) -> Result<Json<SearchResponse>, StatusCode> {
    let query = params.q.unwrap_or_default();
    let country = normalize_country(params.country.as_deref());
    let limit = params.limit.unwrap_or(10).clamp(1, 50);

    if let Some(country_code) = country.as_deref() {
        if !indexes.has_country(country_code) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }

    match search_indexes_async(indexes.clone(), country.clone(), query.clone(), limit).await {
        Ok(results) => Ok(Json(SearchResponse {
            query,
            country,
            count: results.len(),
            results,
        })),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

fn normalize_country(country: Option<&str>) -> Option<String> {
    country
        .map(str::trim)
        .map(str::to_uppercase)
        .filter(|country| !country.is_empty())
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

#[cfg(test)]
mod tests {
    use super::{health, home, normalize_country, search};
    use std::collections::HashMap;
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

    use crate::models::{Address, StructuredAddress};
    use crate::search::{AddressIndex, AddressIndexes, IndexFields, IndexStorage};

    #[test]
    fn normalize_country_uppercases_and_trims() {
        assert_eq!(normalize_country(Some(" sk ")), Some(String::from("SK")));
    }

    #[tokio::test]
    async fn search_endpoint_returns_structured_address_fields() {
        let indexes = Arc::new(test_indexes().expect("test index"));
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
        *req.uri_mut() = Uri::from_static("/search?q=hlavna&country=SK");

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
        assert_eq!(payload["results"][0]["address"]["premise"], "68");
        assert_eq!(payload["results"][0]["address"]["postal_code"], "040 01");
        assert_eq!(
            payload["results"][0]["address"]["full_address"],
            "Hlavna 68, Kosice, 040 01, SK"
        );
    }

    #[tokio::test]
    async fn home_endpoint_returns_html() {
        let indexes = Arc::new(test_indexes().expect("test index"));
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
        assert!(body.contains("input.addEventListener(\"input\""));
        assert!(body.contains("Start typing to see suggestions."));
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
