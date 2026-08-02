//! Weekly digest email: a plain-English, week-over-week summary of a site's
//! core metrics, pushed to its `CONTACT_TO_<SITE>` recipient so owners don't
//! have to open a dashboard.
//!
//! Deliberately small: it reuses the existing `/stats` query helpers (no new
//! SQL) and the existing `Mailer` (no new send path). Run once a week by a
//! systemd timer via `dullahan --digest`.

use crate::config::Config;
use crate::db;
use crate::email::Mailer;
use crate::types::{Summary, TopDimension, TopRow};
use sqlx::PgPool;

const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// One site's week: the current 7 days, the preceding 7 days for comparison,
/// and the top pages / referrers over the current week.
pub struct Digest {
    pub site: String,
    pub current: Summary,
    pub previous: Summary,
    pub top_pages: Vec<TopRow>,
    pub top_referrers: Vec<TopRow>,
    /// Empty unless a proxy supplies `x-country` on ingest, in which case the
    /// section is omitted rather than rendered as zeros — same honest-null rule the
    /// session metrics follow.
    pub top_countries: Vec<TopRow>,
    pub top_devices: Vec<TopRow>,
}

/// Compute a site's digest over the 7 days ending at `now_ms`, comparing
/// against the preceding 7 days. Reuses the `/stats/summary` and `/stats/top`
/// helpers verbatim — same windows, same honest-null semantics.
pub async fn compute(pool: &PgPool, site: &str, now_ms: i64) -> sqlx::Result<Digest> {
    let to_ts = now_ms;
    let from_ts = to_ts - WEEK_MS;
    // `from_ts - 1`: the current window's BETWEEN is inclusive of `from_ts`, so
    // the previous window stops one ms short to avoid double-counting the edge.
    let current = db::summary(pool, site, from_ts, to_ts).await?;
    let previous = db::summary(pool, site, from_ts - WEEK_MS, from_ts - 1).await?;
    let top_pages = db::top(pool, site, from_ts, to_ts, TopDimension::Path, 5).await?;
    let top_referrers = db::top(pool, site, from_ts, to_ts, TopDimension::Referrer, 5).await?;
    let top_countries = db::top(pool, site, from_ts, to_ts, TopDimension::Country, 5).await?;
    // Three, not five: the dimension only ever holds mobile/tablet/desktop.
    let top_devices = db::top(pool, site, from_ts, to_ts, TopDimension::Device, 3).await?;
    Ok(Digest {
        site: site.to_string(),
        current,
        previous,
        top_pages,
        top_referrers,
        top_countries,
        top_devices,
    })
}

/// Send (or, when `dry_run`, print) a digest for every configured site.
///
/// Iterates the active tenants in the `sites` table. A site with no
/// `contact_to` is skipped (never delivered elsewhere, mirroring `/contact`).
/// Errors on a single site are logged and do not abort the rest.
///
/// This is a one-shot process (`dullahan --digest`), so it reads the registry
/// directly rather than using the server's cache.
pub async fn run(
    pool: &PgPool,
    mailer: Option<&Mailer>,
    _config: &Config,
    now_ms: i64,
    dry_run: bool,
) -> anyhow::Result<()> {
    let sites = crate::sites::load(pool).await?.active();
    if sites.is_empty() {
        anyhow::bail!("--digest found no active rows in `sites`; register a site first");
    }
    if !dry_run && mailer.is_none() {
        anyhow::bail!(
            "--digest needs email configured (RESEND_API_KEY + EMAIL_FROM); use --dry-run to preview"
        );
    }

    for entry in sites {
        let site = &*entry.id;
        let Some(to) = entry.contact_to.as_deref() else {
            tracing::warn!(site, "no contact_to recipient; skipping digest");
            continue;
        };
        let digest = match compute(pool, site, now_ms).await {
            Ok(d) => d,
            Err(err) => {
                tracing::error!(site, error = %err, "digest computation failed; skipping");
                continue;
            }
        };
        let subject = subject(&digest);
        let html = render_html(&digest);

        if dry_run {
            println!("--- digest for {site} -> {to} ---\nSubject: {subject}\n{html}\n");
            continue;
        }

        match mailer
            .expect("mailer presence checked above")
            .send_html(
                to,
                &subject,
                &html,
                entry.email_from.as_deref(),
                entry.email_from_name.as_deref(),
                None,
            )
            .await
        {
            Ok(()) => tracing::info!(site, to, "digest sent"),
            Err(err) => tracing::error!(site, error = %err, "digest send failed"),
        }
    }
    Ok(())
}

