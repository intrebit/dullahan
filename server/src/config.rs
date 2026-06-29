use std::env;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: String,
    pub database_url: String,
    pub allowed_sites: Option<Vec<String>>,
    /// Gates the admin surface. When set, `/stats/*` and the gated blog operations
    /// require `Authorization: Bearer <token>`. When unset, `/stats/*` and blog
    /// reads are open (fine on a trusted network, dangerous on the public internet
    /// — the server warns at startup), but the blog *write* endpoints
    /// (`POST`/`PATCH`/`DELETE /posts`) are refused entirely until a token is
    /// configured: destructive operations are secure by default.
    pub admin_token: Option<String>,
    pub email: Option<EmailConfig>,
    /// Recipient for `POST /contact` submissions. Required for the endpoint to
    /// accept; without it the route returns 503 so misconfigured deploys
    /// fail loudly instead of silently dropping form submissions.
    pub contact_to: Option<String>,
    /// Allowed `Origin` values for `/stats/*`. Empty/unset = `*`. Set this
    /// to your dashboard origin (e.g. `https://stats.example.com`) so a
    /// browser on any other origin can't read stats responses even if the
    /// admin token leaks into URL bar / page source.
    pub stats_origins: Option<Vec<String>>,
    /// `true` if the server is fronted by HTTPS (so HSTS is safe to send).
    /// The header is harmless on plain HTTP but pointless. Default false.
    pub behind_tls: bool,
    /// Opt-in anonymized sessions (rung 2). When `true`, `/collect` reads the
    /// client IP + User-Agent to derive a salted daily visitor hash (raw IP
    /// never stored) and coarse browser/OS family. Default false — existing
    /// self-hosters process neither IP nor UA unless they turn this on.
    pub sessions_enabled: bool,
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

        let allowed_sites = env::var("ALLOWED_SITES").ok().map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        });

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

        let contact_to = env::var("CONTACT_TO").ok().and_then(|s| {
            let s = s.trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        });

        let stats_origins = env::var("STATS_ORIGINS").ok().map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        });

        let behind_tls = env::var("BEHIND_TLS")
            .ok()
            .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);

        let sessions_enabled = env::var("SESSIONS_ENABLED")
            .ok()
            .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);

        Ok(Self {
            bind_addr,
            database_url,
            allowed_sites,
            admin_token,
            email,
            contact_to,
            stats_origins,
            behind_tls,
            sessions_enabled,
        })
    }
}
