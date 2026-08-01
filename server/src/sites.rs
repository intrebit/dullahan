//! In-memory registry of tenants, backed by the `sites` table. Replaces the
//! `ALLOWED_SITES` env allowlist.
//!
//! `/collect` is a hot fire-and-forget path (`ingest.rs`), so admission must
//! never touch the database per request — the whole table is a handful of rows
//! and is cached wholesale, refreshed on a timer exactly like the daily salt in
//! [`crate::salt`].
//!
//! **Scoping is not admission.** Scoping (`WHERE site_id = $n`) is applied
//! unconditionally from the request's `site` and is what prevents cross-tenant
//! reads and writes. Admission ([`check`]) is a separate gate deciding whether
//! the named site is one this server serves. Scoping never depends on the
//! registry being populated, which is what makes the "empty registry is
//! permissive" rule below safe: with an empty registry `GET /posts?site=x`
//! returns an *empty list*, never another tenant's rows.

use crate::state::AppState;
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPool;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// One tenant. Deliberately carries no credential: nothing downstream should be
/// able to reach a token from a resolved site.
#[derive(Debug, Clone)]
pub struct Site {
    pub id: Arc<str>,
    pub name: String,
    pub domain: Option<String>,
    pub contact_to: Option<String>,
    pub email_from: Option<String>,
    pub email_from_name: Option<String>,
    pub active: bool,
}

#[derive(Debug, Default)]
pub struct Registry {
    by_id: HashMap<Arc<str>, Arc<Site>>,
    /// SHA-256 of the bearer token → site id. See [`token_digest`] for why a
    /// map probe on a digest is both O(1) and timing-safe.
    by_token: HashMap<[u8; 32], Arc<str>>,
}

impl Registry {
    pub fn get(&self, id: &str) -> Option<&Arc<Site>> {
        self.by_id.get(id)
    }

    /// `true` when the site is registered and not suspended. A registry with no
    /// rows admits everything — see the module docs.
    pub fn is_active(&self, id: &str) -> bool {
        if self.by_id.is_empty() {
            return true;
        }
        self.by_id.get(id).is_some_and(|s| s.active)
    }

    /// The site a bearer token belongs to. A suspended site's token resolves to
    /// `None` so it behaves exactly like an unrecognized one.
    pub fn site_for_token(&self, digest: &[u8; 32]) -> Option<Arc<str>> {
        let id = self.by_token.get(digest)?;
        let site = self.by_id.get(id)?;
        site.active.then(|| site.id.clone())
    }

