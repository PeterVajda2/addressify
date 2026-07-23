use std::env;
use std::net::SocketAddr;
use std::path::Path;

use serde::Serialize;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use url::Url;
use xitca_web::http::{StatusCode, WebRequest, header};

const MIGRATIONS_DIR: &str = "db";

#[derive(Clone)]
pub enum AuthState {
    Disabled,
    Enabled(PgPool),
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
        Ok(Self::Enabled(pool))
    }

    pub async fn authorize(
        &self,
        req: &WebRequest<()>,
        remote_addr: SocketAddr,
        api_key: Option<&str>,
    ) -> Result<(), ErrorResponse> {
        match self {
            Self::Disabled => Ok(()),
            Self::Enabled(pool) => authorize_request(pool, req, remote_addr, api_key).await,
        }
    }
}

async fn authorize_request(
    pool: &PgPool,
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

    let api_key_id = sqlx::query(
        "select id
         from api_keys
         where api_key = $1 and is_active",
    )
    .bind(api_key)
    .fetch_optional(pool)
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
    .bind(&request_domain)
    .fetch_one(pool)
    .await
    .map_err(internal_error)?;

    if !allowed {
        return Err(ErrorResponse {
            error: "domain_not_allowed",
            message: format!("API key is not allowed for domain `{request_domain}`"),
        });
    }

    sqlx::query(
        "update api_keys
         set total_requests = total_requests + 1,
             last_used_at = now(),
             last_used_domain = $2,
             last_used_ip = $3
         where id = $1",
    )
    .bind(api_key_id)
    .bind(&request_domain)
    .bind(remote_addr.ip().to_string())
    .execute(pool)
    .await
    .map_err(internal_error)?;

    sqlx::query(
        "insert into api_key_usage_daily (api_key_id, usage_date, request_domain, request_count, last_request_at)
         values ($1, current_date, $2, 1, now())
         on conflict (api_key_id, usage_date, request_domain)
         do update
         set request_count = api_key_usage_daily.request_count + 1,
             last_request_at = excluded.last_request_at",
    )
    .bind(api_key_id)
    .bind(&request_domain)
    .execute(pool)
    .await
    .map_err(internal_error)?;

    Ok(())
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
    req.headers().get(name).and_then(|value| value.to_str().ok())
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
