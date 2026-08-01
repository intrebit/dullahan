//! Product catalog CRUD for a headless webshop (no cart, no orders).
//!
//! Mirrors `blog.rs`: snake_case JSON, public list/get, admin-gated writes
//! (`ADMIN_TOKEN` bearer), `draft` hides an item from the public list, and `id`
//! is a Postgres `uuid` carried as a `String` (`id::text`) so no `uuid` crate is
//! needed.
//!
//! Price is an integer count of minor units (`price_cents`); the shop's single
//! currency comes from config (`SHOP_CURRENCY`) and is echoed as `currency` in
//! every response so the frontend can format without hardcoding it.

//! Tenant scoping mirrors `blog.rs` exactly — see that module's header for the
//! two-gates rule and the bind-numbering convention.

use crate::auth::SiteScope;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Product {
    pub id: String,
    pub site_id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub image: Option<String>,
    pub price_cents: i64,
    pub available: bool,
    pub position: i32,
    pub draft: bool,
    pub views: i64,
    pub created_at: DateTime<Utc>,
    pub updated_date: Option<DateTime<Utc>>,
}

const COLS: &str = "id::text AS id, site_id, slug, title, description, image, price_cents, \
     available, position, draft, views, created_at, updated_date";

/// A product plus the shop-wide currency, so the JSON is self-describing.
#[derive(Debug, Serialize)]
pub struct ProductResponse {
    #[serde(flatten)]
    pub product: Product,
    pub currency: String,
}

fn with_currency(product: Product, currency: &str) -> ProductResponse {
    ProductResponse {
        product,
        currency: currency.to_string(),
    }
}

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub products: Vec<ProductResponse>,
    pub total: i64,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
    /// `published` (default) | `all`. `all` (includes drafts) requires admin.
    #[serde(default)]
    pub status: Option<String>,
}

fn default_limit() -> u32 {
    50
}

#[derive(Debug, Deserialize)]
pub struct CreateProduct {
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub price_cents: Option<i64>,
    #[serde(default)]
    pub available: Option<bool>,
    #[serde(default)]
    pub position: Option<i32>,
    #[serde(default)]
    pub draft: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProduct {
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// A provided value sets `image`; an omitted field leaves it unchanged.
    /// There is no way to clear `image` back to NULL via PATCH (matches blog).
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub price_cents: Option<i64>,
    #[serde(default)]
    pub available: Option<bool>,
    #[serde(default)]
    pub position: Option<i32>,
    #[serde(default)]
    pub draft: Option<bool>,
}

/// `^[a-z0-9-]+$`, with a sane upper bound.
fn slug_valid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn is_pg_code(err: &sqlx::Error, code: &str) -> bool {
    err.as_database_error().and_then(|e| e.code()).as_deref() == Some(code)
}

fn internal(context: &'static str, err: &sqlx::Error) -> StatusCode {
    tracing::error!(error = %err, "{context}");
    StatusCode::INTERNAL_SERVER_ERROR
}

/// GET /products — paginated list ordered by `position`, then newest. `status=all`
/// includes drafts but requires admin; otherwise the list is published-only.
/// Sold-out (`available = false`) items are still listed — that's a display flag.
pub async fn list(
    State(state): State<AppState>,
    ctx: SiteScope,
    Query(q): Query<ListQuery>,
) -> Result<Json<ListResponse>, StatusCode> {
    let limit = q.limit.clamp(1, 200) as i64;
    let offset = q.offset as i64;
    let include_drafts =
        ctx.scope.can_read_private(&ctx.site) && q.status.as_deref() == Some("all");

    let draft_clause = if include_drafts {
        ""
    } else {
        "AND draft = false"
    };

    let rows = sqlx::query_as::<_, Product>(&format!(
        "SELECT {COLS} FROM products WHERE site_id = $3 {draft_clause} \
         ORDER BY position ASC, created_at DESC LIMIT $1 OFFSET $2"
    ))
    .bind(limit)
    .bind(offset)
    .bind(&ctx.site)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| internal("products list query failed", &e))?;

    // Previously took no binds; the tenant filter adds one.
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM products WHERE site_id = $1 {draft_clause}"
    ))
    .bind(&ctx.site)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| internal("products count query failed", &e))?;

    let products = rows
        .into_iter()
        .map(|p| with_currency(p, &state.config.shop_currency))
        .collect();
    Ok(Json(ListResponse { products, total }))
}

