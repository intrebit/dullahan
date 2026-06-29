#!/usr/bin/env bash
# Generate credentials for a dashboard that consumes dullahan:
#   - ADMIN_USERNAME      (plain)
#   - ADMIN_PASSWORD_HASH (argon2id PHC string)
#   - SESSION_SECRET      (32 random bytes, hex)
#
# Usage:
#   ./deploy/gen-dashboard-creds.sh                    # interactive prompt
#   ./deploy/gen-dashboard-creds.sh -u admin           # set username, prompt for password
#   ./deploy/gen-dashboard-creds.sh -u admin -p secret # both via flags (avoid in shared shells)
#   ./deploy/gen-dashboard-creds.sh -u admin -o .env   # write to file (mode 600) instead of stdout
#
# Requires: openssl, argon2.
#   macOS:  brew install argon2
#   Debian: apt install argon2

set -euo pipefail

USERNAME=""
PASSWORD=""
OUTFILE=""

while getopts "u:p:o:h" opt; do
    case $opt in
        u) USERNAME=$OPTARG ;;
        p) PASSWORD=$OPTARG ;;
        o) OUTFILE=$OPTARG ;;
        h) sed -n '2,15p' "$0"; exit 0 ;;
        *) exit 2 ;;
    esac
done

err() { printf '%s\n' "$*" >&2; }

command -v openssl >/dev/null || { err "openssl not found"; exit 1; }
command -v argon2  >/dev/null || {
    err "argon2 not found. Install: brew install argon2  |  apt install argon2"
    exit 1
}

if [[ -z $USERNAME ]]; then
    read -rp "admin username: " USERNAME
fi
[[ -n $USERNAME ]] || { err "username required"; exit 1; }

if [[ -z $PASSWORD ]]; then
    read -rsp "admin password: " PASSWORD; echo
    read -rsp "confirm:        " CONFIRM;  echo
    [[ $PASSWORD == "$CONFIRM" ]] || { err "passwords do not match"; exit 1; }
fi
[[ ${#PASSWORD} -ge 8 ]] || { err "password must be at least 8 characters"; exit 1; }

# argon2id, 64 MiB memory (-m 16 = 2^16 KiB), 3 iterations, PHC-encoded output (-e),
# 16-byte salt. Matches OWASP 2024 baseline.
SALT=$(openssl rand -hex 16)
HASH=$(printf '%s' "$PASSWORD" | argon2 "$SALT" -id -t 3 -m 16 -p 1 -e)
SESSION_SECRET=$(openssl rand -hex 32)

OUT=$(cat <<EOF
ADMIN_USERNAME=$USERNAME
ADMIN_PASSWORD_HASH=$HASH
SESSION_SECRET=$SESSION_SECRET
EOF
)

if [[ -n $OUTFILE ]]; then
    umask 077
    printf '%s\n' "$OUT" > "$OUTFILE"
    chmod 600 "$OUTFILE"
    err "wrote $OUTFILE (mode 600)"
else
    printf '%s\n' "$OUT"
fi
