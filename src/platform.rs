//! Shared product gateway and usage reporting. Existing address handlers and
//! their authentication/counters are deliberately independent of this module.
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use xitca_web::{
    handler::{query::Query, state::StateRef},
    http::{StatusCode, WebRequest, WebResponse, header},
};

use crate::auth::{ErrorResponse, error_status, extract_request_domain};
use crate::billing::{Operation, Reservation};
use crate::http::{AppState, authorize_admin, json_error, json_ok};

pub async fn landing(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Query(query): Query<crate::i18n::LanguageQuery>,
    req: &WebRequest<()>,
) -> WebResponse {
    let language = crate::i18n::Language::from_request(&query, req);
    let demo_key = serde_json::to_string(&state.demo_api_key)
        .expect("public demo key")
        .replace('<', "\\u003c");
    let countries =
        serde_json::to_string(&state.indexes.country_codes()).expect("active countries");
    crate::i18n::html(
        crate::i18n::template("platform", language)
            .replace("__DEMO_API_KEY__", &demo_key)
            .replace("__DEMO_COUNTRIES__", &countries)
            .replace(
                "__BILLING_CONFIG__",
                &crate::billing::public_config().to_string(),
            ),
        language,
    )
}
pub async fn account(
    Query(query): Query<crate::i18n::LanguageQuery>,
    req: &WebRequest<()>,
) -> WebResponse {
    let language = crate::i18n::Language::from_request(&query, req);
    crate::i18n::html(crate::i18n::template("account", language), language)
}
pub async fn docs(
    Query(query): Query<crate::i18n::LanguageQuery>,
    req: &WebRequest<()>,
) -> WebResponse {
    let language = crate::i18n::Language::from_request(&query, req);
    crate::i18n::html(crate::i18n::template("docs", language), language)
}
pub async fn terms(
    Query(query): Query<crate::i18n::LanguageQuery>,
    req: &WebRequest<()>,
) -> WebResponse {
    let language = crate::i18n::Language::from_request(&query, req);
    crate::i18n::html(crate::i18n::template("terms", language), language)
}

#[derive(Deserialize)]
pub struct KeyQuery {
    pub(crate) api_key: Option<String>,
    pub(crate) month: Option<String>,
}

pub(crate) fn supplied_key<'a>(query: &'a KeyQuery, req: &'a WebRequest<()>) -> Option<&'a str> {
    query
        .api_key
        .as_deref()
        .or_else(|| {
            req.headers()
                .get(header::AUTHORIZATION)?
                .to_str()
                .ok()?
                .strip_prefix("Bearer ")
        })
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

fn failure(status: StatusCode, error: &'static str, message: &str) -> WebResponse {
    json_error(
        status,
        ErrorResponse {
            error,
            message: message.into(),
        },
    )
}

pub(crate) async fn key_id(pool: &PgPool, key: &str) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar("select id from api_keys where api_key = $1 and is_active")
        .bind(key)
        .fetch_optional(pool)
        .await
}

fn company_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(90))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("Companywise HTTP client")
    })
}

