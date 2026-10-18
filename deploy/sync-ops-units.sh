#!/usr/bin/env bash
# Reinstall the ops scripts and systemd units from this checkout, then prove the
# backup actually runs.
#
#   sudo deploy/sync-ops-units.sh [--verify] [--drill]
#
#     --verify   run dullahan-backup through systemd afterwards and confirm a
#                new run directory landed (recommended — see below)
#     --drill    also run the monthly restore drill (slow: restores a full dump
#                into a throwaway database)
#
# install.sh is the only thing that installs these files, and it is far too heavy
# to re-run for a one-line change (apt, rustup, a release build, Caddy). This does
# just the ops half, idempotently, and reports what it changed.
#
# It does NOT touch /opt/dullahan or restart the API server — that is
# dullahan-deploy's job — and it does NOT overwrite /etc/dullahan/backup.env,
# which holds local settings.
#
# On --verify, the backup is started with `systemctl start`, NEVER by calling
# /usr/local/bin/dullahan-backup directly. That distinction is the whole reason
# this script exists: a hand-run gets a normal shell, while the timer gets the
# unit's sandbox (NoNewPrivileges, ProtectHome, ProtectSystem, PrivateTmp). A
# backup that works by hand and fails every night is what happens when you only
# ever test the former.

set -euo pipefail

verify=0
drill=0
for arg in "$@"; do
    case $arg in
        --verify) verify=1 ;;
        --drill)  drill=1; verify=1 ;;
        -h|--help) sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) printf 'unknown flag: %s\n' "$arg" >&2; exit 2 ;;
    esac
done

[[ $EUID -eq 0 ]] || { echo "must run as root (sudo $0)" >&2; exit 1; }

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$REPO_DIR/deploy"
BACKUP_DIR_DEFAULT=/var/backups/dullahan

ok()      { printf '  \033[32m✓\033[0m %s\n' "$*"; }
changed() { printf '  \033[36m→\033[0m %s\n' "$*"; }
warn()    { printf '  \033[33m!\033[0m %s\n' "$*"; }
bad()     { printf '  \033[31m✗\033[0m %s\n' "$*"; }

changes=0
server_unit_changed=0

# sync <src> <dst> <mode> — install only when the content differs, and say so.
sync() {
    local src=$1 dst=$2 mode=$3
    if [[ ! -f $src ]]; then bad "missing from checkout: $src"; return 1; fi
    if [[ -f $dst ]] && cmp -s "$src" "$dst"; then
        ok "$(basename "$dst") unchanged"
        return 0
    fi
    install -m "$mode" -o root -g root "$src" "$dst"
    changed "$(basename "$dst") updated"
    changes=$((changes + 1))
    [[ $(basename "$dst") == dullahan.service ]] && server_unit_changed=1
    return 0
}

printf '\nScripts (/usr/local/bin)\n'
sync "$SRC/dullahan-deploy.sh"        /usr/local/bin/dullahan-deploy        755
sync "$SRC/dullahan-backup.sh"        /usr/local/bin/dullahan-backup        755
sync "$SRC/dullahan-restore-drill.sh" /usr/local/bin/dullahan-restore-drill 755
sync "$SRC/dullahan-backup-freshness.sh" /usr/local/bin/dullahan-backup-freshness 755
if [[ -f $SRC/register-site.sh ]]; then
    sync "$SRC/register-site.sh" /usr/local/bin/dullahan-register-site 755
fi

printf '\nUnits (/etc/systemd/system)\n'
units=(
    dullahan.service
    dullahan-backup.service dullahan-backup.timer
    dullahan-backup-freshness.service dullahan-backup-freshness.timer
    dullahan-restore-drill.service dullahan-restore-drill.timer
    dullahan-digest.service dullahan-digest.timer
    dullahan-selfcheck.service dullahan-selfcheck.timer
)
for u in "${units[@]}"; do sync "$SRC/$u" "/etc/systemd/system/$u" 644; done

if (( changes > 0 )); then
    systemctl daemon-reload
    changed "daemon-reload"
else
    ok "nothing to reload"
