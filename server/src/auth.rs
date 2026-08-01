//! Authorization: what an incoming credential is allowed to do, and to which
//! tenant.
//!
//! Replaces the `is_admin` / `is_admin_strict` boolean pair. The distinction
//! those two encoded — lenient "what may this caller *see*" versus strict "may
//! this caller *mutate*" — survives as two methods on [`Scope`], but both now
//! take the site being acted on, because in a multi-tenant server "is this
//! caller an admin" is no longer a complete question.

use crate::sites;
use crate::state::AppState;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use std::sync::Arc;

/// What an incoming credential may do.
///
/// [`Scope::Unconfigured`] gets its own variant rather than being a `bool`
/// alongside the others so that every `match` is forced to consider it and
/// `grep Unconfigured` finds every place open-mode leaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// No `ADMIN_TOKEN` and no registered tenants: the legacy unconfigured
    /// deploy. Reads are open, writes are refused. See [`AppState::open_mode`].
    Unconfigured,
    /// The global `ADMIN_TOKEN`. Every site, plus the `/sites` registry.
    Operator,
    /// A per-site token matched in the registry. Exactly one site.
    Site(Arc<str>),
    /// No credential, an unrecognized one, or a suspended site's.
    Anonymous,
}

impl Scope {
    /// May this caller see unpublished content for `site`? Lenient — replaces
    /// `is_admin` at the call sites that decide *what to show*.
    pub fn can_read_private(&self, site: &str) -> bool {
        match self {
            Scope::Unconfigured | Scope::Operator => true,
            Scope::Site(own) => &**own == site,
            Scope::Anonymous => false,
        }
    }

    /// May this caller mutate `site`'s content? Strict — an unconfigured deploy
    /// is *not* writable, preserving the "secure by default" property that
    /// `is_admin_strict` existed for.
    pub fn can_write(&self, site: &str) -> bool {
        match self {
            Scope::Operator => true,
            Scope::Site(own) => &**own == site,
            Scope::Unconfigured | Scope::Anonymous => false,
        }
    }

    /// The `/sites` registry surface. Operator only, always — a tenant that
    /// could mint tenants would collapse the whole model.
    pub fn is_operator(&self) -> bool {
        matches!(self, Scope::Operator)
    }

    /// May this caller *name* `site` at all? Answered purely from the caller's
    /// own credential, deliberately without consulting the registry, so a 403
    /// is identical whether the named site belongs to another tenant or does not
    /// exist. No existence oracle.
    pub fn covers(&self, site: &str) -> bool {
        match self {
            Scope::Unconfigured | Scope::Operator | Scope::Anonymous => true,
            Scope::Site(own) => &**own == site,
        }
    }

    /// `true` for anything that presented no usable credential.
    pub fn is_anonymous(&self) -> bool {
        matches!(self, Scope::Anonymous)
    }
}

/// The bearer token, if the header is present and well-formed. Case-sensitive
/// `Bearer ` prefix, matching the previous implementation.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Map an incoming request's `Authorization` header to a [`Scope`]. Never
/// touches the database; one SHA-256 plus at most two map/array compares.
pub fn resolve_scope(state: &AppState, headers: &HeaderMap) -> Scope {
    // Open mode short-circuits before the header is even read, matching the old
    // `is_admin`: an unconfigured deploy returns Unconfigured even when a bogus
    // token is presented.
    if state.open_mode {
        return Scope::Unconfigured;
    }

    let Some(token) = bearer(headers) else {
        return Scope::Anonymous;
    };

    // Reject implausible lengths before hashing so a caller can't spend our CPU.
    if token.len() > 512 {
        return Scope::Anonymous;
    }

    let presented = sites::token_digest(token);

    // Operator is checked first, so a token that is somehow both resolves to the
    // stronger scope.
    if let Some(expected) = state.admin_token_hash
        && ct_eq_32(&presented, &expected)
    {
        return Scope::Operator;
    }

    match sites::snapshot(&state.sites).site_for_token(&presented) {
        Some(id) => Scope::Site(id),
        None => Scope::Anonymous,
    }
}

/// Constant-time compare of two fixed-width digests.
///
/// Callers hash first — the expected side is hashed once at startup rather than
/// per request. Hashing both sides is what keeps this from leaking either the
/// contents or the *length* of the expected token (an earlier version compared
/// raw bytes and returned early on a length mismatch, revealing the token
/// length via timing). The fixed-width signature makes that a type property
/// rather than a comment.
fn ct_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// The tenant a request targets, plus the caller's scope.
///
/// Obtaining one proves the caller is entitled to *name* this site; the
/// per-endpoint predicate ([`Scope::can_read_private`] / [`Scope::can_write`])
/// then decides what they may do with it. Handlers take this instead of reading
/// `site` from their own query struct, so a handler that forgets to authorize
/// cannot compile — there is no other way to obtain a site to pass to the DB
/// layer.
#[derive(Clone, Debug)]
pub struct SiteScope {
    pub site: String,
    pub scope: Scope,
}