    pub fn active(&self) -> Vec<Arc<Site>> {
        let mut sites: Vec<Arc<Site>> = self
            .by_id
            .values()
            .filter(|s| s.active)
            .map(Arc::clone)
            .collect();
        sites.sort_by(|a, b| a.id.cmp(&b.id));
        sites
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Read-mostly: an `RwLock` around an `Arc` snapshot. Readers clone the `Arc`
/// under the guard and never hold it across an await.
pub type SiteCache = Arc<RwLock<Arc<Registry>>>;

pub fn new_cache() -> SiteCache {
    Arc::new(RwLock::new(Arc::new(Registry::default())))
}

/// Build a cache pre-populated with `sites`, for tests and for the synchronous
/// startup load.
pub fn cache_from(sites: Vec<Site>) -> SiteCache {
    Arc::new(RwLock::new(Arc::new(build(sites, &[]))))
}

pub fn snapshot(cache: &SiteCache) -> Arc<Registry> {
    Arc::clone(&cache.read().unwrap())
}

#[derive(Debug, sqlx::FromRow)]
struct SiteRow {
    id: String,
    name: String,
    domain: Option<String>,
    contact_to: Option<String>,
    email_from: Option<String>,
    email_from_name: Option<String>,
    admin_token_hash: Option<Vec<u8>>,
    active: bool,
}

const SITE_COLS: &str = "id, name, domain, contact_to, email_from, email_from_name, \
     admin_token_hash, active";

/// Load the whole registry. Rows whose `email_from` fails validation are loaded
/// with `email_from = None` plus a warning, so a malformed address can never
/// reach a `From:` header even if the DB CHECK were dropped — defence in depth
/// against header injection.
pub async fn load(pool: &PgPool) -> sqlx::Result<Registry> {
    let rows: Vec<SiteRow> = sqlx::query_as(&format!("SELECT {SITE_COLS} FROM sites"))
        .fetch_all(pool)
        .await?;

    let mut tokens = Vec::new();
    let sites = rows
        .into_iter()
        .map(|r| {
            let id: Arc<str> = Arc::from(r.id.as_str());
            if let Some(hash) = r.admin_token_hash {
                match <[u8; 32]>::try_from(hash.as_slice()) {
                    Ok(digest) => tokens.push((digest, Arc::clone(&id))),
                    Err(_) => tracing::warn!(
                        site = %id,
                        len = hash.len(),
                        "admin_token_hash is not 32 bytes; ignoring the token for this site"
                    ),
                }
            }
            let email_from = r.email_from.filter(|addr| {
                let ok = crate::contact::email_looks_valid(addr);
                if !ok {
                    tracing::warn!(site = %id, "sites.email_from is not a valid address; ignoring it");
                }
                ok
            });
            Site {
                id,
                name: r.name,
                domain: r.domain,
                contact_to: r.contact_to,
                email_from,
                email_from_name: r.email_from_name,
                active: r.active,
            }
        })
        .collect();

    Ok(build(sites, &tokens))
}

fn build(sites: Vec<Site>, tokens: &[([u8; 32], Arc<str>)]) -> Registry {
    let by_id = sites
        .into_iter()
        .map(|s| (Arc::clone(&s.id), Arc::new(s)))
        .collect();
    let by_token = tokens.iter().cloned().collect();
    Registry { by_id, by_token }
}

/// Reload the cache. On error the previous snapshot is left in place: a
/// transient DB blip must never un-gate ingest or lock out every tenant.
pub async fn refresh(pool: &PgPool, cache: &SiteCache) -> sqlx::Result<usize> {
    let registry = load(pool).await?;
    let len = registry.len();
    *cache.write().unwrap() = Arc::new(registry);
    Ok(len)
}

/// The single admission gate, shared by stats, blog, products and site_config so
/// there is exactly one definition of "403".
pub fn check(state: &AppState, site: &str) -> Result<(), StatusCode> {
    if snapshot(&state.sites).is_active(site) {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

/// The `?site=` every tenant-scoped endpoint requires. No serde default, so a
/// missing param is a 400 from axum's `Query` extractor — mirroring the
/// mandatory `site` on `/stats/*`.
#[derive(Debug, Deserialize)]
pub struct SiteQuery {
    pub site: String,
}

/// `^[a-z0-9-]+$` with a sane bound. The canonical site-id validator, matching
/// the `sites_id_check` constraint in migration 0004.
pub fn site_valid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// SHA-256 of a bearer token.
///
/// The registry is keyed by this digest, which is what makes token lookup both
/// O(1) in the number of tenants and free of a timing oracle: we never compare
/// the presented secret against N stored secrets, we hash it once and probe a
/// map. SHA-256's runtime depends only on input length, and the resulting bucket
/// index is derived from the attacker's *own* token, so it reveals nothing about
/// any stored credential. See `docs/SECURITY.md` for why a salted KDF would be
/// the wrong primitive (it would also make this lookup impossible).
pub fn token_digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// A fresh per-site token: 256 bits from the OS CSPRNG, base64url, prefixed so
/// secret scanners can recognize it and so it is distinguishable from
/// `ADMIN_TOKEN` in a log that should not have contained either.
///
/// This is deliberately the *only* way a token is constructed — the API never
/// accepts a caller-supplied one. That is what keeps "256 bits of CSPRNG" a
/// property of the code rather than a line in the docs, which in turn is what
/// justifies the fast unsalted hash above.
pub fn generate_token() -> String {
    let mut raw = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    format!("dh_s_{}", URL_SAFE_NO_PAD.encode(raw))
}

/// Last four characters of a token, stored alongside the hash so an operator can
/// tell which credential is deployed where without holding the plaintext.
pub fn token_last4(token: &str) -> String {
    let n = token.chars().count();
    token.chars().skip(n.saturating_sub(4)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(id: &str, active: bool) -> Site {
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

    #[test]
    fn empty_registry_admits_everything() {
        let reg = Registry::default();
        assert!(
            reg.is_active("anything"),
            "empty registry must be permissive"
        );
    }

    #[test]
    fn populated_registry_rejects_unknown_and_suspended() {
        let reg = build(vec![site("a", true), site("b", false)], &[]);
        assert!(reg.is_active("a"));
        assert!(!reg.is_active("b"), "suspended site is not admitted");
        assert!(!reg.is_active("c"), "unknown site is not admitted");
    }

    #[test]
    fn token_resolves_to_its_site_and_not_to_a_suspended_one() {
        let live = token_digest("live-token");
        let dead = token_digest("dead-token");
        let reg = build(
            vec![site("a", true), site("b", false)],
            &[(live, Arc::from("a")), (dead, Arc::from("b"))],
        );
        assert_eq!(reg.site_for_token(&live).as_deref(), Some("a"));
        assert_eq!(
            reg.site_for_token(&dead),
            None,
            "a suspended site's token must behave like an unknown one"
        );
        assert_eq!(reg.site_for_token(&token_digest("nope")), None);
    }

    #[test]
    fn active_is_sorted_and_excludes_suspended() {
        let reg = build(
            vec![site("zed", true), site("acme", true), site("off", false)],
            &[],
        );
        let active = reg.active();
        let ids: Vec<&str> = active.iter().map(|s| &*s.id).collect();
        assert_eq!(ids, vec!["acme", "zed"]);
    }

    #[test]
    fn site_id_rules() {
        assert!(site_valid("acme-cafe"));
        assert!(site_valid("a"));
        assert!(!site_valid(""));
        assert!(!site_valid("Acme"));
        assert!(!site_valid("has space"));
        assert!(!site_valid("under_score"));
        assert!(!site_valid(&"x".repeat(65)));
    }

    #[test]
    fn generated_tokens_are_distinct_and_prefixed() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a, b);
        assert!(a.starts_with("dh_s_"));
        assert_eq!(a.len(), 5 + 43, "5-char prefix + 43 base64url chars");
        assert_eq!(token_last4(&a), a[a.len() - 4..]);
    }

    #[test]
    fn digest_is_stable_and_input_dependent() {
        assert_eq!(token_digest("x"), token_digest("x"));
        assert_ne!(token_digest("x"), token_digest("y"));
    }
}
