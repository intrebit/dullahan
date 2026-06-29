use crate::types::{
    Engagement, EngagementRow, HeatmapCell, MetricBucket, RawPayload, Realtime, ScrollFunnel,
    Sessions, Summary, TimeseriesPoint, TopDimension, TopRow, Vitals, VitalsDistribution,
    VitalsRow,
};

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
    browser: Option<&str>,
    os: Option<&str>,
) -> sqlx::Result<()> {
    let (
        site_id,
        kind,
        path,
        ts,
        referrer,
        device,
        viewport,
        event_name,
        event_props,
        metrics,
        dur_ms,
        utm,
    ) = match payload {
        RawPayload::Pageview {
            s,
            p,
            ts,
            r,
            d,
            v,
            u,
            vid: _,
        } => (
            s.as_str(),
            "pageview",
            p.as_str(),
            *ts,
            r.as_deref(),
            d.as_deref(),
            *v,
            None,
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
            None,
            Some(n.as_str()),
            pr.as_ref().map(|m| serde_json::to_value(m).unwrap()),
            None,
            None,
            None,
        ),
        RawPayload::Performance {
            s,
            p,
            ts,
            pf,
            vid: _,
        } => (
            s.as_str(),
            "performance",
            p.as_str(),
            *ts,
            None,
            None,
            None,
            None,
            None,
            Some(serde_json::to_value(pf).unwrap()),
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
            (site_id, type, path, ts, referrer, device, viewport, event_name, event_props, metrics, country, dur_ms, utm_source, utm_medium, utm_campaign, view_id, visitor_hash, browser, os)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)",
    )
    .bind(site_id)
    .bind(kind)
    .bind(path)
    .bind(ts)
    .bind(referrer)
    .bind(device)
    .bind(viewport)
    .bind(event_name)
    .bind(event_props)
    .bind(metrics)
    .bind(country)
    .bind(dur_ms)
    .bind(utm_source)
    .bind(utm_medium)
    .bind(utm_campaign)
    .bind(payload.vid())
    .bind(visitor_hash)
    .bind(browser)
    .bind(os)
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
            (percentile_cont(0.5)  WITHIN GROUP (ORDER BY dur_ms) FILTER (WHERE type = 'pageleave'))::float8 AS median_time_on_page_ms,
            (percentile_cont(0.75) WITHIN GROUP (ORDER BY dur_ms) FILTER (WHERE type = 'pageleave'))::float8 AS p75_time_on_page_ms,
            NULLIF(
              COUNT(DISTINCT visitor_hash) FILTER (WHERE type = 'pageview' AND visitor_hash IS NOT NULL),
              0
            )::bigint AS unique_visitors,
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
        median_time_on_page_ms: row
            .try_get::<Option<f64>, _>("median_time_on_page_ms")
            .ok()
            .flatten(),
        p75_time_on_page_ms: row
            .try_get::<Option<f64>, _>("p75_time_on_page_ms")
            .ok()
            .flatten(),
        unique_visitors: row
            .try_get::<Option<i64>, _>("unique_visitors")
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
    let trunc = if bucket == "hour" { "hour" } else { "day" };
    let rows = sqlx::query(&format!(
        "SELECT date_trunc('{trunc}', to_timestamp(ts / 1000.0)) AS bucket,
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
    if matches!(dim, TopDimension::Path) {
        let rows = sqlx::query(
            "SELECT path AS key,
                    COUNT(*) FILTER (WHERE type = 'pageview')::bigint AS count,
                    (AVG(dur_ms) FILTER (WHERE type = 'pageleave'))::float8 AS avg_dur_ms,
                    (percentile_cont(0.5) WITHIN GROUP (ORDER BY dur_ms) FILTER (WHERE type = 'pageleave'))::float8 AS median_dur_ms
             FROM analytics_events
             WHERE site_id = $1 AND ts BETWEEN $2 AND $3
                   AND type IN ('pageview', 'pageleave')
                   AND path IS NOT NULL
             GROUP BY path
             HAVING COUNT(*) FILTER (WHERE type = 'pageview') > 0
             ORDER BY count DESC
             LIMIT $4",
        )
        .bind(site_id)
        .bind(from_ts)
        .bind(to_ts)
        .bind(limit)
        .fetch_all(pool)
        .await?;

        return Ok(rows
            .into_iter()
            .map(|r| TopRow {
                key: r
                    .try_get::<Option<String>, _>("key")
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "(none)".into()),
                count: r.try_get("count").unwrap_or(0),
                avg_dur_ms: r.try_get::<Option<f64>, _>("avg_dur_ms").ok().flatten(),
                median_dur_ms: r.try_get::<Option<f64>, _>("median_dur_ms").ok().flatten(),
            })
            .collect());
    }

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
            avg_dur_ms: None,
            median_dur_ms: None,
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
            avg_dur_ms: None,
            median_dur_ms: None,
        })
        .collect())
}

