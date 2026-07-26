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

- The Rust server (`server/`) — ingest, stats, contact, auth middleware, anything that could leak user data or break the cookie-free guarantee
- The default deploy scripts (`deploy/`) when used as documented

Out of scope:

- Operator misconfiguration (e.g. running without `ADMIN_TOKEN` on the public internet — the server warns about this at startup)
- Issues in third-party services (Postgres, Caddy, Resend) unless triggered by an unsafe default in dullahan
- Vulnerabilities in old, unsupported versions

## Hardening notes for operators

The checklist lives with the rest of the configuration reference, in
[`deploy.md`](deploy.md#operator-hardening-self-host-checklist): set `ADMIN_TOKEN`
and `ALLOWED_SITES`, scope `STATS_ORIGINS`, turn on `BEHIND_TLS`, and set
`TRUST_PROXY_HEADERS=1` only when a trusted proxy is the sole public peer.

Two properties worth stating here, since they shape the threat model:

- `/collect` and `/contact` are **intentionally unauthenticated** — they take input from browsers. Both are capped at a 16 KB body and rate-limited per IP in-process (`/collect` ~120/min burst 60, `/contact` ~5/min burst 3). `/contact` sends an outbound email per request, so on a hostile deploy layer further limits at the proxy.
- CORS on `/stats/*` is permissive (`*`) but Bearer-gated; lock it to your dashboard origin with `STATS_ORIGINS` if the token could ever reach a browser.

## Known advisories

- **RUSTSEC-2023-0071** (`rsa` Marvin attack) appears in `cargo audit`. `rsa` is pulled transitively via `sqlx-mysql` for `sqlx` compile-time macros. Dullahan enables only the `postgres` feature of `sqlx`, so `rsa` is never linked into the runtime binary. CI passes `--ignore RUSTSEC-2023-0071` for this reason; the ignore will be dropped once upstream `sqlx` no longer pulls `sqlx-mysql` transitively.
