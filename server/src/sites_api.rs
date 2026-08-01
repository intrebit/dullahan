//! `/sites` — the tenant registry admin surface. Operator scope only.
//!
//! Exists so tenants can be created, suspended and rotated without a `psql`
//! shell or a service restart, which is the whole reason the registry is a table
//! rather than an env var. Every mutation refreshes the in-memory registry
//! before responding, so a rotated token is dead on this node before the new one
//! reaches the caller.
//!
//! Deliberately has **no CORS layer at all** — not `Any`, not an allowlist —
//! so a browser cannot reach it cross-origin under any configuration. This
//! extends the reasoning already recorded for `cors_products` (which withholds
//! `AUTHORIZATION` so a page can't preflight an admin write); do not "fix" the
//! missing layer.

use crate::sites;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

fn internal(context: &'static str, err: &sqlx::Error) -> StatusCode {
    tracing::error!(error = %err, "{context}");
    StatusCode::INTERNAL_SERVER_ERROR
}

fn is_pg_code(err: &sqlx::Error, code: &str) -> bool {
    err.as_database_error().and_then(|e| e.code()).as_deref() == Some(code)
}

/// A tenant as returned by reads. Never carries a credential.
///
/// Separate struct from [`SiteCreated`] rather than one type with an
/// `Option<String> token`, so there is no code path where serializing a stored
/// row could emit a secret.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct SiteView {
    pub id: String,
    pub name: String,
    pub domain: Option<String>,
    pub contact_to: Option<String>,
    pub email_from: Option<String>,
    pub email_from_name: Option<String>,
    pub active: bool,
    /// Last four characters of the current token, so an operator can tell which
    /// credential is deployed where without holding the plaintext.
    pub token_last4: Option<String>,
    pub token_rotated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
}

const VIEW_COLS: &str = "id, name, domain, contact_to, email_from, email_from_name, active, \
     token_last4, token_rotated_at, created_at, updated_at";

