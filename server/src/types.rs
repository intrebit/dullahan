use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "t")]
pub enum RawPayload {
    #[serde(rename = "pageview")]
    Pageview {
        s: String,
        p: String,
        ts: i64,
        #[serde(default)]
        r: Option<String>,
        #[serde(default)]
        d: Option<String>,
        #[serde(default)]
        v: Option<i32>,
        #[serde(default)]
        u: Option<Utm>,
        #[serde(default)]
        vid: Option<String>,
    },
    #[serde(rename = "event")]
    Event {
        s: String,
        p: String,
        ts: i64,
        n: String,
        #[serde(default)]
        pr: Option<HashMap<String, serde_json::Value>>,
        #[serde(default)]
        vid: Option<String>,
    },
    #[serde(rename = "performance")]
    Performance {
        s: String,
        p: String,
        ts: i64,
        pf: PerformanceMetrics,
        #[serde(default)]
        vid: Option<String>,
    },
    #[serde(rename = "pageleave")]
    Pageleave {
        s: String,
        p: String,
        ts: i64,
        dur: i32,
        #[serde(default)]
        vid: Option<String>,
    },
}

pub const MAX_SITE_ID: usize = 64;
pub const MAX_PATH: usize = 2048;
pub const MAX_REFERRER: usize = 253;
pub const MAX_EVENT_NAME: usize = 64;
pub const MAX_UTM: usize = 128;
pub const MAX_VID: usize = 64;
pub const MAX_PROP_KEYS: usize = 32;
pub const MAX_PROP_KEY_LEN: usize = 64;
pub const MAX_PROP_VALUE_LEN: usize = 1024;

const VALID_DEVICES: [&str; 3] = ["mobile", "tablet", "desktop"];

/// Client `ts` is not trusted for range bucketing. Absurd values (clock skew,
/// spoofing) are clamped into a sane window around the server clock so one
/// client can't poison the time series with year-3000 (or epoch-0) rows.
pub const TS_MAX_FUTURE_MS: i64 = 24 * 60 * 60 * 1000;
pub const TS_MAX_PAST_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// UTM campaign tags parsed from the landing URL query string (pageview only).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Utm {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub m: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c: Option<String>,
}

