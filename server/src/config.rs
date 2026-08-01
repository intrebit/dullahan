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
    /// ISO-4217 currency for the product catalog (`/products`). Prices are
    /// stored as integer minor units (`price_cents`) with no per-product
    /// currency; this is the single shop-wide code echoed in each response so
    /// the frontend can format. Default `EUR`. See `SHOP_CURRENCY`.
    pub shop_currency: String,
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

        let shop_currency = env::var("SHOP_CURRENCY")
            .ok()
            .map(|s| s.trim().to_ascii_uppercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "EUR".into());

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
            shop_currency,
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
            shop_currency: "EUR".into(),
        }
    }
}