/// `"site — 1,234 views last week (↑12%)"`.
pub fn subject(d: &Digest) -> String {
    format!(
        "{} — {} views last week ({})",
        d.site,
        thousands(d.current.pageviews),
        arrow(pct_change(d.current.pageviews, d.previous.pageviews)),
    )
}

pub fn render_html(d: &Digest) -> String {
    let mut html = String::new();
    html.push_str(
        "<div style=\"font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;\
         max-width:560px;margin:0 auto;color:#1a1a1a;line-height:1.5\">",
    );
    html.push_str(&format!(
        "<h1 style=\"font-size:20px;margin:0 0 4px\">Weekly report — {}</h1>\
         <p style=\"color:#666;margin:0 0 24px;font-size:14px\">The last 7 days vs the 7 before.</p>",
        esc(&d.site)
    ));

    // Headline metrics as labelled rows with a week-over-week delta.
    html.push_str("<table style=\"width:100%;border-collapse:collapse;font-size:15px\">");
    html.push_str(&metric_row(
        "Pageviews",
        &thousands(d.current.pageviews),
        pct_change(d.current.pageviews, d.previous.pageviews),
    ));
    match (d.current.avg_daily_visitors, d.previous.avg_daily_visitors) {
        (Some(cur), prev) => html.push_str(&metric_row(
            "Avg. daily visitors",
            &thousands(cur.round() as i64),
            prev.and_then(|p| pct_change_f64(cur, p)),
        )),
        // Honest null: sessions off ⇒ not measured, never shown as 0.
        (None, _) => html.push_str(&not_measured_row("Avg. daily visitors")),
    }
    match d.current.bounce_rate {
        Some(rate) => html.push_str(&plain_row("Bounce rate", &format!("{:.0}%", rate * 100.0))),
        None => html.push_str(&not_measured_row("Bounce rate")),
    }
    if let Some(ms) = d.current.avg_time_on_page_ms {
        html.push_str(&plain_row("Avg. time on page", &duration(ms)));
    }
    html.push_str("</table>");

    html.push_str(&list_section("Top pages", &d.top_pages));
    html.push_str(&list_section("Top referrers", &d.top_referrers));
    html.push_str(&list_section("Top countries", &d.top_countries));
    html.push_str(&list_section("Devices", &d.top_devices));

    html.push_str(
        "<p style=\"color:#999;font-size:12px;margin-top:32px\">\
         Sent by dullahan. No cookies, no tracking of you — just your own site's aggregate numbers.</p>",
    );
    html.push_str("</div>");
    html
}

fn metric_row(label: &str, value: &str, change: Option<f64>) -> String {
    format!(
        "<tr><td style=\"padding:8px 0;border-bottom:1px solid #eee\">{}</td>\
         <td style=\"padding:8px 0;border-bottom:1px solid #eee;text-align:right;font-weight:600\">{}</td>\
         <td style=\"padding:8px 0 8px 12px;border-bottom:1px solid #eee;text-align:right;color:#666;white-space:nowrap\">{}</td></tr>",
        esc(label),
        esc(value),
        arrow(change),
    )
}

fn plain_row(label: &str, value: &str) -> String {
    format!(
        "<tr><td style=\"padding:8px 0;border-bottom:1px solid #eee\">{}</td>\
         <td style=\"padding:8px 0;border-bottom:1px solid #eee;text-align:right;font-weight:600\">{}</td>\
         <td style=\"border-bottom:1px solid #eee\"></td></tr>",
        esc(label),
        esc(value),
    )
}

fn not_measured_row(label: &str) -> String {
    format!(
        "<tr><td style=\"padding:8px 0;border-bottom:1px solid #eee\">{}</td>\
         <td colspan=\"2\" style=\"padding:8px 0;border-bottom:1px solid #eee;text-align:right;color:#999\">not measured</td></tr>",
        esc(label),
    )
}

fn list_section(title: &str, rows: &[TopRow]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut s = format!(
        "<h2 style=\"font-size:15px;margin:28px 0 8px\">{}</h2>\
         <table style=\"width:100%;border-collapse:collapse;font-size:14px\">",
        esc(title)
    );
    for r in rows {
        s.push_str(&format!(
            "<tr><td style=\"padding:6px 0;border-bottom:1px solid #f2f2f2\">{}</td>\
             <td style=\"padding:6px 0;border-bottom:1px solid #f2f2f2;text-align:right;color:#666\">{}</td></tr>",
            esc(&r.key),
            thousands(r.count),
        ));
    }
    s.push_str("</table>");
    s
}

