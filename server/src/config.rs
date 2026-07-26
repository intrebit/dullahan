//! Environment-driven configuration: env vars parsed into a typed `Config`.

use std::collections::HashMap;
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
    /// Default recipient for `POST /contact` submissions that name no tenant.
    /// Required for the endpoint to accept; without it the route returns 503 so
    /// misconfigured deploys fail loudly instead of silently dropping form
    /// submissions.
    pub contact_to: Option<String>,
    /// Per-tenant `/contact` recipients from `CONTACT_TO_<SITE>` env vars, keyed
    /// by normalized site id (`CONTACT_TO_MY_SITE` → `my_site`). A submission naming a
    /// site with no entry here is refused rather than delivered to
    /// `contact_to` — one tenant's enquiries must never land in another's inbox.
    pub contact_to_sites: HashMap<String, String>,
    /// Allowed `Origin` values for `/stats/*`. Empty/unset = `*`. Set this
    /// to your dashboard origin (e.g. `https://stats.example.com`) so a
    /// browser on any other origin can't read stats responses even if the
    /// admin token leaks into URL bar / page source.
    pub stats_origins: Option<Vec<String>>,
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
            allowed_sites,
            admin_token,
            email,
            contact_to,
            contact_to_sites: contact_sites_from(env::vars()),
            stats_origins,
            behind_tls,
            trust_proxy_headers,
            sessions_enabled,
            shop_currency,
        })
    }

    /// Recipient for a `/contact` submission. `None` means "refuse": a tenant
    /// with no configured recipient must get a 503, not another tenant's inbox.
    pub fn contact_recipient(&self, site: Option<&str>) -> Option<&str> {
        match site {
            Some(site) => self
                .contact_to_sites
                .get(&normalize_site(site))
                .map(String::as_str),
            None => self.contact_to.as_deref(),
        }
    }
}

/// Site ids are lowercase and may contain `-`, which env var names can't, so
/// both sides normalize to lowercase with non-alphanumerics as `_`
/// (`CONTACT_TO_MY_SITE` matches site `my-site`).
fn normalize_site(site: &str) -> String {
    site.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn contact_sites_from(vars: impl Iterator<Item = (String, String)>) -> HashMap<String, String> {
    vars.filter_map(|(key, value)| {
        let site = key.strip_prefix("CONTACT_TO_")?;
        let value = value.trim();
        if site.is_empty() || value.is_empty() {
            return None;
        }
        Some((normalize_site(site), value.to_string()))
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(default_to: Option<&str>, sites: &[(&str, &str)]) -> Config {
        Config {
            bind_addr: String::new(),
            database_url: String::new(),
            allowed_sites: None,
            admin_token: None,
            email: None,
            contact_to: default_to.map(String::from),
            contact_to_sites: sites
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            stats_origins: None,
            behind_tls: false,
            trust_proxy_headers: false,
            sessions_enabled: false,
            shop_currency: "EUR".into(),
        }
    }

    fn vars(pairs: &[(&str, &str)]) -> impl Iterator<Item = (String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn env_prefix_builds_the_tenant_map() {
        let map = contact_sites_from(vars(&[
            ("CONTACT_TO", "default@example.com"),
            ("CONTACT_TO_ACME", "forms@acme.test"),
            ("CONTACT_TO_MY_SITE", "hi@my-site.com"),
            ("CONTACT_TO_BLANK", "  "),
            ("CONTACT_TO_", "orphan@example.com"),
            ("DATABASE_URL", "postgres://x"),
        ]));

        assert_eq!(map.get("acme").map(String::as_str), Some("forms@acme.test"));
        assert_eq!(
            map.get("my_site").map(String::as_str),
            Some("hi@my-site.com")
        );
        assert_eq!(map.len(), 2, "blank values and bare CONTACT_TO are skipped");
    }

    #[test]
    fn tenant_gets_its_own_recipient() {
        let cfg = config(Some("default@example.com"), &[("acme", "forms@acme.test")]);
        assert_eq!(cfg.contact_recipient(Some("acme")), Some("forms@acme.test"));
    }

    #[test]
    fn unknown_tenant_never_falls_back_to_the_default() {
        let cfg = config(Some("default@example.com"), &[("acme", "forms@acme.test")]);
        assert_eq!(cfg.contact_recipient(Some("someone-else")), None);
    }

    #[test]
    fn no_tenant_named_uses_the_default() {
        let cfg = config(Some("default@example.com"), &[("acme", "forms@acme.test")]);
        assert_eq!(cfg.contact_recipient(None), Some("default@example.com"));
        assert_eq!(config(None, &[]).contact_recipient(None), None);
    }

    #[test]
    fn site_lookup_normalizes_case_and_dashes() {
        let cfg = config(None, &[("my_site", "hi@my-site.com")]);
        for site in ["my-site", "My-Site", "MY_SITE", "my.site"] {
            assert_eq!(
                cfg.contact_recipient(Some(site)),
                Some("hi@my-site.com"),
                "site {site} should resolve"
            );
        }
    }
}
