# Quickstart — from zero to your first stats

dullahan is one self-hosted binary your website talks to. You run it, point your
site's `<script>` at it, and read stats back with a token. This walks the whole
path end to end.

You need two things: a **Postgres** database and a box to run the binary on.

---

## 1. Run the server

```bash
# install (pick one)
cargo install dullahan                       # from crates.io
# or use the Docker image / deploy/install.sh on a fresh Debian/Ubuntu VM

# run it
DATABASE_URL=postgres://user:pass@localhost/dullahan \
ADMIN_TOKEN=$(openssl rand -hex 24) \
dullahan
```

It binds `0.0.0.0:3001`, applies its migrations automatically, and starts
serving. In production you'd front it with HTTPS (Caddy/nginx) at something like
`https://analytics.example.com` — see [`../deploy/`](../deploy) for a ready-made
Caddyfile + systemd unit.

**Save the `ADMIN_TOKEN`.** It gates the stats reads and the blog writes. Without
it, stats and blog reads are open to anyone — fine on a trusted network, a real
problem on the public internet. Set `BEHIND_TLS=1` once you're behind HTTPS.

> The examples below assume a local server at `http://localhost:3001`. Swap in
> your real host for production.

---

## 2. Add the tracker to your site

One line in your page `<head>` — no npm, no build step. The tracker is baked
into the binary and served at `/pt.js`:

```html
<script defer src="http://localhost:3001/pt.js" data-site="my-site"></script>
```

From load, it auto-collects pageviews (including SPA navigations), web vitals,
and visible time-on-page. Turn on extras with `data-*` attributes:

```html
<script defer src="http://localhost:3001/pt.js"
        data-site="my-site"
        data-track-scroll
        data-track-outbound
        data-respect-dnt></script>
```

Fire your own events from inline JS via the global the tag exposes:

```js
window.dullahan.track("signup", { plan: "pro" });
window.dullahan.page("/virtual-path");   // manual pageview for client-side routes
```

A complete runnable page is in [`script-tag.html`](script-tag.html) — open it
against a running server and watch the events land.

---

## 3. Read your stats

Every `/stats/*` endpoint takes `site` + a time window, returns JSON, and needs
the bearer token when `ADMIN_TOKEN` is set.

```bash
TOKEN=your-admin-token
BASE=http://localhost:3001

# headline numbers for the last 30 days
curl -H "Authorization: Bearer $TOKEN" \
  "$BASE/stats/summary?site=my-site&days=30"

# top pages
curl -H "Authorization: Bearer $TOKEN" \
  "$BASE/stats/top?site=my-site&dim=path&limit=10"

# who's on the site right now (no opt-in needed)
curl -H "Authorization: Bearer $TOKEN" \
  "$BASE/stats/realtime?site=my-site&minutes=5"

# a day-by-day pageview series
curl -H "Authorization: Bearer $TOKEN" \
  "$BASE/stats/timeseries?site=my-site&days=30&bucket=day"
```

Other endpoints: `vitals`, `heatmap`, `channels`, `engagement`, and — with
`SESSIONS_ENABLED=1` — `sessions` and `funnel`. Full catalog with every field is
in the [README](../README.md#3-read-stats).

Wire these into a dashboard, a Grafana panel, or just curl them.

---

## 4. (Optional) the other two jobs

Same binary, same token.

### Blog / content API

A headless content store for an SSR frontend. Markdown is stored and returned
raw — your frontend renders it.

```bash
# create a post (admin)
curl -H "Authorization: Bearer $TOKEN" -X POST "$BASE/posts" \
  -H 'content-type: application/json' \
  -d '{"slug":"hello-world","title":"Hello, world","body_markdown":"# Hi\nFirst post."}'
# -> 201 Created

# list published posts (public)
curl "$BASE/posts?limit=20"

# read one (public)
curl "$BASE/posts/hello-world"

# count a view (public, atomic, no body) -> 204
curl -X POST "$BASE/posts/hello-world/view"
```

### Contact endpoint

A spam-resistant contact form that emails the submission. Needs
`RESEND_API_KEY`, `EMAIL_FROM`, and `CONTACT_TO` env vars set on the server
(without them the endpoint returns `503`).

```bash
curl -X POST "$BASE/contact" \
  -H 'content-type: application/json' \
  -d '{"name":"Jane","email":"jane@example.com","message":"Hi there!"}'
# -> 201 Created
```

---

## What you end up with

One Rust binary + a Postgres replaces a tracking SaaS, a headless CMS, and a
contact-form service — cookie-free, no raw IPs stored, fully self-hosted. For the
architecture and the privacy model, read [`../docs/overview.md`](../docs/overview.md).
