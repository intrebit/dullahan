#!/usr/bin/env bash
# One-shot installer for dullahan on a fresh Debian/Ubuntu box.
# Run AS ROOT. Requires: a domain pointing at this box, ports 80/443 open.
#
# Usage:
#   sudo DOMAIN=analytics.example.com ACME_EMAIL=you@example.com \
#        PG_PASSWORD=$(openssl rand -hex 24) \
#        ./install.sh
#
# What it does:
#   1. apt install postgresql, caddy, build deps
#   2. create OS user `dullahan`, dir /opt/dullahan
#   3. create PG role + DB
#   4. install Rust toolchain (rustup, user-local)
#   5. build dullahan from ../server (release)
#   6. drop binary into /opt/dullahan + write env file
#   7. install systemd unit + Caddyfile, enable + start
#
# Re-running is safe: each step checks for existing state.

set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo "must run as root" >&2; exit 1
fi
: "${DOMAIN:?DOMAIN env var required}"
: "${ACME_EMAIL:?ACME_EMAIL env var required}"
: "${PG_PASSWORD:?PG_PASSWORD env var required}"

ADMIN_TOKEN="${ADMIN_TOKEN:-}"
PG_DB="${PG_DB:-dullahan}"
PG_USER="${PG_USER:-dullahan}"

ENV_FILE="/opt/dullahan/dullahan.env"

# Re-runs reuse the existing token. Only generate on first install.
if [[ -z "$ADMIN_TOKEN" ]]; then
    if [[ -f "$ENV_FILE" ]] && grep -q '^ADMIN_TOKEN=' "$ENV_FILE"; then
        ADMIN_TOKEN="$(grep '^ADMIN_TOKEN=' "$ENV_FILE" | cut -d= -f2-)"
    else
        ADMIN_TOKEN="$(openssl rand -hex 24)"
        ADMIN_TOKEN_GENERATED=1
    fi
fi

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SERVER_DIR="$REPO_DIR/server"

echo "==> apt packages"
apt-get update -qq
apt-get install -y --no-install-recommends \
    postgresql ca-certificates curl debian-keyring debian-archive-keyring \
    apt-transport-https build-essential pkg-config libssl-dev git gettext-base

if ! command -v caddy >/dev/null 2>&1; then
    echo "==> installing caddy"
    curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
    curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' > /etc/apt/sources.list.d/caddy-stable.list
    apt-get update -qq
    apt-get install -y caddy
fi

echo "==> os user + dirs"
id dullahan >/dev/null 2>&1 || useradd --system --create-home --home /opt/dullahan --shell /usr/sbin/nologin dullahan
install -d -o dullahan -g dullahan -m 750 /opt/dullahan

echo "==> postgres role + db"
sudo -u postgres psql -tAc "SELECT 1 FROM pg_roles WHERE rolname='${PG_USER}'" | grep -q 1 \
    || sudo -u postgres psql -c "CREATE ROLE ${PG_USER} LOGIN PASSWORD '${PG_PASSWORD}'"
sudo -u postgres psql -tAc "SELECT 1 FROM pg_database WHERE datname='${PG_DB}'" | grep -q 1 \
    || sudo -u postgres createdb -O "${PG_USER}" "${PG_DB}"

echo "==> rust toolchain (user-local for dullahan)"
if ! sudo -u dullahan test -x /opt/dullahan/.cargo/bin/cargo; then
    sudo -u dullahan bash -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal'
fi

echo "==> building release binary (this takes a few minutes)"
install -d -o dullahan -g dullahan /opt/dullahan/build-src
# Pure Rust: the build needs only the crate sources and migrations — no Node,
# no client build, no vendored assets.
cp -r "$SERVER_DIR/Cargo.toml" "$SERVER_DIR/src" "$SERVER_DIR/migrations" /opt/dullahan/build-src/
chown -R dullahan:dullahan /opt/dullahan/build-src

sudo -u dullahan bash -c "cd /opt/dullahan/build-src && /opt/dullahan/.cargo/bin/cargo build --release --bin dullahan"
install -o dullahan -g dullahan -m 755 /opt/dullahan/build-src/target/release/dullahan /opt/dullahan/dullahan
rm -rf /opt/dullahan/build-src/target

echo "==> migrations dir (sqlx reads from CWD/migrations on boot)"
rm -rf /opt/dullahan/migrations
cp -r "$SERVER_DIR/migrations" /opt/dullahan/migrations
chown -R dullahan:dullahan /opt/dullahan/migrations

echo "==> env file"
PG_PORT=$(pg_lsclusters --no-header | awk '$4=="online"{print $3; exit}')
PG_PORT="${PG_PORT:-5432}"
if [[ ! -f "$ENV_FILE" ]]; then
    cat > "$ENV_FILE" <<EOF
DATABASE_URL=postgres://${PG_USER}:${PG_PASSWORD}@127.0.0.1:${PG_PORT}/${PG_DB}
BIND_ADDR=127.0.0.1:3011
ADMIN_TOKEN=${ADMIN_TOKEN}
TRUST_PROXY_HEADERS=1
RUST_LOG=info,sqlx=warn
EOF
    chown dullahan:dullahan "$ENV_FILE"
    chmod 600 "$ENV_FILE"
fi

