use crate::state::AppState;
use crate::types::RawPayload;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};

pub async fn collect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut payload): Json<RawPayload>,
) -> StatusCode {
    if let Err(reason) = payload.validate() {
        tracing::debug!(reason, "rejected /collect payload");
        return StatusCode::BAD_REQUEST;
    }
    payload.clamp_ts(chrono::Utc::now().timestamp_millis());

    let site_id = payload.site_id();
    if let Some(allowed) = &state.config.allowed_sites
        && !allowed.iter().any(|s| s == site_id)
    {
        return StatusCode::FORBIDDEN;
    }

    let country = headers
        .get("x-country")
        .and_then(|v| v.to_str().ok())
        .filter(|c| c.len() == 2 && c.chars().all(|ch| ch.is_ascii_alphabetic()))
        .map(|c| c.to_ascii_uppercase());

    // Rung 2 enrichment (opt-in). With sessions disabled we read neither the
    // User-Agent nor the client IP, so an upgrade changes nothing by default.
    let (visitor_hash, browser, os) = if state.config.sessions_enabled {
        let ua = headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let (browser, os) = crate::ua::parse_ua(ua);
        let visitor_hash = match client_ip(&headers) {
            Some(ip) => {
                let today = chrono::Utc::now().date_naive();
                match crate::salt::current_salt(&state.pool, &state.salt_cache, today).await {
                    Ok(salt) => Some(crate::salt::visitor_hash(&salt, payload.site_id(), &ip, ua)),
                    Err(err) => {
                        tracing::warn!(error = %err, "salt lookup failed; skipping visitor hash");
                        None
                    }
                }
            }
            None => None,
        };
        (visitor_hash, browser, os)
    } else {
        (None, None, None)
    };

    let pool = state.pool.clone();
    tokio::spawn(async move {
        if let Err(err) = crate::db::insert_event(
            &pool,
            &payload,
            country.as_deref(),
            visitor_hash.as_deref(),
            browser.as_deref(),
            os.as_deref(),
        )
        .await
        {
            // Ingest is fire-and-forget (202 already returned), so a failed insert
            // is otherwise invisible. Count it so silent data loss is alertable.
            metrics::counter!("dullahan_ingest_insert_failures_total").increment(1);
            tracing::warn!(error = %err, "failed to insert event");
        }
    });

    StatusCode::ACCEPTED
}

/// Client IP from the proxy headers the rate limiter already trusts. The raw IP
/// is only used transiently to derive the salted hash — never stored.
fn client_ip(headers: &HeaderMap) -> Option<String> {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(first) = xff.split(',').next()
    {
        let ip = first.trim();
        if !ip.is_empty() {
            return Some(ip.to_string());
        }
    }
    headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
