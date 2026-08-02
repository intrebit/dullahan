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
DATABASE_URL=$TEST_DB cargo test --locked   # needs Postgres, see below
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Tests use `#[sqlx::test]`, which creates an ephemeral database **per test** from
`DATABASE_URL`, so that role needs `CREATEDB` and the named database must exist.

Don't assume the system Postgres can provide that — it may be on a non-default
port, lack a role matching your username, or require a password you don't have
(all three are true on the deploy VPS). A throwaway cluster you own outright
needs no root, no daemon, and no container, and never touches a real database:

```bash
export PATH=/usr/lib/postgresql/16/bin:$PATH   # or your version
PGDIR=$(mktemp -d) && SOCK=/tmp/pgs-$USER && mkdir -p "$SOCK"
initdb -D "$PGDIR" -U "$USER" --auth=trust
pg_ctl -D "$PGDIR" -o "-p 5599 -k $SOCK -c listen_addresses=''" -l "$PGDIR/pg.log" start
createdb -h "$SOCK" -p 5599 dullahan_test

export TEST_DB="postgres:///dullahan_test?host=$SOCK&port=5599"
# ... run the tests ...
pg_ctl -D "$PGDIR" stop -m fast
```

Keep the socket directory path short: Postgres caps the full socket path at 107
bytes, and a long `TMPDIR` silently blows past it (the failure is "Unix-domain
socket path is too long" in the log, and a refused connection).

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

## Multi-tenancy (get this right too)

Every row that belongs to a tenant carries `site_id`, and **two independent
gates** protect it. Dropping either one is a cross-tenant leak:

1. **Authorization** — `Scope::can_read_private` / `can_write` in `auth.rs`.
   Stops a caller who may not touch this site at all.
2. **Scoping** — a `site_id = $n` predicate in *every* statement. Stops an
   *authorized* caller reaching another tenant's row through an id they guessed
   or scraped. It is unconditional; it never depends on the registry.

Rules that follow, all learned the hard way:

- **Handlers take `SiteScope`, never a `site` field on their own query struct.**
  The extractor authorizes before it yields a site, so a handler that forgets the
  check has no site to pass to the DB layer and does not compile. This is why
  `site` was *deleted* from the `/stats/*` query structs — don't add it back.
- **Public endpoints still need the predicate.** `blog::view` / `products::view`
  take no auth, but slugs are only unique per site, so without `site_id` one
  anonymous ping to a shared slug increments every tenant's row.
- **A wrong-tenant row id is `404`, not `403`.** 403 would confirm the id exists
  somewhere. Likewise the scope check consults the *credential* before the
  registry, so another tenant's site and a nonexistent one are indistinguishable.
- **An empty `sites` table is permissive** (fresh installs, `#[sqlx::test]`).
  Safe only because scoping is separate from admission: an empty registry yields
  an empty list, never someone else's data. Don't couple the two.
- **`open_mode` is decided once at startup**, never from the live registry — a DB
  blip that empties the cache must not flip a locked-down deploy to open.
- Token hashing is unsalted SHA-256 **on purpose**; see `docs/SECURITY.md` before
  "fixing" it to argon2.
- `ALLOWED_SITES` and `CONTACT_TO_<SITE>` were **removed** on 2026-08-01 (a
  deliberate scope decision, as with the metric cut): the `sites` table is the
  single source of truth for which tenants exist and where their mail goes.

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

- `0001_init.sql` is the whole schema, squashed twice now: first from the original
  seven migrations, then again to fold in tenancy, products, per-site config, and
  the retention index. Never edit it: sqlx stores each applied migration's
  checksum and refuses to start when one changes ("previously applied but has
  been modified"). Schema changes are new files — `0002_*.sql` onward.
- **No more squashing. The schema is frozen as of v0.1.4.** `server/migrations.sha384`
  is the manifest and the `migrations (frozen)` CI job enforces it: no listed file
  may change, and every migration must be listed. Adding one means appending it and
  running `cd server && sha384sum migrations/*.sql > migrations.sha384`.
- **What the last squash taught, kept because it is the general lesson.** Diffing
  `pg_dump --schema-only` of a DB migrated the old way against one built from the
  squashed file is not a formality — doing it caught two unintended diffs that a
  naive checksum rewrite would have baked into production: `analytics_events_ts_idx`
  was missing (so every retention sweep would seq-scan the biggest table), and
  `analytics_events_type_check` still admitted the long-dead `'performance'` type.
  `deploy/rebaseline-migrations.sh` exists to reconcile exactly those, and is the
  supported upgrade path from the pre-freeze chain — recreating the database is no
  longer necessary. Column order differs there too (inline vs `ADD COLUMN`) and is
  deliberately left alone: nothing uses `SELECT *` or positional inserts.
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
  - `(ts)` alone — the one index not led by `site_id`. `db::prune_events` filters
    on `ts` across every tenant, so all the others are useless to it.
- **`db::summary` scans the range once** into a `MATERIALIZED` CTE. Two measured
  choices, both easy to undo by accident: `MATERIALIZED` is required (Postgres would
  otherwise inline the CTE into each reference, restoring the five scans it replaced),
  and `path` is deliberately *excluded* from the CTE — it is up to 2048 bytes, so
  carrying it spilled the materialised set to temp files (10753 temp blocks vs 3679).
  `top_path` therefore still reads the base table, where its own index serves it.

## Gotchas

- **Ingest is asynchronous but bounded** (`ingest::spawn_writer`): `/collect`
  enqueues on a 10k channel and returns `202` before the row is written, so reads
  can lag — tests use a `wait_for_count` poll helper. A *full* queue returns `503`
  and bumps `dullahan_ingest_queue_full_total`; never make it `202`, which would
  promise durability the server isn't providing. One writer task batches up to 128
  events per INSERT via `QueryBuilder::push_values`, retrying a failed batch row by
  row so one poison event costs one event. `main` awaits the writer's `JoinHandle`
  after `serve` returns, which is what makes a restart lossless — don't drop it.
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
