#!/usr/bin/env bash
# End-to-end check of payer deadline rules (FB-10) against a running local
# stack. With a 90-day corrected-claim rule for SYNTHETIC TEST PAYER and a
# 120-day default timely-filing rule, a denial whose analysis recommends a
# corrected claim shows the corrected-claim deadline (remittance 2026-08-01 +
# 90 = 2026-10-30) as its action deadline, and the timely-filing deadline
# (service 2026-07-01 + 120 = 2026-10-29) alongside. A specialist cannot
# change the rules. Removes what it created, even on failure.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
FIXTURE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/fixtures/deadlines/denied_needs_correction.835"
API="${API_BASE_URL}/api/v1"
RULE_IDS=()

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}
cleanup() {
  for id in "${RULE_IDS[@]}"; do psql_exec "DELETE FROM payer_deadline_rules WHERE id = '${id}'" >/dev/null || true; done
  psql_exec "DELETE FROM claims WHERE claim_number = 'TESTDL1'" >/dev/null || true
  psql_exec "DELETE FROM ingestion_log WHERE file_name LIKE 'deadline-test-%'" >/dev/null || true
  psql_exec "DELETE FROM users WHERE username = 'dl-test-specialist'" >/dev/null || true
}
trap cleanup EXIT
cleanup

fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { [[ "$2" == "$3" ]] || fail "$1: expected '$3', got '$2'"; }

TOKEN="$(jq -n --arg u "${ADMIN_USER:-admin}" --arg p "${ADMIN_PASSWORD:-admin123}" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API}/auth/login" | jq -er '.access_token')" \
  || fail "admin login failed; if the account must change its password first, sign in once in the UI and rerun with ADMIN_PASSWORD set"
[[ -z "$(psql_exec "SELECT id FROM payer_deadline_rules WHERE organization_id = '00000000-0000-0000-0000-000000000001' AND payer_name = '*' AND deadline_type = 'timely_filing'")" ]] \
  || fail "a default timely-filing rule already exists; this test would overwrite it"

rule() {
  curl -fsS -X PUT -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' \
    --data "{\"payer_name\":\"$1\",\"deadline_type\":\"$2\",\"days\":$3}" "${API}/denials/deadline-rules" | jq -er '.id'
}
RULE_IDS+=("$(rule 'SYNTHETIC TEST PAYER' corrected_claim 90)")
RULE_IDS+=("$(rule '*' timely_filing 120)")

curl -fsS -X POST -H "Authorization: Bearer ${TOKEN}" -F "file=@${FIXTURE};filename=deadline-test.835" "${API}/ingestion/ingest" >/dev/null
DENIAL="$(psql_exec "SELECT d.id FROM denials d JOIN claims c ON c.id = d.claim_id WHERE c.claim_number = 'TESTDL1'")"
psql_exec "INSERT INTO ai_analyses (denial_id, claim_id, model_name, required_action, denial_category) \
           SELECT d.id, d.claim_id, 'deadline-test', 'coding_correction', 'coding_error' FROM denials d WHERE d.id = '${DENIAL}'" >/dev/null

detail="$(curl -fsS -H "Authorization: Bearer ${TOKEN}" "${API}/denials/${DENIAL}")"
expect "recommended" "$(jq -r '.recommended_resolution' <<<"$detail")" corrected_claim
expect "action deadline" "$(jq -r '.action_deadline | "\(.type) \(.due_date)"' <<<"$detail")" "corrected_claim 2026-10-30"
expect "timely filing" "$(jq -r '.deadlines[] | select(.type == "timely_filing") | .due_date' <<<"$detail")" "2026-10-29"

psql_exec "INSERT INTO users (username, email, password_hash, full_name, role, is_active) VALUES ('dl-test-specialist', 'dl-test-specialist@example.test', crypt('Deadline-Test-2026', gen_salt('bf', 12)), 'Deadline test', 'billing_specialist', TRUE)" >/dev/null
psql_exec "INSERT INTO organization_memberships (organization_id, user_id, role) SELECT '00000000-0000-0000-0000-000000000001', id, 'billing_specialist' FROM users WHERE username = 'dl-test-specialist'" >/dev/null
SPEC="$(curl -fsS -H 'Content-Type: application/json' --data '{"username":"dl-test-specialist","password":"Deadline-Test-2026"}' "${API}/auth/login" | jq -er '.access_token')"
expect "specialist changes a rule" "$(curl -sS -o /dev/null -w '%{http_code}' -X PUT -H "Authorization: Bearer ${SPEC}" -H 'Content-Type: application/json' \
  --data '{"payer_name":"*","deadline_type":"reconsideration","days":30}' "${API}/denials/deadline-rules")" 403

echo 'Deadline rules test passed.'