/// GET /products/:slug — single product. 404 if absent, or if it is a draft and
/// the request is not admin-authed.
pub async fn get_product(
    State(state): State<AppState>,
    ctx: SiteScope,
    Path(slug): Path<String>,
) -> Result<Json<ProductResponse>, StatusCode> {
    let product = sqlx::query_as::<_, Product>(&format!(
        "SELECT {COLS} FROM products WHERE slug = $1 AND site_id = $2"
    ))
    .bind(&slug)
    .bind(&ctx.site)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| internal("products get query failed", &e))?;

    match product {
        Some(p) if p.draft && !ctx.scope.can_read_private(&ctx.site) => Err(StatusCode::NOT_FOUND),
        Some(p) => Ok(Json(with_currency(p, &state.config.shop_currency))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// POST /products/:slug/view — public, atomic increment so the owner can see
/// which products are being viewed. Always 204: a missing or draft slug is a
/// no-op. The frontend should ping this on a product-page view (debounce
/// client-side); it mirrors the blog view counter.
///
/// The `site_id` predicate is load-bearing despite this endpoint taking no auth:
/// slugs are only unique per site, so without it one anonymous ping would
/// increment every tenant's product of the same name.
pub async fn view(
    State(state): State<AppState>,
    ctx: SiteScope,
    Path(slug): Path<String>,
) -> Result<StatusCode, StatusCode> {
    sqlx::query(
        "UPDATE products SET views = views + 1 \
         WHERE slug = $1 AND site_id = $2 AND draft = false",
    )
    .bind(&slug)
    .bind(&ctx.site)
    .execute(&state.pool)
    .await
    .map_err(|e| internal("products view increment failed", &e))?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /products — create. Admin only. 409 on duplicate slug.
pub async fn create(
    State(state): State<AppState>,
    ctx: SiteScope,
    Json(body): Json<CreateProduct>,
) -> Result<(StatusCode, Json<ProductResponse>), StatusCode> {
    if !ctx.scope.can_write(&ctx.site) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let slug = body.slug.trim();
    if !slug_valid(slug) || body.title.trim().is_empty() || body.price_cents.is_some_and(|c| c < 0)
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    let product = sqlx::query_as::<_, Product>(&format!(
        "INSERT INTO products (slug, title, description, image, price_cents, available, position, draft, site_id)
         VALUES ($1, $2, COALESCE($3::text, ''), $4::text, COALESCE($5::bigint, 0),
                 COALESCE($6::boolean, true), COALESCE($7::integer, 0), COALESCE($8::boolean, false), $9)
         RETURNING {COLS}"
    ))
    .bind(slug)
    .bind(body.title.trim())
    .bind(body.description.as_deref())
    .bind(body.image.as_deref())
    .bind(body.price_cents)
    .bind(body.available)
    .bind(body.position)
    .bind(body.draft)
    .bind(&ctx.site)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| {
        if is_pg_code(&e, "23505") {
            // Duplicate (site_id, slug) — two tenants may share a slug.
            StatusCode::CONFLICT
        } else if is_pg_code(&e, "23503") {
            // Unknown site; see the matching comment in blog::create.
            StatusCode::BAD_REQUEST
        } else {
            internal("products create failed", &e)
        }
    })?;

    Ok((
        StatusCode::CREATED,
        Json(with_currency(product, &state.config.shop_currency)),
    ))
}

/// PATCH /products/:id — update by id. Admin only. Any subset of the create
/// fields; omitted fields are left unchanged. Sets `updated_date = now()`.
/// Same non-oracle property as `blog::update`: a correct uuid under another
/// tenant matches zero rows and returns 404, never 403.
pub async fn update(
    State(state): State<AppState>,
    ctx: SiteScope,
    Path(id): Path<String>,
    Json(body): Json<UpdateProduct>,
) -> Result<Json<ProductResponse>, StatusCode> {
    if !ctx.scope.can_write(&ctx.site) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    if let Some(slug) = body.slug.as_deref()
        && !slug_valid(slug.trim())
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.title.as_deref().is_some_and(|t| t.trim().is_empty())
        || body.price_cents.is_some_and(|c| c < 0)
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    // A malformed id ($1 not a valid uuid) raises Postgres 22P02; treat it as a
    // miss rather than a 500. COALESCE leaves omitted columns untouched.
    let product = sqlx::query_as::<_, Product>(&format!(
        "UPDATE products SET
             slug = COALESCE($2::text, slug),
             title = COALESCE($3::text, title),
             description = COALESCE($4::text, description),
             image = COALESCE($5::text, image),
             price_cents = COALESCE($6::bigint, price_cents),
             available = COALESCE($7::boolean, available),
             position = COALESCE($8::integer, position),
             draft = COALESCE($9::boolean, draft),
             updated_date = now()
         WHERE id = $1::uuid AND site_id = $10
         RETURNING {COLS}"
    ))
    .bind(&id)
    .bind(body.slug.as_deref().map(str::trim))
    .bind(body.title.as_deref().map(str::trim))
    .bind(body.description.as_deref())
    .bind(body.image.as_deref())
    .bind(body.price_cents)
    .bind(body.available)
    .bind(body.position)
    .bind(body.draft)
    .bind(&ctx.site)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
        if is_pg_code(&e, "23505") {
            StatusCode::CONFLICT
        } else if is_pg_code(&e, "22P02") {
            StatusCode::NOT_FOUND
        } else {
            internal("products update failed", &e)
        }
    })?;

    product
        .map(|p| Json(with_currency(p, &state.config.shop_currency)))
        .ok_or(StatusCode::NOT_FOUND)
}

/// DELETE /products/:id — within a site. 404 both when missing and when the id
/// belongs to another tenant.
pub async fn delete_product(
    State(state): State<AppState>,
    ctx: SiteScope,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    if !ctx.scope.can_write(&ctx.site) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let result = sqlx::query("DELETE FROM products WHERE id = $1::uuid AND site_id = $2")
        .bind(&id)
        .bind(&ctx.site)
        .execute(&state.pool)
        .await
        .map_err(|e| {
            if is_pg_code(&e, "22P02") {
                StatusCode::NOT_FOUND
            } else {
                internal("products delete failed", &e)
            }
        })?;

    if result.rows_affected() == 0 {
        Err(StatusCode::NOT_FOUND)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

#[cfg(test)]
mod tests {
    use super::slug_valid;

    #[test]
    fn slug_rules() {
        assert!(slug_valid("blue-widget-2026"));
        assert!(slug_valid("a"));
        assert!(!slug_valid(""));
        assert!(!slug_valid("Blue"));
        assert!(!slug_valid("has space"));
        assert!(!slug_valid("under_score"));
        assert!(!slug_valid("slash/path"));
        assert!(!slug_valid(&"x".repeat(201)));
    }
}
