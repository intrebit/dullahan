#!/usr/bin/env bash
# Fail loudly if the newest OFF-BOX backup is older than MAX_AGE_HOURS.
#
# Runs as root from dullahan-backup-freshness.timer. `dullahan --selfcheck` only
# reads the local backup directory, and a run whose upload fails keeps its local
# copy — so selfcheck stays green while the off-box copy goes stale. That is not
# hypothetical: from 1 to 16 October 2026 every nightly upload failed with a 401
# and nothing noticed. This lists the remote itself.
#
# It alerts by exiting non-zero; selfcheck watches for this unit being failed and
# mails about it.
set -uo pipefail
BACKUP_ENV=/etc/dullahan/backup.env
# shellcheck source=/dev/null
[[ -f "$BACKUP_ENV" ]] && source "$BACKUP_ENV"
RCLONE_CONFIG="${RCLONE_CONFIG:-/etc/dullahan/rclone.conf}"; export RCLONE_CONFIG
RCLONE_REMOTE="${RCLONE_REMOTE:-}"
MAX_AGE_HOURS="${MAX_AGE_HOURS:-30}"

# No off-box destination is a configuration choice dullahan-backup already warns
# about on every run; failing here too would only add a second alert for it.
if [[ -z "$RCLONE_REMOTE" ]]; then
    echo "RCLONE_REMOTE is unset: there is no off-box copy to check"
    exit 0
fi

# Separate "cannot reach the bucket" from "the bucket is empty". Collapsing the
# two produces an alert that sends you looking in the wrong place, which is the
# failure mode this whole check exists to prevent.
if ! listing="$(rclone lsf --dirs-only "$RCLONE_REMOTE" 2>&1)"; then
    echo "CRITICAL: cannot reach $RCLONE_REMOTE (auth or network). rclone said:" >&2
    printf '%s\n' "$listing" | head -3 >&2
    exit 1
fi
# Only rotation runs count: an out-of-rotation snapshot like chessko-final-<stamp>
# is not evidence that the nightly backup is still landing.
newest="$(printf '%s\n' "$listing" | tr -d '/' \
          | { grep -E '^20[0-9]{6}T[0-9]{6}Z$' || true; } | sort -r | head -1)"
if [[ -z "$newest" ]]; then
    echo "CRITICAL: $RCLONE_REMOTE is reachable but holds no timestamped backup" >&2
    exit 1
fi
ts="${newest%Z}"; ts="${ts/T/ }"
epoch="$(date -u -d "${ts:0:4}-${ts:4:2}-${ts:6:2} ${ts:9:2}:${ts:11:2}:${ts:13:2}" +%s 2>/dev/null)"
[[ -n "$epoch" ]] || { echo "CRITICAL: cannot parse backup stamp $newest" >&2; exit 1; }
age=$(( ( $(date -u +%s) - epoch ) / 3600 ))
if (( age > MAX_AGE_HOURS )); then
    echo "CRITICAL: newest off-box backup $newest is ${age}h old (limit ${MAX_AGE_HOURS}h)" >&2
    exit 1
fi
echo "OK: newest off-box backup $newest is ${age}h old"
