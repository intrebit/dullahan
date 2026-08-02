//! `dullahan --selfcheck` — the on-box half of monitoring.
//!
//! A oneshot process driven by a systemd timer, in the same shape as
//! `--digest`. Being a *separate* process is the point: it can report that the
//! server is down, which an in-process health task never can.
//!
//! What it cannot do is notice that the whole box is gone. That is what
//! `HEALTHCHECK_URL` is for — an external dead-man's-switch that alerts on the
//! *absence* of a ping, so a dead host, a dead timer and a wedged check all
//! surface even though nothing on this machine is left to send an email.

use crate::config::Config;
use crate::email::Mailer;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// Counters whose *increase* since the last run is worth waking someone for.
/// Both mean events were accepted and then lost, which no other signal reveals.
const WATCHED_COUNTERS: [&str; 3] = [
    "dullahan_ingest_insert_failures_total",
    "dullahan_ingest_queue_full_total",
    "dullahan_ingest_queue_closed_total",
];

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// One thing found wrong. `key` is stable so repeat alerts can be throttled per
/// problem rather than per run — a full disk should not silence a DB outage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub key: String,
    pub detail: String,
}

/// Persisted between runs: counter baselines, and when each problem was last
/// mailed about.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    counters: BTreeMap<String, u64>,
    #[serde(default)]
    last_notified_ms: BTreeMap<String, i64>,
}

impl State {
    fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|err| {
                // A corrupt state file must not stop the checks from running;
                // starting fresh only costs one cycle of counter baselines.
                tracing::warn!(error = %err, path = %path.display(), "unreadable selfcheck state; starting fresh");
                Self::default()
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                tracing::warn!(error = %err, path = %path.display(), "could not read selfcheck state");
                Self::default()
            }
        }
    }

    fn save(&self, path: &Path) -> anyhow::Result<()> {
        let body = serde_json::to_string_pretty(self)?;
        // Write-then-rename so an interrupted run cannot leave a half-written
        // file that the next one has to discard.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Run every check, alert on what is wrong, and return the findings.
pub async fn run(config: &Config, dry_run: bool) -> anyhow::Result<Vec<Finding>> {
    let state_path = Path::new(&config.selfcheck_state_path);
    let mut state = State::load(state_path);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let http = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;

    let mut findings = Vec::new();
    findings.extend(check_health(&http, config).await);
    findings.extend(check_database(config).await);
    findings.extend(check_counters(&http, config, &mut state).await);
    findings.extend(check_disk(
        &config.selfcheck_state_path,
        config.alert_disk_percent,
    ));
    findings.extend(check_backup_freshness(
        &config.backup_dir,
        config.alert_backup_max_age_hours,
        now_ms,
    ));

    for f in &findings {
        tracing::warn!(check = %f.key, "{}", f.detail);
    }
    if findings.is_empty() {
        tracing::info!("selfcheck: all checks passed");
    }

    // Only mail about problems not already mailed about recently, so a condition
    // that persists for a week does not send a message every ten minutes.
    let due: Vec<&Finding> = findings
        .iter()
        .filter(|f| {
            let last = state.last_notified_ms.get(&f.key).copied();
            should_notify(now_ms, last, config.alert_repeat_hours)
        })
        .collect();

    if !due.is_empty() {
        let body = render_alert(&due);
        if dry_run {
            println!("--- selfcheck alert ({} due) ---\n{body}", due.len());
        } else {
            match (config.email.clone().map(Mailer::new), &config.alert_to) {
                (Some(mailer), Some(to)) => {
                    let subject = format!(
                        "[dullahan] {} check{} failing",
                        due.len(),
                        if due.len() == 1 { "" } else { "s" }
                    );
                    mailer
                        .send_html(to, &subject, &body, None, None, None)
                        .await?;
                    tracing::info!(to = %to, checks = due.len(), "selfcheck alert sent");
                }
                _ => tracing::warn!(
                    "selfcheck found problems but cannot alert: set ALERT_TO plus \
                     RESEND_API_KEY/EMAIL_FROM"
                ),
            }
        }
        for f in &due {
            state.last_notified_ms.insert(f.key.clone(), now_ms);
        }
    }

    // Clear throttles for checks that recovered, so the *next* occurrence alerts
    // immediately instead of being suppressed by a stale timestamp.
    let failing: Vec<String> = findings.iter().map(|f| f.key.clone()).collect();
    state.last_notified_ms.retain(|k, _| failing.contains(k));

    state.save(state_path)?;
    ping_healthcheck(&http, config, findings.is_empty()).await;
    Ok(findings)
}

/// True when a problem should be mailed about now.
fn should_notify(now_ms: i64, last_ms: Option<i64>, repeat_hours: u32) -> bool {
    match last_ms {
        None => true,
        Some(last) => now_ms - last >= (repeat_hours as i64) * 3_600_000,
    }
}

async fn check_health(http: &reqwest::Client, config: &Config) -> Vec<Finding> {
    let url = format!("{}/health", local_base_url(&config.bind_addr));
    match http.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => Vec::new(),
        Ok(resp) => vec![Finding {
            key: "health".into(),
            detail: format!("{url} returned HTTP {}", resp.status().as_u16()),
        }],
        Err(err) => vec![Finding {
            key: "health".into(),
            detail: format!("{url} is unreachable: {err}"),
        }],
    }
}

