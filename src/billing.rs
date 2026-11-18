//! Separate successful-request credit ledger with atomic monthly reservations.
//! Legacy HTTP contracts/counters are kept; chosen-package exhaustion is an
//! explicit HTTP 429. No automatic upgrades, payments or historical backfill.
use serde::Deserialize;
use serde::Serialize;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use xitca_web::handler::{json::Json, query::Query, state::StateRef};
use xitca_web::http::WebRequest;
use xitca_web::http::{StatusCode, WebResponse, header};

use crate::auth::AuthState;
use crate::http::{AppState, authorize_admin};
use crate::http::{json_ok, json_response};
use crate::platform::{KeyQuery, billing_month, key_id, supplied_key};

#[derive(Clone, Copy, Debug)]
pub enum Operation {
    Autocomplete,
    Validation,
    Company,
}

impl Operation {
    pub fn name(self) -> &'static str {
        match self {
            Self::Autocomplete => "autocomplete",
            Self::Validation => "validation",
            Self::Company => "company",
        }
    }
    pub fn cost(self) -> i64 {
        match self {
            Self::Autocomplete => 1,
            Self::Validation => 3,
            Self::Company => 5,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Plan {
    pub id: &'static str,
    pub name: &'static str,
    pub credit_limit: i64,
    pub monthly_price_eur: Option<i64>,
}

pub fn plan(id: &str, custom_limit: Option<i64>) -> Option<Plan> {
    let (id, name, credit_limit, monthly_price_eur) = match id {
        "free" => ("free", "Free", 1_000, Some(0)),
        "starter" => ("starter", "Starter", 100_000, Some(9)),
        "growth" => ("growth", "Growth", 1_000_000, Some(19)),
        "custom" => (
            "custom",
            "Custom",
            custom_limit.filter(|limit| *limit > 0)?,
            None,
        ),
        _ => return None,
    };
    Some(Plan {
        id,
        name,
        credit_limit,
        monthly_price_eur,
    })
}

pub fn suggested_plan(credits: i64) -> &'static str {
    match credits {
        ..=1_000 => "free",
        1_001..=100_000 => "starter",
        100_001..=1_000_000 => "growth",
        _ => "custom",
    }
}

pub fn public_config() -> serde_json::Value {
    serde_json::json!({"rates":{"autocomplete":1,"validation":3,"company":5},
        "plans":[plan("free",None),plan("starter",None),plan("growth",None)],
        "currency":"EUR","reset_timezone":"UTC"})
}

fn unavailable() -> WebResponse {
    json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        &serde_json::json!({
            "error":"billing_unavailable", "message":"Credit accounting is temporarily unavailable. Please retry later."
        }),
    )
}

pub struct Reservation {
    entry: Option<(PgPool, i64, i64)>,
}

impl Reservation {
    pub async fn reserve(
        auth: &AuthState,
        key_id: Option<i64>,
        operation: Operation,
        is_demo: bool,
    ) -> Result<Self, Box<WebResponse>> {
        if key_id.is_none() {
            return Ok(Self { entry: None });
        }
        let Some(pool) = auth.pool() else {
            return Err(Box::new(unavailable()));
        };
        if is_demo {
            return match demo_allowance(pool, operation.cost()).await {
                Ok(true) => Ok(Self { entry: None }),
                Ok(false) => Err(Box::new(no_store(json_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    &serde_json::json!({"error":"demo_limit_exceeded","message":"The shared public demo allowance has been reached for today. Request your own Free API key to continue.","account_url":"/account"}),
                )))),
                Err(error) => {
                    eprintln!("demo allowance failed: {error}");
                    Err(Box::new(unavailable()))
                }
            };
        }
        match reserve(pool, key_id.expect("checked"), operation).await {
            Ok(Ok(id)) => Ok(Self {
                entry: Some((pool.clone(), id, key_id.expect("checked"))),
            }),
            Ok(Err(response)) => Err(response),
            Err(error) => {
                eprintln!("credit reservation failed: {error}");
                Err(Box::new(unavailable()))
            }
        }
    }

    pub async fn finish(self, response: WebResponse) -> WebResponse {
        let Some((pool, reservation, key_id)) = self.entry else {
            return response;
        };
        if let Err(error) =
            finalize(&pool, reservation, key_id, response.status().is_success()).await
        {
            eprintln!("credit finalization failed: {error}");
            // A pending reservation is never an invoiceable charge. Abandoned
            // reservations expire after five minutes, above all upstream timeouts.
            return unavailable();
        }
        response
    }
}

