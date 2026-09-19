#!/usr/bin/env bash
# End-to-end check of the secondary-coverage rule (FB-06) against a running
# local stack, using the synthetic files in scripts/fixtures/secondary/:
#   - TESTSEC1: the 837 names a secondary payer; its PR-2 balance goes there
#   - TESTSEC2: the 835 names a crossover carrier; same
#   - TESTSEC3: no other coverage; the PR-2 balance is billed to the patient
# Removes the claims and ingestion records it created, even on failure.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
FIXTURES="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/fixtures/secondary"
API="${API_BASE_URL}/api/v1"

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}
cleanup() {
  psql_exec "DELETE FROM claims WHERE claim_number LIKE 'TESTSEC%'" >/dev/null || true
  psql_exec "DELETE FROM ingestion_log WHERE file_name LIKE 'secondary-test-%'" >/dev/null || true
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
    -F "file=@${FIXTURES}/$1;filename=secondary-test-$1" "${API}/ingestion/ingest" >/dev/null
}
ingest 1_claims_with_secondary.837
ingest 2_remittance.835

next_payer() { psql_exec "SELECT COALESCE(next_payer_name, '') || '|' || COALESCE(next_payer_source, '') FROM claims WHERE claim_number = '$1'"; }
expect "837 secondary" "$(next_payer TESTSEC1)" "SYNTHETIC SECONDARY PLAN|837_other_subscriber"
expect "835 crossover" "$(next_payer TESTSEC2)" "SYNTHETIC SUPPLEMENT PLAN|835_crossover"
expect "no other coverage" "$(next_payer TESTSEC3)" "|"

recommended() {
  local id
  id="$(psql_exec "SELECT d.id FROM denials d JOIN claims c ON c.id = d.claim_id WHERE c.claim_number = '$1' AND d.cagc = 'PR'")"
  curl -fsS -H "Authorization: Bearer ${TOKEN}" "${API}/denials/${id}" | jq -r '.recommended_resolution'
}
expect "TESTSEC1 resolution" "$(recommended TESTSEC1)" bill_secondary
expect "TESTSEC2 resolution" "$(recommended TESTSEC2)" bill_secondary
expect "TESTSEC3 resolution" "$(recommended TESTSEC3)" bill_patient

echo 'Secondary coverage test passed.'