async fn check_database(config: &Config) -> Vec<Finding> {
    // A short-lived single connection, not the server's pool: this process only
    // needs to answer "can Postgres serve a query right now".
    match sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&config.database_url)
        .await
    {
        Ok(pool) => {
            let probe = sqlx::query_scalar::<_, i32>("SELECT 1")
                .fetch_one(&pool)
                .await;
            pool.close().await;
            match probe {
                Ok(_) => Vec::new(),
                Err(err) => vec![Finding {
                    key: "database".into(),
                    detail: format!("Postgres accepted a connection but failed a query: {err}"),
                }],
            }
        }
        Err(err) => vec![Finding {
            key: "database".into(),
            detail: format!("cannot connect to Postgres: {err}"),
        }],
    }
}

/// Alert when a lost-event counter has gone up since the previous run.
///
/// These counters only ever increase, so the interesting quantity is the delta.
///
/// A missing baseline counts as **zero**, not as "unknown". The `metrics` crate
/// does not publish a counter until it is first incremented, so the run that
/// first *sees* one already has a non-zero value — treating that as a fresh
/// baseline would swallow exactly the failure worth alerting on. Zero is also
/// the true value at process start, so this is not a guess.
///
/// A *decrease* means the process restarted and the counters reset. That is not
/// a recovery to announce, so it only re-baselines.
async fn check_counters(
    http: &reqwest::Client,
    config: &Config,
    state: &mut State,
) -> Vec<Finding> {
    let url = format!("{}/metrics", local_base_url(&config.bind_addr));
    let body = match http.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.text().await {
            Ok(text) => text,
            Err(err) => {
                tracing::warn!(error = %err, "could not read {url}");
                return Vec::new();
            }
        },
        // Not a finding of its own: `check_health` already reports an unreachable
        // server, and duplicating it here would send two alerts for one outage.
        _ => return Vec::new(),
    };

    let mut findings = Vec::new();
    for name in WATCHED_COUNTERS {
        let Some(current) = parse_counter(&body, name) else {
            continue;
        };
        let previous = state.counters.get(name).copied();
        state.counters.insert(name.to_string(), current);
        findings.extend(counter_finding(name, current, previous));
    }
    findings
}

/// The delta decision, split out from the HTTP fetch so it can be tested.
fn counter_finding(name: &str, current: u64, previous: Option<u64>) -> Option<Finding> {
    let previous = previous.unwrap_or(0);
    if current <= previous {
        return None;
    }
    Some(Finding {
        key: name.to_string(),
        detail: format!(
            "{name} rose by {} (to {current}) since the last check — events were \
             accepted and then lost",
            current - previous
        ),
    })
}

