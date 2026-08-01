use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use dullahan::{AppState, config::Config, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

mod common;
use common::{OTHER, SITE, Tenant, state, state_no_tenants, state_two_tenants, state_with_tenants};

async fn state_cur(pool: PgPool, admin_token: Option<&str>, currency: &str) -> AppState {
    state_with_tenants(
        pool,
        admin_token,
        Config {
            shop_currency: currency.into(),
            ..Config::default()
        },
        &[Tenant::new(SITE)],
    )
    .await
}

async fn state_cors(pool: PgPool, product_origins: Option<Vec<String>>) -> AppState {
    state_with_tenants(
        pool,
        None,
        Config {
            product_origins,
            ..Config::default()
        },
        &[Tenant::new(SITE)],
    )
    .await
}

/// The site is injected here rather than repeated across ~30 URL literals.
fn request(method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    request_for(SITE, method, uri, token, body)
}

fn request_for(
    site: &str,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Request<Body> {
    let uri = common::scoped(uri, site);
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

async fn create_product_for(app: &Router, site: &str, token: &str, body: Value) -> Value {
    let resp = app
        .clone()
        .oneshot(request_for(
            site,
            "POST",
            "/products",
            Some(token),
            Some(body),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create should succeed");
    body_json(resp).await
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
    let app = router(state(pool, Some("t")).await);
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
    let app = router(state(pool, Some("t")).await);
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
    let app = router(state_cur(pool, Some("t"), "USD").await);
    let created =
        create_product(&app, "t", json!({"slug":"p","title":"P","price_cents":500})).await;
    assert_eq!(created["currency"], "USD", "SHOP_CURRENCY plumbs through");
}

#[sqlx::test]
async fn published_list_hides_drafts(pool: PgPool) {
    let app = router(state(pool, Some("t")).await);
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
    let app = router(state(pool, Some("t")).await);
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
    let app = router(state(pool, Some("t")).await);
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
    let app = router(state(pool, Some("t")).await);
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
    let app = router(state(pool, Some("t")).await);
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
    let app = router(state(pool, Some("t")).await);
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
    let app = router(state(pool, Some("t")).await);
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
    let app = router(state(pool, Some("t")).await);
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
    let app = router(state(pool, Some("t")).await);
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
    let app = router(state(pool, Some("t")).await);
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
    let app = router(state(pool, Some("t")).await);
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
    let app = router(state(pool, Some("secret")).await);
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
    let app = router(state(pool, Some("t")).await);
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
async fn cors_open_by_default_for_cross_origin_reads(pool: PgPool) {
    // No PRODUCT_ORIGINS => any origin may read the catalog (mirrors /collect).
    let app = router(state_cors(pool, None).await);
    let resp = app
        .oneshot(
            Request::get("/products?site=t")
                .header(header::ORIGIN, "https://shop.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .unwrap(),
        "*",
        "open reads advertise a wildcard ACAO"
    );
}

#[sqlx::test]
async fn cors_reflects_allowlisted_origin(pool: PgPool) {
    let app = router(state_cors(pool, Some(vec!["https://shop.example".into()])).await);
    let resp = app
        .oneshot(
            Request::get("/products?site=t")
                .header(header::ORIGIN, "https://shop.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .unwrap(),
        "https://shop.example",
        "an allowlisted origin is reflected"
    );
}

#[sqlx::test]
async fn cors_wildcard_origin_does_not_panic(pool: PgPool) {
    // PRODUCT_ORIGINS="*" must collapse to Any, not be passed to allow_origin(list)
    // (which tower-http rejects) — mirrors the stats wildcard guard.
    let app = router(state_cors(pool, Some(vec!["*".into()])).await);
    let resp = app
        .oneshot(
            Request::get("/products?site=t")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[sqlx::test]
async fn cors_preflight_allows_get_and_post_not_delete(pool: PgPool) {
    let app = router(state_cors(pool, None).await);
    let resp = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/products")
                .header(header::ORIGIN, "https://shop.example")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "preflight answered by CORS layer; got {:?}",
        resp.status()
    );
    let allow = resp
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_METHODS)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(allow.contains("GET"), "GET allowed; got {allow}");
    assert!(
        allow.contains("POST"),
        "POST (the /view ping) allowed; got {allow}"
    );
    assert!(
        !allow.contains("DELETE") && !allow.contains("PATCH"),
        "admin mutating verbs are not CORS-exposed; got {allow}"
    );
}

#[sqlx::test]
async fn open_mode_keeps_reads_open_but_refuses_writes(pool: PgPool) {
    // ADMIN_TOKEN unset: reads stay open, writes refused (secure by default).
    let app = router(state(pool.clone(), None).await);
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

    sqlx::query("INSERT INTO products (site_id, slug, title) VALUES ($1, 'seeded', 'Seeded')")
        .bind(SITE)
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

// ---------------------------------------------------------------------------
// Cross-tenant isolation — mirrors tests/blog.rs. Same slug in both tenants,
// because that is where a missing `WHERE site_id` gives a wrong answer rather
// than an error.
// ---------------------------------------------------------------------------

const OP: &str = "op";

async fn seed_both(app: &Router) -> (Value, Value) {
    let a = create_product_for(app, SITE, OP, json!({"slug": "mug", "title": "A's mug"})).await;
    let b = create_product_for(app, OTHER, OP, json!({"slug": "mug", "title": "B's mug"})).await;
    (a, b)
}

#[sqlx::test]
async fn same_slug_in_two_sites_both_succeed(pool: PgPool) {
    let app = router(state_two_tenants(pool, OP).await);
    let (a, b) = seed_both(&app).await;
    assert_eq!(a["site_id"], SITE);
    assert_eq!(b["site_id"], OTHER);
}

#[sqlx::test]
async fn list_never_leaks_across_tenants(pool: PgPool) {
    let app = router(state_two_tenants(pool, OP).await);
    seed_both(&app).await;

    let resp = app
        .clone()
        .oneshot(request_for(SITE, "GET", "/products", None, None))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["total"], 1, "got {body}");
    assert_eq!(body["products"][0]["title"], "A's mug");
}

#[sqlx::test]
async fn get_returns_the_named_tenants_row(pool: PgPool) {
    let app = router(state_two_tenants(pool, OP).await);
    seed_both(&app).await;

    let resp = app
        .clone()
        .oneshot(request_for(OTHER, "GET", "/products/mug", None, None))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["title"], "B's mug", "got {body}");
}

#[sqlx::test]
async fn site_token_cannot_reach_another_tenant(pool: PgPool) {
    let app = router(state_two_tenants(pool, OP).await);
    seed_both(&app).await;

    let resp = app
        .clone()
        .oneshot(request_for(
            OTHER,
            "GET",
            "/products",
            Some("token-a"),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn correct_uuid_under_the_wrong_tenant_is_a_404_and_mutates_nothing(pool: PgPool) {
    let app = router(state_two_tenants(pool.clone(), OP).await);
    let (_, b) = seed_both(&app).await;
    let b_id = b["id"].as_str().unwrap();

    let resp = app
        .clone()
        .oneshot(request_for(
            SITE,
            "PATCH",
            &format!("/products/{b_id}"),
            Some(OP),
            Some(json!({"title": "hijacked"})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "must not be 403");

    let title: String = sqlx::query_scalar("SELECT title FROM products WHERE id = $1::uuid")
        .bind(b_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(title, "B's mug");
}

#[sqlx::test]
async fn view_counter_only_increments_the_named_tenant(pool: PgPool) {
    let app = router(state_two_tenants(pool.clone(), OP).await);
    seed_both(&app).await;

    let resp = app
        .clone()
        .oneshot(request_for(SITE, "POST", "/products/mug/view", None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let views: Vec<(String, i64)> =
        sqlx::query_as("SELECT site_id, views FROM products ORDER BY site_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(views, vec![(OTHER.to_string(), 0), (SITE.to_string(), 1)]);
}

#[sqlx::test]
async fn unknown_site_on_create_is_a_400_not_a_500(pool: PgPool) {
    // The registry is permissive while empty, but the foreign key is not.
    let app = router(state_no_tenants(pool, Some(OP)).await);
    let resp = app
        .clone()
        .oneshot(request_for(
            "ghost",
            "POST",
            "/products",
            Some(OP),
            Some(json!({"slug": "x", "title": "x"})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
