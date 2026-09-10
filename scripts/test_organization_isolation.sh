#!/usr/bin/env bash
# End-to-end tenant isolation check for a running local Docker stack.
# Creates two temporary organizations and claims, verifies API boundaries, then
# removes only the fixture rows it created.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
RUN_ID="$(date +%s)-$$"
OTHER_SLUG="isolation-test-${RUN_ID}"
OTHER_USER="isolation-${RUN_ID}"
TEST_PASSWORD='IsolationPass123!'
DEV_ORG='00000000-0000-0000-0000-000000000001'
OTHER_ORG=''
OTHER_USER_ID=''
DEV_CLAIM_ID=''
OTHER_CLAIM_ID=''

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}

cleanup() {
  if [[ -n "$DEV_CLAIM_ID" ]]; then psql_exec "DELETE FROM claims WHERE id = '${DEV_CLAIM_ID}'::uuid" >/dev/null || true; fi
  if [[ -n "$OTHER_CLAIM_ID" ]]; then psql_exec "DELETE FROM claims WHERE id = '${OTHER_CLAIM_ID}'::uuid" >/dev/null || true; fi
  if [[ -n "$OTHER_ORG" ]]; then psql_exec "DELETE FROM audit_log WHERE organization_id = '${OTHER_ORG}'::uuid" >/dev/null || true; fi
  if [[ -n "$OTHER_USER_ID" ]]; then psql_exec "DELETE FROM users WHERE id = '${OTHER_USER_ID}'::uuid" >/dev/null || true; fi
  if [[ -n "$OTHER_ORG" ]]; then psql_exec "DELETE FROM organizations WHERE id = '${OTHER_ORG}'::uuid" >/dev/null || true; fi
}
trap cleanup EXIT

command -v curl >/dev/null
command -v jq >/dev/null
docker inspect "$DB_CONTAINER" >/dev/null
curl -fsS "${API_BASE_URL}/health/ready" >/dev/null

OTHER_ORG="$(psql_exec "INSERT INTO organizations (slug, name) VALUES ('${OTHER_SLUG}', 'Isolation Test ${RUN_ID}') RETURNING id")"
OTHER_USER_ID="$(psql_exec "INSERT INTO users (username, email, password_hash, full_name, role, is_active) VALUES ('${OTHER_USER}', '${OTHER_USER}@example.test', crypt('${TEST_PASSWORD}', gen_salt('bf', 12)), 'Isolation Test User', 'billing_manager', TRUE) RETURNING id")"
psql_exec "INSERT INTO organization_memberships (organization_id, user_id, role) VALUES ('${OTHER_ORG}'::uuid, '${OTHER_USER_ID}'::uuid, 'billing_manager')" >/dev/null

DEV_CLAIM_ID="$(psql_exec "INSERT INTO claims (organization_id, claim_number, patient_id, payer_name, total_charge, status) VALUES ('${DEV_ORG}'::uuid, 'ISO-DEV-${RUN_ID}', 'ISO-DEV-PATIENT', 'Isolation Payer', 1.00, 'ingested') RETURNING id")"
OTHER_CLAIM_ID="$(psql_exec "INSERT INTO claims (organization_id, claim_number, patient_id, payer_name, total_charge, status) VALUES ('${OTHER_ORG}'::uuid, 'ISO-OTHER-${RUN_ID}', 'ISO-OTHER-PATIENT', 'Isolation Payer', 1.00, 'ingested') RETURNING id")"

OTHER_TOKEN="$(curl -fsS -H 'Content-Type: application/json' \
  --data "{\"username\":\"${OTHER_USER}\",\"password\":\"${TEST_PASSWORD}\"}" \
  "${API_BASE_URL}/api/v1/auth/login" | jq -er '.access_token')"
ADMIN_TOKEN="$(curl -fsS -H 'Content-Type: application/json' \
  --data '{"username":"admin","password":"admin123"}' \
  "${API_BASE_URL}/api/v1/auth/login" | jq -er '.access_token')"

curl -fsS -H "Authorization: Bearer ${OTHER_TOKEN}" "${API_BASE_URL}/api/v1/claims?limit=500" \
  | jq -e --arg id "$OTHER_CLAIM_ID" 'length == 1 and .[0].id == $id' >/dev/null

STATUS="$(curl -sS -o /dev/null -w '%{http_code}' -H "Authorization: Bearer ${OTHER_TOKEN}" "${API_BASE_URL}/api/v1/claims/${DEV_CLAIM_ID}")"
[[ "$STATUS" == '404' ]]

curl -fsS -H "Authorization: Bearer ${ADMIN_TOKEN}" "${API_BASE_URL}/api/v1/claims?limit=500" \
  | jq -e --arg id "$OTHER_CLAIM_ID" '[.[] | select(.id == $id)] | length == 0' >/dev/null

echo 'Organization isolation integration test passed.'
