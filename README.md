# Dullahan

[![crates.io](https://img.shields.io/crates/v/dullahan.svg)](https://crates.io/crates/dullahan)
[![docs.rs](https://docs.rs/dullahan/badge.svg)](https://docs.rs/dullahan)
[![CI](https://github.com/intrebit/dullahan/actions/workflows/ci.yml/badge.svg)](https://github.com/intrebit/dullahan/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**The headless backend for your site.** A self-hosted, cookie-free Rust binary that gives a small site three things over plain HTTP: privacy-first **analytics**, a headless **blog/content API**, and a **contact** endpoint. Pure Rust: your frontend's tracker POSTs events to its `/collect` endpoint (the reference tracker ships with the [storefront template](https://github.com/anfocic/storefront)).

One Rust binary + a Postgres replaces a tracking SaaS, a headless CMS, and a contact-form service.

## What you get

- **Analytics:** `/collect` ingest + a `/stats/*` read API: pageviews, unique visitors, bounce rate, time-on-page, top pages/referrers/countries/devices/campaigns, acquisition channels, real-time active visitors, and custom events/goals.
- **Provider-agnostic ingest:** `/collect` accepts a small JSON event shape any browser tracker can POST (open CORS). The reference ~3 KB tracker lives with the frontend, not in this binary.
- **Blog / content API:** `/posts` CRUD with an atomic per-post view counter. Stores raw Markdown; your frontend renders it.
- **Contact:** `/contact` takes a form POST and emails it (via Resend), with a per-site recipient so one server can host several sites' forms.
- **Products:** `/products`, a headless product listing for a simple shop (no cart, no orders), with the same auth model as the blog.
- **Privacy by design:** no cookies, no fingerprinting, **no raw IP storage, ever**.

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

To try it locally with Docker instead: `ADMIN_TOKEN=$(openssl rand -hex 24) docker compose up --build`.

## Deploy on a server

[`deploy/install.sh`](deploy/install.sh) sets up a fresh Debian or Ubuntu box in one run: Postgres, Caddy with automatic HTTPS, the systemd service, nightly encrypted backups, a monthly restore drill, health alerts, and the weekly digest email. Usage is at the top of the script.

## Upgrading from 0.1.x

0.2.0 squashed the schema into a single migration, so it will not start on a database created by 0.1.x (`migration 2 was previously applied but is missing`). Nothing is changed when it refuses. Start 0.2.0 on a fresh database. From 0.2.0 on, migrations are append-only.

## Endpoints

Each site is registered once under `/sites`, and content and stats calls name it with `?site=<id>`. A token is either the global `ADMIN_TOKEN`, which covers every site, or a per-site token, which covers its own site only.

| Endpoint | Who can call it | What it does |
|---|---|---|
| `POST /collect` | anyone | Takes a pageview or custom event and answers 202. Rate limited per IP. |
| `GET /stats/summary`, `/timeseries`, `/top`, `/events`, `/channels`, `/realtime` | token | Reads the analytics. |
| `GET /posts`, `GET /posts/:slug` | anyone | Lists and reads published posts. Drafts need a token. |
| `POST /posts/:slug/view` | anyone | Counts a view. Always answers 204. |
| `POST /posts`, `PATCH /posts/:id`, `DELETE /posts/:id` | token | Writes posts. A slug is unique within its site. |
| `/products`, with the same routes as `/posts` | as for posts | Products, with a price in minor units (cents for EUR) and a sold-out flag. |
| `POST /contact` | anyone | Emails the form to the site's recipient, about 5 a minute per IP. Answers 503 until email and a recipient are set up. |
| `GET /site-config/:site` | anyone | Returns the site's config document, stored as JSON the server does not read. |
| `PUT /site-config/:site`, `DELETE /site-config/:site` | token | Writes or removes it. |
| `GET /site-config` | `ADMIN_TOKEN` only | Lists the sites that have a config. |
| `/sites`, `/sites/:id`, `POST /sites/:id/token` | `ADMIN_TOKEN` only | Registers sites and issues per-site tokens, shown once. Deleting a site answers 409 while it still has content. |
| `GET /health` | anyone | Answers `ok`. |
| `GET /metrics` | anyone | Prometheus metrics. The Caddy config in `deploy/` answers 404 for it: do the same in any other proxy. |

## Privacy

No cookies, no fingerprinting, **no raw IP storage, ever.** The surface is deliberately lean: no web vitals, no scroll depth, and no outbound-link clicks are collected, and the server never parses the User-Agent for analytics (no browser/OS/viewport columns). The server processes the client IP transiently for rate limiting; with sessions **off** (the default), `/collect` otherwise uses neither IP nor User-Agent. With sessions **on** (`SESSIONS_ENABLED=1`), the selected client IP + User-Agent are combined with a daily-rotating salt into an anonymized hash and immediately discarded (the UA is never stored); old salts are pruned on startup and periodically while the server runs, making historical hashes permanently unlinkable after retention. The event rows themselves are kept indefinitely unless you set `RETENTION_DAYS`, which sweeps `analytics_events` older than N days. Consequences embraced on purpose: a cross-day unique count is impossible, so `summary` reports **average daily visitors** rather than an inflated range-wide total, and new-vs-returning / retention / DAU-MAU are intentionally not built.

## Security

Found a vulnerability? Report it privately through [GitHub](https://github.com/intrebit/dullahan/security/advisories/new) or email hello@intrebit.com. Do not open a public issue.

## Who makes it

Dullahan is made by [intrebit](https://intrebit.com), a software studio in Dublin. It runs behind intrebit.com, where it counts the visits and delivers the contact form.

## License

MIT
