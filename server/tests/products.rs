use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use dullahan::{config::Config, router, state::AppState};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;
use tower::ServiceExt;

fn state_cur(pool: PgPool, admin_token: Option<&str>, currency: &str) -> AppState {
    AppState {
        config: Arc::new(Config {
            bind_addr: "0.0.0.0:0".into(),
            database_url: String::new(),
            allowed_sites: None,
            admin_token: admin_token.map(String::from),
            email: None,
            contact_to: None,
            contact_to_sites: Default::default(),
            stats_origins: None,
            behind_tls: false,
            trust_proxy_headers: false,
            sessions_enabled: false,
            shop_currency: currency.into(),
        }),
        pool,
        mailer: None,
        salt_cache: dullahan::salt::new_cache(),
    }
}

fn state(pool: PgPool, admin_token: Option<&str>) -> AppState {
    state_cur(pool, admin_token, "EUR")
}

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

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn create_product(app: &Router, token: &str, body: Value) -> Value {
    let resp = app
        .clone()
        .oneshot(request("POST", "/products", Some(token), Some(body)))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create should succeed");
    body_json(resp).await
}

#[sqlx::test]
async fn create_then_get_round_trip(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    let created = create_product(
        &app,
        "t",
        json!({
            "slug":"blue-widget","title":"Blue Widget","description":"A widget, in blue.",
            "image":"/img/widget.png","price_cents":1299
        }),
    )
    .await;
    assert_eq!(created["slug"], "blue-widget");
    assert_eq!(created["title"], "Blue Widget");
    assert_eq!(created["description"], "A widget, in blue.");
    assert_eq!(created["image"], "/img/widget.png");
    assert_eq!(created["price_cents"], 1299);
    assert_eq!(created["currency"], "EUR", "currency echoed from config");
    assert_eq!(created["available"], true, "defaults to available");
    assert_eq!(created["position"], 0);
    assert_eq!(created["draft"], false);
    assert_eq!(created["views"], 0, "views start at 0");
    assert!(created["updated_date"].is_null());
    assert!(
        created["id"].as_str().unwrap().len() >= 32,
        "uuid id; got {created}"
    );

    let fetched = body_json(
        app.oneshot(request("GET", "/products/blue-widget", None, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(fetched["id"], created["id"]);
    assert_eq!(fetched["price_cents"], 1299);
    assert_eq!(fetched["currency"], "EUR");
}

#[sqlx::test]
async fn create_applies_defaults(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    let created = create_product(&app, "t", json!({"slug":"min","title":"Min"})).await;
    assert_eq!(created["description"], "");
    assert_eq!(created["price_cents"], 0, "no price defaults to 0");
    assert_eq!(created["available"], true);
    assert_eq!(created["position"], 0);
    assert_eq!(created["draft"], false);
    assert!(created["image"].is_null());
}

#[sqlx::test]
async fn currency_comes_from_config(pool: PgPool) {
    let app = router(state_cur(pool, Some("t"), "USD"));
    let created =
        create_product(&app, "t", json!({"slug":"p","title":"P","price_cents":500})).await;
    assert_eq!(created["currency"], "USD", "SHOP_CURRENCY plumbs through");
}

#[sqlx::test]
async fn published_list_hides_drafts(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    create_product(&app, "t", json!({"slug":"live","title":"Live"})).await;
    create_product(
        &app,
        "t",
        json!({"slug":"hidden","title":"Hidden","draft":true}),
    )
    .await;

    let body = body_json(
        app.oneshot(request("GET", "/products", None, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["total"], 1, "got {body}");
    assert_eq!(body["products"].as_array().unwrap().len(), 1);
    assert_eq!(body["products"][0]["slug"], "live");
    assert_eq!(body["products"][0]["currency"], "EUR");
}

#[sqlx::test]
async fn sold_out_items_still_listed(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    create_product(
        &app,
        "t",
        json!({"slug":"gone","title":"Gone","available":false}),
    )
    .await;

    let body = body_json(
        app.oneshot(request("GET", "/products", None, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        body["total"], 1,
        "available is a display flag, not a filter"
    );
    assert_eq!(body["products"][0]["available"], false);
}

#[sqlx::test]
async fn view_increments_published_and_noops_otherwise(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    create_product(&app, "t", json!({"slug":"seen","title":"Seen"})).await;
    create_product(&app, "t", json!({"slug":"hidden","title":"H","draft":true})).await;

    for _ in 0..3 {
        let resp = app
            .clone()
            .oneshot(request("POST", "/products/seen/view", None, None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }
    // Missing and draft slugs are no-op 204s.
    for slug in ["ghost", "hidden"] {
        let resp = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/products/{slug}/view"),
                None,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    let body = body_json(
        app.clone()
            .oneshot(request("GET", "/products/seen", None, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["views"], 3, "three increments; got {body}");

    let body = body_json(
        app.oneshot(request("GET", "/products/hidden", Some("t"), None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["views"], 0, "draft not incremented; got {body}");
}

#[sqlx::test]
async fn status_all_requires_admin(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    create_product(&app, "t", json!({"slug":"p","title":"P"})).await;
    create_product(&app, "t", json!({"slug":"d","title":"D","draft":true})).await;

    let body = body_json(
        app.clone()
            .oneshot(request("GET", "/products?status=all", None, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["total"], 1, "no token forces published; got {body}");

    let body = body_json(
        app.oneshot(request("GET", "/products?status=all", Some("t"), None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["total"], 2, "admin sees drafts; got {body}");
}

#[sqlx::test]
async fn single_draft_hidden_unless_admin(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    create_product(&app, "t", json!({"slug":"secret","title":"S","draft":true})).await;

    let resp = app
        .clone()
        .oneshot(request("GET", "/products/secret", None, None))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "draft hidden to public"
    );

    let resp = app
        .oneshot(request("GET", "/products/secret", Some("t"), None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[sqlx::test]
async fn list_orders_by_position_then_recency(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    create_product(&app, "t", json!({"slug":"c","title":"C","position":2})).await;
    create_product(&app, "t", json!({"slug":"a","title":"A","position":0})).await;
    create_product(&app, "t", json!({"slug":"b","title":"B","position":1})).await;

    let body = body_json(
        app.oneshot(request("GET", "/products", None, None))
            .await
            .unwrap(),
    )
    .await;
    let slugs: Vec<&str> = body["products"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, vec!["a", "b", "c"], "position ascending; got {body}");
}

#[sqlx::test]
async fn create_rejects_invalid_input(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    for body in [
        json!({"slug":"Bad Slug","title":"T"}),
        json!({"slug":"ok","title":"  "}),
        json!({"slug":"ok","title":"T","price_cents":-1}),
    ] {
        let resp = app
            .clone()
            .oneshot(request("POST", "/products", Some("t"), Some(body.clone())))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body {body}");
    }
}

#[sqlx::test]
async fn create_duplicate_slug_is_409(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    create_product(&app, "t", json!({"slug":"dup","title":"A"})).await;
    let resp = app
        .oneshot(request(
            "POST",
            "/products",
            Some("t"),
            Some(json!({"slug":"dup","title":"B"})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[sqlx::test]
async fn update_patches_subset_and_sets_updated_date(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    let created = create_product(
        &app,
        "t",
        json!({"slug":"u","title":"Old","price_cents":1000}),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/products/{id}"),
            Some("t"),
            Some(json!({"price_cents":1500,"available":false})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["price_cents"], 1500);
    assert_eq!(body["available"], false);
    assert!(
        !body["updated_date"].is_null(),
        "updated_date set; got {body}"
    );
    // Untouched fields preserved.
    assert_eq!(body["title"], "Old");
    assert_eq!(body["slug"], "u");

    // Unknown id -> 404.
    let resp = app
        .oneshot(request(
            "PATCH",
            "/products/11111111-1111-1111-1111-111111111111",
            Some("t"),
            Some(json!({"title":"z"})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn update_rejects_bad_present_fields(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    let created = create_product(&app, "t", json!({"slug":"v","title":"T"})).await;
    let id = created["id"].as_str().unwrap().to_string();

    for bad in [
        json!({"slug":"Bad Slug"}),
        json!({"title":"  "}),
        json!({"price_cents":-5}),
    ] {
        let resp = app
            .clone()
            .oneshot(request(
                "PATCH",
                &format!("/products/{id}"),
                Some("t"),
                Some(bad.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body {bad}");
    }
}

#[sqlx::test]
async fn delete_removes_then_404s(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    let created = create_product(&app, "t", json!({"slug":"del","title":"D"})).await;
    let id = created["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/products/{id}"),
            Some("t"),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .oneshot(request(
            "DELETE",
            &format!("/products/{id}"),
            Some("t"),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn admin_endpoints_reject_missing_and_bad_token(pool: PgPool) {
    let app = router(state(pool, Some("secret")));
    let valid = json!({"slug":"x","title":"T"});
    let nil = "00000000-0000-0000-0000-000000000000";
    let cases = [
        request("POST", "/products", None, Some(valid.clone())),
        request("POST", "/products", Some("nope"), Some(valid.clone())),
        request(
            "PATCH",
            &format!("/products/{nil}"),
            None,
            Some(json!({"title":"z"})),
        ),
        request("DELETE", &format!("/products/{nil}"), Some("nope"), None),
    ];
    for req in cases {
        let method = req.method().clone();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "method {method}");
    }
}

#[sqlx::test]
async fn malformed_uuid_is_404_on_patch_and_delete(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    let resp = app
        .clone()
        .oneshot(request(
            "PATCH",
            "/products/not-a-uuid",
            Some("t"),
            Some(json!({"title":"z"})),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "bad uuid -> 404 not 500"
    );
    let resp = app
        .oneshot(request("DELETE", "/products/not-a-uuid", Some("t"), None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn open_mode_keeps_reads_open_but_refuses_writes(pool: PgPool) {
    // ADMIN_TOKEN unset: reads stay open, writes refused (secure by default).
    let app = router(state(pool.clone(), None));
    let resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/products",
            None,
            Some(json!({"slug":"d","title":"D"})),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "writes refused unconfigured"
    );

    sqlx::query("INSERT INTO products (slug, title) VALUES ('seeded', 'Seeded')")
        .execute(&pool)
        .await
        .unwrap();
    let resp = app
        .oneshot(request("GET", "/products/seeded", None, None))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "reads open in unconfigured mode"
    );
}
