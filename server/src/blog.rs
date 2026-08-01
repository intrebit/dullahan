//! Blog posts CRUD + a public view counter.
//!
//! The JSON contract is snake_case (unlike `/stats/*`, which is camelCase) and
//! is depended on field-for-field by the intrebit SSR frontend, which talks to
//! these endpoints server-to-server reusing the `ADMIN_TOKEN` bearer auth.
//!
//! `body_markdown` is stored and returned raw — markdown is never rendered to
//! HTML here; the frontend sanitizes and renders it.
//!
//! `id` is a Postgres `uuid` but is carried in Rust as a `String` (selected via
//! `id::text`) so the crate needs no `uuid` dependency or sqlx `uuid` feature.

//! Every endpoint is tenant-scoped: the caller names a site via `?site=`, which
//! arrives as an [`crate::auth::SiteScope`] that has already authorized the
//! caller for that tenant. Handlers take that rather than reading `site` from
//! their own query struct, so there is no way to reach the DB layer with an
//! unauthorized site.
//!
//! **Two independent gates, both mandatory.** The `Scope` predicate stops an
//! unauthorized caller; the `site_id = $n` predicate in every statement stops an
//! *authorized* caller from reaching another tenant's row through an id they
//! guessed or scraped. Dropping either one is a cross-tenant leak.
//!
//! Bind-numbering convention: the `site_id` placeholder is **appended as the
//! highest-numbered parameter** in every statement, so no existing placeholder
//! is renumbered. That is why `WHERE site_id = $10` looks out of order.

use crate::auth::SiteScope;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// List item — every field except `body_markdown`.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PostMeta {
    pub id: String,
    pub site_id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub author: String,
    pub image: Option<String>,
    pub pub_date: DateTime<Utc>,
    pub updated_date: Option<DateTime<Utc>>,
    pub draft: bool,
    pub views: i64,
}

/// Single post — `PostMeta` plus the raw markdown body.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PostDetail {
    pub id: String,
    pub site_id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub author: String,
    pub image: Option<String>,
    pub pub_date: DateTime<Utc>,
    pub updated_date: Option<DateTime<Utc>>,
    pub draft: bool,
    pub views: i64,
    pub body_markdown: String,
}

const META_COLS: &str = "id::text AS id, site_id, slug, title, description, author, image, \
     pub_date, updated_date, draft, views";
const DETAIL_COLS: &str = "id::text AS id, site_id, slug, title, description, author, image, \
     pub_date, updated_date, draft, views, body_markdown";

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub posts: Vec<PostMeta>,
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
    20
}

#[derive(Debug, Deserialize)]
pub struct CreatePost {
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    pub body_markdown: String,
    #[serde(default)]
    pub draft: Option<bool>,
    #[serde(default)]
    pub pub_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePost {
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    /// A provided value sets `image`; an omitted field leaves it unchanged.
    /// (There is no way to clear `image` back to NULL via PATCH — not needed by
    /// the frontend, which always sends an image or none on create.)
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub body_markdown: Option<String>,
    #[serde(default)]
    pub draft: Option<bool>,
    #[serde(default)]
    pub pub_date: Option<DateTime<Utc>>,
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

/// GET /posts — paginated list. `status=all` includes drafts but requires admin;
/// without it (or unauthed) the list is forced to published-only.
pub async fn list(
    State(state): State<AppState>,
    ctx: SiteScope,
    Query(q): Query<ListQuery>,
) -> Result<Json<ListResponse>, StatusCode> {
    let limit = q.limit.clamp(1, 100) as i64;
    let offset = q.offset as i64;
    let include_drafts =
        ctx.scope.can_read_private(&ctx.site) && q.status.as_deref() == Some("all");

    // The tenant predicate is unconditional; only the draft filter varies.
    let draft_clause = if include_drafts {
        ""
    } else {
        "AND draft = false"
    };

    let posts = sqlx::query_as::<_, PostMeta>(&format!(
        "SELECT {META_COLS} FROM blog_posts WHERE site_id = $3 {draft_clause} \
         ORDER BY pub_date DESC LIMIT $1 OFFSET $2"
    ))
    .bind(limit)
    .bind(offset)
    .bind(&ctx.site)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| internal("blog list query failed", &e))?;

    // Note this query previously took no binds at all — the tenant filter adds
    // one, and forgetting it is a compile-clean runtime 500.
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM blog_posts WHERE site_id = $1 {draft_clause}"
    ))
    .bind(&ctx.site)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| internal("blog count query failed", &e))?;

    Ok(Json(ListResponse { posts, total }))
}

