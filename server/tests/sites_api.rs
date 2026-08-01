//! `/sites` registry admin surface, and the scope boundaries around it.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use dullahan::router;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

mod common;
use common::{OTHER, SITE, state_no_tenants, state_two_tenants};

const OP: &str = "op";

fn request(method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

/// `/collect` is rate-limited, and the governor's key extractor needs a peer
/// address — without one it fails to extract a key and the request 500s before
/// it ever reaches the tenant check.
fn with_peer(mut req: Request<Body>) -> Request<Body> {
    let addr: std::net::SocketAddr = "203.0.113.7:5000".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// The privilege-escalation test. The whole model collapses if a tenant can mint
/// tenants or read another's metadata.
#[sqlx::test]
async fn site_token_cannot_reach_the_registry(pool: PgPool) {
    let app = router(state_two_tenants(pool, OP).await);

    for (method, uri, body) in [
        ("GET", "/sites", None),
        ("POST", "/sites", Some(json!({"id": "evil"}))),
        ("GET", "/sites/b", None),
        ("POST", "/sites/b/token", None),
        ("DELETE", "/sites/b", None),
    ] {
        let resp = app
            .clone()
            .oneshot(request(method, uri, Some("token-a"), body))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "{method} {uri} must be operator-only"
        );
    }
}

#[sqlx::test]
async fn anonymous_cannot_reach_the_registry(pool: PgPool) {
    let app = router(state_two_tenants(pool, OP).await);
    let resp = app
        .clone()
        .oneshot(request("GET", "/sites", None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Open mode is not operator: a deploy with no ADMIN_TOKEN must not be able to
/// create tenants, which is what keeps open mode and multi-tenancy from ever
/// coexisting at runtime.
#[sqlx::test]
async fn open_mode_cannot_reach_the_registry(pool: PgPool) {
    let app = router(state_no_tenants(pool, None).await);
    let resp = app
        .clone()
        .oneshot(request("POST", "/sites", None, Some(json!({"id": "x"}))))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn create_returns_the_token_exactly_once(pool: PgPool) {
    let app = router(state_no_tenants(pool, Some(OP)).await);

    let resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/sites",
            Some(OP),
            Some(json!({"id": "acme", "name": "Acme Ltd"})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp).await;
    let token = created["token"]
        .as_str()
        .expect("token on create")
        .to_string();
    assert!(token.starts_with("dh_s_"));

    // ...and never again.
    let resp = app
        .clone()
        .oneshot(request("GET", "/sites/acme", Some(OP), None))
        .await
        .unwrap();
    let view = body_json(resp).await;
    assert!(view.get("token").is_none(), "GET must not return the token");
    assert_eq!(view["token_last4"], token[token.len() - 4..]);

    let resp = app
        .clone()
        .oneshot(request("GET", "/sites", Some(OP), None))
        .await
        .unwrap();
    let list = body_json(resp).await;
    assert!(
        list["sites"][0].get("token").is_none(),
        "list must not return tokens"
    );
}

/// The "without a service restart" requirement: the write path refreshes the
/// registry synchronously, so the old token is dead before this test's next
/// request — no restart, no waiting for the 60s timer.
///
/// Asserted against a *write*, deliberately. A revoked token downgrades the
/// caller to `Anonymous`, and anonymous callers can still read published
/// content — so `GET /posts` keeps returning 200 with a dead token and proves
/// nothing. What revocation actually takes away is everything the token gated.
#[sqlx::test]
async fn rotation_invalidates_the_old_token_immediately(pool: PgPool) {
    let app = router(state_two_tenants(pool, OP).await);

    let post = |token: &str, slug: &str| {
        request(
            "POST",
            "/posts?site=t",
            Some(token),
            Some(json!({"slug": slug, "title": "x", "body_markdown": "x"})),
        )
    };

    // token-a can write to start with.
    let resp = app
        .clone()
        .oneshot(post("token-a", "before"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = app
        .clone()
        .oneshot(request("POST", "/sites/t/token", Some(OP), None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let new_token = body_json(resp).await["token"].as_str().unwrap().to_string();

    let resp = app.clone().oneshot(post("token-a", "after")).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "the rotated-away token must stop working at once"
    );

    // Reads stay public, though — revocation is not a lockout.
    let resp = app
        .clone()
        .oneshot(request("GET", "/posts?site=t", Some("token-a"), None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app.clone().oneshot(post(&new_token, "new")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[sqlx::test]
async fn suspending_a_site_blocks_its_token_and_its_ingest(pool: PgPool) {
    let app = router(state_two_tenants(pool, OP).await);

    let resp = app
        .clone()
        .oneshot(request(
            "PATCH",
            "/sites/t",
            Some(OP),
            Some(json!({"active": false})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 403, not 401: the token stops resolving (so the caller is anonymous), but
    // the request dies earlier than that — admission refuses the suspended site
    // outright, for every caller including the operator. Suspension takes the
    // whole tenant offline rather than just revoking its credential.
    let resp = app
        .clone()
        .oneshot(request("GET", "/posts?site=t", Some("token-a"), None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let resp = app
        .clone()
        .oneshot(request("GET", "/posts?site=t", Some(OP), None))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "a suspended tenant is offline even to the operator"
    );

    // And its ingest is refused.
    let resp = app
        .clone()
        .oneshot(with_peer(request(
            "POST",
            "/collect",
            None,
            Some(json!({"t": "pageview", "s": "t", "p": "/", "ts": 1_700_000_000_000i64})),
        )))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // The other tenant is unaffected.
    let resp = app
        .clone()
        .oneshot(request("GET", "/posts?site=b", Some("token-b"), None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[sqlx::test]
async fn delete_refuses_while_the_tenant_still_owns_content(pool: PgPool) {
    let app = router(state_two_tenants(pool, OP).await);

    let resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/posts?site=t",
            Some(OP),
            Some(json!({"slug": "keep", "title": "keep", "body_markdown": "x"})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = app
        .clone()
        .oneshot(request("DELETE", "/sites/t", Some(OP), None))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "offboarding must be an explicit purge, not a silent cascade"
    );

    // A tenant with no content deletes cleanly.
    let resp = app
        .clone()
        .oneshot(request("DELETE", "/sites/b", Some(OP), None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[sqlx::test]
async fn duplicate_and_malformed_ids_are_rejected(pool: PgPool) {
    let app = router(state_two_tenants(pool, OP).await);

    let resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/sites",
            Some(OP),
            Some(json!({"id": SITE})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    for bad in ["", "Acme", "under_score", "has space"] {
        let resp = app
            .clone()
            .oneshot(request(
                "POST",
                "/sites",
                Some(OP),
                Some(json!({"id": bad})),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "id {bad:?}");
    }
}

/// A header-injectable address must never reach the registry.
#[sqlx::test]
async fn addresses_that_could_inject_headers_are_rejected(pool: PgPool) {
    let app = router(state_no_tenants(pool, Some(OP)).await);
    let resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/sites",
            Some(OP),
            Some(json!({"id": "acme", "email_from": "ok@x.com\r\nBcc: leak@evil.com"})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn generated_tokens_are_distinct(pool: PgPool) {
    let app = router(state_no_tenants(pool, Some(OP)).await);
    let mut tokens = std::collections::HashSet::new();
    for i in 0..20 {
        let resp = app
            .clone()
            .oneshot(request(
                "POST",
                "/sites",
                Some(OP),
                Some(json!({"id": format!("site-{i}")})),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        tokens.insert(body_json(resp).await["token"].as_str().unwrap().to_string());
    }
    assert_eq!(tokens.len(), 20, "a constant-seeded RNG would be silent");
}

/// The operator reaches every tenant; a per-site token reaches exactly one.
#[sqlx::test]
async fn operator_reaches_every_tenant(pool: PgPool) {
    let app = router(state_two_tenants(pool, OP).await);
    for site in [SITE, OTHER] {
        let resp = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/posts?site={site}"),
                Some(OP),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "operator must reach {site}");
    }
}

#[sqlx::test]
async fn an_unrecognized_token_is_anonymous_not_operator(pool: PgPool) {
    let app = router(state_two_tenants(pool, OP).await);
    let resp = app
        .clone()
        .oneshot(request("GET", "/sites", Some("nonsense"), None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
