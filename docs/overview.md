# dullahan — what's in the app today

A privacy-first, cookie-free web analytics product in two halves: a tiny browser
SDK that collects events, and a self-hostable Rust server that ingests them and
serves an admin-only read API. No cookies, no fingerprinting, no raw IP storage.

This is a state-of-the-app tour. For install/usage see [`README.md`](../README.md);
for how to work on the code see [`AGENTS.md`](../AGENTS.md).

## Shape

```
 Browser (your site)                      Your server (self-hosted)
 ┌─────────────────────┐   POST /collect  ┌───────────────────────────┐
 │  dullahan tracker  │ ───────────────▶ │  Axum ingest (fire-and-    │   ┌──────────┐
 │  (~3 KB gz, TS)     │   202 Accepted   │  forget write)            │──▶│ Postgres │
 └─────────────────────┘                  │                           │   │ analytics│
                                          │  /stats/* read API        │◀──│ _events  │
 Dashboard / curl ───── Bearer token ───▶ │  (admin-gated, CORS)      │   └──────────┘
                                          └───────────────────────────┘
```

- **Tracker** (`tracker/`, npm `dullahan`): auto-tracks pageviews (incl. SPA
  navigations), web vitals, and visible time-on-page; optional opt-ins for scroll
  depth and outbound/download clicks; a `track()` API for custom events. Batches
  nothing sensitive — strips query/hash from URLs, rounds viewport, coarsens
  device. Guards against double-init and prerender double-counts.
- **Server** (`server/`, crate `dullahan`): Axum + sqlx + Postgres.
  `/collect` ingest (rate-limited, validated, fire-and-forget insert), `/contact`,
  `/health`, Prometheus `/metrics`, and the `/stats/*` read API. Migrations apply
  automatically on startup.

## What gets collected

| Always | Opt-in (`SESSIONS_ENABLED`) |
|---|---|
| Pageviews (path, referrer host, device class, viewport bucket, country, UTM) | Anonymized visitor hash → unique visitors, sessions, bounce |
| Custom events (`track`) + opt-in scroll-depth / outbound-click events | Browser + OS *family* (never versions) |
| Web vitals (LCP, FCP, CLS, INP, TTFB) | |
| Time on page (visible only, capped 30 min) | |

Two identifiers, two grains:
- **`view_id`** — one page-visit (regenerated every pageview/SPA-nav); always present.
- **`visitor_hash`** — one visitor per UTC day; opt-in; basis for sessions.

## The read API (`/stats/*`, admin-gated)

All endpoints take `site` + `days`; responses are JSON. Complete catalog:

| Endpoint | Returns |
|---|---|
| `summary` | pageviews, events, top path, time-on-page (avg/median/p75); `+compare=prev` adds previous window + % change; `uniqueVisitors`/`bounceRate` with sessions on |
| `timeseries` | pageviews per day/hour bucket; per-bucket `uniqueVisitors` with sessions on |
| `top?dim=` | top values for path, referrer, country, device, viewport, utm_*, (sessions) browser/os; path rows carry avg/median time |
| `events` | top custom-event names; `name=&by=` gives one event's prop-value distribution |
| `vitals` | site-wide p75 + Core-Web-Vitals pass-rate buckets; `dim=path` → per-path p75 with per-metric sample counts |
| `heatmap` | pageviews by ISO weekday × hour, in an optional IANA `tz` |
| `channels` | pageviews grouped into Direct / Organic / Social / Paid / Campaign / Referral |
| `realtime` | active page-visits in the last `minutes` (server receive-time) + top active pages |
| `engagement` | engaged-visit rate, events/visit, scroll reach + funnel, outbound rate; `dim=path` per page |
| `sessions` | sessions count, pages/session, duration, session bounce; `dim=entry\|exit` top pages |
| `funnel` | ordered path funnel — sessions reaching each `step` + conversion rates |

Design conventions that run through all of it:
- **Additive** — new fields/params/endpoints only; the dashboard never breaks.
- **Honest nulls** — a metric whose source is an opt-in (sessions, scroll/outbound
  tracking) is *omitted*, never shown as a misleading `0`.
- **Dual-shape** — summary object with no `dim`, array with `dim=…`.

## Privacy model

- No cookies, no fingerprinting. **Raw IPs are never stored.** With sessions off
  (default) the server reads neither IP nor User-Agent.
- With sessions on, `(daily salt, site, IP, UA)` → a hash; the IP is discarded
  immediately and the **salt is deleted after 48h**, making old hashes permanently
  unlinkable.
- Consequences embraced on purpose: `uniqueVisitors` counts *visitor-days*;
  sessions can't cross **00:00 UTC**; and **new-vs-returning, retention, DAU/MAU
  are impossible — and intentionally not built.**

## Run & deploy

- **Local server:** `DATABASE_URL=… ADMIN_TOKEN=… cargo run --release` (migrations
  auto-apply).
- **Embed:** add the server-served `/pt.js` script tag; the tracker is vendored
  into the Rust binary and is not published as a separate npm package.
- **Self-host:** `deploy/` has a one-shot `install.sh`, a systemd unit, a
  `Caddyfile`, and an env example. Operator hardening checklist is in the README.
- **Ops:** per-IP rate limiting built in; Prometheus `/metrics`; security headers
  (CSP/HSTS/etc.) on by default.

## Status

The metrics roadmap is **complete** — read-only stats, real-time, per-visit
engagement, sessions, and funnels are all shipped and merged. The server crate is
currently `0.1.0`; the tracker is a private build package vendored into
`server/assets/pt.js`.

Deliberately out of scope (privacy): new-vs-returning, retention cohorts,
DAU/MAU. Possible future work only if wanted: event-name funnels (would need
joining events into pageview-sessions) and a per-view drill-down endpoint.
