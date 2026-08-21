#!/usr/bin/env bash
#
# Register (or repair) a tenant in the `sites` table so its `POST /contact`
# form works, then verify the wiring.
#
#   sudo deploy/register-site.sh <site-id> <recipient-email> [--test-send]
#                                [--from-name "Display Name"]
#                                [--from-address noreply@their-domain.com]
#   sudo deploy/register-site.sh <site-id> --deactivate
#   sudo deploy/register-site.sh <site-id> --from-name "X"   (branding only)
#
# `/contact` needs exactly two things: an active row in `sites` carrying a
# `contact_to`, and a global mail transport (RESEND_API_KEY + EMAIL_FROM).
# This script establishes the first and reports on the second. It does not
# touch site_config, tokens for the storefront, or the tracker — a contact
# form needs none of them.
#
# Existing tenants are repaired rather than duplicated: a suspended row is
# reactivated and a stale recipient corrected, both via PATCH /sites/:id.
#
# `--from-name` sets this tenant's From display name. Without it a site inherits
# the global EMAIL_FROM_NAME, which defaults to the literal string "dullahan" —
# fine for your own sites, wrong for a client who has never heard of it. Note
# that a display name from a domain the address does not match is a phishing
# signal to some filters: pairing a client's own name with a foreign From address
# can cost you deliverability. Verify their domain in Resend and set the site's
# `email_from` too, or keep the display name neutral.
#
# Reads ADMIN_TOKEN from the environment if already set, else from
# /opt/dullahan/dullahan.env (which is root-only, hence the sudo). The token
# is passed to curl through a 0600 config file rather than argv, so it never
# appears in `ps` output. Nothing secret is ever printed.
#
# NOTE: a tenant with a contact_to is also picked up by the weekly digest
# (`dullahan --digest` iterates every active site with a recipient), so the
# address given here will receive a digest email as well.

set -euo pipefail

ENV_FILE=${DULLAHAN_ENV:-/opt/dullahan/dullahan.env}
TOKEN_DIR=${DULLAHAN_TOKEN_DIR:-/root/dullahan-tokens}

usage() {
    sed -n '3,6p' "$0" | sed 's/^# \{0,1\}//'
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$*"; }

# ---------------------------------------------------------------- arguments

test_send=0
deactivate=0
from_name=""
have_from_name=0
from_address=""
have_from_address=0
positional=()
while (( $# )); do
    case $1 in
        --test-send) test_send=1 ;;
        --deactivate) deactivate=1 ;;
        --from-address)
            shift
            [[ $# -gt 0 ]] || die "--from-address needs a value"
            from_address=$1
            have_from_address=1
            ;;
        --from-name)
            shift
            [[ $# -gt 0 ]] || die "--from-name needs a value"
            from_name=$1
            have_from_name=1
            ;;
        -h|--help)   usage; exit 0 ;;
        -*)          die "unknown flag: $1" ;;
        *)           positional+=("$1") ;;
    esac
    shift
done

