#!/usr/bin/env bash
# Nightly encrypted Postgres backup, pushed off the box.
#
# Runs as root from dullahan-backup.timer. Reads its settings from
# /etc/dullahan/backup.env (see backup.env.example) so no credential is baked
# into this file or into the unit.
#
# What it produces per run, under $BACKUP_DIR/<UTC timestamp>/:
#   globals.sql.age        roles and grants (pg_dumpall --globals-only)
#   <db>.dump.age          one pg_dump -Fc per database in $BACKUP_DATABASES
#   MANIFEST               sizes + sha256 of each artifact, and the PG version
#
# Design notes:
#   * -Fc (custom format) not plain SQL: it is compressed, and pg_restore can
#     read it selectively, which the restore drill relies on.
#   * age encryption happens *before* upload, so the object store never holds
#     readable analytics data. It also means the private key is what you actually
#     need to guard — see the warning in backup.env.example.
#   * The healthcheck ping is the point of the whole exercise being noticed. A
#     backup cron's normal failure mode is dying quietly months before you look.

set -euo pipefail

CONF="${DULLAHAN_BACKUP_ENV:-/etc/dullahan/backup.env}"
# shellcheck source=/dev/null
[[ -f "$CONF" ]] && source "$CONF"

BACKUP_DIR="${BACKUP_DIR:-/var/backups/dullahan}"
BACKUP_DATABASES="${BACKUP_DATABASES:-dullahan}"
KEEP_DAILY="${KEEP_DAILY:-7}"
KEEP_WEEKLY="${KEEP_WEEKLY:-4}"
KEEP_MONTHLY="${KEEP_MONTHLY:-3}"
AGE_RECIPIENTS_FILE="${AGE_RECIPIENTS_FILE:-/etc/dullahan/backup-recipients.txt}"
RCLONE_REMOTE="${RCLONE_REMOTE:-}"
HEALTHCHECK_BACKUP_URL="${HEALTHCHECK_BACKUP_URL:-}"
# How to reach Postgres as a superuser. The default suits the local-cluster,
# peer-authentication install that install.sh produces. Override it for a managed
# or remote database (`PG_SUDO=` with PGHOST/PGPORT/PGUSER set), which also makes
# this script runnable against a throwaway cluster for testing.
PG_SUDO="${PG_SUDO-sudo -u postgres}"

log() { printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }
die() {
    log "FAILED: $*"
    # Signal the watchdog immediately rather than letting it time out, so the
    # alert names the run that broke instead of only "no ping since yesterday".
    [[ -n "$HEALTHCHECK_BACKUP_URL" ]] &&
        curl -fsS -m 10 --data-raw "$*" "${HEALTHCHECK_BACKUP_URL%/}/fail" >/dev/null 2>&1 || true
    exit 1
}

command -v age >/dev/null 2>&1 || die "age is not installed (apt install age)"
[[ -s "$AGE_RECIPIENTS_FILE" ]] || die "no age recipients at $AGE_RECIPIENTS_FILE"

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
DEST="$BACKUP_DIR/$STAMP"
install -d -m 700 "$DEST"

# A partial directory is worse than none: it looks like a backup to anything
# that only lists filenames. Remove it unless we reach the end cleanly.
trap 'rm -rf "$DEST"' EXIT

encrypt_to() { age --encrypt --recipients-file "$AGE_RECIPIENTS_FILE" --output "$1"; }

log "dumping globals"
$PG_SUDO pg_dumpall --globals-only | encrypt_to "$DEST/globals.sql.age" ||
    die "pg_dumpall --globals-only"

for db in $BACKUP_DATABASES; do
    log "dumping $db"
    # Not piped through a subshell whose exit status would be lost: pipefail plus
    # an explicit || means a failed pg_dump aborts the run rather than shipping a
    # truncated, cheerfully-encrypted file.
    $PG_SUDO pg_dump -Fc --no-owner --no-privileges "$db" |
        encrypt_to "$DEST/$db.dump.age" || die "pg_dump $db"
    # An encrypted empty file is 100-odd bytes of header. Anything that small is
    # a failed dump that pipefail did not catch.
    [[ $(stat -c%s "$DEST/$db.dump.age") -gt 1024 ]] || die "$db dump is implausibly small"
