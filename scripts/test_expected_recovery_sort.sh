#!/usr/bin/env bash
# End-to-end check of expected-recovery sorting on the appeals/worklist
# queue (FB-17). Two open items, same amount, different CARC: one has a
# history of favorable resubmission (a real payer_carc rate), the other
# has none (falls back to the prior). Sorting by expected_recovery must
# rank the one with a proven rate first, and must never drop either item
# from the list. Removes what it created, even on failure.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
API="${API_BASE_URL}/api/v1"

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}
cleanup() {
  psql_exec "DELETE FROM claims WHERE claim_number IN ('FB17PROVEN', 'FB17UNKNOWN')" >/dev/null || true
}
trap cleanup EXIT
cleanup

fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { [[ "$2" == "$3" ]] || fail "$1: expected '$3', got '$2'"; }

TOKEN="$(jq -n --arg u "${ADMIN_USER:-admin}" --arg p "${ADMIN_PASSWORD:-admin123}" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API}/auth/login" | jq -er '.access_token')" \
  || fail "admin login failed; if the account must change its password first, sign in once in the UI and rerun with ADMIN_PASSWORD set"
AUTH=(-H "Authorization: Bearer ${TOKEN}")

# A claim/denial/analysis/appeals_queue item for one CARC, with feedback
# history; and an identical-amount item for a CARC with no history at all.
setup_item() { # claim_number, carc, paid_history (space-separated t/f, or empty)
  local claim_id denial_id analysis_id
  claim_id="$(psql_exec "INSERT INTO claims (organization_id, claim_number, patient_id, payer_name, total_charge, status) \
    VALUES ('00000000-0000-0000-0000-000000000001', '$1', '$1-PATIENT', 'FB17 Test Payer', 1000.00, 'denied') RETURNING id")"
  denial_id="$(psql_exec "INSERT INTO denials (claim_id, cagc, carc_code, charge_amount, status) \
    VALUES ('${claim_id}'::uuid, 'CO', '$2', 1000.00, 'open') RETURNING id")"
  analysis_id="$(psql_exec "INSERT INTO ai_analyses (denial_id, claim_id, model_name, required_action, denial_category) \
    VALUES ('${denial_id}'::uuid, '${claim_id}'::uuid, 'fb17-test', 'appeal_letter', 'coding_error') RETURNING id")"
  for outcome in $3; do
    psql_exec "INSERT INTO feedback_loop (ai_analysis_id, was_paid_on_resubmit) VALUES ('${analysis_id}'::uuid, $([ "$outcome" = t ] && echo TRUE || echo FALSE))" >/dev/null
  done
  curl -fsS -X POST "${AUTH[@]}" -H 'Content-Type: application/json' \
    --data "{\"denial_id\":\"${denial_id}\",\"resolution_type\":\"appeal_letter\"}" "${API}/appeals" | jq -er '.id'
}

PROVEN_ID="$(setup_item FB17PROVEN 201 't t t')"
UNKNOWN_ID="$(setup_item FB17UNKNOWN 202 '')"

listed="$(curl -fsS "${AUTH[@]}" "${API}/appeals?category=appeal&sort=expected_recovery&descending=true&limit=200")"
expect "both items are present, never hidden" \
  "$(jq --arg p "$PROVEN_ID" --arg u "$UNKNOWN_ID" '[.[] | select(.id == $p or .id == $u)] | length' <<<"$listed")" 2
expect "the proven CARC's basis is payer_carc" \
  "$(jq -r --arg p "$PROVEN_ID" '.[] | select(.id == $p) | .overturn_rate_basis' <<<"$listed")" "payer_carc"
expect "the proven CARC's rate is 100%" \
  "$(jq -r --arg p "$PROVEN_ID" '.[] | select(.id == $p) | .overturn_rate' <<<"$listed")" "1.0"
expect "the unknown CARC falls back to the prior" \
  "$(jq -r --arg u "$UNKNOWN_ID" '.[] | select(.id == $u) | .overturn_rate_basis' <<<"$listed")" "prior"

PROVEN_RANK="$(jq -r --arg p "$PROVEN_ID" '[.[].id] | index($p)' <<<"$listed")"
UNKNOWN_RANK="$(jq -r --arg u "$UNKNOWN_ID" '[.[].id] | index($u)' <<<"$listed")"
[[ "$PROVEN_RANK" -lt "$UNKNOWN_RANK" ]] \
  || fail "the proven, higher-overturn-rate item did not rank above the unproven one (proven=$PROVEN_RANK, unknown=$UNKNOWN_RANK)"

# The default sort is unaffected: both still present, in some order.
default_listed="$(curl -fsS "${AUTH[@]}" "${API}/appeals?category=appeal&limit=200")"
expect "default sort still shows both" \
  "$(jq --arg p "$PROVEN_ID" --arg u "$UNKNOWN_ID" '[.[] | select(.id == $p or .id == $u)] | length' <<<"$default_listed")" 2

echo 'Expected recovery sort test passed.'