/// Percentage used of the filesystem holding `path`.
///
/// Shells out to `df` rather than taking a dependency on a libc wrapper for one
/// `statvfs` call. Unparseable output is not a finding — a broken check must not
/// masquerade as a full disk.
fn check_disk(path: &str, threshold_percent: u8) -> Vec<Finding> {
    let target = Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    let output = match std::process::Command::new("df")
        .arg("-P")
        .arg(target)
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        Ok(_) | Err(_) => {
            tracing::warn!(path = %target.display(), "could not read disk usage");
            return Vec::new();
        }
    };

    let text = String::from_utf8_lossy(&output);
    let Some(used) = parse_df_percent(&text) else {
        tracing::warn!("could not parse df output");
        return Vec::new();
    };

    if used >= threshold_percent {
        vec![Finding {
            key: "disk".into(),
            detail: format!(
                "filesystem holding {} is {used}% full (threshold {threshold_percent}%)",
                target.display()
            ),
        }]
    } else {
        Vec::new()
    }
}

/// Alert when the newest backup is too old — i.e. the nightly job has stopped
/// running.
///
/// A backup cron's normal failure mode is dying quietly, months before anyone
/// looks. An external watchdog catches that by noticing a missing ping; this
/// catches it from the inside, by noticing the *artifacts* stopped appearing,
/// which needs no third-party service.
///
/// A missing backup directory means backups were never set up here, and is not
/// reported: nagging every ten minutes about a deliberate choice trains people to
/// ignore the alerts that matter. Once the directory exists, backups have run at
/// least once, and stopping is a regression worth hearing about.
///
/// What this cannot replace is a watchdog's other half: if the whole host is
/// down, nothing running on it will tell you.
fn check_backup_freshness(dir: &str, max_age_hours: u32, now_ms: i64) -> Vec<Finding> {
    if max_age_hours == 0 {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let names = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok());

    let max_age_ms = (max_age_hours as i64) * 3_600_000;
    match newest_backup_stamp(names) {
        None => vec![Finding {
            key: "backup".into(),
            detail: format!("{dir} exists but contains no recognisable backup runs"),
        }],
        Some(newest) if now_ms - newest > max_age_ms => vec![Finding {
            key: "backup".into(),
            detail: format!(
                "newest backup in {dir} is {}h old (limit {max_age_hours}h) — the nightly \
                 backup has stopped running",
                (now_ms - newest) / 3_600_000
            ),
        }],
        Some(_) => Vec::new(),
    }
}

/// Newest `YYYYMMDDTHHMMSSZ` directory name, as epoch ms. Names that do not parse
/// are ignored rather than treated as ancient — an unrelated directory must not
/// raise a false "backups have stopped".
fn newest_backup_stamp(names: impl Iterator<Item = String>) -> Option<i64> {
    names
        .filter_map(|n| {
            chrono::NaiveDateTime::parse_from_str(&n, "%Y%m%dT%H%M%SZ")
                .ok()
                .map(|dt| dt.and_utc().timestamp_millis())
        })
        .max()
}

/// Tell the external watchdog this run happened, and whether it was clean.
///
/// The `/fail` suffix is a healthchecks.io convention: it turns a failing check
/// into an immediate external alert instead of waiting for the ping to time out,
/// which matters when the reason for the failure is also why email is down.
async fn ping_healthcheck(http: &reqwest::Client, config: &Config, healthy: bool) {
    let Some(base) = config.healthcheck_url.as_deref() else {
        return;
    };
    let url = if healthy {
        base.to_string()
    } else {
        format!("{}/fail", base.trim_end_matches('/'))
    };
    if let Err(err) = http.post(&url).send().await {
        tracing::warn!(error = %err, "could not reach the healthcheck watchdog");
    }
}

/// Turn a bind address into something this host can actually connect to.
/// `0.0.0.0` and `[::]` mean "every interface" to a listener but are not
/// dialable, so they become loopback.
fn local_base_url(bind_addr: &str) -> String {
    let addr = match bind_addr.rsplit_once(':') {
        Some((host, port)) => match host.trim() {
            "0.0.0.0" | "" | "*" => format!("127.0.0.1:{port}"),
            "[::]" | "::" => format!("[::1]:{port}"),
            host => format!("{host}:{port}"),
        },
        None => bind_addr.to_string(),
    };
    format!("http://{addr}")
}