done

log "writing manifest"
{
    echo "created_utc=$STAMP"
    echo "host=$(hostname -f 2>/dev/null || hostname)"
    # `-d postgres` explicitly: without it psql connects to a database named
    # after the invoking user, which need not exist, and the field silently
    # comes out empty just when you want it during a restore.
    echo "pg_version=$($PG_SUDO psql -d postgres -tAc 'SHOW server_version' | tr -d ' ')"
    echo "databases=$BACKUP_DATABASES"
    echo "---"
    (cd "$DEST" && sha256sum ./*.age)
} > "$DEST/MANIFEST"

if [[ -n "$RCLONE_REMOTE" ]]; then
    command -v rclone >/dev/null 2>&1 || die "RCLONE_REMOTE set but rclone is not installed"
    log "uploading to $RCLONE_REMOTE/$STAMP"
    rclone copy --checksum "$DEST" "$RCLONE_REMOTE/$STAMP" || die "rclone upload"
else
    log "WARNING: RCLONE_REMOTE is unset — this backup exists only on this host,"
    log "         which is the same disk it is meant to survive the loss of."
fi

trap - EXIT
chmod -R go-rwx "$DEST"

# Retention: keep the newest KEEP_DAILY runs outright, plus the newest run of
# each of the last KEEP_WEEKLY ISO weeks and KEEP_MONTHLY months. Grandfathering
# by *bucket* rather than by age is what stops a long gap in runs from silently
# expiring every copy you have.
log "pruning local copies (${KEEP_DAILY}d/${KEEP_WEEKLY}w/${KEEP_MONTHLY}m)"
# Only directories whose name parses as one of our timestamps are considered at
# all. Filtering *before* indexing matters: a stray directory in $BACKUP_DIR that
# sorted into the middle would otherwise occupy a KEEP_DAILY slot and quietly
# shorten retention by one. Anything unrecognised is left alone, never deleted.
runs=()
while read -r name; do
    [[ ${#name} -ge 8 ]] && date -u -d "${name:0:8}" >/dev/null 2>&1 && runs+=("$name")
done < <(find "$BACKUP_DIR" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort -r)

declare -A keep=() seen_week=() seen_month=()
for i in "${!runs[@]}"; do
    run="${runs[$i]}"
    day="${run:0:8}"
    ((i < KEEP_DAILY)) && keep["$run"]=1
    week="$(date -u -d "$day" +%G-%V)"
    month="${day:0:6}"
    if [[ -z "${seen_week[$week]:-}" ]]; then
        seen_week["$week"]=1
        ((${#seen_week[@]} <= KEEP_WEEKLY)) && keep["$run"]=1
    fi
    if [[ -z "${seen_month[$month]:-}" ]]; then
        seen_month["$month"]=1
        ((${#seen_month[@]} <= KEEP_MONTHLY)) && keep["$run"]=1
    fi
done
for run in "${runs[@]}"; do
    [[ -n "${keep[$run]:-}" ]] || { log "  removing $run"; rm -rf "$BACKUP_DIR/$run"; }
done

# Object-store retention is deliberately *not* done from here: a compromised or
# buggy backup host must not be able to delete history. Set a lifecycle rule on
# the bucket instead — docs/deploy.md has the numbers.

log "backup complete: $STAMP ($(du -sh "$DEST" | cut -f1))"
[[ -n "$HEALTHCHECK_BACKUP_URL" ]] &&
    curl -fsS -m 10 --data-raw "ok $STAMP" "$HEALTHCHECK_BACKUP_URL" >/dev/null 2>&1 || true
exit 0
