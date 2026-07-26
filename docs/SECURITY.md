# Security policy

## Reporting a vulnerability

If you believe you've found a security issue in dullahan, please **do not** open a public GitHub issue.

Instead, use GitHub's private vulnerability reporting on this repository (Security → Report a vulnerability), or email the maintainer listed in `Cargo.toml` / `package.json`. Include:

- A description of the issue
- Steps to reproduce (or a proof of concept)
- The version / commit you tested against
- The impact you believe it has

You should expect an acknowledgement within a few days. Fixes for confirmed issues are released as soon as practical, with credit if you'd like it.

## Scope

In scope:

- The Rust server (`server/`) — ingest, stats, contact, auth middleware
- The browser tracker (`tracker/`) — anything that could leak user data, bypass DNT, or break the cookie-free guarantee
- The default deploy scripts (`deploy/`) when used as documented

Out of scope:

- Operator misconfiguration (e.g. running without `ADMIN_TOKEN` on the public internet — the server warns about this at startup)
- Issues in third-party services (Postgres, Caddy, Resend) unless triggered by an unsafe default in dullahan
- Vulnerabilities in old, unsupported versions

## Hardening notes for operators

- Always set `ADMIN_TOKEN` when exposing the server to the public internet. Without it, `/stats/*` is readable by anyone.
- Restrict `ALLOWED_SITES` to the site IDs you actually own; otherwise anyone can write events with any `siteId`.
- The `/collect` and `/contact` endpoints are intentionally unauthenticated — they accept input from browsers. The server enforces a 16 KB request-body cap **and** a built-in per-IP rate limit on both (`/collect` ~120/min burst 60, `/contact` ~5/min burst 3). By default the limiter keys on TCP peer IP; set `TRUST_PROXY_HEADERS=1` only behind a trusted proxy to use `x-forwarded-for` / `x-real-ip`. `/contact` triggers an outbound email per request and is a prime abuse target, so for a hostile public deploy you **should** layer additional limits at your reverse proxy too.
- Keep the server behind TLS (Caddy in `deploy/install.sh` does this automatically).
- CORS on `/stats/*` is permissive (`*`) but Bearer-gated. If you only call it from a known backend, lock it down at the reverse proxy.
- `SESSIONS_ENABLED` is **off by default**: outside transient rate-limiter keying, `/collect` does not use the client IP and does not read the User-Agent. Turning it on enables anonymized sessions — the selected client IP and User-Agent are hashed with a daily-rotating salt (raw IP never stored) and old salts are pruned on startup and periodically while the server runs, so historical hashes cannot be re-linked after retention. If you enable it behind a proxy, set `TRUST_PROXY_HEADERS=1` and confirm your privacy policy reflects the change.

## Known advisories

- **RUSTSEC-2023-0071** (`rsa` Marvin attack) appears in `cargo audit`. `rsa` is pulled transitively via `sqlx-mysql` for `sqlx` compile-time macros. Dullahan enables only the `postgres` feature of `sqlx`, so `rsa` is never linked into the runtime binary. CI passes `--ignore RUSTSEC-2023-0071` for this reason; the ignore will be dropped once upstream `sqlx` no longer pulls `sqlx-mysql` transitively.
