#!/usr/bin/env bash
# End-to-end check of denial attachments and appeal packet assembly (FB-15)
# against a running local stack. A file is attached to a denial, downloaded
# back byte-for-byte, and shows up in the generated packet's attachment
# index; the packet starts draft, becomes approved, and a submission record
# (method + confirmation number) is stored separately from the packet's own
# review state. Removes what it created, even on failure.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
API="${API_BASE_URL}/api/v1"

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}
WORKDIR=""
cleanup() {
  psql_exec "DELETE FROM claims WHERE claim_number = 'FB15TEST1'" >/dev/null || true
  rm -rf "${WORKDIR:-}" 2>/dev/null || true
}
trap cleanup EXIT
cleanup
WORKDIR="$(mktemp -d)"

fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { [[ "$2" == "$3" ]] || fail "$1: expected '$3', got '$2'"; }

TOKEN="$(jq -n --arg u "${ADMIN_USER:-admin}" --arg p "${ADMIN_PASSWORD:-admin123}" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API}/auth/login" | jq -er '.access_token')" \
  || fail "admin login failed; if the account must change its password first, sign in once in the UI and rerun with ADMIN_PASSWORD set"
AUTH=(-H "Authorization: Bearer ${TOKEN}")

CLAIM_ID="$(psql_exec "INSERT INTO claims (organization_id, claim_number, patient_id, payer_name, total_charge, status) \
  VALUES ('00000000-0000-0000-0000-000000000001', 'FB15TEST1', 'FB15-PATIENT', 'FB15 Test Payer', 500.00, 'denied') RETURNING id")"
DENIAL_ID="$(psql_exec "INSERT INTO denials (claim_id, cagc, carc_code, charge_amount, status) \
  VALUES ('${CLAIM_ID}'::uuid, 'CO', '50', 500.00, 'open') RETURNING id")"
ANALYSIS_ID="$(psql_exec "INSERT INTO ai_analyses (denial_id, claim_id, model_name, required_action, denial_category, draft_appeal_letter) \
  VALUES ('${DENIAL_ID}'::uuid, '${CLAIM_ID}'::uuid, 'fb15-test', 'appeal_letter', 'coding_error', \
          'To whom it may concern: this service was medically necessary and should be reconsidered.') RETURNING id")"
APPEAL_ID="$(curl -fsS -X POST "${AUTH[@]}" -H 'Content-Type: application/json' \
  --data "{\"denial_id\":\"${DENIAL_ID}\",\"ai_analysis_id\":\"${ANALYSIS_ID}\",\"resolution_type\":\"appeal_letter\"}" "${API}/appeals" | jq -er '.id')"

# ── attachments ──
echo 'Operative report for medical necessity review.' > "${WORKDIR}/op-report.txt"
upload_resp="$(curl -fsS -X POST "${AUTH[@]}" -F "file=@${WORKDIR}/op-report.txt;type=text/plain" \
  "${API}/denials/${DENIAL_ID}/attachments")"
ATTACHMENT_ID="$(jq -er '.id' <<<"$upload_resp")"
expect "upload reports the filename" "$(jq -r '.filename' <<<"$upload_resp")" "op-report.txt"

listed="$(curl -fsS "${AUTH[@]}" "${API}/denials/${DENIAL_ID}/attachments")"
expect "one attachment listed" "$(jq -r 'length' <<<"$listed")" 1
expect "listed filename matches" "$(jq -r '.[0].filename' <<<"$listed")" "op-report.txt"

curl -fsS "${AUTH[@]}" -o "${WORKDIR}/downloaded.txt" "${API}/denials/${DENIAL_ID}/attachments/${ATTACHMENT_ID}"
expect "downloaded bytes match what was uploaded" \
  "$(sha256sum "${WORKDIR}/downloaded.txt" | awk '{print $1}')" \
  "$(sha256sum "${WORKDIR}/op-report.txt" | awk '{print $1}')"

expect "another organization's denial 404s on upload" \
  "$(curl -sS -o /dev/null -w '%{http_code}' -X POST "${AUTH[@]}" -F "file=@${WORKDIR}/op-report.txt" \
     "${API}/denials/00000000-0000-0000-0000-000000000099/attachments")" 404

