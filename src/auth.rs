use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use url::Url;
use xitca_web::http::{StatusCode, WebRequest, header};

const MIGRATIONS_DIR: &str = "db";
const AUTH_CACHE_TTL: Duration = Duration::from_secs(30);
const USAGE_FLUSH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub enum AuthState {
    Disabled,
    Enabled(Arc<AuthService>),
}

pub(crate) struct AuthService {
    pool: PgPool,
    authorized_keys: Mutex<HashMap<(String, String), CachedAuthorization>>,
    pending_usage: Mutex<HashMap<UsageKey, u64>>,
}

struct CachedAuthorization {
    api_key_id: i64,
    expires_at: Instant,
}

#[derive(Hash, Eq, PartialEq)]
struct UsageKey {
    api_key_id: i64,
    domain: String,
    ip: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: &'static str,
    pub message: String,
}

impl AuthState {
    pub async fn from_env_required() -> crate::AppResult<Self> {
        let database_url = database_url_from_env()?;
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(&database_url)
            .await?;
        let service = Arc::new(AuthService {
            pool,
            authorized_keys: Mutex::new(HashMap::new()),
            pending_usage: Mutex::new(HashMap::new()),
        });
        start_usage_flusher(Arc::clone(&service));
        Ok(Self::Enabled(service))
    }

    pub async fn authorize(
        &self,
        req: &WebRequest<()>,
        remote_addr: SocketAddr,
        api_key: Option<&str>,
    ) -> Result<(), ErrorResponse> {
        match self {
            Self::Disabled => Ok(()),
            Self::Enabled(service) => authorize_request(service, req, remote_addr, api_key).await,
        }
    }
}

async fn authorize_request(
    service: &AuthService,
    req: &WebRequest<()>,
    remote_addr: SocketAddr,
    api_key: Option<&str>,
) -> Result<(), ErrorResponse> {
    let api_key = api_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ErrorResponse {
            error: "missing_api_key",
            message: String::from("query parameter `api_key` is required"),
        })?;

    let request_domain = extract_request_domain(req).ok_or_else(|| ErrorResponse {
        error: "missing_origin",
        message: String::from("request must include an Origin or Referer header"),
    })?;

    let cache_key = (api_key.to_owned(), request_domain.clone());
    let api_key_id = cached_api_key_id(service, &cache_key).await?;
    queue_usage(
        service,
        api_key_id,
        request_domain,
        remote_addr.ip().to_string(),
    );

    Ok(())
}

async fn cached_api_key_id(
    service: &AuthService,
    cache_key: &(String, String),
) -> Result<i64, ErrorResponse> {
    if let Some(cached) = service
        .authorized_keys
        .lock()
        .expect("authorization cache lock poisoned")
        .get(cache_key)
        .filter(|cached| cached.expires_at > Instant::now())
    {
        return Ok(cached.api_key_id);
    }

    let api_key_id = sqlx::query(
        "select id
         from api_keys
         where api_key = $1 and is_active",
    )
    .bind(&cache_key.0)
    .fetch_optional(&service.pool)
    .await
    .map_err(internal_error)?;

    let Some(api_key_row) = api_key_id else {
        return Err(ErrorResponse {
            error: "invalid_api_key",
            message: String::from("API key is invalid or inactive"),
        });
    };

    let api_key_id: i64 = api_key_row.get("id");
    let allowed = sqlx::query_scalar::<_, bool>(
        "select exists(
            select 1
            from api_key_domains
            where api_key_id = $1 and domain = $2
        )",
    )
    .bind(api_key_id)
    .bind(&cache_key.1)
    .fetch_one(&service.pool)
    .await
    .map_err(internal_error)?;

    if !allowed {
        return Err(ErrorResponse {
            error: "domain_not_allowed",
            message: format!("API key is not allowed for domain `{}`", cache_key.1),
        });
    }

    service
        .authorized_keys
        .lock()
        .expect("authorization cache lock poisoned")
        .insert(
            cache_key.clone(),
            CachedAuthorization {
                api_key_id,
                expires_at: Instant::now() + AUTH_CACHE_TTL,
            },
        );
    Ok(api_key_id)
}

fn queue_usage(service: &AuthService, api_key_id: i64, domain: String, ip: String) {
    let key = UsageKey {
        api_key_id,
        domain,
        ip,
    };
    *service
        .pending_usage
        .lock()
        .expect("usage queue lock poisoned")
        .entry(key)
        .or_default() += 1;
}