/// Percentage change of `current` vs `previous`; `None` when there's no prior
/// baseline (a 0 previous makes the percentage undefined).
fn pct_change(current: i64, previous: i64) -> Option<f64> {
    if previous == 0 {
        return None;
    }
    Some((current - previous) as f64 / previous as f64 * 100.0)
}

/// Percentage change for fractional metrics (avg daily visitors).
fn pct_change_f64(current: f64, previous: f64) -> Option<f64> {
    if previous == 0.0 {
        return None;
    }
    Some((current - previous) / previous * 100.0)
}

/// `"↑12%"` / `"↓5%"` / `"–"` (no baseline) / `"0%"` (flat).
fn arrow(change: Option<f64>) -> String {
    match change {
        None => "–".to_string(),
        Some(pct) if pct > 0.0 => format!("↑{:.0}%", pct),
        Some(pct) if pct < 0.0 => format!("↓{:.0}%", pct.abs()),
        Some(_) => "0%".to_string(),
    }
}

/// `1234` -> `"1,234"`.
fn thousands(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// Milliseconds -> a coarse human string (`"45s"`, `"2m 05s"`).
fn duration(ms: f64) -> String {
    let secs = (ms / 1000.0).round() as i64;
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// Minimal HTML-attribute/text escaping for values interpolated into the email
/// (paths, referrers, site ids are attacker-influenced in principle).
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(pageviews: i64, avg_daily: Option<f64>, bounce: Option<f64>) -> Summary {
        Summary {
            pageviews,
            events: 0,
            top_path: None,
            avg_time_on_page_ms: Some(65_000.0),
            avg_daily_visitors: avg_daily,
            bounce_rate: bounce,
        }
    }

    fn row(key: &str, count: i64) -> TopRow {
        TopRow {
            key: key.into(),
            count,
        }
    }

    fn digest() -> Digest {
        Digest {
            site: "acme".into(),
            current: summary(1200, Some(800.0), Some(0.42)),
            previous: summary(1000, Some(700.0), Some(0.5)),
            top_pages: vec![row("/", 500), row("/pricing", 200)],
            top_referrers: vec![row("google.com", 300)],
            top_countries: vec![row("IE", 700), row("GB", 250)],
            top_devices: vec![row("desktop", 800), row("mobile", 400)],
        }
    }

    #[test]
    fn thousands_groups_digits() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_234), "1,234");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn arrow_reflects_direction() {
        assert_eq!(arrow(None), "–");
        assert_eq!(arrow(Some(0.0)), "0%");
        assert_eq!(arrow(Some(12.4)), "↑12%");
        assert_eq!(arrow(Some(-5.0)), "↓5%");
    }

    #[test]
    fn pct_change_is_none_without_baseline() {
        assert_eq!(pct_change(10, 0), None);
        assert_eq!(pct_change(120, 100), Some(20.0));
    }

    #[test]
    fn subject_summarizes_the_week() {
        assert_eq!(subject(&digest()), "acme — 1,200 views last week (↑20%)");
    }

    #[test]
    fn render_includes_metrics_and_lists() {
        let html = render_html(&digest());
        assert!(html.contains("Weekly report — acme"));
        assert!(html.contains("1,200")); // pageviews
        assert!(html.contains("↑20%")); // pageviews delta
        assert!(html.contains("42%")); // bounce
        assert!(html.contains("/pricing"));
        assert!(html.contains("google.com"));
    }

    #[test]
    fn render_includes_countries_and_devices() {
        let html = render_html(&digest());
        assert!(html.contains("Top countries"));
        assert!(html.contains("IE"));
        assert!(html.contains("Devices"));
        assert!(html.contains("desktop"));
    }

    #[test]
    fn country_section_is_omitted_when_no_country_data() {
        // country is only populated when a proxy supplies `x-country`. Without it
        // the section must disappear rather than render an empty table or a "0" —
        // the same rule the session metrics follow. Devices are unaffected, since
        // the tracker always sends `d`.
        let mut d = digest();
        d.top_countries = vec![];
        let html = render_html(&d);
        assert!(!html.contains("Top countries"));
        assert!(html.contains("Devices"));
    }

    #[test]
    fn visitor_metrics_omitted_when_sessions_off() {
        let mut d = digest();
        d.current.avg_daily_visitors = None;
        d.current.bounce_rate = None;
        let html = render_html(&d);
        assert!(html.contains("not measured"));
        // Must never fabricate a zero for an unmeasured metric.
        assert!(!html.contains(">0<"));
    }

    #[test]
    fn escapes_hostile_values() {
        let mut d = digest();
        d.top_pages = vec![row("/<script>", 1)];
        let html = render_html(&d);
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("/<script>"));
    }
}
