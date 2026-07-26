//! Postgres access: the connection pool, migrations, and event insert/query helpers.

use crate::types::{RawPayload, Realtime, Summary, TimeseriesPoint, TopDimension, TopRow};

const PAGELEAVE_DUR_MAX_MS: i32 = 1_800_000;
use sqlx::Row;
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

pub async fn connect(database_url: &str) -> sqlx::Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await
}

pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}

pub async fn insert_event(
    pool: &PgPool,
    payload: &RawPayload,
    country: Option<&str>,
    visitor_hash: Option<&str>,
) -> sqlx::Result<()> {
    let (site_id, kind, path, ts, referrer, device, event_name, event_props, dur_ms, utm) =
        match payload {
            RawPayload::Pageview {
                s,
                p,
                ts,
                r,
                d,
                u,
                vid: _,
            } => (
                s.as_str(),
                "pageview",
                p.as_str(),
                *ts,
                r.as_deref(),
                d.as_deref(),
                None,
                None,
                None,
                u.as_ref(),
            ),
            RawPayload::Event {
                s,
                p,
                ts,
                n,
                pr,
                vid: _,
            } => (
                s.as_str(),
                "event",
                p.as_str(),
                *ts,
                None,
                None,
                Some(n.as_str()),
                pr.as_ref().map(|m| serde_json::to_value(m).unwrap()),
                None,
                None,
            ),
            RawPayload::Pageleave {
                s,
                p,
                ts,
                dur,
                vid: _,
            } => (
                s.as_str(),
                "pageleave",
                p.as_str(),
                *ts,
                None,
                None,
                None,
                None,
                Some((*dur).clamp(0, PAGELEAVE_DUR_MAX_MS)),
                None,
            ),
        };

    let (utm_source, utm_medium, utm_campaign) = match utm {
        Some(u) => (u.s.as_deref(), u.m.as_deref(), u.c.as_deref()),
        None => (None, None, None),
    };

    sqlx::query(
        "INSERT INTO analytics_events
            (site_id, type, path, ts, referrer, device, event_name, event_props, country, dur_ms, utm_source, utm_medium, utm_campaign, view_id, visitor_hash)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
    )
    .bind(site_id)
    .bind(kind)
    .bind(path)
    .bind(ts)
    .bind(referrer)
    .bind(device)
    .bind(event_name)
    .bind(event_props)
    .bind(country)
    .bind(dur_ms)
    .bind(utm_source)
    .bind(utm_medium)
    .bind(utm_campaign)
    .bind(payload.vid())
    .bind(visitor_hash)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn summary(
    pool: &PgPool,
    site_id: &str,
    from_ts: i64,
    to_ts: i64,
) -> sqlx::Result<Summary> {
    let row = sqlx::query(
        "SELECT
            COUNT(*) FILTER (WHERE type = 'pageview')::bigint AS pageviews,
            COUNT(*) FILTER (WHERE type = 'event')::bigint     AS events,
            (AVG(dur_ms) FILTER (WHERE type = 'pageleave'))::float8 AS avg_time_on_page_ms,
            (
              -- Unique visitors averaged per UTC day. The visitor hash is salted
              -- per day, so a distinct count over a multi-day range tallies
              -- visitor-days, not people; averaging the daily counts gives the
              -- honest \"typical day\" figure. NULL (⇒ omitted) when sessions off.
              -- `ts / 86400000` buckets epoch-ms into UTC days — integer math, so
              -- immune to the DB session timezone, and aligned with the salt day.
              SELECT AVG(daily)::float8 FROM (
                SELECT COUNT(DISTINCT visitor_hash) AS daily
                  FROM analytics_events
                 WHERE site_id = $1 AND ts BETWEEN $2 AND $3
                       AND type = 'pageview' AND visitor_hash IS NOT NULL
                 GROUP BY ts / 86400000
              ) d
            ) AS avg_daily_visitors,
            (
              SELECT path FROM analytics_events
               WHERE site_id = $1 AND ts BETWEEN $2 AND $3 AND type = 'pageview'
               GROUP BY path ORDER BY COUNT(*) DESC LIMIT 1
            ) AS top_path,
            (
              SELECT AVG((pv = 1)::int)::float8 FROM (
                SELECT COUNT(*) AS pv
                  FROM analytics_events
                 WHERE site_id = $1 AND ts BETWEEN $2 AND $3
                       AND type = 'pageview' AND visitor_hash IS NOT NULL
                 -- visitor_hash already encodes the UTC day (daily salt), so it
                 -- alone is the visitor-day grain. A date_trunc here would be
                 -- evaluated in the DB session timezone and could split a
                 -- cross-midnight visit on a non-UTC server.
                 GROUP BY visitor_hash
              ) sessions
            ) AS bounce_rate
         FROM analytics_events
         WHERE site_id = $1 AND ts BETWEEN $2 AND $3",
    )
    .bind(site_id)
    .bind(from_ts)
    .bind(to_ts)
    .fetch_one(pool)
    .await?;

    Ok(Summary {
        pageviews: row.try_get("pageviews").unwrap_or(0),
        events: row.try_get("events").unwrap_or(0),
        top_path: row.try_get::<Option<String>, _>("top_path").ok().flatten(),
        avg_time_on_page_ms: row
            .try_get::<Option<f64>, _>("avg_time_on_page_ms")
            .ok()
            .flatten(),
        avg_daily_visitors: row
            .try_get::<Option<f64>, _>("avg_daily_visitors")
            .ok()
            .flatten(),
        bounce_rate: row.try_get::<Option<f64>, _>("bounce_rate").ok().flatten(),
    })
}

pub async fn timeseries(
    pool: &PgPool,
    site_id: &str,
    from_ts: i64,
    to_ts: i64,
    bucket: &str,
) -> sqlx::Result<Vec<TimeseriesPoint>> {
    let bucket_ms = if bucket == "hour" {
        60 * 60 * 1000
    } else {
        24 * 60 * 60 * 1000
    };
    let rows = sqlx::query(&format!(
        "SELECT to_timestamp(((ts / {bucket_ms}) * {bucket_ms}) / 1000.0) AS bucket,
                COUNT(*)::bigint AS pageviews,
                NULLIF(COUNT(DISTINCT visitor_hash) FILTER (WHERE visitor_hash IS NOT NULL), 0)::bigint AS uniques
         FROM analytics_events
         WHERE site_id = $1 AND ts BETWEEN $2 AND $3 AND type = 'pageview'
         GROUP BY bucket ORDER BY bucket ASC"
    ))
    .bind(site_id)
    .bind(from_ts)
    .bind(to_ts)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| TimeseriesPoint {
            bucket: r.get("bucket"),
            pageviews: r.try_get("pageviews").unwrap_or(0),
            unique_visitors: r.try_get::<Option<i64>, _>("uniques").ok().flatten(),
        })
        .collect())
}

