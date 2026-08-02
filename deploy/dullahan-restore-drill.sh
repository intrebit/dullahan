#!/usr/bin/env bash
# Monthly proof that the backups can actually be restored.
#
# Runs as root from dullahan-restore-drill.timer. Takes the newest backup —
# from the object store when one is configured, because that is the copy that
# would survive losing this host, and an untested remote copy is the whole risk
# this drill exists to retire — decrypts it, restores into a throwaway database,
# checks the result is a plausible dullahan schema with data in it, and drops it.
#
# A backup system whose restore path has never run is a hope, not a backup. This
# is the difference between "we take backups" and "we can recover".

set -euo pipefail

CONF="${DULLAHAN_BACKUP_ENV:-/etc/dullahan/backup.env}"
# shellcheck source=/dev/null
[[ -f "$CONF" ]] && source "$CONF"

BACKUP_DIR="${BACKUP_DIR:-/var/backups/dullahan}"
AGE_IDENTITY_FILE="${AGE_IDENTITY_FILE:-/etc/dullahan/backup-identity.txt}"
RCLONE_REMOTE="${RCLONE_REMOTE:-}"
HEALTHCHECK_DRILL_URL="${HEALTHCHECK_DRILL_URL:-}"
DRILL_DB="${DRILL_DB:-dullahan_restore_drill}"
# The tables 0001_init creates. A restore that comes back without one of these
# succeeded at the file level and still lost the schema.
DRILL_EXPECT_TABLES="${DRILL_EXPECT_TABLES:-analytics_events blog_posts daily_salts products sites site_config}"
DRILL_SOURCE_DB="${DRILL_SOURCE_DB:-dullahan}"
# See dullahan-backup.sh: override for a managed/remote database, or to run this
# drill against a throwaway cluster.
PG_SUDO="${PG_SUDO-sudo -u postgres}"

log() { printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }

WORK="$(mktemp -d /tmp/dullahan-drill.XXXXXX)"
cleanup() {
    rm -rf "$WORK"
    $PG_SUDO dropdb --if-exists "$DRILL_DB" 2>/dev/null || true
}
trap cleanup EXIT

die() {
    log "DRILL FAILED: $*"
    [[ -n "$HEALTHCHECK_DRILL_URL" ]] &&
        curl -fsS -m 10 --data-raw "$*" "${HEALTHCHECK_DRILL_URL%/}/fail" >/dev/null 2>&1 || true
    exit 1
}

command -v age >/dev/null 2>&1 || die "age is not installed"
[[ -s "$AGE_IDENTITY_FILE" ]] || die "no age identity at $AGE_IDENTITY_FILE — cannot decrypt anything"

if [[ -n "$RCLONE_REMOTE" ]]; then
    command -v rclone >/dev/null 2>&1 || die "RCLONE_REMOTE set but rclone is missing"
    log "listing $RCLONE_REMOTE"
    NEWEST="$(rclone lsf --dirs-only "$RCLONE_REMOTE" 2>/dev/null | tr -d '/' | sort -r | head -1)"
    [[ -n "$NEWEST" ]] || die "no backups found at $RCLONE_REMOTE"
    log "restoring from remote copy $NEWEST"
    rclone copy "$RCLONE_REMOTE/$NEWEST" "$WORK" || die "could not download $NEWEST"
