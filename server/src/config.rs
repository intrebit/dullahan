//! Environment-driven configuration: env vars parsed into a typed `Config`.

use std::env;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: String,
    pub database_url: String,
    /// Gates the admin surface. When set, `/stats/*` and the gated blog operations
    /// require `Authorization: Bearer <token>`. When unset, `/stats/*` and blog
    /// reads are open (fine on a trusted network, dangerous on the public internet
    /// — the server warns at startup), but the blog *write* endpoints
    /// (`POST`/`PATCH`/`DELETE /posts`) are refused entirely until a token is
    /// configured: destructive operations are secure by default.
    pub admin_token: Option<String>,
    pub email: Option<EmailConfig>,
    /// Allowed `Origin` values for `/stats/*`. Empty/unset = `*`. Set this
    /// to your dashboard origin (e.g. `https://stats.example.com`) so a
    /// browser on any other origin can't read stats responses even if the
    /// admin token leaks into URL bar / page source.
    pub stats_origins: Option<Vec<String>>,
    /// Allowed `Origin` values for public product reads (`GET /products` and the
    /// `/products/:slug/view` ping) so a storefront on another origin can fetch
    /// the catalog from the browser. Empty/unset = `*`: the published catalog is
    /// public, so open by default (like `/collect`). Set to your storefront
    /// origin(s) to restrict. Admin writes stay token-gated regardless.
    pub product_origins: Option<Vec<String>>,
    /// `true` if the server is fronted by HTTPS (so HSTS is safe to send).
    /// The header is harmless on plain HTTP but pointless. Default false.
    pub behind_tls: bool,
    /// Trust proxy-populated client IP headers (`x-forwarded-for`, `x-real-ip`)
    /// for rate limiting and session hashing. Default false: use the TCP peer
    /// only, so direct public deploys are not vulnerable to spoofed headers.
    pub trust_proxy_headers: bool,
    /// Opt-in anonymized sessions (rung 2). When `true`, `/collect` uses the
    /// selected client IP + User-Agent to derive a salted daily visitor hash
    /// (raw IP never stored) and coarse browser/OS family. Default false:
    /// outside transient rate-limiter keying, no IP/UA analytics are derived.
    pub sessions_enabled: bool,
    /// Delete `analytics_events` rows older than this many days. Unset (or `0`)
    /// keeps events forever, which is the historical behaviour — every other
    /// table can shrink, so this was the one unbounded one. Note the privacy
    /// floor is independent of this: `daily_salts` is pruned regardless, so old
    /// visitor hashes are already unlinkable whether or not events are kept.
    pub retention_days: Option<u32>,
    /// ISO-4217 currency for the product catalog (`/products`). Prices are
    /// stored as integer minor units (`price_cents`) with no per-product
    /// currency; this is the single shop-wide code echoed in each response so
    /// the frontend can format. Default `EUR`. See `SHOP_CURRENCY`.
    pub shop_currency: String,
    /// Where `--selfcheck` mails its findings. This is the *operator's* address,
    /// not a tenant's: the alerts are about the process and the host, not about
    /// any one site. Unset disables alert email (the checks still run and log).
    pub alert_to: Option<String>,
    /// Disk-usage percentage at or above which `--selfcheck` raises a finding.
    pub alert_disk_percent: u8,
    /// Hours before `--selfcheck` re-mails about a problem that is still present.
    /// Without this a ten-minute timer would send 144 identical emails a day.
    pub alert_repeat_hours: u32,
    /// Where `dullahan-backup` writes its runs. `--selfcheck` reads this to notice
    /// that the nightly backup has stopped producing artifacts.
    pub backup_dir: String,
    /// Age at which the newest backup counts as stale. Default 48h — one missed
    /// nightly run plus grace. `0` disables the check.
    pub alert_backup_max_age_hours: u32,
    /// Optional external dead-man's-switch URL (e.g. healthchecks.io).
    /// `--selfcheck` pings it on success and `<url>/fail` on failure. Unset by
    /// default. Worth adding eventually: it is the only signal that survives the
    /// host itself going down, which nothing running on the host can report.
    pub healthcheck_url: Option<String>,
    /// Where `--selfcheck` keeps counter baselines and alert throttles between
    /// runs. Relative paths resolve against the unit's `WorkingDirectory`.
    pub selfcheck_state_path: String,
}

