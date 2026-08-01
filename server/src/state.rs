//! Shared application state (`AppState`): config, DB pool, the salt store, and
//! the tenant registry.

use crate::config::Config;
use crate::email::Mailer;
use crate::salt::SaltCache;
use crate::sites::SiteCache;
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub mailer: Option<Mailer>,
    pub salt_cache: SaltCache,
    /// Cached `sites` table. Admission and per-site token resolution read this
    /// instead of hitting the DB — `/collect` is a hot path.
    pub sites: SiteCache,
    /// `true` when no `ADMIN_TOKEN` is configured *and* no tenants are
    /// registered: the legacy unconfigured-deploy mode where reads are open and
    /// writes are refused.
    ///
    /// Decided once at startup and never recomputed from the live registry. If
    /// it were derived per request from `registry.is_empty()`, a transient DB
    /// failure that emptied the cache would silently flip a locked-down deploy
    /// to world-readable.
    pub open_mode: bool,
    /// SHA-256 of the global `ADMIN_TOKEN`, precomputed so the per-request cost
    /// is a 32-byte compare rather than a fresh digest.
    pub admin_token_hash: Option<[u8; 32]>,
}
