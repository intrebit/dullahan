use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use dullahan::{config::Config, router, state::AppState};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;
use tower::ServiceExt;

fn state(pool: PgPool, admin_token: Option<&str>) -> AppState {
    AppState {
        config: Arc::new(Config {
            bind_addr: "0.0.0.0:0".into(),
            database_url: String::new(),
            allowed_sites: None,
            admin_token: admin_token.map(String::from),
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

async fn create_post(app: &Router, token: &str, body: Value) -> Value {
    let resp = app
        .clone()
        .oneshot(request("POST", "/posts", Some(token), Some(body)))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create should succeed");
    body_json(resp).await
}

#[sqlx::test]
async fn published_list_hides_drafts(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    create_post(
        &app,
        "t",
        json!({"slug":"published-1","title":"Pub","body_markdown":"# hi"}),
    )
    .await;
    create_post(
        &app,
        "t",
        json!({"slug":"draft-1","title":"Draft","body_markdown":"x","draft":true}),
    )
    .await;

    let resp = app
        .oneshot(request("GET", "/posts", None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["total"], 1, "got {body}");
    assert_eq!(body["posts"].as_array().unwrap().len(), 1);
    assert_eq!(body["posts"][0]["slug"], "published-1");
    // List items must not carry the markdown body.
    assert!(
        body["posts"][0].get("body_markdown").is_none(),
        "got {body}"
    );
}

#[sqlx::test]
async fn status_all_requires_admin(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    create_post(
        &app,
        "t",
        json!({"slug":"p","title":"P","body_markdown":"x"}),
    )
    .await;
    create_post(
        &app,
        "t",
        json!({"slug":"d","title":"D","body_markdown":"x","draft":true}),
    )
    .await;

    // status=all without a token must be forced back to published-only.
    let body = body_json(
        app.clone()
            .oneshot(request("GET", "/posts?status=all", None, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["total"], 1, "no token forces published; got {body}");

    // status=all with a token includes drafts.
    let body = body_json(
        app.oneshot(request("GET", "/posts?status=all", Some("t"), None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["total"], 2, "admin sees drafts; got {body}");
}

#[sqlx::test]
async fn list_paginates_but_total_is_unbounded(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    for i in 0..3 {
        create_post(
            &app,
            "t",
            json!({"slug": format!("p{i}"), "title": format!("T{i}"), "body_markdown":"x"}),
        )
        .await;
    }
    let body = body_json(
        app.oneshot(request("GET", "/posts?limit=2&offset=0", None, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["posts"].as_array().unwrap().len(), 2, "got {body}");
    assert_eq!(body["total"], 3, "total ignores pagination; got {body}");
}

#[sqlx::test]
async fn single_draft_hidden_unless_admin(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    create_post(
        &app,
        "t",
        json!({"slug":"secret","title":"S","body_markdown":"body"}),
    )
    .await;
    // Flip it to draft via PATCH (admin) to avoid relying on create-with-draft.
    create_post(
        &app,
        "t",
        json!({"slug":"secret2","title":"S2","body_markdown":"body","draft":true}),
    )
    .await;

    let resp = app
        .clone()
        .oneshot(request("GET", "/posts/secret2", None, None))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "draft hidden to public"
    );

    let resp = app
        .oneshot(request("GET", "/posts/secret2", Some("t"), None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["body_markdown"], "body");
}

#[sqlx::test]
async fn get_missing_is_404(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    let resp = app
        .oneshot(request("GET", "/posts/nope", None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn view_increments_published_and_noops_otherwise(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    create_post(
        &app,
        "t",
        json!({"slug":"post","title":"P","body_markdown":"x"}),
    )
    .await;
    create_post(
        &app,
        "t",
        json!({"slug":"hidden","title":"H","body_markdown":"x","draft":true}),
    )
    .await;

    for _ in 0..2 {
        let resp = app
            .clone()
            .oneshot(request("POST", "/posts/post/view", None, None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }
    // Absent slug is a no-op 204.
    let resp = app
        .clone()
        .oneshot(request("POST", "/posts/ghost/view", None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    // Draft is a no-op 204 (WHERE draft = false).
    let resp = app
        .clone()
        .oneshot(request("POST", "/posts/hidden/view", None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let body = body_json(
        app.clone()
            .oneshot(request("GET", "/posts/post", None, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["views"], 2, "two increments; got {body}");

    let body = body_json(
        app.oneshot(request("GET", "/posts/hidden", Some("t"), None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["views"], 0, "draft not incremented; got {body}");
}

#[sqlx::test]
async fn create_then_get_round_trip(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    let created = create_post(
        &app,
        "t",
        json!({
            "slug":"hello","title":"Hello","description":"d","author":"Me",
            "image":"/img.png","body_markdown":"# Body"
        }),
    )
    .await;
    assert_eq!(created["slug"], "hello");
    assert_eq!(created["title"], "Hello");
    assert_eq!(created["description"], "d");
    assert_eq!(created["author"], "Me");
    assert_eq!(created["image"], "/img.png");
    assert_eq!(created["body_markdown"], "# Body");
    assert_eq!(created["draft"], false);
    assert_eq!(created["views"], 0);
    assert!(created["updated_date"].is_null());
    assert!(
        created["id"].as_str().unwrap().len() >= 32,
        "uuid id; got {created}"
    );
    let pd = created["pub_date"].as_str().unwrap();
    assert!(
        pd.ends_with('Z'),
        "pub_date is UTC RFC3339 ending in Z; got {pd}"
    );

    let fetched = body_json(
        app.oneshot(request("GET", "/posts/hello", None, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(fetched["id"], created["id"]);
    assert_eq!(fetched["body_markdown"], "# Body");
}

#[sqlx::test]
async fn create_applies_defaults(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    let created = create_post(
        &app,
        "t",
        json!({"slug":"min","title":"Min","body_markdown":"x"}),
    )
    .await;
    assert_eq!(created["author"], "Andrej Focic");
    assert_eq!(created["description"], "");
    assert_eq!(created["draft"], false);
    assert!(created["image"].is_null());
    assert!(!created["pub_date"].as_str().unwrap().is_empty());
}

#[sqlx::test]
async fn create_duplicate_slug_is_409(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    create_post(
        &app,
        "t",
        json!({"slug":"dup","title":"A","body_markdown":"x"}),
    )
    .await;
    let resp = app
        .oneshot(request(
            "POST",
            "/posts",
            Some("t"),
            Some(json!({"slug":"dup","title":"B","body_markdown":"y"})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[sqlx::test]
async fn create_rejects_invalid_input(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    for body in [
        json!({"slug":"Bad Slug","title":"T","body_markdown":"x"}),
        json!({"slug":"ok","title":"  ","body_markdown":"x"}),
        json!({"slug":"ok","title":"T","body_markdown":"   "}),
    ] {
        let resp = app
            .clone()
            .oneshot(request("POST", "/posts", Some("t"), Some(body.clone())))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body {body}");
    }
}

#[sqlx::test]
async fn admin_endpoints_reject_missing_and_bad_token(pool: PgPool) {
    let app = router(state(pool, Some("secret")));
    let valid = json!({"slug":"x","title":"T","body_markdown":"b"});
    let nil = "00000000-0000-0000-0000-000000000000";

    let cases = [
        request("POST", "/posts", None, Some(valid.clone())),
        request("POST", "/posts", Some("nope"), Some(valid.clone())),
        request(
            "PATCH",
            &format!("/posts/{nil}"),
            None,
            Some(json!({"title":"z"})),
        ),
        request(
            "PATCH",
            &format!("/posts/{nil}"),
            Some("nope"),
            Some(json!({"title":"z"})),
        ),
        request("DELETE", &format!("/posts/{nil}"), None, None),
        request("DELETE", &format!("/posts/{nil}"), Some("nope"), None),
    ];
    for req in cases {
        let method = req.method().clone();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "method {method}");
    }
}

#[sqlx::test]
async fn update_patches_subset_and_sets_updated_date(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    let created = create_post(
        &app,
        "t",
        json!({"slug":"u","title":"Old","body_markdown":"x"}),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    assert!(created["updated_date"].is_null());

    let resp = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/posts/{id}"),
            Some("t"),
            Some(json!({"title":"New","draft":true})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["title"], "New");
    assert_eq!(body["draft"], true);
    assert!(
        !body["updated_date"].is_null(),
        "updated_date set; got {body}"
    );
    // Untouched fields are preserved.
    assert_eq!(body["body_markdown"], "x");
    assert_eq!(body["slug"], "u");

    // Unknown id -> 404.
    let resp = app
        .oneshot(request(
            "PATCH",
            "/posts/11111111-1111-1111-1111-111111111111",
            Some("t"),
            Some(json!({"title":"z"})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn delete_removes_then_404s(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    let created = create_post(
        &app,
        "t",
        json!({"slug":"del","title":"D","body_markdown":"x"}),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(request("DELETE", &format!("/posts/{id}"), Some("t"), None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .clone()
        .oneshot(request("GET", "/posts/del", None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let resp = app
        .oneshot(request("DELETE", &format!("/posts/{id}"), Some("t"), None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn list_orders_by_pub_date_desc(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    // Insert out of chronological order; newest pub_date must come first.
    create_post(
        &app,
        "t",
        json!({"slug":"jan","title":"Jan","body_markdown":"x","pub_date":"2026-01-01T00:00:00Z"}),
    )
    .await;
    create_post(
        &app,
        "t",
        json!({"slug":"mar","title":"Mar","body_markdown":"x","pub_date":"2026-03-01T00:00:00Z"}),
    )
    .await;
    create_post(
        &app,
        "t",
        json!({"slug":"feb","title":"Feb","body_markdown":"x","pub_date":"2026-02-01T00:00:00Z"}),
    )
    .await;

    let body = body_json(
        app.clone()
            .oneshot(request("GET", "/posts", None, None))
            .await
            .unwrap(),
    )
    .await;
    let slugs: Vec<&str> = body["posts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, vec!["mar", "feb", "jan"], "newest first; got {body}");

    // Pagination returns the newest page first.
    let body = body_json(
        app.oneshot(request("GET", "/posts?limit=2&offset=0", None, None))
            .await
            .unwrap(),
    )
    .await;
    let slugs: Vec<&str> = body["posts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, vec!["mar", "feb"], "got {body}");
}

#[sqlx::test]
async fn malformed_uuid_is_404_on_patch_and_delete(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    let resp = app
        .clone()
        .oneshot(request(
            "PATCH",
            "/posts/not-a-uuid",
            Some("t"),
            Some(json!({"title":"z"})),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "PATCH bad uuid -> 404 not 500"
    );
    let resp = app
        .oneshot(request("DELETE", "/posts/not-a-uuid", Some("t"), None))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "DELETE bad uuid -> 404 not 500"
    );
}

#[sqlx::test]
async fn update_rename_to_existing_slug_is_409(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    create_post(
        &app,
        "t",
        json!({"slug":"a","title":"A","body_markdown":"x"}),
    )
    .await;
    let b = create_post(
        &app,
        "t",
        json!({"slug":"b","title":"B","body_markdown":"x"}),
    )
    .await;
    let id = b["id"].as_str().unwrap();

    let resp = app
        .oneshot(request(
            "PATCH",
            &format!("/posts/{id}"),
            Some("t"),
            Some(json!({"slug":"a"})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[sqlx::test]
async fn update_validates_only_present_fields(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    let created = create_post(
        &app,
        "t",
        json!({"slug":"v","title":"T","body_markdown":"x"}),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    for bad in [
        json!({"slug":"Bad Slug"}),
        json!({"title":"  "}),
        json!({"body_markdown":"   "}),
    ] {
        let resp = app
            .clone()
            .oneshot(request(
                "PATCH",
                &format!("/posts/{id}"),
                Some("t"),
                Some(bad.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body {bad}");
    }

    // An omitted field skips its validation — a description-only PATCH succeeds.
    let resp = app
        .oneshot(request(
            "PATCH",
            &format!("/posts/{id}"),
            Some("t"),
            Some(json!({"description":"d"})),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[sqlx::test]
async fn open_mode_keeps_reads_open_but_refuses_writes(pool: PgPool) {
    // ADMIN_TOKEN unset: reads (including drafts) stay open, but writes are
    // refused — an unconfigured deploy can't be mutated by anonymous callers.
    let app = router(state(pool.clone(), None));

    // Writes are refused when no token is configured.
    let resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/posts",
            None,
            Some(json!({"slug":"d","title":"D","body_markdown":"x","draft":true})),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "writes refused without a configured ADMIN_TOKEN"
    );

    // Seed a draft directly (writes are gated) to confirm reads stay open.
    sqlx::query(
        "INSERT INTO blog_posts (slug, title, body_markdown, draft) VALUES ('d', 'D', 'x', true)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let body = body_json(
        app.clone()
            .oneshot(request("GET", "/posts?status=all", None, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["total"], 1, "open reads include drafts; got {body}");

    let resp = app
        .oneshot(request("GET", "/posts/d", None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "draft readable in open mode");
}

#[sqlx::test]
async fn drafts_opt_in_only_on_literal_status_all(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    create_post(
        &app,
        "t",
        json!({"slug":"p","title":"P","body_markdown":"x"}),
    )
    .await;
    create_post(
        &app,
        "t",
        json!({"slug":"d","title":"D","body_markdown":"x","draft":true}),
    )
    .await;

    // Admin token present, but only the literal status=all opts drafts in.
    for q in ["", "?status=published", "?status=garbage"] {
        let body = body_json(
            app.clone()
                .oneshot(request("GET", &format!("/posts{q}"), Some("t"), None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            body["total"], 1,
            "q={q:?} should exclude drafts; got {body}"
        );
    }
}

#[sqlx::test]
async fn list_limit_is_clamped(pool: PgPool) {
    let app = router(state(pool, Some("t")));
    for i in 0..3 {
        create_post(
            &app,
            "t",
            json!({"slug": format!("p{i}"), "title":"T", "body_markdown":"x"}),
        )
        .await;
    }

    // A limit above the cap is accepted and clamped, not rejected.
    let resp = app
        .clone()
        .oneshot(request("GET", "/posts?limit=500", None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["posts"].as_array().unwrap().len(), 3);

    // limit=0 clamps up to 1.
    let body = body_json(
        app.oneshot(request("GET", "/posts?limit=0", None, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        body["posts"].as_array().unwrap().len(),
        1,
        "limit 0 clamps to 1; got {body}"
    );
}