pub async fn vitals(
    pool: &PgPool,
    site_id: &str,
    from_ts: i64,
    to_ts: i64,
) -> sqlx::Result<Vitals> {
    let row = sqlx::query(
        "SELECT
            (percentile_cont(0.75) WITHIN GROUP (ORDER BY (metrics->>'lcp')::numeric))::float8  AS lcp,
            (percentile_cont(0.75) WITHIN GROUP (ORDER BY (metrics->>'fcp')::numeric))::float8  AS fcp,
            (percentile_cont(0.75) WITHIN GROUP (ORDER BY (metrics->>'cls')::numeric))::float8  AS cls,
            (percentile_cont(0.75) WITHIN GROUP (ORDER BY (metrics->>'inp')::numeric))::float8  AS inp,
            (percentile_cont(0.75) WITHIN GROUP (ORDER BY (metrics->>'ttfb')::numeric))::float8 AS ttfb,
            COUNT(*) FILTER (WHERE (metrics->>'lcp')  IS NOT NULL)::bigint AS lcp_total,
            COUNT(*) FILTER (WHERE (metrics->>'lcp')::numeric  <= 2500)::bigint AS lcp_good,
            COUNT(*) FILTER (WHERE (metrics->>'lcp')::numeric  >  4000)::bigint AS lcp_poor,
            COUNT(*) FILTER (WHERE (metrics->>'fcp')  IS NOT NULL)::bigint AS fcp_total,
            COUNT(*) FILTER (WHERE (metrics->>'fcp')::numeric  <= 1800)::bigint AS fcp_good,
            COUNT(*) FILTER (WHERE (metrics->>'fcp')::numeric  >  3000)::bigint AS fcp_poor,
            COUNT(*) FILTER (WHERE (metrics->>'cls')  IS NOT NULL)::bigint AS cls_total,
            COUNT(*) FILTER (WHERE (metrics->>'cls')::numeric  <= 0.10)::bigint AS cls_good,
            COUNT(*) FILTER (WHERE (metrics->>'cls')::numeric  >  0.25)::bigint AS cls_poor,
            COUNT(*) FILTER (WHERE (metrics->>'inp')  IS NOT NULL)::bigint AS inp_total,
            COUNT(*) FILTER (WHERE (metrics->>'inp')::numeric  <= 200)::bigint AS inp_good,
            COUNT(*) FILTER (WHERE (metrics->>'inp')::numeric  >  500)::bigint AS inp_poor,
            COUNT(*) FILTER (WHERE (metrics->>'ttfb') IS NOT NULL)::bigint AS ttfb_total,
            COUNT(*) FILTER (WHERE (metrics->>'ttfb')::numeric <= 800)::bigint AS ttfb_good,
            COUNT(*) FILTER (WHERE (metrics->>'ttfb')::numeric >  1800)::bigint AS ttfb_poor
         FROM analytics_events
         WHERE site_id = $1 AND ts BETWEEN $2 AND $3 AND type = 'performance'",
    )
    .bind(site_id)
    .bind(from_ts)
    .bind(to_ts)
    .fetch_one(pool)
    .await?;

    let pick = |k: &str| -> Option<f64> { row.try_get::<Option<f64>, _>(k).ok().flatten() };
    let count = |k: &str| -> i64 { row.try_get::<i64, _>(k).unwrap_or(0) };
    let bucket = |m: &str| -> MetricBucket {
        let total = count(&format!("{m}_total"));
        let good = count(&format!("{m}_good"));
        let poor = count(&format!("{m}_poor"));
        MetricBucket {
            good,
            poor,
            total,
            needs_improvement: (total - good - poor).max(0),
        }
    };
    let has_data = ["lcp", "fcp", "cls", "inp", "ttfb"]
        .iter()
        .any(|m| count(&format!("{m}_total")) > 0);

    Ok(Vitals {
        lcp_p75: pick("lcp"),
        fcp_p75: pick("fcp"),
        cls_p75: pick("cls"),
        inp_p75: pick("inp"),
        ttfb_p75: pick("ttfb"),
        distribution: has_data.then(|| VitalsDistribution {
            lcp: bucket("lcp"),
            fcp: bucket("fcp"),
            cls: bucket("cls"),
            inp: bucket("inp"),
            ttfb: bucket("ttfb"),
        }),
    })
}

