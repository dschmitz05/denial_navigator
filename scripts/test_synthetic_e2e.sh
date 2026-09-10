#!/usr/bin/env bash
set -euo pipefail

# Validates the supported 837 professional/institutional and 835 remittance
# ingestion path against the local API using only checked-in synthetic fixtures.
#
# Usage:
#   E2E_PASSWORD=... ./scripts/test_synthetic_e2e.sh

api_base_url="${API_BASE_URL:-http://127.0.0.1:18000}"
e2e_username="${E2E_USERNAME:-admin}"
: "${E2E_PASSWORD:?Set E2E_PASSWORD to the local test user password}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
login_response=$(curl -fsS \
    -H 'Content-Type: application/json' \
    --data "{\"username\":\"${e2e_username}\",\"password\":\"${E2E_PASSWORD}\"}" \
    "${api_base_url}/api/v1/auth/login")
token=$(jq -er '.access_token' <<<"${login_response}")

upload() {
  local fixture="$1"
  local filename="$2"
  curl -fsS -X POST \
    -H "Authorization: Bearer ${token}" \
    -F "file=@${fixture};filename=${filename}" \
    "${api_base_url}/api/v1/ingestion/ingest?force=true"
}

professional=$(upload "${repo_root}/scripts/sample_837p.txt" 'synthetic-e2e-professional.837')
institutional=$(upload "${repo_root}/scripts/sample_837i.txt" 'synthetic-e2e-institutional.837')
remittance=$(upload "${repo_root}/scripts/sample_835.txt" 'synthetic-e2e-remittance.835')
claims=$(curl -fsS -H "Authorization: Bearer ${token}" "${api_base_url}/api/v1/claims?q=PAT001&limit=10")
denials=$(curl -fsS -H "Authorization: Bearer ${token}" "${api_base_url}/api/v1/denials?q=PAT001&limit=50")

jq -ne \
  --argjson professional "${professional}" \
  --argjson institutional "${institutional}" \
  --argjson remittance "${remittance}" \
  --argjson claims "${claims}" \
  --argjson denials "${denials}" \
  'def has_claim: ([ $claims[] | select(.claim_number == "PAT001") ] | length > 0);
   def has_denial: ([ $denials.items[]? | select(.claim_number == "PAT001" and .carc_code == "197") ] | length > 0);
   if $professional.claims_stored == 1
      and $institutional.claims_stored == 1
      and $remittance.claims_stored == 3
      and ($remittance.denials_stored + $remittance.denials_skipped_as_duplicates) >= 1
      and has_claim and has_denial
   then {
     status: "passed",
     professional_claims: $professional.claims_stored,
     institutional_claims: $institutional.claims_stored,
     remittance_claims: $remittance.claims_stored,
     remittance_denials: $remittance.denials_stored,
     remittance_denials_skipped_as_duplicates: $remittance.denials_skipped_as_duplicates,
     pat001_denial_codes: ([ $denials.items[]? | .carc_code ] | unique)
   }
   else error("synthetic E2E persistence assertions failed")
   end'
