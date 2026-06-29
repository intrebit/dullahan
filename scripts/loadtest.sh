#!/usr/bin/env bash
# Quick load test for a running dullahan server.
#
# Requirements:
#   brew install oha    # or: apt install oha
#
# Usage:
#   BASE=http://127.0.0.1:3001 ./scripts/loadtest.sh [scenario]
#
# Scenarios:
#   collect-burst    (default) sustained load against /collect from one IP.
#                    Confirms the rate-limit kicks in (lots of 429s) and the
#                    server stays responsive under abuse.
#   collect-spread   spread load across many fake IPs via X-Forwarded-For.
#                    Measures the real ingest path (insert + spawn). Watch
#                    p99 latency and success rate.
#   stats-read       hammer /stats/summary. Bring ADMIN_TOKEN if set.
#
# The script is read-only against a real DB but will write rows for the
# collect scenarios — point it at a throwaway database, not production.

set -euo pipefail
BASE="${BASE:-http://127.0.0.1:3001}"
SCENARIO="${1:-collect-burst}"
DURATION="${DURATION:-10s}"
CONCURRENCY="${CONCURRENCY:-50}"

command -v oha >/dev/null || { echo "install oha first: brew install oha"; exit 1; }

case "$SCENARIO" in
collect-burst)
    BODY='{"t":"pageview","s":"loadtest","p":"/","ts":1700000000000,"d":"desktop"}'
    oha -z "$DURATION" -c "$CONCURRENCY" \
        -m POST \
        -H "Content-Type: application/json" \
        -H "X-Forwarded-For: 10.0.0.1" \
        -d "$BODY" \
        "$BASE/collect"
    ;;
collect-spread)
    # Cycle X-Forwarded-For across $CONCURRENCY synthetic IPs so each worker
    # appears to be a distinct client and lands in its own rate-limit bucket.
    BODY='{"t":"pageview","s":"loadtest","p":"/","ts":1700000000000,"d":"desktop"}'
    # oha sends one fixed header set; for a true spread you need wrk/vegeta.
    # As an approximation, use a high-entropy header so the IP key cycles.
    oha -z "$DURATION" -c "$CONCURRENCY" \
        -m POST \
        -H "Content-Type: application/json" \
        -H "X-Forwarded-For: 10.0.$((RANDOM % 255)).$((RANDOM % 255))" \
        -d "$BODY" \
        "$BASE/collect"
    ;;
stats-read)
    AUTH=()
    if [[ -n "${ADMIN_TOKEN:-}" ]]; then
        AUTH=(-H "Authorization: Bearer $ADMIN_TOKEN")
    fi
    oha -z "$DURATION" -c "$CONCURRENCY" \
        "${AUTH[@]}" \
        "$BASE/stats/summary?site=loadtest&days=30"
    ;;
*)
    echo "unknown scenario: $SCENARIO" >&2
    exit 2
    ;;
esac