/// Browser demos reuse the existing public demo key, subject to its domain
/// allowlist. Machine clients must supply their own key. Never forward keys to
/// the data service or allow user input to choose an upstream host.
pub async fn company_lookup(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Query(query): Query<KeyQuery>,
    req: &WebRequest<()>,
) -> WebResponse {
    let Some(pool) = state.auth.pool() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing_unavailable",
            "Shared authentication is unavailable",
        );
    };
    let supplied = supplied_key(&query, req);
    let Some(key) = supplied.or_else(|| browser_demo(req).then_some(state.demo_api_key.as_str()))
    else {
        return failure(
            StatusCode::UNAUTHORIZED,
            "missing_api_key",
            "Supply api_key or an Authorization: Bearer header",
        );
    };
    let id = match key_id(pool, key).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            return failure(
                StatusCode::UNAUTHORIZED,
                "invalid_api_key",
                "API key is invalid or inactive",
            );
        }
        Err(_) => {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "billing_unavailable",
                "Shared authentication is unavailable",
            );
        }
    };
    if key == state.demo_api_key {
        // Use the same domain rules as autocomplete, including explicit demo-key requests.
        let domain = extract_request_domain(req).unwrap_or_default();
        match sqlx::query_scalar::<_, bool>(
            "select exists(select 1 from api_key_domains where api_key_id = $1 and domain = $2)",
        )
        .bind(id)
        .bind(domain)
        .fetch_one(pool)
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                return failure(
                    StatusCode::FORBIDDEN,
                    "domain_not_allowed",
                    "The demo key requires an allowed browser origin",
                );
            }
            Err(_) => {
                return failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "billing_unavailable",
                    "Shared authentication is unavailable",
                );
            }
        }
    }
    let reservation = match Reservation::reserve(
        &state.auth,
        Some(id),
        Operation::Company,
        key == state.demo_api_key,
    )
    .await
    {
        Ok(reservation) => reservation,
        Err(response) => return *response,
    };
    let response = async {
    let Some(path) = req.uri().path().strip_prefix("/companywise") else {
        return failure(
            StatusCode::NOT_FOUND,
            "not_found",
            "Unknown company endpoint",
        );
    };
    let query = upstream_query(req.uri().query().unwrap_or_default());
    let upstream = match company_client()
        .get(format!("http://127.0.0.1:3001{path}"))
        .query(&query)
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => {
            return failure(
                StatusCode::BAD_GATEWAY,
                "companywise_unavailable",
                "Company lookup is temporarily unavailable",
            );
        }
    };
    let status = upstream.status();
    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_owned();
    let body = match upstream.bytes().await {
        Ok(body) => body,
        Err(_) => {
            return failure(
                StatusCode::BAD_GATEWAY,
                "companywise_unavailable",
                "Company lookup returned an incomplete response",
            );
        }
    };
    if status.is_success() {
        // Persist before acknowledging success. Do not bill upstream/validation errors.
        if sqlx::query("insert into companywise_usage_daily (api_key_id, usage_date, request_count)
            values ($1, current_date, 1) on conflict (api_key_id, usage_date)
            do update set request_count = companywise_usage_daily.request_count + 1, last_request_at = now()")
            .bind(id).execute(pool).await.is_err() {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "billing_unavailable", "Usage recording is temporarily unavailable");
        }
    }
    let mut response = WebResponse::new(body.to_vec().into());
    *response.status_mut() =
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(&content_type)
            .unwrap_or_else(|_| header::HeaderValue::from_static("application/json")),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
    }.await;
    reservation.finish(response).await
}

fn browser_demo(req: &WebRequest<()>) -> bool {
    req.headers()
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| url::Url::parse(value).ok())
        .is_some_and(|url| {
            url.scheme() == "https"
                && url.host_str() == Some("addresswise.eu")
                && (url.path().starts_with("/companywise/") || url.path() == "/")
        })
}

fn upstream_query(raw: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(raw.as_bytes())
        .filter(|(key, _)| key != "api_key")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

#[derive(Serialize)]
struct UsageDay {
    api_key_id: i64,
    label: Option<String>,
    product: String,
    date: String,
    requests: i64,
}

// Addresswise historical usage remains in its original table. No backfill,
// counter reset, or change to request counting is needed for unified billing.
const USAGE_SQL: &str = "select u.api_key_id, k.label, u.product, u.usage_date::text as date,
    sum(u.request_count)::bigint as requests from (
        select api_key_id, 'addresswise' as product, usage_date, request_count from api_key_usage_daily
        union all
        select api_key_id, 'companywise' as product, usage_date, request_count from companywise_usage_daily
    ) u join api_keys k on k.id = u.api_key_id
    where ($1::bigint is null or u.api_key_id = $1)
        and u.usage_date >= date_trunc('month', coalesce($2::text::date, current_date))::date
        and u.usage_date < (date_trunc('month', coalesce($2::text::date, current_date)) + interval '1 month')::date
    group by u.api_key_id, k.label, u.product, u.usage_date
    order by u.usage_date desc, u.api_key_id, u.product";

pub(crate) fn billing_month(month: Option<&str>) -> Result<Option<String>, ()> {
    match month {
        None => Ok(None),
        Some(value)
            if value.len() == 7
                && value.is_ascii()
                && value.as_bytes()[4] == b'-'
                && value[..4].bytes().all(|c| c.is_ascii_digit())
                && value[5..]
                    .parse::<u8>()
                    .is_ok_and(|m| (1..=12).contains(&m))
                && value[..4]
                    .parse::<u16>()
                    .is_ok_and(|y| (2000..=9999).contains(&y)) =>
        {
            Ok(Some(format!("{value}-01")))
        }
        Some(_) => Err(()),
    }
}

async fn usage(pool: &PgPool, id: Option<i64>, month: Option<&str>) -> WebResponse {
    let month = match billing_month(month) {
        Ok(month) => month,
        Err(()) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "invalid_month",
                "Use month=YYYY-MM",
            );
        }
    };
    match sqlx::query(USAGE_SQL)
        .bind(id)
        .bind(month)
        .fetch_all(pool)
        .await
    {
        Ok(rows) => {
            let days = rows
                .into_iter()
                .map(|row| UsageDay {
                    api_key_id: row.get("api_key_id"),
                    label: row.get("label"),
                    product: row.get("product"),
                    date: row.get("date"),
                    requests: row.get("requests"),
                })
                .collect::<Vec<_>>();
            let mut response = json_ok(days);
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing_unavailable",
            "Usage reporting is temporarily unavailable",
        ),
    }
}