// A separate, bounded public preview, never a substitute for customer keys.
// Attempts count here (including errors); this is abuse control, not billing.
// A conditional upsert serializes concurrent calls without trusting IP headers.
async fn demo_allowance(pool: &PgPool, credits: i64) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar::<_, i64>(
        "INSERT INTO api_demo_allowance (usage_date,credits) VALUES ((now() AT TIME ZONE 'UTC')::date,$1)
         ON CONFLICT (usage_date) DO UPDATE SET credits=api_demo_allowance.credits+excluded.credits
         WHERE api_demo_allowance.credits+excluded.credits <= 10000 RETURNING credits",
    ).bind(credits).fetch_optional(pool).await?.is_some())
}

async fn expire_pending(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    key_id: i64,
) -> Result<(), sqlx::Error> {
    // The key row is locked by every reserve/finalize/plan update. This makes
    // expiry and counter adjustment safe across processes and concurrent calls.
    sqlx::query(
        "WITH expired AS (
        UPDATE api_credit_reservations SET state='refunded', finalized_at=now()
        WHERE api_key_id=$1 AND state='pending' AND created_at < now()-interval '5 minutes'
        RETURNING period, credits
    ), totals AS (SELECT period, sum(credits)::bigint AS credits FROM expired GROUP BY period)
    UPDATE api_credit_months m SET reserved_credits=m.reserved_credits-t.credits
    FROM totals t WHERE m.api_key_id=$1 AND m.period=t.period",
    )
    .bind(key_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn reserve(
    pool: &PgPool,
    key_id: i64,
    operation: Operation,
) -> Result<Result<i64, Box<WebResponse>>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let row=sqlx::query("SELECT billing_plan, custom_credit_limit, label FROM api_keys WHERE id=$1 AND is_active FOR UPDATE")
        .bind(key_id).fetch_optional(&mut *tx).await?;
    let Some(row) = row else {
        return Ok(Err(Box::new(json_response(
            StatusCode::UNAUTHORIZED,
            &serde_json::json!({"error":"invalid_api_key","message":"API key is invalid or inactive"}),
        ))));
    };
    let chosen = plan(
        row.get::<&str, _>("billing_plan"),
        row.get("custom_credit_limit"),
    );
    let Some(chosen) = chosen else {
        return Ok(Err(Box::new(unavailable())));
    };
    expire_pending(&mut tx, key_id).await?;
    sqlx::query(
        "INSERT INTO api_credit_months (api_key_id,period,key_label,plan_id,credit_limit,monthly_price_eur)
        VALUES ($1,date_trunc('month',now() AT TIME ZONE 'UTC')::date,$2,$3,$4,$5) ON CONFLICT DO NOTHING",
    )
    .bind(key_id)
    .bind(row.get::<Option<String>, _>("label"))
    .bind(chosen.id).bind(chosen.credit_limit).bind(chosen.monthly_price_eur)
    .execute(&mut *tx)
    .await?;
    let period=sqlx::query("SELECT period::text AS period, used_credits, reserved_credits,
        to_char(period+interval '1 month','YYYY-MM-DD')||'T00:00:00Z' AS resets_at,
        greatest(1,extract(epoch FROM ((period+interval '1 month') AT TIME ZONE 'UTC')-now())::bigint) AS retry_after
        FROM api_credit_months WHERE api_key_id=$1 AND period=date_trunc('month',now() AT TIME ZONE 'UTC')::date")
        .bind(key_id).fetch_one(&mut *tx).await?;
    let used: i64 = period.get("used_credits");
    let reserved: i64 = period.get("reserved_credits");
    if used
        .checked_add(reserved)
        .and_then(|total| total.checked_add(operation.cost()))
        .is_none_or(|total| total > chosen.credit_limit)
    {
        let retry_after = period.get::<i64, _>("retry_after");
        let quota_payload = serde_json::json!({
            "error":"credit_limit_exceeded", "message":"This request would exceed your chosen package's monthly credit limit. Requests are blocked until the next month or an approved package change. No overage is charged.",
            "plan":chosen.id,"credit_limit":chosen.credit_limit,"used_credits":used,"reserved_credits":reserved,
            "requested_credits":operation.cost(),"remaining_credits":(chosen.credit_limit-used-reserved).max(0),
            "resets_at":period.get::<String,_>("resets_at")
        });
        tx.commit().await?;
        let mut response = json_response(StatusCode::TOO_MANY_REQUESTS, &quota_payload);
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
        response.headers_mut().insert(
            header::RETRY_AFTER,
            retry_after.to_string().parse().expect("integer header"),
        );
        return Ok(Err(Box::new(response)));
    }
    let id = sqlx::query_scalar(
        "INSERT INTO api_credit_reservations (api_key_id,period,operation,credits)
        VALUES ($1,$2::text::date,$3,$4) RETURNING id",
    )
    .bind(key_id)
    .bind(period.get::<String, _>("period"))
    .bind(operation.name())
    .bind(operation.cost() as i16)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE api_credit_months SET reserved_credits=reserved_credits+$3
        WHERE api_key_id=$1 AND period=$2::text::date",
    )
    .bind(key_id)
    .bind(period.get::<String, _>("period"))
    .bind(operation.cost())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Ok(id))
}

