//! The admin-gated `/stats/*` read API (summary, top, timeseries, vitals, …).

use crate::state::AppState;
use crate::types::{SummaryChange, SummaryResponse, TopDimension};
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct RangeQuery {
    pub site: String,
    #[serde(default = "default_days")]
    pub days: u32,
    /// `prev` adds a comparison against the immediately preceding equal window.
    #[serde(default)]
    pub compare: Option<String>,
}

fn default_days() -> u32 {
    30
}

#[derive(Debug, Deserialize)]
pub struct TimeseriesQuery {
    pub site: String,
    #[serde(default = "default_days")]
    pub days: u32,
    #[serde(default)]
    pub bucket: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TopQuery {
    pub site: String,
    #[serde(default = "default_days")]
    pub days: u32,
    pub dim: String,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    10
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    pub site: String,
    #[serde(default = "default_days")]
    pub days: u32,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub by: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

#[derive(Debug, Deserialize)]
pub struct RealtimeQuery {
    pub site: String,
    /// Trailing window in minutes (clamped 1–60). Defaults to 5.
    #[serde(default = "default_realtime_minutes")]
    pub minutes: u32,
}

fn default_realtime_minutes() -> u32 {
    5
}

fn range(days: u32) -> (i64, i64) {
    let days = days.clamp(1, 365) as i64;
    let to_ts = chrono::Utc::now().timestamp_millis();
    let from_ts = to_ts - days * 24 * 60 * 60 * 1000;
    (from_ts, to_ts)
}

fn site_check(state: &AppState, site: &str) -> Result<(), StatusCode> {
    if let Some(allowed) = &state.config.allowed_sites
        && !allowed.iter().any(|s| s == site)
    {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
}

pub async fn summary(
    State(state): State<AppState>,
    Query(q): Query<RangeQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    site_check(&state, &q.site)?;
    let (from_ts, to_ts) = range(q.days);
    let current = crate::db::summary(&state.pool, &q.site, from_ts, to_ts)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "summary query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let (previous, change) = if q.compare.as_deref() == Some("prev") {
        let span = to_ts - from_ts;
        // Upper bound is from_ts - 1: the current window's BETWEEN is inclusive of
        // from_ts, so sharing that boundary would double-count an event at exactly
        // from_ts in both windows.
        let prev = crate::db::summary(&state.pool, &q.site, from_ts - span, from_ts - 1)
            .await
            .map_err(|err| {
                tracing::error!(error = %err, "summary compare query failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        let change = SummaryChange {
            pageviews: pct_change(current.pageviews, prev.pageviews),
            events: pct_change(current.events, prev.events),
            unique_visitors: match (current.unique_visitors, prev.unique_visitors) {
                (Some(c), Some(p)) => pct_change(c, p),
                _ => None,
            },
        };
        (Some(prev), Some(change))
    } else {
        (None, None)
    };

    Ok(Json(SummaryResponse {
        current,
        previous,
        change,
    }))
}

/// Percentage change of `current` vs `previous`. `None` when `previous` is 0.
fn pct_change(current: i64, previous: i64) -> Option<f64> {
    if previous == 0 {
        return None;
    }
    Some((current - previous) as f64 / previous as f64 * 100.0)
}

pub async fn timeseries(
    State(state): State<AppState>,
    Query(q): Query<TimeseriesQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    site_check(&state, &q.site)?;
    let (from_ts, to_ts) = range(q.days);
    let bucket = q.bucket.as_deref().unwrap_or("day");
    let bucket = if bucket == "hour" { "hour" } else { "day" };
    let rows = crate::db::timeseries(&state.pool, &q.site, from_ts, to_ts, bucket)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "timeseries query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(rows))
}

pub async fn top(
    State(state): State<AppState>,
    Query(q): Query<TopQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    site_check(&state, &q.site)?;
    let dim = TopDimension::parse(&q.dim).ok_or(StatusCode::BAD_REQUEST)?;
    let limit = q.limit.clamp(1, 100) as i64;
    let (from_ts, to_ts) = range(q.days);
    let rows = crate::db::top(&state.pool, &q.site, from_ts, to_ts, dim, limit)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "top query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(rows))
}

pub async fn events(
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    site_check(&state, &q.site)?;
    // A prop breakdown needs an event to break down; reject `by` without `name`.
    if q.by.is_some() && q.name.is_none() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let limit = q.limit.clamp(1, 100) as i64;
    let (from_ts, to_ts) = range(q.days);
    let rows = crate::db::events(
        &state.pool,
        &q.site,
        from_ts,
        to_ts,
        q.name.as_deref(),
        q.by.as_deref(),
        limit,
    )
    .await
    .map_err(|err| {
        tracing::error!(error = %err, "events query failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(rows))
}

pub async fn channels(
    State(state): State<AppState>,
    Query(q): Query<RangeQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    site_check(&state, &q.site)?;
    let (from_ts, to_ts) = range(q.days);
    let rows = crate::db::channels(&state.pool, &q.site, from_ts, to_ts)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "channels query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(rows))
}

pub async fn realtime(
    State(state): State<AppState>,
    Query(q): Query<RealtimeQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    site_check(&state, &q.site)?;
    let minutes = q.minutes.clamp(1, 60) as i32;
    let rt = crate::db::realtime(&state.pool, &q.site, minutes)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "realtime query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(rt))
}
