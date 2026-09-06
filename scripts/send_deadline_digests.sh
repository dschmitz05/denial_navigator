#!/bin/bash
# ============================================================
# Denial Navigator — daily deadline digests
#
# Filing deadlines are only useful if somebody is told about them. This asks
# the API to build today's digests: one per person for their own work, plus an
# escalation to managers for anything overdue that nobody owns.
#
# Safe to run more than once a day - generation is idempotent per user per day,
# so a retry after a failure will not send duplicates.
#
# Suggested cron, weekday mornings:
#   0 7 * * 1-5 /path/to/denial-navigator/scripts/send_deadline_digests.sh >> /var/log/dn-digests.log 2>&1
# ============================================================
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[ -f "$ROOT/.env" ] && set -a && . "$ROOT/.env" && set +a

API="${API_URL:-http://127.0.0.1:8000}"
KEY="${SERVICE_API_KEY:-}"

if [ -z "$KEY" ]; then
    echo "ERROR: SERVICE_API_KEY is not set; cannot authenticate to the API" >&2
    exit 1
fi

echo "[$(date -Is)] generating deadline digests"
response=$(curl -sS -f -X POST "$API/api/v1/notifications/generate-digests" \
    -H "X-Service-Key: $KEY" \
    -H "X-Service-Name: digest-cron") || {
        echo "ERROR: the API call failed" >&2
        exit 1
    }
echo "[$(date -Is)] $response"
