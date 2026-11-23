//! Customer identities and sessions. API credentials and legacy API auth remain
//! independent. Paid choices are requests, never proof of payment.
use crate::{
    http::{AppState, json_response},
    platform::billing_month,
};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{
    net::SocketAddr,
    sync::{Arc, LazyLock},
};
use subtle::ConstantTimeEq;
use xitca_web::{
    handler::{json::Json, query::Query, state::StateRef},
    http::{StatusCode, WebRequest, WebResponse, header},
};

pub const TERMS_VERSION: &str = "2026-11-23-accounts";
const COOKIE: &str = "__Host-addresswise_session";
static HASH_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| hash_password("dummy-password-not-an-account"));
fn token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}
fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn csrf(value: &str) -> String {
    digest(&format!("addresswise-csrf:{value}"))
}
fn hash_password(value: &str) -> String {
    let salt = SaltString::encode_b64(&rand::random::<[u8; 16]>()).expect("random salt");
    Argon2::default()
        .hash_password(value.as_bytes(), &salt)
        .expect("valid hash parameters")
        .to_string()
}
fn response(status: StatusCode, value: serde_json::Value) -> WebResponse {
    let mut response = json_response(status, &value);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
        .headers_mut()
        .insert(header::VARY, header::HeaderValue::from_static("Cookie"));
    response
}
fn error(status: StatusCode, code: &str, message: &str) -> WebResponse {
    response(status, serde_json::json!({"error":code,"message":message}))
}
fn unavailable() -> WebResponse {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "account_unavailable",
        "Account service is temporarily unavailable. Please retry later.",
    )
}
fn database(error: sqlx::Error) -> WebResponse {
    eprintln!(
        "customer account database operation failed: {}",
        error
            .as_database_error()
            .map(|e| e.code().map(|s| s.into_owned()).unwrap_or_default())
            .unwrap_or_default()
    );
    unavailable()
}
fn has_account_origin(req: &WebRequest<()>) -> bool {
    req.headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        == Some("https://addresswise.eu")
}
fn invalid_origin() -> WebResponse {
    error(
        StatusCode::FORBIDDEN,
        "invalid_origin",
        "Account changes must be made from addresswise.eu.",
    )
}
fn cookie(req: &WebRequest<()>) -> Option<&str> {
    req.headers()
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|entry| entry.trim().strip_prefix(&format!("{COOKIE}=")))
        .filter(|value| {
            value.len() == 43
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}
fn session_cookie(mut response: WebResponse, value: &str, clear: bool) -> WebResponse {
    let age = if clear { 0 } else { 30 * 24 * 60 * 60 };
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!("{COOKIE}={value}; Path=/; Max-Age={age}; Secure; HttpOnly; SameSite=Lax")
            .parse()
            .expect("cookie"),
    );
    response
}
fn email(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    let (local, host) = value.split_once('@')?;
    if value.len() > 254
        || !value.is_ascii()
        || local.is_empty()
        || host.is_empty()
        || host.contains('@')
        || !host.contains('.')
        || host.starts_with('.')
        || host.ends_with('.')
        || value.chars().any(char::is_whitespace)
    {
        None
    } else {
        Some(value)
    }
}
pub fn registration_domain(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches('.');
    if value.is_empty()
        || value.len() > 253
        || value.chars().any(char::is_whitespace)
        || value.contains(['/', ':', '*', '@', '%', '_', '?', '#'])
    {
        return None;
    }
    let url = url::Url::parse(&format!("https://{value}")).ok()?;
    let url::Host::Domain(host) = url.host()? else {
        return None;
    };
    if host.split('.').any(|label| {
        label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }) {
        return None;
    }
    psl::domain(host.as_bytes())?;
    Some(host.to_ascii_lowercase())
}
fn client_bucket(req: &WebRequest<()>, remote: SocketAddr) -> String {
    // Only the loopback reverse proxy can supply X-Real-IP. Nginx overwrites it.
    let ip = if remote.ip().is_loopback() {
        req.headers()
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<std::net::IpAddr>().ok())
            .unwrap_or(remote.ip())
    } else {
        remote.ip()
    };
    digest(&ip.to_string())
}
async fn limited(
    pool: &PgPool,
    bucket: &str,
    maximum: i32,
    seconds: i32,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar::<_,i32>("INSERT INTO customer_auth_limits(bucket,attempts,resets_at) VALUES($1,1,now()+make_interval(secs=>$3))
      ON CONFLICT(bucket) DO UPDATE SET attempts=CASE WHEN customer_auth_limits.resets_at<=now() THEN 1 ELSE customer_auth_limits.attempts+1 END,
      resets_at=CASE WHEN customer_auth_limits.resets_at<=now() THEN excluded.resets_at ELSE customer_auth_limits.resets_at END
      WHERE customer_auth_limits.resets_at<=now() OR customer_auth_limits.attempts<$2 RETURNING attempts")
      .bind(bucket).bind(maximum).bind(seconds).fetch_optional(pool).await?.is_none())
}
async fn limit_response(
    pool: &PgPool,
    bucket: &str,
    maximum: i32,
    seconds: i32,
) -> Option<WebResponse> {
    match limited(pool, bucket, maximum, seconds).await {
        Ok(false) => None,
        Ok(true) => {
            let mut r = error(
                StatusCode::TOO_MANY_REQUESTS,
                "account_rate_limited",
                "Too many account attempts. Please try again later.",
            );
            r.headers_mut()
                .insert(header::RETRY_AFTER, seconds.to_string().parse().unwrap());
            Some(r)
        }
        Err(e) => Some(database(e)),
    }
}
async fn identity(
    pool: &PgPool,
    req: &WebRequest<()>,
    mutation: bool,
) -> Result<(i64, String), Box<WebResponse>> {
    if mutation && !has_account_origin(req) {
        return Err(Box::new(invalid_origin()));
    }
    let raw = cookie(req).ok_or_else(|| {
        error(
            StatusCode::UNAUTHORIZED,
            "login_required",
            "Sign in to access your account.",
        )
    })?;
    if mutation {
        let expected = csrf(raw);
        let supplied = req
            .headers()
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !bool::from(expected.as_bytes().ct_eq(supplied.as_bytes())) {
            return Err(Box::new(error(
                StatusCode::FORBIDDEN,
                "invalid_csrf",
                "Refresh your account page and try again.",
            )));
        }
    }
    let owner = sqlx::query_scalar::<_, i64>(
        "SELECT customer_id FROM customer_sessions WHERE token_hash=$1 AND expires_at>now()",
    )
    .bind(digest(raw))
    .fetch_optional(pool)
    .await
    .map_err(database)?
    .ok_or_else(|| {
        error(
            StatusCode::UNAUTHORIZED,
            "session_expired",
            "Your session has expired. Sign in again.",
        )
    })?;
    Ok((owner, csrf(raw)))
}
#[derive(Deserialize)]
pub struct RegisterRequest {
    name: String,
    #[serde(default)]
    business_address: String,
    email: String,
    password: String,
    domain: String,
    plan: String,
    #[serde(default)]
    business: bool,
    #[serde(default)]
    accept_terms: bool,
    terms_version: String,
    language: String,
    #[serde(default)]
    website: String,
}
pub async fn register(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Json(request): Json<RegisterRequest>,
    req: &WebRequest<()>,
    remote: SocketAddr,
) -> WebResponse {
    if !has_account_origin(req) {
        return invalid_origin();
    }
    let Some(pool) = state.auth.pool() else {
        return unavailable();
    };
    if let Some(r) = limit_response(
        pool,
        &format!("register:{}", client_bucket(req, remote)),
        20,
        3600,
    )
    .await
    {
        return r;
    }
    let Some(email) = email(&request.email) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_registration",
            "Enter a valid email address.",
        );
    };
    let Some(domain) = registration_domain(&request.domain) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_domain",
            "Enter a website hostname such as mydomain.com, without a URL, path or wildcard.",
        );
    };
    if request.name.trim().is_empty()
        || request.name.chars().count() > 200
        || request.password.chars().count() < 12
        || request.password.len() > 1024
        || !request.website.is_empty()
        || !matches!(
            request.plan.as_str(),
            "free" | "starter" | "growth" | "custom"
        )
        || !matches!(request.language.as_str(), "cs" | "sk" | "en")
        || !request.business
        || !request.accept_terms
    {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_registration",
            "Enter your business name, a password of at least 12 characters, and accept the business terms.",
        );
    }
    let business_address = request.business_address.trim();
    if business_address.chars().count() > 500 || business_address.contains('\0') {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_business_address",
            "Enter a business address of at most 500 characters.",
        );
    }
    let business_address = Some(business_address).filter(|address| !address.is_empty());
    if request.terms_version != TERMS_VERSION {
        return error(
            StatusCode::CONFLICT,
            "terms_updated",
            "The terms have changed. Reload the page before registering.",
        );
    }
    let hash = {
        let Ok(slot) = HASH_SLOTS.try_acquire() else {
            return unavailable();
        };
        match tokio::task::spawn_blocking(move || {
            let _slot = slot;
            hash_password(&request.password)
        })
        .await
        {
            Ok(hash) => hash,
            Err(_) => return unavailable(),
        }
    };
    let raw = token();
    let key = crate::http::generate_api_key();
    let result:Result<(),sqlx::Error>=async {
        let mut tx=pool.begin().await?;
        let owner=sqlx::query_scalar::<_,i64>("INSERT INTO customer_accounts(email,name,password_hash,domain,requested_plan,terms_version,terms_language,business_address) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id")
            .bind(email).bind(request.name.trim()).bind(hash).bind(&domain).bind(request.plan).bind(TERMS_VERSION).bind(request.language).bind(business_address).fetch_one(&mut *tx).await?;
        let key_id=sqlx::query_scalar::<_,i64>("INSERT INTO api_keys(api_key,label,billing_plan) VALUES($1,$2,'free') RETURNING id")
            .bind(key).bind(format!("{} · {}",request.name.trim(),domain)).fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO api_key_domains(api_key_id,domain) VALUES($1,$2)").bind(key_id).bind(domain).execute(&mut *tx).await?;
        sqlx::query("UPDATE customer_accounts SET api_key_id=$2 WHERE id=$1").bind(owner).bind(key_id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO customer_sessions(token_hash,customer_id) VALUES($1,$2)").bind(digest(&raw)).bind(owner).execute(&mut *tx).await?;
        tx.commit().await
    }.await;
    match result {
        Ok(()) => session_cookie(
            response(
                StatusCode::CREATED,
                serde_json::json!({"created":true,"redirect":"/account","csrf_token":csrf(&raw)}),
            ),
            &raw,
            false,
        ),
        Err(e)
            if e.as_database_error()
                .is_some_and(|e| e.is_unique_violation()) =>
        {
            error(
                StatusCode::CONFLICT,
                "account_exists",
                "An account with this email already exists. Sign in instead.",
            )
        }
        Err(e) => database(e),
    }
}
#[derive(Deserialize)]
pub struct LoginRequest {
    email: String,
    password: String,
}
pub async fn login(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Json(request): Json<LoginRequest>,
    req: &WebRequest<()>,
    remote: SocketAddr,
) -> WebResponse {
    if !has_account_origin(req) {
        return invalid_origin();
    }
    let Some(pool) = state.auth.pool() else {
        return unavailable();
    };
    if let Some(r) = limit_response(
        pool,
        &format!("login-ip:{}", client_bucket(req, remote)),
        120,
        900,
    )
    .await
    {
        return r;
    }
    let email = email(&request.email).unwrap_or_default();
    if let Some(r) = limit_response(pool, &format!("login-email:{}", digest(&email)), 20, 900).await
    {
        return r;
    }
    if request.password.len() > 1024 {
        return error(
            StatusCode::UNAUTHORIZED,
            "invalid_login",
            "Email or password is incorrect.",
        );
    }
    let row = match sqlx::query("SELECT id,password_hash FROM customer_accounts WHERE email=$1")
        .bind(email)
        .fetch_optional(pool)
        .await
    {
        Ok(row) => row,
        Err(e) => return database(e),
    };
    let owner = row.as_ref().map(|r| r.get::<i64, _>("id"));
    let hash = row.map(|r| r.get::<String, _>("password_hash"));
    let verified_hash = hash.clone();
    let valid = {
        let Ok(slot) = HASH_SLOTS.try_acquire() else {
            return unavailable();
        };
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            let hash = hash.as_deref().unwrap_or(&DUMMY_HASH);
            PasswordHash::new(hash).is_ok_and(|parsed| {
                Argon2::default()
                    .verify_password(request.password.as_bytes(), &parsed)
                    .is_ok()
            })
        })
        .await
        .unwrap_or(false)
    };
    let Some(owner) = owner.filter(|_| valid) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "invalid_login",
            "Email or password is incorrect.",
        );
    };
    let raw = token();
    let result: Result<bool, sqlx::Error> = async {
        let mut tx = pool.begin().await?;
        let current = sqlx::query_scalar::<_, String>(
            "SELECT password_hash FROM customer_accounts WHERE id=$1 FOR UPDATE",
        )
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await?;
        if current.is_none() || current != verified_hash {
            return Ok(false);
        }
        if let Some(old) = cookie(req) {
            sqlx::query("DELETE FROM customer_sessions WHERE token_hash=$1")
                .bind(digest(old))
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("DELETE FROM customer_sessions WHERE expires_at<=now()")
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO customer_sessions(token_hash,customer_id) VALUES($1,$2)")
            .bind(digest(&raw))
            .bind(owner)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }
    .await;
    match result {
        Ok(true) => session_cookie(
            response(
                StatusCode::OK,
                serde_json::json!({"signed_in":true,"csrf_token":csrf(&raw)}),
            ),
            &raw,
            false,
        ),
        Ok(false) => error(
            StatusCode::UNAUTHORIZED,
            "invalid_login",
            "Email or password is incorrect.",
        ),
        Err(e) => database(e),
    }
}
#[derive(Default, Deserialize)]
pub struct AccountQuery {
    month: Option<String>,
}
pub async fn me(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Query(query): Query<AccountQuery>,
    req: &WebRequest<()>,
) -> WebResponse {
    let Some(pool) = state.auth.pool() else {
        return unavailable();
    };
    let (owner, csrf) = match identity(pool, req, false).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let month = match billing_month(query.month.as_deref()) {
        Ok(v) => v,
        Err(()) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_month",
                "Use month=YYYY-MM",
            );
        }
    };
    match profile(pool, owner, &csrf, month.as_deref()).await {
        Ok(body) => response(StatusCode::OK, body),
        Err(e) => database(e),
    }
}
async fn profile(
    pool: &PgPool,
    owner: i64,
    csrf: &str,
    month: Option<&str>,
) -> Result<serde_json::Value, sqlx::Error> {
    let key = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT api_key_id FROM customer_accounts WHERE id=$1",
    )
    .bind(owner)
    .fetch_one(pool)
    .await?;
    let credits = if let Some(key) = key {
        Some(crate::billing::report(pool, key, month, false).await?)
    } else {
        None
    };
    let row=sqlx::query("SELECT a.id,a.name,a.email,a.domain,a.requested_plan,a.terms_version,a.business_address,
      to_char(a.cancel_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS cancel_at,
      k.id AS key_id,k.api_key,k.is_active,k.billing_plan,
      to_char((date_trunc('month',now() AT TIME ZONE 'UTC')+interval '1 month'),'YYYY-MM-DD')||'T00:00:00Z' AS period_end
      FROM customer_accounts a LEFT JOIN api_keys k ON k.id=a.api_key_id WHERE a.id=$1")
      .bind(owner).fetch_one(pool).await?;
    let active = row
        .get::<Option<String>, _>("billing_plan")
        .unwrap_or_else(|| "free".into());
    let requested: String = row.get("requested_plan");
    let cancel: Option<String> = row.get("cancel_at");
    let status = if cancel.is_some() {
        "cancelling"
    } else if active != "free" {
        "active"
    } else if requested != "free" {
        "pending_payment"
    } else {
        "free"
    };
    Ok(
        serde_json::json!({"customer":{"id":row.get::<i64,_>("id"),"name":row.get::<String,_>("name"),"email":row.get::<String,_>("email"),"domain":row.get::<String,_>("domain"),"business_address":row.get::<Option<String>,_>("business_address")},
      "api_key":row.get::<Option<String>,_>("api_key"),"api_key_id":row.get::<Option<i64>,_>("key_id"),"key_active":row.get::<Option<bool>,_>("is_active").unwrap_or(false),
      "subscription":{"active_plan":active,"selected_plan":requested,"status":status,"cancel_at":cancel,"period_end":row.get::<String,_>("period_end"),"payment_provider":"gopay","payments_enabled":false},"credits":credits,"csrf_token":csrf,"terms_version":row.get::<String,_>("terms_version")}),
    )
}
// Caller holds the API key lock first. All reservation/report/change paths use
// this ordering, so cancellation cannot race a charge or reset usage mid-month.
pub async fn apply_cancellation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    key: i64,
) -> Result<(), sqlx::Error> {
    let due = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM customer_accounts WHERE api_key_id=$1 AND cancel_at<=now() FOR UPDATE",
    )
    .bind(key)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(owner) = due {
        sqlx::query("UPDATE api_keys SET billing_plan='free',custom_credit_limit=NULL,updated_at=now() WHERE id=$1").bind(key).execute(&mut **tx).await?;
        sqlx::query(
            "UPDATE customer_accounts SET requested_plan='free',cancel_at=NULL WHERE id=$1",
        )
        .bind(owner)
        .execute(&mut **tx)
        .await?;
        sqlx::query("UPDATE api_credit_months SET plan_id='free',credit_limit=1000,monthly_price_eur=0 WHERE api_key_id=$1 AND period=date_trunc('month',now() AT TIME ZONE 'UTC')::date").bind(key).execute(&mut **tx).await?;
    }
    Ok(())
}
#[derive(Deserialize)]
pub struct SubscriptionRequest {
    plan: String,
}
pub async fn select_plan(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Json(request): Json<SubscriptionRequest>,
    req: &WebRequest<()>,
) -> WebResponse {
    if !matches!(request.plan.as_str(), "starter" | "growth" | "custom") {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_plan",
            "Choose Starter, Growth or Custom. Use cancellation to return to Free.",
        );
    }
    mutate_subscription(state, req, Some(&request.plan)).await
}
pub async fn cancel(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    req: &WebRequest<()>,
) -> WebResponse {
    mutate_subscription(state, req, None).await
}
async fn mutate_subscription(
    state: &AppState,
    req: &WebRequest<()>,
    selected: Option<&str>,
) -> WebResponse {
    let Some(pool) = state.auth.pool() else {
        return unavailable();
    };
    let (owner, _) = match identity(pool, req, true).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let result:Result<(),sqlx::Error>=async{let mut tx=pool.begin().await?;
        let key=sqlx::query_scalar::<_,Option<i64>>("SELECT api_key_id FROM customer_accounts WHERE id=$1").bind(owner).fetch_one(&mut *tx).await?;
        let active=if let Some(key)=key {
            sqlx::query("SELECT id FROM api_keys WHERE id=$1 FOR UPDATE").bind(key).execute(&mut *tx).await?;
            apply_cancellation(&mut tx,key).await?;
            sqlx::query_scalar::<_,String>("SELECT billing_plan FROM api_keys WHERE id=$1").bind(key).fetch_one(&mut *tx).await?
        }else{"free".into()};
        if let Some(plan)=selected {sqlx::query("UPDATE customer_accounts SET requested_plan=$2 WHERE id=$1").bind(owner).bind(plan).execute(&mut *tx).await?;}
        else if active=="free" {sqlx::query("UPDATE customer_accounts SET requested_plan='free',cancel_at=NULL WHERE id=$1").bind(owner).execute(&mut *tx).await?;}
        else {sqlx::query("UPDATE customer_accounts SET requested_plan='free',cancel_at=(date_trunc('month',now() AT TIME ZONE 'UTC')+interval '1 month') AT TIME ZONE 'UTC' WHERE id=$1 AND cancel_at IS NULL").bind(owner).execute(&mut *tx).await?;}
        tx.commit().await}.await;
    match result {
        Ok(()) => {
            state.auth.clear_authorization_cache();
            response(StatusCode::OK, serde_json::json!({"saved":true}))
        }
        Err(e) => database(e),
    }
}
pub async fn payment(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    req: &WebRequest<()>,
) -> WebResponse {
    let Some(pool) = state.auth.pool() else {
        return unavailable();
    };
    if let Err(r) = identity(pool, req, true).await {
        return *r;
    }
    error(
        StatusCode::NOT_IMPLEMENTED,
        "gopay_not_configured",
        "GoPay payments are not available yet. Your active plan has not changed and no payment was made.",
    )
}
pub async fn logout(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    req: &WebRequest<()>,
) -> WebResponse {
    let Some(pool) = state.auth.pool() else {
        return unavailable();
    };
    if let Err(r) = identity(pool, req, true).await {
        return *r;
    }
    match sqlx::query("DELETE FROM customer_sessions WHERE token_hash=$1")
        .bind(digest(cookie(req).unwrap_or_default()))
        .execute(pool)
        .await
    {
        Ok(_) => session_cookie(
            response(StatusCode::OK, serde_json::json!({"signed_out":true})),
            "",
            true,
        ),
        Err(e) => database(e),
    }
}
#[derive(Deserialize)]
pub struct PasswordRequest {
    current_password: String,
    new_password: String,
}
pub async fn change_password(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Json(request): Json<PasswordRequest>,
    req: &WebRequest<()>,
) -> WebResponse {
    let Some(pool) = state.auth.pool() else {
        return unavailable();
    };
    let (owner, _) = match identity(pool, req, true).await {
        Ok(v) => v,
        Err(r) => return *r,
    };
    if request.new_password.chars().count() < 12
        || request.new_password.len() > 1024
        || request.current_password.len() > 1024
    {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_password",
            "Use a password of at least 12 characters.",
        );
    }
    if let Some(r) = limit_response(pool, &format!("password:{owner}"), 10, 900).await {
        return r;
    }
    let old = match sqlx::query_scalar::<_, String>(
        "SELECT password_hash FROM customer_accounts WHERE id=$1",
    )
    .bind(owner)
    .fetch_one(pool)
    .await
    {
        Ok(hash) => hash,
        Err(e) => return database(e),
    };
    let compared_hash = old.clone();
    let new = {
        let Ok(slot) = HASH_SLOTS.try_acquire() else {
            return unavailable();
        };
        match tokio::task::spawn_blocking(move || {
            let _slot = slot;
            if !PasswordHash::new(&old).is_ok_and(|parsed| {
                Argon2::default()
                    .verify_password(request.current_password.as_bytes(), &parsed)
                    .is_ok()
            }) {
                None
            } else {
                Some(hash_password(&request.new_password))
            }
        })
        .await
        {
            Ok(Some(hash)) => hash,
            _ => {
                return error(
                    StatusCode::UNAUTHORIZED,
                    "invalid_login",
                    "Email or password is incorrect.",
                );
            }
        }
    };
    let raw = token();
    let result: Result<bool, sqlx::Error> = async {
        let mut tx = pool.begin().await?;
        let changed = sqlx::query(
            "UPDATE customer_accounts SET password_hash=$2 WHERE id=$1 AND password_hash=$3",
        )
        .bind(owner)
        .bind(new)
        .bind(compared_hash)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        if !changed {
            return Ok(false);
        }
        sqlx::query("DELETE FROM customer_sessions WHERE customer_id=$1")
            .bind(owner)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO customer_sessions(token_hash,customer_id) VALUES($1,$2)")
            .bind(digest(&raw))
            .bind(owner)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }
    .await;
    match result {
        Ok(true) => session_cookie(
            response(
                StatusCode::OK,
                serde_json::json!({"changed":true,"csrf_token":csrf(&raw)}),
            ),
            &raw,
            false,
        ),
        Ok(false) => error(
            StatusCode::CONFLICT,
            "password_changed",
            "Your password changed in another session. Sign in again.",
        ),
        Err(e) => database(e),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn domain_normalization_rejects_urls_suffixes_and_hostile_inputs() {
        assert_eq!(
            registration_domain("MyDomain.COM."),
            Some("mydomain.com".into())
        );
        assert_eq!(
            registration_domain("shop.mydomain.com"),
            Some("shop.mydomain.com".into())
        );
        for value in [
            "https://mydomain.com",
            "com",
            "co.uk",
            "*.mydomain.com",
            "mydomain.com/evil",
            "evil@mydomain.com",
            "127.0.0.1",
            "my_domain.com",
        ] {
            assert_eq!(registration_domain(value), None, "{value}");
        }
    }
    #[test]
    fn passwords_use_salted_argon2_and_tokens_are_not_database_secrets() {
        let first = hash_password("long test password");
        let second = hash_password("long test password");
        assert_ne!(first, second);
        assert!(first.starts_with("$argon2id$"));
        let parsed = PasswordHash::new(&first).unwrap();
        assert!(
            Argon2::default()
                .verify_password(b"long test password", &parsed)
                .is_ok()
        );
        assert!(
            Argon2::default()
                .verify_password(b"wrong", &parsed)
                .is_err()
        );
        let raw = token();
        assert_eq!(raw.len(), 43);
        assert_ne!(digest(&raw), csrf(&raw));
        assert!(!digest(&raw).contains(&raw));
    }
}