pub async fn top(
    pool: &PgPool,
    site_id: &str,
    from_ts: i64,
    to_ts: i64,
    dim: TopDimension,
    limit: i64,
) -> sqlx::Result<Vec<TopRow>> {
    let col = dim.column();
    let rows = sqlx::query(&format!(
        "SELECT {col} AS key, COUNT(*)::bigint AS count
         FROM analytics_events
         WHERE site_id = $1 AND ts BETWEEN $2 AND $3 AND type = 'pageview'
               AND {col} IS NOT NULL
         GROUP BY {col} ORDER BY count DESC LIMIT $4"
    ))
    .bind(site_id)
    .bind(from_ts)
    .bind(to_ts)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| TopRow {
            key: r
                .try_get::<Option<String>, _>("key")
                .ok()
                .flatten()
                .unwrap_or_else(|| "(none)".into()),
            count: r.try_get("count").unwrap_or(0),
        })
        .collect())
}

/// Custom-event analytics. With `by` set, returns the distribution of a single
/// event's prop value (e.g. `name=scroll_depth&by=pct`). Without `by`, returns
/// the top event names. `name` and `by` are bind params — never interpolated.
pub async fn events(
    pool: &PgPool,
    site_id: &str,
    from_ts: i64,
    to_ts: i64,
    name: Option<&str>,
    by: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<TopRow>> {
    let rows = if let Some(by) = by {
        sqlx::query(
            "SELECT event_props ->> $4 AS key, COUNT(*)::bigint AS count
             FROM analytics_events
             WHERE site_id = $1 AND ts BETWEEN $2 AND $3
                   AND type = 'event' AND event_name = $5
                   AND event_props ->> $4 IS NOT NULL
             GROUP BY key ORDER BY count DESC LIMIT $6",
        )
        .bind(site_id)
        .bind(from_ts)
        .bind(to_ts)
        .bind(by)
        .bind(name)
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            "SELECT event_name AS key, COUNT(*)::bigint AS count
             FROM analytics_events
             WHERE site_id = $1 AND ts BETWEEN $2 AND $3
                   AND type = 'event' AND event_name IS NOT NULL
             GROUP BY key ORDER BY count DESC LIMIT $4",
        )
        .bind(site_id)
        .bind(from_ts)
        .bind(to_ts)
        .bind(limit)
        .fetch_all(pool)
        .await?
    };

    Ok(rows
        .into_iter()
        .map(|r| TopRow {
            key: r
                .try_get::<Option<String>, _>("key")
                .ok()
                .flatten()
                .unwrap_or_else(|| "(none)".into()),
            count: r.try_get("count").unwrap_or(0),
        })
        .collect())
}

