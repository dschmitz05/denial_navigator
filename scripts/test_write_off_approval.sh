#!/usr/bin/env bash
# End-to-end check of write-off approval (FB-03) against a running local stack.
# Creates a temporary specialist and two managers, synthetic denials from
# scripts/fixtures/reprocessing/1_denied.835, and a $100 threshold, then checks:
#   - a specialist's write-off at/above the threshold is held, not applied
#   - a specialist cannot approve; a manager can, and the denial is written off
#   - a manager cannot approve their own request; a different manager can
#   - a write-off below the threshold is applied immediately
# Restores the threshold and removes everything it created, even on failure.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
FIXTURE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/fixtures/reprocessing/1_denied.835"
API="${API_BASE_URL}/api/v1"
RUN_ID="$(date +%s)"
PASSWORD='WriteOffTest123!'
ORIGINAL_THRESHOLD=''

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}

cleanup() {
  psql_exec "DELETE FROM write_off_requests WHERE denial_id IN (SELECT d.id FROM denials d JOIN claims c ON c.id = d.claim_id WHERE c.claim_number LIKE 'TESTREV%')" >/dev/null || true
  psql_exec "DELETE FROM appeals_queue WHERE denial_id IN (SELECT d.id FROM denials d JOIN claims c ON c.id = d.claim_id WHERE c.claim_number LIKE 'TESTREV%')" >/dev/null || true
  psql_exec "DELETE FROM claims WHERE claim_number LIKE 'TESTREV%'" >/dev/null || true
  psql_exec "DELETE FROM ingestion_log WHERE file_name LIKE 'write-off-test-%'" >/dev/null || true
  psql_exec "DELETE FROM users WHERE username LIKE 'wo-test-%'" >/dev/null || true
  if [[ -n "$ORIGINAL_THRESHOLD" ]]; then
    curl -fsS -X PUT -H "Authorization: Bearer ${ADMIN}" -H 'Content-Type: application/json' \
      --data "{\"threshold\": ${ORIGINAL_THRESHOLD}}" "${API}/settings/write-off-approval" >/dev/null || true
  fi
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { [[ "$2" == "$3" ]] || fail "$1: expected '$3', got '$2'"; }

login() {
  curl -fsS -H 'Content-Type: application/json' \
    --data "{\"username\":\"$1\",\"password\":\"$2\"}" "${API}/auth/login" | jq -er '.access_token'
}
call() {  # call TOKEN METHOD PATH [BODY] -> prints "HTTP_CODE BODY"
  local out code
  out="$(curl -sS -o /dev/stdout -w '\n%{http_code}' -X "$2" -H "Authorization: Bearer $1" \
    -H 'Content-Type: application/json' ${4:+--data "$4"} "${API}$3")"
  code="${out##*$'\n'}"; echo "$code ${out%$'\n'*}"
}
status_of() { cut -d' ' -f1 <<<"$1"; }
body_of() { cut -d' ' -f2- <<<"$1"; }
denial_status() { psql_exec "SELECT status FROM denials WHERE id = '$1'"; }

ADMIN="$(login admin admin123)"
STARTED_AT="$(psql_exec "SELECT NOW()")"
ORIGINAL_THRESHOLD="$(curl -fsS -H "Authorization: Bearer ${ADMIN}" "${API}/settings/write-off-approval" | jq -r '.threshold')"

for who in specialist:billing_specialist manager1:revenue_cycle_manager manager2:revenue_cycle_manager; do
  name="${who%%:*}"; role="${who##*:}"
  curl -fsS -X POST -H "Authorization: Bearer ${ADMIN}" -H 'Content-Type: application/json' \
    --data "{\"username\":\"wo-test-${name}\",\"email\":\"wo-test-${name}-${RUN_ID}@example.test\",\"password\":\"${PASSWORD}\",\"full_name\":\"Write-off test ${name}\",\"role\":\"${role}\"}" \
    "${API}/auth/register" >/dev/null
done
SPECIALIST="$(login wo-test-specialist "$PASSWORD")"
MANAGER1="$(login wo-test-manager1 "$PASSWORD")"
MANAGER2="$(login wo-test-manager2 "$PASSWORD")"

curl -fsS -X PUT -H "Authorization: Bearer ${ADMIN}" -H 'Content-Type: application/json' \
  --data '{"threshold": 100}' "${API}/settings/write-off-approval" >/dev/null
curl -fsS -X POST -H "Authorization: Bearer ${ADMIN}" \
  -F "file=@${FIXTURE};filename=write-off-test-${RUN_ID}.835" "${API}/ingestion/ingest" >/dev/null

denial_id() {
  psql_exec "SELECT d.id FROM denials d JOIN claims c ON c.id = d.claim_id WHERE c.claim_number = '$1' AND d.cpt_code = '$2'"
}
BIG="$(denial_id TESTREV1 71046)"      # CO-197, 800.00
OTHER="$(denial_id TESTREV1 85025)"    # CO-197, 700.00
SMALL="$(denial_id TESTREV2 99213)"    # CO-45, 50.00

# A specialist closes a write-off worklist item at/above the threshold: held.
ITEM="$(body_of "$(call "$SPECIALIST" POST /appeals "{\"denial_id\":\"${BIG}\",\"resolution_type\":\"write_off\"}")" | jq -r '.id')"
BEFORE="$(denial_status "$BIG")"   # queueing the item already moved it to in_progress
result="$(call "$SPECIALIST" PATCH "/appeals/${ITEM}" '{"outcome_status":"resolved"}')"
expect "specialist write-off" "$(status_of "$result")" 202
expect "held response" "$(body_of "$result" | jq -r '.status')" pending_approval
REQUEST="$(body_of "$result" | jq -r '.write_off_request_id')"
expect "denial unchanged by the request" "$(denial_status "$BIG")" "$BEFORE"
expect "asking again returns the same request" \
  "$(body_of "$(call "$SPECIALIST" PATCH "/appeals/${ITEM}" '{"outcome_status":"resolved"}')" | jq -r '.write_off_request_id')" "$REQUEST"

expect "specialist approves" "$(status_of "$(call "$SPECIALIST" POST "/write-offs/${REQUEST}/approve")")" 403
expect "manager approves" "$(status_of "$(call "$MANAGER1" POST "/write-offs/${REQUEST}/approve" '{"note":"verified"}')")" 200
expect "denial after approval" "$(denial_status "$BIG")" written_off
expect "worklist item after approval" "$(psql_exec "SELECT outcome_status FROM appeals_queue WHERE id = '${ITEM}'")" resolved
expect "request after approval" "$(psql_exec "SELECT status || '|' || (decided_by IS NOT NULL) FROM write_off_requests WHERE id = '${REQUEST}'")" "approved|true"

# A manager's own write-off: they cannot approve it, another manager can.
result="$(call "$MANAGER1" PATCH "/denials/${OTHER}" '{"status":"written_off"}')"
expect "manager write-off" "$(status_of "$result")" 202
REQUEST="$(body_of "$result" | jq -r '.write_off_request_id')"
expect "self-approval" "$(status_of "$(call "$MANAGER1" POST "/write-offs/${REQUEST}/approve")")" 403
expect "rejection needs a reason" "$(status_of "$(call "$MANAGER2" POST "/write-offs/${REQUEST}/reject" '{}')")" 400
expect "other manager rejects" "$(status_of "$(call "$MANAGER2" POST "/write-offs/${REQUEST}/reject" '{"note":"appeal instead"}')")" 200
expect "denial after rejection" "$(denial_status "$OTHER")" open

# Below the threshold, the write-off applies at once.
expect "small write-off" "$(status_of "$(call "$SPECIALIST" PATCH "/denials/${SMALL}" '{"status":"written_off"}')")" 200
expect "small denial" "$(denial_status "$SMALL")" written_off

expect "audit entries" "$(psql_exec "SELECT count(*) FROM audit_log WHERE action IN ('write_off_requested','write_off_approved','write_off_rejected') AND created_at >= '${STARTED_AT}'")" 4

echo 'Write-off approval test passed.'
