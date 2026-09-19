#!/usr/bin/env bash
# End-to-end check of the forced password change (FB-04) against a running
# local stack, using a temporary account created with must_change_password set,
# as the seeded admin is:
#   - sign-in returns only a password-change token, which reaches nothing else
#   - weak new passwords are refused; a good one yields a full session
#   - the old password stops working; an admin reset flags the account again
# Removes the account it created, even on failure.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
API="${API_BASE_URL}/api/v1"
DEV_ORG='00000000-0000-0000-0000-000000000001'
USERNAME="pwchange-test-$(date +%s)"
FIRST='Initial-Secret-2026'
CHOSEN='Chosen-Secret-For-Test'
RESET_TO='Admin-Reset-Secret-2026'

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}
cleanup() { psql_exec "DELETE FROM users WHERE username LIKE 'pwchange-test-%'" >/dev/null || true; }
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { [[ "$2" == "$3" ]] || fail "$1: expected '$3', got '$2'"; }
code() {  # code TOKEN METHOD PATH [BODY]
  curl -sS -o /dev/null -w '%{http_code}' -X "$2" -H "Authorization: Bearer $1" \
    -H 'Content-Type: application/json' ${4:+--data "$4"} "${API}$3"
}
login() {
  jq -n --arg u "$USERNAME" --arg p "$1" '{username: $u, password: $p}' \
    | curl -sS -H 'Content-Type: application/json' --data @- "${API}/auth/login"
}
change_body() { jq -n --arg c "$1" --arg n "$2" '{current_password: $c, new_password: $n}'; }

USER_ID="$(psql_exec "INSERT INTO users (username, email, password_hash, full_name, role, is_active, must_change_password) \
  VALUES ('${USERNAME}', '${USERNAME}@example.test', crypt('${FIRST}', gen_salt('bf', 12)), 'Password change test', 'system_admin', TRUE, TRUE) RETURNING id")"
psql_exec "INSERT INTO organization_memberships (organization_id, user_id, role) VALUES ('${DEV_ORG}', '${USER_ID}', 'system_admin')" >/dev/null

response="$(login "$FIRST")"
expect "first sign-in" "$(jq -r '.status' <<<"$response")" password_change_required
expect "no full session yet" "$(jq -r '.access_token // "none"' <<<"$response")" none
LIMITED="$(jq -r '.password_change_token' <<<"$response")"

expect "limited token on claims" "$(code "$LIMITED" GET /claims)" 401
expect "limited token on users" "$(code "$LIMITED" GET /users)" 401
expect "limited token on me" "$(code "$LIMITED" GET /auth/me)" 200

expect "too short" "$(code "$LIMITED" POST /auth/change-password "$(change_body "$FIRST" 'Short-1')")" 400
expect "contains username" "$(code "$LIMITED" POST /auth/change-password "$(change_body "$FIRST" "x-${USERNAME}-x")")" 400
expect "common password" "$(code "$LIMITED" POST /auth/change-password "$(change_body "$FIRST" 'password1234')")" 400
expect "wrong current password" "$(code "$LIMITED" POST /auth/change-password "$(change_body 'not-the-password' "$CHOSEN")")" 401

FULL="$(curl -fsS -X POST -H "Authorization: Bearer ${LIMITED}" -H 'Content-Type: application/json' \
  --data "$(change_body "$FIRST" "$CHOSEN")" "${API}/auth/change-password" | jq -er '.access_token')"
expect "full token after change" "$(code "$FULL" GET /claims)" 200
expect "flag cleared" "$(psql_exec "SELECT must_change_password FROM users WHERE id = '${USER_ID}'")" f
expect "old password" "$(login "$FIRST" | jq -r 'if .access_token or .password_change_token then "accepted" else "refused" end')" refused
expect "new password" "$(login "$CHOSEN" | jq -r 'if .access_token then "session" else .status end')" session

# An administrator resetting the password (here the account itself, which is
# an administrator) means someone else knows it: flagged again.
expect "admin reset" "$(code "$FULL" POST "/users/${USER_ID}/password" "{\"password\":\"${RESET_TO}\"}")" 200
expect "flag after reset" "$(psql_exec "SELECT must_change_password FROM users WHERE id = '${USER_ID}'")" t
expect "sign-in after reset" "$(login "$RESET_TO" | jq -r '.status')" password_change_required

echo 'Forced password change test passed.'
