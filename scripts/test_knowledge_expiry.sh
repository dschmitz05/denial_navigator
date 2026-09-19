#!/usr/bin/env bash
# End-to-end check of knowledge-document expiry alerts and supersession
# (FB-14) against a running local stack. A document expiring within the
# window, and one already expired but not archived, both show up in the
# expiry summary and the matching Knowledge Base filter; superseding the
# expired one links the replacement and never pushes its expiration later.
# Removes what it created, even on failure.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
API="${API_BASE_URL}/api/v1"

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}
cleanup() {
  psql_exec "DELETE FROM knowledge_documents WHERE title LIKE 'FB14 test %'" >/dev/null || true
}
trap cleanup EXIT
cleanup

fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { [[ "$2" == "$3" ]] || fail "$1: expected '$3', got '$2'"; }

TOKEN="$(jq -n --arg u "${ADMIN_USER:-admin}" --arg p "${ADMIN_PASSWORD:-admin123}" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API}/auth/login" | jq -er '.access_token')" \
  || fail "admin login failed; if the account must change its password first, sign in once in the UI and rerun with ADMIN_PASSWORD set"
AUTH=(-H "Authorization: Bearer ${TOKEN}")

create() { # title, expiration_date (or null)
  jq -n --arg t "$1" --argjson exp "$2" \
    '{title: $t, source_type: "payer_policy", expiration_date: $exp, content: "placeholder text for a synthetic test document"}' \
    | curl -fsS "${AUTH[@]}" -H 'Content-Type: application/json' --data @- "${API}/knowledge/documents" | jq -er '.id'
}
today() { date -u +%Y-%m-%d; }
in_days() { date -u -d "+$1 days" +%Y-%m-%d 2>/dev/null || date -u -v+"$1"d +%Y-%m-%d; }

summary_before="$(curl -fsS "${AUTH[@]}" "${API}/knowledge/documents/expiry-summary?within_days=30")"
BEFORE_SOON="$(jq -r '.expiring_soon' <<<"$summary_before")"
BEFORE_EXPIRED="$(jq -r '.expired_active' <<<"$summary_before")"

CURRENT_ID="$(create 'FB14 test current policy' null)"
EXPIRING_ID="$(create 'FB14 test expiring-soon policy' "\"$(in_days 10)\"")"
EXPIRED_ID="$(create 'FB14 test expired policy' "\"$(in_days -30)\"")"
REPLACEMENT_ID="$(create 'FB14 test replacement policy' null)"

# Two new documents (expiring-soon and expired) landed since the baseline;
# the two others (open-ended) do not count toward either bucket.
summary_after="$(curl -fsS "${AUTH[@]}" "${API}/knowledge/documents/expiry-summary?within_days=30")"
expect "expiring-soon summary counts our document"   "$(jq -r '.expiring_soon' <<<"$summary_after")" "$((BEFORE_SOON + 1))"
expect "expired-active summary counts our document"   "$(jq -r '.expired_active' <<<"$summary_after")" "$((BEFORE_EXPIRED + 1))"

list_ids() { # expiry filter
  curl -fsS "${AUTH[@]}" "${API}/knowledge/documents?expiry=$1&expiring_within_days=30&limit=500" | jq -r '.[].id'
}
expect "expiring-soon filter includes the expiring document" \
  "$(list_ids expiring_soon | grep -c "^${EXPIRING_ID}\$")" 1
expect "expiring-soon filter excludes the current document" \
  "$(list_ids expiring_soon | grep -c "^${CURRENT_ID}\$")" 0
expect "expired-active filter includes the expired document" \
  "$(list_ids expired_active | grep -c "^${EXPIRED_ID}\$")" 1
expect "expired-active filter excludes the expiring-soon document" \
  "$(list_ids expired_active | grep -c "^${EXPIRING_ID}\$")" 0

# Superseding never extends an expiration: the expired document already
# expired 30 days ago and must stay there, not jump forward to today.
supersede_resp="$(curl -fsS -X POST "${AUTH[@]}" -H 'Content-Type: application/json' \
  --data "{\"new_document_id\":\"${REPLACEMENT_ID}\"}" "${API}/knowledge/documents/${EXPIRED_ID}/supersede")"
expect "supersede links the replacement" "$(jq -r '.superseded_by' <<<"$supersede_resp")" "$REPLACEMENT_ID"
expect "supersede does not push the expiration date forward" \
  "$(jq -r '.expiration_date' <<<"$supersede_resp")" "$(in_days -30)"

# Superseding a document with no expiration date brings it forward to today.
supersede2="$(curl -fsS -X POST "${AUTH[@]}" -H 'Content-Type: application/json' \
  --data "{\"new_document_id\":\"${REPLACEMENT_ID}\"}" "${API}/knowledge/documents/${CURRENT_ID}/supersede")"
expect "supersede expires an open-ended document as of today" \
  "$(jq -r '.expiration_date' <<<"$supersede2")" "$(today)"

expect "list shows the replacement's title" \
  "$(curl -fsS "${AUTH[@]}" "${API}/knowledge/documents?limit=500" \
     | jq -r --arg id "$EXPIRED_ID" '.[] | select(.id == $id) | .superseded_by_title')" \
  'FB14 test replacement policy'

expect "cannot supersede a document with itself" \
  "$(curl -sS -o /dev/null -w '%{http_code}' -X POST "${AUTH[@]}" -H 'Content-Type: application/json' \
     --data "{\"new_document_id\":\"${REPLACEMENT_ID}\"}" "${API}/knowledge/documents/${REPLACEMENT_ID}/supersede")" 400

echo 'Knowledge expiry test passed.'
