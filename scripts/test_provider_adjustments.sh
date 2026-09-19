#!/usr/bin/env bash
# End-to-end check of PLB provider-level adjustments (FB-07) against a running
# local stack, using scripts/fixtures/provider_adjustments/:
#   - a WO recoupment naming TESTPLB1 is stored and linked to that claim
#   - an L6 interest line is stored with its negative amount
#   - the same payment re-sent in another file adds nothing
# Removes what it created, even on failure.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
FIXTURES="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/fixtures/provider_adjustments"
API="${API_BASE_URL}/api/v1"

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}
cleanup() {
  psql_exec "DELETE FROM provider_adjustments WHERE trace_number = 'SYNEFT3001'" >/dev/null || true
  psql_exec "DELETE FROM claims WHERE claim_number = 'TESTPLB1'" >/dev/null || true
  psql_exec "DELETE FROM ingestion_log WHERE file_name LIKE 'plb-test-%'" >/dev/null || true
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
    -F "file=@${FIXTURES}/$1;filename=plb-test-$1" "${API}/ingestion/ingest" | jq -r '.provider_adjustments_stored'
}
expect "first file" "$(ingest 1_recoupment.835)" 2
expect "re-sent file" "$(ingest 2_recoupment_resent.835)" 0

lines="$(curl -fsS -H "Authorization: Bearer ${TOKEN}" "${API}/ingestion/provider-adjustments?claim_number=TESTPLB1")"
expect "linked to claim" "$(jq -r '[.[] | "\(.reason_code):\(.amount * 100 | round)"] | join(",")' <<<"$lines")" "WO:15000"
expect "interest line" "$(psql_exec "SELECT amount FROM provider_adjustments WHERE trace_number = 'SYNEFT3001' AND reason_code = 'L6'")" "-4.25"

claim_id="$(psql_exec "SELECT id FROM claims WHERE claim_number = 'TESTPLB1'")"
expect "claim detail" "$(curl -fsS -H "Authorization: Bearer ${TOKEN}" "${API}/claims/${claim_id}" | jq -r '.provider_adjustments | length')" 1
expect "summary" "$(curl -fsS -H "Authorization: Bearer ${TOKEN}" "${API}/ingestion/provider-adjustments/summary" \
  | jq -r '[.[] | select(.payer_name == "SYNTHETIC TEST PAYER" and .month == "2026-08")] | map("\(.reason_code):\(.amount * 100 | round)") | sort | join(",")')" "L6:-425,WO:15000"

echo 'Provider adjustment test passed.'