async fn finalize(
    pool: &PgPool,
    reservation: i64,
    key_id: i64,
    success: bool,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    // Delete/disable operations may have removed the credential. The reservation
    // and financial history still exist and must be finalized independently.
    sqlx::query("SELECT id FROM api_keys WHERE id=$1 FOR UPDATE")
        .bind(key_id)
        .fetch_optional(&mut *tx)
        .await?;
    let row=sqlx::query("UPDATE api_credit_reservations SET state=$2,finalized_at=now()
        WHERE id=$1 AND state='pending' RETURNING api_key_id,period::text AS period,operation,credits::bigint AS credits,(created_at AT TIME ZONE 'UTC')::date::text AS usage_date")
        .bind(reservation).bind(if success {"charged"} else {"refunded"}).fetch_optional(&mut *tx).await?;
    if let Some(row) = row {
        let cost: i64 = row.get("credits");
        sqlx::query("UPDATE api_credit_months SET reserved_credits=reserved_credits-$3,used_credits=used_credits+$4
            WHERE api_key_id=$1 AND period=$2::text::date")
            .bind(key_id).bind(row.get::<String,_>("period")).bind(cost).bind(if success {cost} else {0})
            .execute(&mut *tx).await?;
        if success {
            sqlx::query("INSERT INTO api_credit_usage_daily (api_key_id,usage_date,operation,request_count,credits)
                VALUES ($1,$4::text::date,$2,1,$3)
                ON CONFLICT (api_key_id,usage_date,operation) DO UPDATE SET
                    request_count=api_credit_usage_daily.request_count+1, credits=api_credit_usage_daily.credits+excluded.credits")
                .bind(key_id).bind(row.get::<String,_>("operation")).bind(cost).bind(row.get::<String,_>("usage_date")).execute(&mut *tx).await?;
        }
    }
    tx.commit().await
}

fn error(status: StatusCode, code: &str, message: &str) -> WebResponse {
    json_response(status, &serde_json::json!({"error":code,"message":message}))
}

#[derive(Serialize)]
struct OperationUsage {
    operation: String,
    requests: i64,
    credits: i64,
}

#[derive(Serialize)]
struct CreditReport {
    api_key_id: i64,
    label: Option<String>,
    archived: bool,
    month: String,
    plan: Plan,
    used_credits: i64,
    reserved_credits: i64,
    remaining_credits: i64,
    is_demo: bool,
    resets_at: String,
    metering_started_at: String,
    operations: Vec<OperationUsage>,
    suggested_plan: &'static str,
}

async fn report(
    pool: &PgPool,
    id: i64,
    month: Option<&str>,
    demo: bool,
) -> Result<CreditReport, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let key = sqlx::query(
        "SELECT label,billing_plan,custom_credit_limit FROM api_keys WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    expire_pending(&mut tx, id).await?;
    let dates=sqlx::query("SELECT to_char(coalesce($1::text::date,date_trunc('month',now() AT TIME ZONE 'UTC')::date),'YYYY-MM') AS month,
        to_char(coalesce($1::text::date,date_trunc('month',now() AT TIME ZONE 'UTC')::date)+interval '1 month','YYYY-MM-DD')||'T00:00:00Z' AS resets_at,
        coalesce($1::text::date,date_trunc('month',now() AT TIME ZONE 'UTC')::date)=date_trunc('month',now() AT TIME ZONE 'UTC')::date AS is_current,
        (SELECT to_char(started_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') FROM credit_metering_config WHERE singleton) AS started_at")
        .bind(month).fetch_one(&mut *tx).await?;
    let month_label: String = dates.get("month");
    let period = format!("{month_label}-01");
    let balance = sqlx::query(
        "SELECT * FROM api_credit_months WHERE api_key_id=$1 AND period=$2::text::date",
    )
    .bind(id)
    .bind(&period)
    .fetch_optional(&mut *tx)
    .await?;
    let chosen = if let Some(row) = &balance {
        plan(row.get::<&str, _>("plan_id"), Some(row.get("credit_limit")))
    } else if !dates.get::<bool, _>("is_current") {
        Some(Plan {
            id: "unrecorded",
            name: "No billing record",
            credit_limit: 0,
            monthly_price_eur: Some(0),
        })
    } else if let Some(row) = &key {
        plan(
            row.get::<&str, _>("billing_plan"),
            row.get("custom_credit_limit"),
        )
    } else {
        None
    }
    .ok_or(sqlx::Error::RowNotFound)?;
    let used: i64 = balance.as_ref().map_or(0, |row| row.get("used_credits"));
    let reserved: i64 = balance
        .as_ref()
        .map_or(0, |row| row.get("reserved_credits"));
    let operations=sqlx::query("SELECT operation,sum(request_count)::bigint AS requests,sum(credits)::bigint AS credits
        FROM api_credit_usage_daily WHERE api_key_id=$1 AND usage_date >= $2::text::date AND usage_date < $2::text::date+interval '1 month'
        GROUP BY operation ORDER BY operation")
        .bind(id).bind(&period).fetch_all(&mut *tx).await?.into_iter().map(|row|OperationUsage {
            operation:row.get("operation"),requests:row.get("requests"),credits:row.get("credits")
        }).collect();
    let label = key
        .as_ref()
        .and_then(|row| row.get("label"))
        .or_else(|| balance.as_ref().and_then(|row| row.get("key_label")));
    let mut chosen = chosen;
    if let Some(row) = &balance {
        chosen.monthly_price_eur = row.get("monthly_price_eur");
    }
    let result = CreditReport {
        api_key_id: id,
        label,
        archived: key.is_none(),
        month: month_label,
        plan: chosen,
        used_credits: used,
        reserved_credits: reserved,
        remaining_credits: (chosen.credit_limit - used - reserved).max(0),
        is_demo: demo,
        resets_at: dates.get("resets_at"),
        metering_started_at: dates.get("started_at"),
        operations,
        suggested_plan: suggested_plan(used),
    };
    tx.commit().await?;
    Ok(result)
}

fn no_store(mut response: WebResponse) -> WebResponse {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

pub async fn customer_credits(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Query(query): Query<KeyQuery>,
    req: &WebRequest<()>,
) -> WebResponse {
    let Some(key) = supplied_key(&query, req) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "missing_api_key",
            "Supply your API key",
        );
    };
    let month = match billing_month(query.month.as_deref()) {
        Ok(month) => month,
        Err(()) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_month",
                "Use month=YYYY-MM",
            );
        }
    };
    let Some(pool) = state.auth.pool() else {
        return unavailable();
    };
    let id = match key_id(pool, key).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            return error(
                StatusCode::UNAUTHORIZED,
                "invalid_api_key",
                "API key is invalid or inactive",
            );
        }
        Err(_) => return unavailable(),
    };
    match report(pool, id, month.as_deref(), key == state.demo_api_key).await {
        Ok(report) => no_store(json_ok(report)),
        Err(error) => {
            eprintln!("credit report failed: {error}");
            unavailable()
        }
    }
}

pub async fn admin_credits(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Query(query): Query<KeyQuery>,
    req: &WebRequest<()>,
) -> WebResponse {
    if let Err((status, message)) = authorize_admin(state, req) {
        return error(status, "admin_error", message);
    }
    let month = match billing_month(query.month.as_deref()) {
        Ok(month) => month,
        Err(()) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_month",
                "Use month=YYYY-MM",
            );
        }
    };
    let Some(pool) = state.auth.pool() else {
        return unavailable();
    };
    let ids=match sqlx::query_scalar::<_,i64>("SELECT id FROM api_keys UNION SELECT api_key_id FROM api_credit_months WHERE period=coalesce($1::text::date,date_trunc('month',now() AT TIME ZONE 'UTC')::date) ORDER BY 1")
        .bind(month.as_deref()).fetch_all(pool).await {Ok(ids)=>ids,Err(_)=>return unavailable()};
    let demo_id = key_id(pool, &state.demo_api_key).await.ok().flatten();
    let mut reports = Vec::new();
    for id in ids {
        match report(pool, id, month.as_deref(), demo_id == Some(id)).await {
            Ok(row) => reports.push(row),
            Err(_) => return unavailable(),
        }
    }
    no_store(json_ok(reports))
}

