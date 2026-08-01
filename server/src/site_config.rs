//! Per-site configuration store for the config-driven storefront + dashboard.
//!
//! Each site's whole config document is stored as an opaque JSON blob: dullahan
//! doesn't know the schema — the TypeScript `SiteConfig` type is the contract,
//! shared by the storefront that reads the config and the dashboard that writes
//! it. Public reads (so a storefront can fetch its own config cross-origin),
//! admin-gated writes (`ADMIN_TOKEN` bearer), keyed by a caller-chosen `site`
//! id (`^[a-z0-9-]+$`) — the same handle used for analytics and contact routing.

use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

fn internal(context: &'static str, err: &sqlx::Error) -> StatusCode {
    tracing::error!(error = %err, "{context}");
    StatusCode::INTERNAL_SERVER_ERROR
}

/// `^[a-z0-9-]+$`, with a sane upper bound. Matches the product/blog slug rule.
fn site_valid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// One row of the admin site list — id plus when it was last saved.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct SiteSummary {
    pub site: String,
    pub updated_at: DateTime<Utc>,
}

/// GET /site-config/:site — public. Returns the stored config document verbatim,
/// or 404 if the site has none yet. This is what a storefront fetches to render.
pub async fn get_config(
    State(state): State<AppState>,
    Path(site): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let config: Option<Value> =
        sqlx::query_scalar("SELECT config FROM site_config WHERE site = $1")
            .bind(&site)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| internal("site_config get failed", &e))?;
    config.map(Json).ok_or(StatusCode::NOT_FOUND)
}

/// GET /site-config — operator only. Enumerates every tenant that has a config,
/// so it is not something one tenant may see; a per-site caller gets 403. Never
/// returns the (potentially large) config bodies.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<SiteSummary>>, StatusCode> {
    match crate::auth::resolve_scope(&state, &headers) {
        s if s.is_operator() => {}
        crate::auth::Scope::Site(_) => return Err(StatusCode::FORBIDDEN),
        _ => return Err(StatusCode::UNAUTHORIZED),
    }
    let rows = sqlx::query_as::<_, SiteSummary>(
        "SELECT site, updated_at FROM site_config ORDER BY site ASC",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|e| internal("site_config list failed", &e))?;
    Ok(Json(rows))
}

/// PUT /site-config/:site — upserts the whole config document for one tenant.
/// The body must be a JSON object (a stray array/scalar is rejected); its inner
/// shape is the storefront's concern, not dullahan's.
///
/// The site travels in the path here rather than `?site=`, so this authorizes
/// against the path segment directly instead of going through `SiteScope`.
pub async fn put_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(site): Path<String>,
    Json(config): Json<Value>,
) -> Result<StatusCode, StatusCode> {
    let site = site.trim();
    if !site_valid(site) || !config.is_object() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !crate::auth::resolve_scope(&state, &headers).can_write(site) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    sqlx::query(
        "INSERT INTO site_config (site, config, updated_at) VALUES ($1, $2::jsonb, now())
         ON CONFLICT (site) DO UPDATE SET config = EXCLUDED.config, updated_at = now()",
    )
    .bind(site)
    .bind(&config)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        // Unknown tenant: site_config.site references sites(id).
        if e.as_database_error().and_then(|d| d.code()).as_deref() == Some("23503") {
            StatusCode::BAD_REQUEST
        } else {
            internal("site_config put failed", &e)
        }
    })?;
    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /site-config/:site — 404 if the site has no config.
pub async fn delete_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(site): Path<String>,
) -> Result<StatusCode, StatusCode> {
    if !crate::auth::resolve_scope(&state, &headers).can_write(site.trim()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let result = sqlx::query("DELETE FROM site_config WHERE site = $1")
        .bind(&site)
        .execute(&state.pool)
        .await
        .map_err(|e| internal("site_config delete failed", &e))?;
    if result.rows_affected() == 0 {
        Err(StatusCode::NOT_FOUND)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

#[cfg(test)]
mod tests {
    use super::site_valid;

    #[test]
    fn site_rules() {
        assert!(site_valid("acme-cafe"));
        assert!(site_valid("a"));
        assert!(!site_valid(""));
        assert!(!site_valid("Acme"));
        assert!(!site_valid("has space"));
        assert!(!site_valid("under_score"));
        assert!(!site_valid(&"x".repeat(101)));
    }
}
