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
| `CONTACT_TO` | no | (disables `/contact` for submissions that name no `site`) |
| `CONTACT_TO_<SITE>` | no | (per-site recipient — `CONTACT_TO_MY_SITE` serves `"site": "my-site"`; an unconfigured site gets a 503, never `CONTACT_TO`) |
| `STATS_ORIGINS` | no | `*` (any origin) |
| `BEHIND_TLS` | no | `false` (disables HSTS) |
| `TRUST_PROXY_HEADERS` | no | `false` (use TCP peer IP for rate limiting/session hashing) |
| `SESSIONS_ENABLED` | no | `false` (no session IP/UA processing; opt-in for unique visitors, sessions, bounce rate, browser/OS) |
| `LOG_FORMAT` | no | `text` (set `json` for structured logs) |
| `RUST_LOG` | no | `info,sqlx=warn` |

> **Schema:** `0001_init.sql` creates everything and is applied on first start. Migrations are checksummed, so an applied one must never be edited — the server refuses to start if it changes.

## Operator hardening (self-host checklist)

The defaults are safe for a private deploy. For a public-internet host:

- **Set `ADMIN_TOKEN`.** Without it `/stats/*` and blog reads are open. The server logs a warning at startup if unset; blog writes remain disabled until a token is configured.
- **Set `ALLOWED_SITES`** if you only collect for known sites — otherwise any caller can write any `siteId` and bloat your DB.
- **Set `STATS_ORIGINS`** to your dashboard origin so a browser elsewhere can't read `/stats/*` responses even if the admin token leaks.
- **Set `BEHIND_TLS=1`** once the deploy is fronted by HTTPS so the server emits `Strict-Transport-Security`. The other security headers (`X-Content-Type-Options`, `Referrer-Policy`, `X-Frame-Options`) ship unconditionally.
- **Rate limiting** is built in (per-IP, in-process): `/collect` allows ~120/min burst 60, `/contact` allows ~5/min burst 3. By default the server keys on the TCP peer IP and ignores spoofable forwarded headers. If it runs behind a trusted reverse proxy, set `TRUST_PROXY_HEADERS=1` so rate limiting and session hashing use `x-forwarded-for`, then `x-real-ip`, then the TCP peer. The bundled Caddy installer sets this because Caddy is the only public peer. For a hostile public deploy, layer additional limits at Caddy/nginx.
- **Strip the `x-country` header at the proxy** before re-injecting it from a GeoIP lookup — the server trusts whatever the client sends if no proxy strips it.
- **Watch your access logs.** The `/collect` body never stores IPs, but IPs are processed transiently for rate limiting, and your reverse proxy / request traces likely log them. Configure log retention / redaction to match your privacy posture.

## Continuous deployment

A `deploy` job in [`ci.yml`](../.github/workflows/ci.yml) runs after all six CI
gates pass on a push to `master`. It builds a release binary on the runner and
installs it over SSH — the binary is the only artifact, because
`sqlx::migrate!` compiles the migrations into it.

On the server, `/usr/local/bin/dullahan-deploy`
([source](../deploy/dullahan-deploy.sh), installed by `install.sh`) keeps the
running binary as `dullahan.prev`, installs the new one, restarts, and polls
`/health` for 45s. **If the new build does not come up, it restores the previous
binary and fails the job** — a red deploy means the old version is still serving.

One-time setup. On the server:

```bash
# 1. an account for CI, with no rights beyond the deploy helper
adduser --disabled-password --gecos "" deploy
install -d -m 700 -o deploy -g deploy /home/deploy/.ssh

# 2. its own key pair — generate on your machine, upload only the public half
#    ssh-keygen -t ed25519 -f ~/.ssh/dullahan-deploy -C "github-actions"
install -m 600 -o deploy -g deploy /dev/stdin /home/deploy/.ssh/authorized_keys <<< "ssh-ed25519 AAAA... github-actions"

# 3. exactly one sudo right
install -m 0440 -o root -g root deploy/dullahan-deploy.sudoers /etc/sudoers.d/dullahan-deploy
visudo -c

# 4. confirm the helper is present (install.sh puts it there)
test -x /usr/local/bin/dullahan-deploy && echo ok
```

Then add these repo secrets (Settings → Secrets and variables → Actions). The job
fails fast with the missing names if any are absent:

| Secret | Value |
|---|---|
| `DEPLOY_HOST` | hostname or IP the runner can SSH to (the origin, not a proxied CDN name) |
| `DEPLOY_USER` | `deploy` |
| `DEPLOY_SSH_KEY` | the **private** key from step 2 |
| `DEPLOY_KNOWN_HOSTS` | output of `ssh-keyscan -p 22 <host>` — pins the host key so a hijacked DNS record can't harvest the deploy key |
| `DEPLOY_PORT` | optional, defaults to 22 |
| `DEPLOY_HEALTH_URL` | optional public `/health` URL; when set, the job verifies the public route after deploying |

Two things to know:

- **The runner's glibc must not be newer than the server's.** The job pins
  `ubuntu-22.04` (glibc 2.35), which runs on Debian 12 (2.36) and Ubuntu 22.04+.
  On an older server, build in a matching container instead — the symptom is
  `version 'GLIBC_2.xx' not found` in `journalctl -u dullahan`.
- **Want a human in the loop?** The job targets the `production` environment;
  add required reviewers to it in repo settings and every deploy waits for a
  click.

## Weekly digest email

`dullahan --digest` computes a plain-English, week-over-week summary (pageviews,
unique visitors, bounce, avg time on page, top pages/referrers) for each site in
`ALLOWED_SITES` and emails it to that site's `CONTACT_TO_<SITE>` recipient. It
reuses the existing Resend config (`RESEND_API_KEY` / `EMAIL_FROM`); sites with
no recipient are skipped. Preview without sending:

```bash
/opt/dullahan/dullahan --digest --dry-run   # prints each email to stdout
```

Run it weekly with the bundled units:

```bash
sudo cp deploy/dullahan-digest.service deploy/dullahan-digest.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now dullahan-digest.timer
systemctl list-timers dullahan-digest.timer   # confirm the next run
```

The timer defaults to **Sunday 18:00 Europe/Dublin** — edit `OnCalendar` in
`dullahan-digest.timer` to change it (the timezone suffix needs systemd v240+;
drop it for server-local time on older systemd).

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

With [`oha`](https://github.com/hatoo/oha) (`brew install oha`), against a server
on `127.0.0.1:3001`. Note `X-Forwarded-For` only keys the rate limiter when the
server runs with `TRUST_PROXY_HEADERS=1`:

```bash
# ingest under sustained load from one client — expect 429s once the burst is spent
oha -z 30s -c 50 -m POST -H 'Content-Type: application/json' \
  -H 'X-Forwarded-For: 10.0.0.1' \
  -d '{"t":"pageview","s":"loadtest","p":"/","ts":1700000000000,"d":"desktop"}' \
  http://127.0.0.1:3001/collect

# read path
oha -z 30s -c 50 -H "Authorization: Bearer $ADMIN_TOKEN" \
  'http://127.0.0.1:3001/stats/summary?site=loadtest&days=30'
```

Reference numbers from a release build on an M-class laptop, single Postgres on the same box:

| Scenario | Throughput | p99 | Notes |
|---|---|---|---|
| `/collect` from one IP | ~71k rps | <3 ms | Rate-limit returns 429 after the burst is exhausted, server stays responsive |
| `/stats/summary` reads | ~20k rps | ~5 ms | Hits Postgres on every request |

Treat these as smoke-test floors, not throughput guarantees — production numbers depend on disk, Postgres tuning, and the size of the `analytics_events` table.