/// Per-path p75 vitals with per-metric sample counts. Ordered by total perf
/// rows so the busiest pages surface first.
pub async fn vitals_by_path(
    pool: &PgPool,
    site_id: &str,
    from_ts: i64,
    to_ts: i64,
    limit: i64,
) -> sqlx::Result<Vec<VitalsRow>> {
    let rows = sqlx::query(
        "SELECT path AS key,
                COUNT(*)::bigint AS samples,
                (percentile_cont(0.75) WITHIN GROUP (ORDER BY (metrics->>'lcp')::numeric))::float8  AS lcp,
                (percentile_cont(0.75) WITHIN GROUP (ORDER BY (metrics->>'fcp')::numeric))::float8  AS fcp,
                (percentile_cont(0.75) WITHIN GROUP (ORDER BY (metrics->>'cls')::numeric))::float8  AS cls,
                (percentile_cont(0.75) WITHIN GROUP (ORDER BY (metrics->>'inp')::numeric))::float8  AS inp,
                (percentile_cont(0.75) WITHIN GROUP (ORDER BY (metrics->>'ttfb')::numeric))::float8 AS ttfb,
                COUNT(*) FILTER (WHERE (metrics->>'lcp')  IS NOT NULL)::bigint AS lcp_n,
                COUNT(*) FILTER (WHERE (metrics->>'fcp')  IS NOT NULL)::bigint AS fcp_n,
                COUNT(*) FILTER (WHERE (metrics->>'cls')  IS NOT NULL)::bigint AS cls_n,
                COUNT(*) FILTER (WHERE (metrics->>'inp')  IS NOT NULL)::bigint AS inp_n,
                COUNT(*) FILTER (WHERE (metrics->>'ttfb') IS NOT NULL)::bigint AS ttfb_n
         FROM analytics_events
         WHERE site_id = $1 AND ts BETWEEN $2 AND $3 AND type = 'performance' AND path IS NOT NULL
         GROUP BY path ORDER BY samples DESC LIMIT $4",
    )
    .bind(site_id)
    .bind(from_ts)
    .bind(to_ts)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let p = |k: &str| r.try_get::<Option<f64>, _>(k).ok().flatten();
            let n = |k: &str| r.try_get::<i64, _>(k).unwrap_or(0);
            VitalsRow {
                key: r
                    .try_get::<Option<String>, _>("key")
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "(none)".into()),
                samples: r.try_get("samples").unwrap_or(0),
                lcp_p75: p("lcp"),
                lcp_n: n("lcp_n"),
                fcp_p75: p("fcp"),
                fcp_n: n("fcp_n"),
                cls_p75: p("cls"),
                cls_n: n("cls_n"),
                inp_p75: p("inp"),
                inp_n: n("inp_n"),
                ttfb_p75: p("ttfb"),
                ttfb_n: n("ttfb_n"),
            }
        })
        .collect())
}

/// Pageviews bucketed by weekday (ISO 1-7) and hour (0-23) in `tz`. An invalid
/// `tz` surfaces as a Postgres `invalid_parameter_value` error (mapped to 400 by
/// the handler). `tz` is a bind param — never interpolated.
pub async fn heatmap(
    pool: &PgPool,
    site_id: &str,
    from_ts: i64,
    to_ts: i64,
    tz: &str,
) -> sqlx::Result<Vec<HeatmapCell>> {
    let rows = sqlx::query(
        "SELECT EXTRACT(isodow FROM to_timestamp(ts / 1000.0) AT TIME ZONE $4)::int AS weekday,
                EXTRACT(hour   FROM to_timestamp(ts / 1000.0) AT TIME ZONE $4)::int AS hour,
                COUNT(*)::bigint AS pageviews
         FROM analytics_events
         WHERE site_id = $1 AND ts BETWEEN $2 AND $3 AND type = 'pageview'
         GROUP BY weekday, hour ORDER BY weekday, hour",
    )
    .bind(site_id)
    .bind(from_ts)
    .bind(to_ts)
    .bind(tz)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| HeatmapCell {
            weekday: r.try_get("weekday").unwrap_or(0),
            hour: r.try_get("hour").unwrap_or(0),
            pageviews: r.try_get("pageviews").unwrap_or(0),
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
            avg_dur_ms: None,
            median_dur_ms: None,
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
            avg_dur_ms: None,
            median_dur_ms: None,
        })
        .collect();

    Ok(Realtime {
        active,
        window_minutes: minutes as i64,
        pages,
    })
}

