#!/usr/bin/env bash
# End-to-end check of the unanswered-claim queue (FB-09) against a running
# local stack, using scripts/fixtures/unanswered/:
#   - TESTNR1 submitted 45 days ago and unanswered: in the queue (30-day default)
#   - TESTNR2 submitted 10 days ago: not yet, until a 7-day payer-response rule
#   - a recorded follow-up shows as the last action
#   - with a 120-day timely-filing rule, TESTNR1 shows its filing deadline
#   - TESTNR1's 835 takes it out of the queue
# Removes what it created, even on failure.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
FIXTURES="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/fixtures/unanswered"
API="${API_BASE_URL}/api/v1"
PAYER='SYNTHETIC TEST PAYER'

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}
cleanup() {
  psql_exec "DELETE FROM payer_deadline_rules WHERE payer_name = '${PAYER}' AND deadline_type IN ('payer_response', 'timely_filing')" >/dev/null || true
  psql_exec "DELETE FROM claims WHERE claim_number LIKE 'TESTNR%'" >/dev/null || true
  psql_exec "DELETE FROM ingestion_log WHERE file_name LIKE 'unanswered-test-%'" >/dev/null || true
}
trap cleanup EXIT
cleanup

fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { [[ "$2" == "$3" ]] || fail "$1: expected '$3', got '$2'"; }

TOKEN="$(jq -n --arg u "${ADMIN_USER:-admin}" --arg p "${ADMIN_PASSWORD:-admin123}" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API}/auth/login" | jq -er '.access_token')" \
  || fail "admin login failed; if the account must change its password first, sign in once in the UI and rerun with ADMIN_PASSWORD set"

ingest() { curl -fsS -X POST -H "Authorization: Bearer ${TOKEN}" -F "file=@${FIXTURES}/$1;filename=unanswered-test-$1" "${API}/ingestion/ingest" >/dev/null; }
queue() { curl -fsS -H "Authorization: Bearer ${TOKEN}" "${API}/claims/unanswered" | jq -c "[.[] | select(.claim_number | startswith(\"TESTNR\"))]"; }
rule() {
  curl -fsS -X PUT -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' \
    --data "{\"payer_name\":\"${PAYER}\",\"deadline_type\":\"$1\",\"days\":$2}" "${API}/denials/deadline-rules" >/dev/null
}

ingest 1_submitted.837
expect "submitted stamped" "$(psql_exec "SELECT COUNT(*) FROM claims WHERE claim_number LIKE 'TESTNR%' AND submitted_at IS NOT NULL AND remittance_received_at IS NULL")" 2
psql_exec "UPDATE claims SET submitted_at = NOW() - INTERVAL '45 days' WHERE claim_number = 'TESTNR1'" >/dev/null
psql_exec "UPDATE claims SET submitted_at = NOW() - INTERVAL '10 days' WHERE claim_number = 'TESTNR2'" >/dev/null

expect "default 30 days" "$(queue | jq -r 'map(.claim_number) | join(",")')" TESTNR1
rule payer_response 7
expect "7-day rule" "$(queue | jq -r 'map(.claim_number) | sort | join(",")')" "TESTNR1,TESTNR2"

claim_id="$(psql_exec "SELECT id FROM claims WHERE claim_number = 'TESTNR1'")"
curl -fsS -X POST -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' \
  --data '{"action":"status_inquiry","note":"276 sent"}' "${API}/claims/${claim_id}/followups" >/dev/null
expect "last action" "$(queue | jq -r '.[] | select(.claim_number == "TESTNR1") | "\(.last_action) \(.last_note)"')" "status_inquiry 276 sent"

rule timely_filing 120
service_from="$(psql_exec "SELECT service_from FROM claims WHERE claim_number = 'TESTNR1'")"
expected_due="$(psql_exec "SELECT (DATE '${service_from}' + 120)::text")"
expect "timely filing due" "$(queue | jq -r '.[] | select(.claim_number == "TESTNR1") | .timely_filing_due')" "$expected_due"

ingest 2_remittance_for_first.835
expect "answered claim leaves" "$(queue | jq -r 'map(.claim_number) | join(",")')" TESTNR2

echo 'Unanswered claims test passed.'
