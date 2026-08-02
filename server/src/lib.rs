//! dullahan — a self-hosted, cookie-free backend for small sites.
//!
//! This crate ships the `dullahan` **binary**: a single Rust server providing
//! privacy-first analytics, a headless blog/content API, and a contact endpoint,
//! backed by Postgres. It is meant to be run (`cargo install dullahan`), not
//! consumed as a library dependency.
//!
//! For install and the privacy model see the
//! [README](https://github.com/intrebit/dullahan#readme); for the full `/stats/*`
//! reference and configuration, see
//! [`docs/api.md`](https://github.com/intrebit/dullahan/blob/master/docs/api.md)
//! and [`docs/deploy.md`](https://github.com/intrebit/dullahan/blob/master/docs/deploy.md).
//!
//! # Embedding
//!
//! The whole app is one [`axum::Router`], built by [`router`] (or
//! [`router_with_metrics`]) from a [`Config`]-derived [`AppState`]:
//!
//! ```no_run
//! # async fn run(state: dullahan::AppState) {
//! let app = dullahan::router(state);
//! // serve `app` with axum / hyper as usual
//! # }
//! ```
//!
//! Everything else (ingest, stats, blog, contact handlers, the DB layer, …) is
//! internal plumbing — `pub` only so the integration tests can reach it, hidden
//! here, and **not a stable API**.

// Internal modules: public for the integration tests, hidden from the docs.
#[doc(hidden)]
pub mod auth;
pub mod blog;
#[doc(hidden)]
pub mod channels;
#[doc(hidden)]
pub mod client_ip;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod contact;
#[doc(hidden)]
pub mod db;
#[doc(hidden)]
pub mod digest;
#[doc(hidden)]
pub mod email;
#[doc(hidden)]
pub mod ingest;
#[doc(hidden)]
pub mod products;
#[doc(hidden)]
pub mod salt;

/// Operational checks behind `dullahan --selfcheck`.
pub mod selfcheck;
pub mod site_config;
pub mod sites;
pub mod sites_api;
#[doc(hidden)]
pub mod state;
#[doc(hidden)]
pub mod stats;
#[doc(hidden)]
pub mod types;

