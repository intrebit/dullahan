#!/usr/bin/env bash
# One-time upgrade for a database created BEFORE 0001_init.sql was squashed.
#
# Background. Up to v0.1.3 the schema shipped as 0001_init plus 0002..0005. Those
# were then squashed into a single 0001_init, which changes its checksum — so a
# server built after the squash refuses to start against a database migrated
# before it ("migration 1 was previously applied but has been modified").
#
# The documented fix used to be DROP DATABASE. It does not have to be: the two
# schemas are provably equivalent apart from two objects, so the recorded history
# can be rewritten in place instead, keeping every row. This script does that.
#
# The differences the squash introduced, both verified by diffing a dump of the
# 0001..0005 chain against a dump of the squashed 0001:
#
#   1. analytics_events_ts_idx ON analytics_events (ts) — new. db::prune_events
#      filters on `ts` alone across all tenants, so without it every RETENTION_DAYS
#      sweep is a sequential scan of the largest table you have.
#   2. analytics_events_type_check no longer admits 'performance'. That event type
#      was removed from the code long before; the constraint had simply kept it.
#
# Column order for blog_posts.site_id and products.site_id also differs (inline in
# the squash, ADD COLUMN in the chain). That is cosmetic — nothing in the codebase
# uses SELECT * or positional inserts — and is deliberately left alone, since
# fixing it would mean rewriting both tables for no behavioural gain.
#
# Usage:
#   ./rebaseline-migrations.sh                      # report only, changes nothing
#   ./rebaseline-migrations.sh --apply              # perform the rebaseline
#
# Environment:
#   DB              database to operate on (default: dullahan)
#   MIGRATIONS_DIR  where 0001_init.sql lives (default: ../server/migrations)
#   PG_SUDO         how to run psql as superuser (default: sudo -u postgres)
#
# TAKE A BACKUP FIRST. `dullahan-backup` does this; this script will not run
# without being told to apply, precisely so the report can be read first.

set -euo pipefail

APPLY=0
[[ "${1:-}" == "--apply" ]] && APPLY=1

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DB="${DB:-dullahan}"
MIGRATIONS_DIR="${MIGRATIONS_DIR:-$HERE/../server/migrations}"
PG_SUDO="${PG_SUDO-sudo -u postgres}"

INIT="$MIGRATIONS_DIR/0001_init.sql"
[[ -f "$INIT" ]] || { echo "no 0001_init.sql at $INIT" >&2; exit 1; }

psql_() { $PG_SUDO psql -d "$DB" -v ON_ERROR_STOP=1 "$@"; }
q() { psql_ -tAc "$1"; }

# sqlx stores the SHA-384 of the migration file, unmodified. Verified against a
# real _sqlx_migrations row: the column is 48 bytes and matches `sha384sum`.
WANT="$(sha384sum "$INIT" | cut -d' ' -f1)"

echo "database:        $DB"
echo "0001_init.sql:   $INIT"
echo "wanted checksum: $WANT"
echo

applied="$(q "SELECT string_agg(version::text, ',' ORDER BY version) FROM _sqlx_migrations")"
have="$(q "SELECT encode(checksum,'hex') FROM _sqlx_migrations WHERE version = 1")"
echo "applied versions: ${applied:-<none>}"
echo "stored checksum:  ${have:-<none>}"
echo

if [[ "$applied" == "1" && "$have" == "$WANT" ]]; then
    echo "Already rebaselined — nothing to do."
    exit 0
fi
if [[ -z "$have" ]]; then
    echo "This database has no migration 1 recorded. That is not the pre-squash" >&2
    echo "state this script upgrades; it is either empty (just start the server) or" >&2
    echo "something else entirely. Refusing to guess." >&2
    exit 1
fi

# The pre-squash chain is versions 1..5. Anything else means an unknown state and
# a guess would be a data-loss bug, so stop and let a human look.
if [[ "$applied" != "1,2,3,4,5" ]]; then
    echo "Expected the pre-squash chain (versions 1,2,3,4,5) but found: $applied" >&2
    echo "Refusing to rebaseline an unrecognised migration history." >&2
    exit 1
fi