echo "==> systemd units"
# Every unit the project ships, not just the server. The digest timer used to be
# a copy-these-files-by-hand step in the docs, which is exactly why it was never
# running anywhere: a shipped feature that the installer does not install is a
# feature that does not happen.
for unit in dullahan.service \
            dullahan-digest.service dullahan-digest.timer \
            dullahan-selfcheck.service dullahan-selfcheck.timer \
            dullahan-backup.service dullahan-backup.timer \
            dullahan-restore-drill.service dullahan-restore-drill.timer; do
    install -m 644 "$REPO_DIR/deploy/$unit" "/etc/systemd/system/$unit"
done
systemctl daemon-reload
systemctl enable --now dullahan

echo "==> helper scripts"
install -m 755 -o root -g root "$REPO_DIR/deploy/dullahan-deploy.sh" /usr/local/bin/dullahan-deploy
install -m 755 -o root -g root "$REPO_DIR/deploy/dullahan-backup.sh" /usr/local/bin/dullahan-backup
install -m 755 -o root -g root "$REPO_DIR/deploy/dullahan-restore-drill.sh" /usr/local/bin/dullahan-restore-drill

echo "==> backup config"
install -d -m 700 -o root -g root /etc/dullahan
if [[ ! -f /etc/dullahan/backup.env ]]; then
    install -m 600 -o root -g root "$REPO_DIR/deploy/backup.env.example" /etc/dullahan/backup.env
    BACKUP_ENV_CREATED=1
fi
# `age` is what makes an off-box copy safe to store; without it the backup script
# refuses to run rather than uploading readable analytics data. Failure here is
# reported, not fatal: the server is already installed and serving by this point,
# and aborting would leave a working deploy looking like a failed install.
if ! command -v age >/dev/null 2>&1; then
    apt-get install -y --no-install-recommends age || true
fi

# Generate the encryption keypair on first install so the operator has one to
# copy off the box, rather than discovering at 03:15 that backups never started.
if ! command -v age-keygen >/dev/null 2>&1; then
    AGE_MISSING=1
elif [[ ! -f /etc/dullahan/backup-identity.txt ]]; then
    age-keygen -o /etc/dullahan/backup-identity.txt 2>/dev/null
    chmod 600 /etc/dullahan/backup-identity.txt
    age-keygen -y /etc/dullahan/backup-identity.txt > /etc/dullahan/backup-recipients.txt
    chmod 600 /etc/dullahan/backup-recipients.txt
    AGE_KEY_CREATED=1
fi

echo "==> timers"
# The digest and selfcheck timers are safe to start immediately. The backup and
# drill timers are enabled but NOT started: they need a destination and an
# off-box copy of the key first, and a backup run that silently lands only on
# this disk is the failure mode worth refusing to set up automatically.
systemctl enable --now dullahan-digest.timer dullahan-selfcheck.timer
systemctl enable dullahan-backup.timer dullahan-restore-drill.timer
if grep -q '^RCLONE_REMOTE=.\+' /etc/dullahan/backup.env 2>/dev/null; then
    systemctl start dullahan-backup.timer dullahan-restore-drill.timer
else
    BACKUP_NEEDS_CONFIG=1
fi

echo "==> caddyfile"
mkdir -p /etc/caddy
DOMAIN="$DOMAIN" ACME_EMAIL="$ACME_EMAIL" envsubst < "$REPO_DIR/deploy/Caddyfile" > /etc/caddy/Caddyfile.tmp
mv /etc/caddy/Caddyfile.tmp /etc/caddy/Caddyfile
systemctl reload caddy 2>/dev/null || systemctl restart caddy

echo
echo "=========================================="
echo "  dullahan is up at https://${DOMAIN}"
echo "=========================================="
echo "  health:    curl https://${DOMAIN}/health"
echo "  logs:      journalctl -u dullahan -f"
echo "  timers:    systemctl list-timers 'dullahan*'"
echo "  redeploy:  re-run install.sh (rebuilds binary), or set up CD — docs/deploy.md"
echo "=========================================="
if [[ "${ADMIN_TOKEN_GENERATED:-0}" == "1" ]]; then
    echo
    echo "Generated ADMIN_TOKEN — save it now, it gates /stats/*:"
    echo "    $ADMIN_TOKEN"
    echo "(stored in $ENV_FILE; re-runs of install.sh will reuse it.)"
fi
if [[ "${AGE_KEY_CREATED:-0}" == "1" ]]; then
    echo
    echo "!! COPY THE BACKUP KEY OFF THIS MACHINE NOW !!"
    echo "    /etc/dullahan/backup-identity.txt"
    echo "It is the only thing that can decrypt your backups. Left here alone, a"
    echo "dead disk destroys the backups along with the data they were protecting."
fi
if [[ "${AGE_MISSING:-0}" == "1" ]]; then
    echo
    echo "Could not install \`age\`, so no backup encryption key was generated and"
    echo "backups will refuse to run. Install it and re-run install.sh:"
    echo "    apt install age    # or see https://github.com/FiloSottile/age"
fi
if [[ "${BACKUP_NEEDS_CONFIG:-0}" == "1" ]]; then
    echo
    echo "Backups are installed but NOT running: no off-box destination is set."
    echo "    1. edit /etc/dullahan/backup.env and set RCLONE_REMOTE"
    echo "    2. systemctl start dullahan-backup.timer dullahan-restore-drill.timer"
    echo "    3. prove it:  dullahan-backup && dullahan-restore-drill"
fi
if [[ "${BACKUP_ENV_CREATED:-0}" == "1" ]]; then
    echo
    echo "Alerting: set ALERT_TO (plus RESEND_API_KEY/EMAIL_FROM) in $ENV_FILE so"
    echo "--selfcheck can mail you. It checks health, Postgres, ingest-loss counters,"
    echo "disk, and whether the nightly backup has stopped running."
fi