#[derive(Deserialize)]
pub struct PlanUpdate {
    id: i64,
    plan: String,
    custom_credit_limit: Option<i64>,
}

pub async fn set_plan(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Json(request): Json<PlanUpdate>,
    req: &WebRequest<()>,
) -> WebResponse {
    if let Err((status, message)) = authorize_admin(state, req) {
        return error(status, "admin_error", message);
    }
    let Some(chosen) = plan(&request.plan, request.custom_credit_limit) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_plan",
            "Choose free, starter, growth, or a custom package with a positive hard limit",
        );
    };
    let Some(pool) = state.auth.pool() else {
        return unavailable();
    };
    match update_plan(pool, request.id, chosen).await {
        Ok(PlanChange::Applied) => json_ok(chosen),
        Ok(PlanChange::Missing) => {
            error(StatusCode::NOT_FOUND, "not_found", "API key was not found")
        }
        Ok(PlanChange::LimitBelowUsage) => error(
            StatusCode::CONFLICT,
            "package_limit_below_usage",
            "The chosen limit is below this month's used and pending credits. Keep a larger package or request the downgrade after the monthly reset.",
        ),
        Err(_) => unavailable(),
    }
}

#[derive(Debug, PartialEq)]
enum PlanChange {
    Applied,
    Missing,
    LimitBelowUsage,
}
async fn update_plan(pool: &PgPool, id: i64, chosen: Plan) -> Result<PlanChange, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let result = sqlx::query(
        "UPDATE api_keys SET billing_plan=$2,custom_credit_limit=$3,updated_at=now() WHERE id=$1",
    )
    .bind(id)
    .bind(chosen.id)
    .bind(if chosen.id == "custom" {
        Some(chosen.credit_limit)
    } else {
        None
    })
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(PlanChange::Missing);
    }
    let current=sqlx::query_scalar::<_,i64>("SELECT used_credits+reserved_credits FROM api_credit_months WHERE api_key_id=$1 AND period=date_trunc('month',now() AT TIME ZONE 'UTC')::date")
        .bind(id).fetch_optional(&mut *tx).await?.unwrap_or(0);
    if current > chosen.credit_limit {
        return Ok(PlanChange::LimitBelowUsage);
    }
    sqlx::query(
        "INSERT INTO api_credit_months (api_key_id,period,key_label,plan_id,credit_limit,monthly_price_eur)
        SELECT id,date_trunc('month',now() AT TIME ZONE 'UTC')::date,label,$2,$3,$4 FROM api_keys WHERE id=$1
        ON CONFLICT (api_key_id,period) DO UPDATE SET plan_id=excluded.plan_id,credit_limit=excluded.credit_limit,
            monthly_price_eur=excluded.monthly_price_eur",
    )
    .bind(id)
    .bind(chosen.id)
    .bind(chosen.credit_limit)
    .bind(chosen.monthly_price_eur)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(PlanChange::Applied)
}

