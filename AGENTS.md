# dullahan — developer guide

GDPR-compliant, cookie-free web analytics: a self-hostable Rust ingest + read API
(plus a headless blog + contact endpoint) backed by Postgres. Clients POST events
over plain HTTP — the browser tracker itself lives with the frontend, not here.

For what it does and how to run it read [`README.md`](README.md); for the HTTP
surface read [`docs/api.md`](docs/api.md). This file is how to **work on** the repo.

## Layout

| Path | What |
|---|---|
| `server/` | Ingest + read API (Rust + Axum + sqlx + Postgres). Crate `dullahan`. |
| `server/migrations/` | sqlx SQL migrations, applied automatically on server startup. |
| `deploy/` | Self-host: `install.sh`, systemd unit, `Caddyfile`, env example. |
| `docs/` | `api.md` (HTTP reference), `deploy.md` (config + hardening), `SECURITY.md` (policy). |
| `.github/workflows/ci.yml` | CI: lint (fmt+clippy), server (build+test+boot), cargo audit, docker (build+smoke). |

Pure Rust: the repo ships no JavaScript. The browser tracker that POSTs to
`/collect` lives in the frontend project that consumes this backend.

## Build / test / lint

**Server** (from `server/`):
```bash
cargo build --locked
DATABASE_URL=postgres://$USER@localhost/dullahan_test cargo test --locked   # needs Postgres
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```
Tests use `#[sqlx::test]`, which spins up an ephemeral DB per test from
`DATABASE_URL` (a `dullahan_test` DB with CREATEDB rights must exist locally).

CI runs the above plus `docker` (image build + `/health` smoke). Four gates
(lint, server, audit, docker); keep them green.

## The data model (get this right)

`analytics_events` has two identifiers at **different grains** — conflating them
promises metrics the data can't support:

- **`view_id`** — regenerated on *every* pageview / SPA navigation, attached to
  all events of that pageload. It is **one page-visit**, NOT a multi-page session.
  Always present (no opt-in). Join an event to its pageview by `view_id`.
- **`visitor_hash`** — `H(daily_salt, site, ip, ua)`, one value per visitor per
  **UTC day** (salt rotates at 00:00 UTC, then is pruned after retention). Only set when
  `SESSIONS_ENABLED`. The basis for sessions; **cannot** link across days.

`type ∈ {pageview, event, pageleave}`. `pageleave` carries only a dwell time
(`dur`) for time-on-page — there is no scroll-depth. Client time is `ts` (bigint
ms, clamped to a sane window on ingest); server receive time is `received_at`.

## Stats API conventions

- **Additive within the kept surface.** Day to day, treat the six endpoints below
  as append-only: new stats are new fields / params / endpoints; don't rename or
  remove them, and prove back-compat by leaving existing tests unchanged.
- **Removal is allowed as a deliberate scope decision.** On 2026-07-26 the metric
  surface was intentionally trimmed — `/stats/vitals`, `/stats/heatmap`,
  `/stats/engagement`, `/stats/sessions`, and `/stats/funnel` were removed (along
  with web-vitals, scroll, and outbound tracking). This was a positioning call:
  dullahan is **not** trying to match Plausible/competitors on breadth; it keeps a
  small, sharp core. So "never remove" is a rule of thumb about not churning the
  *kept* endpoints, not a ban on cutting scope when it's a considered decision.
- **The kept endpoints**: `/stats/summary`, `/stats/timeseries`, `/stats/top`,
  `/stats/events`, `/stats/channels`, `/stats/realtime`.
- **Dual-shape pattern**: an endpoint returns a summary *object* with no `dim`,
  and an *array* with `dim=…`. Reuse it.
- **Honest nulls**: a metric that depends on an opt-in (sessions) is **omitted**
  when its source data is absent, never reported as `0` (mirrors the
  `uniqueVisitors` NULLIF). "Not measured" ≠ "zero".
- `/stats/*` is admin-gated (`ADMIN_TOKEN`) + CORS-scoped (`STATS_ORIGINS`).

## Migrations & indexes (lessons)

- `0001_init.sql` is the whole schema, squashed from the original seven
  migrations. Never edit it: sqlx stores each applied migration's checksum and
  refuses to start when one changes ("previously applied but has been modified").
  Schema changes are new files — `0002_*.sql` onward.
- sqlx runs each migration in a transaction on startup. A migration starting with
  `-- no-transaction` runs outside one, which is what `CREATE INDEX CONCURRENTLY`
  needs. The squashed init builds its indexes on an empty table, so it wants
  neither; reach for CONCURRENTLY only when indexing a table that already has
  live traffic, and know that an interrupted build leaves an *invalid* index that
  `IF NOT EXISTS` silently skips (drop it, then re-run).
- **Index decisions were settled with `EXPLAIN ANALYZE`, not intuition** — repeat
  that before adding any index:
  - `(site_id, received_at)` — needed for `/stats/realtime` (filters server
    receive time; every other index is on client `ts`).
  - **No `(site_id, view_id)` index** — time-on-page groups by `view_id` over a
    `(site_id, ts)`-bounded scan; the view_id index is ignored on selective ranges
    and loses to a parallel seq-scan on wide ones, while costing random-UUID writes
    on the hot `/collect` path.

## Gotchas

- **Ingest is fire-and-forget** (`tokio::spawn` in `ingest.rs`): `/collect`
  returns `202` before the row is written, so reads can lag — tests use a
  `wait_for_count` poll helper.
- **Range bucketing uses `ts`** (client, clamped); **realtime uses `received_at`**
  (server). Don't mix them.
- A free-text value bound into SQL must be charset/length-guarded, then a Postgres
  `22023` error mapped to HTTP 400 (don't 500).
- Casting attacker-controlled JSON: guard before `::int` with a
  `~ '^[0-9]{1,3}$'`-style filter so a hostile event prop can't crash a read query.
- A numeric `top` dimension needs `column()` to return `"<col>::text"` (the generic
  query reads `key` as text, else `"(none)"`).
- `percentile_cont` over a `bigint` expression needs `::float8`.

## Workflow

- **Feature branch per change** (`feat/…`, `fix/…`, `docs/…`); never push to
  `master`. Open a PR with `gh pr create`. **PRs merge by squash**; GitHub appends
  ` (#N)` to the title (repo convention).
- Commit messages containing backticks: use `git commit -F <file>` /
  `gh pr ... --body-file` (zsh runs backticked words in `-m`).
- Ask before: DB migrations on a live deploy, deleting branches, env-var changes,
  deploys.

## Privacy invariants (never break)

No cookies, no fingerprinting, **no raw IP storage — ever**. The selected client
IP is processed transiently for rate limiting. Sessions off (default) ⇒ `/collect`
otherwise uses neither IP nor User-Agent. The salt is daily-rotating and pruned,
by design — so new-vs-returning, retention, and DAU/MAU are **impossible and
intentionally not built**. Don't fake cross-day identity.

## Status

The metric surface is a deliberately small core: read-only stats (summary,
timeseries, top, events, channels) plus realtime. Engagement, sessions, and
funnels were built and then **removed** on 2026-07-26 as a scope decision (see
*Stats API conventions*) — don't reintroduce them without a positioning reason.
See [`docs/api.md`](docs/api.md) for the full catalog.
