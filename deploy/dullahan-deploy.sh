#!/usr/bin/env bash
# Installs an uploaded dullahan binary and restarts the service, rolling back to
# the previous binary if the new one does not come up healthy.
#
# Lives at /usr/local/bin/dullahan-deploy, owned by root. The CD workflow uploads
# a binary to /tmp and runs this via the single sudoers rule in
# dullahan-deploy.sudoers, so the deploy account needs no other root rights.
#
# Usage:  sudo /usr/local/bin/dullahan-deploy /tmp/dullahan.new
set -euo pipefail

NEW="${1:?path to the new dullahan binary required}"
TARGET=/opt/dullahan/dullahan
PREV=/opt/dullahan/dullahan.prev
ENV_FILE=/opt/dullahan/dullahan.env
HEALTH_TIMEOUT=45

# The workflow is the only intended caller, but this runs as root: refuse
# anything that is not a regular ELF file before installing it as the service
# binary. `file` is not installed on a minimal box, so read the magic directly.
[[ -f "$NEW" ]] || { echo "no such file: $NEW" >&2; exit 1; }
magic="$(head -c 4 "$NEW" | od -An -tx1 | tr -d ' \n')"
[[ "$magic" == "7f454c46" ]] || { echo "$NEW is not an ELF binary (magic: $magic)" >&2; exit 1; }

# Health-check the address the service actually binds, not a guess.
BIND_ADDR="$(sed -n 's/^BIND_ADDR=//p' "$ENV_FILE" | tail -1)"
HEALTH="http://${BIND_ADDR:-127.0.0.1:3001}/health"

wait_healthy() {
    local deadline=$((SECONDS + HEALTH_TIMEOUT))
    while ((SECONDS < deadline)); do
        curl -fsS -m 2 "$HEALTH" >/dev/null 2>&1 && return 0
        sleep 1
    done
    return 1
}

# Keep the running binary so a failed rollout has something to go back to. On a
# first deploy there is nothing to keep, and rollback below is then impossible —
# the health check still fails loudly.
[[ -f "$TARGET" ]] && cp -a "$TARGET" "$PREV"

install -o dullahan -g dullahan -m 755 "$NEW" "$TARGET"
rm -f "$NEW"
systemctl restart dullahan

if wait_healthy; then
    echo "deployed: $(sha256sum "$TARGET" | cut -c1-12) healthy at $HEALTH"
    exit 0
fi

echo "new build unhealthy after ${HEALTH_TIMEOUT}s at $HEALTH" >&2
if [[ -f "$PREV" ]]; then
    echo "rolling back to the previous binary" >&2
    install -o dullahan -g dullahan -m 755 "$PREV" "$TARGET"
    systemctl restart dullahan
    wait_healthy && echo "rollback healthy — the deployed build was rejected" >&2 \
        || echo "rollback also unhealthy — inspect: journalctl -u dullahan -n 50" >&2
else
    echo "no previous binary to roll back to — inspect: journalctl -u dullahan -n 50" >&2
fi
exit 1