# Blocking condition, checked before anything is changed: the tightened CHECK
# cannot be added while rows violate it.
legacy="$(q "SELECT count(*) FROM analytics_events WHERE type = 'performance'")"
echo "rows with type='performance': $legacy"
if [[ "$legacy" != "0" ]]; then
    echo
    echo "These rows would violate the tightened analytics_events_type_check." >&2
    echo "'performance' events have not been produced for several versions, so these" >&2
    echo "are historical. Decide explicitly, then re-run:" >&2
    echo "    DELETE FROM analytics_events WHERE type = 'performance';" >&2
    exit 1
fi

has_index="$(q "SELECT count(*) FROM pg_indexes WHERE tablename='analytics_events' AND indexname='analytics_events_ts_idx'")"
echo "analytics_events_ts_idx present: $has_index"

if [[ "$APPLY" != "1" ]]; then
    cat <<EOF

Report only — nothing has been changed. With --apply this would:
  $([[ "$has_index" == "0" ]] && echo "* CREATE INDEX CONCURRENTLY analytics_events_ts_idx ON analytics_events (ts)" || echo "* (index already present)")
  * replace analytics_events_type_check, dropping 'performance'
  * DELETE FROM _sqlx_migrations WHERE version > 1
  * UPDATE _sqlx_migrations SET checksum = <sha384 above> WHERE version = 1

No rows in any application table are read or written. Take a backup anyway:
    dullahan-backup
EOF
    exit 0
fi

echo
echo "==> applying"

if [[ "$has_index" == "0" ]]; then
    echo "  creating analytics_events_ts_idx (CONCURRENTLY — no write lock)"
    # CONCURRENTLY cannot run inside a transaction block, hence its own psql call.
    # On a large table this takes a while and that is fine: the server keeps
    # serving throughout, which a plain CREATE INDEX would not allow.
    psql_ -c "CREATE INDEX CONCURRENTLY IF NOT EXISTS analytics_events_ts_idx ON analytics_events (ts)"
fi

echo "  tightening analytics_events_type_check"
# NOT VALID then VALIDATE: adding a validated CHECK takes ACCESS EXCLUSIVE for a
# full scan, which would stall ingest on a big table. NOT VALID is instant and
# still enforced for new rows; VALIDATE then scans under a weaker lock.
psql_ <<'SQL'
BEGIN;
ALTER TABLE analytics_events DROP CONSTRAINT IF EXISTS analytics_events_type_check;
ALTER TABLE analytics_events ADD CONSTRAINT analytics_events_type_check
    CHECK (type = ANY (ARRAY['pageview'::text, 'event'::text, 'pageleave'::text])) NOT VALID;
COMMIT;
SQL
psql_ -c "ALTER TABLE analytics_events VALIDATE CONSTRAINT analytics_events_type_check"

echo "  rewriting _sqlx_migrations"
# One transaction: a history with the new checksum but the old extra rows would
# still refuse to boot, so these two statements must not be separable.
psql_ <<SQL
BEGIN;
DELETE FROM _sqlx_migrations WHERE version > 1;
UPDATE _sqlx_migrations SET checksum = decode('$WANT', 'hex') WHERE version = 1;
COMMIT;
SQL

echo
echo "==> verifying"
after_applied="$(q "SELECT string_agg(version::text, ',' ORDER BY version) FROM _sqlx_migrations")"
after_sum="$(q "SELECT encode(checksum,'hex') FROM _sqlx_migrations WHERE version = 1")"
after_index="$(q "SELECT count(*) FROM pg_indexes WHERE tablename='analytics_events' AND indexname='analytics_events_ts_idx'")"
after_check="$(q "SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname='analytics_events_type_check'")"

fail=0
[[ "$after_applied" == "1" ]] || { echo "  FAIL: versions are now '$after_applied'" >&2; fail=1; }
[[ "$after_sum" == "$WANT" ]] || { echo "  FAIL: checksum did not take" >&2; fail=1; }
[[ "$after_index" == "1" ]] || { echo "  FAIL: analytics_events_ts_idx missing" >&2; fail=1; }
[[ "$after_check" == *"'performance'"* ]] && { echo "  FAIL: CHECK still admits 'performance'" >&2; fail=1; }
((fail)) && exit 1

echo "  versions:  $after_applied"
echo "  checksum:  $after_sum"
echo "  ts index:  present"
echo "  type check: $after_check"
echo
echo "Rebaseline complete. Restart the server; migrations should be a no-op:"
echo "    systemctl restart dullahan && journalctl -u dullahan -n 20"
