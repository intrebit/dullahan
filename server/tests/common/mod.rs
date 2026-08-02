//! Shared fixtures for the integration tests.
//!
//! Every tenant-scoped endpoint needs a registered site (blog_posts and products
//! carry a foreign key to `sites`), so building test state means inserting rows
//! *and* pre-warming the in-memory registry — pre-warming synchronously rather
//! than relying on the 60s refresh task, so no test races the timer.

#![allow(dead_code)]

use dullahan::sites::{self, Site};
use dullahan::{config::Config, state::AppState};
use sqlx::PgPool;
use std::sync::Arc;

/// The default tenant used by tests that are not about tenancy.
pub const SITE: &str = "t";
/// A second tenant, for the cross-tenant isolation tests.
pub const OTHER: &str = "b";

/// One tenant to register.
#[derive(Clone, Debug)]
pub struct Tenant {
    pub id: &'static str,
    /// Per-site admin token, if this tenant should have one.
    pub token: Option<&'static str>,
    pub contact_to: Option<&'static str>,
    pub email_from: Option<&'static str>,
    pub active: bool,
}

impl Tenant {
    pub fn new(id: &'static str) -> Self {
        Self {
            id,
            token: None,
            contact_to: None,
            email_from: None,
            active: true,
        }
    }

    pub fn token(mut self, token: &'static str) -> Self {
        self.token = Some(token);
        self
    }

    pub fn contact_to(mut self, addr: &'static str) -> Self {
        self.contact_to = Some(addr);
        self
    }

    pub fn email_from(mut self, addr: &'static str) -> Self {
        self.email_from = Some(addr);
        self
    }

    pub fn inactive(mut self) -> Self {
        self.active = false;
        self
    }
}

/// Insert the tenants and return state whose registry already knows them.
pub async fn state_with_tenants(
    pool: PgPool,
    admin_token: Option<&str>,
    config: Config,
    tenants: &[Tenant],
) -> AppState {
    for t in tenants {
        sqlx::query(
            "INSERT INTO sites (id, name, contact_to, email_from, active, admin_token_hash)
             VALUES ($1, $1, $2, $3, $4, $5) ON CONFLICT (id) DO NOTHING",
        )
        .bind(t.id)
        .bind(t.contact_to)
        .bind(t.email_from)
        .bind(t.active)
        .bind(t.token.map(|tok| sites::token_digest(tok).to_vec()))
        .execute(&pool)
        .await
        .expect("seed sites");
    }

    let cache = sites::new_cache();
    sites::refresh(&pool, &cache).await.expect("load registry");

    let admin_token_hash = admin_token.map(sites::token_digest);
    // Mirrors main.rs.
    let open_mode = admin_token.is_none();

    // Each test state gets its own writer task. The `JoinHandle` is dropped
    // rather than awaited — the task lives as long as the sender inside the
    // returned state, which is exactly the lifetime of the test.
    let (ingest_tx, _writer) = dullahan::ingest::spawn_writer(pool.clone());

    AppState {
        config: Arc::new(Config {
            admin_token: admin_token.map(String::from),
            ..config
        }),
        pool,
        mailer: None,
        salt_cache: dullahan::salt::new_cache(),
        ingest_tx,
        sites: cache,
        open_mode,
        admin_token_hash,
    }
}

/// The common case: one tenant (`SITE`), default config.
pub async fn state(pool: PgPool, admin_token: Option<&str>) -> AppState {
    state_with_tenants(pool, admin_token, Config::default(), &[Tenant::new(SITE)]).await
}

/// State with no tenants registered at all — the "empty registry is permissive"
/// path, and what a fresh install looks like.
pub async fn state_no_tenants(pool: PgPool, admin_token: Option<&str>) -> AppState {
    state_with_tenants(pool, admin_token, Config::default(), &[]).await
}

/// Two tenants with their own tokens, for cross-tenant isolation tests.
pub async fn state_two_tenants(pool: PgPool, operator: &str) -> AppState {
    state_with_tenants(
        pool,
        Some(operator),
        Config::default(),
        &[
            Tenant::new(SITE).token("token-a"),
            Tenant::new(OTHER).token("token-b"),
        ],
    )
    .await
}

/// Build a `Site` directly, for unit-ish tests that need a registry without a DB.
pub fn site(id: &str, active: bool) -> Site {
    Site {
        id: Arc::from(id),
        name: String::new(),
        domain: None,
        contact_to: None,
        email_from: None,
        email_from_name: None,
        active,
    }
}

/// Append `site=` to a test URL, picking `?` or `&` as appropriate.
pub fn scoped(path: &str, site: &str) -> String {
    let sep = if path.contains('?') { '&' } else { '?' };
    format!("{path}{sep}site={site}")
}