else
    NEWEST="$(find "$BACKUP_DIR" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort -r | head -1)"
    [[ -n "$NEWEST" ]] || die "no backups found in $BACKUP_DIR"
    log "no remote configured; restoring from LOCAL copy $NEWEST (this does not"
    log "prove off-box recoverability — configure RCLONE_REMOTE)"
    cp "$BACKUP_DIR/$NEWEST"/* "$WORK/"
fi

DUMP="$WORK/$DRILL_SOURCE_DB.dump.age"
[[ -f "$DUMP" ]] || die "$NEWEST contains no $DRILL_SOURCE_DB.dump.age"

# Verify the manifest checksum before trusting the bytes: this catches truncated
# uploads and bit-rot in the object store, which is a different failure from
# "pg_restore did not like it".
if [[ -f "$WORK/MANIFEST" ]]; then
    log "verifying checksums against MANIFEST"
    (cd "$WORK" && grep -F '.age' MANIFEST | sha256sum --check --status) ||
        die "checksum mismatch — the stored backup does not match its manifest"
fi

log "decrypting"
age --decrypt --identity "$AGE_IDENTITY_FILE" --output "$WORK/restore.dump" "$DUMP" ||
    die "decryption failed — is AGE_IDENTITY_FILE the key these were encrypted to?"

log "restoring into $DRILL_DB"
$PG_SUDO dropdb --if-exists "$DRILL_DB"
$PG_SUDO createdb "$DRILL_DB"
# The dump is fed on **stdin**, not passed as a path.
#
# pg_restore runs as the postgres user (via $PG_SUDO) while $WORK is a mktemp -d
# owned by root with mode 0700, so postgres cannot open a file inside it:
# "could not open input file ... Permission denied". The obvious fixes are worse
# than this one — chmod 755 would expose a decrypted copy of the whole database to
# every local user for the duration of the drill, and chown/chgrp juggling adds two
# more things to get wrong. Redirecting means the *root* shell opens the file and
# the sudo'd process inherits the descriptor, so the plaintext dump stays 0600 and
# root-owned and is never readable by anyone else.
#
# pg_restore reads a -Fc archive from a non-seekable stream fine; only parallel
# (-j) and selective restores need to seek, and this does neither.
#
# It warns about ownership and extensions it cannot recreate as-is; those are
# expected here and not what the drill tests, so only a hard failure to produce the
# objects below counts.
$PG_SUDO pg_restore --no-owner --no-privileges --dbname "$DRILL_DB" \
    < "$WORK/restore.dump" 2>"$WORK/restore.log" || log "pg_restore reported issues (see below)"

missing=()
for t in $DRILL_EXPECT_TABLES; do
    $PG_SUDO psql -d "$DRILL_DB" -tAc \
        "SELECT 1 FROM information_schema.tables WHERE table_schema='public' AND table_name='$t'" |
        grep -q 1 || missing+=("$t")
done
if ((${#missing[@]})); then
    tail -20 "$WORK/restore.log" >&2 || true
    die "restored database is missing tables: ${missing[*]}"
fi

# Row counts, compared against the SOURCE database rather than against zero.
#
# "The restore must be non-empty" is the wrong assertion. A faithful restore of an
# empty database *is* empty, so a deploy that has not started collecting yet would
# fail this drill on every run and therefore never get its backups scheduled —
# precisely inverting the point. What matters is fidelity: if the source holds rows,
# the restore must hold rows too.
#
# Exact equality is not available either: the backup is older than now, and
# retention prunes the source, so the counts legitimately drift in both directions.
events="$($PG_SUDO psql -d "$DRILL_DB" -tAc 'SELECT count(*) FROM analytics_events')"
sites="$($PG_SUDO psql -d "$DRILL_DB" -tAc 'SELECT count(*) FROM sites')"
applied="$($PG_SUDO psql -d "$DRILL_DB" -tAc \
    "SELECT count(*) FROM _sqlx_migrations WHERE success" 2>/dev/null || echo 0)"
src_events="$($PG_SUDO psql -d "$DRILL_SOURCE_DB" -tAc \
    'SELECT count(*) FROM analytics_events' 2>/dev/null || echo unknown)"
src_sites="$($PG_SUDO psql -d "$DRILL_SOURCE_DB" -tAc \
    'SELECT count(*) FROM sites' 2>/dev/null || echo unknown)"

log "restored:   analytics_events=$events sites=$sites migrations_applied=$applied"
log "source now: analytics_events=$src_events sites=$src_sites"

[[ "$applied" -ge 1 ]] || die "restored database has no successful migration rows"

if [[ "$src_events" != "unknown" && "$src_events" -gt 0 && "$events" -eq 0 ]]; then
    die "source holds $src_events events but the restore produced none — the dump is not capturing table data"
fi
if [[ "$src_sites" != "unknown" && "$src_sites" -gt 0 && "$sites" -eq 0 ]]; then
    die "source holds $src_sites sites but the restore produced none"
fi

if [[ "$events" -eq 0 && "$sites" -eq 0 ]]; then
    # Say what was and was not proven. A drill that reports success without
    # qualification, on a database with nothing in it, is the kind of green tick
    # that gets trusted in a real recovery and shouldn't be.
    log "NOTE: source and restore are both empty, so this run proved the schema, the"
    log "      encryption round-trip and the whole pipeline — but NOT row fidelity."
    log "      Row checks begin automatically once there is data to check."
fi

log "DRILL PASSED for backup $NEWEST"
[[ -n "$HEALTHCHECK_DRILL_URL" ]] &&
    curl -fsS -m 10 --data-raw "restored $NEWEST events=$events" "$HEALTHCHECK_DRILL_URL" >/dev/null 2>&1 || true
exit 0
