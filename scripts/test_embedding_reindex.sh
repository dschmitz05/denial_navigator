#!/usr/bin/env bash
# End-to-end check of embedding provenance and re-indexing (FB-13) against a
# running local stack. A document indexed under a simulated older model is
# excluded from vector search and counted as mismatched in health, even
# though its anchor terms and keyword match are otherwise perfect; the
# re-index endpoint fixes it in place and the document becomes findable
# again, with health reporting zero mismatches once it does.
# Removes what it created, even on failure.
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
  psql_exec "DELETE FROM knowledge_documents WHERE title = 'FB13 reindex test policy'" >/dev/null || true
}
trap cleanup EXIT
cleanup

fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { [[ "$2" == "$3" ]] || fail "$1: expected '$3', got '$2'"; }

TOKEN="$(jq -n --arg u "${ADMIN_USER:-admin}" --arg p "${ADMIN_PASSWORD:-admin123}" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API}/auth/login" | jq -er '.access_token')" \
  || fail "admin login failed; if the account must change its password first, sign in once in the UI and rerun with ADMIN_PASSWORD set"
AUTH=(-H "Authorization: Bearer ${TOKEN}")

DOC_ID="$(curl -fsS "${AUTH[@]}" -H 'Content-Type: application/json' --data '{
    "title": "FB13 reindex test policy", "source_type": "payer_policy",
    "content": "Quokka wombat platypus modifier ZQ policy. Claims for the quokka wombat procedure billed with modifier ZQ require a platypus attestation form on file."
  }' "${API}/knowledge/documents" | jq -er '.id')"

search() {
  jq -n '{query: "quokka wombat platypus modifier ZQ attestation", top_k: 10, filters: {}}' \
    | curl -fsS "${AUTH[@]}" -H 'Content-Type: application/json' --data @- "${API}/knowledge/search" \
    | jq -r '[.results[].knowledge_document_id] | unique | .[]'
}
mismatch_count() {
  curl -fsS "${AUTH[@]}" "${API}/system/health" \
    | jq -r '[.services[] | select(.name == "Embedding provenance")][0].mismatched_chunks'
}

expect "found before simulated drift" "$(search)" "$DOC_ID"
BEFORE="$(mismatch_count)"

psql_exec "UPDATE knowledge_chunks SET embedding_model = 'old-simulated-model' WHERE knowledge_document_id = '${DOC_ID}'" >/dev/null

AFTER_DRIFT="$(mismatch_count)"
[[ "$AFTER_DRIFT" -gt "$BEFORE" ]] || fail "health did not count the simulated drift as mismatched (before=$BEFORE, after=$AFTER_DRIFT)"
expect "excluded from search while mismatched" "$(search)" ""

expect "reindex processes the chunk" "$(curl -fsS -X POST "${AUTH[@]}" -H 'Content-Type: application/json' \
  --data '{"limit": 25}' "${API}/knowledge/reindex" | jq -r '.processed')" 1

expect "found again after reindex" "$(search)" "$DOC_ID"
expect "health mismatch count returns to baseline" "$(mismatch_count)" "$BEFORE"

echo 'Embedding reindex test passed.'
