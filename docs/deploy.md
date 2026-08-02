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
| `ADMIN_TOKEN` | recommended | unset (stats and content reads are public; writes disabled) — this is the **operator** token: every tenant, plus the `/sites` registry |
| `RESEND_API_KEY` | no | (disables email) |
| `EMAIL_FROM` | no | — |
| `EMAIL_FROM_NAME` | no | `dullahan` |
| `STATS_ORIGINS` | no | `*` (any origin) |
| `BEHIND_TLS` | no | `false` (disables HSTS) |
| `TRUST_PROXY_HEADERS` | no | `false` (use TCP peer IP for rate limiting/session hashing) |
| `SESSIONS_ENABLED` | no | `false` (no session IP/UA processing; opt-in for unique visitors, sessions, bounce rate, browser/OS) |
| `RETENTION_DAYS` | no | unset (events kept forever) — set e.g. `365` to sweep `analytics_events` older than that |
| `LOG_FORMAT` | no | `text` (set `json` for structured logs) |
| `RUST_LOG` | no | `info,sqlx=warn` |
| `ALERT_TO` | recommended | unset (checks run and log, but cannot page) — operator address for `--selfcheck` |
| `ALERT_DISK_PERCENT` | no | `85` |
| `ALERT_REPEAT_HOURS` | no | `6` (how long before re-mailing an ongoing problem) |
| `BACKUP_DIR` | no | `/var/backups/dullahan` (where `--selfcheck` looks for backup runs) |
| `ALERT_BACKUP_MAX_AGE_HOURS` | no | `48` (`0` disables the staleness check) |
| `HEALTHCHECK_URL` | optional | unset — external dead-man's-switch; add it to cover the host itself dying |
| `SELFCHECK_STATE_PATH` | no | `selfcheck-state.json` (relative to `WorkingDirectory`) |

> **Tenancy:** sites live in the `sites` table, not in env vars. `ALLOWED_SITES`
> and `CONTACT_TO_<SITE>` were removed — register a tenant with `POST /sites`
> (see [`api.md`](api.md)) and set its `contact_to` / `email_from` there.
> **While the `sites` table is empty every site id is admitted** on `/collect`
> and `/stats/*`, so a fresh install works out of the box; the server warns at
> startup until you register your tenants.

> **Schema:** `0001_init.sql` creates everything and is applied on first start.
> **The schema is frozen as of v0.1.4 and migrations are append-only from here on.**
> sqlx records the SHA-384 of each migration in `_sqlx_migrations` and refuses to
> start if it stops matching, so editing a released migration does not break your
> build — it breaks someone else's server after they upgrade. A CI job
> (`migrations (frozen)`) enforces both halves: no listed file may change, and every
> migration must be listed in `server/migrations.sha384`. Adding a migration means
> appending it and regenerating the manifest:
>
> ```bash
> cd server && sha384sum migrations/*.sql > migrations.sha384
> ```
>
> **Upgrading from before the freeze** (any deploy migrated with the old
> `0001`–`0005` chain) needs a one-time rebaseline, because squashing those into a
> single `0001` changed its checksum. This used to call for `DROP DATABASE`; it no
> longer does. The two schemas are equivalent apart from one index and one CHECK
> constraint, so the recorded history can be rewritten in place with every row kept:
>
> ```bash
> deploy/rebaseline-migrations.sh            # report: changes nothing
> deploy/rebaseline-migrations.sh --apply    # after taking a backup
> ```
>
> It refuses to run against any history it does not recognise, and blocks if
> legacy `type='performance'` rows would violate the tightened constraint. The
> symptom it fixes is `migration 1 was previously applied but has been modified`
> in `journalctl -u dullahan`.

> **Event retention:** `analytics_events` is the only table that grows without
> bound, and by default nothing deletes from it. Set `RETENTION_DAYS` to sweep
> rows older than N days — the sweep runs at startup and every 6 hours after,
> deletes in 10k chunks (so the first pass over a long-retained table doesn't
> hold one long transaction), and logs the row count at `info`. It compares
> against `ts`, the client clock `/stats/*` filters on, so retention matches
> what the API can still report. This is disk hygiene, not a privacy control:
> the unlinkability of old visitor hashes comes from pruning `daily_salts`,
> which happens regardless.

