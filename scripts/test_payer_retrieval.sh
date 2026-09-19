#!/usr/bin/env bash
# End-to-end check of payer aliases in retrieval (FB-11) against a running
# local stack. A policy filed under "ZENITH TEST HEALTH PLAN OF OHIO" is not
# found for a claim from "Zenith Test Health" until both names are aliases of
# one payer; a claim that carries only the payer ID finds it once the ID is an
# alias too. A policy that expired before the date of service is never
# returned. Another organization neither sees the payer nor can change it.
# Removes what it created, even on failure.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
API="${API_BASE_URL}/api/v1"
RUN_ID="$(date +%s)"
OTHER_ORG=""

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}
cleanup() {
  psql_exec "DELETE FROM knowledge_documents WHERE title LIKE 'FB11 test %'" >/dev/null || true
  psql_exec "DELETE FROM payers WHERE name LIKE 'Zenith Test Health%'" >/dev/null || true
  psql_exec "DELETE FROM users WHERE username = 'payer-test-other'" >/dev/null || true
  if [[ -n "$OTHER_ORG" ]]; then
    psql_exec "DELETE FROM audit_log WHERE organization_id = '${OTHER_ORG}'" >/dev/null || true
    psql_exec "DELETE FROM organizations WHERE id = '${OTHER_ORG}'" >/dev/null || true
  fi
}
trap cleanup EXIT
cleanup

fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { [[ "$2" == "$3" ]] || fail "$1: expected '$3', got '$2'"; }

TOKEN="$(jq -n --arg u "${ADMIN_USER:-admin}" --arg p "${ADMIN_PASSWORD:-admin123}" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API}/auth/login" | jq -er '.access_token')" \
  || fail "admin login failed; if the account must change its password first, sign in once in the UI and rerun with ADMIN_PASSWORD set"
AUTH=(-H "Authorization: Bearer ${TOKEN}")

document() { # title, expiration date or null
  jq -n --arg t "$1" --argjson exp "$2" '{
    title: $t, source_type: "payer_policy", payer_name: "ZENITH TEST HEALTH PLAN OF OHIO",
    effective_date: "2025-01-01", expiration_date: $exp,
    content: "Zenith quasar modifier policy. Claims billed with modifier QZ for the quasar procedure require the operative report attached. Zenith denies quasar claims without the operative report under CARC 252."
  }' | curl -fsS "${AUTH[@]}" -H 'Content-Type: application/json' --data @- "${API}/knowledge/documents" | jq -er '.status'
}
expect "current policy indexed" "$(document 'FB11 test current policy' null)" indexed
expect "expired policy indexed" "$(document 'FB11 test expired policy' '"2026-01-31"')" indexed

# Titles of the test policies found for a claim, as "a,b". Knowledge calls
# share a per-minute limit, so a 429 waits and retries; any other error fails
# the test rather than reading as "nothing found".
found() { # filters json
  local body out headers status
  body="$(jq -n --argjson f "$1" '{query: "quasar modifier QZ operative report", top_k: 20, filters: $f}')"
  headers="$(mktemp)"
  for _ in 1 2 3; do
    out="$(curl -sS -D "$headers" -w '\n%{http_code}' "${AUTH[@]}" -H 'Content-Type: application/json' \
      --data "$body" "${API}/knowledge/search")"
    status="${out##*$'\n'}"
    [[ "$status" == 429 ]] || break
    sleep "$(awk -F': ' 'tolower($1) == "retry-after" {print $2 + 0}' "$headers" | tail -1 || echo 60)"
  done
  rm -f "$headers"
  [[ "$status" == 200 ]] || { echo "knowledge search returned HTTP ${status}" >&2; kill -TERM $$; exit 1; }
  jq -r '[.results[].document_title | select(startswith("FB11 test"))] | unique | join(",")' <<<"${out%$'\n'*}"
}
BY_NAME='{"payer":"Zenith Test Health","effective_on":"2026-09-01"}'
BY_ID='{"payer":"ZTH Commercial","payer_id_number":"ZTH987","effective_on":"2026-09-01"}'

expect "before mapping" "$(found "$BY_NAME")" ""

PAYER="$(curl -fsS "${AUTH[@]}" -H 'Content-Type: application/json' --data '{"name":"Zenith Test Health"}' "${API}/payers" | jq -er '.id')"
alias_of() { # payer, alias, kind
  curl -sS -o /dev/null -w '%{http_code}' "${AUTH[@]}" -H 'Content-Type: application/json' \
    --data "{\"alias\":\"$2\",\"kind\":\"$3\"}" "${API}/payers/$1/aliases"
}
expect "add document spelling" "$(alias_of "$PAYER" 'Zenith Test Health Plan of Ohio' name)" 200
expect "alias differing only in case and punctuation" "$(alias_of "$PAYER" 'ZENITH TEST HEALTH PLAN, OF OHIO' name)" 409

expect "after mapping, expired policy excluded" "$(found "$BY_NAME")" "FB11 test current policy"
expect "while in force, expired policy included" "$(found '{"payer":"Zenith Test Health","effective_on":"2026-01-15"}')" \
  "FB11 test current policy,FB11 test expired policy"
expect "payer ID before mapping" "$(found "$BY_ID")" ""
expect "add payer ID" "$(alias_of "$PAYER" ZTH987 payer_id)" 200
expect "payer ID after mapping" "$(found "$BY_ID")" "FB11 test current policy"

curl -fsS "${AUTH[@]}" "${API}/payers" | jq -e --arg id "$PAYER" \
  '(.payers[] | select(.id == $id) | .aliases | length) == 3 and ([.unmapped[] | select(.name == "ZENITH TEST HEALTH PLAN OF OHIO")] | length) == 0' >/dev/null \
  || fail "payer list does not show the three aliases, or still lists the document spelling as unmapped"

# Another organization's manager sees none of this and cannot change it.
OTHER_ORG="$(psql_exec "INSERT INTO organizations (slug, name) VALUES ('payer-test-${RUN_ID}', 'Payer Test ${RUN_ID}') RETURNING id")"
psql_exec "INSERT INTO users (username, email, password_hash, full_name, role, is_active) VALUES ('payer-test-other', 'payer-test-other@example.test', crypt('Payer-Test-2026', gen_salt('bf', 12)), 'Payer test', 'revenue_cycle_manager', TRUE)" >/dev/null
psql_exec "INSERT INTO organization_memberships (organization_id, user_id, role) SELECT '${OTHER_ORG}', id, 'revenue_cycle_manager' FROM users WHERE username = 'payer-test-other'" >/dev/null
OTHER="$(curl -fsS -H 'Content-Type: application/json' --data '{"username":"payer-test-other","password":"Payer-Test-2026"}' "${API}/auth/login" | jq -er '.access_token')"
expect "other org lists payers" "$(curl -fsS -H "Authorization: Bearer ${OTHER}" "${API}/payers" | jq -r '.payers | length')" 0
expect "other org adds an alias" "$(curl -sS -o /dev/null -w '%{http_code}' -H "Authorization: Bearer ${OTHER}" -H 'Content-Type: application/json' \
  --data '{"alias":"Anything"}' "${API}/payers/${PAYER}/aliases")" 404
expect "other org deletes the payer" "$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE -H "Authorization: Bearer ${OTHER}" "${API}/payers/${PAYER}")" 404

expect "delete payer" "$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE "${AUTH[@]}" "${API}/payers/${PAYER}")" 200
expect "after deleting the payer" "$(found "$BY_NAME")" ""

echo 'Payer retrieval test passed.'