/// GET /posts/:slug — single post within a site. 404 if absent, or if it is a
/// draft and the caller may not read the site's private content.
pub async fn get_post(
    State(state): State<AppState>,
    ctx: SiteScope,
    Path(slug): Path<String>,
) -> Result<Json<PostDetail>, StatusCode> {
    let post = sqlx::query_as::<_, PostDetail>(&format!(
        "SELECT {DETAIL_COLS} FROM blog_posts WHERE slug = $1 AND site_id = $2"
    ))
    .bind(&slug)
    .bind(&ctx.site)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| internal("blog get query failed", &e))?;

    match post {
        Some(p) if p.draft && !ctx.scope.can_read_private(&ctx.site) => Err(StatusCode::NOT_FOUND),
        Some(p) => Ok(Json(p)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// POST /posts/:slug/view — public, atomic increment. Always 204: a missing or
/// draft slug is a no-op. The frontend debounces once per session, so no dedupe
/// happens here.
///
/// The `site_id` predicate is load-bearing even though this endpoint takes no
/// auth: slugs are only unique *per site*, so without it an anonymous ping to a
/// shared slug like `about` would increment every tenant's post at once.
pub async fn view(
    State(state): State<AppState>,
    ctx: SiteScope,
    Path(slug): Path<String>,
) -> Result<StatusCode, StatusCode> {
    sqlx::query(
        "UPDATE blog_posts SET views = views + 1 \
         WHERE slug = $1 AND site_id = $2 AND draft = false",
    )
    .bind(&slug)
    .bind(&ctx.site)
    .execute(&state.pool)
    .await
    .map_err(|e| internal("blog view increment failed", &e))?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /posts — create. Admin only. 409 on duplicate slug.
pub async fn create(
    State(state): State<AppState>,
    ctx: SiteScope,
    Json(body): Json<CreatePost>,
) -> Result<(StatusCode, Json<PostDetail>), StatusCode> {
    if !ctx.scope.can_write(&ctx.site) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let slug = body.slug.trim();
    if !slug_valid(slug) || body.title.trim().is_empty() || body.body_markdown.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let post = sqlx::query_as::<_, PostDetail>(&format!(
        "INSERT INTO blog_posts (slug, title, description, author, image, body_markdown, draft, pub_date, site_id)
         VALUES ($1, $2, COALESCE($3::text, ''), COALESCE($4::text, ''), $5::text, $6,
                 COALESCE($7::boolean, false), COALESCE($8::timestamptz, now()), $9)
         RETURNING {DETAIL_COLS}"
    ))
    .bind(slug)
    .bind(body.title.trim())
    .bind(body.description.as_deref())
    .bind(body.author.as_deref().map(str::trim))
    .bind(body.image.as_deref())
    .bind(body.body_markdown.as_str())
    .bind(body.draft)
    .bind(body.pub_date)
    .bind(&ctx.site)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| {
        if is_pg_code(&e, "23505") {
            // Now a duplicate (site_id, slug) — two tenants may share a slug.
            StatusCode::CONFLICT
        } else if is_pg_code(&e, "23503") {
            // Unknown site: the registry is permissive while empty, the foreign
            // key is not. A bad `?site=` is a client error, not a 500. 400 not
            // 403, so 403 stays unambiguously "not yours / suspended".
            StatusCode::BAD_REQUEST
        } else {
            internal("blog create failed", &e)
        }
    })?;

    Ok((StatusCode::CREATED, Json(post)))
}

/// PATCH /posts/:id — update by id, within a site. Any subset of the create
/// fields; omitted fields are left unchanged. Sets `updated_date = now()`.
///
/// The tenant predicate lives inside the same `UPDATE`, not a separate SELECT,
/// so there is no window in which ownership could change between check and
/// write. A correct uuid belonging to another tenant matches zero rows, mutates
/// nothing, and returns **404** rather than 403 — the response must not confirm
/// that the id exists under some other tenant.
///
/// `UpdatePost` deliberately has no `site` field: `?site=` is a *scope*, never a
/// *target*. Moving content between tenants is not an API operation.
pub async fn update(
    State(state): State<AppState>,
    ctx: SiteScope,
    Path(id): Path<String>,
    Json(body): Json<UpdatePost>,
) -> Result<Json<PostDetail>, StatusCode> {
    if !ctx.scope.can_write(&ctx.site) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    if let Some(slug) = body.slug.as_deref()
        && !slug_valid(slug.trim())
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.title.as_deref().is_some_and(|t| t.trim().is_empty())
        || body
            .body_markdown
            .as_deref()
            .is_some_and(|b| b.trim().is_empty())
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    // A malformed id ($1 not a valid uuid) raises Postgres 22P02; treat it as a
    // miss rather than a 500. COALESCE leaves omitted columns untouched.
    let post = sqlx::query_as::<_, PostDetail>(&format!(
        "UPDATE blog_posts SET
             slug = COALESCE($2::text, slug),
             title = COALESCE($3::text, title),
             description = COALESCE($4::text, description),
             author = COALESCE($5::text, author),
             image = COALESCE($6::text, image),
             body_markdown = COALESCE($7::text, body_markdown),
             draft = COALESCE($8::boolean, draft),
             pub_date = COALESCE($9::timestamptz, pub_date),
             updated_date = now()
         WHERE id = $1::uuid AND site_id = $10
         RETURNING {DETAIL_COLS}"
    ))
    .bind(&id)
    .bind(body.slug.as_deref().map(str::trim))
    .bind(body.title.as_deref().map(str::trim))
    .bind(body.description.as_deref())
    .bind(body.author.as_deref().map(str::trim))
    .bind(body.image.as_deref())
    .bind(body.body_markdown.as_deref())
    .bind(body.draft)
    .bind(body.pub_date)
    .bind(&ctx.site)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
        if is_pg_code(&e, "23505") {
            StatusCode::CONFLICT
        } else if is_pg_code(&e, "22P02") {
            StatusCode::NOT_FOUND
        } else {
            internal("blog update failed", &e)
        }
    })?;

    post.map(Json).ok_or(StatusCode::NOT_FOUND)
}

/// DELETE /posts/:id — within a site. 404 if missing, and equally 404 if the id
/// belongs to another tenant (same non-oracle property as PATCH).
pub async fn delete_post(
    State(state): State<AppState>,
    ctx: SiteScope,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    if !ctx.scope.can_write(&ctx.site) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let result = sqlx::query("DELETE FROM blog_posts WHERE id = $1::uuid AND site_id = $2")
        .bind(&id)
        .bind(&ctx.site)
        .execute(&state.pool)
        .await
        .map_err(|e| {
            // Malformed uuid -> 404 (no such post), consistent with PATCH.
            if is_pg_code(&e, "22P02") {
                StatusCode::NOT_FOUND
            } else {
                internal("blog delete failed", &e)
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
        assert!(slug_valid("hello-world-2026"));
        assert!(slug_valid("a"));
        assert!(!slug_valid(""));
        assert!(!slug_valid("Hello"));
        assert!(!slug_valid("has space"));
        assert!(!slug_valid("under_score"));
        assert!(!slug_valid("slash/path"));
        assert!(!slug_valid(&"x".repeat(201)));
    }
}
