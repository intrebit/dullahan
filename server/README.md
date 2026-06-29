# dullahan

[![crates.io](https://img.shields.io/crates/v/dullahan.svg)](https://crates.io/crates/dullahan)
[![downloads](https://img.shields.io/crates/d/dullahan.svg)](https://crates.io/crates/dullahan)
[![CI](https://github.com/intrebit/dullahan/actions/workflows/ci.yml/badge.svg)](https://github.com/intrebit/dullahan/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/intrebit/dullahan/blob/master/LICENSE)

A self-hosted, cookie-free backend for small sites — privacy-first analytics, a
headless blog/content API, and a contact endpoint, in a single Rust binary that
serves its own browser tracker. Bring a Postgres; that's the only dependency.

Named for the headless rider of Irish folklore: a backend with no front end of
its own, built because wiring up a tracker, a CMS, and a contact form on every
small site — three services, three bills, three sets of cookies — got old.

## Install

```bash
cargo install dullahan
```

Run it with a database URL and an admin token:

```bash
DATABASE_URL=postgres://user@localhost/dullahan \
ADMIN_TOKEN=$(openssl rand -hex 24) \
dullahan
```

It binds `0.0.0.0:3001`, applies its migrations, and starts serving. Add the
tracker to your site — the script is baked into the binary, no npm:

```html
<script defer src="https://your-host/pt.js" data-site="my-site"></script>
```

> Without `ADMIN_TOKEN`, stats reads and blog reads are **open to anyone**.
> Blog writes are refused until a token is configured. Open reads are fine on a
> trusted network, but a real problem on the public internet — set a token on any
> real deploy. The server logs a warning if you skip it.

A Docker image and a one-shot Debian/Ubuntu VM installer are in the repo.

## What's in the binary

- **Analytics** — `/collect` ingest + a `/stats/*` read API. Cookie-free, no
  fingerprinting, never stores a raw IP. Pageviews, web vitals, time-on-page,
  custom events, funnels, sessions (opt-in).
- **Tracker** — the browser script, baked in and served at `/pt.js`. No npm, no
  build step, no separate package to keep in sync.
- **Blog / content API** — `/posts` CRUD with an atomic per-post view counter.
  Stores raw Markdown; your frontend renders it.
- **Contact** — `/contact` takes a form POST and emails it (via Resend).
- Plus self-applying SQL migrations, per-IP rate limiting, security headers, and
  a Prometheus `/metrics` endpoint.

## Docs

Full quick start, every `/stats/*` endpoint, configuration, the privacy model,
and the self-host hardening checklist live in the repository:
**<https://github.com/intrebit/dullahan>**

License: MIT.
