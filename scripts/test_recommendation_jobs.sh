#!/usr/bin/env bash
# End-to-end check of the durable recommendation job queue against a running
# local stack. Creates one synthetic claim/denial, then verifies:
#   - a queued job is picked up by the worker and reaches a terminal state
#   - a job left `running` by a dead worker (stale lock) is recovered and run
# Removes everything it created, even on failure. Needs curl, jq and the
# Docker Postgres container.
set -euo pipefail

API_BASE_URL="${API_BASE_URL:-http://127.0.0.1:18000}"
DB_CONTAINER="${DB_CONTAINER:-denialnav-rust-postgres}"
DB_USER="${POSTGRES_USER:-denial_nav}"
DB_NAME="${POSTGRES_DB:-denial_navigator}"
DEV_ORG='00000000-0000-0000-0000-000000000001'
RUN_ID="$(date +%s)-$$"
CLAIM_ID=''

psql_exec() {
  docker exec "$DB_CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$DB_USER" -d "$DB_NAME" -Atc "$1"
}

cleanup() {
  # Cascades to the denial, its jobs and analyses.
  if [[ -n "$CLAIM_ID" ]]; then psql_exec "DELETE FROM claims WHERE id = '${CLAIM_ID}'::uuid" >/dev/null || true; fi
}
trap cleanup EXIT

command -v curl >/dev/null
command -v jq >/dev/null
docker inspect "$DB_CONTAINER" >/dev/null
curl -fsS "${API_BASE_URL}/health/ready" >/dev/null

TOKEN="$(jq -n --arg u "${ADMIN_USER:-admin}" --arg p "${ADMIN_PASSWORD:-admin123}" '{username: $u, password: $p}' \
  | curl -fsS -H 'Content-Type: application/json' --data @- "${API_BASE_URL}/api/v1/auth/login" | jq -er '.access_token')" \
  || { echo "admin login failed; sign in once in the UI and rerun with ADMIN_PASSWORD set" >&2; exit 1; }
AUTH=(-H "Authorization: Bearer ${TOKEN}")

CLAIM_ID="$(psql_exec "INSERT INTO claims (organization_id, claim_number, patient_id, payer_name, total_charge, status) VALUES ('${DEV_ORG}'::uuid, 'JOBS-${RUN_ID}', 'JOBS-PATIENT', 'Jobs Payer', 1.00, 'ingested') RETURNING id")"
DENIAL_ID="$(psql_exec "INSERT INTO denials (claim_id, cagc, carc_code, charge_amount, status) VALUES ('${CLAIM_ID}'::uuid, 'CO', '16', 1.00, 'open') RETURNING id")"

wait_terminal() {
  local job_id="$1" status=''
  for _ in $(seq 1 60); do
    status="$(curl -fsS "${AUTH[@]}" "${API_BASE_URL}/api/v1/analyses/generate-jobs/${job_id}" | jq -er '.status')"
    [[ "$status" == 'completed' || "$status" == 'failed' ]] && { echo "$status"; return; }
    sleep 2
  done
  echo "$status"
}

# 1. A queued job is claimed and finishes. With no model server the pipeline
#    falls back to deterministic rules, so a healthy queue ends `completed`.
JOB_ID="$(curl -fsS -X POST "${AUTH[@]}" -H 'Content-Type: application/json' \
  --data "{\"denial_id\":\"${DENIAL_ID}\"}" "${API_BASE_URL}/api/v1/analyses/generate-jobs" | jq -er '.id')"
[[ "$(wait_terminal "$JOB_ID")" == 'completed' ]]
[[ "$(psql_exec "SELECT attempts FROM recommendation_jobs WHERE id = '${JOB_ID}'::uuid")" -ge 1 ]]

# 2. A job stranded `running` by a dead worker is recovered by the reaper and
#    then run. Age its lock past the timeout plus slack (default 300 s + 60 s);
#    the reaper runs every 30 s.
STALE_ID="$(psql_exec "INSERT INTO recommendation_jobs (denial_id, organization_id, status, attempts, locked_at, started_at) VALUES ('${DENIAL_ID}'::uuid, '${DEV_ORG}'::uuid, 'running', 1, NOW() - interval '1 hour', NOW() - interval '1 hour') RETURNING id")"
for _ in $(seq 1 30); do
  [[ "$(psql_exec "SELECT status FROM recommendation_jobs WHERE id = '${STALE_ID}'::uuid")" == 'completed' ]] && break
  sleep 3
done
[[ "$(psql_exec "SELECT status FROM recommendation_jobs WHERE id = '${STALE_ID}'::uuid")" == 'completed' ]]
[[ "$(psql_exec "SELECT attempts FROM recommendation_jobs WHERE id = '${STALE_ID}'::uuid")" -ge 2 ]]

echo 'Recommendation job queue integration test passed.'
