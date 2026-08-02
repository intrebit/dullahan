//! Postgres access: the connection pool, migrations, and event insert/query helpers.

use crate::types::{RawPayload, Realtime, Summary, TimeseriesPoint, TopDimension, TopRow};

const PAGELEAVE_DUR_MAX_MS: i32 = 1_800_000;
use sqlx::Row;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use std::time::Duration;

/// Decode the `(key, count)` shape every breakdown query returns.
///
/// Decode failures propagate rather than defaulting: these queries are not
/// compile-time checked (the `query!` macros would need `DATABASE_URL` at build
/// time, which would break `cargo install dullahan`), so swallowing a bad column
/// would report `0` with a `200 OK` and make schema drift invisible. A SQL `NULL`
/// key is a different matter — it is real data, and renders as `(none)`.
fn top_rows(rows: Vec<PgRow>) -> sqlx::Result<Vec<TopRow>> {
    rows.into_iter()
        .map(|r| {
            Ok(TopRow {
                key: r
                    .try_get::<Option<String>, _>("key")?
                    .unwrap_or_else(|| "(none)".into()),
                count: r.try_get("count")?,
            })
        })
        .collect()
}

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

/// Columns written by [`insert_events`]. Postgres caps a statement at 65535 bind
/// parameters, so this is what bounds [`crate::ingest::BATCH_MAX`].
const EVENT_COLUMNS: usize = 15;

/// One validated event, flattened into the shape `analytics_events` stores and
/// owning its strings so it can cross the ingest channel — the request that
/// accepted the event and the task that writes it are no longer the same task.
///
/// Building this is where a [`RawPayload`] variant collapses into columns; the
/// insert below is then uniform over every event type.
pub struct PendingEvent {
    site_id: String,
    kind: &'static str,
    path: String,
    ts: i64,
    referrer: Option<String>,
    device: Option<String>,
    event_name: Option<String>,
    event_props: Option<serde_json::Value>,
    country: Option<String>,
    dur_ms: Option<i32>,
    utm_source: Option<String>,
    utm_medium: Option<String>,
    utm_campaign: Option<String>,
    view_id: Option<String>,
    visitor_hash: Option<String>,
}

impl PendingEvent {
    /// Consumes the payload: the handler has no further use for it, and moving
    /// the strings keeps the hot path allocation-free.
    pub fn new(payload: RawPayload, country: Option<String>, visitor_hash: Option<String>) -> Self {
        let view_id = match &payload {
            RawPayload::Pageview { vid, .. }
            | RawPayload::Event { vid, .. }
            | RawPayload::Pageleave { vid, .. } => vid.clone(),
        };

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
                } => (s, "pageview", p, ts, r, d, None, None, None, u),
                RawPayload::Event {
                    s,
                    p,
                    ts,
                    n,
                    pr,
                    vid: _,
                } => (
                    s,
                    "event",
                    p,
                    ts,
                    None,
                    None,
                    Some(n),
                    pr.map(|m| serde_json::to_value(m).unwrap()),
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
                    s,
                    "pageleave",
                    p,
                    ts,
                    None,
                    None,
                    None,
                    None,
                    Some(dur.clamp(0, PAGELEAVE_DUR_MAX_MS)),
                    None,
                ),
            };

        let (utm_source, utm_medium, utm_campaign) = match utm {
            Some(u) => (u.s, u.m, u.c),
            None => (None, None, None),
        };

        Self {
            site_id,
            kind,
            path,
            ts,
            referrer,
            device,
            event_name,
            event_props,
            country,
            dur_ms,
            utm_source,
            utm_medium,
            utm_campaign,
            view_id,
            visitor_hash,
        }
    }
}

/// Insert a batch of events as one multi-row statement.
///
/// Batching is what lets a 10-connection pool absorb a burst: one acquire and
/// one round trip per batch instead of per event. Callers must keep
/// `batch.len() * EVENT_COLUMNS` under Postgres' 65535-parameter ceiling —
/// [`crate::ingest::BATCH_MAX`] does.
pub async fn insert_events(pool: &PgPool, batch: &[PendingEvent]) -> sqlx::Result<()> {
    debug_assert!(batch.len() * EVENT_COLUMNS <= 65535);
    if batch.is_empty() {
        return Ok(());
    }

    let mut qb = sqlx::QueryBuilder::new(
        "INSERT INTO analytics_events
            (site_id, type, path, ts, referrer, device, event_name, event_props, country, dur_ms, utm_source, utm_medium, utm_campaign, view_id, visitor_hash) ",
    );
    qb.push_values(batch, |mut row, e| {
        row.push_bind(&e.site_id)
            .push_bind(e.kind)
            .push_bind(&e.path)
            .push_bind(e.ts)
            .push_bind(&e.referrer)
            .push_bind(&e.device)
            .push_bind(&e.event_name)
            .push_bind(&e.event_props)
            .push_bind(&e.country)
            .push_bind(e.dur_ms)
            .push_bind(&e.utm_source)
            .push_bind(&e.utm_medium)
            .push_bind(&e.utm_campaign)
            .push_bind(&e.view_id)
            .push_bind(&e.visitor_hash);
    });
    qb.build().execute(pool).await?;
    Ok(())
}

/// Rows removed per statement by [`prune_events`]. Bounded so the first sweep of
/// a table that has been accumulating for months cannot hold one long
/// transaction (and its locks, and its dead-tuple bloat) while it runs.
const PRUNE_CHUNK: i64 = 10_000;

