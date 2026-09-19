#!/usr/bin/env bash
# ============================================================
# Denial Navigator — re-embed chunks with stale embedding provenance (FB-13)
#
# Changing EMBEDDING_MODEL or an EMBED_*_PREFIX leaves already-embedded
# chunks in a vector space the new config can no longer compare against;
# they are excluded from vector search (not deleted) until re-embedded.
# Settings -> Service health -> "Embedding provenance" reports how many.
#
# This drives POST /api/v1/knowledge/reindex in a loop until nothing is
# left to do. It is resumable and safe to interrupt or re-run: the endpoint
# always re-selects whatever still mismatches, so a partial run picks up
# exactly where it left off.
#
# Scoped to one organization at a time, signed in as that organization's
# manager or system administrator (same as the "Re-index" button in
# Settings, which this script is an unattended alternative to). Run it once
# per organization if you have more than one.
#
#   ADMIN_USER=... ADMIN_PASSWORD=... ./scripts/reindex_knowledge.sh
#
# Suggested cron, after a config change that might have shifted the
# embedding space (not needed on a routine schedule otherwise):
#   ./scripts/reindex_knowledge.sh >> /var/log/dn-reindex.log 2>&1
# ============================================================
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
API="${API_BASE_URL}/api/v1"
BATCH_LIMIT="${REINDEX_BATCH_LIMIT:-25}"

command -v curl >/dev/null
command -v jq >/dev/null

TOKEN="$(jq -n --arg u "${ADMIN_USER:-admin}" --arg p "${ADMIN_PASSWORD:-admin123}" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API}/auth/login" | jq -er '.access_token')" \
  || { echo "ERROR: login failed; if the account must change its password first, sign in once in the UI and rerun with ADMIN_PASSWORD set" >&2; exit 1; }

echo "[$(date -Is)] checking for chunks with stale embedding provenance"
total_processed=0
while :; do
  response="$(curl -fsS -X POST -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' \
    --data "{\"limit\": ${BATCH_LIMIT}}" "${API}/knowledge/reindex")"
  processed="$(jq -r '.processed' <<<"$response")"
  remaining="$(jq -r '.remaining' <<<"$response")"
  done_flag="$(jq -r '.done' <<<"$response")"
  total_processed=$((total_processed + processed))
  echo "[$(date -Is)] re-embedded ${processed} chunk(s), ${remaining} remaining"
  if [ "$done_flag" = "true" ] || [ "$processed" = "0" ]; then
    break
  fi
done
echo "[$(date -Is)] done: re-embedded ${total_processed} chunk(s) in total"
