# Dullahan

[![crates.io](https://img.shields.io/crates/v/dullahan.svg)](https://crates.io/crates/dullahan)
[![docs.rs](https://docs.rs/dullahan/badge.svg)](https://docs.rs/dullahan)
[![CI](https://github.com/intrebit/dullahan/actions/workflows/ci.yml/badge.svg)](https://github.com/intrebit/dullahan/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**The headless backend for your site.** A self-hosted, cookie-free Rust binary that gives a small site three things over plain HTTP: privacy-first **analytics**, a headless **blog/content API**, and a **contact** endpoint. Pure Rust — your frontend's tracker POSTs events to its `/collect` endpoint (the reference tracker ships with the [storefront template](https://github.com/anfocic/storefront)).

One Rust binary + a Postgres replaces a tracking SaaS, a headless CMS, and a contact-form service.

## What you get

- **Analytics** — `/collect` ingest + a `/stats/*` read API: pageviews, unique visitors, bounce rate, time-on-page, top pages/referrers/countries/devices/campaigns, acquisition channels, real-time active visitors, and custom events/goals.
- **Provider-agnostic ingest** — `/collect` accepts a small JSON event shape any browser tracker can POST (open CORS). The reference ~3 KB tracker lives with the frontend, not in this binary.
- **Blog / content API** — `/posts` CRUD with an atomic per-post view counter. Stores raw Markdown; your frontend renders it.
- **Contact** — `/contact` takes a form POST and emails it (via Resend), with a per-site recipient so one server can host several sites' forms.
- **Privacy by design** — no cookies, no fingerprinting, **no raw IP storage, ever**.

```
 Browser (your site)                      Your server (self-hosted)
 ┌─────────────────────┐   POST /collect  ┌───────────────────────────┐
 │  dullahan tracker   │ ───────────────▶ │  Axum ingest (fire-and-   │   ┌──────────┐
 │  (~3 KB gz, TS)     │   202 Accepted   │  forget write)            │──▶│ Postgres │
 └─────────────────────┘                  │                           │   │ analytics│
                                          │  /stats/* read API        │◀──│ _events  │
 Dashboard / curl ───── Bearer token ───▶ │  (admin-gated, CORS)      │   └──────────┘
                                          └───────────────────────────┘
```

## Quick start

```bash
cargo install dullahan

DATABASE_URL=postgres://user@localhost/dullahan \
ADMIN_TOKEN=$(openssl rand -hex 24) \
dullahan
```

Migrations apply on startup; the server binds `0.0.0.0:3001`. Point your site's tracker at this server's `/collect` (the [storefront template](https://github.com/anfocic/storefront) does this from config), then read your stats with the token:

```bash
curl -H "Authorization: Bearer $ADMIN_TOKEN" \
  "https://analytics.example.com/stats/summary?site=my-site&days=30"
```

> **Set `ADMIN_TOKEN` on any public deploy.** Without it, `/stats/*` and blog reads are open to anyone (blog writes are refused). The server logs a warning when it's unset.

Every endpoint — stats, blog, contact, `/collect` — is in [`docs/api.md`](docs/api.md).

## Privacy

No cookies, no fingerprinting, **no raw IP storage — ever.** The surface is deliberately lean: no web vitals, no scroll depth, and no outbound-link clicks are collected, and the server never parses the User-Agent for analytics (no browser/OS/viewport columns). The server processes the client IP transiently for rate limiting; with sessions **off** (the default), `/collect` otherwise uses neither IP nor User-Agent. With sessions **on** (`SESSIONS_ENABLED=1`), the selected client IP + User-Agent are combined with a daily-rotating salt into an anonymized hash and immediately discarded (the UA is never stored); old salts are pruned on startup and periodically while the server runs, making historical hashes permanently unlinkable after retention. Consequences embraced on purpose: a cross-day unique count is impossible, so `summary` reports **average daily visitors** rather than an inflated range-wide total, and new-vs-returning / retention / DAU-MAU are intentionally not built.

## Documentation

| Doc | What |
|---|---|
| [`docs/api.md`](docs/api.md) | Full HTTP API reference — `/stats/*`, blog, `/collect`, what's collected |
| [`docs/deploy.md`](docs/deploy.md) | Configuration, self-host hardening, metrics, load testing |
| [`AGENTS.md`](AGENTS.md) | Developer guide (build/test/lint, conventions, gotchas) |

## Security

Found a vulnerability? Please report it privately — see [`docs/SECURITY.md`](docs/SECURITY.md). Do not open a public issue.

## License

MIT