/// Read a single counter out of Prometheus text format.
///
/// Only unlabelled samples are matched: the ingest counters carry no labels, and
/// summing across an unknown label set would silently change meaning if one were
/// ever added.
fn parse_counter(body: &str, name: &str) -> Option<u64> {
    body.lines()
        .filter(|l| !l.starts_with('#'))
        .find_map(|line| {
            let rest = line.strip_prefix(name)?;
            let value = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('\t'))?;
            // Counters render as floats ("3" or "3.0"); truncation is exact for
            // any count small enough to matter.
            value.trim().parse::<f64>().ok().map(|v| v as u64)
        })
}

/// Pull the "Capacity" column out of `df -P` output.
fn parse_df_percent(text: &str) -> Option<u8> {
    let line = text.lines().nth(1)?;
    line.split_whitespace()
        .find_map(|f| f.strip_suffix('%'))
        .and_then(|n| n.parse::<u8>().ok())
}

fn render_alert(findings: &[&Finding]) -> String {
    let items: String = findings
        .iter()
        .map(|f| {
            format!(
                "<li><strong>{}</strong>: {}</li>",
                html_escape(&f.key),
                html_escape(&f.detail)
            )
        })
        .collect();
    format!(
        "<p>dullahan selfcheck found {} problem{}:</p><ul>{items}</ul>\
         <p>Inspect with <code>journalctl -u dullahan -n 50</code> and \
         <code>systemctl status dullahan</code>.</p>",
        findings.len(),
        if findings.len() == 1 { "" } else { "s" }
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_base_url_makes_wildcards_dialable() {
        assert_eq!(local_base_url("0.0.0.0:3001"), "http://127.0.0.1:3001");
        assert_eq!(local_base_url("[::]:3001"), "http://[::1]:3001");
        assert_eq!(local_base_url("127.0.0.1:3021"), "http://127.0.0.1:3021");
        assert_eq!(local_base_url("192.168.1.9:80"), "http://192.168.1.9:80");
    }

    #[test]
    fn parse_counter_reads_unlabelled_samples_only() {
        let body = "\
# HELP dullahan_ingest_queue_full_total Total.
# TYPE dullahan_ingest_queue_full_total counter
dullahan_ingest_insert_failures_total 7
dullahan_ingest_queue_full_total 12
axum_http_requests_total{method=\"GET\"} 900
";
        assert_eq!(
            parse_counter(body, "dullahan_ingest_insert_failures_total"),
            Some(7)
        );
        assert_eq!(
            parse_counter(body, "dullahan_ingest_queue_full_total"),
            Some(12)
        );
        assert_eq!(
            parse_counter(body, "dullahan_ingest_queue_closed_total"),
            None
        );
        // Labelled series must not be picked up by a bare-name lookup.
        assert_eq!(parse_counter(body, "axum_http_requests_total"), None);
    }

    #[test]
    fn parse_counter_accepts_float_rendering() {
        assert_eq!(parse_counter("some_total 3.0\n", "some_total"), Some(3));
    }

    #[test]
    fn a_counters_first_sighting_still_alerts() {
        // The `metrics` crate publishes a counter only once it is incremented, so
        // the first run that sees one is already looking at a real failure. If a
        // missing baseline were treated as "unknown" this alert would be lost.
        let f = counter_finding("dullahan_ingest_queue_full_total", 4, None);
        assert!(f.is_some(), "first sighting must not be swallowed");
        assert!(f.unwrap().detail.contains("rose by 4"));
    }

    #[test]
    fn counter_findings_track_deltas_and_ignore_restarts() {
        // Steady state: no change, no alert.
        assert!(counter_finding("c", 9, Some(9)).is_none());
        // Increase: alert, reporting the delta rather than the total.
        assert!(
            counter_finding("c", 11, Some(9))
                .unwrap()
                .detail
                .contains("rose by 2")
        );
        // Decrease means the process restarted and the counter reset. Not a
        // recovery to announce — just re-baseline.
        assert!(counter_finding("c", 1, Some(500)).is_none());
    }

    #[test]
    fn newest_backup_stamp_picks_the_latest_and_ignores_strangers() {
        let names = [
            "20260801T031500Z".to_string(),
            "20260802T031500Z".to_string(),
            "20260715T031500Z".to_string(),
            "not-a-backup".to_string(),
            "tmp".to_string(),
        ];
        let newest = newest_backup_stamp(names.into_iter()).expect("a stamp");
        let expected = chrono::NaiveDateTime::parse_from_str("20260802T031500Z", "%Y%m%dT%H%M%SZ")
            .unwrap()
            .and_utc()
            .timestamp_millis();
        assert_eq!(newest, expected);
        // An unrelated directory must not read as an ancient backup, which would
        // be a false "backups have stopped".
        assert_eq!(newest_backup_stamp(["tmp".to_string()].into_iter()), None);
    }

    #[test]
    fn backup_freshness_only_complains_once_backups_exist() {
        let now = chrono::NaiveDateTime::parse_from_str("20260802T120000Z", "%Y%m%dT%H%M%SZ")
            .unwrap()
            .and_utc()
            .timestamp_millis();

        // No directory at all: backups were never set up here. Silence, so a
        // deliberate choice does not train anyone to ignore alerts.
        assert!(check_backup_freshness("/nonexistent/dullahan-backups", 48, now).is_empty());

        let dir = std::env::temp_dir().join(format!("dullahan-bf-{now}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.to_str().unwrap();

        // Exists but empty: backups have run before and produced nothing now.
        let f = check_backup_freshness(path, 48, now);
        assert_eq!(f.len(), 1);
        assert!(f[0].detail.contains("no recognisable backup runs"));

        // A run from this morning is fine.
        std::fs::create_dir_all(dir.join("20260802T031500Z")).unwrap();
        assert!(check_backup_freshness(path, 48, now).is_empty());

        // Nothing since three days ago: the nightly job has stopped.
        std::fs::remove_dir(dir.join("20260802T031500Z")).unwrap();
        std::fs::create_dir_all(dir.join("20260730T031500Z")).unwrap();
        let stale = check_backup_freshness(path, 48, now);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].key, "backup");
        assert!(stale[0].detail.contains("has stopped running"));

        // Zero disables the check outright.
        assert!(check_backup_freshness(path, 0, now).is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_df_percent_reads_the_capacity_column() {
        let df = "\
Filesystem     1024-blocks     Used Available Capacity Mounted on
/dev/sda1         39433448 18234112  19168952      49% /
";
        assert_eq!(parse_df_percent(df), Some(49));
        assert_eq!(parse_df_percent("header only\n"), None);
    }

    #[test]
    fn notify_is_throttled_until_the_repeat_window_passes() {
        let now = 1_700_000_000_000_i64;
        let hour = 3_600_000_i64;
        // Never alerted before: always notify.
        assert!(should_notify(now, None, 6));
        // Alerted an hour ago, window is six: stay quiet.
        assert!(!should_notify(now, Some(now - hour), 6));
        // Exactly at the window: notify again.
        assert!(should_notify(now, Some(now - 6 * hour), 6));
        // A zero window means every run alerts.
        assert!(should_notify(now, Some(now), 0));
    }

    #[test]
    fn recovered_checks_stop_being_throttled() {
        // Mirrors the retain() in `run`: only currently-failing keys keep a
        // timestamp, so a problem that comes back alerts at once.
        let mut last: BTreeMap<String, i64> = BTreeMap::new();
        last.insert("disk".into(), 1);
        last.insert("database".into(), 2);
        let failing = ["disk".to_string()];
        last.retain(|k, _| failing.contains(k));
        assert!(last.contains_key("disk"));
        assert!(!last.contains_key("database"));
    }

    #[test]
    fn alert_body_escapes_check_detail() {
        let f = Finding {
            key: "health".into(),
            detail: "<script>alert(1)</script> & more".into(),
        };
        let body = render_alert(&[&f]);
        assert!(!body.contains("<script>"));
        assert!(body.contains("&lt;script&gt;"));
        assert!(body.contains("&amp; more"));
        assert!(body.contains("1 problem:"));
    }
}