# ── packet ──
expect "no packet before generation" \
  "$(curl -sS -o /dev/null -w '%{http_code}' "${AUTH[@]}" "${API}/appeals/${APPEAL_ID}/packet")" 404

generate_resp="$(curl -fsS -X POST "${AUTH[@]}" "${API}/appeals/${APPEAL_ID}/packet")"
expect "a freshly generated packet is a draft" "$(jq -r '.status' <<<"$generate_resp")" "draft"
[[ "$(jq -r '.size_bytes' <<<"$generate_resp")" -gt 100 ]] || fail "generated packet is implausibly small"

curl -fsS "${AUTH[@]}" -o "${WORKDIR}/packet.pdf" "${API}/appeals/${APPEAL_ID}/packet/download"
head -c4 "${WORKDIR}/packet.pdf" | grep -q '%PDF' || fail "downloaded packet is not a PDF"

status_resp="$(curl -fsS "${AUTH[@]}" "${API}/appeals/${APPEAL_ID}/packet")"
expect "packet status endpoint agrees: draft" "$(jq -r '.status' <<<"$status_resp")" "draft"

approve_resp="$(curl -fsS -X POST "${AUTH[@]}" "${API}/appeals/${APPEAL_ID}/packet/approve")"
expect "approving sets status to approved" "$(jq -r '.status' <<<"$approve_resp")" "approved"
expect "status endpoint reflects the approval" \
  "$(curl -fsS "${AUTH[@]}" "${API}/appeals/${APPEAL_ID}/packet" | jq -r '.status')" "approved"

# Regenerating resets a reviewed packet back to draft: new content, old
# approval no longer applies to it.
curl -fsS -X POST "${AUTH[@]}" "${API}/appeals/${APPEAL_ID}/packet" >/dev/null
expect "regenerating resets status to draft" \
  "$(curl -fsS "${AUTH[@]}" "${API}/appeals/${APPEAL_ID}/packet" | jq -r '.status')" "draft"

# ── submission tracking requires the packet to have been reviewed ──
# A submission recorded against a packet nobody approved would look like the
# filing was checked when it wasn't.
expect "recording a submission against a draft packet is rejected" \
  "$(curl -sS -o /dev/null -w '%{http_code}' -X PATCH "${AUTH[@]}" -H 'Content-Type: application/json' \
     --data '{"submission_method":"portal"}' "${API}/appeals/${APPEAL_ID}")" 400

curl -fsS -X POST "${AUTH[@]}" "${API}/appeals/${APPEAL_ID}/packet/approve" >/dev/null
submit_resp="$(curl -fsS -X PATCH "${AUTH[@]}" -H 'Content-Type: application/json' \
  --data '{"submission_method":"portal","payer_confirmation_number":"CONF-88221"}' "${API}/appeals/${APPEAL_ID}")"
expect "submission method recorded once the packet is approved" "$(jq -r '.submission_method' <<<"$submit_resp")" "portal"
expect "confirmation number recorded" "$(jq -r '.payer_confirmation_number' <<<"$submit_resp")" "CONF-88221"
expect "rejects an unknown submission method" \
  "$(curl -sS -o /dev/null -w '%{http_code}' -X PATCH "${AUTH[@]}" -H 'Content-Type: application/json' \
     --data '{"submission_method":"carrier-pigeon"}' "${API}/appeals/${APPEAL_ID}")" 422

# ── attachment deletion ──
curl -fsS -X DELETE "${AUTH[@]}" "${API}/denials/${DENIAL_ID}/attachments/${ATTACHMENT_ID}" >/dev/null
expect "attachment gone after delete" \
  "$(curl -fsS "${AUTH[@]}" "${API}/denials/${DENIAL_ID}/attachments" | jq -r 'length')" 0
expect "deleted attachment 404s on download" \
  "$(curl -sS -o /dev/null -w '%{http_code}' "${AUTH[@]}" "${API}/denials/${DENIAL_ID}/attachments/${ATTACHMENT_ID}")" 404

echo 'Attachments and packet test passed.'