/// The per-view aggregate that backs both engagement queries. One row per
/// `view_id`: `is_visit` anchors on a real pageview, `max_scroll` is the deepest
/// milestone reached (the `~` guard means a hostile non-numeric `pct` is ignored
/// rather than crashing the `::int` cast), `has_click` covers outbound+download,
/// `custom_events` excludes the auto-instrumentation names.
const VIEWS_CTE: &str = "WITH views AS (
    SELECT
        view_id,
        bool_or(type = 'pageview')                            AS is_visit,
        max(path) FILTER (WHERE type = 'pageview')            AS path,
        max(dur_ms) FILTER (WHERE type = 'pageleave')         AS dur_ms,
        max((event_props->>'pct')::int) FILTER (
            WHERE type = 'event' AND event_name = 'scroll_depth'
              AND event_props->>'pct' ~ '^[0-9]{1,3}$'
        )                                                     AS max_scroll,
        count(*) FILTER (WHERE type = 'event' AND event_name = 'outbound')        AS outbound_n,
        bool_or(type = 'event' AND event_name IN ('outbound','download'))         AS has_click,
        count(*) FILTER (
            WHERE type = 'event' AND event_name NOT IN ('scroll_depth','outbound','download')
        )                                                     AS custom_events
    FROM analytics_events
    WHERE site_id = $1 AND ts BETWEEN $2 AND $3 AND view_id IS NOT NULL
    GROUP BY view_id
)";

/// Site-wide per-page-visit engagement. Rates omitted (None) per the opt-in
/// rules on `Engagement`.
pub async fn engagement(
    pool: &PgPool,
    site_id: &str,
    from_ts: i64,
    to_ts: i64,
) -> sqlx::Result<Engagement> {
    let sql = format!(
        "{VIEWS_CTE}
         SELECT
            count(*) FILTER (WHERE is_visit)::bigint AS visits,
            count(*) FILTER (WHERE is_visit AND (
                coalesce(dur_ms,0) >= 10000 OR coalesce(max_scroll,0) >= 50 OR has_click
            ))::bigint AS engaged,
            count(*) FILTER (WHERE is_visit AND max_scroll >= 25)::bigint  AS scrolled_25,
            count(*) FILTER (WHERE is_visit AND max_scroll >= 50)::bigint  AS scrolled_50,
            count(*) FILTER (WHERE is_visit AND max_scroll >= 75)::bigint  AS scrolled_75,
            count(*) FILTER (WHERE is_visit AND max_scroll >= 100)::bigint AS scrolled_100,
            count(*) FILTER (WHERE is_visit AND max_scroll IS NOT NULL)::bigint AS scroll_visits,
            count(*) FILTER (WHERE is_visit AND outbound_n > 0)::bigint    AS outbound_visits,
            (avg(custom_events) FILTER (WHERE is_visit))::float8 AS avg_events
         FROM views"
    );
    let row = sqlx::query(&sql)
        .bind(site_id)
        .bind(from_ts)
        .bind(to_ts)
        .fetch_one(pool)
        .await?;

    let g = |k: &str| -> i64 { row.try_get::<i64, _>(k).unwrap_or(0) };
    let visits = g("visits");
    let scroll_visits = g("scroll_visits");
    let outbound_visits = g("outbound_visits");
    let frac = |n: i64| -> Option<f64> { (visits > 0).then(|| n as f64 / visits as f64) };

    Ok(Engagement {
        visits,
        engaged_visit_rate: frac(g("engaged")),
        avg_events_per_visit: (visits > 0).then(|| {
            row.try_get::<Option<f64>, _>("avg_events")
                .ok()
                .flatten()
                .unwrap_or(0.0)
        }),
        scroll_reach_75: (scroll_visits > 0)
            .then(|| frac(g("scrolled_75")))
            .flatten(),
        outbound_rate: (outbound_visits > 0)
            .then(|| frac(g("outbound_visits")))
            .flatten(),
        scroll_funnel: (scroll_visits > 0).then(|| ScrollFunnel {
            p25: g("scrolled_25") as f64 / visits as f64,
            p50: g("scrolled_50") as f64 / visits as f64,
            p75: g("scrolled_75") as f64 / visits as f64,
            p100: g("scrolled_100") as f64 / visits as f64,
        }),
    })
}