## Operator hardening (self-host checklist)

The defaults are safe for a private deploy. For a public-internet host:

- **Set `ADMIN_TOKEN`.** Without it `/stats/*` and blog reads are open. The server logs a warning at startup if unset; blog writes remain disabled until a token is configured.
- **Register your tenants** in the `sites` table (`POST /sites`). While it is empty any caller can write any `siteId` and bloat your DB.
- **Set `STATS_ORIGINS`** to your dashboard origin so a browser elsewhere can't read `/stats/*` responses even if the admin token leaks.
- **Set `BEHIND_TLS=1`** once the deploy is fronted by HTTPS so the server emits `Strict-Transport-Security`. The other security headers (`X-Content-Type-Options`, `Referrer-Policy`, `X-Frame-Options`) ship unconditionally.
- **Rate limiting** is built in (per-IP, in-process): `/collect` allows ~120/min burst 60, `/contact` allows ~5/min burst 3. By default the server keys on the TCP peer IP and ignores spoofable forwarded headers. If it runs behind a trusted reverse proxy, set `TRUST_PROXY_HEADERS=1` so rate limiting and session hashing use `x-forwarded-for`, then `x-real-ip`, then the TCP peer. The bundled Caddy installer sets this because Caddy is the only public peer. For a hostile public deploy, layer additional limits at Caddy/nginx.
- **Consider `RETENTION_DAYS`.** Nothing deletes analytics events by default, so a busy site's `analytics_events` grows until the disk fills — and every `/stats/*` query slows as it does. Data minimisation also argues for setting it.
- **Set `x-country` at the proxy, and delete any inbound copy first.** The server trusts the header as given, so without a proxy overwriting it a client can post fake countries — and with no proxy setting it at all, `country` is simply `NULL` on every row and the `/stats/top?dim=country` breakdown and the digest's "Top countries" section stay empty. Behind Cloudflare it is free, since the edge already sends `CF-IPCountry`:

  ```caddy
  reverse_proxy 127.0.0.1:3001 {
      header_up -X-Country
      header_up X-Country {http.request.header.Cf-Ipcountry}
  }
  ```

  The delete is not redundant: it is what stops a caller supplying its own value. With another CDN, substitute its equivalent header; with none, use a GeoIP module. Country is captured at collection time and cannot be backfilled.
