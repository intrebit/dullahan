//! `POST /contact` handler coverage.
//!
//! The integration harness always runs with `mailer: None`, so these exercise
//! every branch of `contact::submit` up to the actual SMTP send: input
//! validation, the required `site`, the per-tenant recipient, and the transport
//! check. The send itself (and its failure path) needs a real `Mailer`, which is
//! a concrete struct with no test seam, so it stays out of scope here — the
//! closest a mailer-less test reaches is the "transport not configured" 503,
//! which proves validation, lookup and recipient all passed first.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use dullahan::{config::Config, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::{IpAddr, SocketAddr};
use tower::ServiceExt;

mod common;
use common::{SITE, Tenant, state_with_tenants};

/// The `/contact` rate limiter keys on the peer IP, so the request needs a
/// `ConnectInfo` extension the way a served connection would carry one.
fn post_contact(body: Value) -> Request<Body> {
    let mut req = Request::builder()
        .uri("/contact")
        .method("POST")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let ip: IpAddr = "10.0.0.1".parse().unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip, 12345)));
    req
}

async fn message(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    v.get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn valid_body(site: &str) -> Value {
    json!({
        "email": "visitor@example.com",
        "name": "Visitor",
        "message": "Hello, I would like to get in touch about a project.",
        "site": site,
    })
}

async fn configured_site(pool: PgPool) -> dullahan::AppState {
    state_with_tenants(
        pool,
        None,
        Config::default(),
        &[Tenant::new(SITE).contact_to("owner@example.com")],
    )
    .await
}

/// A fully valid submission for a tenant that has a recipient, on a server with
/// no mail transport: the furthest a mailer-less test can go, and proof the
/// validation, site lookup and recipient check all passed.
#[sqlx::test]
async fn valid_submission_stops_only_at_the_missing_transport(pool: PgPool) {
    let app = router(configured_site(pool).await);
    let resp = app.oneshot(post_contact(valid_body(SITE))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(message(resp).await, "email transport not configured");
}

#[sqlx::test]
async fn invalid_email_is_rejected(pool: PgPool) {
    let app = router(configured_site(pool).await);
    let mut body = valid_body(SITE);
    body["email"] = json!("not-an-email");
    let resp = app.oneshot(post_contact(body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn overlong_name_is_rejected(pool: PgPool) {
    let app = router(configured_site(pool).await);
    let mut body = valid_body(SITE);
    body["name"] = json!("a".repeat(81));
    let resp = app.oneshot(post_contact(body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn too_short_message_is_rejected(pool: PgPool) {
    let app = router(configured_site(pool).await);
    let mut body = valid_body(SITE);
    body["message"] = json!("short"); // 5 chars, under the floor of 10
    let resp = app.oneshot(post_contact(body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn too_long_message_is_rejected(pool: PgPool) {
    let app = router(configured_site(pool).await);
    let mut body = valid_body(SITE);
    body["message"] = json!("a".repeat(2001)); // one over the ceiling of 2000
    let resp = app.oneshot(post_contact(body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// An unregistered `site` must be refused, not delivered to some default inbox:
/// the guard that stops one tenant's enquiries landing in another's mail.
#[sqlx::test]
async fn unknown_site_is_refused(pool: PgPool) {
    let app = router(configured_site(pool).await);
    let resp = app
        .oneshot(post_contact(valid_body("ghost")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(message(resp).await, "contact recipient not configured");
}

#[sqlx::test]
async fn site_without_recipient_is_refused(pool: PgPool) {
    let state =
        state_with_tenants(pool, None, Config::default(), &[Tenant::new(SITE)]).await;
    let app = router(state);
    let resp = app.oneshot(post_contact(valid_body(SITE))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(message(resp).await, "contact recipient not configured");
}

/// A suspended tenant has a recipient but is filtered out of the lookup, so its
/// form is refused exactly like an unknown site.
#[sqlx::test]
async fn inactive_site_is_refused(pool: PgPool) {
    let state = state_with_tenants(
        pool,
        None,
        Config::default(),
        &[Tenant::new(SITE).contact_to("owner@example.com").inactive()],
    )
    .await;
    let app = router(state);
    let resp = app.oneshot(post_contact(valid_body(SITE))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(message(resp).await, "contact recipient not configured");
}