#[derive(Deserialize)]
pub struct AccessRequest {
    name: String,
    email: String,
    domain: String,
    plan: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    website: String,
}

pub async fn request_access(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Json(request): Json<AccessRequest>,
    req: &WebRequest<()>,
) -> WebResponse {
    if !request.website.is_empty() {
        return json_ok(serde_json::json!({"saved":true}));
    }
    let email = request.email.trim().to_ascii_lowercase();
    let domain = crate::auth::normalize_domain(&request.domain);
    if request.name.trim().is_empty()
        || request.name.chars().count() > 200
        || email.len() > 254
        || email.split('@').count() != 2
        || email.starts_with('@')
        || !email
            .split('@')
            .nth(1)
            .is_some_and(|v| v.contains('.') && !v.ends_with('.'))
        || email.chars().any(char::is_whitespace)
        || request.message.chars().count() > 4000
        || domain.as_ref().is_none_or(|domain| {
            domain.len() > 253
                || !domain.contains('.')
                || domain.contains('/')
                || domain.chars().any(char::is_whitespace)
        })
        || !matches!(
            request.plan.as_str(),
            "free" | "starter" | "growth" | "custom"
        )
    {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Enter a name, valid email, plain website domain, package, and a message of at most 4,000 characters",
        );
    }
    let Some(pool) = state.auth.pool() else {
        return unavailable();
    };
    let query = KeyQuery {
        api_key: None,
        month: None,
    };
    let owner = if let Some(key) = supplied_key(&query, req) {
        match key_id(pool, key).await {
            Ok(Some(id)) => Some(id),
            _ => {
                return error(
                    StatusCode::UNAUTHORIZED,
                    "invalid_api_key",
                    "The supplied API key is invalid or inactive",
                );
            }
        }
    } else {
        None
    };
    // Deduplicate repeated browser submissions, without leaking whether an email
    // already has a request. Plan requests never activate paid access themselves.
    let result = sqlx::query(
        "INSERT INTO api_access_requests(name,email,domain,plan,message,api_key_id)
        VALUES ($1,$2,$3,$4,$5,$6)
        ON CONFLICT (email,domain,(coalesce(api_key_id,0))) WHERE state='pending'
        DO UPDATE SET name=excluded.name,plan=excluded.plan,message=excluded.message,created_at=now()",
    )
    .bind(request.name.trim())
    .bind(email)
    .bind(domain)
    .bind(request.plan)
    .bind(request.message.trim())
    .bind(owner)
    .execute(pool)
    .await;
    match result {
        Ok(_) => json_ok(
            serde_json::json!({"saved":true,"message":"Your request has been saved for the Addresswise team. No package change or payment has been made."}),
        ),
        Err(_) => unavailable(),
    }
}