/// The only response shape that ever contains a token.
#[derive(Debug, Serialize)]
pub struct SiteCreated {
    #[serde(flatten)]
    pub site: SiteView,
    /// Shown exactly once. Not recoverable — rotation is the recovery path.
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub sites: Vec<SiteView>,
    pub total: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateSite {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub contact_to: Option<String>,
    #[serde(default)]
    pub email_from: Option<String>,
    #[serde(default)]
    pub email_from_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSite {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub contact_to: Option<String>,
    #[serde(default)]
    pub email_from: Option<String>,
    #[serde(default)]
    pub email_from_name: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
}

/// Reload the registry after a mutation. A failure here is logged but does not
/// fail the request: the write already committed, and the periodic refresh will
/// pick it up — reporting 500 would wrongly suggest the change was rejected.
async fn refresh(state: &AppState) {
    if let Err(err) = sites::refresh(&state.pool, &state.sites).await {
        tracing::error!(error = %err, "site registry refresh after write failed");
    }
}

/// Reject an address that could smuggle a header into outbound mail. The DB has
/// a matching CHECK; this produces a 400 instead of a 500.
fn mail_fields_valid(contact_to: Option<&str>, email_from: Option<&str>) -> bool {
    [contact_to, email_from]
        .into_iter()
        .flatten()
        .all(crate::contact::email_looks_valid)
}

/// `GET /sites` — operator only.
pub async fn list(State(state): State<AppState>) -> Result<Json<ListResponse>, StatusCode> {
    let sites: Vec<SiteView> =
        sqlx::query_as(&format!("SELECT {VIEW_COLS} FROM sites ORDER BY id ASC"))
            .fetch_all(&state.pool)
            .await
            .map_err(|e| internal("sites list failed", &e))?;

    let total = sites.len() as i64;
    Ok(Json(ListResponse { sites, total }))
}

/// `GET /sites/:id` — operator only.
pub async fn get_site(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SiteView>, StatusCode> {
    let site: Option<SiteView> =
        sqlx::query_as(&format!("SELECT {VIEW_COLS} FROM sites WHERE id = $1"))
            .bind(&id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| internal("sites get failed", &e))?;
    site.map(Json).ok_or(StatusCode::NOT_FOUND)
}

/// `POST /sites` — operator only. Generates the token; there is deliberately no
/// way to supply one (see [`sites::generate_token`]).
pub async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateSite>,
) -> Result<(StatusCode, Json<SiteCreated>), StatusCode> {
    let id = body.id.trim();
    if !sites::site_valid(id) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !mail_fields_valid(body.contact_to.as_deref(), body.email_from.as_deref()) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let token = sites::generate_token();
    let digest = sites::token_digest(&token);

    let site: SiteView = sqlx::query_as(&format!(
        "INSERT INTO sites (id, name, domain, contact_to, email_from, email_from_name,
                            admin_token_hash, token_last4, token_rotated_at)
         VALUES ($1, COALESCE($2::text, ''), $3, $4, $5, $6, $7, $8, now())
         RETURNING {VIEW_COLS}"
    ))
    .bind(id)
    .bind(body.name.as_deref().map(str::trim))
    .bind(body.domain.as_deref().map(str::trim))
    .bind(body.contact_to.as_deref().map(str::trim))
    .bind(body.email_from.as_deref().map(str::trim))
    .bind(body.email_from_name.as_deref().map(str::trim))
    .bind(&digest[..])
    .bind(sites::token_last4(&token))
    .fetch_one(&state.pool)
    .await
    .map_err(|e| {
        if is_pg_code(&e, "23505") {
            StatusCode::CONFLICT
        } else if is_pg_code(&e, "23514") {
            // A CHECK constraint rejected the id or an address.
            StatusCode::BAD_REQUEST
        } else {
            internal("sites create failed", &e)
        }
    })?;

    refresh(&state).await;
    Ok((StatusCode::CREATED, Json(SiteCreated { site, token })))
}

/// `PATCH /sites/:id` — operator only. Cannot touch the token; use
/// [`rotate_token`].
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateSite>,
) -> Result<Json<SiteView>, StatusCode> {
    if !mail_fields_valid(body.contact_to.as_deref(), body.email_from.as_deref()) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let site: Option<SiteView> = sqlx::query_as(&format!(
        "UPDATE sites SET
             name            = COALESCE($2::text, name),
             domain          = COALESCE($3::text, domain),
             contact_to      = COALESCE($4::text, contact_to),
             email_from      = COALESCE($5::text, email_from),
             email_from_name = COALESCE($6::text, email_from_name),
             active          = COALESCE($7::boolean, active),
             updated_at      = now()
         WHERE id = $1
         RETURNING {VIEW_COLS}"
    ))
    .bind(&id)
    .bind(body.name.as_deref().map(str::trim))
    .bind(body.domain.as_deref().map(str::trim))
    .bind(body.contact_to.as_deref().map(str::trim))
    .bind(body.email_from.as_deref().map(str::trim))
    .bind(body.email_from_name.as_deref().map(str::trim))
    .bind(body.active)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
        if is_pg_code(&e, "23514") {
            StatusCode::BAD_REQUEST
        } else {
            internal("sites update failed", &e)
        }
    })?;

    let site = site.ok_or(StatusCode::NOT_FOUND)?;
    refresh(&state).await;
    Ok(Json(site))
}

/// `POST /sites/:id/token` — operator only. Hard cutover: the old token stops
/// working the instant this returns. There is no grace window; a tenant with the
/// old token deployed will get 401s until they redeploy.
pub async fn rotate_token(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SiteCreated>, StatusCode> {
    let token = sites::generate_token();
    let digest = sites::token_digest(&token);

    let site: Option<SiteView> = sqlx::query_as(&format!(
        "UPDATE sites
            SET admin_token_hash = $2, token_last4 = $3,
                token_rotated_at = now(), updated_at = now()
          WHERE id = $1
          RETURNING {VIEW_COLS}"
    ))
    .bind(&id)
    .bind(&digest[..])
    .bind(sites::token_last4(&token))
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| internal("sites token rotation failed", &e))?;

    let site = site.ok_or(StatusCode::NOT_FOUND)?;
    refresh(&state).await;
    Ok(Json(SiteCreated { site, token }))
}

/// `DELETE /sites/:id` — operator only. Refuses while the tenant still owns
/// content: the FKs are `ON DELETE RESTRICT`, so offboarding is an explicit
/// purge rather than a silent cascade.
pub async fn delete_site(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let result = sqlx::query("DELETE FROM sites WHERE id = $1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(|e| {
            if is_pg_code(&e, "23503") {
                // Foreign key violation: content still references this tenant.
                StatusCode::CONFLICT
            } else {
                internal("sites delete failed", &e)
            }
        })?;

    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }
    refresh(&state).await;
    Ok(StatusCode::NO_CONTENT)
}
