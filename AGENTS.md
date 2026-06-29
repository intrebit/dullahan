# dullahan — contributor guide

GDPR-compliant, cookie-free web analytics. A TypeScript browser client that POSTs
events to a self-hostable Rust ingest + read API backed by Postgres.

For a feature/architecture tour read [`OVERVIEW.md`](OVERVIEW.md). This file is
how to **work on** the repo.

## Layout

| Path | What |
|---|---|
| `tracker/` | Browser SDK (TypeScript, built with tsup, tested with vitest). npm package `dullahan`. |
| `server/` | Ingest + read API (Rust + Axum + sqlx + Postgres). Crate `dullahan`. |
| `server/migrations/` | sqlx SQL migrations, applied automatically on server startup. |
| `deploy/` | Self-host: `install.sh`, systemd unit, `Caddyfile`, env example, dashboard-cred gen. |
| `scripts/` | `loadtest.sh` (oha wrapper). |
| `.github/workflows/ci.yml` | CI: lint (fmt+clippy), server (build+test+boot), tracker (vitest+build), cargo audit. |

## Build / test / lint

**Server** (from `server/`):
```bash
cargo build --locked
DATABASE_URL=postgres://fole@localhost/dullahan_test cargo test --locked   # needs Postgres
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```
Tests use `#[sqlx::test]`, which spins up an ephemeral DB per test from
`DATABASE_URL` (a `dullahan_test` DB with CREATEDB rights must exist locally).

**Tracker** (from `tracker/`):
```bash
npm ci
npm test            # vitest
npm run build        # tsup
npx tsc --noEmit     # tsup/esbuild does NOT typecheck — run tsc to catch type errors CI would
```

CI (`.github/workflows/ci.yml`) runs exactly these four gates; keep them green.

## The data model (get this right)

`analytics_events` has two identifiers at **different grains** — conflating them
promises metrics the data can't support:

- **`view_id`** — regenerated on *every* pageview / SPA navigation, attached to
  all events of that pageload. It is **one page-visit**, NOT a multi-page session.
  Always present (no opt-in). Join an event to its pageview by `view_id`.
- **`visitor_hash`** — `H(daily_salt, site, ip, ua)`, one value per visitor per
  **UTC day** (salt rotates at 00:00 UTC, deleted after 48h). Only set when
  `SESSIONS_ENABLED`. The basis for sessions; **cannot** link across days.

`type ∈ {pageview, event, performance, pageleave}`. Client time is `ts` (bigint
ms, clamped to a sane window on ingest); server receive time is `received_at`.

## Stats API conventions

- **Additive only.** New stats are new fields / params / endpoints; never rename
  or remove. Prove back-compat by leaving existing tests unchanged.
- **Dual-shape pattern**: an endpoint returns a summary *object* with no `dim`,
  and an *array* with `dim=…` (see `vitals`, `engagement`, `sessions`). Reuse it.
- **Honest nulls**: a metric that depends on an opt-in (sessions, scroll/outbound
  tracking) is **omitted** when its source data is absent, never reported as `0`
  (mirrors the `uniqueVisitors` NULLIF). "Not measured" ≠ "zero".
- `/stats/*` is admin-gated (`ADMIN_TOKEN`) + CORS-scoped (`STATS_ORIGINS`).

## Migrations & indexes (lessons)

- sqlx runs each migration in a transaction on startup. A migration starting with
  `-- no-transaction` runs outside one — needed for `CREATE INDEX CONCURRENTLY`
  (see `0006`). On a fresh DB an interrupted CONCURRENTLY build leaves an invalid
  index; drop and re-run.
- **Index decisions were settled with `EXPLAIN ANALYZE`, not intuition** — repeat
  that before adding any index:
  - `0006` `(site_id, received_at)` — needed for `/stats/realtime` (filters server
    receive time; every other index is on client `ts`).
  - **No `(site_id, view_id)` index** — engagement groups by `view_id` over a
    `(site_id, ts)`-bounded scan; the view_id index is ignored on selective ranges
    and loses to a parallel seq-scan on wide ones, while costing random-UUID writes
    on the hot `/collect` path.
  - **No new index for sessions/funnels** — `0005`'s `(site_id, visitor_hash, ts)`
    already serves the sessionization window.

## Gotchas

- **Ingest is fire-and-forget** (`tokio::spawn` in `ingest.rs`): `/collect`
  returns `202` before the row is written, so reads can lag — tests use a
  `wait_for_count` poll helper.
- **Range bucketing uses `ts`** (client, clamped); **realtime uses `received_at`**
  (server). Don't mix them.
- A free-text value bound into SQL (e.g. `tz`) must be charset/length-guarded,
  then a Postgres `22023` error mapped to HTTP 400 (don't 500). See `stats::heatmap`.
- Casting attacker-controlled JSON: guard before `::int` (e.g. scroll `pct` uses a
  `~ '^[0-9]{1,3}$'` filter) so a hostile event prop can't crash a read query.
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

No cookies, no fingerprinting, **no raw IP storage — ever**. Sessions off (default)
⇒ the server reads neither IP nor User-Agent. The salt is daily-rotating and
deleted, by design — so new-vs-returning, retention, and DAU/MAU are **impossible
and intentionally not built**. Don't fake cross-day identity.

## Status

The metrics roadmap is complete (read-only stats, realtime, engagement, sessions,
funnels — all merged). See [`OVERVIEW.md`](OVERVIEW.md) for the full catalog.
