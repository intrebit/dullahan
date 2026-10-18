# dullahan

[![crates.io](https://img.shields.io/crates/v/dullahan.svg)](https://crates.io/crates/dullahan)
[![downloads](https://img.shields.io/crates/d/dullahan.svg)](https://crates.io/crates/dullahan)
[![CI](https://github.com/intrebit/dullahan/actions/workflows/ci.yml/badge.svg)](https://github.com/intrebit/dullahan/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/intrebit/dullahan/blob/master/LICENSE)

Self-hosted, cookie-free backend for small sites: privacy-first analytics, a
blog and product API, and contact forms. One Rust binary plus Postgres.

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

It binds `0.0.0.0:3001`, applies its migrations, and starts serving. Point your
site's browser tracker at its `/collect` endpoint — the tracker ships with your
frontend (e.g. the storefront template), not with this binary.

> Without `ADMIN_TOKEN`, stats reads and blog reads are **open to anyone**.
> Blog writes are refused until a token is configured. Open reads are fine on a
> trusted network, but a real problem on the public internet — set a token on any
> real deploy. The server logs a warning if you skip it.

A Docker image and a one-shot Debian/Ubuntu VM installer are in the repo.

> **Upgrading from 0.1.x:** 0.2.0 squashed the schema, so it will not start on a
> 0.1.x database. It refuses without changing anything; start it on a fresh
> database.

## What's in the binary

- **Analytics** — `/collect` ingest + a `/stats/*` read API. Cookie-free, no
  fingerprinting, never stores a raw IP. Pageviews, unique visitors, bounce,
  time-on-page, top pages/referrers/countries/devices/campaigns, channels,
  realtime, and custom events (sessions opt-in).
- **Blog / content API** — `/posts` CRUD with an atomic per-post view counter.
  Stores raw Markdown; your frontend renders it.
- **Contact** — `/contact` takes a form POST and emails it (via Resend).
- **Products** — `/products`, a headless product listing for a simple shop, with
  the same auth model as the blog.
- Plus self-applying SQL migrations, per-IP rate limiting, security headers, and
  a Prometheus `/metrics` endpoint.

## Docs

The quick start, every endpoint and the privacy model are in the repository's
README: **<https://github.com/intrebit/dullahan#readme>**

License: MIT.