pub async fn customer_usage(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Query(query): Query<KeyQuery>,
    req: &WebRequest<()>,
) -> WebResponse {
    let Some(key) = supplied_key(&query, req) else {
        let error = ErrorResponse {
            error: "missing_api_key",
            message: "Supply your API key".into(),
        };
        return json_error(error_status(&error), error);
    };
    let Some(pool) = state.auth.pool() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing_unavailable",
            "Usage reporting is unavailable",
        );
    };
    match key_id(pool, key).await {
        Ok(Some(id)) => usage(pool, Some(id), query.month.as_deref()).await,
        Ok(None) => failure(
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "API key is invalid or inactive",
        ),
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing_unavailable",
            "Usage reporting is unavailable",
        ),
    }
}

pub async fn admin_usage(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Query(query): Query<KeyQuery>,
    req: &WebRequest<()>,
) -> WebResponse {
    if let Err((status, message)) = authorize_admin(state, req) {
        return failure(status, "admin_error", message);
    }
    match state.auth.pool() {
        Some(pool) => usage(pool, None, query.month.as_deref()).await,
        None => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing_unavailable",
            "Usage reporting is unavailable",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_validation_rejects_invalid_dates_and_unicode() {
        assert_eq!(billing_month(None), Ok(None));
        assert_eq!(
            billing_month(Some("2026-11")),
            Ok(Some("2026-11-01".into()))
        );
        for value in [
            "2026-00",
            "2026-13",
            "2026-1",
            "0000-01",
            "2026-11-01",
            "💥-11",
            "💥💥",
        ] {
            assert!(billing_month(Some(value)).is_err(), "{value}");
        }
    }

    #[test]
    fn forwarding_removes_credentials_and_preserves_flags() {
        assert_eq!(
            upstream_query("api_key=secret&minimal=true&all_registers=false"),
            vec![
                ("minimal".into(), "true".into()),
                ("all_registers".into(), "false".into())
            ]
        );
    }

    #[test]
    fn demo_requires_the_real_site_and_product_page() {
        let mut req = WebRequest::default();
        assert!(!browser_demo(&req));
        for (referer, allowed) in [
            ("https://addresswise.eu/companywise/", true),
            ("https://addresswise.eu/companywise/docs/", true),
            ("https://addresswise.eu.evil.test/companywise/", false),
            ("https://evil.test/companywise/", false),
            ("http://addresswise.eu/companywise/", false),
        ] {
            req.headers_mut()
                .insert(header::REFERER, referer.parse().unwrap());
            assert_eq!(browser_demo(&req), allowed);
        }
    }

    #[test]
    fn customer_key_can_use_bearer_without_appearing_in_urls() {
        let mut req = WebRequest::default();
        req.headers_mut().insert(
            header::AUTHORIZATION,
            "Bearer aw_live_test".parse().unwrap(),
        );
        assert_eq!(
            supplied_key(
                &KeyQuery {
                    api_key: None,
                    month: None
                },
                &req
            ),
            Some("aw_live_test")
        );
    }
}
