#!/bin/bash
# Assert the API documentation pages reference nothing outside this host.
#
# The docs are vendored so they work air-gapped (see
# api-gateway/static/docs/README.md). A FastAPI upgrade that resets docs_url,
# or a well-meaning revert, silently reintroduces a CDN dependency - and the
# only symptom on the target deployment is a blank white page.
set -euo pipefail
BASE="${1:-http://localhost:8000}"
fail=0

for page in /docs /redoc; do
    html=$(curl -sf "$BASE$page") || { echo "FAIL  $page did not respond"; fail=1; continue; }
    external=$(echo "$html" | grep -oE 'https?://[^"]+' || true)
    if [ -n "$external" ]; then
        echo "FAIL  $page references external URLs:"
        echo "$external" | sed 's/^/        /'
        fail=1
    else
        echo "ok    $page references nothing off-host"
    fi
    # Every local asset it does reference must actually be served.
    for asset in $(echo "$html" | grep -oE '(src|href)="/[^"]+"' | cut -d'"' -f2); do
        code=$(curl -s -o /dev/null -w '%{http_code}' "$BASE$asset")
        if [ "$code" != "200" ]; then
            echo "FAIL  $page -> $asset returned HTTP $code"
            fail=1
        fi
    done
done

[ "$fail" -eq 0 ] && echo "API docs are fully self-contained" || echo "API docs would break on an air-gapped host"
exit "$fail"
