#!/usr/bin/env bash
# Upload every .txt file scripts/split_lcd_csv.py wrote into the knowledge
# base as a CMS LCD document. Each filename is "lcd_<determination_number>_
# <slug>.txt"; the determination number becomes the document title prefix so
# duplicates are easy to spot in Settings -> Knowledge.
#
# Re-running is safe to retry after a failure: it does not check for an
# existing document with the same title first (the API does not expose a
# title-uniqueness check), so re-running after a FULLY successful pass will
# create duplicates - only rerun to pick up files that failed.
#
#   API_BASE_URL   gateway URL            (default http://127.0.0.1:18000)
#   ADMIN_USER     admin/manager account  (default admin)
#   ADMIN_PASSWORD its password           (default admin123, the dev seed)
#
# Usage: scripts/upload_lcd_documents.sh <dir-of-lcd-txt-files>
set -euo pipefail

DIR="${1:?Usage: $0 <dir-of-lcd-txt-files>}"
API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
ADMIN_USER="${ADMIN_USER:-admin}"
ADMIN_PASSWORD="${ADMIN_PASSWORD:-admin123}"
API="${API_BASE_URL}/api/v1"

command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }
curl -fsS "${API_BASE_URL}/health" >/dev/null

TOKEN="$(jq -n --arg u "$ADMIN_USER" --arg p "$ADMIN_PASSWORD" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API}/auth/login" \
  | jq -er '.access_token')"

uploaded=0 failed=0
shopt -s nullglob
for f in "$DIR"/*.txt; do
  resp="$(curl -sS -w '\n%{http_code}' -X POST \
    "${API}/knowledge/documents/upload?source_type=cms_lcd" \
    -H "Authorization: Bearer ${TOKEN}" \
    -F "file=@${f}")"
  code="$(tail -n1 <<<"$resp")"
  body="$(sed '$d' <<<"$resp")"
  if [[ "$code" == "200" || "$code" == "201" ]]; then
    chunks="$(jq -r '.chunks_indexed // "?"' <<<"$body" 2>/dev/null || echo "?")"
    echo "  ok    $(basename "$f")  (${chunks} chunks)"
    uploaded=$((uploaded + 1))
  else
    detail="$(jq -r '.detail // .' <<<"$body" 2>/dev/null || echo "$body")"
    echo "  FAIL  $(basename "$f")  HTTP ${code}: ${detail}" >&2
    failed=$((failed + 1))
  fi
done

echo "Uploaded ${uploaded}, failed ${failed}."
[[ "$failed" -eq 0 ]]
