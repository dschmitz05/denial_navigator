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
DEV_DENIAL_ID=''
DEV_APPEAL_ID=''
DEV_PLAYBOOK_ID=''
DEV_ADMIN_ID=''

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}

cleanup() {
  if [[ -n "$DEV_CLAIM_ID" ]]; then psql_exec "DELETE FROM claims WHERE id = '${DEV_CLAIM_ID}'::uuid" >/dev/null || true; fi
  if [[ -n "$OTHER_CLAIM_ID" ]]; then psql_exec "DELETE FROM claims WHERE id = '${OTHER_CLAIM_ID}'::uuid" >/dev/null || true; fi
  psql_exec "DELETE FROM provider_adjustments WHERE trace_number = 'ISO-${RUN_ID}'" >/dev/null || true
  psql_exec "DELETE FROM payer_deadline_rules WHERE payer_name = 'ISO-${RUN_ID}'" >/dev/null || true
  if [[ -n "$DEV_PLAYBOOK_ID" ]]; then psql_exec "DELETE FROM institutional_playbooks WHERE id = '${DEV_PLAYBOOK_ID}'::uuid" >/dev/null || true; fi
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
OTHER_USER_ID="$(psql_exec "INSERT INTO users (username, email, password_hash, full_name, role, is_active) VALUES ('${OTHER_USER}', '${OTHER_USER}@example.test', crypt('${TEST_PASSWORD}', gen_salt('bf', 12)), 'Isolation Test User', 'system_admin', TRUE) RETURNING id")"
psql_exec "INSERT INTO organization_memberships (organization_id, user_id, role) VALUES ('${OTHER_ORG}'::uuid, '${OTHER_USER_ID}'::uuid, 'system_admin')" >/dev/null
DEV_ADMIN_ID="$(psql_exec "SELECT id FROM users WHERE username = 'admin' LIMIT 1")"

DEV_CLAIM_ID="$(psql_exec "INSERT INTO claims (organization_id, claim_number, patient_id, payer_name, total_charge, status) VALUES ('${DEV_ORG}'::uuid, 'ISO-DEV-${RUN_ID}', 'ISO-DEV-PATIENT', 'Isolation Payer', 1.00, 'ingested') RETURNING id")"
OTHER_CLAIM_ID="$(psql_exec "INSERT INTO claims (organization_id, claim_number, patient_id, payer_name, total_charge, status) VALUES ('${OTHER_ORG}'::uuid, 'ISO-OTHER-${RUN_ID}', 'ISO-OTHER-PATIENT', 'Isolation Payer', 1.00, 'ingested') RETURNING id")"

OTHER_TOKEN="$(curl -fsS -H 'Content-Type: application/json' \
  --data "{\"username\":\"${OTHER_USER}\",\"password\":\"${TEST_PASSWORD}\"}" \
  "${API_BASE_URL}/api/v1/auth/login" | jq -er '.access_token')"
ADMIN_TOKEN="$(jq -n --arg u "${ADMIN_USER:-admin}" --arg p "${ADMIN_PASSWORD:-admin123}" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API_BASE_URL}/api/v1/auth/login" | jq -er '.access_token')" \
  || { echo "admin login failed; if the account must change its password first, sign in once in the UI and rerun with ADMIN_PASSWORD set" >&2; exit 1; }

curl -fsS -H "Authorization: Bearer ${OTHER_TOKEN}" "${API_BASE_URL}/api/v1/claims?limit=500" \
  | jq -e --arg id "$OTHER_CLAIM_ID" 'length == 1 and .[0].id == $id' >/dev/null

STATUS="$(curl -sS -o /dev/null -w '%{http_code}' -H "Authorization: Bearer ${OTHER_TOKEN}" "${API_BASE_URL}/api/v1/claims/${DEV_CLAIM_ID}")"
[[ "$STATUS" == '404' ]]

curl -fsS -H "Authorization: Bearer ${ADMIN_TOKEN}" "${API_BASE_URL}/api/v1/claims?limit=500" \
  | jq -e --arg id "$OTHER_CLAIM_ID" '[.[] | select(.id == $id)] | length == 0' >/dev/null

# Playbooks are organization-owned: a rule created in Development must neither
# appear in nor be executable from the second organization.
DEV_PLAYBOOK_ID="$(curl -fsS -X POST -H "Authorization: Bearer ${ADMIN_TOKEN}" -H 'Content-Type: application/json' \
  --data '{"name":"Isolation rule","triggers":{},"recommendation":{"required_action":"review"}}' \
  "${API_BASE_URL}/api/v1/playbooks" | jq -er '.id')"
curl -fsS -H "Authorization: Bearer ${OTHER_TOKEN}" "${API_BASE_URL}/api/v1/playbooks" \
  | jq -e --arg id "$DEV_PLAYBOOK_ID" '[.[] | select(.id == $id)] | length == 0' >/dev/null

# An administrator is restricted to accounts in their own organization.
curl -fsS -H "Authorization: Bearer ${OTHER_TOKEN}" "${API_BASE_URL}/api/v1/users" \
  | jq -e --arg id "$DEV_ADMIN_ID" '[.[] | select(.id == $id)] | length == 0' >/dev/null
STATUS="$(curl -sS -o /dev/null -w '%{http_code}' -H "Authorization: Bearer ${OTHER_TOKEN}" "${API_BASE_URL}/api/v1/users/${DEV_ADMIN_ID}")"
[[ "$STATUS" == '404' ]]