/// Per-path engagement. `site_scroll_visits` / `site_outbound_visits` are window
/// totals over every path (computed before LIMIT) so the opt-in omission is
/// decided site-wide — a tracked path with zero clicks shows `0.0`, not omitted.
pub async fn engagement_by_path(
    pool: &PgPool,
    site_id: &str,
    from_ts: i64,
    to_ts: i64,
    limit: i64,
) -> sqlx::Result<Vec<EngagementRow>> {
    let sql = format!(
        "{VIEWS_CTE}
         SELECT
            path AS key,
            count(*)::bigint AS visits,
            count(*) FILTER (WHERE
                coalesce(dur_ms,0) >= 10000 OR coalesce(max_scroll,0) >= 50 OR has_click
            )::bigint AS engaged,
            count(*) FILTER (WHERE max_scroll >= 75)::bigint AS scrolled_75,
            count(*) FILTER (WHERE outbound_n > 0)::bigint AS outbound_visits,
            (avg(custom_events))::float8 AS avg_events,
            (sum(count(*) FILTER (WHERE max_scroll IS NOT NULL)) OVER ())::bigint AS site_scroll_visits,
            (sum(count(*) FILTER (WHERE outbound_n > 0)) OVER ())::bigint AS site_outbound_visits
         FROM views
         WHERE is_visit AND path IS NOT NULL
         GROUP BY path ORDER BY visits DESC, key ASC LIMIT $4"
    );
    let rows = sqlx::query(&sql)
        .bind(site_id)
        .bind(from_ts)
        .bind(to_ts)
        .bind(limit)
        .fetch_all(pool)
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let g = |k: &str| -> i64 { r.try_get::<i64, _>(k).unwrap_or(0) };
            let visits = g("visits");
            let frac = |n: i64| n as f64 / visits as f64;
            EngagementRow {
                key: r
                    .try_get::<Option<String>, _>("key")
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "(none)".into()),
                visits,
                engaged_visit_rate: frac(g("engaged")),
                avg_events_per_visit: r
                    .try_get::<Option<f64>, _>("avg_events")
                    .ok()
                    .flatten()
                    .unwrap_or(0.0),
                scroll_reach_75: (g("site_scroll_visits") > 0).then(|| frac(g("scrolled_75"))),
                outbound_rate: (g("site_outbound_visits") > 0).then(|| frac(g("outbound_visits"))),
            }
        })
        .collect())
}

/// Sessionizes a site's pageviews (rung 3, opt-in). Partitions by `visitor_hash`
/// (which already encodes the UTC day — the salt rotates daily), orders by client
/// `ts`, and opens a new session whenever the gap exceeds `$4` ms. `$4` is a bind
/// param. Reused by `sessions` and `session_pages`.
const SESS_CTE: &str = "WITH pv AS (
    SELECT visitor_hash, ts, path,
           ts - lag(ts) OVER (PARTITION BY visitor_hash ORDER BY ts) AS gap
    FROM analytics_events
    WHERE site_id = $1 AND ts BETWEEN $2 AND $3
      AND type = 'pageview' AND visitor_hash IS NOT NULL
),
marked AS (
    SELECT *, sum(CASE WHEN gap IS NULL OR gap > $4 THEN 1 ELSE 0 END)
                  OVER (PARTITION BY visitor_hash ORDER BY ts) AS session_seq
    FROM pv
),
sess AS (
    SELECT visitor_hash, session_seq,
           count(*) AS pageviews,
           max(ts) - min(ts) AS duration_ms,
           (array_agg(path ORDER BY ts))[1]      AS entry_path,
           (array_agg(path ORDER BY ts DESC))[1] AS exit_path
    FROM marked
    GROUP BY visitor_hash, session_seq
)";

