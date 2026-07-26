//! Shared request/response payloads and database row types.

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
        Ok(())
    }

    pub fn site_id(&self) -> &str {
        match self {
            RawPayload::Pageview { s, .. }
            | RawPayload::Event { s, .. }
            | RawPayload::Pageleave { s, .. } => s,
        }
    }

    pub fn vid(&self) -> Option<&str> {
        match self {
            RawPayload::Pageview { vid, .. }
            | RawPayload::Event { vid, .. }
            | RawPayload::Pageleave { vid, .. } => vid.as_deref(),
        }
    }

    /// Pin the client timestamp into `[now - TS_MAX_PAST_MS, now + TS_MAX_FUTURE_MS]`.
    pub fn clamp_ts(&mut self, now_ms: i64) {
        let ts = match self {
            RawPayload::Pageview { ts, .. }
            | RawPayload::Event { ts, .. }
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
        }
    }
}
