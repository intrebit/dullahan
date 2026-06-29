use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use dullahan::{config::Config, router, state::AppState};
use http_body_util::BodyExt;
use sqlx::PgPool;
use std::sync::Arc;
use tower::ServiceExt;

fn test_state(pool: PgPool) -> AppState {
    AppState {
        config: Arc::new(Config {
            bind_addr: "0.0.0.0:0".into(),
            database_url: String::new(),
            allowed_sites: None,
            admin_token: None,
            email: None,
            contact_to: None,
            stats_origins: None,
            behind_tls: false,
            sessions_enabled: false,
        }),
        pool,
        mailer: None,
        salt_cache: dullahan::salt::new_cache(),
    }
}

#[sqlx::test]
async fn serves_tracking_script(pool: PgPool) {
    let app = router(test_state(pool));
    let resp = app
        .oneshot(Request::get("/pt.js").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/javascript; charset=utf-8"
    );
    let cache = resp
        .headers()
        .get(header::CACHE_CONTROL)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        cache.contains("max-age="),
        "expected a cache header, got {cache}"
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(!body.is_empty(), "script body should not be empty");
}

#[sqlx::test]
async fn script_route_rejects_post(pool: PgPool) {
    let app = router(test_state(pool));
    let resp = app
        .oneshot(Request::post("/pt.js").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}
