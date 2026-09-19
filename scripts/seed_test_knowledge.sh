#!/usr/bin/env bash
# Load the synthetic knowledge base documents and playbooks in
# scripts/fixtures/test_knowledge.json into a running local stack.
#
# Everything in the fixture is fabricated test data (titles start with
# "[TEST]"). Documents go through the API so they are chunked and embedded
# exactly as a real upload would be. Re-running is safe: a document or playbook
# whose title/name already exists (and is not archived) is skipped.
#
#   API_BASE_URL   gateway URL            (default http://127.0.0.1:18000)
#   ADMIN_USER     admin/manager account  (default admin)
#   ADMIN_PASSWORD its password           (default admin123, the dev seed)
#   APPROVE        approve new playbooks  (default true)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIXTURE="${SCRIPT_DIR}/fixtures/test_knowledge.json"
API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
ADMIN_USER="${ADMIN_USER:-admin}"
ADMIN_PASSWORD="${ADMIN_PASSWORD:-admin123}"
APPROVE="${APPROVE:-true}"
API="${API_BASE_URL}/api/v1"

command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }
curl -fsS "${API_BASE_URL}/health" >/dev/null

TOKEN="$(jq -n --arg u "$ADMIN_USER" --arg p "$ADMIN_PASSWORD" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API}/auth/login" \
  | jq -er '.access_token')"
AUTH=(-H "Authorization: Bearer ${TOKEN}")

existing_docs="$(curl -fsS "${AUTH[@]}" "${API}/knowledge/documents?limit=500" \
  | jq -c '[.[] | select(.status != "archived") | .title]')"
existing_playbooks="$(curl -fsS "${AUTH[@]}" "${API}/playbooks" \
  | jq -c '[.[] | select(.status != "archived") | .name]')"

created=0 skipped=0 failed=0
while IFS= read -r doc; do
  title="$(jq -r '.title' <<<"$doc")"
  if jq -e --arg t "$title" 'index($t) != null' <<<"$existing_docs" >/dev/null; then
    skipped=$((skipped + 1)); continue
  fi
  body="$(jq -c 'with_entries(select(.value != null))' <<<"$doc")"
  response="$(curl -sS -w '\n%{http_code}' "${AUTH[@]}" -H 'Content-Type: application/json' \
    --data "$body" "${API}/knowledge/documents")"
  status="${response##*$'\n'}"
  if [[ "$status" == 2* ]] && [[ "$(jq -r '.status' <<<"${response%$'\n'*}")" == indexed ]]; then
    created=$((created + 1)); echo "  indexed  ${title}"
  else
    failed=$((failed + 1)); echo "  FAILED   ${title} (HTTP ${status}): ${response%$'\n'*}" >&2
  fi
done < <(jq -c '.documents[]' "$FIXTURE")
echo "Documents: ${created} created, ${skipped} already present, ${failed} failed"

pb_created=0 pb_skipped=0
while IFS= read -r playbook; do
  name="$(jq -r '.name' <<<"$playbook")"
  if jq -e --arg n "$name" 'index($n) != null' <<<"$existing_playbooks" >/dev/null; then
    pb_skipped=$((pb_skipped + 1)); continue
  fi
  id="$(curl -fsS "${AUTH[@]}" -H 'Content-Type: application/json' --data "$playbook" \
    "${API}/playbooks" | jq -er '.id')"
  if [[ "$APPROVE" == true ]]; then
    curl -fsS -X POST "${AUTH[@]}" "${API}/playbooks/${id}/approve" >/dev/null
  fi
  pb_created=$((pb_created + 1)); echo "  playbook ${name}"
done < <(jq -c '.playbooks[]' "$FIXTURE")
echo "Playbooks: ${pb_created} created$([[ "$APPROVE" == true ]] && echo ' and approved'), ${pb_skipped} already present"

[[ "$failed" -eq 0 ]]
