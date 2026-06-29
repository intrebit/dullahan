# Deployment & operations

How to configure, harden, and observe a dullahan server. For a one-shot install
on a fresh Debian/Ubuntu VM, see [`../deploy/install.sh`](../deploy/install.sh)
(plus the systemd unit, `Caddyfile`, and env example alongside it).

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

> **Upgrading an existing large table:** the `realtime` index ships as `CREATE INDEX CONCURRENTLY` so the build does not block `/collect` writes. If a build is interrupted Postgres leaves an *invalid* index that the migration then skips — drop it (`DROP INDEX analytics_events_site_received_idx;`) and restart to rebuild.

## Operator hardening (self-host checklist)

The defaults are safe for a private deploy. For a public-internet host:

- **Set `ADMIN_TOKEN`.** Without it `/stats/*` and blog reads are open. The server logs a warning at startup if unset; blog writes remain disabled until a token is configured.
- **Set `ALLOWED_SITES`** if you only collect for known sites — otherwise any caller can write any `siteId` and bloat your DB.
- **Set `STATS_ORIGINS`** to your dashboard origin so a browser elsewhere can't read `/stats/*` responses even if the admin token leaks.
- **Set `BEHIND_TLS=1`** once the deploy is fronted by HTTPS so the server emits `Strict-Transport-Security`. The other security headers (`X-Content-Type-Options`, `Referrer-Policy`, `X-Frame-Options`) ship unconditionally.
- **Rate limiting** is built in (per-IP, in-process): `/collect` allows ~120/min burst 60, `/contact` allows ~5/min burst 3. The server reads the client IP from `x-forwarded-for` / `x-real-ip` (with the TCP peer as fallback), so make sure your reverse proxy sets one of those. For a hostile public deploy, layer additional limits at Caddy/nginx.
- **Strip the `x-country` header at the proxy** before re-injecting it from a GeoIP lookup — the server trusts whatever the client sends if no proxy strips it.
- **Watch your access logs.** The `/collect` body never stores IPs, but your reverse proxy and `tower-http` request traces likely log the client IP. Configure log retention / redaction to match your privacy posture.

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

A small wrapper around [`oha`](https://github.com/hatoo/oha) lives at [`../scripts/loadtest.sh`](../scripts/loadtest.sh):

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