/// Runtime configuration, built from environment variables. See [`Config::from_env`].
pub use config::Config;
/// Shared application state passed to [`router`] — holds the DB pool, config, and salt cache.
pub use state::AppState;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderName, HeaderValue, header};
use axum::middleware;
use axum::routing::{get, post};
use axum_prometheus::PrometheusMetricLayer;
use std::sync::Arc;
use std::time::Duration;
use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};
use tower_http::cors::{Any, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

/// Build the application router with HTTP metrics + a `/metrics` endpoint.
/// This installs a process-wide Prometheus recorder, so it can only be called
/// once. `router()` (without metrics) is the entry point for tests and
/// fixtures that may run in parallel.
pub fn router_with_metrics(state: AppState) -> Router {
    let (metrics_layer, metrics_handle) = PrometheusMetricLayer::pair();
    let metrics_route = Router::new().route(
        "/metrics",
        get(move || {
            let handle = metrics_handle.clone();
            async move { handle.render() }
        }),
    );
    router(state).merge(metrics_route).layer(metrics_layer)
}

pub fn router(state: AppState) -> Router {
    let cors_collect = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let cors_stats = match state.config.stats_origins.as_ref() {
        // A literal "*" means "any origin" and must use `Any` — tower-http panics
        // if a wildcard is passed inside `allow_origin(<list>)`. Mixed lists
        // containing "*" also collapse to "any".
        Some(origins) if !origins.is_empty() && !origins.iter().any(|o| o == "*") => {
            let parsed: Vec<HeaderValue> = origins
                .iter()
                .filter_map(|o| HeaderValue::from_str(o).ok())
                .collect();
            CorsLayer::new()
                .allow_origin(parsed)
                .allow_methods([axum::http::Method::GET])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
        }
        _ => CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([axum::http::Method::GET])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
    };

    // Public-read CORS for the product catalog so a storefront on another origin
    // can fetch `/products` from the browser and ping `/products/:slug/view`.
    // Mirrors `cors_stats` (allowlist from `product_origins`, or `Any` when unset
    // or containing "*"), but is GET+POST only: PATCH/DELETE are deliberately
    // absent so a browser can't preflight the admin writes, and only CONTENT_TYPE
    // is allowed (not AUTHORIZATION), so cross-origin admin calls fail — the
    // handlers stay admin-gated regardless. Published catalog data is public;
    // drafts require admin and are not exposed by these reads.
    let cors_products = match state.config.product_origins.as_ref() {
        Some(origins) if !origins.is_empty() && !origins.iter().any(|o| o == "*") => {
            let parsed: Vec<HeaderValue> = origins
                .iter()
                .filter_map(|o| HeaderValue::from_str(o).ok())
                .collect();
            CorsLayer::new()
                .allow_origin(parsed)
                .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
                .allow_headers([header::CONTENT_TYPE])
        }
        _ => CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
            .allow_headers([header::CONTENT_TYPE]),
    };

    // CORS for the per-site config store. Reads are public (a storefront fetches
    // its own config cross-origin); writes are admin-gated in the handler.
    // Mirrors `cors_products` exactly: GET only, and AUTHORIZATION deliberately
    // withheld so a browser cannot cross-origin preflight an admin PUT/DELETE.
    // There is no browser dashboard yet; when there is one, widening this is a
    // deliberate decision to make then, not a default to inherit now.
    let cors_site_config = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([axum::http::Method::GET])
        .allow_headers([header::CONTENT_TYPE]);

    let stats_routes = Router::new()
        .route("/stats/summary", get(stats::summary))
        .route("/stats/timeseries", get(stats::timeseries))
        .route("/stats/top", get(stats::top))
        .route("/stats/events", get(stats::events))
        .route("/stats/channels", get(stats::channels))
        .route("/stats/realtime", get(stats::realtime))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_authenticated,
        ))
        .layer(cors_stats);

    const PUBLIC_BODY_LIMIT: usize = 16 * 1024;

    // /collect: high volume, generous limit. burst absorbs SPA navigations
    // that fire pageleave + pageview close together. 500ms replenish period =
    // ~120/min sustained once the burst is spent.
    let collect_governor = Arc::new(
        GovernorConfigBuilder::default()
            .per_millisecond(500)
            .burst_size(60)
            .key_extractor(client_ip::ClientIpKeyExtractor::new(
                state.config.trust_proxy_headers,
            ))
            .finish()
            .expect("collect rate-limit config is valid"),
    );
    // tower_governor's keyed store never evicts on its own; without this the
    // per-IP map grows without bound — a memory-exhaustion DoS under IP churn or
    // spoofed `x-forwarded-for`. Periodically drop fully-replenished entries.
    {
        let limiter = collect_governor.limiter().clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                limiter.retain_recent();
            }
        });
    }

    // /contact: low volume, strict. 5/min steady, burst 3.
    let contact_governor = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(12)
            .burst_size(3)
            .key_extractor(client_ip::ClientIpKeyExtractor::new(
                state.config.trust_proxy_headers,
            ))
            .finish()
            .expect("contact rate-limit config is valid"),
    );
    {
        let limiter = contact_governor.limiter().clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                limiter.retain_recent();
            }
        });
    }

    let collect_route = Router::new()
        .route("/collect", post(ingest::collect))
        .layer(GovernorLayer {
            config: collect_governor,
        })
        .layer(cors_collect.clone());

    let contact_route = Router::new()
        .route("/contact", post(contact::submit))
        .layer(GovernorLayer {
            config: contact_governor,
        })
        .layer(cors_collect);

    let public_routes = Router::new()
        .merge(collect_route)
        .merge(contact_route)
        .layer(DefaultBodyLimit::max(PUBLIC_BODY_LIMIT));

    // Blog CRUD + view counter. Auth is checked per-handler (some endpoints are
    // public, some admin-only, some change behaviour based on whether the caller
    // is admin), so unlike `/stats/*` there is no router-level admin layer. GET,
    // PATCH and DELETE share `/posts/:key` under one param name — axum/matchit
    // reject the same path registered with differing capture names.
    let blog_routes = Router::new()
        .route("/posts", get(blog::list).post(blog::create))
        .route(
            "/posts/:key",
            get(blog::get_post)
                .patch(blog::update)
                .delete(blog::delete_post),
        )
        .route("/posts/:key/view", post(blog::view));

    // Product catalog. Same auth model as the blog (public reads, admin writes
    // checked per-handler). `:key` is a slug on GET and an id on PATCH/DELETE —
    // one capture name because axum/matchit reject differing names on a path.
    let product_routes = Router::new()
        .route("/products", get(products::list).post(products::create))
        .route(
            "/products/:key",
            get(products::get_product)
                .patch(products::update)
                .delete(products::delete_product),
        )
        .route("/products/:key/view", post(products::view))
        .layer(cors_products);

    // Tenant registry. All-or-nothing operator gate, so unlike blog/products
    // this genuinely is a job for a router-level layer. No CORS layer at all —
    // see the module docs.
    let sites_routes = Router::new()
        .route("/sites", get(sites_api::list).post(sites_api::create))
        .route(
            "/sites/:id",
            get(sites_api::get_site)
                .patch(sites_api::update)
                .delete(sites_api::delete_site),
        )
        .route("/sites/:id/token", post(sites_api::rotate_token))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_operator,
        ));

    let site_config_routes = Router::new()
        .route("/site-config", get(site_config::list))
        .route(
            "/site-config/:site",
            get(site_config::get_config)
                .put(site_config::put_config)
                .delete(site_config::delete_config),
        )
        .layer(cors_site_config);

    let mut app = Router::new()
        .merge(public_routes)
        .route("/health", get(health))
        .merge(stats_routes)
        .merge(blog_routes)
        .merge(product_routes)
        .merge(site_config_routes)
        .merge(sites_routes)
        .with_state(state.clone());

    // Security response headers (defense in depth — most are also useful when
    // clients embed our endpoints in their own pages). CSP `default-src 'none'`
    // is appropriate because every response is JSON or plain text — nothing
    // we serve should ever load subresources or execute script.
    app = app
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("referrer-policy"),
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("content-security-policy"),
            HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
        ));

    if state.config.behind_tls {
        app = app.layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("strict-transport-security"),
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        ));
    }

    let x_request_id = HeaderName::from_static("x-request-id");

    app.layer(TimeoutLayer::new(Duration::from_secs(15)))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(
                    DefaultMakeSpan::new()
                        .level(Level::INFO)
                        .include_headers(false),
                )
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(PropagateRequestIdLayer::new(x_request_id.clone()))
        .layer(SetRequestIdLayer::new(x_request_id, MakeRequestUuid))
}

async fn health() -> &'static str {
    "ok"
}
