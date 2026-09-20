#!/bin/bash
# ============================================================
# Denial Navigator — compare reasoning models on your own denials
#
# Runs inside the api container, which can reach the rag-engine and llama.cpp
# host. Cases are copied from checked-in synthetic fixtures; PostgreSQL is not
# read during evaluation.
#
# One model at a time:
#
#   ./scripts/eval_model.sh run                 # with model A loaded
#   <swap the model on the llama.cpp host>
#   ./scripts/eval_model.sh run                 # with model B loaded
#   ./scripts/eval_model.sh compare eval/results/A.json eval/results/B.json
#
# The second run should evaluate the SAME denials as the first, or the
# comparison is not like for like:
#
#   ./scripts/eval_model.sh run --denial-ids $(./scripts/eval_model.sh ids eval/results/A.json)
#
# Nothing is written to or read from the database.
# ============================================================
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Convenience: print the denial ids from a previous run, to repeat it exactly.
if [ "${1:-}" = "ids" ]; then
    python3 -c "import json,sys; print(' '.join(json.load(open(sys.argv[1]))['denial_ids']))" "$2"
    exit 0
fi

# Runs inside the ALREADY RUNNING api container rather than `docker compose
# run`, which starts a fresh container that does not join the project network -
# it cannot resolve postgres or rag-engine, and fails on DNS before doing any
# work.
CONTAINER="${API_CONTAINER:-denial-navigator-api}"

docker cp "$ROOT/scripts/eval_model.py" "$CONTAINER:/tmp/eval_model.py" >/dev/null
docker exec "$CONTAINER" mkdir -p /tmp/fixtures
docker cp "$ROOT/scripts/fixtures/model_eval_cases.json" "$CONTAINER:/tmp/fixtures/model_eval_cases.json" >/dev/null
docker exec "$CONTAINER" mkdir -p /tmp/eval-results

docker exec "$CONTAINER" python /tmp/eval_model.py "$@" --out-dir /tmp/eval-results
status=$?

# Bring results back out to the repo, where they can be compared and kept.
mkdir -p "$ROOT/eval/results"
for f in $(docker exec "$CONTAINER" sh -c 'ls /tmp/eval-results/*.json 2>/dev/null' || true); do
    docker cp "$CONTAINER:$f" "$ROOT/eval/results/" >/dev/null 2>&1 || true
done
exit $status
