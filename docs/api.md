# HTTP API reference

**Everything is tenant-scoped.** Sites live in the `sites` table; every content
and stats endpoint takes a mandatory `?site=` and refuses a caller whose
credential does not cover it. See [Tenants](#tenants-sites) below.

dullahan exposes five HTTP surfaces: the **stats read API** (`/stats/*`,
camelCase JSON, admin-gated), the **blog/content API** (`/posts`, snake_case
JSON), the **contact form** (`/contact`), ingest (`/collect`, which any
browser tracker POSTs events to), and the **tenant registry** (`/sites`). Configuration and self-host hardening
are in [`deploy.md`](deploy.md).

## Stats (`/stats/*`)

All `/stats/*` endpoints require `Authorization: Bearer $ADMIN_TOKEN` when the
server has `ADMIN_TOKEN` set.

```
GET /stats/summary?site=my-site&days=30
GET /stats/timeseries?site=my-site&days=30&bucket=day
GET /stats/top?site=my-site&dim=path&limit=10
GET /stats/events?site=my-site&name=signup&by=plan
GET /stats/channels?site=my-site&days=30
GET /stats/realtime?site=my-site&minutes=5
```

`summary` returns `avgTimeOnPageMs`. With sessions enabled (see below), it also returns `avgDailyVisitors` and `bounceRate`.

- **`summary?compare=prev`** adds `previous` (same metrics for the immediately preceding equal-length window) and `change` (percentage deltas; `null` when the previous value is 0).
- **`timeseries`** includes a per-bucket `uniqueVisitors` when sessions are on — the distinct visitors *within each bucket* (a bucket is one UTC day at the default `day` granularity).
- **`channels`** groups pageviews into marketing channels (Direct / Organic Search / Social / Paid / Campaign / Referral) from the referrer host + UTM tags. The brand lists are heuristic.
- **`realtime`** returns `active` — distinct page-visits with any event in the last `minutes` (default 5, clamped 1–60) — plus the top active `pages`. It counts on the server's receive time (not the client clock) and needs no opt-in. Cookie-free, so "active" means page-visits in progress, not logged-in people.

> **Note on visitor counts:** the visitor hash is salted with a salt that rotates every UTC day and is pruned after retention, so the same person hashes differently each day — a true cross-day unique count is impossible by design (cookie-free, unlinkable). `summary` therefore reports **`avgDailyVisitors`**: the mean of the per-UTC-day distinct-visitor counts over the range — an honest "typical day" figure rather than an inflated range-wide total. For the day-by-day series, read `uniqueVisitors` from `timeseries`.

`top` dimensions: `path`, `referrer`, `country`, `device`, `utm_source`, `utm_medium`, `utm_campaign`.

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
- **`POST /posts`** body: `{ slug, title, description?, author?, image?, body_markdown, draft?, pub_date? }`. `slug` must match `^[a-z0-9-]+$`; a duplicate slug **within the same site** returns `409` — two tenants may each own `about`. **`PATCH /posts/:id`** accepts any subset of those fields and sets `updated_date`.

## Product catalog (`/products`)

A headless product listing for a simple webshop — no cart, no orders, no stock counts. Same shape and auth model as the blog: JSON with **snake_case** keys, public reads, admin-gated writes. Each response also carries a `currency` field (from `SHOP_CURRENCY`, default `EUR`).

```
GET    /products?limit=50&offset=0&status=published   # list (status=all incl. drafts needs admin)
GET    /products/:slug                                # single product
POST   /products/:slug/view                           # public, atomic view++ -> 204
POST   /products                                      # create (admin) -> 201
PATCH  /products/:id                                  # update (admin) -> 200
DELETE /products/:id                                  # delete (admin) -> 204
```

- **View counter.** Each product carries a `views` count. `POST /products/:slug/view` increments it — call it from the frontend when a product page is shown (debounce client-side). Like the blog counter it only counts non-draft products and is always a no-op `204` (missing/draft slug included). This is how the owner sees which products are being viewed, without touching `/stats/*`.
- **Fields.** `{ slug, title, description?, image?, price_cents?, available?, position?, draft? }` (`views` is read-only, server-managed). `price_cents` is an **integer count of minor units** (e.g. `1299` = €12.99 when `SHOP_CURRENCY=EUR`) — there is no per-product currency and no floating-point money. `slug` must match `^[a-z0-9-]+$`; a duplicate slug **within the same site** returns `409`. A negative `price_cents` returns `400`.
- **Listing.** Ordered by `position` ascending, then newest first. `available=false` (sold out) items are **still listed** — it's a display flag for the frontend, not a filter. `draft=true` items are hidden from the public list and 404 on `GET /products/:slug` unless admin-authed.
- **Auth.** Create / update / delete require a configured `ADMIN_TOKEN` (same bearer as `/stats/*`); reads follow stats open-mode (open when no token is set). **`PATCH /products/:id`** accepts any subset of the create fields and sets `updated_date`.
- **CORS.** `GET /products` and the `POST /products/:slug/view` ping send CORS headers so a storefront on another origin can read the catalog from the browser. Open to any origin by default (the published catalog is public); set `PRODUCT_ORIGINS` to restrict. Only `GET`/`POST` are exposed — the admin mutating verbs aren't reachable cross-origin from a browser.

## Contact form (`POST /contact`)

Takes a form submission and emails it via Resend. Public (no auth), rate-limited
to ~5/min per IP, and disabled — `503` — until the server has both an email
transport (`RESEND_API_KEY` + `EMAIL_FROM`) and a recipient.

```
POST /contact   {"site": "my-site", "name": "...", "email": "...", "message": "..."}  -> 201
```

`email` must look like an address, `name` ≤ 80 chars, `message` 10–2000 chars;
anything else is a `400` with `{"message": "..."}`. The sender is the server's
`EMAIL_FROM`, with the submitter's address as `Reply-To`.

- **`site` is required** and selects the recipient — the tenant's `contact_to`
  in the `sites` table. There is no server-wide default.
- **An unregistered, suspended, or recipient-less `site` is refused with `503`,
  never delivered elsewhere.** A typo'd tenant fails loudly (and logs a warning)
  instead of leaking one site's enquiries into another's inbox.
- The sender is the tenant's `email_from` if set, else the server's `EMAIL_FROM`;
  the submitter's address is always the `Reply-To`.

## What gets collected

- Pageviews (path, referrer domain, device class, country, UTM tags)
- Custom events (name + optional props) — including opt-in outbound/download clicks
- **Time on page** — visible duration only. The client never measures while the tab is hidden, and stops at 30 minutes per page.

Optional, only when `SESSIONS_ENABLED=1` (off by default):

- Unique visitors, sessions, bounce rate

The selected client IP is processed transiently for rate limiting. It is never
stored. Session hashing uses that same selected IP only when
`SESSIONS_ENABLED=1`; behind a trusted proxy, set `TRUST_PROXY_HEADERS=1` to use
`x-forwarded-for` / `x-real-ip` instead of the TCP peer.

## Avoiding PII leaks

dullahan doesn't fingerprint or store IPs, but two channels can still leak PII if you're not careful:

- **URL paths.** `dullahan` strips `?query` and `#hash` but not path segments. A path like `/users/jane@example.com/orders/42` will be stored verbatim. Strip or hash sensitive segments client-side before navigating, or pass a sanitized path to `dullahan.page(path)`.
- **Custom event props.** `dullahan.track(name, props)` stores `props` as-is. Don't pass emails, names, or tokens. Use a stable `userId` hash if you need correlation.

## Tenants (`/sites`)

The tenant registry. **Operator scope only** — the global `ADMIN_TOKEN`. A
per-site token gets `403` here; that boundary is what stops one tenant minting
another. There is no CORS layer on these routes at all, so they are unreachable
from a browser under any configuration.

```
GET    /sites             list
POST   /sites             create -> 201, returns the token ONCE
GET    /sites/:id         detail
PATCH  /sites/:id         update name/domain/contact_to/email_from/email_from_name/active
POST   /sites/:id/token   rotate -> returns the new token ONCE
DELETE /sites/:id         204, or 409 while the tenant still owns content
```

- **`id`** must match `^[a-z0-9-]+$`, ≤ 64 chars. It is the same handle the
  tracker sends as `s`, that `?site=` carries, and that scopes every row.
- **Tokens are generated server-side** (256 bits, `dh_s_`-prefixed) and returned
  exactly once, on create and on rotate. They are stored only as a hash and are
  never recoverable — rotation is the recovery path. Reads return `token_last4`
  so you can tell which credential is deployed where.
- **Rotation is a hard cutover**: the old token stops working immediately, with
  no grace window. A tenant running the old token will get `401`s until it is
  redeployed.
- **`active: false`** suspends a tenant: its token stops resolving, `/collect`
  refuses its events, and the weekly digest skips it. Data is retained.
- **Delete refuses (`409`) while the tenant owns content**, so offboarding is an
  explicit purge rather than a silent cascade.

### Scopes

| Credential | Content + stats | `/sites` |
|---|---|---|
| Global `ADMIN_TOKEN` (operator) | every site | yes |
| Per-site token | its own site only | no (`403`) |
| None | public reads only | no (`401`) |
| No `ADMIN_TOKEN` configured | reads open, all writes refused | no (`401`) |

A caller naming a site its credential does not cover gets `403` — and that
response is byte-identical whether the site belongs to another tenant or does
not exist, so it is not an existence oracle. A correct row id addressed under the
wrong site returns `404`, never `403`, for the same reason.
