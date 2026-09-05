#!/bin/bash
# ============================================================
# Denial Navigator — Tear down
# ============================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_DIR"

echo "Stopping Denial Navigator services..."
docker-compose down

echo ""
echo "To remove all data (including volumes):"
echo "  docker-compose down -v"
