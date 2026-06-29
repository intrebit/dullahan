use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use dullahan::{config::Config, router, router_with_metrics, state::AppState};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

fn test_state(
    pool: PgPool,
    admin_token: Option<&str>,
    allowed_sites: Option<Vec<String>>,
) -> AppState {
    state_with(pool, admin_token, allowed_sites, false)
}

fn state_with(
    pool: PgPool,
    admin_token: Option<&str>,
    allowed_sites: Option<Vec<String>>,
    sessions_enabled: bool,
) -> AppState {
    AppState {
        config: Arc::new(Config {
            bind_addr: "0.0.0.0:0".into(),
            database_url: String::new(),
            allowed_sites,
            admin_token: admin_token.map(String::from),
            email: None,
            contact_to: None,
            stats_origins: None,
            behind_tls: false,
            sessions_enabled,
        }),
        pool,
        mailer: None,
        salt_cache: dullahan::salt::new_cache(),
    }
}

fn post_collect(body: Value) -> Request<Body> {
    Request::builder()
        .uri("/collect")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        // SmartIpKeyExtractor needs an IP source; provide one explicitly.
        .header("x-forwarded-for", "10.0.0.1")
        .body(Body::from(body.to_string()))
        .unwrap()
}

const CHROME_WIN: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

fn post_collect_ua(body: Value, ip: &str, ua: &str) -> Request<Body> {
    Request::builder()
        .uri("/collect")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-forwarded-for", ip)
        .header(header::USER_AGENT, ua)
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn wait_for_count(pool: &PgPool, expected: i64) {
    for _ in 0..50 {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM analytics_events")
            .fetch_one(pool)
            .await
            .unwrap();
        if n >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {expected} rows");
}

#[sqlx::test]
async fn health_returns_ok(pool: PgPool) {
    let app = router(test_state(pool, None, None));
    let resp = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[sqlx::test]
async fn collect_inserts_pageview(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let resp = app
        .oneshot(post_collect(json!({
            "t": "pageview",
            "s": "site-1",
            "p": "/about",
            "ts": 1_700_000_000_000_i64,
            "d": "desktop",
            "v": 1280
        })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    wait_for_count(&pool, 1).await;

    let row: (String, String, String) =
        sqlx::query_as("SELECT site_id, type, path FROM analytics_events LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, ("site-1".into(), "pageview".into(), "/about".into()));
}

#[sqlx::test]
async fn collect_rejects_unknown_site_when_allowlisted(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, Some(vec!["site-a".into()])));
    let resp = app
        .oneshot(post_collect(json!({
            "t": "pageview",
            "s": "site-b",
            "p": "/",
            "ts": 1_700_000_000_000_i64
        })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM analytics_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test]
async fn collect_rejects_oversize_path(pool: PgPool) {
    let app = router(test_state(pool, None, None));
    let resp = app
        .oneshot(post_collect(json!({
            "t": "pageview",
            "s": "s",
            "p": "/".repeat(3000),
            "ts": 1_700_000_000_000_i64
        })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn collect_rejects_oversize_body(pool: PgPool) {
    let app = router(test_state(pool, None, None));
    // Stuff a giant `pr` to exceed the 16KB body limit.
    let big = "x".repeat(20_000);
    let resp = app
        .oneshot(post_collect(json!({
            "t": "event",
            "s": "s",
            "p": "/",
            "ts": 1_700_000_000_000_i64,
            "n": "big",
            "pr": { "blob": big }
        })))
        .await
        .unwrap();
    assert!(
        resp.status() == StatusCode::PAYLOAD_TOO_LARGE || resp.status() == StatusCode::BAD_REQUEST,
        "got {}",
        resp.status()
    );
}

#[sqlx::test]
async fn stats_summary_requires_admin_token(pool: PgPool) {
    let app = router(test_state(pool, Some("secret-token"), None));
    let resp = app
        .oneshot(
            Request::get("/stats/summary?site=site-1&days=30")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn stats_summary_rejects_wrong_token(pool: PgPool) {
    let app = router(test_state(pool, Some("secret-token"), None));
    let resp = app
        .oneshot(
            Request::get("/stats/summary?site=s&days=30")
                .header(header::AUTHORIZATION, "Bearer wrong")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn stats_summary_accepts_correct_token(pool: PgPool) {
    let app = router(test_state(pool, Some("secret-token"), None));
    let resp = app
        .oneshot(
            Request::get("/stats/summary?site=s&days=30")
                .header(header::AUTHORIZATION, "Bearer secret-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["pageviews"], 0);
    assert_eq!(body["events"], 0);
}

#[sqlx::test]
async fn stats_summary_counts_inserted_pageviews(pool: PgPool) {
    let state = test_state(pool.clone(), None, None);
    let app = router(state);

    let now_ms = chrono::Utc::now().timestamp_millis();
    for path in ["/a", "/a", "/b"] {
        let r = app
            .clone()
            .oneshot(post_collect(json!({
                "t": "pageview",
                "s": "site-1",
                "p": path,
                "ts": now_ms
            })))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::ACCEPTED);
    }
    wait_for_count(&pool, 3).await;

    let resp = app
        .oneshot(
            Request::get("/stats/summary?site=site-1&days=365")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["pageviews"], 3);
    assert_eq!(body["top_path"], "/a");
}

#[sqlx::test]
async fn security_headers_present(pool: PgPool) {
    let app = router(test_state(pool, None, None));
    let resp = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let h = resp.headers();
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("referrer-policy").unwrap(), "no-referrer");
    assert_eq!(h.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(
        h.get("content-security-policy").unwrap(),
        "default-src 'none'; frame-ancestors 'none'"
    );
    // HSTS is gated on BEHIND_TLS=1
    assert!(h.get("strict-transport-security").is_none());
    // Every response carries an x-request-id
    let rid = h.get("x-request-id").expect("x-request-id");
    assert!(!rid.is_empty());
}

#[sqlx::test]
async fn request_id_is_propagated_when_client_sends_one(pool: PgPool) {
    let app = router(test_state(pool, None, None));
    let resp = app
        .oneshot(
            Request::get("/health")
                .header("x-request-id", "abc-123-test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.headers().get("x-request-id").unwrap(), "abc-123-test");
}

#[sqlx::test]
async fn metrics_endpoint_returns_prometheus_text(pool: PgPool) {
    // router_with_metrics installs a process-global Prometheus recorder, so
    // this is the only test that may exercise it. Other tests use the bare
    // router() to stay parallel-safe.
    let app = router_with_metrics(test_state(pool, None, None));

    // Generate a request so at least one counter exists.
    let _ = app
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let resp = app
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.contains("axum_http_requests_total"), "body: {text}");
}

#[sqlx::test]
async fn collect_stores_utm_and_top_breaks_down(pool: PgPool) {
    let state = test_state(pool.clone(), None, None);
    let app = router(state);

    let now_ms = chrono::Utc::now().timestamp_millis();
    for src in ["newsletter", "newsletter", "twitter"] {
        let r = app
            .clone()
            .oneshot(post_collect(json!({
                "t": "pageview",
                "s": "site-1",
                "p": "/",
                "ts": now_ms,
                "u": { "s": src, "m": "email", "c": "spring" }
            })))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::ACCEPTED);
    }
    wait_for_count(&pool, 3).await;

    let resp = app
        .oneshot(
            Request::get("/stats/top?site=site-1&days=365&dim=utm_source")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body[0]["key"], "newsletter");
    assert_eq!(body[0]["count"], 2);
    assert_eq!(body[1]["key"], "twitter");
}

#[sqlx::test]
async fn stats_events_lists_names_and_breaks_down_by_prop(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));

    let now_ms = chrono::Utc::now().timestamp_millis();
    let events = [
        json!({"n": "scroll_depth", "pr": {"pct": 50}}),
        json!({"n": "scroll_depth", "pr": {"pct": 50}}),
        json!({"n": "scroll_depth", "pr": {"pct": 100}}),
        json!({"n": "outbound", "pr": {"href": "example.com"}}),
    ];
    for e in events {
        let r = app
            .clone()
            .oneshot(post_collect(json!({
                "t": "event",
                "s": "site-1",
                "p": "/",
                "ts": now_ms,
                "n": e["n"],
                "pr": e["pr"],
            })))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::ACCEPTED);
    }
    wait_for_count(&pool, 4).await;

    // No name → top event names.
    let resp = app
        .clone()
        .oneshot(
            Request::get("/stats/events?site=site-1&days=365")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body[0]["key"], "scroll_depth");
    assert_eq!(body[0]["count"], 3);

    // name + by → distribution of that event's prop value.
    let resp = app
        .clone()
        .oneshot(
            Request::get("/stats/events?site=site-1&days=365&name=scroll_depth&by=pct")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body[0]["key"], "50");
    assert_eq!(body[0]["count"], 2);
    assert_eq!(body[1]["key"], "100");
    assert_eq!(body[1]["count"], 1);

    // by without name is rejected.
    let resp = app
        .oneshot(
            Request::get("/stats/events?site=site-1&by=pct")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn collect_round_trips_view_id(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let resp = app
        .oneshot(post_collect(json!({
            "t": "pageview",
            "s": "site-1",
            "p": "/",
            "ts": 1_700_000_000_000_i64,
            "vid": "abc123view"
        })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    wait_for_count(&pool, 1).await;
    let vid: Option<String> =
        sqlx::query_scalar("SELECT view_id FROM analytics_events WHERE type = 'pageview' LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(vid.as_deref(), Some("abc123view"));
}

#[sqlx::test]
async fn collect_rejects_oversize_vid(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let resp = app
        .oneshot(post_collect(json!({
            "t": "event",
            "s": "site-1",
            "p": "/",
            "ts": 1_700_000_000_000_i64,
            "n": "x",
            "vid": "v".repeat(100)
        })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn sessions_disabled_stores_no_visitor_data(pool: PgPool) {
    // Default config: even with IP + UA present, nothing is derived.
    let app = router(test_state(pool.clone(), None, None));
    let resp = app
        .oneshot(post_collect_ua(
            json!({"t": "pageview", "s": "site-1", "p": "/", "ts": 1_700_000_000_000_i64}),
            "203.0.113.5",
            CHROME_WIN,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    wait_for_count(&pool, 1).await;
    let (vh, br, os): (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as("SELECT visitor_hash, browser, os FROM analytics_events LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(vh, None);
    assert_eq!(br, None);
    assert_eq!(os, None);
}

#[sqlx::test]
async fn sessions_enabled_records_visitor_hash_and_ua(pool: PgPool) {
    let app = router(state_with(pool.clone(), None, None, true));
    let resp = app
        .oneshot(post_collect_ua(
            json!({"t": "pageview", "s": "site-1", "p": "/", "ts": 1_700_000_000_000_i64}),
            "203.0.113.5",
            CHROME_WIN,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    wait_for_count(&pool, 1).await;
    let (vh, br, os): (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as("SELECT visitor_hash, browser, os FROM analytics_events LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(vh.as_ref().map(|s| s.len()), Some(18));
    assert_eq!(br.as_deref(), Some("Chrome"));
    assert_eq!(os.as_deref(), Some("Windows"));
}

#[sqlx::test]
async fn summary_reports_unique_visitors_and_bounce_rate_when_enabled(pool: PgPool) {
    let app = router(state_with(pool.clone(), None, None, true));
    let now_ms = chrono::Utc::now().timestamp_millis();

    // Visitor A (one IP): two pageviews → not a bounce.
    for _ in 0..2 {
        app.clone()
            .oneshot(post_collect_ua(
                json!({"t": "pageview", "s": "s", "p": "/a", "ts": now_ms}),
                "1.1.1.1",
                CHROME_WIN,
            ))
            .await
            .unwrap();
    }
    // Visitor B (different IP): one pageview → a bounce.
    app.clone()
        .oneshot(post_collect_ua(
            json!({"t": "pageview", "s": "s", "p": "/b", "ts": now_ms}),
            "2.2.2.2",
            CHROME_WIN,
        ))
        .await
        .unwrap();
    wait_for_count(&pool, 3).await;

    let resp = app
        .oneshot(
            Request::get("/stats/summary?site=s&days=365")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["uniqueVisitors"], 2);
    assert_eq!(body["bounceRate"], 0.5);
}

#[sqlx::test]
async fn summary_omits_session_metrics_when_disabled(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    app.clone()
        .oneshot(post_collect(json!({
            "t": "pageview", "s": "s", "p": "/", "ts": chrono::Utc::now().timestamp_millis()
        })))
        .await
        .unwrap();
    wait_for_count(&pool, 1).await;

    let resp = app
        .oneshot(
            Request::get("/stats/summary?site=s&days=365")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert!(body.get("uniqueVisitors").is_none(), "got {body}");
    assert!(body.get("bounceRate").is_none(), "got {body}");
}

#[sqlx::test]
async fn top_breaks_down_by_browser_when_enabled(pool: PgPool) {
    let app = router(state_with(pool.clone(), None, None, true));
    let now_ms = chrono::Utc::now().timestamp_millis();
    app.clone()
        .oneshot(post_collect_ua(
            json!({"t": "pageview", "s": "s", "p": "/", "ts": now_ms}),
            "1.1.1.1",
            CHROME_WIN,
        ))
        .await
        .unwrap();
    wait_for_count(&pool, 1).await;

    let resp = app
        .oneshot(
            Request::get("/stats/top?site=s&days=365&dim=browser")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body[0]["key"], "Chrome");
    assert_eq!(body[0]["count"], 1);
}

#[sqlx::test]
async fn salt_rotates_daily_and_keeps_48h(pool: PgPool) {
    use dullahan::salt::{current_salt, new_cache};
    let cache = new_cache();
    let d1 = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
    let d2 = chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap();
    let d3 = chrono::NaiveDate::from_ymd_opt(2026, 1, 3).unwrap();
    let d5 = chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();

    let s1 = current_salt(&pool, &cache, d1).await.unwrap();
    let s1_again = current_salt(&pool, &cache, d1).await.unwrap();
    assert_eq!(s1, s1_again, "salt is stable within a day");

    let s2 = current_salt(&pool, &cache, d2).await.unwrap();
    assert_ne!(s1, s2, "salt rotates across days");

    // 48h retention: by d3, only yesterday (d2) and today (d3) remain — d1 is gone.
    current_salt(&pool, &cache, d3).await.unwrap();
    let remaining: Vec<chrono::NaiveDate> =
        sqlx::query_scalar("SELECT day FROM daily_salts ORDER BY day")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(remaining, vec![d2, d3], "keeps only today + yesterday");

    // A multi-day jump prunes everything stale.
    current_salt(&pool, &cache, d5).await.unwrap();
    let remaining: Vec<chrono::NaiveDate> =
        sqlx::query_scalar("SELECT day FROM daily_salts ORDER BY day")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(remaining, vec![d5]);
}

#[sqlx::test]
async fn pageleave_dur_is_clamped(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let resp = app
        .oneshot(post_collect(json!({
            "t": "pageleave",
            "s": "site-1",
            "p": "/",
            "ts": 1_700_000_000_000_i64,
            "dur": 99_999_999_i32  // way over 30min cap
        })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    wait_for_count(&pool, 1).await;
    let dur: i32 =
        sqlx::query_scalar("SELECT dur_ms FROM analytics_events WHERE type = 'pageleave' LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(dur, 1_800_000);
}

#[sqlx::test]
async fn collect_rate_limits_per_ip(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let body = json!({"t": "pageview", "s": "site-1", "p": "/", "ts": 1_700_000_000_000_i64});

    // Burst is 60; firing well past it from one IP must eventually 429.
    let mut saw_429 = false;
    for _ in 0..80 {
        let resp = app
            .clone()
            .oneshot(post_collect(body.clone()))
            .await
            .unwrap();
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;
            break;
        }
    }
    assert!(saw_429, "expected a 429 once the burst is exhausted");

    // The limiter is keyed per IP: a different client is unaffected.
    let other = Request::builder()
        .uri("/collect")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-forwarded-for", "10.9.9.9")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(other).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

#[sqlx::test]
async fn collect_clamps_absurd_future_ts(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let far_future = 32_503_680_000_000_i64; // ~year 3000
    let resp = app
        .oneshot(post_collect(json!({
            "t": "pageview",
            "s": "site-1",
            "p": "/",
            "ts": far_future,
            "d": "desktop"
        })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    wait_for_count(&pool, 1).await;

    let ts: i64 = sqlx::query_scalar("SELECT ts FROM analytics_events LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    assert!(
        ts < far_future,
        "absurd future ts must be clamped, got {ts}"
    );
    assert!(
        ts <= now + 24 * 60 * 60 * 1000 + 5_000,
        "clamped ts should sit within ~1 day of now, got {ts}"
    );
}

// ---- Tier 1 metrics ----

#[sqlx::test]
async fn vitals_breakdown_by_path_has_per_metric_counts(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let now = chrono::Utc::now().timestamp_millis();
    // Two perf rows on /a: both carry lcp+cls, only one carries inp.
    for pf in [
        json!({"lcp": 2000.0, "cls": 0.05, "inp": 150.0, "ttfb": 300.0}),
        json!({"lcp": 3000.0, "cls": 0.20, "ttfb": 500.0}),
    ] {
        let r = app
            .clone()
            .oneshot(post_collect(
                json!({"t":"performance","s":"site-1","p":"/a","ts":now,"pf":pf}),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::ACCEPTED);
    }
    wait_for_count(&pool, 2).await;

    let resp = app
        .oneshot(
            Request::get("/stats/vitals?site=site-1&days=365&dim=path&limit=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let row = &body[0];
    assert_eq!(row["key"], "/a");
    assert_eq!(row["samples"], 2);
    assert_eq!(row["lcpN"], 2);
    assert_eq!(row["inpN"], 1, "only one row had inp; got {body}");
    assert!(row["lcpP75"].as_f64().unwrap() >= 2000.0);
}

#[sqlx::test]
async fn vitals_distribution_buckets_pass_rate(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let now = chrono::Utc::now().timestamp_millis();
    for pf in [json!({"lcp": 2000.0}), json!({"lcp": 5000.0})] {
        app.clone()
            .oneshot(post_collect(
                json!({"t":"performance","s":"s","p":"/","ts":now,"pf":pf}),
            ))
            .await
            .unwrap();
    }
    wait_for_count(&pool, 2).await;

    let resp = app
        .oneshot(
            Request::get("/stats/vitals?site=s&days=365")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    let lcp = &body["distribution"]["lcp"];
    assert_eq!(lcp["good"], 1, "one lcp <= 2500; got {body}");
    assert_eq!(lcp["poor"], 1, "one lcp > 4000");
    assert_eq!(lcp["total"], 2);
    assert_eq!(lcp["needsImprovement"], 0);
}

#[sqlx::test]
async fn summary_reports_time_on_page_percentiles(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let now = chrono::Utc::now().timestamp_millis();
    for dur in [1000, 2000, 3000] {
        app.clone()
            .oneshot(post_collect(
                json!({"t":"pageleave","s":"s","p":"/","ts":now,"dur":dur}),
            ))
            .await
            .unwrap();
    }
    wait_for_count(&pool, 3).await;

    let resp = app
        .oneshot(
            Request::get("/stats/summary?site=s&days=365")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["medianTimeOnPageMs"], 2000.0, "got {body}");
    assert!(body["p75TimeOnPageMs"].as_f64().unwrap() >= 2000.0);
}

#[sqlx::test]
async fn timeseries_reports_unique_visitors_when_enabled(pool: PgPool) {
    let app = router(state_with(pool.clone(), None, None, true));
    let now = chrono::Utc::now().timestamp_millis();
    app.clone()
        .oneshot(post_collect_ua(
            json!({"t":"pageview","s":"s","p":"/","ts":now}),
            "1.1.1.1",
            CHROME_WIN,
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(post_collect_ua(
            json!({"t":"pageview","s":"s","p":"/","ts":now}),
            "2.2.2.2",
            CHROME_WIN,
        ))
        .await
        .unwrap();
    wait_for_count(&pool, 2).await;

    let resp = app
        .oneshot(
            Request::get("/stats/timeseries?site=s&days=365&bucket=day")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body[0]["uniqueVisitors"], 2, "got {body}");
}

#[sqlx::test]
async fn timeseries_omits_unique_visitors_when_disabled(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let now = chrono::Utc::now().timestamp_millis();
    app.clone()
        .oneshot(post_collect(
            json!({"t":"pageview","s":"s","p":"/","ts":now}),
        ))
        .await
        .unwrap();
    wait_for_count(&pool, 1).await;

    let resp = app
        .oneshot(
            Request::get("/stats/timeseries?site=s&days=365")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert!(
        body[0].get("uniqueVisitors").is_none(),
        "sessions off => omitted; got {body}"
    );
}

#[sqlx::test]
async fn heatmap_buckets_and_rejects_bad_tz(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let now = chrono::Utc::now().timestamp_millis();
    app.clone()
        .oneshot(post_collect(
            json!({"t":"pageview","s":"s","p":"/","ts":now}),
        ))
        .await
        .unwrap();
    wait_for_count(&pool, 1).await;

    let resp = app
        .clone()
        .oneshot(
            Request::get("/stats/heatmap?site=s&days=365&tz=UTC")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
    let wd = body[0]["weekday"].as_i64().unwrap();
    assert!((1..=7).contains(&wd), "isodow 1-7; got {body}");
    assert_eq!(body[0]["pageviews"], 1);

    // Well-formed but non-existent tz -> Postgres rejects -> 400.
    let resp = app
        .oneshot(
            Request::get("/stats/heatmap?site=s&days=365&tz=Not/AZone")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn channels_classifies_pageviews(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let now = chrono::Utc::now().timestamp_millis();
    app.clone()
        .oneshot(post_collect(
            json!({"t":"pageview","s":"s","p":"/","ts":now,"r":"www.google.com"}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(post_collect(
            json!({"t":"pageview","s":"s","p":"/","ts":now}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(post_collect(
            json!({"t":"pageview","s":"s","p":"/","ts":now,"u":{"m":"cpc","s":"ads"}}),
        ))
        .await
        .unwrap();
    wait_for_count(&pool, 3).await;

    let resp = app
        .oneshot(
            Request::get("/stats/channels?site=s&days=365")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    let mut map = std::collections::HashMap::new();
    for row in body.as_array().unwrap() {
        map.insert(
            row["key"].as_str().unwrap().to_string(),
            row["count"].as_i64().unwrap(),
        );
    }
    assert_eq!(map.get("Organic Search"), Some(&1), "got {body}");
    assert_eq!(map.get("Direct"), Some(&1));
    assert_eq!(map.get("Paid"), Some(&1));
}

#[sqlx::test]
async fn top_breaks_down_by_viewport(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let now = chrono::Utc::now().timestamp_millis();
    for v in [1280, 1280, 390] {
        app.clone()
            .oneshot(post_collect(
                json!({"t":"pageview","s":"s","p":"/","ts":now,"v":v}),
            ))
            .await
            .unwrap();
    }
    wait_for_count(&pool, 3).await;

    let resp = app
        .oneshot(
            Request::get("/stats/top?site=s&days=365&dim=viewport")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    // Numeric column must come back as a text key, not the "(none)" fallback.
    assert_eq!(body[0]["key"], "1280", "got {body}");
    assert_eq!(body[0]["count"], 2);
}

#[sqlx::test]
async fn summary_compare_prev_returns_change(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let now = chrono::Utc::now().timestamp_millis();
    let day = 24 * 60 * 60 * 1000_i64;
    // current window [now-1d, now]: 2 pageviews
    for _ in 0..2 {
        app.clone()
            .oneshot(post_collect(
                json!({"t":"pageview","s":"s","p":"/","ts":now}),
            ))
            .await
            .unwrap();
    }
    // previous window [now-2d, now-1d]: 1 pageview at now-1.5d
    app.clone()
        .oneshot(post_collect(
            json!({"t":"pageview","s":"s","p":"/","ts": now - day - day / 2}),
        ))
        .await
        .unwrap();
    wait_for_count(&pool, 3).await;

    let resp = app
        .oneshot(
            Request::get("/stats/summary?site=s&days=1&compare=prev")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["pageviews"], 2, "got {body}");
    assert_eq!(body["previous"]["pageviews"], 1);
    assert_eq!(body["change"]["pageviews"], 100.0);
}

// ---- Tier 2 metrics ----

#[sqlx::test]
async fn realtime_counts_distinct_active_views(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let now = chrono::Utc::now().timestamp_millis();
    // view v1 on /a: a pageview + a scroll event (one visit, two rows).
    app.clone()
        .oneshot(post_collect(
            json!({"t":"pageview","s":"s","p":"/a","ts":now,"vid":"v1"}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(post_collect(
            json!({"t":"event","s":"s","p":"/a","ts":now,"n":"scroll_depth","pr":{"pct":50},"vid":"v1"}),
        ))
        .await
        .unwrap();
    // view v2 on /b.
    app.clone()
        .oneshot(post_collect(
            json!({"t":"pageview","s":"s","p":"/b","ts":now,"vid":"v2"}),
        ))
        .await
        .unwrap();
    wait_for_count(&pool, 3).await;

    let resp = app
        .oneshot(
            Request::get("/stats/realtime?site=s")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["active"], 2, "two distinct view_ids; got {body}");
    assert_eq!(body["windowMinutes"], 5);
    let pages = body["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 2, "got {body}");
    // both paths have 1 distinct view; tie broken by key asc.
    assert_eq!(body["pages"][0]["key"], "/a");
    assert_eq!(body["pages"][0]["count"], 1);
}

#[sqlx::test]
async fn realtime_excludes_rows_outside_window(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let now = chrono::Utc::now().timestamp_millis();
    // Fresh row via /collect (received_at defaults to now()).
    app.clone()
        .oneshot(post_collect(
            json!({"t":"pageview","s":"s","p":"/","ts":now,"vid":"fresh"}),
        ))
        .await
        .unwrap();
    wait_for_count(&pool, 1).await;
    // Old row: received_at can't be set through /collect, so insert it directly.
    sqlx::query(
        "INSERT INTO analytics_events (site_id, type, path, ts, view_id, received_at)
         VALUES ($1, 'pageview', '/old', $2, 'old', now() - interval '10 minutes')",
    )
    .bind("s")
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();

    let resp = app
        .oneshot(
            Request::get("/stats/realtime?site=s&minutes=5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(
        body["active"], 1,
        "only the fresh view is within 5 min; got {body}"
    );
}

#[sqlx::test]
async fn engagement_reports_rates(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let now = chrono::Utc::now().timestamp_millis();
    // Visit v1 on /a: scrolled 75, an outbound click, 15s dwell -> engaged.
    for ev in [
        json!({"t":"pageview","s":"s","p":"/a","ts":now,"vid":"v1"}),
        json!({"t":"event","s":"s","p":"/a","ts":now,"n":"scroll_depth","pr":{"pct":75},"vid":"v1"}),
        json!({"t":"event","s":"s","p":"/a","ts":now,"n":"outbound","pr":{"href":"example.com"},"vid":"v1"}),
        json!({"t":"pageleave","s":"s","p":"/a","ts":now,"dur":15000,"vid":"v1"}),
        // Visit v2 on /a: pageview only -> not engaged.
        json!({"t":"pageview","s":"s","p":"/a","ts":now,"vid":"v2"}),
    ] {
        app.clone().oneshot(post_collect(ev)).await.unwrap();
    }
    wait_for_count(&pool, 5).await;

    let resp = app
        .oneshot(
            Request::get("/stats/engagement?site=s&days=365")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["visits"], 2, "got {body}");
    assert_eq!(body["engagedVisitRate"], 0.5, "got {body}");
    assert_eq!(body["scrollReach75"], 0.5);
    assert_eq!(body["outboundRate"], 0.5);
    // scroll_depth + outbound are auto-instrumentation -> excluded from events/visit.
    assert_eq!(body["avgEventsPerVisit"], 0.0, "got {body}");
    assert_eq!(body["scrollFunnel"]["25"], 0.5);
    assert_eq!(body["scrollFunnel"]["75"], 0.5);
    assert_eq!(body["scrollFunnel"]["100"], 0.0);
}

#[sqlx::test]
async fn engagement_omits_untracked_scroll_and_outbound(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let now = chrono::Utc::now().timestamp_millis();
    // Plain visit: pageview + a 20s dwell, no scroll/outbound rows at all.
    app.clone()
        .oneshot(post_collect(
            json!({"t":"pageview","s":"s","p":"/a","ts":now,"vid":"v1"}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(post_collect(
            json!({"t":"pageleave","s":"s","p":"/a","ts":now,"dur":20000,"vid":"v1"}),
        ))
        .await
        .unwrap();
    wait_for_count(&pool, 2).await;

    let resp = app
        .oneshot(
            Request::get("/stats/engagement?site=s&days=365")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["visits"], 1, "got {body}");
    assert_eq!(
        body["engagedVisitRate"], 1.0,
        "engaged via 20s dwell; got {body}"
    );
    assert!(
        body.get("scrollReach75").is_none(),
        "scroll not tracked => omitted; got {body}"
    );
    assert!(
        body.get("outboundRate").is_none(),
        "outbound not tracked => omitted; got {body}"
    );
    assert!(body.get("scrollFunnel").is_none(), "got {body}");
}

#[sqlx::test]
async fn engagement_by_path_gates_scroll_site_wide(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let now = chrono::Utc::now().timestamp_millis();
    // /a: an engaged visit that scrolled to 100%.
    app.clone()
        .oneshot(post_collect(
            json!({"t":"pageview","s":"s","p":"/a","ts":now,"vid":"a1"}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(post_collect(
            json!({"t":"event","s":"s","p":"/a","ts":now,"n":"scroll_depth","pr":{"pct":100},"vid":"a1"}),
        ))
        .await
        .unwrap();
    // /b: a pageview-only visit (no scroll), but the site DOES track scroll.
    app.clone()
        .oneshot(post_collect(
            json!({"t":"pageview","s":"s","p":"/b","ts":now,"vid":"b1"}),
        ))
        .await
        .unwrap();
    wait_for_count(&pool, 3).await;

    let resp = app
        .oneshot(
            Request::get("/stats/engagement?site=s&days=365&dim=path")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 2, "got {body}");
    // Tie on visits (1 each) -> key asc -> /a then /b.
    assert_eq!(body[0]["key"], "/a");
    assert_eq!(body[0]["engagedVisitRate"], 1.0);
    assert_eq!(
        body[0]["scrollReach75"], 1.0,
        "reached 100 >= 75; got {body}"
    );
    assert_eq!(body[1]["key"], "/b");
    assert_eq!(body[1]["engagedVisitRate"], 0.0);
    // /b had no scroll, but the site tracks scroll -> 0.0, NOT omitted.
    assert_eq!(body[1]["scrollReach75"], 0.0, "got {body}");
    // No outbound anywhere on the site -> omitted on every row.
    assert!(body[0].get("outboundRate").is_none(), "got {body}");
}

#[sqlx::test]
async fn engagement_events_per_visit_excludes_auto_instrumentation(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let now = chrono::Utc::now().timestamp_millis();
    // One visit: a custom `track` event plus auto scroll + outbound rows.
    for ev in [
        json!({"t":"pageview","s":"s","p":"/a","ts":now,"vid":"v1"}),
        json!({"t":"event","s":"s","p":"/a","ts":now,"n":"signup","vid":"v1"}),
        json!({"t":"event","s":"s","p":"/a","ts":now,"n":"scroll_depth","pr":{"pct":50},"vid":"v1"}),
        json!({"t":"event","s":"s","p":"/a","ts":now,"n":"outbound","pr":{"href":"x.com"},"vid":"v1"}),
    ] {
        app.clone().oneshot(post_collect(ev)).await.unwrap();
    }
    wait_for_count(&pool, 4).await;

    let resp = app
        .oneshot(
            Request::get("/stats/engagement?site=s&days=365")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["visits"], 1, "got {body}");
    // Only "signup" counts; scroll_depth + outbound are excluded.
    assert_eq!(body["avgEventsPerVisit"], 1.0, "got {body}");
}

// ---- Tier 3 metrics (sessions) ----

#[sqlx::test]
async fn sessions_basic_counts_and_bounce(pool: PgPool) {
    let app = router(state_with(pool.clone(), None, None, true));
    let now = chrono::Utc::now().timestamp_millis();
    // Visitor A (one IP): 3 pageviews close in time -> one 3-page session.
    for p in ["/a", "/b", "/c"] {
        app.clone()
            .oneshot(post_collect_ua(
                json!({"t":"pageview","s":"s","p":p,"ts":now}),
                "1.1.1.1",
                CHROME_WIN,
            ))
            .await
            .unwrap();
    }
    // Visitor B (different IP): a single pageview -> a bounce.
    app.clone()
        .oneshot(post_collect_ua(
            json!({"t":"pageview","s":"s","p":"/a","ts":now}),
            "2.2.2.2",
            CHROME_WIN,
        ))
        .await
        .unwrap();
    wait_for_count(&pool, 4).await;

    let resp = app
        .oneshot(
            Request::get("/stats/sessions?site=s&days=365")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["sessions"], 2, "got {body}");
    assert_eq!(body["avgPagesPerSession"], 2.0, "(3 + 1) / 2; got {body}");
    assert_eq!(
        body["bounceRate"], 0.5,
        "one of two sessions is single-page; got {body}"
    );
}

#[sqlx::test]
async fn sessions_split_on_30min_gap(pool: PgPool) {
    let app = router(state_with(pool.clone(), None, None, true));
    let now = chrono::Utc::now().timestamp_millis();
    let forty_min = 40 * 60 * 1000;
    // Same visitor (hash uses today's salt regardless of ts), two pageviews 40
    // min apart -> the gap exceeds 30 min -> two sessions.
    for ts in [now - forty_min, now] {
        app.clone()
            .oneshot(post_collect_ua(
                json!({"t":"pageview","s":"s","p":"/a","ts":ts}),
                "1.1.1.1",
                CHROME_WIN,
            ))
            .await
            .unwrap();
    }
    wait_for_count(&pool, 2).await;

    let resp = app
        .oneshot(
            Request::get("/stats/sessions?site=s&days=365")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(
        body["sessions"], 2,
        "40min gap splits the visit; got {body}"
    );
    assert_eq!(
        body["bounceRate"], 1.0,
        "both sessions are single-page; got {body}"
    );
}

#[sqlx::test]
async fn sessions_report_entry_and_exit_pages(pool: PgPool) {
    let app = router(state_with(pool.clone(), None, None, true));
    let now = chrono::Utc::now().timestamp_millis();
    let min = 60 * 1000;
    // One visitor, three pages in order inside the gap window.
    for (p, ts) in [("/", now - 2 * min), ("/b", now - min), ("/c", now)] {
        app.clone()
            .oneshot(post_collect_ua(
                json!({"t":"pageview","s":"s","p":p,"ts":ts}),
                "1.1.1.1",
                CHROME_WIN,
            ))
            .await
            .unwrap();
    }
    wait_for_count(&pool, 3).await;

    let entry = body_json(
        app.clone()
            .oneshot(
                Request::get("/stats/sessions?site=s&days=365&dim=entry")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(entry[0]["key"], "/", "entry is the first page; got {entry}");

    let exit = body_json(
        app.oneshot(
            Request::get("/stats/sessions?site=s&days=365&dim=exit")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(exit[0]["key"], "/c", "exit is the last page; got {exit}");
}

#[sqlx::test]
async fn sessions_omitted_when_disabled(pool: PgPool) {
    // Sessions off (default): no visitor_hash is derived, so no sessions exist.
    let app = router(test_state(pool.clone(), None, None));
    app.clone()
        .oneshot(post_collect(json!({
            "t":"pageview","s":"s","p":"/","ts":chrono::Utc::now().timestamp_millis()
        })))
        .await
        .unwrap();
    wait_for_count(&pool, 1).await;

    let resp = app
        .oneshot(
            Request::get("/stats/sessions?site=s&days=365")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["sessions"], 0, "got {body}");
    assert!(body.get("avgPagesPerSession").is_none(), "got {body}");
    assert!(body.get("bounceRate").is_none(), "got {body}");
}

// ---- Tier 3b metrics (funnels) ----

#[sqlx::test]
async fn funnel_counts_ordered_conversions(pool: PgPool) {
    let app = router(state_with(pool.clone(), None, None, true));
    let now = chrono::Utc::now().timestamp_millis();
    let min = 60 * 1000;
    // 4 visitors (distinct IPs) enter /a; 2 continue to /b; 1 reaches /c.
    for (p, t) in [("/a", now - 2 * min), ("/b", now - min), ("/c", now)] {
        app.clone()
            .oneshot(post_collect_ua(
                json!({"t":"pageview","s":"s","p":p,"ts":t}),
                "1.1.1.1",
                CHROME_WIN,
            ))
            .await
            .unwrap();
    }
    for (p, t) in [("/a", now - min), ("/b", now)] {
        app.clone()
            .oneshot(post_collect_ua(
                json!({"t":"pageview","s":"s","p":p,"ts":t}),
                "2.2.2.2",
                CHROME_WIN,
            ))
            .await
            .unwrap();
    }
    for ip in ["3.3.3.3", "4.4.4.4"] {
        app.clone()
            .oneshot(post_collect_ua(
                json!({"t":"pageview","s":"s","p":"/a","ts":now}),
                ip,
                CHROME_WIN,
            ))
            .await
            .unwrap();
    }
    wait_for_count(&pool, 7).await;

    let resp = app
        .oneshot(
            Request::get("/stats/funnel?site=s&days=365&steps=/a,/b,/c")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["steps"].as_array().unwrap().len(), 3, "got {body}");
    assert_eq!(body["steps"][0]["key"], "/a");
    assert_eq!(body["steps"][0]["sessions"], 4, "got {body}");
    assert_eq!(body["steps"][1]["sessions"], 2);
    assert_eq!(body["steps"][2]["sessions"], 1);
    assert_eq!(body["steps"][1]["conversionFromStart"], 0.5);
    assert_eq!(body["steps"][2]["conversionFromStart"], 0.25);
    assert_eq!(body["steps"][1]["conversionFromPrev"], 0.5);
    assert_eq!(body["steps"][2]["conversionFromPrev"], 0.5);
}

#[sqlx::test]
async fn funnel_respects_step_order(pool: PgPool) {
    let app = router(state_with(pool.clone(), None, None, true));
    let now = chrono::Utc::now().timestamp_millis();
    let min = 60 * 1000;
    // One visitor visits /b BEFORE /a. For funnel /a -> /b, only step 1 (/a) is
    // reached; the earlier /b does not count toward step 2.
    for (p, t) in [("/b", now - min), ("/a", now)] {
        app.clone()
            .oneshot(post_collect_ua(
                json!({"t":"pageview","s":"s","p":p,"ts":t}),
                "1.1.1.1",
                CHROME_WIN,
            ))
            .await
            .unwrap();
    }
    wait_for_count(&pool, 2).await;

    let resp = app
        .oneshot(
            Request::get("/stats/funnel?site=s&days=365&steps=/a,/b")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["steps"][0]["sessions"], 1, "reached /a; got {body}");
    assert_eq!(
        body["steps"][1]["sessions"], 0,
        "/b came before /a, so step 2 is not reached; got {body}"
    );
}

#[sqlx::test]
async fn funnel_rejects_too_few_steps(pool: PgPool) {
    let app = router(state_with(pool.clone(), None, None, true));
    let resp = app
        .oneshot(
            Request::get("/stats/funnel?site=s&days=365&steps=/a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn collect_coerces_unknown_device_to_null(pool: PgPool) {
    // A bad device value used to fail the DB CHECK and silently drop the whole
    // pageview (fire-and-forget insert); it must now be coerced to NULL.
    let app = router(test_state(pool.clone(), None, None));
    let resp = app
        .oneshot(post_collect(json!({
            "t": "pageview",
            "s": "site-1",
            "p": "/",
            "ts": 1_700_000_000_000_i64,
            "d": "smart-fridge"
        })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    wait_for_count(&pool, 1).await;

    let device: Option<String> = sqlx::query_scalar("SELECT device FROM analytics_events LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(device, None);
}

#[sqlx::test]
async fn collect_coerces_empty_vid_to_null(pool: PgPool) {
    let app = router(test_state(pool.clone(), None, None));
    let resp = app
        .oneshot(post_collect(json!({
            "t": "event",
            "s": "site-1",
            "p": "/",
            "ts": 1_700_000_000_000_i64,
            "n": "click",
            "vid": ""
        })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    wait_for_count(&pool, 1).await;

    let vid: Option<String> = sqlx::query_scalar("SELECT view_id FROM analytics_events LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(vid, None);
}

#[sqlx::test]
async fn collect_rejects_oversize_event_prop_value(pool: PgPool) {
    let app = router(test_state(pool, None, None));
    let big = "x".repeat(2000);
    let resp = app
        .oneshot(post_collect(json!({
            "t": "event",
            "s": "site-1",
            "p": "/",
            "ts": 1_700_000_000_000_i64,
            "n": "click",
            "pr": { "blob": big }
        })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn collect_rejects_too_many_event_props(pool: PgPool) {
    let app = router(test_state(pool, None, None));
    let mut props = serde_json::Map::new();
    for i in 0..50 {
        props.insert(format!("k{i}"), json!(1));
    }
    let resp = app
        .oneshot(post_collect(json!({
            "t": "event",
            "s": "site-1",
            "p": "/",
            "ts": 1_700_000_000_000_i64,
            "n": "click",
            "pr": Value::Object(props)
        })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn funnel_rejects_duplicate_steps(pool: PgPool) {
    // /a,/b,/a — array_position() collapses the repeat to step 1, so the third
    // step is unreachable; reject rather than report a false 0% conversion.
    let app = router(test_state(pool, None, None));
    let resp = app
        .oneshot(
            Request::get("/stats/funnel?site=s&days=365&steps=/a,/b,/a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn stats_cors_accepts_wildcard_origin(pool: PgPool) {
    // STATS_ORIGINS="*" must not panic the router. tower-http rejects a wildcard
    // inside allow_origin(<list>); a literal "*" means "any origin".
    let state = AppState {
        config: Arc::new(Config {
            bind_addr: "0.0.0.0:0".into(),
            database_url: String::new(),
            allowed_sites: None,
            admin_token: None,
            email: None,
            contact_to: None,
            stats_origins: Some(vec!["*".into()]),
            behind_tls: false,
            sessions_enabled: false,
        }),
        pool,
        mailer: None,
        salt_cache: dullahan::salt::new_cache(),
    };
    let app = router(state);
    let resp = app
        .oneshot(
            Request::get("/stats/summary?site=s&days=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