# A cross-organization assignee is rejected instead of receiving another
# organization's claim details through queue notifications.
DEV_DENIAL_ID="$(psql_exec "INSERT INTO denials (claim_id, cagc, charge_amount, status) VALUES ('${DEV_CLAIM_ID}'::uuid, 'CO', 1.00, 'open') RETURNING id")"
DEV_APPEAL_ID="$(psql_exec "INSERT INTO appeals_queue (denial_id, claim_id, resolution_type, outcome_status) VALUES ('${DEV_DENIAL_ID}'::uuid, '${DEV_CLAIM_ID}'::uuid, 'appeal_letter', 'queued') RETURNING id")"
STATUS="$(curl -sS -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer ${ADMIN_TOKEN}" -H 'Content-Type: application/json' \
  --data "{\"assigned_user_id\":\"${OTHER_USER_ID}\"}" "${API_BASE_URL}/api/v1/appeals/${DEV_APPEAL_ID}/assign")"
[[ "$STATUS" == '404' ]]

# A write-off request is decided only inside its own organization: the second
# organization's administrator neither sees it nor can approve it.
DEV_WRITE_OFF_ID="$(psql_exec "INSERT INTO write_off_requests (organization_id, denial_id, amount) VALUES ('${DEV_ORG}'::uuid, '${DEV_DENIAL_ID}'::uuid, 1.00) RETURNING id")"
curl -fsS -H "Authorization: Bearer ${OTHER_TOKEN}" "${API_BASE_URL}/api/v1/write-offs" \
  | jq -e --arg id "$DEV_WRITE_OFF_ID" '[.[] | select(.id == $id)] | length == 0' >/dev/null
STATUS="$(curl -sS -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer ${OTHER_TOKEN}" "${API_BASE_URL}/api/v1/write-offs/${DEV_WRITE_OFF_ID}/approve")"
[[ "$STATUS" == '404' ]]
[[ "$(psql_exec "SELECT status FROM write_off_requests WHERE id = '${DEV_WRITE_OFF_ID}'")" == 'pending' ]]

# Overpayments and PLB provider adjustments are organization-owned too.
DEV_OVERPAYMENT_ID="$(psql_exec "INSERT INTO overpayments (organization_id, claim_id, kind, amount, due_date) VALUES ('${DEV_ORG}'::uuid, '${DEV_CLAIM_ID}'::uuid, 'duplicate_payment', 1.00, CURRENT_DATE + 60) RETURNING id")"
curl -fsS -H "Authorization: Bearer ${OTHER_TOKEN}" "${API_BASE_URL}/api/v1/overpayments" \
  | jq -e --arg id "$DEV_OVERPAYMENT_ID" '[.[] | select(.id == $id)] | length == 0' >/dev/null
STATUS="$(curl -sS -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer ${OTHER_TOKEN}" -H 'Content-Type: application/json' \
  --data '{"status":"refunded"}' "${API_BASE_URL}/api/v1/overpayments/${DEV_OVERPAYMENT_ID}/status")"
[[ "$STATUS" == '404' ]]
DEV_PLB_ID="$(psql_exec "INSERT INTO provider_adjustments (organization_id, reason_code, amount, trace_number) VALUES ('${DEV_ORG}'::uuid, 'WO', 1.00, 'ISO-${RUN_ID}') RETURNING id")"
curl -fsS -H "Authorization: Bearer ${OTHER_TOKEN}" "${API_BASE_URL}/api/v1/ingestion/provider-adjustments" \
  | jq -e --arg id "$DEV_PLB_ID" '[.[] | select(.id == $id)] | length == 0' >/dev/null
DEV_RULE_ID="$(psql_exec "INSERT INTO payer_deadline_rules (organization_id, payer_name, deadline_type, days) VALUES ('${DEV_ORG}'::uuid, 'ISO-${RUN_ID}', 'reconsideration', 30) RETURNING id")"
curl -fsS -H "Authorization: Bearer ${OTHER_TOKEN}" "${API_BASE_URL}/api/v1/denials/deadline-rules" \
  | jq -e --arg id "$DEV_RULE_ID" '[.[] | select(.id == $id)] | length == 0' >/dev/null
STATUS="$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE -H "Authorization: Bearer ${OTHER_TOKEN}" "${API_BASE_URL}/api/v1/denials/deadline-rules/${DEV_RULE_ID}")"
[[ "$STATUS" == '404' ]]

# Retention status is tenant scoped. A second-organization audit row cannot
# change the Development admin's count.
BEFORE_RETENTION="$(curl -fsS -H "Authorization: Bearer ${ADMIN_TOKEN}" "${API_BASE_URL}/api/v1/retention/audit" | jq -er '.total_entries')"
psql_exec "INSERT INTO audit_log (organization_id, action, resource_type, details) VALUES ('${OTHER_ORG}'::uuid, 'isolation_test', 'test', '{}'::jsonb)" >/dev/null
AFTER_RETENTION="$(curl -fsS -H "Authorization: Bearer ${ADMIN_TOKEN}" "${API_BASE_URL}/api/v1/retention/audit" | jq -er '.total_entries')"
# Each authenticated status read writes its own Development audit event. The
# second read should therefore add exactly one row, not two (which would mean
# the second organization's inserted row leaked into the count).
[[ "$AFTER_RETENTION" -eq "$((BEFORE_RETENTION + 1))" ]]

echo 'Organization isolation integration test passed.'
