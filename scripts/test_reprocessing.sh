#!/usr/bin/env bash
# End-to-end check of remittance reprocessing (FB-02) against a running local
# stack, using the synthetic 835s in scripts/fixtures/reprocessing/:
#   1. two lines of TESTREV1 denied CO-197, TESTREV2 paid with a CO-45
#   2. the same remittance re-sent with a new date: nothing new is created
#   3. the payer reverses TESTREV1 and pays both lines: the denials settle,
#      the one under appeal as overruled, and no phantom claim appears
# Removes the claims and ingestion records it created, even on failure.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
FIXTURES="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/fixtures/reprocessing"
API="${API_BASE_URL}/api/v1"

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}

cleanup() {
  psql_exec "DELETE FROM appeals_queue WHERE denial_id IN (SELECT d.id FROM denials d JOIN claims c ON c.id = d.claim_id WHERE c.claim_number LIKE 'TESTREV%')" >/dev/null || true
  psql_exec "DELETE FROM claims WHERE claim_number LIKE 'TESTREV%'" >/dev/null || true
  psql_exec "DELETE FROM ingestion_log WHERE file_name LIKE 'reprocessing-test-%'" >/dev/null || true
}
trap cleanup EXIT
cleanup

fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { [[ "$2" == "$3" ]] || fail "$1: expected '$3', got '$2'"; }

TOKEN="$(jq -n --arg u "${ADMIN_USER:-admin}" --arg p "${ADMIN_PASSWORD:-admin123}" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API}/auth/login" | jq -er '.access_token')" \
  || fail "admin login failed; if the account must change its password first, sign in once in the UI and rerun with ADMIN_PASSWORD set"

ingest() {
  curl -fsS -X POST -H "Authorization: Bearer ${TOKEN}" \
    -F "file=@${FIXTURES}/$1;filename=reprocessing-test-$1" "${API}/ingestion/ingest"
}
denial() {
  psql_exec "SELECT d.status || '|' || COALESCE(d.resolution_source, '') || '|' || COALESCE(d.recovered_amount::text, '') \
             FROM denials d JOIN claims c ON c.id = d.claim_id \
             WHERE c.claim_number = '$1' AND d.cpt_code = '$2'"
}

expect "first file denials" "$(ingest 1_denied.835 | jq -r '.denials_stored')" 3

result="$(ingest 2_denied_resent.835)"
expect "re-sent file denials" "$(jq -r '.denials_stored' <<<"$result")" 0
expect "re-sent file duplicates" "$(jq -r '.denials_skipped_as_duplicates' <<<"$result")" 3

psql_exec "WITH d AS (UPDATE denials SET status = 'in_appeal' \
             WHERE cpt_code = '71046' AND claim_id = (SELECT id FROM claims WHERE claim_number = 'TESTREV1') \
             RETURNING id, claim_id) \
           INSERT INTO appeals_queue (denial_id, claim_id, resolution_type, outcome_status) \
           SELECT id, claim_id, 'appeal_letter', 'submitted' FROM d" >/dev/null

result="$(ingest 3_reversed_and_paid.835)"
expect "reversed claims" "$(jq -r '.claims_reversed' <<<"$result")" 1
expect "settled denials" "$(jq -r '.denials_settled' <<<"$result")" 2

expect "appealed denial" "$(denial TESTREV1 71046)" "overruled|remittance|800.00"
expect "other denial" "$(denial TESTREV1 85025)" "resolved|remittance|700.00"
expect "unrelated denial" "$(denial TESTREV2 99213)" "open||"
expect "worklist item" "$(psql_exec "SELECT aq.outcome_status FROM appeals_queue aq JOIN denials d ON d.id = aq.denial_id WHERE d.cpt_code = '71046' AND d.claim_id = (SELECT id FROM claims WHERE claim_number = 'TESTREV1')")" approved
expect "claims stored" "$(psql_exec "SELECT string_agg(claim_number, ',' ORDER BY claim_number) FROM claims WHERE claim_number LIKE 'TESTREV%'")" "TESTREV1,TESTREV2"
expect "claim totals" "$(psql_exec "SELECT total_paid || '|' || (reversed_at IS NOT NULL) FROM claims WHERE claim_number = 'TESTREV1'")" "1500.00|true"

echo 'Remittance reprocessing test passed.'