/// Delete events older than `cutoff_ts` (epoch ms), in chunks, until none remain.
/// Returns the total rows removed.
///
/// Housekeeping for `RETENTION_DAYS`. Deleting by `ts` — the client clock, the
/// same field `/stats/*` filters on — means retention lines up with what the API
/// can still report, rather than with server receive time.
pub async fn prune_events(pool: &PgPool, cutoff_ts: i64) -> sqlx::Result<u64> {
    let mut total = 0u64;
    loop {
        // The subselect keeps each statement to PRUNE_CHUNK rows while still
        // driving off analytics_events_ts_idx. That index exists for this query
        // alone: every other index is (site_id, …) first, and this filters on `ts`
        // across all tenants.
        let removed = sqlx::query(
            "DELETE FROM analytics_events
              WHERE id IN (
                SELECT id FROM analytics_events WHERE ts < $1 ORDER BY ts LIMIT $2
              )",
        )
        .bind(cutoff_ts)
        .bind(PRUNE_CHUNK)
        .execute(pool)
        .await?
        .rows_affected();

        total += removed;
        if removed < PRUNE_CHUNK as u64 {
            return Ok(total);
        }
    }
}

pub async fn summary(
    pool: &PgPool,
    site_id: &str,
    from_ts: i64,
    to_ts: i64,
) -> sqlx::Result<Summary> {
    // This used to repeat `site_id = $1 AND ts BETWEEN $2 AND $3` in the outer
    // aggregate and all four subqueries — up to five scans of the same rows. Now
    // the range is scanned once into a CTE and the aggregates read that.
    //
    // Two deliberate choices, both measured on 800k rows across 40 tenants:
    //
    // * MATERIALIZED is load-bearing. Without it Postgres may inline the CTE back
    //   into each reference, which is exactly the plan we are replacing.
    // * `path` is *not* in the CTE, and `top_path` still reads the base table.
    //   Paths are the widest column (up to 2048 bytes), so carrying them makes the
    //   materialised set spill to temp files — 10753 temp blocks versus 3679 when
    //   it is left out. `top_path` has a dedicated index
    //   (`analytics_events_site_path_ts_idx`) that serves it better than a CTE
    //   scan does. Net effect on a 300k-row tenant: 2351ms → 2068ms; on a typical
    //   20k-row month, 156ms → 140ms.
    //
    // The three grains below (UTC day, visitor_hash, path) are genuinely
    // different, so they stay separate GROUP BYs — this removes repeated *scans*,
    // it does not pretend the aggregates can be merged.
    let row = sqlx::query(
        "WITH scoped AS MATERIALIZED (
            SELECT type, dur_ms, visitor_hash, ts
              FROM analytics_events
             WHERE site_id = $1 AND ts BETWEEN $2 AND $3
         )
         SELECT
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
                  FROM scoped
                 WHERE type = 'pageview' AND visitor_hash IS NOT NULL
                 GROUP BY ts / 86400000
              ) d
            ) AS avg_daily_visitors,
            (
              -- Base table, not `scoped`: see the note above on why `path` stays
              -- out of the CTE.
              SELECT path FROM analytics_events
               WHERE site_id = $1 AND ts BETWEEN $2 AND $3 AND type = 'pageview'
               GROUP BY path ORDER BY COUNT(*) DESC LIMIT 1
            ) AS top_path,
            (
              SELECT AVG((pv = 1)::int)::float8 FROM (
                SELECT COUNT(*) AS pv
                  FROM scoped
                 WHERE type = 'pageview' AND visitor_hash IS NOT NULL
                 -- visitor_hash already encodes the UTC day (daily salt), so it
                 -- alone is the visitor-day grain. A date_trunc here would be
                 -- evaluated in the DB session timezone and could split a
                 -- cross-midnight visit on a non-UTC server.
                 GROUP BY visitor_hash
              ) sessions
            ) AS bounce_rate
         FROM scoped",
    )
    .bind(site_id)
    .bind(from_ts)
    .bind(to_ts)
    .fetch_one(pool)
    .await?;

    Ok(Summary {
        pageviews: row.try_get("pageviews")?,
        events: row.try_get("events")?,
        top_path: row.try_get("top_path")?,
        avg_time_on_page_ms: row.try_get("avg_time_on_page_ms")?,
        avg_daily_visitors: row.try_get("avg_daily_visitors")?,
        bounce_rate: row.try_get("bounce_rate")?,
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

    rows.into_iter()
        .map(|r| {
            Ok(TimeseriesPoint {
                bucket: r.try_get("bucket")?,
                pageviews: r.try_get("pageviews")?,
                unique_visitors: r.try_get("uniques")?,
            })
        })
        .collect()
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

    top_rows(rows)
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

    top_rows(rows)
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
        let referrer: Option<String> = r.try_get("referrer")?;
        let utm_source: Option<String> = r.try_get("utm_source")?;
        let utm_medium: Option<String> = r.try_get("utm_medium")?;
        let utm_campaign: Option<String> = r.try_get("utm_campaign")?;
        let count: i64 = r.try_get("count")?;
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

    let pages = top_rows(rows)?;

    Ok(Realtime {
        active,
        window_minutes: minutes as i64,
        pages,
    })
}