pub async fn access_requests(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    req: &WebRequest<()>,
) -> WebResponse {
    if let Err((status, message)) = authorize_admin(state, req) {
        return error(status, "admin_error", message);
    }
    let Some(pool) = state.auth.pool() else {
        return unavailable();
    };
    match sqlx::query("SELECT id,name,email,domain,plan,message,api_key_id,state,to_char(created_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at FROM api_access_requests ORDER BY (state='pending') DESC,id DESC LIMIT 200").fetch_all(pool).await {
        Ok(rows)=>no_store(json_ok(rows.into_iter().map(|row|serde_json::json!({"id":row.get::<i64,_>("id"),"name":row.get::<String,_>("name"),"email":row.get::<String,_>("email"),"domain":row.get::<String,_>("domain"),"plan":row.get::<String,_>("plan"),"message":row.get::<String,_>("message"),"api_key_id":row.get::<Option<i64>,_>("api_key_id"),"state":row.get::<String,_>("state"),"created_at":row.get::<String,_>("created_at")})).collect::<Vec<_>>())),Err(_)=>unavailable()
    }
}

#[derive(Deserialize)]
pub struct HandledRequest {
    id: i64,
}
pub async fn handle_request(
    StateRef(state): StateRef<'_, Arc<AppState>>,
    Json(request): Json<HandledRequest>,
    req: &WebRequest<()>,
) -> WebResponse {
    if let Err((status, message)) = authorize_admin(state, req) {
        return error(status, "admin_error", message);
    }
    let Some(pool) = state.auth.pool() else {
        return unavailable();
    };
    match sqlx::query("UPDATE api_access_requests SET state='handled',handled_at=now() WHERE id=$1")
        .bind(request.id)
        .execute(pool)
        .await
    {
        Ok(result) if result.rows_affected() > 0 => json_ok(serde_json::json!({"handled":true})),
        Ok(_) => error(StatusCode::NOT_FOUND, "not_found", "Request was not found"),
        Err(_) => unavailable(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn test_key(pool: &PgPool, limit: i64) -> i64 {
        sqlx::query_scalar("INSERT INTO api_keys(api_key,label,billing_plan,custom_credit_limit) VALUES($1,'Credit accounting test','custom',$2) RETURNING id")
            .bind(format!("credit-test-{}",rand::random::<u64>())).bind(limit).fetch_one(pool).await.unwrap()
    }

    #[tokio::test]
    #[ignore = "requires isolated CREDIT_TEST_DATABASE_URL"]
    async fn postgres_credit_accounting() {
        let pool =
            PgPool::connect(&std::env::var("CREDIT_TEST_DATABASE_URL").expect("isolated test DB"))
                .await
                .unwrap();
        sqlx::raw_sql(include_str!("../db/0002_api_auth.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../db/0005_credit_billing.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let key = test_key(&pool, 10).await;
        for operation in [Operation::Autocomplete, Operation::Validation] {
            let id = reserve(&pool, key, operation).await.unwrap().ok().unwrap();
            finalize(&pool, id, key, true).await.unwrap();
        }
        let company = reserve(&pool, key, Operation::Company)
            .await
            .unwrap()
            .ok()
            .unwrap();
        assert_eq!(
            reserve(&pool, key, Operation::Validation)
                .await
                .unwrap()
                .err()
                .unwrap()
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        finalize(&pool, company, key, false).await.unwrap();
        let summary = report(&pool, key, None, false).await.unwrap();
        assert_eq!(
            (
                summary.used_credits,
                summary.reserved_credits,
                summary.remaining_credits
            ),
            (4, 0, 6)
        );
        assert_eq!(
            summary
                .operations
                .iter()
                .map(|row| row.requests)
                .sum::<i64>(),
            2
        );
        assert_eq!(
            summary
                .operations
                .iter()
                .map(|row| row.credits)
                .sum::<i64>(),
            4
        );

        // Concurrent calls must reserve the budget atomically, even before any
        // response is completed. There must be exactly ten accepted calls.
        let concurrent = test_key(&pool, 10).await;
        let mut tasks = Vec::new();
        for _ in 0..30 {
            let pool = pool.clone();
            tasks.push(tokio::spawn(async move {
                reserve(&pool, concurrent, Operation::Autocomplete)
                    .await
                    .unwrap()
                    .ok()
            }));
        }
        let mut ids = Vec::new();
        for task in tasks {
            if let Some(id) = task.await.unwrap() {
                ids.push(id);
            }
        }
        assert_eq!(ids.len(), 10);
        let summary = report(&pool, concurrent, None, false).await.unwrap();
        assert_eq!((summary.used_credits, summary.reserved_credits), (0, 10));
        for id in &ids {
            finalize(&pool, *id, concurrent, true).await.unwrap();
        }
        // Finalization is idempotent: retries cannot charge or refund twice.
        finalize(&pool, ids[0], concurrent, true).await.unwrap();
        finalize(&pool, ids[0], concurrent, false).await.unwrap();
        let summary = report(&pool, concurrent, None, false).await.unwrap();
        assert_eq!((summary.used_credits, summary.reserved_credits), (10, 0));
        assert_eq!(summary.operations[0].requests, 10);

        // A pending request that cannot finish is refunded on expiry. Completed
        // responses after expiry cannot create a second charge.
        let abandoned = test_key(&pool, 10).await;
        let pending = reserve(&pool, abandoned, Operation::Validation)
            .await
            .unwrap()
            .ok()
            .unwrap();
        sqlx::query(
            "UPDATE api_credit_reservations SET created_at=now()-interval '6 minutes' WHERE id=$1",
        )
        .bind(pending)
        .execute(&pool)
        .await
        .unwrap();
        let next = reserve(&pool, abandoned, Operation::Company)
            .await
            .unwrap()
            .ok()
            .unwrap();
        let summary = report(&pool, abandoned, None, false).await.unwrap();
        assert_eq!((summary.used_credits, summary.reserved_credits), (0, 5));
        finalize(&pool, pending, abandoned, true).await.unwrap();
        finalize(&pool, next, abandoned, false).await.unwrap();
        assert_eq!(
            report(&pool, abandoned, None, false)
                .await
                .unwrap()
                .used_credits,
            0
        );

        // A previous exhausted month does not consume the current allowance.
        let rollover = test_key(&pool, 10).await;
        sqlx::query("INSERT INTO api_credit_months(api_key_id,period,plan_id,credit_limit,used_credits) VALUES($1,(date_trunc('month',now() AT TIME ZONE 'UTC')-interval '1 month')::date,'custom',10,10)")
            .bind(rollover).execute(&pool).await.unwrap();
        let id = reserve(&pool, rollover, Operation::Autocomplete)
            .await
            .unwrap()
            .ok()
            .unwrap();
        finalize(&pool, id, rollover, true).await.unwrap();
        assert_eq!(
            report(&pool, rollover, None, false)
                .await
                .unwrap()
                .used_credits,
            1
        );
        update_plan(&pool, rollover, plan("starter", None).unwrap())
            .await
            .unwrap();
        let summary = report(&pool, rollover, None, false).await.unwrap();
        assert_eq!(summary.used_credits, 1);
        assert_eq!(summary.plan.credit_limit, 100_000);
        assert_eq!(summary.plan.monthly_price_eur, Some(9));

        assert_eq!(
            update_plan(&pool, rollover, plan("custom", Some(1)).unwrap())
                .await
                .unwrap(),
            PlanChange::Applied
        );
        let second = reserve(&pool, rollover, Operation::Autocomplete)
            .await
            .unwrap();
        assert!(second.is_err());
        let paid_unused = test_key(&pool, 10).await;
        update_plan(&pool, paid_unused, plan("growth", None).unwrap())
            .await
            .unwrap();
        let paid = report(&pool, paid_unused, None, false).await.unwrap();
        assert_eq!(paid.plan.monthly_price_eur, Some(19));
        let historical = report(&pool, paid_unused, Some("2000-01-01"), false)
            .await
            .unwrap();
        assert_eq!(historical.plan.monthly_price_eur, Some(0));
        assert_eq!(historical.plan.id, "unrecorded");
        assert_eq!(
            update_plan(&pool, key, plan("custom", Some(3)).unwrap())
                .await
                .unwrap(),
            PlanChange::LimitBelowUsage
        );
        assert_eq!(
            report(&pool, key, None, false)
                .await
                .unwrap()
                .plan
                .credit_limit,
            10
        );

        // Revoking/deleting a credential must retain its financial record.
        sqlx::query("DELETE FROM api_keys WHERE id=$1")
            .bind(concurrent)
            .execute(&pool)
            .await
            .unwrap();
        let archived = report(&pool, concurrent, None, false).await.unwrap();
        assert!(archived.archived);
        assert_eq!(archived.used_credits, 10);

        // Public demo attempts have their own atomic cap, without consuming
        // any customer's allowance or offering an unlimited billing bypass.
        sqlx::query("INSERT INTO api_demo_allowance(usage_date,credits) VALUES ((now() AT TIME ZONE 'UTC')::date,9993) ON CONFLICT(usage_date) DO UPDATE SET credits=9993")
            .execute(&pool).await.unwrap();
        let mut attempts = Vec::new();
        for _ in 0..30 {
            let pool = pool.clone();
            attempts.push(tokio::spawn(async move {
                demo_allowance(&pool, 1).await.unwrap()
            }));
        }
        let mut accepted = 0;
        for attempt in attempts {
            accepted += i32::from(attempt.await.unwrap());
        }
        assert_eq!(accepted, 7);
        assert!(!demo_allowance(&pool, 3).await.unwrap());
    }
    #[test]
    fn credit_costs_and_tier_boundaries_are_explicit() {
        assert_eq!(
            (
                Operation::Autocomplete.cost(),
                Operation::Validation.cost(),
                Operation::Company.cost()
            ),
            (1, 3, 5)
        );
        for (credits, expected) in [
            (0, "free"),
            (1_000, "free"),
            (1_001, "starter"),
            (100_000, "starter"),
            (100_001, "growth"),
            (1_000_000, "growth"),
            (1_000_001, "custom"),
        ] {
            assert_eq!(suggested_plan(credits), expected);
        }
        assert!(plan("custom", None).is_none());
        assert!(plan("custom", Some(-1)).is_none());
    }
}