impl RawPayload {
    /// Validate user-supplied lengths and float sanity. Body size is already
    /// capped at the router layer; this adds per-field caps so a single 16KB
    /// payload can't stuff one giant value into an indexed column.
    pub fn validate(&mut self) -> Result<(), &'static str> {
        let s = self.site_id();
        if s.is_empty() || s.len() > MAX_SITE_ID {
            return Err("invalid site_id");
        }
        let path = match self {
            RawPayload::Pageview { p, .. }
            | RawPayload::Pageleave { p, .. }
            | RawPayload::Performance { p, .. }
            | RawPayload::Event { p, .. } => p.as_str(),
        };
        if path.is_empty() || path.len() > MAX_PATH {
            return Err("invalid path");
        }
        if let RawPayload::Pageview { r: Some(r), .. } = self
            && r.len() > MAX_REFERRER
        {
            return Err("invalid referrer");
        }
        if let RawPayload::Pageview { u: Some(u), .. } = self {
            for field in [&u.s, &u.m, &u.c] {
                if let Some(v) = field
                    && v.len() > MAX_UTM
                {
                    return Err("invalid utm");
                }
            }
        }
        if let RawPayload::Event { n, .. } = self
            && (n.is_empty() || n.len() > MAX_EVENT_NAME)
        {
            return Err("invalid event name");
        }
        if let Some(vid) = self.vid()
            && vid.len() > MAX_VID
        {
            return Err("invalid vid");
        }
        // An empty view_id is meaningless and, stored as '' (not NULL), would
        // collapse distinct page-visits into one bogus bucket; treat it as absent.
        match self {
            RawPayload::Pageview { vid, .. }
            | RawPayload::Event { vid, .. }
            | RawPayload::Performance { vid, .. }
            | RawPayload::Pageleave { vid, .. } => {
                if vid.as_deref() == Some("") {
                    *vid = None;
                }
            }
        }
        // Coerce an unrecognized device class to NULL instead of letting it fail
        // the DB CHECK constraint — the insert is fire-and-forget, so a rejected
        // row would drop the whole (otherwise valid) event silently.
        if let RawPayload::Pageview { d, .. } = self
            && d.as_deref()
                .is_some_and(|dev| !VALID_DEVICES.contains(&dev))
        {
            *d = None;
        }
        // Cap event props so a single 16KB body can't stuff one giant value (or
        // a flood of keys) into the unindexed jsonb column.
        if let RawPayload::Event {
            pr: Some(props), ..
        } = self
        {
            if props.len() > MAX_PROP_KEYS {
                return Err("too many event props");
            }
            for (k, v) in props.iter() {
                if k.len() > MAX_PROP_KEY_LEN {
                    return Err("invalid event prop key");
                }
                if serde_json::to_string(v).map(|s| s.len()).unwrap_or(0) > MAX_PROP_VALUE_LEN {
                    return Err("invalid event prop value");
                }
            }
        }
        if let RawPayload::Performance { pf, .. } = self {
            // Postgres percentile_cont chokes on NaN; drop non-finite values.
            for v in [
                &mut pf.lcp,
                &mut pf.fcp,
                &mut pf.cls,
                &mut pf.inp,
                &mut pf.ttfb,
            ] {
                if let Some(x) = *v
                    && !x.is_finite()
                {
                    *v = None;
                }
            }
        }
        Ok(())
    }

    pub fn site_id(&self) -> &str {
        match self {
            RawPayload::Pageview { s, .. }
            | RawPayload::Event { s, .. }
            | RawPayload::Performance { s, .. }
            | RawPayload::Pageleave { s, .. } => s,
        }
    }

    pub fn vid(&self) -> Option<&str> {
        match self {
            RawPayload::Pageview { vid, .. }
            | RawPayload::Event { vid, .. }
            | RawPayload::Performance { vid, .. }
            | RawPayload::Pageleave { vid, .. } => vid.as_deref(),
        }
    }

    /// Pin the client timestamp into `[now - TS_MAX_PAST_MS, now + TS_MAX_FUTURE_MS]`.
    pub fn clamp_ts(&mut self, now_ms: i64) {
        let ts = match self {
            RawPayload::Pageview { ts, .. }
            | RawPayload::Event { ts, .. }
            | RawPayload::Performance { ts, .. }
            | RawPayload::Pageleave { ts, .. } => ts,
        };
        *ts = (*ts).clamp(now_ms - TS_MAX_PAST_MS, now_ms + TS_MAX_FUTURE_MS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pv(s: &str, p: &str) -> RawPayload {
        RawPayload::Pageview {
            s: s.into(),
            p: p.into(),
            ts: 0,
            r: None,
            d: None,
            v: None,
            u: None,
            vid: None,
        }
    }

    #[test]
    fn rejects_empty_site() {
        assert!(pv("", "/").validate().is_err());
    }

    #[test]
    fn rejects_oversize_site() {
        assert!(pv(&"x".repeat(MAX_SITE_ID + 1), "/").validate().is_err());
    }

    #[test]
    fn rejects_empty_path() {
        assert!(pv("s", "").validate().is_err());
    }

    #[test]
    fn rejects_oversize_path() {
        assert!(pv("s", &"a".repeat(MAX_PATH + 1)).validate().is_err());
    }

    #[test]
    fn accepts_normal_pageview() {
        assert!(pv("site-1", "/about").validate().is_ok());
    }

    #[test]
    fn rejects_oversize_referrer() {
        let mut p = pv("s", "/");
        if let RawPayload::Pageview { r, .. } = &mut p {
            *r = Some("a".repeat(MAX_REFERRER + 1));
        }
        assert!(p.validate().is_err());
    }

    #[test]
    fn rejects_empty_event_name() {
        let mut p = RawPayload::Event {
            s: "s".into(),
            p: "/".into(),
            ts: 0,
            n: "".into(),
            pr: None,
            vid: None,
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn rejects_oversize_event_name() {
        let mut p = RawPayload::Event {
            s: "s".into(),
            p: "/".into(),
            ts: 0,
            n: "n".repeat(MAX_EVENT_NAME + 1),
            pr: None,
            vid: None,
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn drops_non_finite_metrics() {
        let mut p = RawPayload::Performance {
            s: "s".into(),
            p: "/".into(),
            ts: 0,
            pf: PerformanceMetrics {
                lcp: Some(f64::NAN),
                fcp: Some(f64::INFINITY),
                cls: Some(0.1),
                inp: Some(f64::NEG_INFINITY),
                ttfb: None,
            },
            vid: None,
        };
        p.validate().unwrap();
        if let RawPayload::Performance { pf, .. } = p {
            assert!(pf.lcp.is_none());
            assert!(pf.fcp.is_none());
            assert_eq!(pf.cls, Some(0.1));
            assert!(pf.inp.is_none());
        } else {
            panic!()
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PerformanceMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lcp: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fcp: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cls: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inp: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttfb: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub pageviews: i64,
    pub events: i64,
    pub top_path: Option<String>,
    #[serde(rename = "avgTimeOnPageMs", skip_serializing_if = "Option::is_none")]
    pub avg_time_on_page_ms: Option<f64>,
    #[serde(rename = "medianTimeOnPageMs", skip_serializing_if = "Option::is_none")]
    pub median_time_on_page_ms: Option<f64>,
    #[serde(rename = "p75TimeOnPageMs", skip_serializing_if = "Option::is_none")]
    pub p75_time_on_page_ms: Option<f64>,
    /// Distinct visitor hashes (rung 2). `None` when sessions are disabled or
    /// there is no session data in range.
    #[serde(rename = "uniqueVisitors", skip_serializing_if = "Option::is_none")]
    pub unique_visitors: Option<i64>,
    /// Share (0–1) of sessions with a single pageview. `None` when sessions are
    /// disabled or there is no session data in range.
    #[serde(rename = "bounceRate", skip_serializing_if = "Option::is_none")]
    pub bounce_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimeseriesPoint {
    pub bucket: chrono::DateTime<chrono::Utc>,
    pub pageviews: i64,
    /// Distinct visitor hashes in this bucket. `None` when sessions are disabled.
    /// Reported per-bucket so a multi-day range shows per-day uniques rather than
    /// the daily-salt-inflated total.
    #[serde(rename = "uniqueVisitors", skip_serializing_if = "Option::is_none")]
    pub unique_visitors: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TopRow {
    pub key: String,
    pub count: i64,
    #[serde(rename = "avgDurMs", skip_serializing_if = "Option::is_none")]
    pub avg_dur_ms: Option<f64>,
    #[serde(rename = "medianDurMs", skip_serializing_if = "Option::is_none")]
    pub median_dur_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Vitals {
    #[serde(rename = "lcpP75", skip_serializing_if = "Option::is_none")]
    pub lcp_p75: Option<f64>,
    #[serde(rename = "fcpP75", skip_serializing_if = "Option::is_none")]
    pub fcp_p75: Option<f64>,
    #[serde(rename = "clsP75", skip_serializing_if = "Option::is_none")]
    pub cls_p75: Option<f64>,
    #[serde(rename = "inpP75", skip_serializing_if = "Option::is_none")]
    pub inp_p75: Option<f64>,
    #[serde(rename = "ttfbP75", skip_serializing_if = "Option::is_none")]
    pub ttfb_p75: Option<f64>,
    /// Core-Web-Vitals pass-rate buckets (good / needs-improvement / poor) per
    /// metric, against Google's thresholds. `None` when there is no perf data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distribution: Option<VitalsDistribution>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VitalsDistribution {
    pub lcp: MetricBucket,
    pub fcp: MetricBucket,
    pub cls: MetricBucket,
    pub inp: MetricBucket,
    pub ttfb: MetricBucket,
}

/// Sample counts for one metric. `needsImprovement = total - good - poor`.
#[derive(Debug, Clone, Serialize)]
pub struct MetricBucket {
    pub good: i64,
    #[serde(rename = "needsImprovement")]
    pub needs_improvement: i64,
    pub poor: i64,
    pub total: i64,
}

/// Per-path vitals breakdown (`/stats/vitals?dim=path`). Each metric carries its
/// own sample count because coverage varies — INP only exists on rows with an
/// interaction, so its `n` is typically far below `samples`.
#[derive(Debug, Clone, Serialize)]
pub struct VitalsRow {
    pub key: String,
    pub samples: i64,
    #[serde(rename = "lcpP75", skip_serializing_if = "Option::is_none")]
    pub lcp_p75: Option<f64>,
    #[serde(rename = "lcpN")]
    pub lcp_n: i64,
    #[serde(rename = "fcpP75", skip_serializing_if = "Option::is_none")]
    pub fcp_p75: Option<f64>,
    #[serde(rename = "fcpN")]
    pub fcp_n: i64,
    #[serde(rename = "clsP75", skip_serializing_if = "Option::is_none")]
    pub cls_p75: Option<f64>,
    #[serde(rename = "clsN")]
    pub cls_n: i64,
    #[serde(rename = "inpP75", skip_serializing_if = "Option::is_none")]
    pub inp_p75: Option<f64>,
    #[serde(rename = "inpN")]
    pub inp_n: i64,
    #[serde(rename = "ttfbP75", skip_serializing_if = "Option::is_none")]
    pub ttfb_p75: Option<f64>,
    #[serde(rename = "ttfbN")]
    pub ttfb_n: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeatmapCell {
    /// ISO weekday: 1 = Monday … 7 = Sunday.
    pub weekday: i32,
    /// Hour of day 0–23, in the requested timezone.
    pub hour: i32,
    pub pageviews: i64,
}

/// Real-time active page-visits in the trailing `window_minutes`, keyed on the
/// server `received_at` (not the client `ts`). `active` = distinct `view_id`
/// with any event in the window; `pages` is the top active paths.
#[derive(Debug, Clone, Serialize)]
pub struct Realtime {
    pub active: i64,
    #[serde(rename = "windowMinutes")]
    pub window_minutes: i64,
    pub pages: Vec<TopRow>,
}

/// Per-page-visit engagement (site-wide). Rates are a fraction 0–1. Scroll /
/// outbound metrics are `None` when the site emitted no such rows in range —
/// scroll/outbound tracking is a client opt-in, so absence means "not measured",
/// never "0% engaged".
#[derive(Debug, Clone, Serialize)]
pub struct Engagement {
    /// Distinct page-visits (a `view_id` with a pageview) in range.
    pub visits: i64,
    /// Share of visits that were engaged: visible ≥10s OR scrolled ≥50% OR an
    /// outbound/download click. A lower bound when scroll/outbound tracking is
    /// off (it then rests on the time signal alone).
    #[serde(rename = "engagedVisitRate", skip_serializing_if = "Option::is_none")]
    pub engaged_visit_rate: Option<f64>,
    /// Mean custom `track()` events per visit (auto scroll/outbound/download
    /// excluded).
    #[serde(rename = "avgEventsPerVisit", skip_serializing_if = "Option::is_none")]
    pub avg_events_per_visit: Option<f64>,
    /// Share of visits whose deepest scroll reached ≥75%.
    #[serde(rename = "scrollReach75", skip_serializing_if = "Option::is_none")]
    pub scroll_reach_75: Option<f64>,
    /// Share of visits with at least one outbound-link click.
    #[serde(rename = "outboundRate", skip_serializing_if = "Option::is_none")]
    pub outbound_rate: Option<f64>,
    /// Share of visits reaching each scroll milestone (a visit reaching 100%
    /// counts in every lower bucket too).
    #[serde(rename = "scrollFunnel", skip_serializing_if = "Option::is_none")]
    pub scroll_funnel: Option<ScrollFunnel>,
}

/// Fraction of visits (0–1) reaching each scroll-depth milestone.
#[derive(Debug, Clone, Serialize)]
pub struct ScrollFunnel {
    #[serde(rename = "25")]
    pub p25: f64,
    #[serde(rename = "50")]
    pub p50: f64,
    #[serde(rename = "75")]
    pub p75: f64,
    #[serde(rename = "100")]
    pub p100: f64,
}

/// Per-path engagement row (`/stats/engagement?dim=path`). Each row exists only
/// for paths with ≥1 visit, so `engagedVisitRate` / `avgEventsPerVisit` are
/// always present; scroll / outbound rates follow the same opt-in omission rule
/// as the site-wide object (gated on whether the *site* tracks them at all).
#[derive(Debug, Clone, Serialize)]
pub struct EngagementRow {
    pub key: String,
    pub visits: i64,
    #[serde(rename = "engagedVisitRate")]
    pub engaged_visit_rate: f64,
    #[serde(rename = "avgEventsPerVisit")]
    pub avg_events_per_visit: f64,
    #[serde(rename = "scrollReach75", skip_serializing_if = "Option::is_none")]
    pub scroll_reach_75: Option<f64>,
    #[serde(rename = "outboundRate", skip_serializing_if = "Option::is_none")]
    pub outbound_rate: Option<f64>,
}

/// Session-grain aggregates (`/stats/sessions`). A session is a run of one
/// `visitor_hash`'s pageviews with no gap longer than the requested window;
/// sessions exist only within a UTC day (the salt rotates daily). All rates are
/// `None` when there are no sessions in range (sessions disabled or no data).
#[derive(Debug, Clone, Serialize)]
pub struct Sessions {
    pub sessions: i64,
    #[serde(rename = "avgPagesPerSession", skip_serializing_if = "Option::is_none")]
    pub avg_pages_per_session: Option<f64>,
    #[serde(
        rename = "medianPagesPerSession",
        skip_serializing_if = "Option::is_none"
    )]
    pub median_pages_per_session: Option<f64>,
    #[serde(rename = "avgDurationMs", skip_serializing_if = "Option::is_none")]
    pub avg_duration_ms: Option<f64>,
    #[serde(rename = "medianDurationMs", skip_serializing_if = "Option::is_none")]
    pub median_duration_ms: Option<f64>,
    /// Share of sessions with a single pageview. Distinct from
    /// `summary.bounceRate`, which is single-pageview visitor-*days*.
    #[serde(rename = "bounceRate", skip_serializing_if = "Option::is_none")]
    pub bounce_rate: Option<f64>,
}

/// `summary` wrapper for `compare=prev`. Flattens the current-window summary so
/// the default (no `compare`) response shape is unchanged.
#[derive(Debug, Clone, Serialize)]
pub struct SummaryResponse {
    #[serde(flatten)]
    pub current: Summary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<Summary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<SummaryChange>,
}

/// Percentage change vs the preceding equal-length window. `None` when the
/// previous value is 0 (undefined) or unavailable.
#[derive(Debug, Clone, Serialize)]
pub struct SummaryChange {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pageviews: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<f64>,
    #[serde(rename = "uniqueVisitors", skip_serializing_if = "Option::is_none")]
    pub unique_visitors: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub enum TopDimension {
    Path,
    Referrer,
    Country,
    Device,
    UtmSource,
    UtmMedium,
    UtmCampaign,
    Browser,
    Os,
    Viewport,
}

impl TopDimension {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "path" => Some(Self::Path),
            "referrer" => Some(Self::Referrer),
            "country" => Some(Self::Country),
            "device" => Some(Self::Device),
            "utm_source" => Some(Self::UtmSource),
            "utm_medium" => Some(Self::UtmMedium),
            "utm_campaign" => Some(Self::UtmCampaign),
            "browser" => Some(Self::Browser),
            "os" => Some(Self::Os),
            "viewport" => Some(Self::Viewport),
            _ => None,
        }
    }

    pub fn column(&self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Referrer => "referrer",
            Self::Country => "country",
            Self::Device => "device",
            Self::UtmSource => "utm_source",
            Self::UtmMedium => "utm_medium",
            Self::UtmCampaign => "utm_campaign",
            Self::Browser => "browser",
            Self::Os => "os",
            // viewport is an int column; the generic `top` query reads `key` as
            // text, so cast here (fixed allowlisted string — never user input).
            Self::Viewport => "viewport::text",
        }
    }
}

/// An ordered path funnel (`/stats/funnel`). Each step reports how many sessions
/// reached it **in time order** within a session, plus conversion rates.
#[derive(Debug, Clone, Serialize)]
pub struct Funnel {
    pub steps: Vec<FunnelStep>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunnelStep {
    /// 1-based position in the funnel.
    pub step: i32,
    /// The step's pageview path.
    pub key: String,
    /// Sessions that reached this step in order.
    pub sessions: i64,
    /// `sessions[i] / sessions[i-1]`. `1.0` for step 1; `None` if the previous
    /// step has 0 sessions.
    #[serde(rename = "conversionFromPrev", skip_serializing_if = "Option::is_none")]
    pub conversion_from_prev: Option<f64>,
    /// `sessions[i] / sessions[1]`. `None` if step 1 has 0 sessions.
    #[serde(
        rename = "conversionFromStart",
        skip_serializing_if = "Option::is_none"
    )]
    pub conversion_from_start: Option<f64>,
}
