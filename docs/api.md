# HTTP API reference

dullahan exposes three HTTP surfaces: the **stats read API** (`/stats/*`,
camelCase JSON, admin-gated), the **blog/content API** (`/posts`, snake_case
JSON), and ingest (`/collect`, written by the tracker — see the
[README](../README.md)). For an architecture tour see
[`overview.md`](overview.md); for a copy-paste walkthrough see
[`../examples/QUICKSTART.md`](../examples/QUICKSTART.md).

## Stats (`/stats/*`)

All `/stats/*` endpoints require `Authorization: Bearer $ADMIN_TOKEN` when the
server has `ADMIN_TOKEN` set.

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

## Blog / content API

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

## Avoiding PII leaks

dullahan doesn't fingerprint or store IPs, but two channels can still leak PII if you're not careful:

- **URL paths.** `dullahan` strips `?query` and `#hash` but not path segments. A path like `/users/jane@example.com/orders/42` will be stored verbatim. Strip or hash sensitive segments client-side before navigating, or pass a sanitized path to `dullahan.page(path)`.
- **Custom event props.** `dullahan.track(name, props)` stores `props` as-is. Don't pass emails, names, or tokens. Use a stable `userId` hash if you need correlation.