- **Watch your access logs.** The `/collect` body never stores IPs, but IPs are processed transiently for rate limiting, and your reverse proxy / request traces likely log them. Configure log retention / redaction to match your privacy posture.
- **Block `/metrics` at the proxy.** It is unauthenticated by convention and exposes per-tenant traffic shape. The bundled `Caddyfile` returns 404 for it; if you front dullahan with your own nginx/Traefik, replicate that. See [Metrics](#metrics).
- **Set up backups and prove a restore.** See [Backups](#backups). Nothing else in this list matters if the disk dies.
- **Set `ALERT_TO`.** `dullahan_ingest_insert_failures_total` exists because ingest can lose rows without any request failing; unmonitored, it increments into the void. See [Monitoring and alerting](#monitoring-and-alerting).

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
unique visitors, bounce, avg time on page, top pages/referrers/countries/devices)
for each site in
the `sites` table and emails it to that site's `contact_to`. It reuses the
Resend transport (`RESEND_API_KEY` / `EMAIL_FROM`), overridden per site by
`email_from`; sites with no recipient, and suspended sites, are skipped. Preview without sending:

```bash
/opt/dullahan/dullahan --digest --dry-run   # prints each email to stdout
```

`install.sh` installs and starts `dullahan-digest.timer` — this used to be a
copy-these-files-by-hand step here, which is precisely why the digest was shipped
but never actually firing anywhere. Confirm with:

```bash
systemctl list-timers dullahan-digest.timer
```

The timer defaults to **Sunday 18:00 Europe/Dublin** — edit `OnCalendar` in
`dullahan-digest.timer` to change it (the timezone suffix needs systemd v240+;
drop it for server-local time on older systemd).

## Metrics

`GET /metrics` exposes Prometheus-format metrics for HTTP traffic (request rate, latency histograms, status codes per route), plus dullahan's own ingest counters:

| Counter | Meaning |
|---|---|
| `dullahan_ingest_queue_full_total` | `/collect` shed an event because the writer queue was full. The server answered 503, so the client knows — but the event is gone. |
| `dullahan_ingest_insert_failures_total` | An event reached the writer and Postgres rejected it. Silent data loss; nothing else reveals it. |
| `dullahan_ingest_queue_closed_total` | The writer task is gone. A bug or a shutdown race — nothing will be persisted until restart. |

The endpoint is **unauthenticated**, which is the convention everywhere, and it leaks
request rates, path cardinality and error counts for every tenant on the host. So it
must not be reachable from the internet. The bundled `Caddyfile` blocks it:

```caddy
@metrics path /metrics /metrics/*
respond @metrics 404
```

Scrape it over loopback instead (`http://127.0.0.1:<port>/metrics`), which is what
`dullahan --selfcheck` and any local Prometheus agent do. Verify both halves after
changing the proxy config:

```bash
curl -s -o /dev/null -w '%{http_code}\n' https://your.domain/metrics   # want 404
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:3001/metrics # want 200
```

```
# HELP axum_http_requests_total Total HTTP requests.
# TYPE axum_http_requests_total counter
axum_http_requests_total{method="GET",path="/health",status="200"} 1
...
```

## Backups

`install.sh` installs `dullahan-backup.timer` (nightly, 03:15 + jitter) and
`dullahan-restore-drill.timer` (first Sunday of the month). **Both are enabled but
left stopped until you set an off-box destination** — a backup that only ever lands
on the disk it is protecting is the failure mode worth refusing to configure
silently.

Each run writes `$BACKUP_DIR/<UTC timestamp>/` containing `globals.sql.age`, one
`<db>.dump.age` per database (`pg_dump -Fc`), and a `MANIFEST` of SHA-256 sums.
Everything is `age`-encrypted *before* upload, so the object store never holds
readable analytics data.

Setup:

```bash
# 1. encryption keypair (install.sh generates one on first run; this is the manual path)
age-keygen -o /etc/dullahan/backup-identity.txt
age-keygen -y /etc/dullahan/backup-identity.txt > /etc/dullahan/backup-recipients.txt
chmod 600 /etc/dullahan/backup-*.txt

# 2. an off-box destination. Any S3-compatible bucket works; Cloudflare R2 is
#    recommended because it charges nothing for egress — the restore drill
#    downloads a backup every month, and on providers that bill egress that
#    verification is the thing you end up quietly switching off. 10 GB free.
apt install rclone age
rclone config
#   n) new remote   name: r2   storage: s3   provider: Cloudflare
#   access_key_id / secret_access_key: from an R2 API token
#   endpoint: https://<accountid>.r2.cloudflarestorage.com     region: auto
rclone mkdir r2:dullahan-backups

# 3. point the scripts at it
$EDITOR /etc/dullahan/backup.env  # RCLONE_REMOTE=r2:dullahan-backups

# 4. prove it works before trusting it
dullahan-backup && dullahan-restore-drill

# 5. start the timers
systemctl start dullahan-backup.timer dullahan-restore-drill.timer
```

> **Copy `/etc/dullahan/backup-identity.txt` off this machine.** It is the only
> thing that can decrypt these backups. Left only on the host being backed up, a
> dead disk destroys the backups along with the data they existed to protect — you
> would be paying for a bucket full of files nobody can open.

**Retention.** Local copies are grandfathered: `KEEP_DAILY=7`, `KEEP_WEEKLY=4`,
`KEEP_MONTHLY=3`, kept by time *bucket* rather than by age, so a long gap in runs
cannot silently expire every copy you have. Remote retention is deliberately **not**
managed by the script — set a bucket lifecycle rule instead, so a compromised or
buggy backup host cannot delete your history.

**The restore drill is the point.** It downloads the newest backup from the object
store (the copy that would actually survive losing this host), verifies it against
its manifest, decrypts it, restores into a throwaway database, asserts the expected
tables and non-empty row counts, then drops it. A backup system whose restore path
has never executed is a hope, not a backup.

## Monitoring and alerting

Two layers, because they fail differently.

**On-box: `dullahan --selfcheck`**, run every 10 minutes by
`dullahan-selfcheck.timer`. A separate short-lived process, deliberately with no
`After=dullahan.service` dependency — reporting that the server is down is its job,
so depending on it would disable the check in exactly the situation it exists for.
It checks:

| Check | Catches |
|---|---|
| `/health` reachable | the server being down or wedged |
| Postgres answers `SELECT 1` | database down, out of connections, disk-full refusals |
| ingest-loss counters rose since last run | events accepted and then lost — no request failure reveals this |
| disk usage ≥ `ALERT_DISK_PERCENT` | the slow death that takes Postgres with it |
| newest backup younger than `ALERT_BACKUP_MAX_AGE_HOURS` | **the nightly backup having quietly stopped** |

Findings are mailed to `ALERT_TO` through the existing Resend transport. Repeat
alerts for an ongoing problem are throttled to `ALERT_REPEAT_HOURS`, and a recovered
check has its throttle cleared so the next occurrence alerts immediately.

The backup-staleness check is what makes an external watchdog optional rather than
necessary: a backup cron's normal failure mode is dying quietly months before anyone
looks, and this notices that the *artifacts* stopped appearing. It stays silent until
`BACKUP_DIR` exists — i.e. until backups have run at least once — so a deploy that
deliberately has none is not nagged every ten minutes.

```bash
/opt/dullahan/dullahan --selfcheck --dry-run   # print findings, send nothing
```

It exits non-zero when anything is wrong, which is load-bearing: systemd records
the failure, and the watchdog ping below does not happen.

**Off-box (optional): `HEALTHCHECK_URL`.** The checks above cover everything
*on* this box. The one thing they structurally cannot cover is the box itself being
gone — a dead host cannot report its own absence, and neither can a dead timer.

That gap is real but it is one gap, and closing it means depending on a third-party
service, so it is left off by default. When you want it, create checks at
[healthchecks.io](https://healthchecks.io) (free tier is enough) and set
`HEALTHCHECK_URL` in `dullahan.env`, plus `HEALTHCHECK_BACKUP_URL` and
`HEALTHCHECK_DRILL_URL` in `/etc/dullahan/backup.env`. Each pings on success and
`<url>/fail` on failure, so you learn about a failure immediately *and* about
silence. Suggested periods: selfcheck 10min + 30min grace, backup 1 day + 2h, drill
1 month + 1 day.

Until then, host-down detection is whatever you already have — uptime monitoring on
the public hostname, or noticing the site is offline.

Why not Prometheus and Grafana on the box? On a small VPS already running Postgres
and the server, it is several hundred MB of RAM for dashboards — and it still could
not alert you when the host died. If you want metric history, scrape `/metrics` over
loopback from somewhere else.

## Timers

Everything the project schedules, all installed by `install.sh`:

| Timer | Schedule | What it does |
|---|---|---|
| `dullahan-selfcheck.timer` | every 10 min | health, Postgres, ingest-loss counters, disk |
| `dullahan-digest.timer` | Sun 18:00 | per-site weekly digest email |
| `dullahan-backup.timer` | 03:15 nightly | encrypted dump, uploaded off-box |
| `dullahan-restore-drill.timer` | 1st Sunday, 05:00 | restores the newest backup and verifies it |

```bash
systemctl list-timers 'dullahan*'
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
