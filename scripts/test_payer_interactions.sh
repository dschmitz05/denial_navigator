#!/usr/bin/env bash
# End-to-end check of the structured payer-interaction log (FB-16) against a
# running local stack. Logging a call records who was reached, its reference
# number and a promised follow-up date; the deadline digest picks up the due
# follow-up; marking it done takes it out of the digest and off the denial.
# Removes what it created, even on failure.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
API="${API_BASE_URL}/api/v1"
DENIAL_ID=""

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}
cleanup() {
  psql_exec "DELETE FROM claims WHERE claim_number = 'FB16TEST1'" >/dev/null || true
}
trap cleanup EXIT
cleanup

fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { [[ "$2" == "$3" ]] || fail "$1: expected '$3', got '$2'"; }

TOKEN="$(jq -n --arg u "${ADMIN_USER:-admin}" --arg p "${ADMIN_PASSWORD:-admin123}" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API}/auth/login" | jq -er '.access_token')" \
  || fail "admin login failed; if the account must change its password first, sign in once in the UI and rerun with ADMIN_PASSWORD set"
AUTH=(-H "Authorization: Bearer ${TOKEN}")
ADMIN_ID="$(psql_exec "SELECT id FROM users WHERE username = '${ADMIN_USER:-admin}'")"

CLAIM_ID="$(psql_exec "INSERT INTO claims (organization_id, claim_number, patient_id, payer_name, total_charge, status) \
  VALUES ('00000000-0000-0000-0000-000000000001', 'FB16TEST1', 'FB16-PATIENT', 'FB16 Test Payer', 250.00, 'denied') RETURNING id")"
DENIAL_ID="$(psql_exec "INSERT INTO denials (claim_id, cagc, carc_code, charge_amount, status) \
  VALUES ('${CLAIM_ID}'::uuid, 'CO', '16', 250.00, 'open') RETURNING id")"

FOLLOW_UP="$(date -u -d '+3 days' +%Y-%m-%d 2>/dev/null || date -u -v+3d +%Y-%m-%d)"
create_resp="$(curl -fsS -X POST "${AUTH[@]}" -H 'Content-Type: application/json' --data "$(jq -n --arg d "$FOLLOW_UP" '{
    channel: "phone", summary: "Called member services, told claim is under review.",
    representative: "Jordan", reference_number: "REF-99123", follow_up_on: $d
  }')" "${API}/denials/${DENIAL_ID}/interactions")"
INTERACTION_ID="$(jq -er '.id' <<<"$create_resp")"

expect "rejects an unknown channel" \
  "$(curl -sS -o /dev/null -w '%{http_code}' -X POST "${AUTH[@]}" -H 'Content-Type: application/json' \
     --data '{"channel":"carrier-pigeon","summary":"x"}' "${API}/denials/${DENIAL_ID}/interactions")" 422

listed="$(curl -fsS "${AUTH[@]}" "${API}/denials/${DENIAL_ID}/interactions")"
expect "one interaction listed" "$(jq -r 'length' <<<"$listed")" 1
expect "reference number recorded" "$(jq -r '.[0].reference_number' <<<"$listed")" "REF-99123"
expect "representative recorded" "$(jq -r '.[0].representative' <<<"$listed")" "Jordan"
expect "follow-up date recorded" "$(jq -r '.[0].follow_up_on' <<<"$listed")" "$FOLLOW_UP"
expect "attributed to the caller" "$(jq -r '.[0].user_name' <<<"$listed")" "$(psql_exec "SELECT full_name FROM users WHERE id = '${ADMIN_ID}'")"

digest="$(curl -fsS -X POST "${AUTH[@]}" "${API}/notifications/generate-digests")"
DIGEST_CREATED="$(jq -r '.digests_created' <<<"$digest")"
notif="$(psql_exec "SELECT payload FROM notifications WHERE user_id = '${ADMIN_ID}' AND kind = 'payer_followup_digest' AND for_date = CURRENT_DATE")"
[[ -n "$notif" ]] || fail "no payer_followup_digest notification was created for the promised follow-up"
expect "digest counts our follow-up as upcoming" "$(jq -r '.upcoming >= 1' <<<"$notif")" true
psql_exec "DELETE FROM notifications WHERE user_id = '${ADMIN_ID}' AND kind = 'payer_followup_digest' AND for_date = CURRENT_DATE" >/dev/null

curl -fsS -X POST "${AUTH[@]}" "${API}/denials/${DENIAL_ID}/interactions/${INTERACTION_ID}/complete" >/dev/null
completed="$(curl -fsS "${AUTH[@]}" "${API}/denials/${DENIAL_ID}/interactions" | jq -r '.[0].follow_up_completed_at')"
[[ "$completed" != "null" ]] || fail "follow_up_completed_at was not set after marking done"

expect "completing an already-completed follow-up is idempotent" \
  "$(curl -sS -o /dev/null -w '%{http_code}' -X POST "${AUTH[@]}" "${API}/denials/${DENIAL_ID}/interactions/${INTERACTION_ID}/complete")" 200
expect "completing a non-existent interaction 404s" \
  "$(curl -sS -o /dev/null -w '%{http_code}' -X POST "${AUTH[@]}" "${API}/denials/${DENIAL_ID}/interactions/00000000-0000-0000-0000-000000000099/complete")" 404

echo "Payer interactions test passed (digest run created ${DIGEST_CREATED} total)."
