#!/bin/bash
# ============================================================
# Denial Navigator — Quick Start
# Uploads a sample 835 file for testing
# ============================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Find the sample file
SAMPLE_FILE="$PROJECT_DIR/scripts/sample_835.txt"

if [ ! -f "$SAMPLE_FILE" ]; then
    echo "ERROR: Sample file not found at $SAMPLE_FILE"
    exit 1
fi

echo "Uploading sample EDI 835 file..."
echo "File: $SAMPLE_FILE"
echo ""

curl -s -X POST "http://localhost:8000/api/v1/ingestion/upload" \
    -F "file=@$SAMPLE_FILE" \
    -H "Content-Type: multipart/form-data" | python3 -m json.tool 2>/dev/null || \
curl -s -X POST "http://localhost:8000/api/v1/ingestion/upload" \
    -F "file=@$SAMPLE_FILE"

echo ""
echo "Check the API logs for parsing results:"
echo "  docker-compose logs -f ediparser"
echo ""
echo "View results in the frontend:"
echo "  http://localhost:3080/denials"