/// Site-wide session aggregates. Empty (`sessions = 0`, rates `None`) when
/// sessions are disabled or there is no `visitor_hash` data in range.
pub async fn sessions(
    pool: &PgPool,
    site_id: &str,
    from_ts: i64,
    to_ts: i64,
    gap_ms: i64,
) -> sqlx::Result<Sessions> {
    let sql = format!(
        "{SESS_CTE}
         SELECT count(*)::bigint AS sessions,
                avg(pageviews)::float8 AS avg_pages,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY pageviews::float8))::float8 AS median_pages,
                avg(duration_ms)::float8 AS avg_duration,
                (percentile_cont(0.5) WITHIN GROUP (ORDER BY duration_ms::float8))::float8 AS median_duration,
                avg((pageviews = 1)::int)::float8 AS bounce_rate
         FROM sess"
    );
    let row = sqlx::query(&sql)
        .bind(site_id)
        .bind(from_ts)
        .bind(to_ts)
        .bind(gap_ms)
        .fetch_one(pool)
        .await?;

    let pick = |k: &str| -> Option<f64> { row.try_get::<Option<f64>, _>(k).ok().flatten() };
    Ok(Sessions {
        sessions: row.try_get("sessions").unwrap_or(0),
        avg_pages_per_session: pick("avg_pages"),
        median_pages_per_session: pick("median_pages"),
        avg_duration_ms: pick("avg_duration"),
        median_duration_ms: pick("median_duration"),
        bounce_rate: pick("bounce_rate"),
    })
}

/// Top entry or exit pages by session count. `col` is an allowlisted column name
/// (`entry_path` / `exit_path`) chosen by the handler — never user input.
pub async fn session_pages(
    pool: &PgPool,
    site_id: &str,
    from_ts: i64,
    to_ts: i64,
    gap_ms: i64,
    col: &str,
    limit: i64,
) -> sqlx::Result<Vec<TopRow>> {
    let sql = format!(
        "{SESS_CTE}
         SELECT {col} AS key, count(*)::bigint AS count
         FROM sess WHERE {col} IS NOT NULL
         GROUP BY {col} ORDER BY count DESC, key ASC LIMIT $5"
    );
    let rows = sqlx::query(&sql)
        .bind(site_id)
        .bind(from_ts)
        .bind(to_ts)
        .bind(gap_ms)
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
            avg_dur_ms: None,
            median_dur_ms: None,
        })
        .collect())
}

/// Path funnel (rung 3b): how far each session gets down an ordered list of
/// pageview paths, in time order. Returns the number of sessions reaching each
/// step (length == `steps.len()`). Reuses 3a sessionization; `steps` is bound as
/// a `text[]` param, never interpolated. The greedy longest in-order prefix is
/// computed in Rust from the per-session step-index arrays.
pub async fn funnel(
    pool: &PgPool,
    site_id: &str,
    from_ts: i64,
    to_ts: i64,
    gap_ms: i64,
    steps: &[String],
) -> sqlx::Result<Vec<i64>> {
    let rows = sqlx::query(
        "WITH pv AS (
            SELECT visitor_hash, ts, received_at, path,
                   ts - lag(ts) OVER (PARTITION BY visitor_hash ORDER BY ts, received_at) AS gap
            FROM analytics_events
            WHERE site_id = $1 AND ts BETWEEN $2 AND $3
              AND type = 'pageview' AND visitor_hash IS NOT NULL
        ),
        marked AS (
            SELECT visitor_hash, ts, received_at, path,
                   sum(CASE WHEN gap IS NULL OR gap > $4 THEN 1 ELSE 0 END)
                       OVER (PARTITION BY visitor_hash ORDER BY ts, received_at) AS session_seq
            FROM pv
        ),
        matched AS (
            SELECT visitor_hash, session_seq, ts, received_at,
                   array_position($5::text[], path) AS step_idx
            FROM marked WHERE path = ANY($5::text[])
        ),
        seqs AS (
            -- received_at breaks ties so the step order (and the funnel result)
            -- is deterministic when two pageviews share a millisecond ts.
            SELECT array_agg(step_idx ORDER BY ts, received_at) AS steps
            FROM matched GROUP BY visitor_hash, session_seq
        )
        SELECT steps FROM seqs",
    )
    .bind(site_id)
    .bind(from_ts)
    .bind(to_ts)
    .bind(gap_ms)
    .bind(steps)
    .fetch_all(pool)
    .await?;

    let k = steps.len();
    let mut counts = vec![0i64; k];
    for r in &rows {
        let arr: Vec<i32> = r.try_get("steps").unwrap_or_default();
        // Greedy: advance the expected step each time we see it in time order.
        let mut expected = 1i32;
        for s in arr {
            if s == expected {
                expected += 1;
                if expected as usize > k {
                    break;
                }
            }
        }
        let depth = (expected - 1) as usize;
        for c in counts.iter_mut().take(depth) {
            *c += 1;
        }
    }
    Ok(counts)
}