fi

# dullahan.service may have changed, but restarting the API server is a deploy
# decision, not a side effect of syncing units. (Checking NeedDaemonReload here
# would always say "no" — the reload above just cleared it.)
if (( server_unit_changed )); then
    warn "dullahan.service changed — restart it yourself when convenient:"
    warn "  systemctl restart dullahan"
fi

# ---------------------------------------------------------------- local config

printf '\nLocal config (/etc/dullahan/backup.env)\n'
CONF=/etc/dullahan/backup.env
if [[ -f $CONF ]]; then
    ok "present (not overwritten)"
    # An old PG_SUDO=sudo... in here would silently defeat the script default and
    # re-break every timer-driven run, which is exactly the bug this script ships.
    if grep -qE '^[[:space:]]*PG_SUDO=.*sudo' "$CONF"; then
        bad "PG_SUDO in $CONF still uses sudo — it cannot work under"
        bad "NoNewPrivileges=true. Remove that line (the script's own default is"
        bad "setpriv) or set it to:"
        bad '  PG_SUDO="setpriv --reuid=postgres --regid=postgres --clear-groups"'
    else
        ok "no PG_SUDO override that would re-break the sandbox"
    fi
    # shellcheck disable=SC1090
    remote=$(grep -E '^[[:space:]]*RCLONE_REMOTE=' "$CONF" | tail -1 | cut -d= -f2- | tr -d '"' || true)
    if [[ -n ${remote:-} ]]; then
        ok "RCLONE_REMOTE is set ($remote) — backups go off-box"
    else
        warn "RCLONE_REMOTE is unset: backups exist only on this disk, which is the"
        warn "same disk they are meant to survive the loss of"
    fi
else
    warn "$CONF does not exist — install.sh normally creates it from backup.env.example"
fi

# ------------------------------------------------------------------- verify

if (( verify == 0 )); then
    printf '\n  re-run with --verify to start the backup through systemd and check it landed\n\n'
    exit 0
fi

BACKUP_DIR=""
if [[ -f $CONF ]]; then
    BACKUP_DIR=$(grep -E '^[[:space:]]*BACKUP_DIR=' "$CONF" | tail -1 | cut -d= -f2- | tr -d '"' || true)
fi
BACKUP_DIR=${BACKUP_DIR:-$BACKUP_DIR_DEFAULT}

newest() {
    find "$1" -mindepth 1 -maxdepth 1 -type d -printf '%T@ %f\n' 2>/dev/null |
        sort -rn | head -1 | cut -d' ' -f2-
}

printf '\nBackup run (through systemd, with the unit sandbox)\n'
before=$(newest "$BACKUP_DIR")
printf '  newest before: %s\n' "${before:-<none>}"

rc=0
systemctl start dullahan-backup.service || rc=$?

after=$(newest "$BACKUP_DIR")
if [[ -n $after && $after != "$before" ]]; then
    ok "new run landed: $after"
    ls -la "$BACKUP_DIR/$after" | sed 's/^/    /'
else
    bad "no new run directory in $BACKUP_DIR"
    bad "the script deletes partial runs on failure, so look at the log:"
    bad "  journalctl -u dullahan-backup.service -n 40"
fi
if (( rc == 0 )); then
    ok "unit exited 0"
else
    bad "unit exited $rc (a PARTIAL run also exits non-zero — read the log)"
fi
journalctl -u dullahan-backup.service --since '-10 min' --no-pager -o cat 2>/dev/null |
    tail -20 | sed 's/^/    /'

if (( drill )); then
    printf '\nRestore drill (slow)\n'
    drc=0
    systemctl start dullahan-restore-drill.service || drc=$?
    (( drc == 0 )) && ok "drill exited 0" || bad "drill exited $drc"
    journalctl -u dullahan-restore-drill.service --since '-30 min' --no-pager -o cat 2>/dev/null |
        tail -25 | sed 's/^/    /'
fi

printf '\nTimers\n'
systemctl list-timers 'dullahan*' --all --no-pager | sed 's/^/  /'
printf '\n'