#[derive(Clone, Debug)]
pub struct EmailConfig {
    pub resend_api_key: String,
    pub from: String,
    pub from_name: String,
    pub timeout: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing required env var: {0}")]
    Missing(&'static str),
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let database_url =
            env::var("DATABASE_URL").map_err(|_| ConfigError::Missing("DATABASE_URL"))?;

        let bind_addr = env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3001".into());

        let admin_token = env::var("ADMIN_TOKEN").ok().and_then(|s| {
            let s = s.trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        });

        let email = match (env::var("RESEND_API_KEY"), env::var("EMAIL_FROM")) {
            (Ok(api_key), Ok(from)) if !api_key.is_empty() && !from.is_empty() => {
                Some(EmailConfig {
                    resend_api_key: api_key,
                    from,
                    from_name: env::var("EMAIL_FROM_NAME").unwrap_or_else(|_| "dullahan".into()),
                    timeout: Duration::from_secs(10),
                })
            }
            _ => None,
        };

        let stats_origins = env::var("STATS_ORIGINS").ok().map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        });

        let product_origins = env::var("PRODUCT_ORIGINS").ok().map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        });

        let behind_tls = env::var("BEHIND_TLS")
            .ok()
            .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);

        let trust_proxy_headers = env::var("TRUST_PROXY_HEADERS")
            .ok()
            .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);

        let sessions_enabled = env::var("SESSIONS_ENABLED")
            .ok()
            .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);

        // A malformed or zero value means "keep everything" rather than an
        // error: a typo here must not stop the server from booting, and the
        // safe reading of an unparseable retention is to delete nothing.
        let retention_days = env::var("RETENTION_DAYS")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .filter(|d| *d > 0);

        let shop_currency = env::var("SHOP_CURRENCY")
            .ok()
            .map(|s| s.trim().to_ascii_uppercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "EUR".into());

        let alert_to = env::var("ALERT_TO").ok().and_then(|s| {
            let s = s.trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        });

        // Out-of-range or unparseable values fall back to the default rather than
        // failing: these gate an *alerting* path, and a typo must not be the
        // reason monitoring silently never ran.
        let alert_disk_percent = env::var("ALERT_DISK_PERCENT")
            .ok()
            .and_then(|s| s.trim().parse::<u8>().ok())
            .filter(|p| (1..=100).contains(p))
            .unwrap_or(85);

        let alert_repeat_hours = env::var("ALERT_REPEAT_HOURS")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(6);

        let backup_dir = env::var("BACKUP_DIR")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/var/backups/dullahan".into());

        let alert_backup_max_age_hours = env::var("ALERT_BACKUP_MAX_AGE_HOURS")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(48);

        let healthcheck_url = env::var("HEALTHCHECK_URL").ok().and_then(|s| {
            let s = s.trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        });

        let selfcheck_state_path = env::var("SELFCHECK_STATE_PATH")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "selfcheck-state.json".into());

        Ok(Self {
            bind_addr,
            database_url,
            admin_token,
            email,
            stats_origins,
            product_origins,
            behind_tls,
            trust_proxy_headers,
            sessions_enabled,
            retention_days,
            shop_currency,
            alert_to,
            alert_disk_percent,
            alert_repeat_hours,
            backup_dir,
            alert_backup_max_age_hours,
            healthcheck_url,
            selfcheck_state_path,
        })
    }
}

/// Test/default config. Exists so the integration test files can write
/// `Config { admin_token: .., ..Default::default() }` instead of repeating every
/// field — previously any field addition here meant editing three test files.
impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:0".into(),
            database_url: String::new(),
            admin_token: None,
            email: None,
            stats_origins: None,
            product_origins: None,
            behind_tls: false,
            trust_proxy_headers: false,
            sessions_enabled: false,
            retention_days: None,
            shop_currency: "EUR".into(),
            alert_to: None,
            alert_disk_percent: 85,
            alert_repeat_hours: 6,
            backup_dir: "/var/backups/dullahan".into(),
            alert_backup_max_age_hours: 48,
            healthcheck_url: None,
            selfcheck_state_path: "selfcheck-state.json".into(),
        }
    }
}
