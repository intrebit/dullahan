//! Daily-rotating salt for anonymized visitor hashing (rung 2). The salt lives
//! in `daily_salts`, is generated once per UTC day, cached in memory, and
//! deleted after 48h. Once a day's salt is gone, its visitor hashes can never
//! be recomputed or re-linked.

use base64::Engine;
use chrono::NaiveDate;
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPool;
use std::sync::{Arc, Mutex};

pub type SaltCache = Arc<Mutex<Option<(NaiveDate, [u8; 32])>>>;

pub fn new_cache() -> SaltCache {
    Arc::new(Mutex::new(None))
}

/// Today's (UTC) salt, loading or creating it in `daily_salts` on a date
/// change. Hot-path cheap: a cache hit is just a mutex lock. The lock is never
/// held across an await.
pub async fn current_salt(
    pool: &PgPool,
    cache: &SaltCache,
    today: NaiveDate,
) -> sqlx::Result<[u8; 32]> {
    if let Some((day, salt)) = *cache.lock().unwrap()
        && day == today
    {
        return Ok(salt);
    }

    // Generate a candidate and INSERT ... ON CONFLICT DO NOTHING so concurrent
    // workers converge on one salt per day, then SELECT the winner.
    let mut candidate = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut candidate);

    sqlx::query("INSERT INTO daily_salts (day, salt) VALUES ($1, $2) ON CONFLICT (day) DO NOTHING")
        .bind(today)
        .bind(&candidate[..])
        .execute(pool)
        .await?;

    let stored: Vec<u8> = sqlx::query_scalar("SELECT salt FROM daily_salts WHERE day = $1")
        .bind(today)
        .fetch_one(pool)
        .await?;

    let mut salt = [0u8; 32];
    let n = stored.len().min(32);
    salt[..n].copy_from_slice(&stored[..n]);

    // Opportunistic cleanup: keep only today and yesterday, so a rotated salt is
    // gone within ~48h and can never be used to re-link historical hashes.
    let _ = sqlx::query("DELETE FROM daily_salts WHERE day < $1")
        .bind(today - chrono::Duration::days(1))
        .execute(pool)
        .await;

    *cache.lock().unwrap() = Some((today, salt));
    Ok(salt)
}

/// `base64url(sha256(salt ‖ site_id ‖ ip ‖ ua))[..18]`. The site_id binds the
/// hash to one site (no cross-site correlation); the raw IP is never returned
/// or stored. 18 base64 chars ≈ 108 bits — ample to avoid collisions.
pub fn visitor_hash(salt: &[u8; 32], site_id: &str, ip: &str, ua: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(site_id.as_bytes());
    hasher.update(ip.as_bytes());
    hasher.update(ua.as_bytes());
    let digest = hasher.finalize();
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    encoded[..18].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_18_chars() {
        let salt = [7u8; 32];
        let a = visitor_hash(&salt, "site", "1.2.3.4", "ua");
        let b = visitor_hash(&salt, "site", "1.2.3.4", "ua");
        assert_eq!(a, b);
        assert_eq!(a.len(), 18);
    }

    #[test]
    fn hash_changes_with_each_input() {
        let salt = [7u8; 32];
        let base = visitor_hash(&salt, "site", "1.2.3.4", "ua");
        assert_ne!(base, visitor_hash(&[8u8; 32], "site", "1.2.3.4", "ua"));
        assert_ne!(base, visitor_hash(&salt, "other", "1.2.3.4", "ua"));
        assert_ne!(base, visitor_hash(&salt, "site", "5.6.7.8", "ua"));
        assert_ne!(base, visitor_hash(&salt, "site", "1.2.3.4", "other"));
    }
}
