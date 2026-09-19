#!/usr/bin/env bash
# End-to-end check of overpayment tracking (FB-08) against a running local
# stack, using scripts/fixtures/overpayments/:
#   1. TESTOVP2's line is paid 95.00 against an allowed 80.00 (15.00 over)
#   2. TESTOVP1 is paid again under a new control number: a duplicate payment
#   3. the second payment re-sent adds nothing
#   4. a PLB WO recoupment naming TESTOVP1 marks its overpayment recouped
# and that a past-due overpayment reaches managers in the deadline digest.
# Removes what it created, even on failure.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
FIXTURES="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/fixtures/overpayments"
API="${API_BASE_URL}/api/v1"

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}
cleanup() {
  psql_exec "DELETE FROM provider_adjustments WHERE trace_number LIKE 'SYNEFT400%'" >/dev/null || true
  psql_exec "DELETE FROM claims WHERE claim_number LIKE 'TESTOVP%'" >/dev/null || true
  psql_exec "DELETE FROM ingestion_log WHERE file_name LIKE 'overpayment-test-%'" >/dev/null || true
  if [[ -n "${STARTED_AT:-}" ]]; then
    psql_exec "DELETE FROM notifications WHERE kind = 'overpayment_due' AND created_at >= '${STARTED_AT}'" >/dev/null || true
  fi
}
STARTED_AT=''
trap cleanup EXIT
cleanup
STARTED_AT="$(psql_exec "SELECT NOW()")"

fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { [[ "$2" == "$3" ]] || fail "$1: expected '$3', got '$2'"; }

TOKEN="$(jq -n --arg u "${ADMIN_USER:-admin}" --arg p "${ADMIN_PASSWORD:-admin123}" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API}/auth/login" | jq -er '.access_token')" \
  || fail "admin login failed; if the account must change its password first, sign in once in the UI and rerun with ADMIN_PASSWORD set"

ingest() {
  curl -fsS -X POST -H "Authorization: Bearer ${TOKEN}" \
    -F "file=@${FIXTURES}/$1;filename=overpayment-test-$1" "${API}/ingestion/ingest" | jq -r '.overpayments_identified'
}
overpayment() {
  psql_exec "SELECT o.kind || '|' || o.amount || '|' || o.status FROM overpayments o JOIN claims c ON c.id = o.claim_id \
             WHERE c.claim_number = '$1' ORDER BY o.identified_at"
}

expect "first payments" "$(ingest 1_first_payments.835)" 1
expect "paid above allowed" "$(overpayment TESTOVP2)" "paid_above_allowed|15.00|identified"
expect "paid again" "$(ingest 2_paid_again.835)" 1
expect "duplicate payment" "$(overpayment TESTOVP1)" "duplicate_payment|100.00|identified"
expect "re-sent" "$(ingest 3_paid_again_resent.835)" 0
expect "due date" "$(psql_exec "SELECT o.due_date - CURRENT_DATE FROM overpayments o JOIN claims c ON c.id = o.claim_id WHERE c.claim_number = 'TESTOVP1'")" \
  "$(psql_exec "SELECT overpayment_refund_days FROM organizations WHERE id = '00000000-0000-0000-0000-000000000001'")"

ingest 4_recoupment.835 >/dev/null
expect "recouped" "$(overpayment TESTOVP1)" "duplicate_payment|100.00|recouped"

# The paid-above-allowed one is still open; make it overdue and run the digest.
psql_exec "UPDATE overpayments o SET due_date = CURRENT_DATE - 1 FROM claims c WHERE c.id = o.claim_id AND c.claim_number = 'TESTOVP2'" >/dev/null
expect "overdue in list" "$(curl -fsS -H "Authorization: Bearer ${TOKEN}" "${API}/overpayments?status=identified" \
  | jq -r '[.[] | select(.claim_number == "TESTOVP2")][0].overdue')" true
curl -fsS -X POST -H "Authorization: Bearer ${TOKEN}" "${API}/notifications/generate-digests" >/dev/null
expect "digest" "$(psql_exec "SELECT COUNT(*) > 0 FROM notifications WHERE kind = 'overpayment_due' AND for_date = CURRENT_DATE")" t

echo 'Overpayment test passed.'
