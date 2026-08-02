//! Shared application state (`AppState`): config, DB pool, the salt store, and
//! the tenant registry.

use crate::config::Config;
use crate::email::Mailer;
use crate::ingest::IngestSender;
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
    /// Bounded queue into the ingest writer task. `/collect` enqueues here
    /// instead of spawning a task per event, so overload is shed at a known
    /// depth rather than becoming pool-acquire timeouts and lost rows.
    pub ingest_tx: IngestSender,
    /// Cached `sites` table. Admission and per-site token resolution read this
    /// instead of hitting the DB — `/collect` is a hot path.
    pub sites: SiteCache,
    /// `true` when no `ADMIN_TOKEN` is configured: the legacy
    /// unconfigured-deploy mode where reads are open and writes are refused.
    ///
    /// Deliberately *not* also conditioned on the registry being empty. That
    /// looks safer but is self-contradictory: `blog_posts` and `products`
    /// reference `sites`, so a deploy with no tenants can hold no content, and
    /// the extra condition would silently lock an existing single-tenant
    /// operator out of their own drafts the moment they registered a site.
    ///
    /// The property it was meant to buy — open mode and multi-tenancy never
    /// coexisting — is already guaranteed better elsewhere: `/sites` requires
    /// `Operator`, and `Unconfigured` is not `Operator`, so tenants cannot be
    /// created without an `ADMIN_TOKEN` in the first place.
    ///
    /// Decided once at startup and never recomputed from the live registry, so a
    /// transient DB failure that emptied the cache cannot flip a locked-down
    /// deploy to world-readable.
    pub open_mode: bool,
    /// SHA-256 of the global `ADMIN_TOKEN`, precomputed so the per-request cost
    /// is a 32-byte compare rather than a fresh digest.
    pub admin_token_hash: Option<[u8; 32]>,
}
