#!/usr/bin/env bash
# End-to-end check of the retrieval evidence cut (FB-12) against a running
# local stack seeded with scripts/seed_test_knowledge.sh. A denial query is
# answered with the policy about its codes, and a denial for a service no
# policy in the knowledge base covers returns nothing at all, so the analysis
# records no_evidence instead of citing the least-bad documents.
#
# scripts/eval_retrieval.py measures retrieval quality over the whole labelled
# set; this only guards the engine's behaviour end to end.
set -euo pipefail

API="${API_BASE_URL:-http://127.0.0.1:18000}/api/v1"

fail() { echo "FAIL: $*" >&2; exit 1; }

TOKEN="$(jq -n --arg u "${ADMIN_USER:-admin}" --arg p "${ADMIN_PASSWORD:-admin123}" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API}/auth/login" | jq -er '.access_token')" \
  || fail "admin login failed; if the account must change its password first, sign in once in the UI and rerun with ADMIN_PASSWORD set"

# Titles returned for a denial query, one per line.
search() { # query, payer
  jq -n --arg q "$1" --arg p "$2" '{query: $q, top_k: 5, filters: {payer: $p}}' \
    | curl -fsS -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' --data @- "${API}/knowledge/search" \
    | jq -r '[.results[].document_title] | unique | .[]'
}

ANSWERED="$(search "CPT 20610 ICD-10 M17.11 CARC 197: Precertification/authorization/notification/pre-treatment absent" 'ACME HEALTH PLAN')"
grep -q 'Prior Authorization List 2026: Musculoskeletal Injections' <<<"$ANSWERED" \
  || fail "an authorization denial for CPT 20610 did not retrieve ACME's prior authorization list; got: ${ANSWERED:-nothing}"

for query in \
  "CPT A0429 CARC 96: Non-covered charge. Ambulance transport and mileage" \
  "CPT E1130 CARC 96: Non-covered charge. Manual wheelchair rental"
do
  UNANSWERED="$(search "$query" 'ACME HEALTH PLAN')"
  [[ -z "$UNANSWERED" ]] || fail "no policy covers this service, but retrieval returned: ${UNANSWERED}"
done

echo 'Retrieval cut test passed.'
