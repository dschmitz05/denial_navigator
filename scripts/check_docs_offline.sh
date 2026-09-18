#!/bin/bash
# Assert the API documentation pages reference nothing outside this host.
#
# The docs are vendored so they work air-gapped (see
# crates/api-gateway/static/docs/README.md). A docs.rs change that points the
# pages at a CDN, or a well-meaning revert, silently reintroduces a CDN dependency - and the
# only symptom on the target deployment is a blank white page.
set -euo pipefail
BASE="${1:-http://localhost:8000}"
fail=0

# The application serves HTTPS with a self-signed certificate by default, which
# curl rejects. Retry once with verification disabled and say so, rather than
# reporting a certificate problem as a documentation problem. Certificate
# validity is not what this script is testing.
CURL_OPTS="-sf"      # fetch a page; fails on HTTP error
CURL_CODE_OPTS="-s"  # fetch a status code; must NOT fail on HTTP error
if [ "${BASE#https://}" != "$BASE" ]; then
    if ! curl -sf -o /dev/null "$BASE/openapi.json" 2>/dev/null; then
        if curl -sfk -o /dev/null "$BASE/openapi.json" 2>/dev/null; then
            echo "note  certificate is not trusted by curl; continuing without verification"
            CURL_OPTS="-sfk"
            CURL_CODE_OPTS="-sk"
        fi
    fi
fi

for page in /docs /redoc; do
    html=$(curl $CURL_OPTS "$BASE$page") || { echo "FAIL  $page did not respond"; fail=1; continue; }
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
        code=$(curl $CURL_CODE_OPTS -o /dev/null -w '%{http_code}' "$BASE$asset")
        if [ "$code" != "200" ]; then
            echo "FAIL  $page -> $asset returned HTTP $code"
            fail=1
        fi
    done
done

[ "$fail" -eq 0 ] && echo "API docs are fully self-contained" || echo "API docs would break on an air-gapped host"
exit "$fail"
