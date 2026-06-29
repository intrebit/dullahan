# Dullahan

[![crates.io](https://img.shields.io/crates/v/dullahan.svg)](https://crates.io/crates/dullahan)
[![docs.rs](https://docs.rs/dullahan/badge.svg)](https://docs.rs/dullahan)
[![CI](https://github.com/intrebit/dullahan/actions/workflows/ci.yml/badge.svg)](https://github.com/intrebit/dullahan/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**The headless backend for your site.** A self-hosted, cookie-free Rust binary that gives a small site three things over plain HTTP: privacy-first **analytics**, a headless **blog/content API**, and a **contact** endpoint. It serves its own browser tracker at `/pt.js` — no separate package to install.

One Rust binary + a Postgres replaces a tracking SaaS, a headless CMS, and a contact-form service.

## What you get

- **Analytics** — `/collect` ingest + a `/stats/*` read API: pageviews, web vitals, time-on-page, custom events, channels, engagement, sessions, funnels.
- **Tracker** — a ~3 KB browser script, baked into the binary and served at `/pt.js`. One `<script>` tag, no npm, no build step.
- **Blog / content API** — `/posts` CRUD with an atomic per-post view counter. Stores raw Markdown; your frontend renders it.
- **Contact** — `/contact` takes a form POST and emails it (via Resend).
- **Privacy by design** — no cookies, no fingerprinting, **no raw IP storage, ever**.

## Quick start

```bash
cargo install dullahan

DATABASE_URL=postgres://user@localhost/dullahan \
ADMIN_TOKEN=$(openssl rand -hex 24) \
dullahan
```

Migrations apply on startup; the server binds `0.0.0.0:3001`. Add the tracker to your site:

```html
<script defer src="https://analytics.example.com/pt.js" data-site="my-site"></script>
```

Then read your stats with the token:

```bash
curl -H "Authorization: Bearer $ADMIN_TOKEN" \
  "https://analytics.example.com/stats/summary?site=my-site&days=30"
```

> **Set `ADMIN_TOKEN` on any public deploy.** Without it, `/stats/*` and blog reads are open to anyone (blog writes are refused). The server logs a warning when it's unset.

The full copy-paste walkthrough — tracker opt-ins, custom events, blog, contact — is in **[`examples/QUICKSTART.md`](examples/QUICKSTART.md)**.

## Privacy

No cookies, no fingerprinting, **no raw IP storage — ever.** With sessions **off** (the default) the server reads neither the client IP nor the User-Agent. With sessions **on** (`SESSIONS_ENABLED=1`), the IP + User-Agent are combined with a daily-rotating salt into an anonymized hash and immediately discarded; the salt is deleted after 48h, making old hashes permanently unlinkable. Consequences embraced on purpose: `uniqueVisitors` counts visitor-*days*, sessions can't cross 00:00 UTC, and new-vs-returning / retention / DAU-MAU are impossible and intentionally not built.

## Documentation

| Doc | What |
|---|---|
| [`docs/overview.md`](docs/overview.md) | Architecture + feature tour |
| [`examples/QUICKSTART.md`](examples/QUICKSTART.md) | End-to-end walkthrough (install → first stats) |
| [`docs/api.md`](docs/api.md) | Full HTTP API reference — `/stats/*`, blog, what's collected |
| [`docs/deploy.md`](docs/deploy.md) | Configuration, self-host hardening, metrics, load testing |
| [`AGENTS.md`](AGENTS.md) | Developer guide (build/test/lint, conventions, gotchas) |

## Security

Found a vulnerability? Please report it privately — see [`docs/SECURITY.md`](docs/SECURITY.md). Do not open a public issue.

## License

MIT