fn start_usage_flusher(service: Arc<AuthService>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(USAGE_FLUSH_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(error) = flush_usage(&service).await {
                eprintln!("failed to flush API usage: {error}");
            }
        }
    });
}

async fn flush_usage(service: &AuthService) -> Result<(), sqlx::Error> {
    let pending_usage = {
        let mut pending_usage = service
            .pending_usage
            .lock()
            .expect("usage queue lock poisoned");
        std::mem::take(&mut *pending_usage)
    };

    for (usage, count) in pending_usage {
        if let Err(error) = flush_usage_entry(&service.pool, &usage, count).await {
            *service
                .pending_usage
                .lock()
                .expect("usage queue lock poisoned")
                .entry(usage)
                .or_default() += count;
            return Err(error);
        }
    }
    Ok(())
}

async fn flush_usage_entry(pool: &PgPool, usage: &UsageKey, count: u64) -> Result<(), sqlx::Error> {
    let count = i64::try_from(count).unwrap_or(i64::MAX);
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "update api_keys
         set total_requests = total_requests + $2,
             last_used_at = now(),
             last_used_domain = $3,
             last_used_ip = $4
         where id = $1",
    )
    .bind(usage.api_key_id)
    .bind(count)
    .bind(&usage.domain)
    .bind(&usage.ip)
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        "insert into api_key_usage_daily (api_key_id, usage_date, request_domain, request_count, last_request_at)
         values ($1, current_date, $2, 1, now())
         on conflict (api_key_id, usage_date, request_domain)
         do update set request_count = api_key_usage_daily.request_count + $3,
             last_request_at = excluded.last_request_at",
    )
    .bind(usage.api_key_id)
    .bind(&usage.domain)
    .bind(count)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await
}

fn database_url_from_env() -> crate::AppResult<String> {
    env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL is required for migrations and authenticated serving".into())
}

pub fn error_status(error: &ErrorResponse) -> StatusCode {
    match error.error {
        "missing_api_key" | "missing_origin" | "invalid_api_key" => StatusCode::UNAUTHORIZED,
        "domain_not_allowed" => StatusCode::FORBIDDEN,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub fn extract_request_domain(req: &WebRequest<()>) -> Option<String> {
    header_value(req, header::ORIGIN)
        .and_then(parse_domain_from_url)
        .or_else(|| header_value(req, header::REFERER).and_then(parse_domain_from_url))
}

fn header_value<'a>(req: &'a WebRequest<()>, name: header::HeaderName) -> Option<&'a str> {
    req.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

fn parse_domain_from_url(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    let host = url.host_str()?;
    normalize_domain(host)
}

pub fn normalize_domain(value: &str) -> Option<String> {
    let domain = value
        .trim()
        .trim_end_matches('.')
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();

    if domain.is_empty() {
        None
    } else {
        Some(domain)
    }
}

fn internal_error(error: sqlx::Error) -> ErrorResponse {
    ErrorResponse {
        error: "internal_error",
        message: format!("database operation failed: {error}"),
    }
}

pub fn db_dir() -> &'static Path {
    Path::new(MIGRATIONS_DIR)
}

#[cfg(test)]
mod tests {
    use super::{extract_request_domain, normalize_domain};
    use xitca_web::http::{Uri, WebRequest, header};

    #[test]
    fn normalize_domain_trims_port_case_and_dot() {
        assert_eq!(
            normalize_domain(" AddressWise.EU:443. "),
            Some(String::from("addresswise.eu"))
        );
    }

    #[test]
    fn origin_header_domain_is_extracted() {
        let mut req = WebRequest::default();
        *req.uri_mut() = Uri::from_static("/search?q=test");
        req.headers_mut().insert(
            header::ORIGIN,
            header::HeaderValue::from_static("https://addresswise.eu"),
        );

        assert_eq!(
            extract_request_domain(&req),
            Some(String::from("addresswise.eu"))
        );
    }

    #[test]
    fn referer_header_domain_is_used_as_fallback() {
        let mut req = WebRequest::default();
        *req.uri_mut() = Uri::from_static("/search?q=test");
        req.headers_mut().insert(
            header::REFERER,
            header::HeaderValue::from_static("https://addresswise.eu/form"),
        );

        assert_eq!(
            extract_request_domain(&req),
            Some(String::from("addresswise.eu"))
        );
    }
}