/// Pageviews grouped into marketing channels (see `crate::channels`).
pub async fn channels(
    pool: &PgPool,
    site_id: &str,
    from_ts: i64,
    to_ts: i64,
) -> sqlx::Result<Vec<TopRow>> {
    let rows = sqlx::query(
        "SELECT referrer, utm_source, utm_medium, utm_campaign, COUNT(*)::bigint AS count
         FROM analytics_events
         WHERE site_id = $1 AND ts BETWEEN $2 AND $3 AND type = 'pageview'
         GROUP BY referrer, utm_source, utm_medium, utm_campaign",
    )
    .bind(site_id)
    .bind(from_ts)
    .bind(to_ts)
    .fetch_all(pool)
    .await?;

    let mut totals: std::collections::HashMap<&'static str, i64> = std::collections::HashMap::new();
    for r in &rows {
        let referrer: Option<String> = r.try_get("referrer").ok().flatten();
        let utm_source: Option<String> = r.try_get("utm_source").ok().flatten();
        let utm_medium: Option<String> = r.try_get("utm_medium").ok().flatten();
        let utm_campaign: Option<String> = r.try_get("utm_campaign").ok().flatten();
        let count: i64 = r.try_get("count").unwrap_or(0);
        let channel = crate::channels::classify(
            referrer.as_deref(),
            utm_source.as_deref(),
            utm_medium.as_deref(),
            utm_campaign.as_deref(),
        );
        *totals.entry(channel).or_insert(0) += count;
    }

    let mut out: Vec<TopRow> = totals
        .into_iter()
        .map(|(channel, count)| TopRow {
            key: channel.to_string(),
            count,
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));
    Ok(out)
}

/// Real-time active page-visits in the trailing `minutes`. Filters on the server
/// `received_at` (DB clock, so no client skew) — backed by
/// `analytics_events_site_received_idx`. Distinct `view_id` across all event
/// types: a visitor reading quietly still counts via their load / leave events.
pub async fn realtime(pool: &PgPool, site_id: &str, minutes: i32) -> sqlx::Result<Realtime> {
    let active: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT view_id)::bigint
         FROM analytics_events
         WHERE site_id = $1
           AND received_at > now() - make_interval(mins => $2)
           AND view_id IS NOT NULL",
    )
    .bind(site_id)
    .bind(minutes)
    .fetch_one(pool)
    .await?;

    let rows = sqlx::query(
        "SELECT path AS key, count(DISTINCT view_id)::bigint AS count
         FROM analytics_events
         WHERE site_id = $1
           AND received_at > now() - make_interval(mins => $2)
           AND view_id IS NOT NULL AND path IS NOT NULL
         GROUP BY path ORDER BY count DESC, key ASC LIMIT 10",
    )
    .bind(site_id)
    .bind(minutes)
    .fetch_all(pool)
    .await?;

    let pages = rows
        .into_iter()
        .map(|r| TopRow {
            key: r
                .try_get::<Option<String>, _>("key")
                .ok()
                .flatten()
                .unwrap_or_else(|| "(none)".into()),
            count: r.try_get("count").unwrap_or(0),
        })
        .collect();

    Ok(Realtime {
        active,
        window_minutes: minutes as i64,
        pages,
    })
}
