# Dullahan

[![crates.io](https://img.shields.io/crates/v/dullahan.svg)](https://crates.io/crates/dullahan)
[![docs.rs](https://docs.rs/dullahan/badge.svg)](https://docs.rs/dullahan)
[![CI](https://github.com/intrebit/dullahan/actions/workflows/ci.yml/badge.svg)](https://github.com/intrebit/dullahan/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**The headless backend for your site.** A self-hosted, cookie-free Rust binary that gives a small site three things over plain HTTP: privacy-first **analytics**, a headless **blog/content API**, and a **contact** endpoint. It serves its own browser tracker at `/pt.js` — no separate package to install.

New here? [`OVERVIEW.md`](OVERVIEW.md) is a tour of what it does; [`AGENTS.md`](AGENTS.md) is the contributor guide.

## One binary

| Path | What | Install |
|---|---|---|
| `server/` | The whole backend — ingest, stats, blog, contact (Rust + Postgres) | `cargo install dullahan` |
| `tracker/` | Browser tracking client (a TypeScript build tool) — compiled into the server and served at `/pt.js`; not published | — |

## Quick start

### 1. Run the server

```bash
DATABASE_URL=postgres://... \
ADMIN_TOKEN=$(openssl rand -hex 24) \
cargo run --release
```

Migrations run automatically on startup. **Do not run without `ADMIN_TOKEN`** unless the host is on a trusted network — `/stats/*` and blog reads are open by default, while blog writes are refused until a token is configured. The server logs a warning when the token is unset.

> **Upgrading an existing large table:** the `realtime` index ships as `CREATE INDEX CONCURRENTLY` so the build does not block `/collect` writes. If a build is interrupted Postgres leaves an *invalid* index that the migration then skips — drop it (`DROP INDEX analytics_events_site_received_idx;`) and restart to rebuild.

For a one-shot install on a fresh Debian/Ubuntu VM, see [`deploy/install.sh`](deploy/install.sh).

### 2. Add the tracking script

**The simple way — one line, no build step.** The server hosts the client at
`/pt.js`. Drop this into your page `<head>`:

```html
<script defer src="https://analytics.example.com/pt.js" data-site="my-site"></script>
```

`data-endpoint` is optional; it defaults to `/collect` on the origin that served
the script. Opt-ins are `data-*` attributes:

| Attribute | What |
|---|---|
| `data-site` | (required) site identifier |
| `data-endpoint` | `/collect` URL (default: the script's origin + `/collect`) |
| `data-track-scroll` | emit `scroll_depth` events at 25/50/75/100% |
| `data-track-outbound` | emit `outbound` / `download` click events |
| `data-respect-dnt` | send nothing when DNT / GPC is on |
| `data-auto-track="false"` | disable automatic pageviews |

Fire custom events from inline scripts via the global the tag exposes:

```js
window.dullahan.track("signup", { plan: "pro" });
window.dullahan.page("/virtual-path");
```

See [`examples/script-tag.html`](examples/script-tag.html).

UTM tags (`utm_source` / `utm_medium` / `utm_campaign`) on the landing URL are
always captured and attached to the pageview — no flag needed. To change the
tracker, edit `tracker/`, run `npm run build`, and re-commit `server/assets/pt.js`
(CI fails if the vendored script drifts from a fresh build).

### 3. Read stats

All `/stats/*` endpoints require `Authorization: Bearer $ADMIN_TOKEN` when the server has `ADMIN_TOKEN` set.

```
GET /stats/summary?site=my-site&days=30
GET /stats/timeseries?site=my-site&days=30&bucket=day
GET /stats/top?site=my-site&dim=path&limit=10
GET /stats/events?site=my-site&name=scroll_depth&by=pct
GET /stats/vitals?site=my-site&days=30
GET /stats/heatmap?site=my-site&days=30&tz=Europe/Dublin
GET /stats/channels?site=my-site&days=30
GET /stats/realtime?site=my-site&minutes=5
GET /stats/engagement?site=my-site&days=30
GET /stats/sessions?site=my-site&days=30&gap=30
GET /stats/funnel?site=my-site&days=30&steps=/,/pricing,/signup
```

`top?dim=path` returns `avgDurMs` and `medianDurMs` per path. `summary` returns `avgTimeOnPageMs`, `medianTimeOnPageMs`, and `p75TimeOnPageMs`. With sessions enabled (see below), `summary` also returns `uniqueVisitors` and `bounceRate`.

- **`summary?compare=prev`** adds `previous` (same metrics for the immediately preceding equal-length window) and `change` (percentage deltas; `null` when the previous value is 0).
- **`timeseries`** includes a per-bucket `uniqueVisitors` when sessions are on — plot this instead of the range-wide total (see the note below).
- **`vitals`** (site-wide) includes a `distribution` of Core-Web-Vitals pass-rate buckets (`good` / `needsImprovement` / `poor` / `total`) per metric against Google's thresholds. **`vitals?dim=path&limit=N`** instead returns an array of per-path p75s, each with its own per-metric sample count (`lcpN`, `inpN`, … — INP is sparse, so it is reported separately to flag low-confidence p75s).
- **`heatmap`** returns pageview counts per ISO weekday (1–7) × hour (0–23). `tz` is an optional IANA timezone for the hour bucketing (default `UTC`); an unknown timezone returns 400.
- **`channels`** groups pageviews into marketing channels (Direct / Organic Search / Social / Paid / Campaign / Referral) from the referrer host + UTM tags. The brand lists are heuristic.
- **`realtime`** returns `active` — distinct page-visits with any event in the last `minutes` (default 5, clamped 1–60) — plus the top active `pages`. It counts on the server's receive time (not the client clock) and needs no opt-in. Cookie-free, so "active" means page-visits in progress, not logged-in people.
- **`engagement`** returns per-page-visit engagement (a visit = one `view_id`): `engagedVisitRate` (visible ≥10s OR scrolled ≥50% OR an outbound/download click), `avgEventsPerVisit` (your custom `track()` events; auto scroll/outbound events excluded), and — when the matching client tracking is on — `scrollReach75`, `outboundRate`, and a `scrollFunnel` (25/50/75/100). **`engagement?dim=path&limit=N`** returns the same per path. Scroll/outbound fields are **omitted** (not `0`) when the site emits no such events in range, so "not tracked" never reads as "0% engaged"; `engagedVisitRate` is then a lower bound resting on the time signal alone.
- **`sessions`** (requires `SESSIONS_ENABLED`) groups a visitor's pageviews into sessions split by a `gap` of inactivity (minutes, default 30, clamped 1–240) and returns `sessions`, `avg`/`medianPagesPerSession`, `avg`/`medianDurationMs`, and a session-level `bounceRate`. **`sessions?dim=entry`** / **`dim=exit`** return the top entry / exit pages. A single-pageview session has duration 0. Because the visitor-hash salt rotates at 00:00 UTC, **sessions never cross midnight UTC** (a visit spanning it splits in two) — the same constraint behind `uniqueVisitors`. This `bounceRate` is single-pageview *sessions*; `summary.bounceRate` is single-pageview visitor-*days* — the session figure is the standard one.
- **`funnel`** (requires `SESSIONS_ENABLED`) takes `steps` — 2–10 comma-separated pageview paths — and reports, per step, how many sessions reached it **in order** (`sessions`), plus `conversionFromPrev` and `conversionFromStart`. Steps must occur in time order within a session (gap/`gap` as for `sessions`); a later step seen before its predecessor doesn't count. Example: `steps=/,/pricing,/signup`.

> **Note on `uniqueVisitors`:** the visitor hash is salted with a salt that rotates every UTC day (and is then deleted), so the same person hashes differently each day. Over a multi-day range `uniqueVisitors` therefore counts *visitor-days*, not distinct people — a visitor active on N days counts as N. This is a deliberate consequence of the cookie-free, unlinkable-by-design model. For a per-day figure, query a 1-day range per day.

`top` dimensions: `path`, `referrer`, `country`, `device`, `viewport`, `utm_source`, `utm_medium`, `utm_campaign`, and (sessions only) `browser`, `os`.

`events` returns the top event names for a site; add `name=<event>&by=<prop>` to get the distribution of one event's prop value (e.g. scroll-depth milestones).

## Blog API

An optional set of endpoints for storing blog posts and counting per-post views, intended for an SSR frontend that talks to dullahan server-to-server. Responses are JSON with **snake_case** keys (unlike `/stats/*`, which is camelCase). Markdown is stored and returned **raw** in `body_markdown` — it is never rendered to HTML server-side; the caller sanitizes and renders it.

```
GET    /posts?limit=20&offset=0&status=published   # list (status=all incl. drafts needs admin)
GET    /posts/:slug                                # single post (PostDetail)
POST   /posts/:slug/view                           # public, atomic view++ -> 204
POST   /posts                                      # create (admin) -> 201
PATCH  /posts/:id                                  # update (admin) -> 200
DELETE /posts/:id                                  # delete (admin) -> 204
```

- **Auth.** Create / update / delete require a configured `ADMIN_TOKEN` and the same `Authorization: Bearer $ADMIN_TOKEN` as `/stats/*`; without a configured token, destructive blog writes return `401`. Blog reads follow the stats open-mode behavior: when `ADMIN_TOKEN` is unset, reads are open, including `status=all`.
- **Drafts.** `draft=true` posts are hidden from the published list and return 404 on `GET /posts/:slug` unless the request is admin-authed. `POST /posts/:slug/view` only counts non-draft posts and is always a no-op `204` (missing/draft slug included) — no dedupe; debounce client-side.
- **`POST /posts`** body: `{ slug, title, description?, author?, image?, body_markdown, draft?, pub_date? }`. `slug` must match `^[a-z0-9-]+$`; duplicate slug returns `409`. **`PATCH /posts/:id`** accepts any subset of those fields and sets `updated_date`.

## What gets collected

- Pageviews (path, referrer domain, device class, viewport bucket, country, UTM tags)
- Custom events (name + optional props) — including opt-in scroll depth and outbound/download clicks
- Web vitals (LCP, FCP, CLS, INP, TTFB)
- **Time on page** — visible duration only. The client never measures while the tab is hidden, and stops at 30 minutes per page.

Optional, only when `SESSIONS_ENABLED=1` (off by default):

- Unique visitors, sessions, bounce rate
- Browser + OS *family* (e.g. "Chrome / macOS", never versions)

**Privacy:** no cookies, no fingerprinting, **no raw IP storage — ever.** With sessions **off** (the default) the server reads neither the client IP nor the User-Agent. With sessions **on**, the IP and User-Agent are combined with a daily-rotating salt into an anonymized hash and immediately discarded; the salt is deleted after 48h, making old hashes permanently unlinkable. The browser client is ~3 KB gzipped.

## Configuration

Server env vars:

| Var | Required | Default |
|---|---|---|
| `DATABASE_URL` | yes | — |
| `BIND_ADDR` | no | `0.0.0.0:3001` |
| `ADMIN_TOKEN` | recommended | unset (stats and blog reads are public; blog writes disabled) |
| `ALLOWED_SITES` | no | unrestricted |
| `RESEND_API_KEY` | no | (disables email) |
| `EMAIL_FROM` | no | — |
| `EMAIL_FROM_NAME` | no | `dullahan` |
| `CONTACT_TO` | no | (disables `/contact`) |
| `STATS_ORIGINS` | no | `*` (any origin) |
| `BEHIND_TLS` | no | `false` (disables HSTS) |
| `SESSIONS_ENABLED` | no | `false` (no IP/UA processing; opt-in for unique visitors, sessions, bounce rate, browser/OS) |
| `LOG_FORMAT` | no | `text` (set `json` for structured logs) |
| `RUST_LOG` | no | `info,sqlx=warn` |

## Operator hardening (self-host checklist)

The defaults are safe for a private deploy. For a public-internet host:

- **Set `ADMIN_TOKEN`.** Without it `/stats/*` and blog reads are open. The server logs a warning at startup if unset; blog writes remain disabled until a token is configured.
- **Set `ALLOWED_SITES`** if you only collect for known sites — otherwise any caller can write any `siteId` and bloat your DB.
- **Set `STATS_ORIGINS`** to your dashboard origin so a browser elsewhere can't read `/stats/*` responses even if the admin token leaks.
- **Set `BEHIND_TLS=1`** once the deploy is fronted by HTTPS so the server emits `Strict-Transport-Security`. The other security headers (`X-Content-Type-Options`, `Referrer-Policy`, `X-Frame-Options`) ship unconditionally.
- **Rate limiting** is built in (per-IP, in-process): `/collect` allows ~120/min burst 60, `/contact` allows ~5/min burst 3. The server reads the client IP from `x-forwarded-for` / `x-real-ip` (with the TCP peer as fallback), so make sure your reverse proxy sets one of those. For a hostile public deploy, layer additional limits at Caddy/nginx.
- **Strip the `x-country` header at the proxy** before re-injecting it from a GeoIP lookup — the server trusts whatever the client sends if no proxy strips it.
- **Watch your access logs.** The `/collect` body never stores IPs, but your reverse proxy and `tower-http` request traces likely log the client IP. Configure log retention / redaction to match your privacy posture.

## Privacy notes (for SDK consumers)

The library doesn't fingerprint or store IPs, but two channels can still leak PII if you're not careful:

- **URL paths.** `dullahan` strips `?query` and `#hash` but not path segments. A path like `/users/jane@example.com/orders/42` will be stored verbatim. Strip or hash sensitive segments client-side before navigating, or pass a sanitized path to `analytics.page(path)`.
- **Custom event props.** `analytics.track(name, props)` stores `props` as-is. Don't pass emails, names, or tokens. Use a stable `userId` hash if you need correlation.

## Metrics

`GET /metrics` exposes Prometheus-format metrics for HTTP traffic (request rate, latency histograms, status codes per route). Scrape it with Prometheus / Grafana Agent / Vector.

The endpoint is **unauthenticated** — keep it on an internal interface or block external access at your reverse proxy. Standard practice for `/metrics` everywhere; dullahan follows the convention.

```
# HELP axum_http_requests_total Total HTTP requests.
# TYPE axum_http_requests_total counter
axum_http_requests_total{method="GET",path="/health",status="200"} 1
...
```

## Load testing

A small wrapper around [`oha`](https://github.com/hatoo/oha) lives at [`scripts/loadtest.sh`](scripts/loadtest.sh):

```bash
brew install oha
BASE=http://127.0.0.1:3001 ./scripts/loadtest.sh collect-burst    # single-IP abuse
BASE=http://127.0.0.1:3001 ./scripts/loadtest.sh collect-spread   # parallel IPs
BASE=http://127.0.0.1:3001 ./scripts/loadtest.sh stats-read       # read path
```

Reference numbers from a release build on an M-class laptop, single Postgres on the same box:

| Scenario | Throughput | p99 | Notes |
|---|---|---|---|
| `/collect` from one IP | ~71k rps | <3 ms | Rate-limit returns 429 after the burst is exhausted, server stays responsive |
| `/stats/summary` reads | ~20k rps | ~5 ms | Hits Postgres on every request |

Treat these as smoke-test floors, not throughput guarantees — production numbers depend on disk, Postgres tuning, and the size of the `analytics_events` table.

## Security

If you find a vulnerability, please report it privately — see [`SECURITY.md`](SECURITY.md). Do not open a public issue.

## License

MIT