#[axum::async_trait]
impl<S> FromRequestParts<S> for SiteScope
where
    AppState: axum::extract::FromRef<S>,
    S: Send + Sync,
{
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let State(app): State<AppState> = State::from_request_parts(parts, state)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let scope = resolve_scope(&app, &parts.headers);

        let Query(q) = Query::<sites::SiteQuery>::from_request_parts(parts, state)
            .await
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        let site = q.site.trim().to_string();

        if !sites::site_valid(&site) {
            return Err(StatusCode::BAD_REQUEST);
        }

        // Credential check first, registry second — so a wrong-tenant site and a
        // nonexistent one are indistinguishable.
        if !scope.covers(&site) {
            return Err(StatusCode::FORBIDDEN);
        }
        sites::check(&app, &site)?;

        Ok(SiteScope { site, scope })
    }
}

/// Coarse gate for `/stats/*`: reject callers with no usable credential before
/// the handler runs. The per-site decision happens in [`SiteScope`], which the
/// handlers extract — this layer only answers "is there *a* credential", which
/// is all it can see before the query string is parsed.
pub async fn require_authenticated(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<axum::response::Response, StatusCode> {
    if resolve_scope(&state, &headers).is_anonymous() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

/// Gate for the `/sites` registry surface: operator only.
pub async fn require_operator(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<axum::response::Response, StatusCode> {
    match resolve_scope(&state, &headers) {
        Scope::Operator => Ok(next.run(request).await),
        Scope::Anonymous | Scope::Unconfigured => Err(StatusCode::UNAUTHORIZED),
        Scope::Site(_) => Err(StatusCode::FORBIDDEN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site_scope(id: &str) -> Scope {
        Scope::Site(Arc::from(id))
    }

    /// The truth table *is* the specification.
    #[test]
    fn scope_truth_table() {
        let cases = [
            (Scope::Unconfigured, true, false, false),
            (Scope::Operator, true, true, true),
            (site_scope("a"), true, true, false),
            (Scope::Anonymous, false, false, false),
        ];
        for (scope, read, write, operator) in cases {
            assert_eq!(scope.can_read_private("a"), read, "read {scope:?}");
            assert_eq!(scope.can_write("a"), write, "write {scope:?}");
            assert_eq!(scope.is_operator(), operator, "operator {scope:?}");
        }
    }

    #[test]
    fn site_scope_is_confined_to_its_own_site() {
        let s = site_scope("a");
        assert!(s.can_read_private("a"));
        assert!(s.can_write("a"));
        assert!(!s.can_read_private("b"), "must not read another tenant");
        assert!(!s.can_write("b"), "must not write another tenant");
        assert!(s.covers("a"));
        assert!(!s.covers("b"));
    }

    #[test]
    fn non_site_scopes_cover_every_site() {
        for scope in [Scope::Unconfigured, Scope::Operator, Scope::Anonymous] {
            assert!(scope.covers("anything"), "{scope:?}");
        }
    }

    #[test]
    fn unconfigured_reads_but_never_writes() {
        let s = Scope::Unconfigured;
        assert!(s.can_read_private("a"), "open mode keeps reads open");
        assert!(!s.can_write("a"), "open mode must refuse writes");
        assert!(!s.is_operator(), "open mode must not reach /sites");
    }

    #[test]
    fn ct_eq_32_matches_only_identical_digests() {
        let a = sites::token_digest("x");
        let b = sites::token_digest("x");
        let c = sites::token_digest("y");
        assert!(ct_eq_32(&a, &b));
        assert!(!ct_eq_32(&a, &c));
    }

    #[test]
    fn bearer_parsing() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None, "absent header");

        h.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer tok".parse().unwrap(),
        );
        assert_eq!(bearer(&h), Some("tok"));

        h.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer  tok  ".parse().unwrap(),
        );
        assert_eq!(bearer(&h), Some("tok"), "surrounding whitespace trimmed");

        h.insert(
            axum::http::header::AUTHORIZATION,
            "Basic tok".parse().unwrap(),
        );
        assert_eq!(bearer(&h), None, "only Bearer is accepted");

        // Pinned as a decision rather than an accident: the prefix match is
        // case-sensitive, as it has always been.
        h.insert(
            axum::http::header::AUTHORIZATION,
            "bearer tok".parse().unwrap(),
        );
        assert_eq!(bearer(&h), None, "prefix match is case-sensitive");

        h.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer   ".parse().unwrap(),
        );
        assert_eq!(bearer(&h), None, "empty token is not a credential");
    }
}
