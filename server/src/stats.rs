use crate::state::AppState;
use crate::types::{Funnel, FunnelStep, MAX_PATH, SummaryChange, SummaryResponse, TopDimension};
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
pub struct VitalsQuery {
    pub site: String,
    #[serde(default = "default_days")]
    pub days: u32,
    /// `path` switches the response from the site-wide object to a per-path array.
    #[serde(default)]
    pub dim: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

#[derive(Debug, Deserialize)]
pub struct HeatmapQuery {
    pub site: String,
    #[serde(default = "default_days")]
    pub days: u32,
    /// IANA timezone for hour-of-day bucketing. Defaults to UTC.
    #[serde(default)]
    pub tz: Option<String>,
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

#[derive(Debug, Deserialize)]
pub struct EngagementQuery {
    pub site: String,
    #[serde(default = "default_days")]
    pub days: u32,
    /// `path` switches the response from the site-wide object to a per-path array.
    #[serde(default)]
    pub dim: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

#[derive(Debug, Deserialize)]
pub struct SessionsQuery {
    pub site: String,
    #[serde(default = "default_days")]
    pub days: u32,
    /// Inactivity gap in minutes that splits sessions (clamped 1–240). Default 30.
    #[serde(default = "default_gap_minutes")]
    pub gap: u32,
    /// `entry` / `exit` switch the response from the summary object to a per-page array.
    #[serde(default)]
    pub dim: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_gap_minutes() -> u32 {
    30
}

#[derive(Debug, Deserialize)]
pub struct FunnelQuery {
    pub site: String,
    #[serde(default = "default_days")]
    pub days: u32,
    /// Session inactivity gap in minutes (clamped 1–240). Default 30.
    #[serde(default = "default_gap_minutes")]
    pub gap: u32,
    /// Comma-separated ordered pageview paths (2–10 steps).
    pub steps: String,
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

pub async fn vitals(
    State(state): State<AppState>,
    Query(q): Query<VitalsQuery>,
) -> Result<axum::response::Response, StatusCode> {
    site_check(&state, &q.site)?;
    let (from_ts, to_ts) = range(q.days);
    match q.dim.as_deref() {
        None => {
            let v = crate::db::vitals(&state.pool, &q.site, from_ts, to_ts)
                .await
                .map_err(|err| {
                    tracing::error!(error = %err, "vitals query failed");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            Ok(Json(v).into_response())
        }
        Some("path") => {
            let limit = q.limit.clamp(1, 100) as i64;
            let rows = crate::db::vitals_by_path(&state.pool, &q.site, from_ts, to_ts, limit)
                .await
                .map_err(|err| {
                    tracing::error!(error = %err, "vitals_by_path query failed");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            Ok(Json(rows).into_response())
        }
        Some(_) => Err(StatusCode::BAD_REQUEST),
    }
}

pub async fn heatmap(
    State(state): State<AppState>,
    Query(q): Query<HeatmapQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    site_check(&state, &q.site)?;
    let tz = q.tz.as_deref().unwrap_or("UTC");
    // Defense in depth (it is a bind param anyway): reject obviously-bad tz.
    if tz.len() > 64
        || !tz
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'_' | b'/'))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let (from_ts, to_ts) = range(q.days);
    match crate::db::heatmap(&state.pool, &q.site, from_ts, to_ts, tz).await {
        Ok(rows) => Ok(Json(rows)),
        Err(err) => {
            // Unknown timezone -> Postgres invalid_parameter_value (22023) -> 400.
            if err.as_database_error().and_then(|e| e.code()).as_deref() == Some("22023") {
                Err(StatusCode::BAD_REQUEST)
            } else {
                tracing::error!(error = %err, "heatmap query failed");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        }
    }
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

pub async fn engagement(
    State(state): State<AppState>,
    Query(q): Query<EngagementQuery>,
) -> Result<axum::response::Response, StatusCode> {
    site_check(&state, &q.site)?;
    let (from_ts, to_ts) = range(q.days);
    match q.dim.as_deref() {
        None => {
            let e = crate::db::engagement(&state.pool, &q.site, from_ts, to_ts)
                .await
                .map_err(|err| {
                    tracing::error!(error = %err, "engagement query failed");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            Ok(Json(e).into_response())
        }
        Some("path") => {
            let limit = q.limit.clamp(1, 100) as i64;
            let rows = crate::db::engagement_by_path(&state.pool, &q.site, from_ts, to_ts, limit)
                .await
                .map_err(|err| {
                    tracing::error!(error = %err, "engagement_by_path query failed");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            Ok(Json(rows).into_response())
        }
        Some(_) => Err(StatusCode::BAD_REQUEST),
    }
}

pub async fn sessions(
    State(state): State<AppState>,
    Query(q): Query<SessionsQuery>,
) -> Result<axum::response::Response, StatusCode> {
    site_check(&state, &q.site)?;
    let (from_ts, to_ts) = range(q.days);
    let gap_ms = (q.gap.clamp(1, 240) as i64) * 60_000;
    match q.dim.as_deref() {
        None => {
            let s = crate::db::sessions(&state.pool, &q.site, from_ts, to_ts, gap_ms)
                .await
                .map_err(|err| {
                    tracing::error!(error = %err, "sessions query failed");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            Ok(Json(s).into_response())
        }
        Some(dim @ ("entry" | "exit")) => {
            let col = if dim == "entry" {
                "entry_path"
            } else {
                "exit_path"
            };
            let limit = q.limit.clamp(1, 100) as i64;
            let rows =
                crate::db::session_pages(&state.pool, &q.site, from_ts, to_ts, gap_ms, col, limit)
                    .await
                    .map_err(|err| {
                        tracing::error!(error = %err, "session_pages query failed");
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?;
            Ok(Json(rows).into_response())
        }
        Some(_) => Err(StatusCode::BAD_REQUEST),
    }
}

pub async fn funnel(
    State(state): State<AppState>,
    Query(q): Query<FunnelQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    site_check(&state, &q.site)?;
    let steps: Vec<String> = q
        .steps
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !(2..=10).contains(&steps.len()) || steps.iter().any(|s| s.len() > MAX_PATH) {
        return Err(StatusCode::BAD_REQUEST);
    }
    // Duplicate steps break the funnel: array_position() maps every occurrence of
    // a path to its first index, so a repeated step can never be reached and its
    // conversion is a false 0. Reject rather than report wrong numbers.
    {
        let mut seen = std::collections::HashSet::with_capacity(steps.len());
        if !steps.iter().all(|s| seen.insert(s.as_str())) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let gap_ms = (q.gap.clamp(1, 240) as i64) * 60_000;
    let (from_ts, to_ts) = range(q.days);
    let counts = crate::db::funnel(&state.pool, &q.site, from_ts, to_ts, gap_ms, &steps)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "funnel query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // step 1 is the funnel base (100%); later steps convert relative to it.
    let base = counts.first().copied().unwrap_or(0);
    let steps_out = steps
        .into_iter()
        .enumerate()
        .map(|(i, key)| {
            let sessions = counts[i];
            let conversion_from_prev = if i == 0 {
                Some(1.0)
            } else {
                let prev = counts[i - 1];
                (prev > 0).then(|| sessions as f64 / prev as f64)
            };
            FunnelStep {
                step: (i + 1) as i32,
                key,
                sessions,
                conversion_from_prev,
                conversion_from_start: (base > 0).then(|| sessions as f64 / base as f64),
            }
        })
        .collect();
    Ok(Json(Funnel { steps: steps_out }))
}