branding_only=0
if (( deactivate )); then
    [[ ${#positional[@]} -eq 1 ]] || { usage >&2; exit 2; }
elif [[ ${#positional[@]} -eq 1 ]] && (( have_from_name || have_from_address )); then
    branding_only=1
elif [[ ${#positional[@]} -ne 2 ]]; then
    usage >&2
    exit 2
fi

site=${positional[0]}
recipient=${positional[1]:-}

# Mirrors sites_id_check in migration 0001 and site_valid() in sites.rs.
[[ $site =~ ^[a-z0-9-]+$ ]] ||
    die "site id must match ^[a-z0-9-]+\$ (lowercase, digits, hyphen): $site"
[[ ${#site} -le 64 ]] ||
    die "site id must be 64 characters or fewer"

# Mirrors sites_contact_to_check: no whitespace, comma or angle brackets, since
# this address is interpolated into outbound mail headers.
if (( ! deactivate && ! branding_only )); then
    [[ $recipient =~ ^[^[:space:],\<\>]+@[^[:space:],\<\>]+\.[^[:space:],\<\>]+$ ]] ||
        die "not a valid recipient address: $recipient"
    [[ ${#recipient} -le 254 ]] ||
        die "recipient address must be 254 characters or fewer"
fi

# Mirrors sanitize_display_name() in email.rs: this lands in a From header, so a
# CR/LF or a quote would be header injection. Rejected here rather than silently
# stripped downstream, so what you typed is what gets sent.
if (( have_from_name )); then
    [[ -n ${from_name//[[:space:]]/} ]] ||
        die "--from-name cannot be blank (omit it to inherit EMAIL_FROM_NAME)"
    [[ $from_name != *$'\n'* && $from_name != *$'\r'* ]] ||
        die "--from-name cannot contain a newline"
    [[ $from_name != *'"'* && $from_name != *'<'* && $from_name != *'>'* ]] ||
        die "--from-name cannot contain quotes or angle brackets"
    [[ ${#from_name} -le 64 ]] || die "--from-name must be 64 characters or fewer"
fi

if (( have_from_address )); then
    [[ $from_address =~ ^[^[:space:],\<\>]+@[^[:space:],\<\>]+\.[^[:space:],\<\>]+$ ]] ||
        die "not a valid sender address: $from_address"
    [[ ${#from_address} -le 254 ]] || die "sender address must be 254 characters or fewer"
    # Resend will refuse to send from a domain it has not verified, and the
    # failure surfaces as a 500 from /contact long after this script says OK.
    warn "verify ${from_address#*@} in Resend before relying on this"
fi

command -v curl >/dev/null || die "curl is required"
command -v jq   >/dev/null || die "jq is required"

# ------------------------------------------------------------- configuration

if [[ -z ${ADMIN_TOKEN:-} ]]; then
    [[ -r $ENV_FILE ]] ||
        die "cannot read $ENV_FILE — run with sudo, or export ADMIN_TOKEN yourself"
    set -a
    # shellcheck disable=SC1090
    . "$ENV_FILE"
    set +a
fi

[[ -n ${ADMIN_TOKEN:-} ]] || die "ADMIN_TOKEN is not set in $ENV_FILE"

# The server's own bind address is the source of truth for the port; the
# documented default and the deployed value have drifted before.
addr=${BIND_ADDR:-127.0.0.1:3001}
addr=${addr/#0.0.0.0:/127.0.0.1:}
base=${DULLAHAN_URL:-http://$addr}

cfg=$(mktemp)
chmod 600 "$cfg"
trap 'rm -f "$cfg"' EXIT
printf 'header = "Authorization: Bearer %s"\n' "$ADMIN_TOKEN" >"$cfg"

http_code=""
http_body=""

# api <METHOD> <PATH> [JSON_BODY] — operator-authenticated request.
api() {
    local method=$1 path=$2 body=${3:-} raw
    if [[ -n $body ]]; then
        raw=$(curl -sS -K "$cfg" -X "$method" "$base$path" \
            -H 'Content-Type: application/json' --data "$body" \
            -w $'\n%{http_code}')
    else
        raw=$(curl -sS -K "$cfg" -X "$method" "$base$path" -w $'\n%{http_code}')
    fi
    http_code=${raw##*$'\n'}
    http_body=${raw%$'\n'*}
}

# anon <PATH> — no credential, to prove a public caller is admitted.
anon() {
    local raw
    raw=$(curl -sS -X GET "$base$1" -w $'\n%{http_code}')
    http_code=${raw##*$'\n'}
    http_body=${raw%$'\n'*}
}

if (( deactivate )); then
    printf '\nTarget: %s   site: %s   action: deactivate\n\n' "$base" "$site"
    api GET "/sites/$site"
    case $http_code in
        404) warn "site '$site' does not exist — nothing to do"; exit 0 ;;
        200) ;;
        401|403) die "ADMIN_TOKEN was rejected (HTTP $http_code)" ;;
        *)   die "GET /sites/$site returned HTTP $http_code: $http_body" ;;
    esac
    api PATCH "/sites/$site" '{"active": false}'
    [[ $http_code == 200 ]] || die "PATCH /sites/$site returned HTTP $http_code: $http_body"
    ok "site '$site' is now inactive"
    # Admission is refreshed in-process on mutation, so this is immediate: a
    # throwaway test tenant stops being a live contact endpoint the moment this
    # returns, rather than at the next 60s registry tick.
    anon "/posts?site=$site"
    if [[ $http_code == 403 ]]; then
        ok "confirmed: /contact and /posts now refuse site '$site'"
    else
        warn "expected HTTP 403 for an inactive site, got $http_code"
    fi
    printf '\n'
    exit 0
fi

if (( branding_only )); then
    printf '\nTarget: %s   site: %s   action: set From identity\n\n' "$base" "$site"
else
    printf '\nTarget: %s   site: %s   recipient: %s\n\n' "$base" "$site" "$recipient"
fi

# ------------------------------------------------------------ mail transport

printf 'Mail transport\n'
transport_ok=1
if [[ -n ${RESEND_API_KEY:-} ]]; then
    ok "RESEND_API_KEY is set"
else
    bad "RESEND_API_KEY is MISSING"
    transport_ok=0
fi
if [[ -n ${EMAIL_FROM:-} ]]; then
    ok "EMAIL_FROM = ${EMAIL_FROM}"
else
    bad "EMAIL_FROM is MISSING"
    transport_ok=0
fi
# The display half of the From header. Unset is not neutral: the binary falls
# back to the literal "dullahan", which then appears in every tenant's inbox.
if [[ -n ${EMAIL_FROM_NAME:-} ]]; then
    ok "EMAIL_FROM_NAME = ${EMAIL_FROM_NAME} (global default)"
else
    warn "EMAIL_FROM_NAME is unset — the compiled default is \"dullahan\", so any"
    warn "site without a --from-name of its own sends as: dullahan <${EMAIL_FROM:-...}>"
fi
if [[ $transport_ok -eq 0 ]]; then
    warn "the mailer is only built when both are set — /contact will answer"
    warn "503 \"email transport not configured\" until they are"
fi

# ------------------------------------------------------------------ registry

printf '\nRegistry\n'
api GET "/sites/$site"

case $http_code in
    401|403)
        die "ADMIN_TOKEN was rejected (HTTP $http_code) — wrong token in $ENV_FILE?"
        ;;
    200)
        active=$(jq -r '.active' <<<"$http_body")
        current=$(jq -r '.contact_to // ""' <<<"$http_body")
        ok "site '$site' already exists (active=$active, contact_to='${current}')"

        current_name=$(jq -r '.email_from_name // ""' <<<"$http_body")
        ok "From display name: '${current_name:-<inherits EMAIL_FROM_NAME>}'"

        patch='{}'
        if (( ! branding_only )); then
            [[ $active == true ]]        || patch=$(jq -c '.active = true' <<<"$patch")
            [[ $current == "$recipient" ]] ||
                patch=$(jq -c --arg to "$recipient" '.contact_to = $to' <<<"$patch")
        fi
        if (( have_from_name )) && [[ $current_name != "$from_name" ]]; then
            patch=$(jq -c --arg n "$from_name" '.email_from_name = $n' <<<"$patch")
        fi
        current_from=$(jq -r '.email_from // ""' <<<"$http_body")
        if (( have_from_address )) && [[ $current_from != "$from_address" ]]; then
            patch=$(jq -c --arg a "$from_address" '.email_from = $a' <<<"$patch")
        fi

        if [[ $patch == '{}' ]]; then
            ok "already correct — nothing to change"
        else
            api PATCH "/sites/$site" "$patch"
            [[ $http_code == 200 ]] ||
                die "PATCH /sites/$site returned HTTP $http_code: $http_body"
            ok "updated: $patch"
        fi
        ;;
    404)
        (( ! branding_only )) ||
            die "site '$site' does not exist — create it with a recipient first"
        body=$(jq -nc --arg id "$site" --arg to "$recipient" \
            '{id: $id, name: ($id | ascii_upcase), contact_to: $to}')
        if (( have_from_name )); then
            body=$(jq -c --arg n "$from_name" '.email_from_name = $n' <<<"$body")
        fi
        if (( have_from_address )); then
            body=$(jq -c --arg a "$from_address" '.email_from = $a' <<<"$body")
        fi
        api POST "/sites" "$body"
        case $http_code in
            201)
                ok "created site '$site'"
                # The per-site token is shown exactly once and is not
                # recoverable (rotation is the only recovery path). A contact
                # form does not need it, but throwing it away silently would be
                # unkind — stash it root-only instead of printing it.
                token=$(jq -r '.token // empty' <<<"$http_body")
                if [[ -n $token ]]; then
                    install -d -m 700 "$TOKEN_DIR"
                    umask 077
                    printf '%s\n' "$token" >"$TOKEN_DIR/$site.token"
                    chmod 600 "$TOKEN_DIR/$site.token"
                    ok "per-site token saved to $TOKEN_DIR/$site.token (not needed for /contact)"
                fi
                ;;
            409) die "HTTP 409 — site '$site' exists but GET reported 404; re-run" ;;
            400) die "HTTP 400 — server rejected the id or address: $http_body" ;;
            *)   die "POST /sites returned HTTP $http_code: $http_body" ;;
        esac
        ;;
    *)
        die "GET /sites/$site returned HTTP $http_code: $http_body"
        ;;
esac

# ---------------------------------------------------------------- verification

printf '\nVerification\n'

# Every mutation above refreshes the in-memory registry before responding, so
# admission is live immediately rather than after the 60s refresh tick.
anon "/posts?site=$site"
if [[ $http_code == 200 ]]; then
    ok "admission: an anonymous GET /posts?site=$site returns 200"
else
    bad "admission: GET /posts?site=$site returned HTTP $http_code (expected 200)"
fi

api GET "/sites/$site"
if [[ $http_code == 200 ]]; then
    jq -r '"  ✓ row: active=\(.active) contact_to=\(.contact_to // "null")",
            "         email_from=\(.email_from // "<global default>") email_from_name=\(.email_from_name // "<global default>")"' \
        <<<"$http_body"
fi

if [[ $test_send -eq 1 ]]; then
    printf '\nTest send\n'
    warn "delivering a real email to $recipient"
    # No product name in here: `name` becomes the subject line ("New contact form
    # submission from ...") and the message is the body, so a client running this
    # against their own inbox would otherwise be told what software you use.
    payload=$(jq -nc --arg site "$site" \
        '{site: $site,
          name: "Test Visitor",
          email: "noreply@example.com",
          message: "Automated test of the contact form on this website. No reply is needed; you can delete this message."}')
    raw=$(curl -sS -X POST "$base/contact" \
        -H 'Content-Type: application/json' --data "$payload" -w $'\n%{http_code}')
    code=${raw##*$'\n'}
    body=${raw%$'\n'*}
    case $code in
        201) ok "HTTP 201 — accepted and handed to Resend; check $recipient" ;;
        503) bad "HTTP 503 — $body" ;;
        500) bad "HTTP 500 — accepted but the send failed; is the EMAIL_FROM domain verified in Resend?" ;;
        *)   bad "HTTP $code — $body" ;;
    esac
else
    printf '\n  re-run with --test-send to deliver a real test email to %s\n' "$recipient"
fi

printf '\n'
